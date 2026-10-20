//! The right column: the tracker and GitHub panels beside a ticket, the board beside an agent.

use super::{dim, pr_spans, pr_state, spinner, state_badge, yellow};
use crate::app::{AgentState, App, Remote};
use crate::github::{CheckState, Mergeable, Pr};
use crate::keys::prefix;
use crate::links::Linked;
use crate::store::Cli;
use crate::tracker::Access;
use crate::untrusted;
use crate::util::{ago, ago_str, ago_words, parse_ts, truncate};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap};

fn panel<'a>(title: Line<'a>, freshness: Line<'a>, hint: &'a str) -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .padding(Padding::horizontal(1))
        .title(title)
        .title_top(freshness.right_aligned())
        .title_bottom(Line::from(Span::styled(hint, dim())).right_aligned())
}

/// "⠹ refreshing" while a fetch the user is waiting on runs, otherwise how long ago the data
/// was fetched.
fn freshness<T>(r: &Remote<T>) -> Line<'static> {
    if r.loading && r.loud {
        return Line::from(Span::styled(
            format!(" {} refreshing ", spinner()),
            Style::new().fg(Color::Cyan),
        ));
    }
    match r.fetched_at {
        Some(t) => {
            // Minute buckets, so the label changes at most once a minute.
            let age = ago_words(i64::try_from(t.elapsed().as_secs()).unwrap_or(i64::MAX));
            let style = if r.error.is_some() { Style::new().fg(Color::Red) } else { dim() };
            Line::from(Span::styled(format!(" updated {age} "), style))
        }
        None => Line::from(""),
    }
}

/// Why the last refresh failed, above data that is still shown from before: `freshness` turns the
/// panel's label red then, so a panel showing it says why.
fn refresh_failed(lines: &mut Linked, error: Option<&str>) {
    if let Some(e) = error {
        lines.push(Line::from(Span::styled(
            format!("⚠ refresh failed: {e}"),
            Style::new().fg(Color::Red),
        )));
    }
}

/// "1 older comment", "3 older comments".
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn new_tag(is_new: bool) -> Vec<Span<'static>> {
    if is_new { vec![Span::styled("● ", Style::new().fg(yellow()).bold())] } else { vec![] }
}

fn section(title: &str, total: usize, new: usize) -> Line<'static> {
    let mut v = vec![Span::styled(format!("── {title} ({total})"), Style::new().bold())];
    if new > 0 {
        v.push(Span::styled(format!(" · {new} new"), Style::new().fg(yellow()).bold()));
    }
    v.push(Span::styled(" ──", Style::new().bold()));
    Line::from(v)
}

fn store_site(site: &str) -> String {
    crate::store::site_host(site)
}

fn status_color(category: &str) -> Color {
    match category {
        "done" => Color::Green,
        "indeterminate" => yellow(),
        _ => Color::Blue,
    }
}

