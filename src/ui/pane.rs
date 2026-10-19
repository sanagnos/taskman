//! The session's pane: its header, the embedded terminal or what stands in for it, the footer
//! and the prefix menu over it.

use super::{dim, selection_bg, spinner, yellow};
use crate::app::{AgentState, App, Away, Mode, TicketView};
use crate::keys::prefix;
use crate::store::{Cli, tilde};
use crate::untrusted;
use crate::util::truncate;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use tui_term::widget::{Cursor, PseudoTerminal};
use unicode_width::UnicodeWidthStr;

/// What the header says about a session away from the pane.
fn away_label(tv: &TicketView) -> Option<String> {
    let state = match tv.status.state {
        s @ (AgentState::Working | AgentState::Needs | AgentState::Done) => s.word(),
        _ => "",
    };
    Some(match tv.away? {
        Away::Leaving(_) => "handing off".into(),
        Away::HandedOff => "handed off".into(),
        Away::Back => "back · Enter to resume here".into(),
        Away::Outside if state.is_empty() => "outside".into(),
        Away::Outside => format!("outside · {state}"),
    })
}

pub(super) fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let Some(tv) = app.selected() else {
        return;
    };
    let label =
        if tv.t.agent.is_some() { format!("✦ {}", tv.t.display()) } else { tv.t.key.clone() };
    let mut spans = vec![Span::styled(
        format!(" {label} "),
        Style::new().fg(Color::Black).bg(Color::Cyan).bold(),
    )];
    if let Some(away) = away_label(tv) {
        spans.push(Span::styled(format!(" {away}"), Style::new().fg(Color::Cyan).bold()));
    }
    if tv.t.cli != Cli::Claude {
        spans.push(Span::styled(
            format!(" {}", tv.t.cli.label().to_lowercase()),
            Style::new().fg(Color::Magenta),
        ));
    }
    // Claude titles its window with the session's --name; the label already shows that.
    let title = tv.window_title();
    let shown = title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    if !shown.is_empty() && !shown.eq_ignore_ascii_case(&tv.t.key) {
        spans.push(Span::raw(format!(" {title}")));
    }
    let branch =
        tv.t.branch.as_deref().map(|b| format!(" {}", untrusted::line(b))).unwrap_or_default();
    let right = format!("{}{branch} ", tilde(&tv.t.cwd));
    let left_len: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    // At least a space between the two sides, however long the folder.
    let room = (area.width as usize).saturating_sub(left_len + 3);
    spans.push(Span::raw(" ".repeat(1 + room.saturating_sub(right.chars().count().min(room)))));
    spans.push(Span::styled(truncate(&right, room), dim()));
    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::new().bg(Color::Reset)), area);
}

