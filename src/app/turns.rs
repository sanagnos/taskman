//! `prefix d`: what a session did, a finished turn at a time, newest first, from its log
//! (`events_log`): a row per turn, and Enter shows the selected one's whole message under its row.
//! ↑/↓ choose a turn, ←/→ show the session above or below in the sidebar without closing, `g`
//! selects the session shown, and Esc closes an open turn, then the overlay, leaving the selection
//! as it was. The log is read off the UI thread, and again when a hook changes it while it's open.

use super::sync::Msg;
use super::{App, Id, Mode};
use crate::events_log::{self, Turn};
use crate::{markdown, store, untrusted};
use crossterm::event::{KeyCode, KeyEvent};
use std::path::PathBuf;
use std::time::SystemTime;

/// The overlay while it's open.
#[derive(Clone, Debug, Default)]
pub struct Turns {
    /// The session shown: the selected one at first, then wherever ←/→ moved.
    pub id: Id,
    /// Its turns, newest first; None until they're read.
    pub turns: Option<Vec<Turn>>,
    /// When the log last changed, as last asked for: the poll reads it again when this changes.
    pub stamp: Option<SystemTime>,
    /// The turn selected, counted from the newest.
    pub at: usize,
    /// Whether the selected turn shows its whole message (Enter). One turn at a time: choosing
    /// another closes it.
    pub open: bool,
    /// Set when a turn opens, for the next draw to show as much of its message as fits.
    pub fit: bool,
    /// The list's first row shown, of a row per turn and the open turn's message under its row.
    pub top: usize,
    /// As last drawn: how many rows show, and how many the open turn's message takes.
    pub rows: usize,
    pub open_rows: usize,
}

impl Turns {
    fn count(&self) -> usize {
        self.turns.as_ref().map_or(0, Vec::len)
    }

    /// The open turn's last row, or the selected one's when none is open.
    fn end(&self) -> usize {
        self.at + if self.open { self.open_rows } else { 0 }
    }

    /// Whether the open turn goes on below the view.
    fn more_below(&self) -> bool {
        self.open && self.end() >= self.top + self.rows
    }

    /// Whether the open turn's row is above the view.
    fn more_above(&self) -> bool {
        self.open && self.top > self.at
    }

    fn choose(&mut self, at: usize) {
        self.at = at.min(self.count().saturating_sub(1));
        self.open = false;
    }

    /// ↓ (or ↑ with `up`): through an open turn's message a row at a time, then to the next turn.
    pub fn step(&mut self, up: bool) {
        match up {
            true if self.more_above() => self.top -= 1,
            true => self.choose(self.at.saturating_sub(1)),
            false if self.more_below() => self.top += 1,
            false => self.choose(self.at + 1),
        }
    }

    /// The wheel down (or up): three rows through an open turn's message, never past it, or to
    /// the next turn when none is open.
    pub fn wheel(&mut self, up: bool) {
        if !self.open {
            return self.step(up);
        }
        for _ in 0..3 {
            match up {
                true if self.more_above() => self.top -= 1,
                false if self.more_below() => self.top += 1,
                _ => {}
            }
        }
    }

    /// PgDn (or PgUp with `up`): a page through an open turn's message, then a page of turns.
    pub fn page(&mut self, up: bool) {
        let page = self.rows.saturating_sub(1).max(1);
        match up {
            true if self.more_above() => self.top = self.top.saturating_sub(page).max(self.at),
            true => self.choose(self.at.saturating_sub(page)),
            false if self.more_below() => {
                self.top = (self.top + page).min(self.end() + 1 - self.rows);
            }
            false => self.choose(self.at + page),
        }
    }

    /// Opens the selected turn, or closes it.
    pub fn toggle(&mut self) {
        if self.count() > 0 {
            self.open = !self.open;
            self.fit = self.open;
        }
    }

    /// Where the view starts, for a draw of `rows` rows with the open turn's message taking
    /// `open_rows`: where it shows the selected turn's row, or some of the open turn, and once it
    /// opens, as much of it as fits.
    pub fn place(&mut self, rows: usize, open_rows: usize) {
        self.rows = rows.max(1);
        self.open_rows = if self.open { open_rows } else { 0 };
        self.at = self.at.min(self.count().saturating_sub(1));
        let end = self.end();
        if std::mem::take(&mut self.fit) && end >= self.top + self.rows {
            self.top = (end + 1 - self.rows).min(self.at);
        }
        let lo = (self.at + 1).saturating_sub(self.rows);
        let hi = if self.open { self.at.max((end + 1).saturating_sub(self.rows)) } else { self.at };
        let max = (self.count() + self.open_rows).saturating_sub(self.rows);
        self.top = self.top.clamp(lo, hi).min(max);
    }
}