pub(super) fn draw_jira(f: &mut Frame, app: &mut App, area: Rect) {
    let show_comments = app.prefs.show_comments;
    let Some(sel) = app.selected() else { return };
    let access = app.access(&sel.t);
    let local = access.board().is_some();
    let provider = app
        .project_of(&sel.t)
        .and_then(|p| app.projects[p].tracker.as_ref())
        .map(|t| t.provider.label())
        .unwrap_or("Tracker");
    let url = app.issue_url(&sel.t).unwrap_or_default();
    let Some(tv) = app.tickets.get_mut(app.sel) else { return };
    // The key opens the issue while there's no title line (`panel_head`; `url` is empty for Local
    // files, so nothing is linked).
    let mut title = vec![
        Span::styled(format!(" {provider} "), Style::new().bold()),
        Span::raw(tv.t.key.clone()),
        Span::raw(" "),
    ];
    let mut lines = Linked::default();
    let mut titled = false;
    let setup = |lines: &mut Linked, head: String, body: &[&str]| {
        lines.push(Line::from(head.bold()));
        lines.push(Line::from(""));
        for l in body {
            lines.push(Line::from(l.to_string()));
        }
    };

    if let Access::NoAccount(p, site) = &access {
        setup(
            &mut lines,
            format!("No {} account picked for this project ({}).", p.label(), store_site(site)),
            &[&format!("Open the project's settings with {} , to pick one or add one.", prefix())],
        );
    } else if let Access::Failed(e) = &access {
        lines.push(Line::from(Span::styled(format!("⚠ {e}"), Style::new().fg(Color::Red))));
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            "Check the account's token, or pick another in the project's settings ({} ,).",
            prefix()
        )));
    } else if let Some(issue) = &tv.jira.data {
        title.push(Span::styled(
            format!("{} ", untrusted::line(&issue.status)),
            Style::new().fg(status_color(&issue.status_category)),
        ));
        lines.push(Line::from(untrusted::line(&issue.summary).bold()));
        lines.link(0, &url);
        titled = true;
        let meta = [issue.issue_type.as_str(), issue.priority.as_str(), issue.assignee.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(Line::from(Span::styled(untrusted::line(&meta), dim())));
        if !issue.prs.is_empty() {
            let mut spans = vec![Span::styled("Closing PRs: ", dim())];
            let mut links = Vec::new();
            for (n, p) in issue.prs.iter().enumerate() {
                if n > 0 {
                    spans.push(Span::styled(" · ", dim()));
                }
                links.push((spans.len(), p.url.clone()));
                spans.push(Span::raw(format!("#{} {}", p.number, p.state)));
            }
            lines.push(Line::from(spans));
            for (span, url) in links {
                lines.link(span, &url);
            }
        }
        if let (Some(board), Some((path, _))) = (access.board(), &tv.local_file) {
            lines.push(Line::from(Span::styled(crate::local::rel(board, path), dim())));
        }
        refresh_failed(&mut lines, tv.jira.error.as_deref());
        lines.push(Line::from(""));
        if issue.description.is_empty() {
            lines.push(Line::from(Span::styled("No description.", dim())));
        } else {
            for l in issue.description.lines() {
                lines.text("", l);
            }
        }
        // Local files have no comments; any are in the body.
        if show_comments && !local {
            lines.push(Line::from(""));
            let new = issue.comments.iter().filter(|c| tv.is_new_jira(&c.id)).count();
            lines.push(section("Comments", issue.comments.len(), new));
            if issue.comments.is_empty() {
                lines.push(Line::from(Span::styled("No comments yet.", dim())));
            }
            for c in &issue.comments {
                lines.push(Line::from(""));
                let mut head = new_tag(tv.is_new_jira(&c.id));
                head.push(Span::styled(
                    untrusted::line(&c.author),
                    Style::new().fg(Color::Cyan).bold(),
                ));
                head.push(Span::styled(format!(" · {}", ago_str(&c.created)), dim()));
                lines.push(Line::from(head));
                for l in c.body.lines() {
                    lines.text("", l);
                }
            }
            if issue.comments_left_out > 0 {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} not loaded; o opens the ticket.",
                        count(issue.comments_left_out, "older comment", "older comments")
                    ),
                    dim(),
                )));
            }
        }
    } else if let Some(e) = &tv.jira.error {
        lines.push(Line::from(Span::styled(format!("⚠ {e}"), Style::new().fg(Color::Red))));
    } else {
        lines.push(Line::from(Span::styled(format!("{} Loading…", spinner()), dim())));
    }

    tv.jira_scroll = tv.jira_scroll.min(max_scroll(&lines.lines, area));
    let p = prefix().short();
    let hint = match (local, show_comments) {
        (true, _) => " o edit in $EDITOR ".to_string(),
        (false, true) => format!(" {p} r refresh · o open · c hide comments "),
        (false, false) => format!(" {p} r refresh · o open · c comments "),
    };
    // A local file is reread as it changes, so it's always current.
    let status = if local { Line::from("") } else { freshness(&tv.jira) };
    let scroll = tv.jira_scroll;
    render_panel(f, app, area, panel_head(title, &url, titled), &status, &hint, lines, scroll);
}

/// A panel's border title, its second span (the key, or the PR's number) linked to `url` only
/// while the panel has no title line linking there: the terminal underlines a link's parts
/// together, and the border's adds nothing beside the title's.
fn panel_head(title: Vec<Span<'static>>, url: &str, titled: bool) -> Linked {
    let mut head = Linked::default();
    head.push(title);
    if !titled {
        head.link(1, url);
    }
    head
}

