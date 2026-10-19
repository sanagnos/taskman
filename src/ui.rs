//! Drawing: the layout and `draw`, which calls on a module for each part of the screen, and what
//! they share.

// Terminal coordinates are u16 throughout ratatui, and the lengths cast to them here (text widths,
// line counts) stay far below 65535 in practice. Saturating casts would change the overflow
// behaviour, so that is left to its own change.
#![allow(clippy::cast_possible_truncation)]

mod dialogs;
mod pane;
mod panels;
mod sidebar;

pub use sidebar::{SIDEBAR_STEP, SidebarScroll};

use crate::app::{AgentState, App, Away, Mode, TicketView};
use crate::github::CheckState;
use crate::store::UiPrefs;
use crate::untrusted;
use pane::{draw_claude, draw_footer, draw_header, draw_prefix_menu, draw_selecting};
use panels::{draw_board, draw_gh, draw_jira};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use sidebar::draw_sidebar;

const SIDEBAR_W: u16 = 30;
const SIDEBAR_COLLAPSED_W: u16 = 13;
/// The Claude pane's width below which the panels narrow, then the sidebar folds, then the panels
/// hide: Claude's prompt and diffs are hard to use narrower.
const CLAUDE_MIN_W: u16 = 60;
/// The right column's usual width is a third of the screen within these.
const PANELS_W: (u16, u16) = (36, 72);
/// How far the right column narrows to keep Claude at `CLAUDE_MIN_W`.
const PANELS_MIN_W: u16 = 28;
/// Screens at least this tall leave a blank row above the footer; shorter ones keep every row.
const FOOTER_GAP_MIN_H: u16 = 30;
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Areas {
    pub sidebar: Rect,
    pub header: Rect,
    pub claude: Rect,
    pub jira: Option<Rect>,
    pub gh: Option<Rect>,
    pub footer: Rect,
    /// The sidebar is folded, or the panels hidden, because the screen is too narrow for them
    /// beside a usable Claude pane, whatever the prefs say.
    pub sidebar_folded: bool,
    pub panels_hidden: bool,
}

/// `panels`: whether the tracker and PR panels show beside the session (`App::panels`); an agent's
/// board takes the column when either does.
pub fn layout(area: Rect, prefs: &UiPrefs, panels: (bool, bool)) -> Areas {
    // A blank row between the panes and the footer, so their last rows and bottom borders don't
    // sit on the bar. Claude's terminal is sized to what's left.
    let gap = if area.height >= FOOTER_GAP_MIN_H { 1 } else { 0 };
    let [main, _, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(gap), Constraint::Length(1)])
            .areas(area);
    let want_sw = if prefs.sidebar_collapsed { SIDEBAR_COLLAPSED_W } else { SIDEBAR_W };
    let want_rw =
        if panels.0 || panels.1 { (main.width / 3).clamp(PANELS_W.0, PANELS_W.1) } else { 0 };
    let (sw, rw) = fit_widths(main.width, want_sw, want_rw);
    let right_visible = rw > 0;
    let [sidebar, center, right] =
        Layout::horizontal([Constraint::Length(sw), Constraint::Min(20), Constraint::Length(rw)])
            .areas(main);
    // A one-column gutter each side of the Claude pane, so its full-width rules don't run into
    // the sidebar's or the panels' borders. Claude's terminal is sized to what's left.
    let gap_right = if right_visible { 1 } else { 0 };
    let center =
        Rect { x: center.x + 1, width: center.width.saturating_sub(1 + gap_right), ..center };
    let [header, claude] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(center);
    let (jira, gh) = match panels {
        _ if !right_visible => (None, None),
        (true, true) => {
            let [j, g] = Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
                .areas(right);
            (Some(j), Some(g))
        }
        (true, false) => (Some(right), None),
        (false, true) => (None, Some(right)),
        _ => (None, None),
    };
    Areas {
        sidebar,
        header,
        claude,
        jira,
        gh,
        footer,
        sidebar_folded: sw < want_sw,
        panels_hidden: want_rw > 0 && rw == 0,
    }
}

/// The sidebar's and the right column's widths on a screen `w` wide, given the ones wanted: the
/// first of narrower panels, a folded sidebar, hidden panels, or both, that leaves Claude
/// `CLAUDE_MIN_W` (with its gutters); else the narrowest.
fn fit_widths(w: u16, want_sw: u16, want_rw: u16) -> (u16, u16) {
    let claude = |sw: u16, rw: u16| w.saturating_sub(sw + rw + 1 + u16::from(rw > 0));
    let narrowed = |sw: u16| {
        if want_rw == 0 {
            return 0;
        }
        w.saturating_sub(sw + CLAUDE_MIN_W + 2).clamp(PANELS_MIN_W.min(want_rw), want_rw)
    };
    let folded = SIDEBAR_COLLAPSED_W.min(want_sw);
    let tries =
        [(want_sw, narrowed(want_sw)), (folded, narrowed(folded)), (want_sw, 0), (folded, 0)];
    tries.into_iter().find(|&(sw, rw)| claude(sw, rw) >= CLAUDE_MIN_W).unwrap_or((folded, 0))
}