/// The log of the session `id`.
fn log_of(id: &Id) -> PathBuf {
    events_log::log_path(&store::status_file(&id.0, &id.1))
}

/// When the file at `path` last changed; None when there's none.
fn stamp(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// `turns`, each with the line the overlay shows for it, cleaned for the screen: worked out here,
/// in the thread that reads them, rather than with every frame.
fn with_headlines(mut turns: Vec<Turn>) -> Vec<Turn> {
    for t in &mut turns {
        t.headline = untrusted::line(&markdown::headline(&untrusted::clean(&t.message)));
    }
    turns
}

impl App {
    pub(super) fn open_turns(&mut self) {
        let Some(tv) = self.selected() else {
            self.toast("No session to show the turns of");
            return;
        };
        let id = (tv.t.repo.clone(), tv.t.key.clone());
        self.show_turns(id);
    }

    /// Shows the turns of session `id`, read in the background.
    fn show_turns(&mut self, id: Id) {
        let path = log_of(&id);
        let at = stamp(&path);
        self.mode = Mode::Turns(Box::new(Turns { id: id.clone(), stamp: at, ..Turns::default() }));
        self.read_turns(id, path);
    }

    fn read_turns(&self, id: Id, path: PathBuf) {
        let failed_id = id.clone();
        self.reporter().spawn(
            format!("reading {}", path.display()),
            move || Msg::Turns(id, with_headlines(events_log::read(&path))),
            move |_| Msg::Turns(failed_id, Vec::new()),
        );
    }

    /// The turns read for session `id`, if it's still the one shown.
    pub(super) fn turns_read(&mut self, id: &Id, turns: Vec<Turn>) {
        if let Mode::Turns(t) = &mut self.mode
            && t.id == *id
        {
            // The turn selected stays selected, and open, as newer ones come in above it.
            let was = t.turns.as_ref().and_then(|old| old.get(t.at));
            match was.and_then(|w| turns.iter().position(|n| n == w)) {
                Some(at) => t.at = at,
                None if was.is_some() => {
                    t.at = 0;
                    t.open = false;
                }
                None => {}
            }
            t.turns = Some(turns);
        }
    }

    /// Reads the shown log again when a hook changed it. From the status poll.
    pub(super) fn poll_turns(&mut self) {
        let Mode::Turns(t) = &mut self.mode else { return };
        let path = log_of(&t.id);
        let now = stamp(&path);
        if now != t.stamp {
            t.stamp = now;
            let id = t.id.clone();
            self.read_turns(id, path);
        }
    }

    pub(super) fn turns_key(&mut self, key: KeyEvent) {
        let Mode::Turns(t) = &mut self.mode else { return };
        let id = t.id.clone();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => t.step(true),
            KeyCode::Down | KeyCode::Char('j') => t.step(false),
            KeyCode::PageUp => t.page(true),
            KeyCode::PageDown | KeyCode::Char(' ') => t.page(false),
            KeyCode::Home => t.choose(0),
            KeyCode::End => t.choose(usize::MAX),
            KeyCode::Enter => t.toggle(),
            KeyCode::Esc if t.open => t.open = false,
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Left | KeyCode::Right => {
                let order = self.ordered(true);
                let here = order.iter().position(|&i| self.tickets[i].t.is(&id.0, &id.1));
                let n = order.len().cast_signed();
                let to = match here {
                    _ if n == 0 => return,
                    Some(h) => (h.cast_signed() + if key.code == KeyCode::Left { -1 } else { 1 })
                        .rem_euclid(n),
                    None => 0,
                };
                let tv = &self.tickets[order[to.cast_unsigned()]];
                let to = (tv.t.repo.clone(), tv.t.key.clone());
                if to != id {
                    self.show_turns(to);
                }
            }
            KeyCode::Char('g') => {
                self.mode = Mode::Normal;
                if let Some(i) = self.tickets.iter().position(|tv| tv.t.is(&id.0, &id.1)) {
                    self.select(i);
                }
            }
            _ => {}
        }
    }
}