/// Draws a panel: a block titled with `title`'s line, `body`'s lines in it from `scroll`, and the
/// links in both passed on to the terminal (see `Linked`).
#[allow(clippy::too_many_arguments)]
fn render_panel(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    title: Linked,
    status: &Line<'static>,
    hint: &str,
    body: Linked,
    scroll: u16,
) {
    note_panel(app, &panel(Line::default(), Line::default(), hint), area, &body.lines);
    draw_panel(f.buffer_mut(), area, title, status, hint, body, scroll);
}

/// `render_panel`'s drawing: the panel, then the same panel drawn from the coded lines into a
/// buffer of its own, to find the cells to wrap in OSC 8.
fn draw_panel(
    buf: &mut Buffer,
    area: Rect,
    title: Linked,
    status: &Line<'static>,
    hint: &str,
    body: Linked,
    scroll: u16,
) {
    let draw = |buf: &mut Buffer, title: Line<'static>, lines: Vec<Line<'static>>| {
        let p = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0));
        ratatui::widgets::Widget::render(p.block(panel(title, status.clone(), hint)), area, buf);
    };
    let urls: Vec<String> = title.urls().iter().chain(body.urls()).cloned().collect();
    let coded = (!urls.is_empty()).then(|| {
        let mut coded = Buffer::empty(area);
        draw(
            &mut coded,
            title.coded(0).into_iter().next().unwrap_or_default(),
            body.coded(title.urls().len()),
        );
        coded
    });
    draw(buf, title.lines.into_iter().next().unwrap_or_default(), body.lines);
    if let Some(coded) = coded {
        let urls: Vec<&str> = urls.iter().map(String::as_str).collect();
        crate::links::mark_coded(buf, &coded, area, &urls);
    }
}

