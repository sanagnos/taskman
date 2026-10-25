//! What draws over the screen: confirm dialogs, links, forms, settings and help.

use super::sidebar::{more_line, scroll_to_show, shown_rows};
use super::{asleep, badge, dim, selection_bg, spinner};
use crate::app::{AgentAsk, AgentState, App, Form, Level, Mode, Scroll, SettingsItem, toasts};
use crate::keys::prefix;
use crate::store::tilde;
use crate::untrusted;
use crate::util::{self, pad, truncate, truncate_middle, truncate_start, wrap_words};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};
use std::cell::Cell;
use unicode_segmentation::UnicodeSegmentation as _;

/// The dialog over the screen in `app.mode`, if any.
pub(super) fn draw_dialog(f: &mut Frame, app: &mut App) {
    match &app.mode {
        // The jump box is the sidebar's own top row; move mode shows in the sidebar and footer.
        Mode::Normal | Mode::Jump(_) | Mode::Move(_) => {}
        Mode::Help(_) => draw_help(f, app),
        Mode::Links(_) => draw_links(f, app),
        Mode::Queue(_) => draw_queue(f, app),
        Mode::Log(_) => draw_log(f, app),
        Mode::Form(_) => draw_form(f, app),
        Mode::Settings(_) => draw_settings(f, app),
        Mode::ConfirmDeleteAccount(i) => {
            let label = app.config.accounts.get(*i).map(|a| a.label()).unwrap_or_default();
            let question = format!("Delete account {label}?");
            match app.own_keychain_item(*i) {
                Some(name) => draw_confirm_lines(
                    f,
                    &question,
                    vec![Line::from(Span::styled(
                        format!(
                            "No project uses it. Its token is in your {} as taskman {name}.",
                            app.keychain.as_ref().map_or_else(
                                || crate::keychain::Keychain::os().label(),
                                |k| k.label()
                            )
                        ),
                        dim(),
                    ))],
                    &[
                        ("Enter", "delete, keep the token"),
                        ("d", "delete the token too"),
                        ("Esc", "cancel"),
                    ],
                ),
                None => draw_confirm(
                    f,
                    &question,
                    "No project uses it. The token itself (variable or password manager) is untouched.",
                ),
            }
        }
        Mode::ConfirmClose => {
            let (name, agent) = app
                .selected()
                .map(|tv| (tv.t.display().to_string(), tv.t.agent.is_some()))
                .unwrap_or_default();
            let question = format!("Close {name}?");
            match &app.close_check {
                _ if agent => draw_confirm(
                    f,
                    &question,
                    "Stops its session. Its conversation is kept: add an agent with the same name to resume it.",
                ),
                Some(c) if c.safe() || c.safe_but_ignored() => {
                    // Deleting ignored files has a key of its own, which says so, as Enter
                    // says it keeps them.
                    let keys: &[(&str, &str)] = if c.safe() {
                        &[("Enter", "close"), ("r", "close and remove"), ("Esc", "cancel")]
                    } else {
                        &[
                            ("Enter", "close, keep the worktree"),
                            ("d", "close and delete them"),
                            ("Esc", "cancel"),
                        ]
                    };
                    let room = confirm_width(f, keys).saturating_sub(2 + DIALOG_PAD_W) as usize;
                    let branch = c.branch.as_deref().map(untrusted::line);
                    let body = removal_lines(&tilde(&c.path), branch.as_deref(), &c.ignored, room);
                    draw_confirm_lines(f, &question, body, keys);
                }
                Some(c) => {
                    let mut has = c.problems.clone();
                    if !c.ignored.is_empty() {
                        has.push("ignored files".into());
                    }
                    let detail = format!(
                        "Stops its session. The worktree and branch are kept: it has {}.",
                        join_and(&has)
                    );
                    draw_confirm(f, &question, &detail);
                }
                None => {
                    let kept = "Stops its session. The worktree and branch are kept";
                    let detail = match &app.close_unchecked {
                        Some(why) => format!("{kept}: they couldn't be checked ({why})."),
                        None => format!("{kept}."),
                    };
                    draw_confirm(f, &question, &detail);
                }
            }
        }
        Mode::ConfirmDeleteProject(p) => {
            let (name, agents, tickets) = app
                .projects
                .get(*p)
                .map(|proj| {
                    let open = app.tickets.iter().filter(|tv| tv.t.repo == proj.repo);
                    let agents = open.clone().filter(|tv| tv.t.agent.is_some()).count();
                    (proj.name.clone(), agents, open.count() - agents)
                })
                .unwrap_or_default();
            draw_confirm(
                f,
                &format!("Delete project {name}?"),
                &format!(
                    "{} The repo, worktrees, branches and accounts are kept.",
                    closes_too(agents, tickets)
                ),
            );
        }
        Mode::ConfirmQuit => {
            let resume = "Running sessions stop; they resume when you reopen taskman.";
            let detail = match app.quit_would_wait_for().as_slice() {
                [] => resume.to_string(),
                [one] => format!("{resume} It quits once {one}'s worktree is removed."),
                many => format!(
                    "{resume} It quits once the worktrees of {} are removed.",
                    join_and(&many.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                ),
            };
            draw_confirm(f, "Quit taskman?", &detail);
        }
        Mode::ConfirmRestart((repo, key)) => {
            let Some(tv) = app.tickets.iter().find(|tv| tv.t.is(repo, key)) else { return };
            // Read as it's drawn: the turn may end while the dialog is open.
            let lost = match tv.status.state {
                AgentState::Working => {
                    "It's working: its current turn ends, and any command it's running is stopped part-way."
                }
                AgentState::Needs => {
                    "It's waiting on you: what it asked goes unanswered, and its current turn stops."
                }
                _ => "Its turn has ended, so nothing is cut short.",
            };
            draw_confirm(
                f,
                &format!("Restart {}?", tv.t.display()),
                &format!("{lost} The conversation resumes."),
            );
        }
        Mode::ConfirmAgent(ask) if !ask.req.new_tickets().is_empty() => draw_new_ask(f, app),
        Mode::ConfirmAgent(_) => draw_agent_ask(f, app),
        Mode::OfferTmux((repo, key)) => {
            let Some(i) = app.tickets.iter().position(|tv| tv.t.is(repo, key)) else { return };
            let line = app.resume_line(i);
            draw_confirm_lines(
                f,
                &format!("Continue {} in a new tmux window?", app.tickets[i].t.display()),
                vec![
                    Line::from("It runs there with its brief, hooks and name:"),
                    Line::from(Span::styled(format!("  {line}"), Style::new().fg(Color::Cyan))),
                    Line::from(Span::styled(
                        "The command is copied too, for any other terminal.",
                        dim(),
                    )),
                ],
                &[("Enter", "open window"), ("Esc", "not now")],
            );
        }
    }
}

/// The ticket's links, then the session's newest first (`prefix l`): the text each showed, its URL
/// under it. Links that don't open (not http(s)) are dimmed, and can be copied.
fn draw_links(f: &mut Frame, app: &mut App) {
    let Mode::Links(list) = &mut app.mode else { return };
    let w = (100 + DIALOG_PAD_W).min(f.area().width.saturating_sub(2));
    let room = usize::from(w.saturating_sub(2 + DIALOG_PAD_W + 2));
    let both = list.panel > 0 && list.panel < list.links.len();
    // Two rows a link, under its group's heading when there are both groups; `at` is where each
    // link starts.
    let mut rows = Vec::new();
    let mut at = Vec::new();
    for (i, link) in list.links.iter().enumerate() {
        if both && (i == 0 || i == list.panel) {
            let heading = if i == 0 { "This ticket" } else { "The session, newest first" };
            rows.push(Line::from(Span::styled(heading, dim().bold())));
        }
        at.push(rows.len());
        let openable = crate::links::openable(&link.url);
        let marker = if i == list.sel { "› " } else { "  " };
        let mut style = if openable { Style::new() } else { dim() };
        if i == list.sel {
            style = style.add_modifier(ratatui::style::Modifier::REVERSED);
        }
        let url = truncate(&untrusted::line(&link.url), room);
        let (top, under) = match &link.label {
            Some(label) => (truncate(&untrusted::line(label), room), Some(url)),
            None => (url, None),
        };
        rows.push(Line::from(vec![Span::raw(marker), Span::styled(top, style.bold())]));
        let note = if openable { "" } else { "  (doesn't open: c copies it)" };
        let under = under.unwrap_or_default();
        rows.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                truncate(&under, room.saturating_sub(util::width(note))),
                dim().fg(Color::Cyan),
            ),
            Span::styled(note, dim()),
        ]));
    }
    // The list, a blank row and the keys, inside the borders and padding, with two rows spare
    // above and below the dialog.
    let view_h = usize::from(f.area().height.saturating_sub(2 + DIALOG_PAD_H + 2 + 4)).max(1);
    let row = at[list.sel];
    let header = (both && (list.sel == 0 || list.sel == list.panel)).then(|| row - 1);
    let total = rows.len();
    list.top = scroll_to_show(list.top, total, view_h, row..row + 2, header);
    let (first, end) = shown_rows(list.top, total, view_h);
    // A link cut by an edge of the view shows neither of its rows, and counts as hidden.
    let cut = |r: usize| {
        (r == first && r > 0 && at.contains(&(r - 1))) || (r + 1 == end && at.contains(&r))
    };
    let mut text = Vec::new();
    if first > 0 {
        let above = at.iter().filter(|&&r| r < first).count();
        text.push(more_line("↑", above, false, room));
    }
    for (r, line) in rows.into_iter().enumerate().take(end).skip(first) {
        text.push(if cut(r) { Line::from("") } else { line });
    }
    if end < total {
        let below = at.iter().filter(|&&r| r + 2 > end).count();
        text.push(more_line("↓", below, false, room));
    }
    text.push(Line::from(""));
    let keys = [("↑/↓", "choose"), ("Enter", "open"), ("c", "copy"), ("Esc", "close")];
    text.extend(key_rows(&keys, usize::from(w.saturating_sub(2 + DIALOG_PAD_W))));
    let h = u16::try_from(text.len()).unwrap_or(u16::MAX) + 2 + DIALOG_PAD_H;
    let r = centered(f, w, h);
    f.render_widget(Clear, r);
    let title = match list.panel {
        0 => "Links, newest first",
        n if n == list.links.len() => "This ticket's links",
        _ => "Links",
    };
    f.render_widget(Paragraph::new(text).block(dialog(title).padding(DIALOG_PAD)), r);
}

