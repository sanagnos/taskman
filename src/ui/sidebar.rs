//! The sidebar: the projects, their cards, and the list's scrolling.

use super::{accent, dim, new_total, pr_spans, selection_bg, sidebar_badge, state_badge, yellow};
use crate::app::{App, Hit, Mode, Piece, TicketView};
use crate::keys::prefix;
use crate::untrusted;
use crate::util::{truncate, truncate_start, width};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

fn state_glyph(tv: &TicketView) -> Span<'static> {
    let b = state_badge(tv);
    let g: String = b.content.chars().next().unwrap_or(' ').to_string();
    Span::styled(g, b.style)
}

/// How far the sidebar's list is scrolled. On `App` for the session only, never saved.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SidebarScroll {
    /// The list row at the top of the view. Clamped to the list as it's drawn, so a resize or a
    /// project folding never leaves room at the bottom while rows are hidden above.
    pub top: usize,
    /// The furthest `top` goes, as the list was last drawn.
    pub max: usize,
    /// The selection last scrolled into view, and whether the sidebar was folded then: when
    /// either changes, the list scrolls again, just enough to show the selected card.
    shown: Option<(usize, bool)>,
    /// A card was selected since the last draw (`App::select`): shown then even when it was
    /// selected already, after the wheel scrolled it out of view. A redraw alone keeps a
    /// scroll made with the wheel.
    pub asked: bool,
}

/// How many rows a wheel notch over the sidebar scrolls.
pub const SIDEBAR_STEP: usize = 2;

/// A row of the sidebar's list, the projects' headers and their cards, laid out whole before the
/// view is cut out of it.
struct SidebarRow {
    line: Line<'static>,
    hit: Option<Hit>,
    selected: bool,
    /// A card's row, or a folded project's header: what an edge row counts when it hides them.
    entry: bool,
    /// The row's ticket needs the user, or it's a folded project's header with one that does: an
    /// edge row hiding it shows ●.
    attention: bool,
}