/// The board beside an agent: for each ticket in its project, what the sidebar can't show (why it
/// needs you, its branch, failing and running checks, the latest review and comment).
pub(super) fn draw_board(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(sel) = app.selected() else { return };
    let Some(p) = app.project_of(&sel.t) else { return };
    let sel_cli = sel.t.cli;
    let name = app.projects[p].name.clone();
    let w = area.width.saturating_sub(6) as usize; // borders, padding, indent
    let members = app.members(p);
    let tickets: Vec<usize> =
        members.iter().copied().filter(|&i| app.tickets[i].t.agent.is_none()).collect();
    let agents: Vec<usize> =
        members.iter().copied().filter(|&i| app.tickets[i].t.agent.is_some()).collect();
    let mut lines = Linked::default();
    let indent = || Span::raw("  ");
    if tickets.is_empty() {
        lines.push(Line::from("No tickets in this project yet.".bold()));
        let hint = match sel_cli {
            Cli::Claude => {
                format!("Add one with {} n; this agent can then message it by its key.", prefix())
            }
            Cli::Codex => format!("Add one with {} n.", prefix()),
        };
        lines.push(Line::from(Span::styled(hint, dim())));
    }
    for &i in &tickets {
        let tv = &app.tickets[i];
        let mut head = vec![
            Span::styled(tv.t.key.clone(), Style::new().bold()),
            Span::raw("  "),
            state_badge(tv),
        ];
        if sel_cli == Cli::Claude && tv.t.cli != Cli::Claude {
            head.push(Span::styled(
                format!("  {} · can't message", tv.t.cli.label().to_lowercase()),
                dim(),
            ));
        }
        let new = tv.new_count();
        if new > 0 {
            head.push(Span::styled(format!("  {new} new"), Style::new().fg(yellow()).bold()));
        }
        lines.push(head);
        // The key opens the issue.
        lines.link(0, &app.issue_url(&tv.t).unwrap_or_default());
        if let Some(title) = tv.title() {
            lines
                .push(Line::from(vec![indent(), Span::raw(truncate(&untrusted::line(&title), w))]));
        }
        // Why Claude is waiting, in its own words.
        if tv.status.state == AgentState::Needs && !tv.status.message.is_empty() {
            lines.push(Line::from(vec![
                indent(),
                Span::styled(truncate(&tv.status.message, w), Style::new().fg(yellow())),
            ]));
        }
        if let Some(b) = &tv.t.branch {
            lines.push(Line::from(vec![
                indent(),
                Span::styled(truncate(&untrusted::line(b), w), dim()),
            ]));
        }
        let pr = tv.pr.data.as_ref().and_then(|p| p.as_ref());
        // Without GitHub there's none; the branch is on the line above.
        let summary = pr_spans(app, tv);
        if !summary.is_empty() {
            lines.push(Line::from([vec![indent()], summary].concat()));
        }
        if let Some(pr) = pr {
            // Checks that need attention, by name: failing first, then still running.
            let mut checks = vec![indent()];
            let mut runs = Vec::new();
            for c in pr.checks.iter().filter(|c| c.state == CheckState::Fail).take(2) {
                checks.push(Span::styled("✗ ", Style::new().fg(Color::Red)));
                runs.push((checks.len(), &c.url));
                checks.push(Span::raw(untrusted::line(&c.name)));
                if let Some(t) = c.when {
                    checks.push(Span::styled(format!(" · {}", ago(t)), dim()));
                }
                checks.push(Span::raw("   "));
            }
            for c in pr.checks.iter().filter(|c| c.state == CheckState::Pending).take(2) {
                checks.push(Span::styled("◷ ", Style::new().fg(yellow())));
                runs.push((checks.len(), &c.url));
                checks.push(Span::raw(untrusted::line(&c.name)));
                checks.push(Span::raw("   "));
            }
            if checks.len() > 1 {
                lines.push(checks);
                for (span, url) in runs {
                    lines.link(span, url);
                }
            }
            if let Some(r) = pr.latest_reviews().into_iter().max_by(|a, b| a.when.cmp(&b.when)) {
                lines.push(Line::from(vec![
                    indent(),
                    Span::styled(format!("{} ", r.state.glyph()), Style::new().fg(r.state.color())),
                    Span::raw(format!("{} {}", untrusted::line(&r.author), r.state.verb())),
                    Span::styled(format!(" · {}", ago_str(&r.when)), dim()),
                ]));
            }
        }
        // The latest comment from the tracker or the PR, on one line.
        let tracker =
            tv.jira.data.as_ref().and_then(|i| i.comments.first()).map(|c| {
                (c.created.clone(), c.author.clone(), c.body.clone(), tv.is_new_jira(&c.id))
            });
        let review = pr.and_then(|p| p.comments.first()).map(|c| {
            (c.when.clone(), c.author.clone(), c.body.clone(), tv.is_new_gh(&format!("c:{}", c.id)))
        });
        let latest = match (tracker, review) {
            (Some(a), Some(b)) => Some(if parse_ts(&a.0) >= parse_ts(&b.0) { a } else { b }),
            (a, b) => a.or(b),
        };
        if let Some((when, author, body, is_new)) = latest {
            let text = untrusted::line(&body);
            let lead = format!("{} · {}: ", untrusted::line(&author), ago_str(&when));
            let mut row = vec![indent()];
            if is_new {
                row.push(Span::styled("● ", Style::new().fg(yellow()).bold()));
            }
            row.push(Span::styled(lead.clone(), Style::new().fg(Color::Cyan)));
            row.push(Span::raw(truncate(
                &text,
                w.saturating_sub(lead.chars().count() + 2 * is_new as usize),
            )));
            lines.push(Line::from(row));
        }
        lines.push(Line::from(""));
    }
    // Listed when there are others to reach, or a role to show.
    if agents.len() > 1 || agents.iter().any(|&i| app.tickets[i].t.role.is_some()) {
        lines.push(Line::from("── Agents ──".bold()));
        for &i in &agents {
            let tv = &app.tickets[i];
            lines.push(Line::from(vec![
                Span::styled(format!("✦ {}", tv.t.display()), Style::new().bold()),
                Span::raw("  "),
                state_badge(tv),
            ]));
            if let Some(role) = &tv.t.role {
                lines.push(Line::from(vec![indent(), Span::styled(truncate(role, w), dim())]));
            }
        }
        lines.push(Line::from(""));
    }
    let reach = match sel_cli {
        Cli::Claude => {
            "This agent reaches Claude ticket sessions by key with ListAgents and SendMessage."
        }
        Cli::Codex => {
            "This Codex agent can't message ticket sessions; it can read their worktrees and PRs."
        }
    };
    lines.push(Line::from(Span::styled(reach, dim())));
    let tv = &mut app.tickets[app.sel];
    tv.jira_scroll = tv.jira_scroll.min(max_scroll(&lines.lines, area));
    let mut title = Linked::default();
    title.push(vec![Span::styled(" Project ", Style::new().bold()), Span::raw(format!("{name} "))]);
    let count = format!(" {} ticket{} ", tickets.len(), if tickets.len() == 1 { "" } else { "s" });
    let scroll = tv.jira_scroll;
    let count = Line::from(Span::styled(count, dim()));
    let hint = format!(" {} , settings ", prefix().short());
    render_panel(f, app, area, title, &count, &hint, lines, scroll);
}