/// The waiting-on-you queue: a row per session, oldest first, with how long it has waited, why,
/// and what it asks for. Everything the CLI wrote goes through `untrusted::line`, cut to fit.
fn draw_queue(f: &mut Frame, app: &mut App) {
    let rows = app.queue();
    let at = match &mut app.mode {
        Mode::Queue(q) => {
            q.at = q.at.min(rows.len().saturating_sub(1));
            q.at
        }
        _ => return,
    };
    let w = (100 + DIALOG_PAD_W).min(f.area().width.saturating_sub(2));
    let room = usize::from(w.saturating_sub(2 + DIALOG_PAD_W + 2));
    let mut text = Vec::new();
    // Two rows a session, as many as fit with two rows spare above and below the dialog, the
    // blank row and the keys.
    let view_h = usize::from(f.area().height.saturating_sub(2 + DIALOG_PAD_H + 2 + 2));
    let shown = (view_h / 2).max(1);
    let first = (at + 1).saturating_sub(shown);
    if rows.is_empty() {
        text.push(Line::from(Span::styled("Nothing needs you", dim())));
    }
    for (n, &i) in rows.iter().enumerate().skip(first).take(shown) {
        let tv = &app.tickets[i];
        let chosen = n == at;
        let label = match tv.t.agent {
            Some(_) => format!("✦ {}", tv.t.display()),
            None => tv.t.key.clone(),
        };
        let project = app.project_of(&tv.t).map(|p| app.projects[p].name.as_str()).unwrap_or("");
        let age = tv.status.waiting_since.map_or_else(|| "?".to_string(), crate::util::ago);
        let why = if tv.status.permission { "permission" } else { "question" };
        let mut style = Style::new().bold();
        if chosen {
            style = style.add_modifier(ratatui::style::Modifier::REVERSED);
        }
        text.push(Line::from(vec![
            Span::raw(if chosen { "› " } else { "  " }),
            Span::styled(truncate(&untrusted::line(&label), 28), style),
            Span::styled(format!("  {}", truncate(&untrusted::line(project), 20)), dim()),
            Span::styled(format!("  {age:>3}  "), Style::new().fg(super::yellow()).bold()),
            Span::raw(why),
        ]));
        let ask = match (&tv.status.tool, &tv.status.input) {
            (Some(tool), Some(input)) => format!("{tool}: {input}"),
            (None, Some(input)) => input.clone(),
            _ => tv.status.message.clone(),
        };
        text.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(truncate(&untrusted::line(&ask), room), dim()),
        ]));
    }
    if rows.len() > first + shown {
        text.push(Line::from(Span::styled(
            format!("↓ {} more", rows.len() - first - shown),
            dim(),
        )));
    }
    text.push(Line::from(""));
    let keys = [("↑/↓", "choose"), ("Enter", "go to it"), ("Esc", "close")];
    text.extend(key_rows(&keys, usize::from(w.saturating_sub(2 + DIALOG_PAD_W))));
    let h = u16::try_from(text.len()).unwrap_or(u16::MAX) + 2 + DIALOG_PAD_H;
    let r = centered(f, w, h);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(text).block(dialog("Needs you, oldest first").padding(DIALOG_PAD)),
        r,
    );
}

/// The notices: every toast shown, newest first (`prefix ~`): how long ago, then the text, wrapped
/// under itself. Errors are red and warnings yellow.
fn draw_log(f: &mut Frame, app: &mut App) {
    let Mode::Log(view) = &mut app.mode else { return };
    let w = (100 + DIALOG_PAD_W).min(f.area().width.saturating_sub(2));
    let inner = usize::from(w.saturating_sub(2 + DIALOG_PAD_W)).max(1);
    let age_w = 5;
    let mut text = Vec::new();
    for entry in &app.toast_log {
        let style = match entry.level {
            Level::Info => Style::new(),
            Level::Warn => Style::new().fg(super::yellow()),
            Level::Error => Style::new().fg(Color::Red).bold(),
        };
        let msg = untrusted::line(&entry.msg);
        for (i, l) in wrap_words(&msg, inner.saturating_sub(age_w).max(1)).into_iter().enumerate() {
            let left = if i == 0 {
                format!("{:<age_w$}", toasts::age(entry.ts))
            } else {
                " ".repeat(age_w)
            };
            text.push(Line::from(vec![Span::styled(left, dim()), Span::styled(l, style)]));
        }
    }
    let n = text.len();
    // Rows for the entries: what the screen leaves, less the border, padding, the line saying what
    // this is with a blank row under it, and a blank row and the keys. Those take a row, or when
    // the entries scroll as many as their longest form wraps to, so the entries' rows stay put.
    let left = usize::from(f.area().height.saturating_sub(2 + 2 + DIALOG_PAD_H + 3));
    let longest = format!("{n}-{n} of {n}");
    let keys_h = if n < left {
        1
    } else {
        key_rows(&scroll_hints(SCROLL_KEYS, Some(&longest)), inner).len()
    };
    let room = left.saturating_sub(keys_h).max(1);
    let rows = n.min(room);
    let rect =
        centered(f, w, u16::try_from(rows + keys_h).unwrap_or(u16::MAX) + 2 + DIALOG_PAD_H + 3);
    view.max = n.saturating_sub(rows);
    view.page = rows.saturating_sub(1).max(1);
    view.top = view.top.min(view.max);
    let at = format!("{}-{} of {n}", view.top + 1, (view.top + rows).min(n));
    let keys = scroll_hints(SCROLL_KEYS, (view.max > 0).then_some(at.as_str()));
    let mut shown = vec![
        Line::from(Span::styled("What the footer showed, newest first.", dim())),
        Line::from(""),
    ];
    shown.extend(text.into_iter().skip(view.top).take(rows));
    shown.push(Line::from(""));
    shown.extend(key_rows(&keys, inner));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(shown).block(dialog("Notices").padding(DIALOG_PAD)), rect);
}

/// The most of a role the dialog shows; a longer one says how much is left out.
const ROLE_SHOWN: usize = 600;

/// The most of who asks that the question shows: anything can write the inbox.
const FROM_SHOWN: usize = 40;

const AGENT_ASK_NOTE: &str = "Sessions run taskman add without asking you, and text they read, such as an issue or \
     a PR comment, can tell them to. Approve it only if you expect it.";

/// The dialog for an agent `taskman add` asked for: who asks, the agent, and its role in full
/// (up to `ROLE_SHOWN`), since the role goes into the agent's system prompt.
fn draw_agent_ask(f: &mut Frame, app: &mut App) {
    let Mode::ConfirmAgent(ask) = &mut app.mode else { return };
    let keys = [("Enter", "approve"), ("Esc", "decline")];
    let inner = usize::from(ask_width(f, &keys).saturating_sub(2 + DIALOG_PAD_W)).max(1);
    let (question, body) = agent_ask_text(ask, inner);
    draw_ask(f, &question, body, AGENT_ASK_NOTE, &keys, &mut ask.view);
}

/// The question and the body, in rows `width` columns wide.
fn agent_ask_text(ask: &AgentAsk, width: usize) -> (String, Vec<Line<'static>>) {
    let (name, role) = ask.req.agent().unwrap_or_default();
    let name = crate::store::slug(name);
    let from = ask
        .req
        .from
        .as_deref()
        .map_or("taskman add".to_string(), |f| truncate(&untrusted::line(f), FROM_SHOWN));
    let question = match (ask.open, role) {
        (true, Some(_)) => format!("{from} asks to change ✦ {name}'s role"),
        _ => format!("{from} asks to add agent ✦ {name}"),
    };
    let intro = match (ask.open, role) {
        (true, Some(_)) => "The role goes into its startup brief when its session next starts:",
        (true, None) => "It's open already, so this changes nothing.",
        (false, Some(_)) => "Its session starts at once, with this role in its startup brief:",
        (false, None) => "Its session starts at once in the project's main checkout.",
    };
    let mut body: Vec<Line> = wrap_words(intro, width).into_iter().map(Line::from).collect();
    if let Some(role) = role {
        let shown: String = role.chars().take(ROLE_SHOWN).collect();
        body.extend(quoted(&shown, width));
        let more = role.chars().count().saturating_sub(ROLE_SHOWN);
        if more > 0 {
            body.push(Line::from(Span::styled(format!("… and {more} more characters"), dim())));
        }
    }
    (question, body)
}

/// Text someone else wrote, such as an agent's role, in rows `width` columns wide that each start
/// with a quote bar. It's wrapped here rather than by the paragraph, since a row the paragraph
/// wrapped would start at the dialog's margin and could pass for taskman's own; cleaned as
/// `untrusted::block` cleans it, and a run of blank lines kept to one.
fn quoted(text: &str, width: usize) -> Vec<Line<'static>> {
    let bar = || Span::styled("│ ", dim());
    let mut rows = Vec::new();
    let mut blank = false;
    for line in untrusted::clean(text).trim().lines() {
        if line.trim().is_empty() {
            if !blank {
                rows.push(Line::from(bar()));
            }
            blank = true;
            continue;
        }
        blank = false;
        for row in wrap_words(line, width.saturating_sub(2)) {
            rows.push(Line::from(vec![bar(), Span::raw(row).italic()]));
        }
    }
    rows
}

/// The hints of a dialog whose body scrolls: the scroll keys before `keys`.
fn with_scroll<'a>(keys: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    std::iter::once(("↑/↓", "scroll")).chain(keys.iter().copied()).collect()
}

/// `draw_ask`'s width: `confirm_width`, with room for the scroll keys whether they show or not.
fn ask_width(f: &Frame, keys: &[(&str, &str)]) -> u16 {
    confirm_width(f, &with_scroll(keys))
}

/// A dialog asking about text someone else wrote, such as an agent's role: the question, the
/// `note` on what answering does and the keys stay on screen, and the body between them, in rows
/// already wrapped to the dialog's width (`ask_width`), scrolls when the screen can't fit it, an
/// edge row saying how many rows are hidden past it (`draw_scrolled`). On a screen too short for
/// that, the note goes first.
fn draw_ask(
    f: &mut Frame,
    question: &str,
    body: Vec<Line<'static>>,
    note: &str,
    keys: &[(&str, &str)],
    view: &mut Scroll,
) {
    let w = ask_width(f, keys);
    let inner = usize::from(w.saturating_sub(2 + DIALOG_PAD_W)).max(1);
    let head: Vec<Line> =
        wrap_words(question, inner).into_iter().map(|l| Line::from(l.bold())).collect();
    let mut note: Vec<Line> =
        wrap_words(note, inner).into_iter().map(|l| Line::from(Span::styled(l, dim()))).collect();
    // Rows inside the border and padding, with a row of the screen left above and below.
    let room = usize::from(f.area().height.saturating_sub(2 + 2 + DIALOG_PAD_H));
    // The question, the note and the keys, each with a blank row after the one before.
    let around =
        |note: usize, keys: usize| head.len() + 1 + note + usize::from(note > 0) + 1 + keys;
    let plain = key_rows(keys, inner);
    let keys = if around(note.len(), plain.len()) + body.len() <= room {
        plain
    } else {
        key_rows(&with_scroll(keys), inner)
    };
    // At least a few rows of the body, so an edge row has rows beside it.
    const LEAST: usize = 3;
    if around(note.len(), keys.len()) + LEAST.min(body.len()) > room {
        note.clear();
    }
    let view_h = room.saturating_sub(around(note.len(), keys.len())).max(LEAST).min(body.len());
    let mut foot = Vec::new();
    if !note.is_empty() {
        foot.push(Line::from(""));
        foot.extend(note);
    }
    foot.push(Line::from(""));
    foot.extend(keys);
    let rows = head.len() + 1 + view_h + foot.len();
    let r = centered(f, w, u16::try_from(rows).unwrap_or(u16::MAX) + 2 + DIALOG_PAD_H);
    f.render_widget(Clear, r);
    let block = dialog("Confirm").padding(DIALOG_PAD);
    let area = block.inner(r);
    f.render_widget(block, r);
    // The question and a blank row, then the body scrolling above the note and keys.
    let head_h = u16::try_from(head.len() + 1).unwrap_or(u16::MAX).min(area.height);
    f.render_widget(Paragraph::new(head), Rect { height: head_h, ..area });
    let mut text = Scrolled { foot, ..Scrolled::default() };
    body.into_iter().for_each(|l| text.body.push(l));
    let below = Rect { y: area.y + head_h, height: area.height - head_h, ..area };
    draw_scrolled(f, below, &text, view);
}