fn spinner() -> &'static str {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    SPINNER[(ms / 100) as usize % SPINNER.len()]
}

fn dim() -> Style {
    Style::new().fg(crate::theme::current().hint)
}

/// Yellow text, or a dark amber where yellow wouldn't read (`Theme::yellow`).
fn yellow() -> Color {
    crate::theme::current().yellow
}

pub fn draw(f: &mut Frame, app: &mut App) {
    crate::theme::set_current(app.theme);
    // From the frame's own size: see `App::lay_out`.
    app.lay_out(f.area());
    let a = app.areas;
    draw_sidebar(f, app, a.sidebar);
    draw_header(f, app, a.header);
    draw_claude(f, app, a.claude);
    if app.selected().is_some_and(|tv| tv.t.agent.is_some()) {
        // An agent has no ticket or PR: the right column shows its project's board instead.
        if let Some(r) = match (a.jira, a.gh) {
            (Some(j), Some(g)) => Some(j.union(g)),
            (j, g) => j.or(g),
        } {
            draw_board(f, app, r);
        }
    } else {
        if let Some(r) = a.jira {
            draw_jira(f, app, r);
        }
        if let Some(r) = a.gh {
            draw_gh(f, app, r);
        }
    }
    draw_footer(f, app, a.footer);
    app.apply_selection(f.buffer_mut());
    draw_selecting(f, app);
    if app.prefix && matches!(app.mode, Mode::Normal) {
        draw_prefix_menu(f, a.footer);
    }
    dialogs::draw_dialog(f, app);
}

/// Under NO_COLOR, every cell's colours become the terminal's own, keeping bold, dim, underline
/// and reverse: the session's cells and the selection show by those alone. A cell a background
/// marked out (the selected card, a focused field, the footer, a toast, a session's prompt or diff)
/// flips to reverse video instead, as on a monochrome terminal. Flipped rather than set: the
/// selection reverses its cells, and over such a background it would otherwise vanish.
pub fn drop_colours(buf: &mut Buffer) {
    for cell in &mut buf.content {
        if cell.bg != Color::Reset {
            cell.modifier.toggle(ratatui::style::Modifier::REVERSED);
        }
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
        cell.underline_color = Color::Reset;
    }
}

/// The sidebar's badge. A session in another terminal shows its state with `↗`, which alone
/// means it's there and idle; one that came back waits for Enter.
fn state_badge(tv: &TicketView) -> Span<'static> {
    let outside = || Span::styled("↗ outside", Style::new().fg(Color::Cyan));
    match tv.away {
        // A finished turn you haven't looked at is still news, asleep or not.
        None if tv.asleep && !(tv.status.state == AgentState::Done && !tv.status.seen) => asleep(),
        None => badge(tv.status.state, tv.status.seen, tv.new),
        Some(Away::Leaving(_)) => Span::styled(format!("{} handing off", spinner()), dim()),
        Some(Away::HandedOff) => outside(),
        Some(Away::Back) => Span::styled("↩ back", Style::new().fg(Color::Cyan).bold()),
        Some(Away::Outside) => match tv.status.state {
            AgentState::Idle | AgentState::NotStarted | AgentState::Exited => outside(),
            state => {
                let b = badge(state, tv.status.seen, false);
                Span::styled(format!("{} ↗", b.content), b.style)
            }
        },
    }
}

/// A session stopped to save memory (`TicketView::sleep`), a quiet state: a glyph only.
fn asleep() -> Span<'static> {
    Span::styled("☾", dim())
}