/// A check's line, its name opening its run.
fn check_line(lines: &mut Linked, sym: &str, color: Color, c: &crate::github::Check, when: bool) {
    let mut v = vec![
        Span::styled(format!("{sym} "), Style::new().fg(color)),
        Span::raw(untrusted::line(&c.name)),
    ];
    if let Some(t) = c.when.filter(|_| when) {
        v.push(Span::styled(format!(" · {}", ago(t)), dim()));
    }
    lines.push(v);
    lines.link(1, &c.url);
}

fn pr_lines(pr: &Pr, is_new: impl Fn(&str) -> bool) -> Linked {
    let mut lines = Linked::default();
    lines.push(Line::from(untrusted::line(&pr.title).bold()));
    lines.link(0, &pr.url);
    lines.push(Span::styled(untrusted::line(&format!("{} → {}", pr.head, pr.base)), dim()));

    let mut status = vec![];
    let d = &pr.review_decision;
    if let Some(glyph) = d.glyph() {
        status.push(Span::styled(format!("{glyph} {d}"), Style::new().fg(d.color())));
    }
    if pr.mergeable == Mergeable::Conflicting {
        if !status.is_empty() {
            status.push(Span::raw("  "));
        }
        status.push(Span::styled("⚠ merge conflicts", Style::new().fg(Color::Red)));
    }
    if !status.is_empty() {
        lines.push(Line::from(status));
    }

    lines.push(Line::from(""));
    let (fail, pend, pass) =
        (pr.count(CheckState::Fail), pr.count(CheckState::Pending), pr.count(CheckState::Pass));
    lines.push(Line::from(vec![
        Span::styled("── Checks ", Style::new().bold()),
        Span::styled(format!("✓{pass} "), Style::new().fg(Color::Green)),
        Span::styled(format!("✗{fail} "), Style::new().fg(Color::Red)),
        Span::styled(format!("◷{pend} "), Style::new().fg(yellow())),
        Span::styled("──", Style::new().bold()),
    ]));
    if pr.checks.is_empty() {
        lines.push(Line::from(Span::styled("No checks reported.", dim())));
    }
    for c in pr.checks.iter().filter(|c| c.state == CheckState::Fail) {
        check_line(&mut lines, "✗", Color::Red, c, true);
    }
    for c in pr.checks.iter().filter(|c| c.state == CheckState::Pending) {
        check_line(&mut lines, "◷", yellow(), c, false);
    }
    let passed: Vec<_> = pr.checks.iter().filter(|c| c.state == CheckState::Pass).collect();
    if passed.len() <= 6 {
        for c in passed {
            check_line(&mut lines, "✓", Color::Green, c, false);
        }
    } else {
        lines.push(Span::styled(
            format!("✓ {} passed", passed.len()),
            Style::new().fg(Color::Green),
        ));
        lines.link(0, &format!("{}/checks", pr.url));
    }

    let latest = pr.latest_reviews();
    if !latest.is_empty() {
        lines.push(Line::from(""));
        let new = latest.iter().filter(|r| is_new(&format!("r:{}", r.id))).count();
        lines.push(section("Reviews", latest.len(), new));
        let text = pr.review_text();
        for r in latest {
            let mut row = new_tag(is_new(&format!("r:{}", r.id)));
            // The author and what they did open the review.
            let at = row.len() + 1;
            row.extend([
                Span::styled(format!("{} ", r.state.glyph()), Style::new().fg(r.state.color())),
                Span::styled(untrusted::line(&r.author), Style::new().bold()),
                Span::raw(format!(" {}", r.state.verb())),
                Span::styled(format!(" · {}", ago_str(&r.when)), dim()),
            ]);
            lines.push(row);
            lines.link(at, &r.url);
            lines.link(at + 1, &r.url);
            // What they said that still stands, this review's and their comments since.
            for t in text.iter().filter(|t| t.author == r.author) {
                if t.id != r.id {
                    lines.push(vec![
                        Span::raw("  "),
                        Span::styled(format!("commented · {}", ago_str(&t.when)), dim()),
                    ]);
                    lines.link(1, &t.url);
                }
                body_lines(&mut lines, &t.body, "  ");
            }
        }
    }

    match &pr.threads {
        Ok(t) if !t.list.is_empty() => {
            let open: Vec<_> = t.unresolved().collect();
            lines.push(Line::from(""));
            lines.push(section("Unresolved threads", open.len(), 0));
            for th in &open {
                lines.push(Line::from(""));
                let place = th.line.map_or(th.path.clone(), |l| format!("{}:{l}", th.path));
                let mut head =
                    vec![Span::styled(untrusted::line(&place), Style::new().fg(Color::Cyan))];
                if th.outdated {
                    head.push(Span::styled(" · outdated", dim()));
                }
                lines.push(head);
                // The place opens the thread; each author, their comment.
                if let Some(c) = th.comments.first() {
                    lines.link(0, &c.url);
                }
                for (i, c) in th.comments.iter().enumerate() {
                    if i == 1 && th.left_out > 0 {
                        lines.push(Span::styled(
                            format!("  … {}", count(th.left_out, "more reply", "more replies")),
                            dim(),
                        ));
                    }
                    lines.push(vec![
                        Span::raw("  "),
                        Span::styled(untrusted::line(&c.author), Style::new().bold()),
                        Span::styled(format!(" · {}", ago_str(&c.when)), dim()),
                    ]);
                    lines.link(1, &c.url);
                    body_lines(&mut lines, &c.body, "  ");
                }
            }
            let resolved = t.list.len() - open.len();
            if resolved > 0 {
                lines.push(Span::styled(
                    format!(
                        "{} not shown.",
                        count(resolved, "resolved thread", "resolved threads")
                    ),
                    dim(),
                ));
            }
            if t.left_out() > 0 {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} not loaded; p opens the PR.",
                        count(t.left_out(), "older thread", "older threads")
                    ),
                    dim(),
                )));
            }
        }
        Ok(_) => {}
        Err(e) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("⚠ review threads: {e}"),
                Style::new().fg(Color::Red),
            )));
        }
    }

    if !pr.comments.is_empty() {
        lines.push(Line::from(""));
        let new = pr.comments.iter().filter(|c| is_new(&format!("c:{}", c.id))).count();
        lines.push(section("Comments", pr.comments.len(), new));
        for c in pr.comments.iter().take(crate::links::SHOWN_COMMENTS.max(new)) {
            lines.push(Line::from(""));
            let mut head = new_tag(is_new(&format!("c:{}", c.id)));
            // The author opens the comment.
            let at = head.len();
            head.push(Span::styled(
                untrusted::line(&c.author),
                Style::new().fg(Color::Cyan).bold(),
            ));
            head.push(Span::styled(format!(" · {}", ago_str(&c.when)), dim()));
            lines.push(head);
            lines.link(at, &c.url);
            body_lines(&mut lines, &c.body, "");
        }
    }
    lines
}