/// The most of a new ticket's title or description the dialog shows: far more than a ticket
/// needs, and it scrolls, but drawing stays quick whatever a request holds. A longer one says how
/// much is left out.
const NEW_SHOWN: usize = 20_000;

const NEW_ASK_NOTE: &str = "Sessions run taskman add without asking you, and text they read, such as an issue or \
     a PR comment, can tell them to put a secret in a ticket. Approve it only if you expect it and it holds nothing \
     private.";

/// The dialog for tickets a session's `taskman add --new` asked to create: who asks, and each
/// one's title and description in full, since the tracker publishes them under the user's
/// account.
fn draw_new_ask(f: &mut Frame, app: &mut App) {
    let Mode::ConfirmAgent(ask) = &app.mode else { return };
    let project = app.projects.iter().find(|p| p.repo == ask.req.repo);
    let tracker = project.and_then(|p| p.tracker.as_ref()).map(|t| t.provider.label());
    let place = tracker.unwrap_or("the tracker");
    let Mode::ConfirmAgent(ask) = &mut app.mode else { return };
    let keys = [("Enter", "create"), ("Esc", "decline")];
    let inner = usize::from(ask_width(f, &keys).saturating_sub(2 + DIALOG_PAD_W)).max(1);
    let (question, body) = new_ask_text(ask, place, inner);
    draw_ask(f, &question, body, NEW_ASK_NOTE, &keys, &mut ask.view);
}

/// The question and the body for tickets to create in `place` (the tracker), in rows `width`
/// columns wide.
fn new_ask_text(ask: &AgentAsk, place: &str, width: usize) -> (String, Vec<Line<'static>>) {
    let wanted = ask.req.new_tickets();
    let from = ask
        .req
        .from
        .as_deref()
        .map_or("taskman add".to_string(), |f| truncate(&untrusted::line(f), FROM_SHOWN));
    let question = match wanted.len() {
        1 => format!("{from} asks to create a ticket in {place}"),
        n => format!("{from} asks to create {n} tickets in {place}"),
    };
    let intro = "taskman creates it with the project's account, as you, then adds it and starts its session.";
    let mut body: Vec<Line> = wrap_words(intro, width).into_iter().map(Line::from).collect();
    let quote = |text: &str, body: &mut Vec<Line<'static>>| {
        let shown: String = text.chars().take(NEW_SHOWN).collect();
        body.extend(quoted(&shown, width));
        let more = text.chars().count().saturating_sub(NEW_SHOWN);
        if more > 0 {
            let more = format!("… and {more} more characters, not shown");
            body.push(Line::from(Span::styled(more, Style::new().bold())));
        }
    };
    for (title, description) in wanted {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled("Title", dim())));
        quote(title, &mut body);
        body.push(Line::from(Span::styled("Description", dim())));
        if description.trim().is_empty() {
            body.push(Line::from(Span::styled("(none)", dim())));
        } else {
            quote(description, &mut body);
        }
    }
    (question, body)
}

fn centered(f: &Frame, w: u16, h: u16) -> Rect {
    let a = f.area();
    let w = w.min(a.width.saturating_sub(2));
    let h = h.min(a.height.saturating_sub(2));
    Rect { x: a.x + (a.width - w) / 2, y: a.y + (a.height - h) / 2, width: w, height: h }
}

fn dialog(title: &str) -> Block<'_> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(Span::styled(format!(" {title} "), Style::new().bold()))
}

const CONFIRM_W: u16 = 68;
/// Space inside a dialog's border: two columns each side, a blank row under the title and one at
/// the bottom. Sizes add `DIALOG_PAD_W` and `DIALOG_PAD_H` to fit it.
const DIALOG_PAD: Padding = Padding::new(2, 2, 1, 1);
const DIALOG_PAD_W: u16 = 4;
const DIALOG_PAD_H: u16 = 2;

const YES_NO: &[(&str, &str)] = &[("Enter", "yes"), ("Esc", "cancel")];

/// The question, a blank line, the body, a blank line and the keys, for a dialog `width` columns
/// wide inside. No trimming when wrapping, so indented body lines keep their indent.
fn confirm_text<'a>(
    question: &str,
    body: Vec<Line<'a>>,
    keys: &[(&str, &str)],
    width: u16,
) -> Paragraph<'a> {
    let mut text = vec![Line::from(question.to_string().bold()), Line::from("")];
    text.extend(body);
    text.push(Line::from(""));
    text.extend(key_rows(keys, usize::from(width)));
    Paragraph::new(text).wrap(Wrap { trim: false })
}

/// Columns between one key's hint and the next.
const KEY_GAP: &str = "   ";

/// Every dialog's and form's key hints: each key in bold and what it does, `KEY_GAP` apart, in
/// as many rows of `width` columns as they take. A row breaks between hints, never inside one,
/// and a hint wider than a row is cut. A hint with no key is a note, such as how far a list is
/// scrolled.
fn key_rows(keys: &[(&str, &str)], width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row: Vec<Span> = Vec::new();
    let mut used = 0;
    for &(key, what) in keys {
        let mut hint = Vec::new();
        if key.is_empty() {
            hint.push(Span::raw(truncate(what, width)));
        } else {
            hint.push(truncate(key, width).bold());
            let room = width.saturating_sub(util::width(key) + 1);
            if room > 0 {
                hint.push(Span::raw(format!(" {}", truncate(what, room))));
            }
        }
        let w: usize = hint.iter().map(|s| util::width(&s.content)).sum();
        if used > 0 && used + KEY_GAP.len() + w > width {
            rows.push(Line::from(std::mem::take(&mut row)));
            used = 0;
        }
        if used > 0 {
            row.push(Span::raw(KEY_GAP));
            used += KEY_GAP.len();
        }
        row.extend(hint);
        used += w;
    }
    if !row.is_empty() {
        rows.push(Line::from(row));
    }
    rows
}

/// The key hints on one row, however wide: to measure them.
fn key_row(keys: &[(&str, &str)]) -> Line<'static> {
    key_rows(keys, usize::MAX).pop().unwrap_or_default()
}

/// The keys that scroll help and the notices.
const SCROLL_KEYS: &str = "↑/↓ PgUp/PgDn";

/// The hints under a list that scrolls: `scroll` names its keys, `at` where it is, such as
/// `1-20 of 40`, or None when it shows all of it.
fn scroll_hints<'a>(scroll: &'a str, at: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    match at {
        Some(at) => vec![(scroll, "scroll"), ("", at), ("other keys", "close")],
        None => vec![("", "Press any key to close.")],
    }
}

/// Box height for a confirm dialog `width` columns wide, so the wrapped text and the key row
/// always fit.
fn confirm_height(question: &str, body: Vec<Line>, keys: &[(&str, &str)], width: u16) -> u16 {
    let inner_w = width.saturating_sub(2 + DIALOG_PAD_W).max(1);
    // line_count counts a block's rows but not its columns, so wrap at the inner width without one.
    let lines = confirm_text(question, body, keys, inner_w).line_count(inner_w) as u16;
    lines + 2 + DIALOG_PAD_H
}

/// A confirm dialog's width: `CONFIRM_W`, or as wide as its key row needs, since a key row that
/// wraps reads badly; within the screen either way.
fn confirm_width(f: &Frame, keys: &[(&str, &str)]) -> u16 {
    let row = u16::try_from(util::line_width(&key_row(keys))).unwrap_or(u16::MAX);
    CONFIRM_W.max(row.saturating_add(2 + DIALOG_PAD_W)).min(f.area().width.saturating_sub(2))
}

fn draw_confirm(f: &mut Frame, question: &str, detail: &str) {
    draw_confirm_lines(
        f,
        question,
        vec![Line::from(Span::styled(detail.to_string(), dim()))],
        YES_NO,
    );
}

fn draw_confirm_lines(f: &mut Frame, question: &str, body: Vec<Line>, keys: &[(&str, &str)]) {
    let w = confirm_width(f, keys);
    let r = centered(f, w, confirm_height(question, body.clone(), keys, w));
    f.render_widget(Clear, r);
    let inner_w = r.width.saturating_sub(2 + DIALOG_PAD_W);
    f.render_widget(
        confirm_text(question, body, keys, inner_w).block(dialog("Confirm").padding(DIALOG_PAD)),
        r,
    );
}

/// The close dialog's account of what `r` removes, or `d` when the worktree has `ignored` files,
/// which it deletes: the folder, the merged branch and the ignored files on lines of their own,
/// shortened to `width` rather than broken mid-name.
fn removal_lines(
    path: &str,
    branch: Option<&str>,
    ignored: &[String],
    width: usize,
) -> Vec<Line<'static>> {
    const MERGED: &str = "  (merged branch)";
    let room = |note: &str| width.saturating_sub(2 + util::width(note));
    let item = |s: String, note: &str| {
        Line::from(vec![Span::raw(format!("  {s}")), Span::styled(note.to_string(), dim())])
    };
    let dim_line = |s: &'static str| Line::from(Span::styled(s, dim()));
    // A path keeps its end (the folder's name), a branch its start (the ticket key).
    let path = truncate_start(path, room(""));
    let mut lines = if ignored.is_empty() {
        vec![
            dim_line("Stops its session. Nothing in its worktree would be lost."),
            Line::from(""),
            dim_line("r also removes:"),
        ]
    } else {
        // Plainly that removing deletes them: the key that does is in the key row, beside what
        // Enter keeps.
        vec![
            dim_line("Stops its session. Its worktree has files git ignores."),
            Line::from(""),
            dim_line("Removing it also deletes files git ignores (not saved anywhere else):"),
            item(
                truncate(&format!("ignored: {}", crate::worktree::some_of(ignored)), room("")),
                "",
            ),
        ]
    };
    lines.push(item(path, ""));
    match branch {
        Some(b) => lines.push(item(truncate(b, room(MERGED)), MERGED)),
        None => {
            lines.push(Line::from(Span::styled("Its branch isn't merged, so it stays.", dim())));
        }
    }
    lines
}

