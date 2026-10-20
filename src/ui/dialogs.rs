//! What draws over the screen: confirm dialogs, links, forms, settings and help.

use super::sidebar::{more_line, scroll_to_show, shown_rows};
use super::{asleep, badge, dim, selection_bg, spinner};
use crate::app::{AgentAsk, AgentState, App, Form, Level, Mode, SettingsItem, toasts};
use crate::keys::prefix;
use crate::store::tilde;
use crate::untrusted;
use crate::util::{truncate, truncate_middle, truncate_start, wrap_words};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

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
        Mode::ConfirmAgent(ask) => {
            let (question, body) = agent_ask_text(ask);
            draw_confirm_lines(f, &question, body, &[("Enter", "approve"), ("Esc", "decline")]);
        }
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
            Span::styled(truncate(&under, room.saturating_sub(note.len())), dim().fg(Color::Cyan)),
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
        text.push(Line::from(Span::styled("Nothing is waiting on you", dim())));
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
        Paragraph::new(text).block(dialog("Waiting on you, oldest first").padding(DIALOG_PAD)),
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

/// The dialog for an agent `taskman add` asked for: who asks, the agent, and its role in full
/// (up to `ROLE_SHOWN`), since the role goes into the agent's system prompt.
fn agent_ask_text(ask: &AgentAsk) -> (String, Vec<Line<'static>>) {
    let (name, role) = ask.req.agent().unwrap_or_default();
    let name = crate::store::slug(name);
    let from = ask.req.from.as_deref().map_or("taskman add".to_string(), untrusted::line);
    let question = match (ask.open, role) {
        (true, Some(_)) => format!("{from} asks to change ✦ {name}'s role"),
        _ => format!("{from} asks to add agent ✦ {name}"),
    };
    let mut body = vec![Line::from(match (ask.open, role) {
        (true, Some(_)) => "The role goes into its startup brief when its session next starts:",
        (true, None) => "It's open already, so this changes nothing.",
        (false, Some(_)) => "Its session starts at once, with this role in its startup brief:",
        (false, None) => "Its session starts at once in the project's main checkout.",
    })];
    if let Some(role) = role {
        let shown: String = role.chars().take(ROLE_SHOWN).collect();
        body.extend(untrusted::block(&shown, 2).lines().map(|l| Line::from(l.to_string())));
        let more = role.chars().count().saturating_sub(ROLE_SHOWN);
        if more > 0 {
            body.push(Line::from(Span::styled(format!("  … and {more} more characters"), dim())));
        }
    }
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "Sessions run taskman add without asking you, and text they read, such as an issue or a PR comment, can tell \
         them to. Approve it only if you expect it.",
        dim(),
    )));
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
            let room = width.saturating_sub(key.width() + 1);
            if room > 0 {
                hint.push(Span::raw(format!(" {}", truncate(what, room))));
            }
        }
        let w: usize = hint.iter().map(|s| s.content.width()).sum();
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
    let row = u16::try_from(key_row(keys).width()).unwrap_or(u16::MAX);
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
    let room = |note: &str| width.saturating_sub(2 + note.chars().count());
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
/// issues to pick from, else ←/→ for a choice.
fn editing_keys(focused: &crate::app::Field) -> Vec<(&'static str, &'static str)> {
    let mut keys = vec![("Enter", "save"), ("Tab", "next field")];
    if focused.list.picks > 0 {
        keys.insert(0, ("↑/↓", "choose"));
    } else {
        keys.push(("←/→", "change choice"));
    }
    keys
}

fn draw_form(f: &mut Frame, app: &App) {
    let Mode::Form(form) = &app.mode else { return };
    let mut keys = editing_keys(&form.fields[form.focus]);
    keys.extend([("ctrl+u", "clear"), ("Esc", "cancel")]);
    let w = (78 + DIALOG_PAD_W).min(f.area().width.saturating_sub(2));
    // The note can wrap on a narrow terminal; size to the wrapped text so the key row still shows.
    let inner_w = w.saturating_sub(2 + DIALOG_PAD_W);
    let rows = form_rows(form_lines(form, true, &keys, inner_w).0.lines, inner_w);
    let r = centered(f, w, rows + 2 + DIALOG_PAD_H);
    f.render_widget(Clear, r);
    let block = dialog(form.title).padding(DIALOG_PAD);
    let inner = block.inner(r);
    f.render_widget(block, r);
    render_form(f, form, inner, true, &keys);
}