/// A comment's first six non-blank lines, with `…` when there's more.
fn body_lines(out: &mut Linked, body: &str, pad: &str) {
    let body: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    for l in body.iter().take(6) {
        out.text(pad, l);
    }
    if body.len() > 6 {
        out.push(Span::styled(format!("{pad}…"), dim()));
    }
}

pub(super) fn draw_gh(f: &mut Frame, app: &mut App, area: Rect) {
    // A stacked ticket's standing, above the PR: yellow once the base merged, as there's
    // something to do.
    let stack = app.tickets.get(app.sel).and_then(|tv| {
        let (base, state) = app.stack_of(&tv.t)?;
        let pr = tv.pr.data.as_ref().and_then(Option::as_ref);
        let text = crate::stack::hint(base, pr, state, &app.default_branch(&tv.t.repo))?;
        let style =
            if state == crate::stack::State::Open { dim() } else { Style::new().fg(yellow()) };
        Some(Line::from(Span::styled(format!("↳ {text}"), style)))
    });
    let Some(tv) = app.tickets.get_mut(app.sel) else { return };
    let mut title = vec![Span::styled(" GitHub ", Style::new().bold())];
    let mut lines = Linked::default();
    if let Some(l) = stack {
        lines.push(l);
        lines.push("");
    }
    let mut url = String::new();
    match (&tv.pr.data, &tv.pr.error) {
        (Some(Some(pr)), err) => {
            url.clone_from(&pr.url);
            // The title line opens the PR, so the number doesn't (`panel_head`).
            title.extend([Span::raw(format!("PR #{}", pr.number)), Span::raw(" ")]);
            let (state, color) = pr_state(pr);
            title.push(Span::styled(format!("{state} "), Style::new().fg(color)));
            refresh_failed(&mut lines, err.as_deref());
            lines.append(pr_lines(pr, |id| tv.is_new_gh(id)));
        }
        (Some(None), err) => {
            refresh_failed(&mut lines, err.as_deref());
            lines.push(Line::from("No pull request for this ticket yet.".bold()));
            if let Some(n) = tv.closed_pr {
                lines.push(Line::from(Span::styled(
                    format!("#{n} was closed without merging."),
                    dim(),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "Looking for a branch or PR mentioning {}. Checked every 20s while this ticket is open.",
                    tv.t.key
                ),
                dim(),
            )));
        }
        (None, Some(e)) if e == crate::github::NO_REMOTE => {
            lines.push(Line::from(Span::styled(e.clone(), dim())));
        }
        (None, Some(e)) => {
            lines.push(Line::from(Span::styled(format!("⚠ {e}"), Style::new().fg(Color::Red))));
        }
        (None, None) => {
            lines.push(Line::from(Span::styled(format!("{} Loading…", spinner()), dim())));
        }
    }
    tv.gh_scroll = tv.gh_scroll.min(max_scroll(&lines.lines, area));
    let (status, scroll) = (freshness(&tv.pr), tv.gh_scroll);
    // A PR always comes with its title line.
    let head = panel_head(title, &url, !url.is_empty());
    let hint = format!(" {} r refresh · p open · g hide ", prefix().short());
    render_panel(f, app, area, head, &status, &hint, lines, scroll);
}