/// What deleting a project with `agents` agents and `tickets` tickets closes with it.
fn closes_too(agents: usize, tickets: usize) -> String {
    let count = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let what = match (agents, tickets) {
        (0, 0) => return "It has no tickets or agents.".into(),
        (0, 1) => "ticket".to_string(),
        (1, 0) => "agent".to_string(),
        (0, t) => count(t, "ticket"),
        (a, 0) => count(a, "agent"),
        (a, t) => format!("{} and {}", count(t, "ticket"), count(a, "agent")),
    };
    match agents + tickets {
        1 => format!("Its {what} closes too, and its session stops."),
        _ => format!("Its {what} close too, and their sessions stop."),
    }
}

/// `a`, `a and b`, `a, b and c`.
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The keys a form takes while one of its fields, `focused`, is being edited: ↑/↓ while it lists
/// issues or presets to pick from, ←/→ for a choice.
fn editing_keys(focused: &crate::app::Field) -> Vec<(&'static str, &'static str)> {
    if focused.url.is_some() {
        return vec![("Enter", "open"), ("c", "copy the link"), ("Tab", "next")];
    }
    let enter = if focused.list.at.is_some() && !focused.presets.is_empty() {
        ("Enter", "pick")
    } else {
        ("Enter", "save")
    };
    let mut keys = vec![enter, ("Tab", "next field")];
    if focused.list.picks > 0 {
        keys.insert(0, ("↑/↓", "choose"));
    } else if !focused.is_text() {
        keys.push(("←/→", "change choice"));
    }
    keys
}

fn draw_form(f: &mut Frame, app: &App) {
    let Mode::Form(form) = &app.mode else { return };
    let mut keys = editing_keys(&form.fields[form.focus]);
    keys.extend([("ctrl+u", "clear"), ("Esc", "cancel")]);
    let w = (78 + DIALOG_PAD_W).min(f.area().width.saturating_sub(2));
    // Sized to the wrapped text, notes and hints included; on a shorter screen the form scrolls.
    let inner_w = w.saturating_sub(2 + DIALOG_PAD_W);
    let rows = form_lines(form, true, &keys, inner_w).rows(inner_w);
    let r = centered(f, w, rows + 2 + DIALOG_PAD_H);
    f.render_widget(Clear, r);
    let block = dialog(form.title).padding(DIALOG_PAD);
    let inner = block.inner(r);
    f.render_widget(block, r);
    render_form(f, form, inner, true, &keys);
}

/// Draws a form's note, fields with hints, error and key line into `area`, scrolled to show the
/// focused field when it's taller. `active` shows the focused field and the text cursor.
fn render_form(f: &mut Frame, form: &Form, area: Rect, active: bool, keys: &[(&str, &str)]) {
    let mut view = form.view.get();
    draw_scrolled(f, area, &form_lines(form, active, keys, area.width), &mut view);
    form.view.set(view);
}

/// Rows `lines` take wrapped at `width`, as `render_form` draws them.
fn form_rows(lines: Vec<Line>, width: u16) -> u16 {
    Paragraph::new(lines).wrap(Wrap { trim: false }).line_count(width.max(1)) as u16
}

/// Text that scrolls above lines that don't: a form, with its error and keys under it, or an
/// approval dialog's body, with its note and keys.
#[derive(Default)]
struct Scrolled {
    /// What scrolls, with its links.
    body: crate::links::Linked,
    /// Under it, on screen however far it scrolls.
    foot: Vec<Line<'static>>,
    /// A text field's line in `body` and column, for the cursor.
    cursor: Option<(usize, u16)>,
    /// What to keep in view however the view was scrolled before: a form's focused field.
    focus: Option<Focus>,
    /// The lines in `body` that start an entry, a form's fields: the edge rows count the entries
    /// scrolled past, or with none the lines.
    starts: Option<Vec<usize>>,
}

impl Scrolled {
    /// Rows it takes unscrolled, wrapped at `width`.
    fn rows(&self, width: u16) -> u16 {
        form_rows(self.body.lines.clone(), width) + form_rows(self.foot.clone(), width)
    }
}

/// What to keep in view: the lines of a form's focused field, the line above them to show too
/// when there's room (its section's heading, or the note above the first field), and the line
/// in them that must show whatever the room: the cursor's, or the highlighted entry of a list.
struct Focus {
    lines: std::ops::Range<usize>,
    header: Option<usize>,
    at: usize,
}

/// Draws `text`'s body wrapped in `area` from `view.top`, and its foot under it: right under it
/// while it fits, and pinned to the bottom of `area` when it doesn't, the body scrolling above it
/// with an edge row where it goes on. Scrolls to its focus first, if any, and sets `view.max`
/// and `view.page` for the keys that scroll it. Puts the text cursor on the cursor's line when
/// that's shown.
fn draw_scrolled(f: &mut Frame, area: Rect, text: &Scrolled, view: &mut Scroll) {
    let width = area.width.max(1);
    let body = &text.body;
    // The row each line starts on, and after the last the rows they all take.
    let mut row_of = vec![0];
    for line in &body.lines {
        row_of.push(row_of[row_of.len() - 1] + usize::from(form_rows(vec![line.clone()], width)));
    }
    let total = row_of[row_of.len() - 1];
    let foot_h = form_rows(text.foot.clone(), width).min(area.height);
    let view_h = usize::from(area.height - foot_h);
    view.max = total.saturating_sub(view_h);
    view.page = view_h.saturating_sub(2).max(1);
    let mut top = view.top.min(view.max);
    if let Some(show) = &text.focus {
        let rows = |lines: std::ops::Range<usize>| row_of[lines.start]..row_of[lines.end];
        let header = show.header.map(|h| row_of[h]);
        top = scroll_to_show(top, total, view_h, rows(show.lines.clone()), header);
        top = scroll_to_show(top, total, view_h, rows(show.at..show.at + 1), None);
    }
    view.top = top;
    let (first, end) = shown_rows(top, total, view_h);
    let at_row = |row: usize| area.y + (row - top) as u16;
    let edge = |arrow: &str, rows: std::ops::Range<usize>| {
        let line = match &text.starts {
            // As the sidebar's, but in line with the text rather than a column in.
            Some(starts) => {
                let n = starts.iter().filter(|&&l| rows.contains(&row_of[l])).count();
                more_line(arrow, n, false, 0).to_string().trim_start().to_string()
            }
            None => {
                let n = rows.len();
                format!("{arrow} {n} more {}", if n == 1 { "line" } else { "lines" })
            }
        };
        Paragraph::new(Line::from(Span::styled(line, dim())))
    };
    if first > top {
        f.render_widget(edge("↑", 0..first), Rect { height: 1, ..area });
    }
    if end < total && view_h > 0 {
        f.render_widget(edge("↓", end..total), Rect { y: at_row(end), height: 1, ..area });
    }
    let shown = Rect { y: at_row(first), height: (end - first) as u16, ..area };
    let paragraph =
        |lines| Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((first as u16, 0));
    if !body.urls().is_empty() {
        let mut coded = ratatui::buffer::Buffer::empty(shown);
        ratatui::widgets::Widget::render(paragraph(body.coded(0)), shown, &mut coded);
        f.render_widget(paragraph(body.lines.clone()), shown);
        let urls: Vec<&str> = body.urls().iter().map(String::as_str).collect();
        crate::links::mark_coded(f.buffer_mut(), &coded, shown, &urls);
    } else {
        f.render_widget(paragraph(body.lines.clone()), shown);
    }
    let under = Rect { y: area.y + total.min(view_h) as u16, height: foot_h, ..area };
    f.render_widget(Paragraph::new(text.foot.clone()).wrap(Wrap { trim: false }), under);
    if let Some((line, col)) = text.cursor
        && (first..end).contains(&row_of[line])
    {
        let x = (area.x + col).min(area.right().saturating_sub(1));
        f.set_cursor_position((x, at_row(row_of[line])));
    }
}

/// Narrower than its labels and this, a form puts each field's value and hint under its label
/// rather than beside it, so neither is squeezed into a few columns.
const VALUE_MIN: usize = 30;