/// Every symbol differs, not just its colour, so the collapsed sidebar, which shows the symbol
/// alone, still tells the states apart.
fn badge(state: AgentState, seen: bool, new: bool) -> Span<'static> {
    let word = state.word();
    match state {
        AgentState::Needs => Span::styled(format!("▲ {word}"), Style::new().fg(yellow()).bold()),
        AgentState::Done if !seen => {
            Span::styled(format!("● {word}"), Style::new().fg(Color::Green).bold())
        }
        AgentState::Done => Span::styled(format!("✓ {word}"), Style::new().fg(Color::Green)),
        AgentState::Working => {
            Span::styled(format!("{} {word}", spinner()), Style::new().fg(Color::Cyan))
        }
        // Added by `taskman add` and not looked at yet.
        AgentState::Idle | AgentState::NotStarted if new => {
            Span::styled("✦ new", Style::new().fg(Color::Magenta).bold())
        }
        // Quiet states get a glyph only, so the ones that need a look stand out.
        AgentState::Idle | AgentState::NotStarted => Span::styled("○", dim()),
        AgentState::Exited if !seen => {
            Span::styled(format!("■ {word}"), Style::new().fg(Color::Red))
        }
        AgentState::Exited => Span::styled("■", dim()),
    }
}

/// A faint background for the selected card, the footer and a focused field (`Theme::selection`).
fn selection_bg() -> Color {
    crate::theme::current().selection
}

/// The PR's summary in a line; `target` is `App::pr_target`, shown after the number so it's the
/// checks that get cut when the line is short. Without GitHub, the branch, if any.
fn pr_spans(app: &App, tv: &TicketView) -> Vec<Span<'static>> {
    if app.gh(&tv.t.repo) != crate::github::Gh::Ready {
        return tv.t.branch.iter().map(|b| Span::styled(untrusted::line(b), dim())).collect();
    }
    let target = app.pr_target(tv);
    let target = target.map(|(label, merged)| match merged {
        true => Span::styled(format!(" →{label} merged"), Style::new().fg(yellow()).bold()),
        false => Span::styled(format!(" →{label}"), dim()),
    });
    match (&tv.pr.data, &tv.pr.error) {
        (None, Some(e)) if e == crate::github::NO_REMOTE => {
            vec![Span::styled("no GitHub remote", dim())]
        }
        (None, Some(_)) => vec![Span::styled("PR ⚠ gh error", Style::new().fg(Color::Red))],
        (None, None) => vec![Span::styled("PR …", dim())],
        (Some(None), _) => [Span::styled("no PR yet", dim())].into_iter().chain(target).collect(),
        (Some(Some(pr)), _) => {
            let mut v = vec![Span::raw(format!("#{}", pr.number))];
            v.extend(target);
            match pr.state.as_str() {
                "MERGED" => v.push(Span::styled(" merged", Style::new().fg(Color::Magenta))),
                "CLOSED" => v.push(Span::styled(" closed", dim())),
                _ if pr.is_draft => v.push(Span::styled(" draft", dim())),
                _ => {}
            }
            for (state, sym, color) in [
                (CheckState::Fail, "✗", Color::Red),
                (CheckState::Pending, "◷", yellow()),
                (CheckState::Pass, "✓", Color::Green),
            ] {
                let n = pr.count(state);
                if n > 0 {
                    v.push(Span::styled(format!(" {sym}{n}"), Style::new().fg(color)));
                }
            }
            match pr.review_decision.as_str() {
                "APPROVED" => v.push(Span::styled(" ✔appr", Style::new().fg(Color::Green))),
                "CHANGES_REQUESTED" => v.push(Span::styled(" ✎chg", Style::new().fg(Color::Red))),
                _ => {}
            }
            v
        }
    }
}