/// Draws a form's note, fields with hints, error and key line into `area`. `active` shows the
/// focused field and the text cursor.
fn render_form(f: &mut Frame, form: &Form, area: Rect, active: bool, keys: &[(&str, &str)]) {
    let (text, cursor) = form_lines(form, active, keys, area.width);
    if let Some((line, col)) = cursor {
        let row = form_rows(text.lines[..line].to_vec(), area.width);
        let x = (area.x + col).min(area.right().saturating_sub(1));
        f.set_cursor_position((x, area.y + row));
    }
    let paragraph = |lines| Paragraph::new(lines).wrap(Wrap { trim: false });
    if !text.urls().is_empty() {
        let mut coded = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(paragraph(text.coded(0)), area, &mut coded);
        f.render_widget(paragraph(text.lines.clone()), area);
        let urls: Vec<&str> = text.urls().iter().map(String::as_str).collect();
        crate::links::mark_coded(f.buffer_mut(), &coded, area, &urls);
    } else {
        f.render_widget(paragraph(text.lines), area);
    }
}

/// Rows `lines` take wrapped at `width`, as `render_form` draws them.
fn form_rows(lines: Vec<Line>, width: u16) -> u16 {
    Paragraph::new(lines).wrap(Wrap { trim: false }).line_count(width.max(1)) as u16
}