/// A form's lines, with what's focused when `active`. Values are cut to fit `width` and hints
/// wrap under them, so neither runs under the labels; notes wrap across the whole width.
fn form_lines(form: &Form, active: bool, keys: &[(&str, &str)], width: u16) -> Scrolled {
    let label_w =
        form.fields.iter().filter(|f| !f.hidden).map(|f| util::width(f.label)).max().unwrap_or(8)
            + 2;
    let stacked = usize::from(width) < label_w + VALUE_MIN;
    // The column values and hints start at.
    let col = if stacked { 2 } else { label_w };
    let mut text: Vec<Line> = Vec::new();
    // The lines that link somewhere, each linked whole after its indent (span 1).
    let mut linked: Vec<(usize, String)> = Vec::new();
    if let Some(note) = &form.note {
        text.extend(
            note.lines()
                .map(|l| Line::from(Span::styled(l.to_string(), Style::new().fg(Color::Cyan)))),
        );
        text.push(Line::from(""));
    }
    let (mut cursor, mut focus, mut starts) = (None, None, Vec::new());
    for (i, field) in form.fields.iter().enumerate().filter(|(_, f)| !f.hidden) {
        // The first field's header is the note above it, if any.
        let mut header = starts.is_empty().then_some(0);
        if let Some(title) = field.section {
            text.push(Line::from(""));
            header = Some(text.len());
            text.push(Line::from(Span::styled(
                title.to_string(),
                dim().add_modifier(ratatui::style::Modifier::BOLD),
            )));
        }
        let start = text.len();
        starts.push(start);
        let focused = active && i == form.focus;
        let label_style = if focused { Style::new().fg(Color::Cyan).bold() } else { Style::new() };
        let mut row = if stacked {
            text.push(Line::from(Span::styled(field.label.to_string(), label_style)));
            vec![Span::raw(" ".repeat(col))]
        } else {
            vec![Span::styled(pad(field.label, label_w), label_style)]
        };
        // The line that must show while the field is focused: the cursor's, or a list's highlight.
        let mut at = text.len();
        // Room for the value after the label and the ‹ › around a choice or presets.
        let arrows = if field.choices.is_empty() { 0 } else { 4 };
        let room = (width as usize).saturating_sub(col + arrows);
        // A prose value's lines after its first, indented to start under the value.
        let mut more = Vec::new();
        if field.prose {
            // One column kept for the cursor, as below.
            let gs: Vec<&str> = field.value.graphemes(true).collect();
            let ranges = wrapped(&gs, room.saturating_sub(1));
            let mut lines: Vec<String> = ranges.iter().map(|r| gs[r.clone()].concat()).collect();
            if focused {
                let here = grapheme_at(&field.value, field.cursor());
                let on = line_of(&ranges, here);
                // Scrolled to keep the cursor's line in view.
                let most = ranges.len().saturating_sub(PROSE_LINES);
                let top = field.view.get().min(on).max((on + 1).saturating_sub(PROSE_LINES));
                let top = top.min(most);
                field.view.set(top);
                lines = lines.drain(top..).take(PROSE_LINES).collect();
                let c = util::width(&gs[ranges[on].start..here].concat());
                cursor = Some((text.len() + on - top, (col + c) as u16));
            } else if lines.len() > PROSE_LINES {
                // The line after the last didn't fit on it, so joined they're cut with `…`.
                let last = format!("{} {}", lines[PROSE_LINES - 1], lines[PROSE_LINES]);
                lines.truncate(PROSE_LINES);
                lines[PROSE_LINES - 1] = truncate(&last, room.saturating_sub(1));
            }
            let mut lines = lines.into_iter();
            row.push(Span::raw(lines.next().unwrap_or_default()));
            more.extend(lines.map(|l| Line::from(format!("{:col$}{l}", ""))));
        } else if field.is_text() {
            // A secret shows as a dot a character.
            let dots = field.masked.then(|| "•".repeat(field.value.graphemes(true).count()));
            let shown = dots.as_deref().unwrap_or(&field.value);
            let (value, c) = if focused {
                let gs: Vec<&str> = shown.graphemes(true).collect();
                in_view(&gs, grapheme_at(&field.value, field.cursor()), &field.view, room)
            } else if dots.is_some() {
                (truncate(shown, room), 0)
            } else {
                (fit(field, room), 0)
            };
            if focused {
                cursor = Some((text.len(), (col + c) as u16));
            }
            row.push(Span::raw(value));
        } else {
            let style = if focused {
                Style::new().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::new().bold()
            };
            // A link is a button: nothing to cycle.
            let value = fit(field, room);
            let shown =
                if field.url.is_some() { format!(" {value} ") } else { format!("‹ {value} ›") };
            row.push(Span::styled(shown, style));
        }
        text.push(Line::from(row));
        text.extend(more);
        // A long hint wraps under the value too, not under the labels; under a link's text, past
        // the button's padding.
        let indent = col + usize::from(field.url.is_some());
        let hint_room = (width as usize).saturating_sub(indent);
        text.extend(
            wrap_prose(&field.hint, hint_room)
                .into_iter()
                .map(|l| Line::from(Span::styled(format!("{:indent$}{l}", ""), dim()))),
        );
        for (before, label, url) in &field.links {
            // A URL shown as itself starts a line of its own, so it wraps as late as it can, and
            // what leads to it goes on the line above.
            let shown = if label == url {
                text.extend(
                    wrap_prose(before, hint_room)
                        .into_iter()
                        .filter(|l| !l.is_empty())
                        .map(|l| Line::from(Span::styled(format!("{:indent$}{l}", ""), dim()))),
                );
                label.clone()
            } else {
                format!("{before}{label}")
            };
            for l in wrap_prose(&shown, hint_room) {
                linked.push((text.len(), url.clone()));
                text.push(Line::from(vec![
                    Span::raw(format!("{:indent$}", "")),
                    Span::styled(l, dim().add_modifier(ratatui::style::Modifier::UNDERLINED)),
                ]));
            }
        }
        // The highlighted issue on the sidebar's selected card's background, across the room.
        let highlighted = field.list.highlighted().filter(|_| focused);
        if let Some(h) = highlighted {
            at = text.len() + h;
        }
        text.extend(field.list.lines.iter().enumerate().map(|(n, l)| {
            let l = truncate(l, hint_room);
            match highlighted {
                Some(h) if h == n => Line::from(vec![
                    Span::raw(format!("{:col$}", "")),
                    Span::styled(pad(&l, hint_room), Style::new().bg(selection_bg())),
                ]),
                _ => Line::from(format!("{:col$}{l}", "")),
            }
        }));
        if focused {
            if let Some((line, _)) = cursor {
                at = line;
            }
            focus = Some(Focus { lines: start..text.len(), header, at });
        }
    }
    let mut foot = vec![Line::from("")];
    foot.push(match &form.error {
        _ if form.checking.is_some() => {
            Line::from(Span::styled(format!("{} Checking the connection…", spinner()), dim()))
        }
        Some(e) => Line::from(Span::styled(e.clone(), Style::new().fg(Color::Red))),
        None => Line::from(""),
    });
    foot.extend(key_rows(keys, usize::from(width)));
    let mut body = crate::links::Linked::default();
    for (n, line) in text.into_iter().enumerate() {
        body.push(line);
        for (_, url) in linked.iter().filter(|(l, _)| *l == n) {
            body.link(1, url);
        }
    }
    Scrolled { body, foot, cursor, focus, starts: Some(starts) }
}

/// The most lines a prose field takes; a longer one scrolls while typed and is cut otherwise.
const PROSE_LINES: usize = 4;

/// `s` wrapped at spaces into lines of at most `room` columns; a word longer than that is split
/// between graphemes. The space a line breaks at is dropped, but every other character is kept, a
/// trailing space too, so the end of the last line is where typing continues.
fn wrap_prose(s: &str, room: usize) -> Vec<String> {
    let gs: Vec<&str> = s.graphemes(true).collect();
    wrapped(&gs, room).into_iter().map(|r| gs[r].concat()).collect()
}

/// The graphemes of each line `gs` wrap into, as `wrap_prose` wraps them: one line at least, and
/// each line's end where the next starts, or the space between them.
fn wrapped(gs: &[&str], room: usize) -> Vec<std::ops::Range<usize>> {
    let room = room.max(1);
    let mut lines = Vec::new();
    let mut start = 0;
    loop {
        // The first grapheme past what fits from `start`.
        let mut end = start;
        let mut used = 0;
        while let Some(g) = gs.get(end)
            && used + util::width(g) <= room
        {
            used += util::width(g);
            end += 1;
        }
        if end == gs.len() {
            lines.push(start..gs.len());
            return lines;
        }
        // A space just past a full line breaks there too.
        match (start + 1..=end).rev().find(|&i| gs[i] == " ") {
            Some(i) => {
                lines.push(start..i);
                start = i + 1;
            }
            None => {
                // A character wider than the room goes on a line of its own.
                let end = end.max(start + 1);
                lines.push(start..end);
                if end == gs.len() {
                    return lines;
                }
                start = end;
            }
        }
    }
}

/// The line of `lines` (as `wrapped` gives them) the cursor before grapheme `at` is on: on the
/// space a line broke at, it's at the end of that line.
fn line_of(lines: &[std::ops::Range<usize>], at: usize) -> usize {
    let on = |l: usize| {
        at < lines[l].end
            || at == lines[l].end && lines.get(l + 1).is_none_or(|next| next.start > at)
    };
    (0..lines.len()).find(|&l| on(l)).unwrap_or(lines.len() - 1)
}

/// How many graphemes of `s` come before the byte offset `at`.
fn grapheme_at(s: &str, at: usize) -> usize {
    s.grapheme_indices(true).take_while(|&(i, _)| i < at).count()
}

/// A focused value's graphemes `gs` in `room` columns, with the cursor before grapheme `at` (after
/// the last at the end): all of them when they fit with a column for the cursor, else a window that
/// shows the cursor, with `…` where it cuts. `start` is the window's first grapheme, kept from the
/// last frame so the value scrolls only when the cursor would leave it. What's drawn, and the
/// cursor's column in it.
fn in_view(gs: &[&str], at: usize, start: &Cell<usize>, room: usize) -> (String, usize) {
    let n = gs.len();
    // Columns up to each grapheme.
    let cols: Vec<usize> = std::iter::once(0)
        .chain(gs.iter().scan(0, |w, g| {
            *w += util::width(g);
            Some(*w)
        }))
        .collect();
    let w = |r: std::ops::Range<usize>| cols[r.end] - cols[r.start];
    if cols[n] < room {
        start.set(0);
        return (gs.concat(), cols[at]);
    }
    let lead = |s: usize| usize::from(s > 0);
    // What a window from `s` needs to show the cursor: the `…` before, the graphemes up to the
    // cursor, the one under it and a `…` after it, or a column after the end.
    let needs = |s: usize| {
        lead(s) + w(s..at) + if at < n { w(at..at + 1) + usize::from(at + 1 < n) } else { 1 }
    };
    let mut s = start.get().min(at);
    while s < at && needs(s) > room {
        s += 1;
    }
    // Back while what's after it still fits: a value cut shorter shows more of what's before.
    while s > 0 && lead(s - 1) + w(s - 1..n) + usize::from(at == n) <= room {
        s -= 1;
    }
    start.set(s);
    let mut end = at;
    while end < n && lead(s) + w(s..end + 1) + usize::from(end + 1 < n) <= room {
        end += 1;
    }
    let shown = format!(
        "{}{}{}",
        if s > 0 { "…" } else { "" },
        gs[s..end].concat(),
        if end < n { "…" } else { "" }
    );
    (truncate(&shown, room), lead(s) + w(s..at))
}

/// A field's value cut to `room` columns: at the end, or for `cut_middle` in the middle of what
/// follows `who · `, so an account keeps who it is and both ends of its source.
fn fit(field: &crate::app::Field, room: usize) -> String {
    if !field.cut_middle {
        return truncate(&field.value, room);
    }
    match field.value.split_once(" · ") {
        Some((who, source)) if util::width(who) + 3 + 12 <= room => {
            format!("{who} · {}", truncate_middle(source, room - util::width(who) - 3))
        }
        _ => truncate_middle(&field.value, room),
    }
}