pub(super) fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::new().borders(Borders::RIGHT).border_style(dim());
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.sidebar_hits.clear();
    let collapsed = app.prefs.sidebar_collapsed || app.areas.sidebar_folded;

    // The jump box takes the title's rows, unless it's the picker over the screen.
    let mut narrowed = None;
    if let Mode::Jump(j) = &app.mode
        && !app.jump_picker()
    {
        let cards = app.jump_matches(&j.query);
        narrowed = Some(Narrowed { focus: cards.get(j.at).copied(), cards });
        draw_jump_box(f, &j.query, inner);
    } else {
        draw_title(f, app, inner);
    }

    // The list starts under the title and its notice, and keeps the sidebar's last row blank.
    let view = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(3),
    };
    let w = inner.width as usize;
    let rows = sidebar_rows(app, w, collapsed, narrowed.as_ref());
    let view_h = view.height as usize;
    let sel = narrowed.as_ref().map_or(Some(app.sel), |n| n.focus);
    // A project moving is folded, its card hidden: the header is what stays in view.
    let moving = app
        .moving_projects()
        .then(|| sel.and_then(|s| app.tickets.get(s)).and_then(|tv| app.project_of(&tv.t)))
        .flatten();
    let scroll = &mut app.sidebar_scroll;
    if scroll.shown != sel.map(|sel| (sel, collapsed)) || scroll.asked {
        scroll.shown = sel.map(|sel| (sel, collapsed));
        scroll.asked = false;
        let is_card = |r: &SidebarRow| match moving {
            Some(p) => r.hit == Some(Hit::Project(p)),
            None => sel.is_some() && r.hit == sel.map(Hit::Ticket),
        };
        if let Some(start) = rows.iter().position(is_card) {
            let end = start + rows[start..].iter().take_while(|r| is_card(r)).count();
            // Its project's header comes along only when the card is the project's first, right
            // under it: for any other, stepping up past the top edge would jump to the header
            // each time and lose the user's place.
            let header = rows[..start]
                .iter()
                .rposition(|r| r.hit.is_some())
                .filter(|&h| matches!(rows[h].hit, Some(Hit::Project(_))));
            scroll.top = scroll_to_show(scroll.top, rows.len(), view_h, start..end, header);
        }
    }
    scroll.max = rows.len().saturating_sub(view_h);
    scroll.top = scroll.top.min(scroll.max);
    let top = scroll.top;
    let (first, end) = shown_rows(top, rows.len(), view_h);

    // What an edge row stands for: the entries hidden past it, and whether one needs the user.
    let hidden = |rows: &[SidebarRow]| {
        let mut entries: Vec<Hit> = rows.iter().filter(|r| r.entry).filter_map(|r| r.hit).collect();
        entries.dedup();
        (entries.len(), rows.iter().any(|r| r.attention))
    };
    let above = (first > 0).then(|| hidden(&rows[..first]));
    let below = (end < rows.len()).then(|| hidden(&rows[end..]));
    let sel_bg = Style::new().bg(selection_bg());
    let bottom = view.bottom();
    let mut y = view.y;
    if let Some((n, attention)) = above
        && y < bottom
    {
        let r = Rect { y, height: 1, ..view };
        f.render_widget(Paragraph::new(more_line("↑", n, attention, w)), r);
        y += 1;
    }
    for row in rows.into_iter().skip(first).take(end - first) {
        if y >= bottom {
            break;
        }
        let r = Rect { y, height: 1, ..view };
        let mut para = Paragraph::new(row.line);
        if row.selected {
            para = para.style(sel_bg);
        }
        f.render_widget(para, r);
        // A card's rows make one hit, so clicks land on it wherever it's scrolled to.
        if let Some(hit) = row.hit {
            match app.sidebar_hits.last_mut() {
                Some((rect, h)) if *h == hit && rect.bottom() == y => rect.height += 1,
                _ => app.sidebar_hits.push((r, hit)),
            }
        }
        y += 1;
    }
    if let Some((n, attention)) = below
        && y < bottom
    {
        let r = Rect { y, height: 1, ..view };
        f.render_widget(Paragraph::new(more_line("↓", n, attention, w)), r);
    }
    // The line a dragged card would land on, from the hits just made.
    app.retarget();
    if let Some(t) = app.drag.as_ref().and_then(|d| d.target)
        && (view.y..bottom).contains(&t.line)
    {
        let r = Rect { y: t.line, height: 1, ..view };
        let on_card = app.sidebar_hits.iter().any(|(c, _)| c.contains(Position::new(r.x, r.y)));
        if on_card {
            // No blank row to draw on, as in the folded sidebar: underline the card's last row.
            let cell = f.buffer_mut();
            for x in r.x..r.right() {
                cell[(x, r.y)].modifier.insert(Modifier::UNDERLINED);
                cell[(x, r.y)].set_fg(accent());
            }
        } else {
            let line = format!(" {}", "━".repeat(w.saturating_sub(2)));
            f.render_widget(Paragraph::new(line).style(Style::new().fg(accent())), r);
        }
    }
}

/// The name, then the version in grey (a terminal can't do a smaller font); the version is
/// dropped when the collapsed sidebar has no room for it. A new release is on the line under it.
fn draw_title(f: &mut Frame, app: &App, inner: Rect) {
    let version = format!(" v{}", env!("CARGO_PKG_VERSION"));
    let mut title = vec![Span::styled(" taskman", Style::new().bold())];
    if inner.width as usize >= 8 + width(&version) {
        title.push(Span::styled(version, dim()));
    }
    f.render_widget(Paragraph::new(Line::from(title)), Rect { height: 1, ..inner });
    if let Some(n) = app.updater.notice(app.config.auto_update)
        && inner.height > 1
    {
        let room = (inner.width as usize).saturating_sub(1);
        let text = truncate(&crate::update::notice_text(&n, room), room);
        let line = Line::from(Span::styled(format!(" {text}"), Style::new().fg(accent())));
        f.render_widget(Paragraph::new(line), Rect { y: inner.y + 1, height: 1, ..inner });
    }
}

