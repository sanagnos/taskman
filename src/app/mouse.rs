//! The mouse: the wheel scrolls the pane under it (the sidebar's list too), a click in the
//! sidebar selects a card or folds a project, a drag selects text in a pane (see `selection.rs`
//! for the selection itself), and the selected text goes to the clipboard.

use super::sync::Msg;
use super::{AgentState, App, Hit, Mode};
use crate::selection::{self, Grab, Pane, Selection, Snapshot};
use crate::ui::SIDEBAR_STEP;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How soon a click on the same cell counts with the one before it as a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// How often a pane scrolls while the button is held past its edge.
const AUTO_SCROLL_EVERY: Duration = Duration::from_millis(50);

impl App {
    /// The content rectangle of each pane: Claude fills its area, the panels sit inside a border
    /// and one column of padding on each side.
    fn pane_rects(&self) -> [(Pane, Rect); 3] {
        let inner = |r: Option<Rect>| {
            r.map(|r| Rect {
                x: r.x + 2,
                y: r.y + 1,
                width: r.width.saturating_sub(4),
                height: r.height.saturating_sub(2),
            })
            .unwrap_or_default()
        };
        let (jira, gh) = (self.areas.jira, self.areas.gh);
        // An agent's board fills the right column, as one panel.
        if self.selected().is_some_and(|tv| tv.t.agent.is_some()) {
            let board = match (jira, gh) {
                (Some(j), Some(g)) => Some(j.union(g)),
                (j, g) => j.or(g),
            };
            return [
                (Pane::Claude, self.areas.claude),
                (Pane::Jira, inner(board)),
                (Pane::Gh, Rect::default()),
            ];
        }
        [(Pane::Claude, self.areas.claude), (Pane::Jira, inner(jira)), (Pane::Gh, inner(gh))]
    }