/// A form's lines with their links, and the focused text field's line and column for the cursor
/// when `active`. Values are cut to fit `width` and hints wrap under them, so neither runs under
/// the labels; notes wrap across the whole width.
fn form_lines(
    form: &Form,
    active: bool,
    keys: &[(&str, &str)],
    width: u16,
) -> (crate::links::Linked, Option<(usize, u16)>) {
    let label_w = form
        .fields
        .iter()
        .filter(|f| !f.hidden)
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(8)
        + 2;
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
    let mut cursor = None;
    for (i, field) in form.fields.iter().enumerate().filter(|(_, f)| !f.hidden) {
        if let Some(title) = field.section {
            text.push(Line::from(""));
            text.push(Line::from(Span::styled(
                title.to_string(),
                dim().add_modifier(ratatui::style::Modifier::BOLD),
            )));
        }
        let focused = active && i == form.focus;
        let label_style = if focused { Style::new().fg(Color::Cyan).bold() } else { Style::new() };
        let mut row = vec![Span::styled(format!("{:<label_w$}", field.label), label_style)];
        // Room for the value after the label and the ‹ › around a choice or presets.
        let arrows = if field.choices.is_empty() { 0 } else { 4 };
        let room = (width as usize).saturating_sub(label_w + arrows);
        // A prose value's lines after its first, indented to start under the value.
        let mut more = Vec::new();
        if field.prose {
            // One column kept for the cursor, as below.
            let mut lines = wrap_prose(&field.value, room.saturating_sub(1));
            if lines.len() > PROSE_LINES {
                if focused {
                    // Scrolled to the end, where the cursor is.
                    lines.drain(..lines.len() - PROSE_LINES);
                } else {
                    // The line after the last didn't fit on it, so joined they're cut with `…`.
                    let last = format!("{} {}", lines[PROSE_LINES - 1], lines[PROSE_LINES]);
                    lines.truncate(PROSE_LINES);
                    lines[PROSE_LINES - 1] = truncate(&last, room.saturating_sub(1));
                }
            }
            if focused {
                let last = lines.len() - 1;
                cursor = Some((text.len() + last, (label_w + lines[last].chars().count()) as u16));
            }
            let mut lines = lines.into_iter();
            row.push(Span::raw(lines.next().unwrap_or_default()));
            more.extend(lines.map(|l| Line::from(format!("{:label_w$}{l}", ""))));
        } else if field.is_text() {
            // Text with presets: the arrows say ←/→ cycles them, the cursor says typing edits.
            let presets = !field.choices.is_empty();
            let arrow = |s: &str| {
                Span::styled(
                    s.to_string(),
                    if focused { Style::new().fg(Color::Cyan) } else { dim() },
                )
            };
            if presets {
                row.push(arrow("‹ "));
            }
            // The cursor is at the end, so a value being typed keeps its end in view; one room
            // is kept for the cursor. A secret shows as a dot a character.
            let dots = field.masked.then(|| "•".repeat(field.value.chars().count()));
            let value = match (&dots, focused) {
                (Some(d), true) => truncate_start(d, room.saturating_sub(1)),
                (Some(d), false) => truncate(d, room),
                (None, true) => truncate_start(&field.value, room.saturating_sub(1)),
                (None, false) => fit(field, room),
            };
            if focused {
                let open = if presets { 2 } else { 0 };
                cursor = Some((text.len(), (label_w + open + value.chars().count()) as u16));
            }
            row.push(Span::raw(value));
            if presets {
                row.push(arrow(" ›"));
            }
        } else {
            let style = if focused {
                Style::new().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::new().bold()
            };
            row.push(Span::styled(format!("‹ {} ›", fit(field, room)), style));
        }
        text.push(Line::from(row));
        text.extend(more);
        // A long hint wraps under the value too, not under the labels.
        let hint_room = (width as usize).saturating_sub(label_w);
        text.extend(
            wrap_prose(&field.hint, hint_room)
                .into_iter()
                .map(|l| Line::from(Span::styled(format!("{:label_w$}{l}", ""), dim()))),
        );
        for (before, label, url) in &field.links {
            // A URL shown as itself starts a line of its own, so it wraps as late as it can, and
            // what leads to it goes on the line above.
            let shown = if label == url {
                text.extend(
                    wrap_prose(before, hint_room)
                        .into_iter()
                        .filter(|l| !l.is_empty())
                        .map(|l| Line::from(Span::styled(format!("{:label_w$}{l}", ""), dim()))),
                );
                label.clone()
            } else {
                format!("{before}{label}")
            };
            for l in wrap_prose(&shown, hint_room) {
                linked.push((text.len(), url.clone()));
                text.push(Line::from(vec![
                    Span::raw(format!("{:label_w$}", "")),
                    Span::styled(l, dim().add_modifier(ratatui::style::Modifier::UNDERLINED)),
                ]));
            }
        }
        // The highlighted issue on the sidebar's selected card's background, across the room.
        let highlighted = field.list.highlighted().filter(|_| focused);
        text.extend(field.list.lines.iter().enumerate().map(|(n, l)| {
            let l = truncate(l, hint_room);
            match highlighted {
                Some(h) if h == n => Line::from(vec![
                    Span::raw(format!("{:label_w$}", "")),
                    Span::styled(format!("{l:<hint_room$}"), Style::new().bg(selection_bg())),
                ]),
                _ => Line::from(format!("{:label_w$}{l}", "")),
            }
        }));
    }
    text.push(Line::from(""));
    match &form.error {
        _ if form.checking.is_some() => {
            text.push(Line::from(Span::styled(
                format!("{} Checking the connection…", spinner()),
                dim(),
            )));
        }
        Some(e) => text.push(Line::from(Span::styled(e.clone(), Style::new().fg(Color::Red)))),
        None => text.push(Line::from("")),
    }
    text.extend(key_rows(keys, usize::from(width)));
    let mut out = crate::links::Linked::default();
    for (n, line) in text.into_iter().enumerate() {
        out.push(line);
        for (_, url) in linked.iter().filter(|(l, _)| *l == n) {
            out.link(1, url);
        }
    }
    (out, cursor)
}