/// The jump box: what's typed on the title's row, with the cursor after it, and the keys under it.
fn draw_jump_box(f: &mut Frame, query: &str, inner: Rect) {
    let room = (inner.width as usize).saturating_sub(4);
    let shown = truncate_start(&untrusted::line(query), room);
    let line = Line::from(vec![
        Span::styled(" / ", Style::new().fg(accent()).bold()),
        Span::raw(shown.clone()),
    ]);
    f.render_widget(Paragraph::new(line), Rect { height: 1, ..inner });
    f.set_cursor_position((inner.x + 3 + width(&shown) as u16, inner.y));
    if inner.height > 1 {
        let hint = truncate(" ↑/↓ · Enter go · Esc", inner.width as usize);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, dim()))),
            Rect { y: inner.y + 1, height: 1, ..inner },
        );
    }
}

/// The sidebar while the jump box is open: only these cards, folded projects open, and the
/// highlighted one in place of the selection.
struct Narrowed {
    cards: Vec<usize>,
    focus: Option<usize>,
}

/// The sidebar's list: each project's header, then its cards, with the rows between them, as
/// `draw_sidebar` lays them out `w` wide. Trailing blank rows are dropped, so the list ends on
/// its last card.
fn sidebar_rows(
    app: &App,
    w: usize,
    collapsed: bool,
    narrowed: Option<&Narrowed>,
) -> Vec<SidebarRow> {
    let plain = |line: Line<'static>| SidebarRow {
        line,
        hit: None,
        selected: false,
        entry: false,
        attention: false,
    };
    let blank = || plain(Line::default());
    // Every project shows folded while one is moved, so each step passes one project.
    let all_folded = app.moving_projects();
    let mine = app.tickets.get(app.sel).and_then(|tv| app.project_of(&tv.t));
    let held = |p: usize| match app.dragged() {
        Some(Piece::Project(repo)) => app.projects[p].repo == *repo,
        _ => false,
    };
    let mut rows: Vec<SidebarRow> = Vec::new();
    for (p, proj) in app.projects.iter().enumerate() {
        let mut members = app.members(p);
        if let Some(n) = narrowed {
            members.retain(|i| n.cards.contains(i));
            if members.is_empty() {
                continue;
            }
        }
        let folded = (proj.collapsed || all_folded) && narrowed.is_none();
        // Group header: ▾ name   (folded: ▸ name (3) ……… ●)
        let arrow = if folded { "▸ " } else { "▾ " };
        let count = if folded { format!(" ({})", members.len()) } else { String::new() };
        let waiting = folded && members.iter().any(|&i| app.tickets[i].needs_attention());
        // The ● ends one column from the edge, where the cards' badges end.
        let room = w.saturating_sub(2 + width(&count) + if waiting { 3 } else { 0 });
        let name_style = if held(p) { dim() } else { Style::new().bold() };
        let name = truncate(&proj.name, room);
        let used = 2 + width(&name) + width(&count);
        let mut header = vec![
            Span::styled(arrow, dim()),
            Span::styled(name, name_style),
            Span::styled(count.clone(), dim()),
        ];
        if waiting {
            header.push(Span::raw(" ".repeat(w.saturating_sub(used + 2).max(1))));
            header.push(Span::styled("●", Style::new().fg(yellow()).bold()));
        }
        rows.push(SidebarRow {
            line: Line::from(header),
            hit: Some(Hit::Project(p)),
            // The project being moved with the keys, among the others folded.
            selected: all_folded && mine == Some(p) && app.dragged().is_none(),
            entry: folded,
            attention: waiting,
        });
        if folded {
            rows.push(blank());
            continue;
        }
        if !collapsed {
            let rule = "─".repeat(w.saturating_sub(2));
            rows.push(plain(Line::from(vec![Span::raw(" "), Span::styled(rule, dim())])));
        }
        if members.is_empty() {
            if !collapsed {
                let empty = if proj.tracker.is_some() {
                    format!("   no tickets · {} n", prefix().short())
                } else {
                    format!("   no agents · {} n", prefix().short())
                };
                rows.push(plain(Line::from(Span::styled(empty, dim()))));
            }
            rows.push(blank());
            continue;
        }

        for &i in &members {
            let tv = &app.tickets[i];
            let selected = Some(i) == narrowed.map_or(Some(app.sel), |n| n.focus);
            let agent = tv.t.agent.is_some();
            let title = if collapsed { None } else { tv.title() };
            // ┃ joins across lines, so the selected card reads as one block.
            let marker = if selected {
                Span::styled("┃", Style::new().fg(accent()))
            } else {
                Span::raw(" ")
            };
            let key_style =
                if selected { Style::new().fg(accent()).bold() } else { Style::new().bold() };
            let mut lines: Vec<Line> = Vec::new();
            if collapsed {
                let label = if agent { format!("✦{}", tv.t.display()) } else { tv.t.key.clone() };
                lines.push(Line::from(vec![
                    marker,
                    Span::styled(truncate(&label, w.saturating_sub(3)), key_style),
                    Span::raw(" "),
                    state_glyph(tv),
                ]));
            } else {
                let badge = sidebar_badge(tv);
                let label =
                    if agent { format!("✦ {}", tv.t.display()) } else { tv.t.key.clone() };
                // The badge keeps its width and ends one column from the edge; the name gets
                // the rest.
                let badge_w = width(&badge.content);
                let label = truncate(&label, w.saturating_sub(2 + 1 + badge_w + 1));
                let used = 2 + width(&label) + badge_w;
                let pad = " ".repeat(w.saturating_sub(used + 1).max(1));
                lines.push(Line::from(vec![
                    marker.clone(),
                    Span::raw(" "),
                    Span::styled(label, key_style),
                    Span::raw(pad),
                    badge,
                ]));
                let new = if selected { None } else { new_total(tv) };
                if let Some(title) = &title {
                    let title_style = if selected { Style::new() } else { dim() };
                    let mut row = vec![marker.clone(), Span::raw(" ")];
                    let mut room = w.saturating_sub(3);
                    if let Some(new) = &new {
                        let tag = format!("{new} · ");
                        room = room.saturating_sub(width(&tag));
                        row.push(Span::styled(tag, Style::new().fg(yellow()).bold()));
                    }
                    row.push(Span::styled(truncate(&untrusted::line(title), room), title_style));
                    lines.push(Line::from(row));
                }
                // Agents have no PR line: a name, and a title when Claude has set one.
                if !agent {
                    let mut pr = vec![marker, Span::raw(" ")];
                    if title.is_none()
                        && let Some(new) = &new
                    {
                        pr.push(Span::styled(
                            format!("{new} · "),
                            Style::new().fg(yellow()).bold(),
                        ));
                    }
                    let summary = pr_spans(app, tv);
                    match &tv.t.branch {
                        // Without GitHub, the branch in its place.
                        Some(b) if summary.is_empty() => {
                            pr.push(Span::styled(untrusted::line(b), dim()));
                        }
                        _ => pr.extend(summary),
                    }
                    lines.push(Line::from(pr));
                }
            }
            let attention = tv.needs_attention();
            // The card being dragged stays where it is, dimmed.
            let lifted =
                matches!(app.dragged(), Some(Piece::Card((repo, key))) if tv.t.is(repo, key));
            rows.extend(lines.into_iter().map(|mut line| {
                if lifted {
                    for span in &mut line.spans {
                        span.style = span.style.fg(Color::DarkGray).add_modifier(Modifier::DIM);
                    }
                }
                SidebarRow { line, hit: Some(Hit::Ticket(i)), selected, entry: true, attention }
            }));
            if !collapsed {
                rows.push(blank());
            }
        }
        if collapsed {
            rows.push(blank());
        }
    }
    if rows.is_empty() && narrowed.is_some() {
        rows.push(plain(Line::from(Span::styled("   No matches", dim()))));
    }
    while rows.last().is_some_and(|r| r.hit.is_none() && r.line.spans.is_empty()) {
        rows.pop();
    }
    rows
}