/// Settings: updates, projects, accounts and Help & feedback listed on the left, the selected one's
/// form on the right.
fn draw_settings(f: &mut Frame, app: &mut App) {
    let Mode::Settings(s) = &app.mode else { return };
    let deletable =
        matches!(s.items[s.cursor], SettingsItem::Project(_) | SettingsItem::Account(_));
    let mut keys = if s.editing {
        editing_keys(&s.form.fields[s.form.focus])
    } else if s.items[s.cursor] == SettingsItem::Help {
        vec![("↑/↓", "choose"), ("Enter", "pick a link")]
    } else {
        vec![("↑/↓", "choose"), ("Enter", "edit")]
    };
    if deletable {
        keys.push(("ctrl+d", "delete"));
    }
    keys.push(("Esc", if s.editing { "back to list" } else { "close" }));
    // Tall enough for the form, wrapped hints and keys included, within the screen: a taller
    // form scrolls. 3 rows for the borders and the pane title. The form is as wide as the dialog
    // `centered` gives, less its borders, the list and the pane's border and gap.
    let form_w = 108u16.min(f.area().width.saturating_sub(2)).saturating_sub(2 + 30 + 2);
    let rows = form_lines(&s.form, s.editing, &keys, form_w).rows(form_w);
    let r = centered(f, 108, (rows + 3).max(28));
    f.render_widget(Clear, r);
    let block = dialog("Settings");
    let inner = block.inner(r);
    f.render_widget(block, r);
    let [list, form_area] =
        Layout::horizontal([Constraint::Length(30), Constraint::Min(30)]).areas(inner);

    let w = list.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let section = |lines: &mut Vec<Line>, title: &str| {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!(" {title}"),
            dim().add_modifier(ratatui::style::Modifier::BOLD),
        )));
    };
    // Where each item's row is, and whether a section's heading is right above it.
    let mut at = Vec::new();
    for (row, item) in s.items.iter().enumerate() {
        let heading = match item {
            SettingsItem::General => Some("GENERAL"),
            SettingsItem::Project(0) => Some("PROJECTS"),
            SettingsItem::NewProject if app.projects.is_empty() => Some("PROJECTS"),
            SettingsItem::Account(0) => Some("ACCOUNTS"),
            SettingsItem::NewAccount if app.config.accounts.is_empty() => Some("ACCOUNTS"),
            SettingsItem::Help => Some("HELP"),
            _ => None,
        };
        if let Some(title) = heading {
            section(&mut lines, title);
        }
        at.push((lines.len(), heading.is_some()));
        let (left, right, new) = match item {
            SettingsItem::General => ("General".to_string(), String::new(), false),
            SettingsItem::Project(p) => {
                let proj = &app.projects[*p];
                let n = app.tickets.iter().filter(|tv| tv.t.repo == proj.repo).count();
                (proj.name.clone(), n.to_string(), false)
            }
            SettingsItem::NewProject => ("+ new project".to_string(), String::new(), true),
            SettingsItem::Account(i) => {
                let a = &app.config.accounts[*i];
                (a.who(), a.provider.label().to_string(), false)
            }
            SettingsItem::NewAccount => ("+ new account".to_string(), String::new(), true),
            SettingsItem::Help => ("Help & feedback".to_string(), String::new(), false),
        };
        let selected = row == s.cursor;
        let room = w.saturating_sub(util::width(&right) + 2);
        let name = truncate(&left, room);
        let pad = w.saturating_sub(util::width(&name) + util::width(&right) + 1);
        let base = match (selected, s.editing, new) {
            (true, false, _) => Style::new().fg(Color::Black).bg(Color::Cyan),
            (true, true, _) => Style::new().fg(Color::Cyan).bold(),
            (false, _, true) => dim(),
            _ => Style::new(),
        };
        let marker = if selected { "▸" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} {name}{}", " ".repeat(pad)), base),
            Span::styled(format!("{right} "), if selected && !s.editing { base } else { dim() }),
        ]));
    }
    let (view_h, total) = (usize::from(list.height), lines.len());
    let (row, headed) = at[s.cursor];
    let top = scroll_to_show(s.top, total, view_h, row..row + 1, headed.then(|| row - 1));
    let (first, end) = shown_rows(top, total, view_h);
    let hidden = |rows: std::ops::Range<usize>| at.iter().filter(|(r, _)| rows.contains(r)).count();
    let mut shown = Vec::new();
    if first > 0 {
        shown.push(more_line("↑", hidden(0..first), false, w));
    }
    shown.extend(lines.drain(first..end));
    if end < total {
        shown.push(more_line("↓", hidden(end..total), false, w));
    }
    f.render_widget(Paragraph::new(shown), list);

    let pane = Block::new()
        .borders(Borders::LEFT)
        .border_style(dim())
        .title(Span::styled(format!(" {} ", s.form.title), Style::new().bold()));
    let pane_inner = pane.inner(form_area);
    f.render_widget(pane, form_area);
    let body =
        Rect { x: pane_inner.x + 1, width: pane_inner.width.saturating_sub(1), ..pane_inner };
    render_form(f, &s.form, body, s.editing, &keys);
    if let Mode::Settings(s) = &mut app.mode {
        s.top = top;
    }
}

/// Help's widest text, inside its border and padding: wider than this, lines wrap.
const HELP_W: usize = 78;