/// The most lines a prose field takes; a longer one scrolls while typed and is cut otherwise.
const PROSE_LINES: usize = 4;

/// `s` wrapped at spaces into lines of at most `width` characters; a word longer than that is
/// split. The space a line breaks at is dropped, but every other character is kept, a trailing
/// space too, so the end of the last line is where typing continues.
fn wrap_prose(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = s.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;
    loop {
        if chars.len() - start <= width {
            lines.push(chars[start..].iter().collect());
            return lines;
        }
        // A space just past a full line breaks there too.
        match (start + 1..=start + width).rev().find(|&i| chars[i] == ' ') {
            Some(i) => {
                lines.push(chars[start..i].iter().collect());
                start = i + 1;
            }
            None => {
                lines.push(chars[start..start + width].iter().collect());
                start += width;
            }
        }
    }
}

/// A field's value cut to `room` columns: at the end, or for `cut_middle` in the middle of what
/// follows `who · `, so an account keeps who it is and both ends of its source.
fn fit(field: &crate::app::Field, room: usize) -> String {
    if !field.cut_middle {
        return truncate(&field.value, room);
    }
    match field.value.split_once(" · ") {
        Some((who, source)) if who.chars().count() + 3 + 12 <= room => {
            format!("{who} · {}", truncate_middle(source, room - who.chars().count() - 3))
        }
        _ => truncate_middle(&field.value, room),
    }
}