/// The rows a view `view_h` tall shows of `total`, from row `top`: an edge row with rows beyond
/// it says how many instead (`more_line`), so the content starts one row in at the top when
/// scrolled and stops one row short at the bottom while the list goes on.
pub(super) fn shown_rows(top: usize, total: usize, view_h: usize) -> (usize, usize) {
    let first = top + usize::from(top > 0);
    let end = if top + view_h < total { (top + view_h).saturating_sub(1) } else { total };
    (first.min(total), end.max(first).min(total))
}

/// The row to scroll to so that rows `want` (a card) show whole, with row `header` (its project's,
/// given when the card is the project's first) above them when there's room, moving `top` only
/// as far as that takes: not at all while the card is in view. A card taller than the view shows
/// from its first row. The Settings and links lists scroll with it too, a section's or group's
/// heading as the header.
pub(super) fn scroll_to_show(
    top: usize,
    total: usize,
    view_h: usize,
    want: std::ops::Range<usize>,
    header: Option<usize>,
) -> usize {
    let max_top = total.saturating_sub(view_h);
    // The tops that show `want` whole: a contiguous run, as both edges move with the top.
    let fitting = |want: std::ops::Range<usize>| {
        let lo = (0..=max_top).find(|&t| shown_rows(t, total, view_h).1 >= want.end)?;
        let hi = (0..=max_top).rev().find(|&t| shown_rows(t, total, view_h).0 <= want.start)?;
        (lo <= hi).then_some((lo, hi))
    };
    let Some((lo, hi)) = fitting(want.clone()) else {
        return want.start.saturating_sub(1).min(max_top);
    };
    if (lo..=hi).contains(&top) {
        return top;
    }
    let (lo, hi) = header.and_then(|h| fitting(h..want.end)).unwrap_or((lo, hi));
    top.clamp(lo, hi)
}