/// The furthest a panel drawn in `area` scrolls: its last wrapped row at the top.
fn max_scroll(lines: &[Line], area: Rect) -> u16 {
    // The border and a column of padding each side, as in `panel`.
    let width = area.width.saturating_sub(4).max(1);
    let rows = Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false }).line_count(width);
    u16::try_from(rows.saturating_sub(1)).unwrap_or(u16::MAX)
}

/// A panel holding the selection, rendered whole, so text scrolled out of view can be copied and
/// wrapped lines are joined again.
fn note_panel(app: &mut App, block: &Block, area: Rect, lines: &[Line]) {
    let inner = block.inner(area);
    if app.selection.as_ref().is_some_and(|s| s.rect == inner && s.snapshot.is_none()) {
        app.panel = Some(crate::selection::Panel::new(inner, lines));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_cells_carry_their_links() {
        use crate::github::{Check, PrComment};
        let pr_url = "https://github.com/acme/web/pull/7";
        let run = "https://github.com/acme/web/actions/runs/1/job/2";
        let long = "https://logs.example/acme/web/runs/123456789/attempts/1";
        let pr = Pr {
            number: 7,
            title: "Fix login".into(),
            url: pr_url.into(),
            state: crate::github::PrState::Open,
            is_draft: false,
            review_decision: crate::github::Decision::Unset,
            mergeable: crate::github::Mergeable::Unknown,
            head: "eng-52".into(),
            cross_repository: false,
            head_oid: String::new(),
            base: "main".into(),
            checks: vec![Check {
                name: "CI / lint".into(),
                state: CheckState::Fail,
                when: None,
                url: run.into(),
            }],
            reviews: vec![],
            comments: vec![PrComment {
                id: "c1".into(),
                author: "bo".into(),
                body: format!("Log: {long} here"),
                url: format!("{pr_url}#issuecomment-1"),
                ..Default::default()
            }],
            threads: Ok(Default::default()),
            stamp: Default::default(),
        };
        let area = Rect::new(2, 1, 34, 20);
        let draw_with = |scroll, titled| {
            let title = vec![Span::raw(" GitHub "), Span::raw("PR #7"), Span::raw(" ")];
            let title = panel_head(title, pr_url, titled);
            let mut buf = Buffer::empty(Rect::new(0, 0, 40, 22));
            draw_panel(
                &mut buf,
                area,
                title,
                &Line::from(""),
                " p open ",
                pr_lines(&pr, |_| false),
                scroll,
            );
            buf
        };
        let draw = |scroll| draw_with(scroll, true);
        // Each cell's link, and its text without the escapes.
        let cell = |buf: &Buffer, x: u16, y: u16| {
            let s = buf[(x, y)].symbol();
            match s.strip_prefix("\x1b]8;") {
                Some(rest) => {
                    let (params, rest) = rest.split_once(';').unwrap();
                    assert!(params.starts_with("id=taskman-"), "{params}");
                    let (url, rest) = rest.split_once('\x07').unwrap();
                    (Some(url.to_string()), rest.strip_suffix("\x1b]8;;\x07").unwrap().to_string())
                }
                None => (None, s.to_string()),
            }
        };
        let row = |buf: &Buffer, y: u16| {
            let cells: Vec<_> = (0..40).map(|x| cell(buf, x, y)).collect();
            let text: String = cells.iter().map(|(_, t)| t.as_str()).collect();
            let linked: String =
                cells.iter().map(|(u, t)| if u.is_some() { t.as_str() } else { "" }).collect();
            let urls: Vec<String> = cells.into_iter().filter_map(|(u, _)| u).collect();
            (text.trim_end().to_string(), linked, urls)
        };
        let buf = draw(0);
        // The title line links, so the border doesn't.
        let (text, linked, _) = row(&buf, 1);
        assert!(text.starts_with("  ╭ GitHub PR #7 "), "{text}");
        assert_eq!(linked, "");
        // Without one (a tracker panel loading its issue), only the key or number on it links.
        let (text, linked, urls) = row(&draw_with(0, false), 1);
        assert!(text.starts_with("  ╭ GitHub PR #7 "), "{text}");
        assert_eq!(linked, "PR #7");
        assert!(urls.iter().all(|u| u == pr_url));
        // Inside the border and a column of padding.
        let (text, linked, _) = row(&buf, 2);
        assert_eq!(
            (text.as_str(), linked.as_str()),
            ("  │ Fix login                      │", "Fix login")
        );
        assert_eq!(
            buf[(4, 2)].diff_option,
            ratatui::buffer::CellDiffOption::ForcedWidth(std::num::NonZeroU16::MIN)
        );
        let (_, linked, _) = row(&buf, 3);
        assert_eq!(linked, "", "head → base isn't a link");
        let find =
            |buf: &Buffer, needle: &str| (0..22).find(|&y| row(buf, y).0.contains(needle)).unwrap();
        let y = find(&buf, "CI / lint");
        let (_, linked, urls) = row(&buf, y);
        assert_eq!((linked.as_str(), urls[0].as_str()), ("CI / lint", run));
        // The author opens the comment; the URL in its text, wrapped over two rows, opens itself.
        let y = find(&buf, "Log: ");
        assert_eq!(row(&buf, y - 1).1, "bo");
        let (first, second) = (row(&buf, y + 1), row(&buf, y + 2));
        assert_eq!(
            (first.0.as_str(), second.0.as_str()),
            ("  │ https://logs.example/acme/web/ │", "  │ runs/123456789/attempts/1 here │")
        );
        assert_eq!(format!("{}{}", first.1, second.1), long);
        assert!(first.2.iter().chain(&second.2).all(|u| u == long));
        // Scrolled, the links move with their text.
        let scrolled = draw(2);
        let y2 = find(&scrolled, "CI / lint");
        assert_eq!(y2 + 2, find(&buf, "CI / lint"));
        assert_eq!(row(&scrolled, y2).1, "CI / lint");
        assert_eq!(row(&scrolled, 2).1, "", "the title's row scrolled away");
    }
}