pub(super) fn draw_claude(f: &mut Frame, app: &App, area: Rect) {
    let Some(tv) = app.selected() else {
        let text = vec![
            Line::from(""),
            Line::from("Welcome to taskman".bold()),
            Line::from(""),
            Line::from(if app.projects.is_empty() {
                format!("Press {} P to add a project: a folder you work in.", prefix())
            } else {
                format!("Press {} n to add an agent (or a ticket, with a tracker).", prefix())
            }),
            Line::from(Span::styled(
                "Connect a tracker (Jira, Linear, GitHub Issues or Local files) for tickets, each in \
                 its own worktree with its issue and PR beside it.",
                dim(),
            )),
        ];
        f.render_widget(
            Paragraph::new(text).alignment(Alignment::Center).wrap(Wrap { trim: true }),
            area,
        );
        return;
    };
    if tv.session.is_none() || matches!(tv.away, Some(Away::Leaving(_))) {
        let line = |i| app.resume_line(i);
        let i = app.sel;
        let (name, cli) = (tv.t.display(), tv.t.cli.label());
        let command = || Line::from(Span::styled(line(i), Style::new().fg(Color::Cyan).bold()));
        let text = match tv.away {
            None => None,
            Some(Away::Leaving(_)) => Some(vec![Line::from(
                format!("{} Handing {name} off: waiting for {cli} to quit…", spinner()).bold(),
            )]),
            Some(Away::HandedOff) => Some(vec![
                Line::from(format!("{name} is handed off").bold()),
                Line::from(""),
                Line::from("Continue it in another terminal (the command is copied):"),
                command(),
                Line::from(""),
                Line::from(Span::styled("Or press Enter to resume it here.", dim())),
            ]),
            Some(Away::Outside) => Some(vec![
                Line::from(format!("{name} is running in another terminal").bold()),
                Line::from(""),
                Line::from(
                    "Its state shows in the sidebar, and other sessions can still message it.",
                ),
                Line::from(Span::styled(
                    format!("Quit {cli} there (/exit or ctrl+d) to bring it back here."),
                    dim(),
                )),
            ]),
            Some(Away::Back) => Some(vec![
                Line::from(format!("{name} is back").bold()),
                Line::from(""),
                Line::from("Its session in the other terminal ended."),
                Line::from("Press Enter to resume it here, with the turns from there."),
                Line::from(""),
                Line::from(Span::styled("Or run it there again:", dim())),
                command(),
            ]),
        };
        if let Some(text) = text {
            f.render_widget(
                Paragraph::new(text).alignment(Alignment::Center).wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
    }
    if tv.session.is_none()
        && let Some(e) = &tv.lock_error
    {
        let text = vec![
            Line::from(""),
            Line::from(
                format!(
                    "Couldn't take the lock that keeps {}'s session to one copy",
                    tv.t.display()
                )
                .bold(),
            ),
            Line::from(Span::styled(e.clone(), Style::new().fg(Color::Red))),
            Line::from(""),
            Line::from("Fix it, then press Enter to retry."),
            Line::from(Span::styled(
                "The session didn't start: without the lock, taskman resume could start a second copy on the same conversation.",
                dim(),
            )),
        ];
        f.render_widget(
            Paragraph::new(text).alignment(Alignment::Center).wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let failed =
        tv.t.pending_worktree.as_ref().and_then(|w| w.error.as_ref()).filter(|_| !tv.creating);
    match (&tv.session, failed) {
        (Some(s), _) if !s.exited() || tv.status.state != AgentState::Exited => {
            // While text is selected, the session as it was then, so output can't move it.
            if let Some(snapshot) = app.selection.as_ref().and_then(|sel| sel.snapshot.as_ref()) {
                let view = snapshot.view();
                f.render_widget(
                    PseudoTerminal::new(&view).cursor(Cursor::default().visibility(false)),
                    area,
                );
                return;
            }
            let parser = crate::session::lock(&s.parser);
            let screen = parser.screen();
            let scrolled = screen.scrollback();
            let show_cursor = scrolled == 0 && matches!(app.mode, Mode::Normal) && !app.prefix;
            let term =
                PseudoTerminal::new(screen).cursor(Cursor::default().visibility(show_cursor));
            f.render_widget(term, area);
            // From here on, whatever is drawn over the pane must render `Clear` first, or its
            // cells keep the links and their forced widths (see `links::mark`).
            crate::links::mark(f.buffer_mut(), area, screen);
            if scrolled > 0 {
                let msg = format!(" ↑ {scrolled} lines back · type to return ");
                let w = msg.chars().count() as u16;
                let r = Rect {
                    x: area.right().saturating_sub(w + 1),
                    y: area.y,
                    width: w.min(area.width),
                    height: 1,
                };
                // Clear drops the links marked under it.
                f.render_widget(Clear, r);
                f.render_widget(
                    Paragraph::new(msg).style(Style::new().fg(Color::Black).bg(Color::Yellow)),
                    r,
                );
            }
        }
        (None, Some(error)) => {
            let text = vec![
                Line::from(""),
                Line::from(format!("Couldn't create a worktree for {}", tv.t.display()).bold()),
                Line::from(Span::styled(error.clone(), Style::new().fg(Color::Red))),
                Line::from(""),
                Line::from("Press Enter to try again once it's fixed."),
                Line::from(Span::styled(
                    format!(
                        "Or {} M to run the {} session in the main checkout {}, shared with other sessions.",
                        prefix(),
                        tv.t.cli.label(),
                        tilde(&tv.t.repo)
                    ),
                    dim(),
                )),
            ];
            f.render_widget(
                Paragraph::new(text).alignment(Alignment::Center).wrap(Wrap { trim: true }),
                area,
            );
        }
        (None, None) if tv.waiting() => {
            let text = vec![
                Line::from(""),
                Line::from(
                    format!("{} Creating a worktree for {}…", spinner(), tv.t.display()).bold(),
                ),
                Line::from(""),
                Line::from(Span::styled(
                    format!("The {} session starts in it once git is done.", tv.t.cli.label()),
                    dim(),
                )),
            ];
            f.render_widget(Paragraph::new(text).alignment(Alignment::Center), area);
        }
        _ if tv.background.is_some() => {
            let Some(b) = &tv.background else { return };
            let mut text = vec![
                Line::from(""),
                Line::from(
                    format!(
                        "The {} session for {} didn't resume.",
                        tv.t.cli.label(),
                        tv.t.display()
                    )
                    .bold(),
                ),
                Line::from(""),
            ];
            text.extend(
                b.message
                    .lines()
                    .map(|l| Line::from(Span::styled(l.to_string(), Style::new().fg(Color::Red)))),
            );
            text.push(Line::from(""));
            if b.stopping {
                text.push(Line::from(format!("{} Stopping it in the background…", spinner())));
            } else {
                text.push(Line::from(format!(
                    "Press Enter to stop it there and resume it here, or {} x to close the ticket.",
                    prefix()
                )));
            }
            if let Some(e) = &b.error {
                text.push(Line::from(""));
                text.push(Line::from(Span::styled(e.clone(), Style::new().fg(Color::Red))));
            }
            f.render_widget(
                Paragraph::new(text).alignment(Alignment::Center).wrap(Wrap { trim: true }),
                area,
            );
        }
        _ => {
            let text = vec![
                Line::from(""),
                Line::from(
                    format!("The {} session for {} has ended.", tv.t.cli.label(), tv.t.display())
                        .bold(),
                ),
                Line::from(""),
                Line::from(format!(
                    "Press Enter to resume it, or {} x to close the ticket.",
                    prefix()
                )),
            ];
            f.render_widget(Paragraph::new(text).alignment(Alignment::Center), area);
        }
    }
}

/// "selecting · Esc to resume" in a corner of the pane holding a selection, away from it.
pub(super) fn draw_selecting(f: &mut Frame, app: &App) {
    let Some(sel) =
        app.selection.as_ref().filter(|s| s.dragged() && matches!(app.mode, Mode::Normal))
    else {
        return;
    };
    let msg = " selecting · Esc to resume ";
    let w = (msg.chars().count() as u16).min(sel.rect.width);
    let (start, end) = sel.span();
    let shown = crate::selection::on_screen(sel.rect, app.view_top(sel), start, end);
    // The top row unless that's where the selection starts, then the bottom one.
    let y = match shown {
        Some((from, to)) if from.y == sel.rect.y && to.y + 1 < sel.rect.bottom() => {
            sel.rect.bottom() - 1
        }
        _ => sel.rect.y,
    };
    let r = Rect { x: sel.rect.right().saturating_sub(w), y, width: w, height: 1 };
    // A panel's linked cells would keep their links under it (see `links::mark_coded`).
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(msg).style(Style::new().fg(Color::Black).bg(Color::Yellow)), r);
}

pub(super) fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = if app.prefix {
        Line::from(vec![
            Span::styled(
                format!(" {} ", prefix()),
                Style::new().fg(Color::Black).bg(Color::Yellow).bold(),
            ),
            Span::raw(" press a command key · Esc cancels"),
        ])
    } else if let Some(hint) = app.reorder_hint() {
        Line::from(Span::styled(format!(" {hint}"), Style::new().fg(Color::Cyan)))
    } else if let Some((msg, _)) = &app.toast {
        Line::from(Span::styled(format!(" {msg}"), Style::new().fg(yellow())))
    } else {
        let key = Style::new().bold();
        let p = prefix().short();
        let mut v = vec![
            Span::styled(format!(" {p}"), key),
            Span::styled(" menu · ", dim()),
            Span::styled(format!("{p} {p}"), key),
            Span::styled(" next waiting · ", dim()),
            Span::styled(format!("{p} ?"), key),
            Span::styled(" help", dim()),
        ];
        let waiting = app
            .tickets
            .iter()
            .enumerate()
            .filter(|(i, tv)| *i != app.sel && tv.needs_attention())
            .count();
        if waiting > 0 {
            // Right-aligned, like a status line's right side.
            let right = format!("● {waiting} waiting on you ");
            let used: usize = v.iter().map(|s| s.content.chars().count()).sum();
            v.push(Span::raw(
                " ".repeat((area.width as usize).saturating_sub(used + right.chars().count())),
            ));
            v.push(Span::styled(right, Style::new().fg(yellow()).bold()));
        }
        Line::from(v)
    };
    // A full-width bar, as in tmux, so the panes end above a solid edge rather than above text.
    f.render_widget(Paragraph::new(line).style(Style::new().bg(selection_bg())), area);
}

/// The commands available after the prefix, shown while it waits for its key.
pub(super) fn draw_prefix_menu(f: &mut Frame, footer: Rect) {
    let p = prefix().short();
    let (again, send) = (format!("{p} tab"), format!("send {p}"));
    let groups: [(&str, &[(&str, &str)]); 5] = [
        (
            "Tickets",
            &[
                ("1-9", "switch"),
                ("[ ]", "prev / next"),
                ("{ }", "move"),
                (&again, "next waiting"),
                ("/", "jump"),
                ("n", "add ticket/agent"),
                ("x", "close"),
            ],
        ),
        (
            "Projects",
            &[(",", "settings"), ("P", "new project"), ("A", "new account"), ("z", "fold")],
        ),
        (
            "Panels",
            &[
                ("j", "tracker"),
                ("c", "comments"),
                ("g", "GitHub"),
                ("b", "sidebar"),
                ("r", "refresh"),
                ("m", "mark read"),
            ],
        ),
        ("Open", &[("o", "ticket"), ("p", "PR")]),
        (
            "Session",
            &[
                ("R", "restart"),
                ("t", "to a terminal"),
                ("e", "rename agent"),
                ("PgUp", "scroll"),
                ("v", "history in $EDITOR"),
                ("l", "links"),
                ("a", &send),
            ],
        ),
    ];
    // A group that doesn't fit the screen goes on in a row of its own, under its first command.
    const INDENT: usize = 10;
    let room = usize::from(f.area().width.saturating_sub(4));
    let mut lines: Vec<Line> = Vec::new();
    for (title, items) in groups {
        let mut v = vec![Span::styled(format!(" {title:<9}"), Style::new().bold())];
        let mut used = INDENT;
        for (k, what) in items {
            let w = k.width() + 1 + what.width();
            if used > INDENT && used + w > room {
                lines.push(Line::from(std::mem::take(&mut v)));
                v.push(Span::raw(" ".repeat(INDENT)));
                used = INDENT;
            }
            v.push(Span::styled(format!("{k} "), Style::new().fg(yellow()).bold()));
            v.push(Span::raw(format!("{what}   ")));
            used += w + 3;
        }
        lines.push(Line::from(v));
    }
    lines.push(Line::from(vec![
        Span::styled(format!(" {:<9}", ""), Style::new()),
        Span::styled("? ", Style::new().fg(yellow()).bold()),
        Span::raw("help   "),
        Span::styled("q ", Style::new().fg(yellow()).bold()),
        Span::raw("quit   "),
        Span::styled("Esc ", Style::new().fg(yellow()).bold()),
        Span::raw("cancel"),
    ]));
    let w = lines.iter().map(|l| l.width() as u16).max().unwrap_or(40) + 2;
    let h = lines.len() as u16 + 2;
    let area = f.area();
    let r = Rect {
        x: area.x + 1,
        y: footer.y.saturating_sub(h),
        width: w.min(area.width.saturating_sub(2)),
        height: h.min(footer.y),
    };
    f.render_widget(Clear, r);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(yellow()));
    f.render_widget(Paragraph::new(lines).block(block), r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    /// The rest of a line cleared while underline is on: the blank cells take the background
    /// colour and nothing else (the vt100 fork's back colour erase), so no underline is drawn
    /// across them.
    #[test]
    fn cleared_cells_draw_no_underline() {
        use ratatui::style::Modifier;
        let mut parser = vt100::Parser::new(1, 8, 0);
        parser.process(b"\x1b[4;44mabc\x1b[K");
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(PseudoTerminal::new(parser.screen()), area, &mut buf);
        for x in 0..3 {
            assert!(buf[(x, 0)].modifier.contains(Modifier::UNDERLINED), "column {x}");
        }
        for x in 3..8 {
            let cell = &buf[(x, 0)];
            assert!(!cell.modifier.contains(Modifier::UNDERLINED), "column {x} is underlined");
            assert_eq!(cell.bg, Color::Indexed(4), "column {x}");
        }
    }
}