/// An edge row where the list goes on past the view: how many entries (cards, folded projects)
/// are hidden there, the arrow alone when only an open project's header is, with ● when one of
/// them needs the user.
pub(super) fn more_line(arrow: &str, entries: usize, attention: bool, w: usize) -> Line<'static> {
    let text = if entries > 0 { format!(" {arrow} {entries} more") } else { format!(" {arrow}") };
    let used = width(&text);
    let mut spans = vec![Span::styled(text, dim())];
    if attention {
        spans.push(Span::raw(" ".repeat(w.saturating_sub(used + 2).max(1))));
        spans.push(Span::styled("●", Style::new().fg(yellow()).bold()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_edge_rows_stand_for_the_rows_hidden_past_them() {
        // Everything fits: no edge rows.
        assert_eq!(shown_rows(0, 10, 20), (0, 10));
        assert_eq!(shown_rows(0, 20, 20), (0, 20));
        // Scrolled: the top row says what's above and the bottom row what's below, while there
        // is something.
        assert_eq!(shown_rows(0, 30, 20), (0, 19));
        assert_eq!(shown_rows(5, 30, 20), (6, 24));
        assert_eq!(shown_rows(10, 30, 20), (11, 30));
        // A view too small for its edge rows shows nothing rather than overflowing.
        assert_eq!(shown_rows(5, 30, 1), (6, 6));
        assert_eq!(shown_rows(5, 30, 0), (6, 6));
        assert_eq!(shown_rows(0, 30, 0), (0, 0));
    }

    #[test]
    fn sidebar_scrolls_just_enough_to_show_the_selection() {
        // 50 rows, 20 in view; the card at rows 30..33 under its project's header at row 20.
        let to =
            |top, want: std::ops::Range<usize>, header| scroll_to_show(top, 50, 20, want, header);
        // Below the view: the card ends on the row above the "more" row.
        assert_eq!(to(0, 30..33, Some(20)), 14);
        // In view already: nothing moves, header shown or not.
        assert_eq!(to(14, 30..33, Some(20)), 14);
        assert_eq!(to(25, 30..33, Some(20)), 25);
        // Above the view: the card starts under the "more" row ...
        assert_eq!(to(40, 30..33, None), 29);
        // ... with its header above it when it's the project's first and there's room.
        assert_eq!(to(40, 30..33, Some(20)), 19);
        assert_eq!(to(40, 30..33, Some(5)), 29);
        // The first card: the list starts at the top, no "more" row above.
        assert_eq!(to(40, 1..4, Some(0)), 0);
        // The last card: the list ends at the bottom, no "more" row below.
        assert_eq!(to(0, 47..50, Some(20)), 30);
        // A card taller than the view shows from its first row.
        assert_eq!(scroll_to_show(0, 50, 3, 30..33, None), 29);
        assert_eq!(shown_rows(29, 50, 3), (30, 31));
        // A list that fits never scrolls.
        assert_eq!(scroll_to_show(0, 10, 20, 6..9, Some(0)), 0);
    }
}