#[cfg(test)]
mod screens;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badges_differ_by_symbol_not_only_colour() {
        let glyph = |state, seen, new| badge(state, seen, new).content.chars().next().unwrap();
        let glyphs = [
            glyph(AgentState::Needs, false, false),
            glyph(AgentState::Done, false, false),
            glyph(AgentState::Done, true, false),
            glyph(AgentState::Idle, true, true),
            glyph(AgentState::Idle, true, false),
            glyph(AgentState::Exited, false, false),
        ];
        for (i, a) in glyphs.iter().enumerate() {
            assert!(!glyphs[i + 1..].contains(a), "{a} is used twice");
        }
    }

    #[test]
    fn dropping_colours_keeps_the_text_and_reverses_backgrounds() {
        use ratatui::style::Modifier;
        let attrs = Modifier::BOLD | Modifier::UNDERLINED;
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        let red = Style::new().fg(Color::Red).add_modifier(attrs);
        buf.set_string(0, 0, "a", red.underline_color(Color::Indexed(42)));
        // A selected cell, on no background and on one.
        buf.set_string(1, 0, "b", red.add_modifier(Modifier::REVERSED));
        buf.set_string(2, 0, "c", Style::new().bg(Color::Rgb(1, 2, 3)).reversed());
        // A background that marks a cell out, as the selected card's does.
        buf.set_string(3, 0, "d", Style::new().fg(Color::Black).bg(Color::Cyan).bold());
        let mut expected = Buffer::empty(buf.area);
        expected.set_string(0, 0, "a", Style::new().add_modifier(attrs));
        expected.set_string(1, 0, "b", Style::new().add_modifier(attrs).reversed());
        expected.set_string(2, 0, "c", Style::new());
        expected.set_string(3, 0, "d", Style::new().bold().reversed());

        drop_colours(&mut buf);
        assert_eq!(buf, expected);
    }

    #[test]
    fn blank_row_above_footer_on_tall_screens() {
        let prefs = UiPrefs::default();
        let bottom = |r: Rect| r.y + r.height;
        for (w, h, panels) in [
            (160, 40, true),
            (160, 40, false),
            (120, 30, true),
            (120, 29, true),
            (120, 20, true),
            (80, 20, false),
        ] {
            let a = layout(Rect::new(0, 0, w, h), &prefs, (panels, panels));
            assert_eq!(a.footer, Rect::new(0, h - 1, w, 1));
            let gap = if h >= FOOTER_GAP_MIN_H { 1 } else { 0 };
            // The tracker panel sits above the GitHub one, so only the lower reaches the bottom.
            let panes = [Some(a.sidebar), Some(a.claude), a.gh.or(a.jira)];
            assert_eq!(panes.iter().flatten().count(), if panels { 3 } else { 2 });
            for r in panes.into_iter().flatten() {
                assert_eq!(bottom(r), a.footer.y - gap, "{w}x{h} panels={panels}: {r:?}");
            }
        }
        // Tiny screens squeeze without panicking.
        for h in 0..4 {
            layout(Rect::new(0, 0, 40, h), &prefs, (true, true));
        }
    }

    #[test]
    fn claude_keeps_sixty_columns() {
        let prefs = UiPrefs::default();
        let at = |w: u16, prefs: &UiPrefs| layout(Rect::new(0, 0, w, 24), prefs, (true, true));
        // Wide: the usual widths.
        let wide = at(160, &prefs);
        assert_eq!((wide.sidebar.width, wide.jira.unwrap().width), (SIDEBAR_W, 53));
        assert!(!wide.sidebar_folded && !wide.panels_hidden);
        // The panels narrow first, then the sidebar folds, then the panels go.
        let a = at(120, &prefs);
        assert_eq!(
            (a.sidebar.width, a.jira.unwrap().width, a.claude.width),
            (SIDEBAR_W, PANELS_MIN_W, 60)
        );
        let a = at(110, &prefs);
        assert_eq!((a.sidebar.width, a.claude.width), (SIDEBAR_COLLAPSED_W, 60));
        assert!(a.sidebar_folded && !a.panels_hidden);
        let a = at(100, &prefs);
        assert_eq!((a.sidebar.width, a.jira, a.claude.width), (SIDEBAR_W, None, 69));
        assert!(!a.sidebar_folded && a.panels_hidden);
        let a = at(80, &prefs);
        assert_eq!(
            (a.sidebar.width, a.jira, a.gh, a.claude.width),
            (SIDEBAR_COLLAPSED_W, None, None, 66)
        );
        assert!(a.sidebar_folded && a.panels_hidden);
        // Folded by the user, it isn't counted as folded by the width.
        let folded = UiPrefs { sidebar_collapsed: true, ..UiPrefs::default() };
        assert!(!at(80, &folded).sidebar_folded);
        // Narrower than that, Claude gets what there is.
        let a = at(50, &prefs);
        assert_eq!((a.sidebar.width, a.claude.width), (SIDEBAR_COLLAPSED_W, 36));
        for w in 0..200 {
            let a = at(w, &prefs);
            assert!(a.sidebar.width + a.claude.width <= w.max(SIDEBAR_COLLAPSED_W), "{w}: {a:?}");
        }
    }

    #[test]
    fn gives_the_column_to_what_is_connected() {
        let prefs = UiPrefs::default();
        let screen = Rect::new(0, 0, 160, 40);
        let both = layout(screen, &prefs, (true, true));
        let (j, g) = (both.jira.unwrap(), both.gh.unwrap());
        assert_eq!((j.x, j.y + j.height), (g.x, g.y));
        // One panel takes the whole column.
        let pr = layout(screen, &prefs, (false, true));
        assert_eq!((pr.jira, pr.gh), (None, Some(j.union(g))));
        let tracker = layout(screen, &prefs, (true, false));
        assert_eq!((tracker.jira, tracker.gh), (Some(j.union(g)), None));
        // Neither: Claude gets the width the column had.
        let none = layout(screen, &prefs, (false, false));
        assert_eq!((none.jira, none.gh), (None, None));
        assert_eq!(none.claude.x + none.claude.width, screen.width);
    }
}