/// Settings: updates, projects and accounts listed on the left, the selected one's form on
/// the right.
fn draw_settings(f: &mut Frame, app: &mut App) {
    let Mode::Settings(s) = &app.mode else { return };
    let deletable =
        matches!(s.items[s.cursor], SettingsItem::Project(_) | SettingsItem::Account(_));
    let mut keys = if s.editing {
        editing_keys(&s.form.fields[s.form.focus])
    } else {
        vec![("↑/↓", "choose"), ("Enter", "edit")]
    };
    if deletable {
        keys.push(("ctrl+d", "delete"));
    }
    keys.push(("Esc", if s.editing { "back to list" } else { "close" }));
    // Tall enough for the form, wrapped hints and keys included, so its error and key lines
    // always show; 3 rows for the borders and the pane title. The form is as wide as the dialog
    // `centered` gives, less its borders, the list and the pane's border and gap.
    let form_w = 108u16.min(f.area().width.saturating_sub(2)).saturating_sub(2 + 30 + 2);
    let rows = form_rows(form_lines(&s.form, s.editing, &keys, form_w).0.lines, form_w);
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
            _ => None,
        };
        if let Some(title) = heading {
            section(&mut lines, title);
        }
        at.push((lines.len(), heading.is_some()));
        let (left, right, new) = match item {
            SettingsItem::General => {
                let how = if app.updater.installs_automatically(app.config.auto_update) {
                    "auto"
                } else {
                    "notify"
                };
                ("Prefix and updates".to_string(), how.to_string(), false)
            }
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
        };
        let selected = row == s.cursor;
        let room = w.saturating_sub(right.chars().count() + 2);
        let name = truncate(&left, room);
        let pad = w.saturating_sub(name.chars().count() + right.chars().count() + 1);
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
        ("1-9", "switch to ticket"),
        ("[ / ]", "previous / next ticket"),
        ("{ / }", "move the card; ↑/↓ go on, p moves its project, Enter ends"),
        (again.as_str(), "next ticket waiting on you (also tab)"),
        ("/", "jump to a ticket, agent or project by typing"),
        ("w", "list the sessions waiting on you, oldest first, with what they ask"),
        ("n", "add a ticket, or an agent with no ticket"),
        (",", "settings: prefix key, updates, projects, accounts"),
        ("P / A", "settings, on a new project / account"),
        ("z", "fold / unfold the current project"),
        ("x", "close the current ticket or agent"),
        ("b", "collapse / expand the sidebar"),
        ("j / c / g", "toggle the tracker panel, its comments, the PR panel"),
        ("o / p", "open ticket / PR in your browser"),
        ("r", "refresh tracker and GitHub now"),
        ("m", "mark new comments on this ticket as read"),
        ("R / t", "restart the session / hand it to another terminal"),
        ("M", "run in the main checkout when the worktree failed"),
        ("e", "rename the current agent"),
        ("PgUp/PgDn", "scroll the session's history"),
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
    let key_w = keys.iter().map(|(k, _)| format!("{p} {k}").width()).max().unwrap_or(0) + 2;
    let badge_w = legend.iter().map(|(b, _)| b.content.width()).max().unwrap_or(0) + 2;
    // Two to a row, each column as wide as its own words.
    let words = |skip: usize| {
        legend.iter().skip(skip).step_by(2).map(|(_, what)| what.width()).max().unwrap_or(0)
    };
    let what_w = words(0) + 3;
    let keys_w = keys.iter().map(|(_, what)| indent + key_w + what.width()).max().unwrap_or(0);
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
            let left =
                if i == 0 { format!("{:indent$}{key:<key_w$}", "") } else { " ".repeat(col) };
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
                let pad = " ".repeat(badge_w - b.content.width());
                let what = if n == 0 { format!("{what:<what_w$}") } else { what.to_string() };
                row.extend([b.clone(), Span::raw(pad), Span::raw(what)]);
            }
            text.push(Line::from(row));
        }
    } else {
        let col = 2 + badge_w;
        for (b, what) in &legend {
            for (i, l) in wrap_words(what, inner.saturating_sub(col)).into_iter().enumerate() {
                let mut row = vec![Span::raw("  ")];
                if i == 0 {
                    row.extend([b.clone(), Span::raw(" ".repeat(badge_w - b.content.width()))]);
                } else {
                    row.push(Span::raw(" ".repeat(badge_w)));
                }
                row.push(Span::raw(l));
                text.push(Line::from(row));
            }
        }
    }
    // Below the text, a blank row and what keys do, which says where you are when it scrolls.
    // Sized for its longest form, so the rows left for the text stay put as it scrolls; without
    // PgUp/PgDn where that keeps it to a row.
    let n = text.len() as u16;
    let longest = format!("{n}-{n} of {n}");
    let scroll = if key_row(&scroll_hints(SCROLL_KEYS, Some(&longest))).width() <= inner {
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
        for width in 0..30 {
            for row in rows(&keys, width) {
                assert!(row.width() <= width.max(1), "{row:?} at {width}");
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
        assert_eq!(
            keys(&form),
            [("Enter", "save"), ("Tab", "next field"), ("←/→", "change choice")]
        );
        form.fields[0].list.picks = 3;
        assert_eq!(keys(&form), [("↑/↓", "choose"), ("Enter", "save"), ("Tab", "next field")]);
        // At the bottom of the form, broken between hints at its width.
        let mut all = keys(&form);
        all.extend([("ctrl+u", "clear"), ("Esc", "cancel")]);
        let (lines, _) = form_lines(&form, true, &all, 40);
        let text: Vec<String> = lines.lines.iter().map(|l| l.to_string()).collect();
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
            let (lines, cursor) = form_lines(form, true, &[], width);
            let lines = lines.lines;
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
        let (lines, _) = form_lines(&form, false, &[], width);
        let lines = lines.lines;
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
    }

    #[test]
    fn the_highlighted_issue_is_on_the_selected_cards_background() {
        use crate::app::{Field, FormKind, List};
        let lines = vec!["ENG-2 Task 2".to_string(), "ENG-3 Task 3".into(), "↓ 1 more".into()];
        let list = List { lines, picks: 3, at: Some(2), top: 1 };
        let field = Field { list, ..Field::text("Ticket", "", "") };
        let form = Form::new(FormKind::Ticket, "Add", vec![field]);
        let bg = |active: bool| -> Vec<bool> {
            let (lines, _) = form_lines(&form, active, &[], 40);
            let lines = lines.lines;
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
            let (lines, cursor) = form_lines(form, true, &[], width);
            let lines = lines.lines;
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