fn draw_help(f: &mut Frame, app: &mut App) {
    let Mode::Help(view) = &mut app.mode else { return };
    let p = prefix();
    let (again, send) = (p.to_string(), format!("send {p} itself to the session"));
    let keys = [
        ("[ / ]", "previous / next ticket"),
        ("{ / }", "move the card; ↑/↓ go on, p moves its project, Enter ends"),
        (again.as_str(), "next ticket needing you, else one that finished (also tab)"),
        ("/", "jump to a ticket, agent or project by typing"),
        ("w", "list the sessions that need you, oldest first, with what they ask"),
        ("n", "add a ticket, or an agent with no ticket"),
        (",", "settings: prefix key, updates, sessions, theme, notifications, projects, accounts"),
        ("P / A", "settings, on a new project / account"),
        ("z", "fold / unfold the current project"),
        ("x", "close the current ticket or agent"),
        ("b", "collapse / expand the sidebar"),
        ("j / c / g", "toggle the tracker panel, its comments, the PR panel"),
        ("o / p", "open ticket / PR in your browser"),
        ("r", "refresh tracker and GitHub now"),
        ("m", "take the ● off comments that were new on this ticket"),
        ("R / t", "restart the session / hand it to another terminal"),
        ("M", "run in the main checkout when the worktree failed"),
        ("e", "rename the current agent"),
        ("PgUp/PgDn", "scroll back, then PgUp/PgDn ↑/↓ Home/End go on"),
        ("v", "open the session's history in $EDITOR"),
        ("l", "list links (ticket, PR, session) to open or copy"),
        ("~", "notices: what the footer showed, newest first"),
        ("a", send.as_str()),
        ("q", "quit"),
    ];
    // The legend, drawn with the sidebar's own badges.
    let legend = [
        (badge(AgentState::Needs, false, false), "asking you something"),
        (badge(AgentState::Working, true, false), "working on something"),
        (badge(AgentState::Done, false, false), "done, not looked at"),
        (badge(AgentState::Done, true, false), "done, and seen"),
        (badge(AgentState::Exited, false, false), "ended, not looked at"),
        (badge(AgentState::Exited, true, false), "ended, and seen"),
        (badge(AgentState::Idle, true, true), "added by taskman add"),
        (badge(AgentState::Idle, true, false), "idle or not started"),
        (asleep(), "asleep; selecting wakes it"),
        (Span::styled("✦ lead", Style::new().bold()), "an agent: no ticket"),
        (Span::styled("↗ outside", Style::new().fg(Color::Cyan)), "in another terminal"),
        (Span::styled("↩ back", Style::new().fg(Color::Cyan).bold()), "Enter resumes it here"),
    ];
    // Indented under the heading, unless the prefix's name is long enough to need the room.
    let indent = if p.name().len() > "ctrl+a".len() { 0 } else { 2 };
    let key_w = keys.iter().map(|(k, _)| util::width(&format!("{p} {k}"))).max().unwrap_or(0) + 2;
    let badge_w = legend.iter().map(|(b, _)| util::width(&b.content)).max().unwrap_or(0) + 2;
    // Two to a row, each column as wide as its own words.
    let words = |skip: usize| {
        legend.iter().skip(skip).step_by(2).map(|(_, what)| util::width(what)).max().unwrap_or(0)
    };
    let what_w = words(0) + 3;
    let keys_w = keys.iter().map(|(_, what)| indent + key_w + util::width(what)).max().unwrap_or(0);
    let pairs_w = 2 + 2 * badge_w + what_w + words(1);
    let room = usize::from(f.area().width.saturating_sub(2 + 2 + DIALOG_PAD_W));
    let inner = keys_w.max(pairs_w).min(HELP_W).min(room).max(1);

    let mut text = Vec::new();
    let heading = format!("Every key goes to the session, except after the {p} prefix:");
    text.extend(wrap_words(&heading, inner).into_iter().map(|l| Line::from(l.bold())));
    text.push(Line::from(""));
    // Each key's description wraps under its own column, or under the key when that column
    // would be too narrow to read.
    let cyan = Style::new().fg(Color::Cyan);
    let stacked = inner < indent + key_w + 20;
    for (k, what) in &keys {
        let key = format!("{p} {k}");
        if stacked {
            text.push(Line::from(Span::styled(format!("{:indent$}{key}", ""), cyan)));
            let under = indent + 4;
            for l in wrap_words(what, inner.saturating_sub(under)) {
                text.push(Line::from(format!("{:under$}{l}", "")));
            }
            continue;
        }
        let col = indent + key_w;
        for (i, l) in wrap_words(what, inner - col).into_iter().enumerate() {
            let left = if i == 0 {
                format!("{:indent$}{}", "", pad(&key, key_w))
            } else {
                " ".repeat(col)
            };
            text.push(Line::from(vec![Span::styled(left, cyan), Span::raw(l)]));
        }
    }
    text.push(Line::from(""));
    let mouse = "Mouse: the wheel scrolls the pane under the pointer; click a ticket or a project \
                 header. Drag to copy text, past an edge or with the wheel for more; ctrl+c copies \
                 it again, Esc clears it. Drag a card or a project header in the sidebar to reorder \
                 it; Esc cancels.";
    text.extend(wrap_words(mouse, inner).into_iter().map(|l| Line::from(l.bold())));
    text.push(Line::from(""));
    // Two to a row where they fit, else one, its words wrapping beside the badge.
    if pairs_w <= inner {
        for pair in legend.chunks(2) {
            let mut row = vec![Span::raw("  ")];
            for (n, (b, what)) in pair.iter().enumerate() {
                let gap = " ".repeat(badge_w - util::width(&b.content));
                let what = if n == 0 { pad(what, what_w) } else { what.to_string() };
                row.extend([b.clone(), Span::raw(gap), Span::raw(what)]);
            }
            text.push(Line::from(row));
        }
    } else {
        let col = 2 + badge_w;
        for (b, what) in &legend {
            for (i, l) in wrap_words(what, inner.saturating_sub(col)).into_iter().enumerate() {
                let mut row = vec![Span::raw("  ")];
                if i == 0 {
                    row.extend([
                        b.clone(),
                        Span::raw(" ".repeat(badge_w - util::width(&b.content))),
                    ]);
                } else {
                    row.push(Span::raw(" ".repeat(badge_w)));
                }
                row.push(Span::raw(l));
                text.push(Line::from(row));
            }
        }
    }
    text.push(Line::from(""));
    let more = format!(
        "The guide, and where to report a bug or ask for a feature: Settings ({p} ,), then Help \
         & feedback."
    );
    text.extend(wrap_words(&more, inner).into_iter().map(Line::from));
    // Below the text, a blank row and what keys do, which says where you are when it scrolls.
    // Sized for its longest form, so the rows left for the text stay put as it scrolls; without
    // PgUp/PgDn where that keeps it to a row.
    let n = text.len() as u16;
    let longest = format!("{n}-{n} of {n}");
    let scroll = if util::line_width(&key_row(&scroll_hints(SCROLL_KEYS, Some(&longest)))) <= inner
    {
        SCROLL_KEYS
    } else {
        "↑/↓"
    };
    let fits = n + 2 + 2 + DIALOG_PAD_H <= f.area().height.saturating_sub(2);
    let keys_h =
        if fits { 1 } else { key_rows(&scroll_hints(scroll, Some(&longest)), inner).len() as u16 };
    let r = centered(f, inner as u16 + 2 + DIALOG_PAD_W, n + 1 + keys_h + 2 + DIALOG_PAD_H);
    let rows = r.height.saturating_sub(2 + DIALOG_PAD_H + 1 + keys_h);
    view.max = n.saturating_sub(rows);
    view.page = rows.saturating_sub(1).max(1);
    view.top = view.top.min(view.max);
    let at = format!("{}-{} of {n}", view.top + 1, (view.top + rows).min(n));
    let keys = scroll_hints(scroll, (view.max > 0).then_some(at.as_str()));
    let mut shown: Vec<Line> =
        text.into_iter().skip(view.top as usize).take(rows as usize).collect();
    shown.push(Line::from(""));
    shown.extend(key_rows(&keys, inner));
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(shown).block(dialog("Help").padding(DIALOG_PAD)), r);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(keys: &[(&str, &str)], width: usize) -> Vec<String> {
        key_rows(keys, width).iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn key_hints_share_a_row_until_it_is_full() {
        let keys = [("Enter", "save"), ("Tab", "next field"), ("Esc", "cancel")];
        assert_eq!(rows(&keys, 80), ["Enter save   Tab next field   Esc cancel"]);
        // Exactly as wide as the row still fits; a column less breaks before the last hint.
        assert_eq!(rows(&keys, 40), ["Enter save   Tab next field   Esc cancel"]);
        assert_eq!(rows(&keys, 39), ["Enter save   Tab next field", "Esc cancel"]);
        assert_eq!(rows(&keys, 14), ["Enter save", "Tab next field", "Esc cancel"]);
        // The keys are bold, the gaps and what they do aren't.
        let row = &key_rows(&keys, 80)[0];
        let bold: Vec<&str> = row
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(ratatui::style::Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(bold, ["Enter", "Tab", "Esc"]);
        assert_eq!(key_row(&keys).to_string(), "Enter save   Tab next field   Esc cancel");
        assert!(key_rows(&[], 80).is_empty());
    }

    #[test]
    fn a_key_hint_wider_than_its_row_is_cut() {
        let keys = [("Esc", "back to list"), ("ctrl+d", "delete")];
        assert_eq!(rows(&keys, 10), ["Esc back …", "ctrl+d de…"]);
        // Only room for the key: the words go.
        assert_eq!(rows(&keys, 6), ["Esc b…", "ctrl+d"]);
        assert_eq!(rows(&keys, 4), ["Esc", "ctr…"]);
        // By columns: a wide hint is cut where it fits, and rows break by width.
        let wide = [("⚠\u{fe0f}", "日本語のヒント"), ("Esc", "閉じる")];
        assert_eq!(rows(&wide, 30), ["⚠\u{fe0f} 日本語のヒント   Esc 閉じる"]);
        assert_eq!(rows(&wide, 29), ["⚠\u{fe0f} 日本語のヒント", "Esc 閉じる"]);
        assert_eq!(rows(&wide, 12), ["⚠\u{fe0f} 日本語の…", "Esc 閉じる"]);
        for width in 0..30 {
            for row in rows(&keys, width).into_iter().chain(rows(&wide, width)) {
                assert!(util::width(&row) <= width.max(1), "{row:?} at {width}");
            }
        }
    }

    #[test]
    fn a_note_among_the_key_hints_has_no_key() {
        let keys = scroll_hints(SCROLL_KEYS, Some("1-20 of 40"));
        assert_eq!(rows(&keys, 80), ["↑/↓ PgUp/PgDn scroll   1-20 of 40   other keys close"]);
        assert_eq!(rows(&keys, 40), ["↑/↓ PgUp/PgDn scroll   1-20 of 40", "other keys close"]);
        assert!(!key_rows(&keys, 80)[0].spans.iter().any(|s| {
            s.content.contains("1-20")
                && s.style.add_modifier.contains(ratatui::style::Modifier::BOLD)
        }));
        assert_eq!(rows(&scroll_hints(SCROLL_KEYS, None), 80), ["Press any key to close."]);
    }

    #[test]
    fn a_forms_keys_follow_its_focused_field() {
        use crate::app::{Field, FormKind};
        let mut form = Form::new(
            FormKind::Project,
            "Project",
            vec![
                Field::text("Folder", "", ""),
                Field::choice("Tracker", vec!["Jira".into()], "Jira", ""),
            ],
        );
        let keys = |form: &Form| editing_keys(&form.fields[form.focus]);
        // ←/→ move the cursor in text, and change a choice.
        assert_eq!(keys(&form), [("Enter", "save"), ("Tab", "next field")]);
        form.focus = 1;
        assert_eq!(
            keys(&form),
            [("Enter", "save"), ("Tab", "next field"), ("←/→", "change choice")]
        );
        form.focus = 0;
        form.fields[0].list.picks = 3;
        assert_eq!(keys(&form), [("↑/↓", "choose"), ("Enter", "save"), ("Tab", "next field")]);
        // At the bottom of the form, broken between hints at its width.
        let mut all = keys(&form);
        all.extend([("ctrl+u", "clear"), ("Esc", "cancel")]);
        let foot = form_lines(&form, true, &all, 40).foot;
        let text: Vec<String> = foot.iter().map(|l| l.to_string()).collect();
        assert_eq!(
            text[text.len() - 2..],
            ["↑/↓ choose   Enter save   Tab next field", "ctrl+u clear   Esc cancel"]
        );
    }

    #[test]
    fn joins_lists_in_prose() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(join_and(&s(&["1 untracked file"])), "1 untracked file");
        assert_eq!(join_and(&s(&["a", "b", "c"])), "a, b and c");
    }

    #[test]
    fn counts_agents_and_tickets_apart() {
        assert_eq!(closes_too(0, 0), "It has no tickets or agents.");
        assert_eq!(closes_too(0, 1), "Its ticket closes too, and its session stops.");
        assert_eq!(closes_too(4, 0), "Its 4 agents close too, and their sessions stop.");
        assert_eq!(
            closes_too(1, 3),
            "Its 3 tickets and 1 agent close too, and their sessions stop."
        );
    }

    #[test]
    fn confirm_height_fits_wrapped_detail() {
        let body = |s: &str| vec![Line::from(s.to_string())];
        // Question, blank, detail, blank and Enter/Esc rows, plus borders and padding.
        assert_eq!(confirm_height("Quit taskman?", body("Running sessions stop."), YES_NO, 64), 9);
        let long = "Its 12 tickets are closed too and their sessions stop. The repo, worktrees, branches and accounts are kept.";
        assert_eq!(confirm_height("Delete project acme?", body(long), YES_NO, 64), 10);
        assert!(confirm_height("Delete project acme?", body(long), YES_NO, 40) > 10);
    }

    /// Focuses the form's field labelled `label`.
    fn focus_on(form: &mut Form, label: &str) {
        form.focus = form.fields.iter().position(|f| f.label == label).unwrap();
    }

    fn set(form: &mut Form, label: &str, value: &str) {
        form.fields.iter_mut().find(|f| f.label == label).unwrap().value = value.into();
    }

    #[test]
    fn form_values_are_cut_not_wrapped() {
        use crate::app::{Field, FormKind};
        let path = "~/work/clients/acme/repositories/acme-web-frontend-monorepo";
        let label =
            "you@example.com · cmd:security find-generic-password -a you -s taskman-jira2 -w";
        let mut account = Field::choice("Account", vec![label.to_string()], label, "");
        account.cut_middle = true;
        let mut form =
            Form::new(FormKind::Project, "Project", vec![Field::text("Path", path, ""), account]);
        let width = 50;
        let rows = |form: &Form| {
            let Scrolled { body, cursor, .. } = form_lines(form, true, &[], width);
            let lines = body.lines;
            assert_eq!(form_rows(lines.clone(), width), lines.len() as u16, "a line wrapped");
            (lines.iter().map(|l| l.to_string()).collect::<Vec<_>>(), cursor)
        };
        // Focused, the path keeps its end where the cursor is.
        let (lines, cursor) = rows(&form);
        assert_eq!(lines[0], "Path     …repositories/acme-web-frontend-monorepo");
        assert_eq!(cursor, Some((0, 49)));
        // An account keeps both ends of its source: the difference is usually at the end.
        assert_eq!(lines[2], "Account  ‹ you@example.com · cmd:secur…-jira2 -w ›");
        focus_on(&mut form, "Account");
        let (lines, _) = rows(&form);
        assert_eq!(lines[0], "Path     ~/work/clients/acme/repositories/acme-we…");
    }

    #[test]
    fn hints_wrap_under_their_value() {
        use crate::app::{Field, FormKind};
        let hint = "e.g. ~/work/clients/acme/repositories/acme-web-frontend-monorepo-ENG-52 on ENG-52-fix-login";
        let form = Form::new(FormKind::Project, "Project", vec![Field::text("Folder", "", hint)]);
        let width = 40;
        let lines = form_lines(&form, false, &[], width).body.lines;
        assert_eq!(form_rows(lines.clone(), width), lines.len() as u16, "a line wrapped");
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(
            text[1..5],
            [
                "        e.g.",
                "        ~/work/clients/acme/repositories",
                "        /acme-web-frontend-monorepo-ENG-",
                "        52 on ENG-52-fix-login",
            ]
            .map(String::from)
        );
    }

    #[test]
    fn narrow_forms_put_values_and_hints_under_their_labels() {
        use crate::app::{Field, FormKind};
        let fields = vec![
            Field::text("Sleep idle sessions after", "1 hour", "unselected this long"),
            Field::text("Theme", "dark", "or light"),
        ];
        let mut form = Form::new(FormKind::Project, "General", fields);
        focus_on(&mut form, "Theme");
        let text = |width| {
            let t = form_lines(&form, true, &[], width);
            let lines: Vec<String> = t.body.lines.iter().map(|l| l.to_string()).collect();
            (lines, t.cursor, t.focus.map(|f| (f.lines, f.at)), t.starts.unwrap_or_default())
        };
        // Room for the labels and a value of `VALUE_MIN`: beside them.
        let (lines, cursor, focus, starts) = text(27 + VALUE_MIN as u16);
        assert_eq!(lines[0], "Sleep idle sessions after  1 hour");
        assert_eq!(lines[1], "                           unselected this long");
        assert_eq!((cursor, focus, starts), (Some((2, 31)), Some((2..4, 2)), vec![0, 2]));
        // Narrower, under them: hints keep the width rather than a few columns.
        let (lines, cursor, focus, starts) = text(26 + VALUE_MIN as u16);
        assert_eq!(
            lines,
            [
                "Sleep idle sessions after",
                "  1 hour",
                "  unselected this long",
                "Theme",
                "  dark",
                "  or light"
            ]
        );
        assert_eq!((cursor, focus, starts), (Some((4, 6)), Some((3..6, 4)), vec![0, 3]));
    }

    #[test]
    fn wraps_prose_at_spaces() {
        assert_eq!(wrap_prose("", 10), [""]);
        assert_eq!(
            wrap_prose("review PRs and keep the board tidy", 10),
            ["review PRs", "and keep", "the board", "tidy"]
        );
        // A trailing space stays, so the cursor goes after it; at a break, it starts a new line.
        assert_eq!(wrap_prose("review ", 10), ["review "]);
        assert_eq!(wrap_prose("review PRs ", 10), ["review PRs", ""]);
        assert_eq!(wrap_prose("abcdefghijklmn op", 5), ["abcde", "fghij", "klmn", "op"]);
        // By columns: a wide character is kept whole, and an emoji with its selector.
        assert_eq!(wrap_prose("日本語の役割", 5), ["日本", "語の", "役割"]);
        assert_eq!(wrap_prose("見る ⚠\u{fe0f} ok", 5), ["見る", "⚠\u{fe0f} ok"]);
        assert_eq!(wrap_prose("ab⚠\u{fe0f}c", 3), ["ab", "⚠\u{fe0f}c"]);
        // A character wider than the room goes on a line of its own rather than loop.
        assert_eq!(wrap_prose("日本", 1), ["日", "本"]);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(200))]

        /// Every line fits, unless it's one character wider than the room, only the spaces lines
        /// break at are dropped, and no empty line is left after the text.
        #[test]
        fn wrapped_prose_fits_and_keeps_the_text(
            parts in proptest::collection::vec(
                proptest::sample::select(vec!["a", "b", " ", "日", "⚠", "\u{fe0f}", "\u{301}", "👍"]),
                0..40,
            ),
            room in 1usize..12,
        ) {
            use unicode_segmentation::UnicodeSegmentation as _;
            let s = parts.concat();
            let lines = wrap_prose(&s, room);
            for l in &lines {
                let one = l.graphemes(true).count() == 1;
                proptest::prop_assert!(util::width(l) <= room || one, "{l:?} wider than {room}");
            }
            let kept: String = lines.concat().split(' ').collect();
            proptest::prop_assert_eq!(kept, s.split(' ').collect::<String>());
            // An empty last line is where typing goes after a space the line broke at.
            let last = lines.last().unwrap();
            proptest::prop_assert!(!last.is_empty() || s.is_empty() || s.ends_with(' '));
        }
    }

    /// The window `in_view` shows of `value` in `room` columns as the cursor goes to each of
    /// `cursors` in turn, with the cursor drawn as `|`.
    fn views(value: &str, room: usize, cursors: &[usize]) -> Vec<String> {
        let gs: Vec<&str> = value.graphemes(true).collect();
        let start = Cell::new(0);
        cursors
            .iter()
            .map(|&at| {
                let (shown, col) = in_view(&gs, at, &start, room);
                let mut gs: Vec<&str> = shown.graphemes(true).collect();
                let i = (0..=gs.len()).find(|&i| util::width(&gs[..i].concat()) == col).unwrap();
                gs.insert(i, "|");
                gs.concat()
            })
            .collect()
    }

    #[test]
    fn a_long_value_scrolls_to_keep_the_cursor_in_view() {
        // All of it while it fits with a column for the cursor.
        assert_eq!(views("abcde", 6, &[5, 2]), ["abcde|", "ab|cde"]);
        // At the end, then back: the window moves only once the cursor would leave it.
        assert_eq!(
            views("abcdefghij", 6, &[10, 7, 6, 5, 2, 0, 3, 5, 6]),
            [
                "…ghij|",
                "…fg|hij",
                "…f|ghij",
                "…|fghij",
                "…|cdef…",
                "|abcde…",
                "abc|de…",
                "…cde|f…",
                "…def|g…"
            ]
        );
        // Wide characters take two columns, and never half a window.
        assert_eq!(views("日本語のテキスト", 7, &[8, 0]), ["…スト|", "|日本語…"]);
    }

    #[test]
    fn the_cursor_is_on_the_line_its_grapheme_wrapped_to() {
        let gs: Vec<&str> = "fix the login page".graphemes(true).collect();
        let lines = wrapped(&gs, 8);
        assert_eq!(lines, [0..7, 8..13, 14..18]);
        // On the space a line broke at: at the end of that line.
        let on: Vec<usize> = [0, 7, 8, 13, 14, 18].iter().map(|&at| line_of(&lines, at)).collect();
        assert_eq!(on, [0, 0, 1, 1, 2, 2]);
        // A word split with no space: the cursor between its halves starts the next line.
        let gs: Vec<&str> = "abcdefghij".graphemes(true).collect();
        let lines = wrapped(&gs, 4);
        assert_eq!(lines, [0..4, 4..8, 8..10]);
        assert_eq!([3, 4, 10].map(|at| line_of(&lines, at)), [0, 1, 2]);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(300))]

        /// Whatever the value, the room (from 4 columns, room for a wide character between `…`s),
        /// the cursor and where the last window started, what's drawn fits, and the cursor is in
        /// it on the grapheme it's before.
        #[test]
        fn the_cursor_is_always_in_view(
            parts in proptest::collection::vec(
                proptest::sample::select(vec!["a", "b", " ", "日", "👍", "e\u{301}"]),
                0..40,
            ),
            at in 0usize..41,
            start in 0usize..41,
            room in 4usize..20,
        ) {
            let at = at.min(parts.len());
            let (shown, col) = in_view(&parts, at, &Cell::new(start), room);
            proptest::prop_assert!(util::width(&shown) <= room, "{shown:?} wider than {room}");
            proptest::prop_assert!(col < room, "cursor at {col} of {room}");
            let gs: Vec<&str> = shown.graphemes(true).collect();
            let i = (0..=gs.len()).find(|&i| util::width(&gs[..i].concat()) == col);
            proptest::prop_assert!(i.is_some(), "cursor inside a grapheme of {shown:?}");
            let under = gs.get(i.unwrap()).copied();
            proptest::prop_assert_eq!(under, parts.get(at).copied(), "{:?}", shown);
        }
    }

    #[test]
    fn the_highlighted_issue_is_on_the_selected_cards_background() {
        use crate::app::{Field, FormKind, List};
        let lines = vec!["ENG-2 Task 2".to_string(), "ENG-3 Task 3".into(), "↓ 1 more".into()];
        let list = List { lines, picks: 3, at: Some(2), top: 1 };
        let field = Field { list, ..Field::text("Ticket", "", "") };
        let form = Form::new(FormKind::Ticket, "Add", vec![field]);
        let bg = |active: bool| -> Vec<bool> {
            let lines = form_lines(&form, active, &[], 40).body.lines;
            let shaded = |l: &Line| l.spans.iter().any(|s| s.style.bg == Some(selection_bg()));
            lines[2..5].iter().map(shaded).collect()
        };
        assert_eq!(bg(true), [false, true, false]);
        // Only while the form has the focus.
        assert_eq!(bg(false), [false; 3]);
    }

    #[test]
    fn prose_fields_wrap_under_their_value() {
        use crate::app::{Field, FormKind};
        let role = "Review every open PR in acme each morning, leave comments where tests are missing, and message the ticket sessions whose PRs need changes before standup.";
        let field = Field { prose: true, ..Field::text("Role", role, "optional") };
        let mut form =
            Form::new(FormKind::Ticket, "Add", vec![Field::text("Name", "reviewer", ""), field]);
        focus_on(&mut form, "Role");
        let width = 40;
        let lines = |form: &Form| {
            let Scrolled { body, cursor, .. } = form_lines(form, true, &[], width);
            let lines = body.lines;
            assert_eq!(form_rows(lines.clone(), width), lines.len() as u16, "a line wrapped");
            (lines.iter().map(|l| l.to_string()).collect::<Vec<_>>(), cursor)
        };
        // Focused: the last four lines, the cursor after the last word.
        let (text, cursor) = lines(&form);
        let expect = |rows: [&str; 5]| rows.map(String::from);
        assert_eq!(
            text[2..7],
            expect([
                "Role  morning, leave comments where",
                "      tests are missing, and message",
                "      the ticket sessions whose PRs",
                "      need changes before standup.",
                "      optional",
            ])
        );
        assert_eq!(cursor, Some((5, 6 + 28)));
        set(&mut form, "Role", "Review open PRs");
        assert_eq!(lines(&form).1, Some((2, 6 + 15)));
        // Not focused: the first four lines, the fourth cut.
        set(&mut form, "Role", role);
        focus_on(&mut form, "Name");
        let (text, _) = lines(&form);
        assert_eq!(
            text[2..7],
            expect([
                "Role  Review every open PR in acme each",
                "      morning, leave comments where",
                "      tests are missing, and message",
                "      the ticket sessions whose PRs ne…",
                "      optional",
            ])
        );
    }

    #[test]
    fn close_dialog_lists_what_r_removes() {
        let text = |lines: Vec<Line>| lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let branch = "eng/eng-52-make-the-login-page-remember-the-last-workspace-you-used";
        let lines = text(removal_lines("~/acme-ENG-52", Some(branch), &[], 62));
        assert_eq!(lines[3], "  ~/acme-ENG-52");
        // Shortened to fit with its note, never wrapped mid-name.
        assert_eq!(lines[4], "  eng/eng-52-make-the-login-page-remember-th…  (merged branch)");
        assert_eq!(lines[4].chars().count(), 62);
        let lines = text(removal_lines(
            "/home/me/work/clients/acme/worktrees/acme-ENG-52-login-remembers-workspace",
            None,
            &[],
            40,
        ));
        assert_eq!(lines[3], "  …acme-ENG-52-login-remembers-workspace");
        assert_eq!(lines.last().unwrap(), "Its branch isn't merged, so it stays.");
        // Drawn, the indent survives (wrapping with trim would strip it).
        let p = confirm_text(
            "Close ENG-52?",
            removal_lines("~/acme-ENG-52", Some("eng/eng-52"), &[], 62),
            YES_NO,
            62,
        );
        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 62, 9));
        ratatui::widgets::Widget::render(p, buf.area, &mut buf);
        let row = |y| {
            (0..62)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        assert_eq!(row(5), "  ~/acme-ENG-52");
        assert_eq!(row(6), "  eng/eng-52  (merged branch)");
        assert_eq!(row(8), "Enter yes   Esc cancel");

        // Ignored files: the dialog says removing deletes them, and lists them, shortened.
        let ignored: Vec<String> =
            [".env", "data.db", "notes/", "secrets.json"].map(String::from).to_vec();
        let lines = text(removal_lines("~/acme-ENG-52", None, &ignored, 69));
        assert_eq!(
            lines[2],
            "Removing it also deletes files git ignores (not saved anywhere else):"
        );
        assert_eq!(lines[3], "  ignored: .env, data.db and 2 more");
        assert_eq!(lines[4], "  ~/acme-ENG-52");
        // The key row names what each key does with them, and sets the dialog's width.
        let keys = [
            ("Enter", "close, keep the worktree"),
            ("d", "close and delete them"),
            ("Esc", "cancel"),
        ];
        assert_eq!(
            key_row(&keys).to_string(),
            "Enter close, keep the worktree   d close and delete them   Esc cancel"
        );
        assert!(key_row(&keys).width() > usize::from(CONFIRM_W - 2 - DIALOG_PAD_W));
        let long = ["a".repeat(80)];
        let lines = text(removal_lines("~/acme-ENG-52", None, &long, 40));
        assert_eq!(lines[3].chars().count(), 40);
    }
}