    fn pane_at(&self, pos: Position) -> Option<(Pane, Rect)> {
        self.pane_rects().into_iter().find(|(_, r)| r.contains(pos))
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) {
        if let Mode::Help(h) = &mut self.mode {
            match ev.kind {
                MouseEventKind::ScrollUp => h.top = h.top.saturating_sub(3),
                MouseEventKind::ScrollDown => h.top = (h.top + 3).min(h.max),
                _ => {}
            }
            return;
        }
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        let pos = Position::new(ev.column, ev.row);
        let a = self.areas;
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = ev.kind == MouseEventKind::ScrollUp;
                // Over the pane holding the selection, the wheel scrolls the selection's view;
                // while the button is held, the selection grows to what comes under the pointer.
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|s| self.pane_at(pos).is_some_and(|(_, r)| r == s.rect))
                {
                    let step = if self.selection.as_ref().is_some_and(|s| s.pane == Pane::Claude) {
                        3
                    } else {
                        2
                    };
                    self.scroll_selection(if up { -step } else { step });
                    return;
                }
                if a.claude.contains(pos) {
                    self.scroll_claude_at(pos, up);
                } else if a.sidebar.contains(pos) {
                    // The selection stays put, even once it's scrolled out of view; the list
                    // clamps the position as it's drawn.
                    let top = &mut self.sidebar_scroll.top;
                    *top = if up {
                        top.saturating_sub(SIDEBAR_STEP)
                    } else {
                        top.saturating_add(SIDEBAR_STEP)
                    };
                } else if let Some(tv) = self.tickets.get_mut(self.sel) {
                    let step = |v: u16| if up { v.saturating_sub(2) } else { v.saturating_add(2) };
                    // An agent's board fills both panels' room and scrolls as the tracker panel.
                    let board = tv.t.agent.is_some() && a.gh.is_some_and(|r| r.contains(pos));
                    if board || a.jira.is_some_and(|r| r.contains(pos)) {
                        tv.jira_scroll = step(tv.jira_scroll);
                    } else if a.gh.is_some_and(|r| r.contains(pos)) {
                        tv.gh_scroll = step(tv.gh_scroll);
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                let clicks = self.click_count(pos);
                match self.sidebar_hits.iter().find(|(r, _)| r.contains(pos)) {
                    Some(&(_, Hit::Ticket(i))) => self.select(i),
                    Some(&(_, Hit::Project(p))) => self.toggle_project(p),
                    None => {
                        if let Some((pane, rect)) = self.pane_at(pos) {
                            let snapshot =
                                if pane == Pane::Claude { self.snapshot_session() } else { None };
                            let top = match &snapshot {
                                Some(s) => s.top(),
                                None => self.panel_scroll(pane),
                            };
                            let at = selection::point_at(rect, top, pos);
                            let mut sel = Selection::new(pane, rect, at, pos, snapshot);
                            sel.grab = match clicks {
                                2 => Some(Grab::Word),
                                3 => Some(Grab::Line),
                                _ => None,
                            };
                            self.selection = Some(sel);
                        }
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(sel) = self.selection.as_ref().filter(|s| !s.released && s.grab.is_none())
                else {
                    return;
                };
                let top = self.view_top(sel);
                if let Some(sel) = self.selection.as_mut() {
                    sel.pointer = pos;
                    sel.head = selection::point_at(sel.rect, top, pos);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(sel) = self.selection.as_mut() {
                    if sel.grab.is_some() {
                        // Picked on the next frame.
                    } else if sel.dragged() {
                        sel.released = true;
                    } else {
                        self.selection = None; // a plain click selects nothing
                    }
                }
            }
            _ => {}
        }
    }

    /// How many clicks in a row this one is: those within `MULTI_CLICK` on the same cell, up to
    /// three, then it starts over.
    fn click_count(&mut self, pos: Position) -> u8 {
        let now = Instant::now();
        let n = match self.last_click {
            Some((at, when, n))
                if at == pos && now.duration_since(when) <= MULTI_CLICK && n < 3 =>
            {
                n + 1
            }
            _ => 1,
        };
        self.last_click = Some((pos, now, n));
        n
    }

    /// Widens a double or triple click to its word or line, now the pane has been drawn; a blank
    /// cell selects nothing. The selection then goes on as a dragged one released.
    fn pick_grab(&mut self) {
        let Some(sel) = self.selection.as_ref().filter(|s| !s.picked) else { return };
        let Some(grab) = sel.grab else { return };
        let row = sel.anchor.row;
        let cells = match &sel.snapshot {
            Some(s) => s.row_symbols(row),
            None => {
                self.panel.as_ref().filter(|p| p.rect == sel.rect).and_then(|p| p.row_symbols(row))
            }
        };
        let span = cells.and_then(|c| selection::grab_span(&c, grab, sel.anchor.col));
        match (span, self.selection.as_mut()) {
            (Some((from, to)), Some(sel)) => {
                sel.anchor.col = from;
                sel.head = selection::Point { row, col: to };
                sel.picked = true;
                sel.released = true;
            }
            _ => self.selection = None,
        }
    }

    /// A still view of the session the Claude pane shows, for a selection to be made on.
    fn snapshot_session(&self) -> Option<Snapshot> {
        let tv = self.selected()?;
        // The same test `ui::pane::draw_claude` makes to show the session rather than a message.
        let s =
            tv.session.as_ref().filter(|s| !s.exited() || tv.status.state != AgentState::Exited)?;
        Some(Snapshot::new(s.parser.clone()))
    }

    /// How far a panel is scrolled, in wrapped rows. The agents' board scrolls as the
    /// tracker panel.
    fn panel_scroll(&self, pane: Pane) -> usize {
        let Some(tv) = self.selected() else { return 0 };
        usize::from(if pane == Pane::Gh { tv.gh_scroll } else { tv.jira_scroll })
    }

    /// The content row at the top of the pane holding `sel`.
    pub fn view_top(&self, sel: &Selection) -> usize {
        match &sel.snapshot {
            Some(s) => s.top(),
            None => self.panel_scroll(sel.pane),
        }
    }

    /// Scrolls the pane holding the selection `delta` rows, positive toward the end; while the
    /// button is held, the selection's head follows the pointer onto the rows brought into view.
    fn scroll_selection(&mut self, delta: i32) {
        let Some(sel) = self.selection.as_mut() else { return };
        match (&mut sel.snapshot, self.tickets.get_mut(self.sel)) {
            (Some(s), _) => _ = s.scroll(delta),
            (None, Some(tv)) => {
                let scroll =
                    if sel.pane == Pane::Gh { &mut tv.gh_scroll } else { &mut tv.jira_scroll };
                // The panel clamps it to its lines as it's drawn.
                *scroll = scroll.saturating_add_signed(i16::try_from(delta).unwrap_or(0));
            }
            (None, None) => {}
        }
        let Some(sel) = self.selection.as_ref().filter(|s| !s.released) else { return };
        let head = selection::point_at(sel.rect, self.view_top(sel), sel.pointer);
        if let Some(sel) = self.selection.as_mut() {
            sel.head = head;
        }
    }

    /// Keeps the selection on its text as the session's history drops rows, and scrolls its pane
    /// while the button is held past the pane's edge. Runs on every tick.
    pub(super) fn follow_selection(&mut self) {
        let Some(sel) = self.selection.as_mut() else { return };
        if sel.snapshot.as_mut().is_some_and(|s| !s.track()) {
            self.selection = None;
            self.dirty.store(true, Ordering::Relaxed);
            return;
        }
        let step = selection::edge_step(sel.rect, sel.pointer);
        if sel.released
            || step == 0
            || sel.scrolled_at.is_some_and(|t| t.elapsed() < AUTO_SCROLL_EVERY)
        {
            return;
        }
        sel.scrolled_at = Some(Instant::now());
        self.scroll_selection(step);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// The selected text, read from the session's history and screen or the panel's lines.
    pub(super) fn selection_text(&self) -> String {
        let Some(sel) = &self.selection else { return String::new() };
        let (start, end) = sel.span();
        match &sel.snapshot {
            Some(s) => s.text(start, end),
            None => self
                .panel
                .as_ref()
                .filter(|p| p.rect == sel.rect)
                .map(|p| p.text(start, end))
                .unwrap_or_default(),
        }
    }

    /// Highlights the selection in the frame just drawn; once the mouse is released, copies it.
    pub fn apply_selection(&mut self, buf: &mut Buffer) {
        if self.selection.is_none() {
            self.panel = None;
            return;
        }
        if !matches!(self.mode, Mode::Normal) {
            self.selection = None;
            return;
        }
        self.pick_grab();
        let Some(sel) = self.selection.as_ref() else { return };
        let (start, end) = sel.span();
        if let Some((from, to)) = selection::on_screen(sel.rect, self.view_top(sel), start, end) {
            selection::highlight(buf, sel.rect, from, to);
        }
        if sel.released && !sel.copied {
            let text = self.selection_text();
            if let Some(sel) = self.selection.as_mut() {
                sel.copied = true;
            }
            if !text.is_empty() {
                self.copy_text(text);
            }
        }
    }

    /// Puts text on the clipboard. The commands that do it can be slow (clip.exe) or hang, so they
    /// run on a thread, and the toast says where the text went once they're done (`Msg::Copied`).
    pub(super) fn copy_text(&mut self, text: String) {
        self.copy_as(text, None);
    }

    /// `copy_text`, with the toast saying `label` (and where it went) rather than a length.
    pub(super) fn copy_as(&mut self, text: String, label: Option<String>) {
        let (tx, copy) = (self.tx.clone(), self.clipboard.clone());
        std::thread::spawn(move || {
            let copied = copy(&text);
            let _ = tx.send(Msg::Copied(text, copied, label));
        });
    }

    /// Scrolls the Claude pane. Claude Code's full-screen mode has no scrollback for us to
    /// walk through: it scrolls itself when it gets mouse-wheel events, so pass them on.
    fn scroll_claude_at(&mut self, pos: Position, up: bool) {
        let area = self.areas.claude;
        let Some(s) = self.tickets.get(self.sel).and_then(|tv| tv.session.as_ref()) else { return };
        if s.wants_mouse() {
            s.send_wheel(pos.x.saturating_sub(area.x), pos.y.saturating_sub(area.y), up);
        } else {
            s.scroll(if up { 3 } else { -3 });
        }
    }
}
