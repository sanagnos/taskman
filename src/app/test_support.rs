//! What the App's tests and the screens' snapshots (`ui::screens`) share: an App built from saved
//! state alone, with nothing read or started (no tokens, no PR panels, no session, no terminal),
//! a mock project to fill it with, fake sessions, and drawing it into a `TestBackend`.
//!
//! None of it reaches outside the test: unit tests keep their state folder and config file in a
//! scratch folder of their own (`store::scratch`), sessions are `Session::fake`, copying goes out
//! as OSC 52 (`clipboard_out`) rather than through the desktop's clipboard commands, and no URL
//! opens in a browser (`browser` records them).

use super::{AgentState, App};
use crate::github::{self, Check, CheckState, Pr, PrComment, Review, Thread, Threads};
use crate::session::{
    Session,
    fake::{Release, Sent},
};
use crate::store::{self, Cli, Saved, Ticket};
use crate::tracker::{Comment, Issue};
use crate::ui;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub(crate) fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

pub(crate) fn offline(saved: Saved) -> App {
    let mut app = App::from_saved(
        Path::new("/opt/taskman"),
        saved,
        store::Config::default(),
        crate::update::Updater::disabled(),
    );
    app.clipboard = Arc::new(|_| crate::selection::Copied::Osc52);
    app.stop_background = Arc::new(|_| Err("no claude in tests".into()));
    app.open_url = Arc::new(|_| false);
    // As if the keychain check had found none; a test of the wait before it sets this back.
    app.keychain_probed = true;
    app
}

pub(crate) fn ticket() -> Ticket {
    serde_json::from_value(
        serde_json::json!({ "key": "ENG-52", "repo": "/src/acme", "cwd": "/src/acme" }),
    )
    .unwrap()
}

/// `secs` ago, for the times a test pretends have passed.
pub(crate) fn ago(secs: f32) -> Option<Instant> {
    Instant::now().checked_sub(Duration::from_secs_f32(secs))
}

/// `secs` ago as the trackers and GitHub write times. Ages show in buckets ("3h"), so a time in
/// the middle of one draws the same however long the test takes.
pub(crate) fn iso(secs: i64) -> String {
    let t = crate::util::now() - secs;
    let (days, s) = (t.div_euclid(86400), t.rem_euclid(86400));
    // Days since 1970 to a civil date (the inverse of `util::days_from_civil`).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

pub(crate) fn agent(name: &str, cli: Cli, role: Option<&str>) -> Ticket {
    serde_json::from_value(serde_json::json!({
        "key": format!("acme-{name}"), "repo": "/src/acme", "cwd": "/src/acme", "agent": name, "cli": cli, "role": role,
    }))
    .unwrap()
}

pub(crate) const MIN: i64 = 60;
pub(crate) const HOUR: i64 = 3600;

/// A mock world: project acme on Linear with two agents and four tickets (ENG-53 stacked on
/// ENG-52, ENG-61 on Codex), and project notes, folded, with no tracker. ENG-52 is selected.
pub(crate) fn acme() -> Saved {
    let t = |key: &str, extra: serde_json::Value| {
        let mut v = serde_json::json!({
            "key": key, "repo": "/src/acme", "cwd": format!("/src/acme-{key}"),
            "branch": format!("{}-work", key.to_lowercase()), "created_worktree": true,
            "seen_jira": [], "seen_gh": [],
        });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    };
    serde_json::from_value(serde_json::json!({
        "projects": [
            { "name": "acme", "repo": "/src/acme", "tracker": { "provider": "linear", "site": "acme", "key": "ENG" } },
            { "name": "notes", "repo": "/src/notes", "collapsed": true },
        ],
        "tickets": [
            { "key": "acme-lead", "repo": "/src/acme", "cwd": "/src/acme", "agent": "lead",
              "role": "Coordinate the tickets" },
            { "key": "acme-release-notes-reviewer", "repo": "/src/acme", "cwd": "/src/acme",
              "agent": "release-notes-reviewer" },
            t("ENG-52", serde_json::json!({ "branch": "eng-52-login", "seen_jira": ["c1"], "seen_gh": ["r:r1"] })),
            t("ENG-53", serde_json::json!({ "base": { "key": "ENG-52", "branch": "eng-52-login" } })),
            t("ENG-61", serde_json::json!({ "cli": "codex" })),
            t("ENG-70", serde_json::json!({})),
            { "key": "notes-writer", "repo": "/src/notes", "cwd": "/src/notes", "agent": "writer" },
        ],
        "selected": 2,
    }))
    .unwrap()
}

/// `acme()` as the TUI holds it a while after starting: each session in a state, the panels
/// fetched, GitHub ready in acme. ENG-52's issue has a comment and its PR a review the user
/// hasn't seen.
pub(crate) fn acme_app() -> App {
    let mut app = offline(acme());
    app.github.insert("/src/acme".into(), github::Gh::Ready);
    let account = serde_json::json!({ "id": "linear-1", "provider": "linear", "site": "acme", "token": "env:ACME_LINEAR_TOKEN" });
    app.config.accounts.push(serde_json::from_value(account).unwrap());
    app.trackers.token_read(
        "env:ACME_LINEAR_TOKEN",
        Ok("lin_mock".into()),
        &app.config,
        &app.projects,
    );
    let states = [
        ("acme-lead", AgentState::Done, false),
        ("acme-release-notes-reviewer", AgentState::Idle, true),
        ("ENG-52", AgentState::Done, true),
        ("ENG-53", AgentState::Needs, false),
        ("ENG-61", AgentState::Working, false),
        ("ENG-70", AgentState::Exited, false),
        ("notes-writer", AgentState::Needs, false),
    ];
    for (key, state, seen) in states {
        let tv = app.tickets.iter_mut().find(|tv| tv.t.key == key).unwrap();
        tv.status.state = state;
        tv.status.seen = seen;
    }
    for tv in &mut app.tickets {
        if tv.t.agent.is_some() {
            continue;
        }
        let n: u64 = tv.t.key[4..].parse().unwrap();
        let mut found = issue(&tv.t.key);
        let mut found_pr = pr(n + 76, &tv.t.branch.clone().unwrap_or_default());
        match tv.t.key.as_str() {
            "ENG-53" => {
                found_pr.is_draft = true;
                found_pr.base = "eng-52-login".into();
                found_pr.checks.truncate(1);
                found_pr.checks[0].state = CheckState::Pending;
                found_pr.reviews.clear();
                found_pr.review_decision.clear();
            }
            "ENG-61" => found.comments.clear(),
            "ENG-70" => {
                tv.pr.finish(Ok(None));
                tv.jira.finish(Ok(found));
                continue;
            }
            _ => {}
        }
        tv.jira.finish(Ok(found));
        tv.pr.finish(Ok(Some(found_pr)));
        tv.jira.fetched_at = ago(150.0);
        tv.pr.fetched_at = ago(30.0);
    }
    app
}

/// `acme_app` with `n` more tickets in acme, ENG-71 on, their issues and PRs fetched: a sidebar
/// longer than the screen. The last one needs you; the others haven't started.
pub(crate) fn long_sidebar(n: usize) -> App {
    let mut app = acme_app();
    let summaries = [
        "Export the audit log",
        "Dark mode for the dashboard",
        "Retry failed webhooks",
        "Archive projects by age",
    ];
    for k in 0..n {
        let key = format!("ENG-{}", 71 + k);
        let t: Ticket = serde_json::from_value(serde_json::json!({
            "key": key, "repo": "/src/acme", "cwd": format!("/src/acme-{key}"),
            "branch": format!("{}-work", key.to_lowercase()), "created_worktree": true,
            "seen_jira": [], "seen_gh": [],
        }))
        .unwrap();
        let mut tv = super::TicketView::new(t);
        let mut found = issue(&key);
        found.summary = summaries[k % summaries.len()].into();
        found.comments.clear();
        tv.jira.finish(Ok(found));
        tv.pr.finish(Ok(Some(pr(147 + k as u64, &format!("{}-work", key.to_lowercase())))));
        tv.status.seen = true;
        if k + 1 == n {
            tv.status.state = AgentState::Needs;
            tv.status.seen = false;
        }
        app.tickets.push(tv);
    }
    app
}

/// `acme_app` with `n` projects in all: acme, notes, then acme-02 and on, with no tracker.
pub(crate) fn many_projects(n: usize) -> App {
    let mut app = acme_app();
    for k in app.projects.len()..n {
        let name = format!("acme-{k:02}");
        let project = serde_json::json!({ "name": name, "repo": format!("/src/{name}") });
        app.projects.push(serde_json::from_value(project).unwrap());
    }
    app
}

pub(crate) fn issue(key: &str) -> Issue {
    let comment = |id: &str, author: &str, secs: i64, body: &str| Comment {
        id: id.into(),
        author: author.into(),
        created: iso(secs),
        body: body.into(),
    };
    let summary = match key {
        "ENG-52" => "Sign in with a magic link",
        "ENG-53" => "Single sign-on for teams",
        "ENG-61" => "Rate-limit the export endpoint",
        _ => "Remove the legacy importer",
    };
    Issue {
        summary: summary.into(),
        status: "In Progress".into(),
        status_category: "indeterminate".into(),
        issue_type: "Feature".into(),
        priority: "High".into(),
        assignee: "Dana Reyes".into(),
        description: "Users get a link by email and are signed in when they open it.\n\nThe link expires after 15 minutes and works once.".into(),
        comments: vec![
            comment("c2", "Sam Okafor", 3 * HOUR + 30 * MIN, "Can the link carry the page the user came from?"),
            comment("c1", "Dana Reyes", 2 * 24 * HOUR + 5 * HOUR, "Design is in the spec."),
        ],
        comments_left_out: 3,
        branch: String::new(),
        prs: Vec::new(),
    }
}

pub(crate) fn pr(number: u64, branch: &str) -> Pr {
    let now = crate::util::now();
    let check = |name: &str, state, secs: i64| Check {
        name: name.into(),
        state,
        when: Some(now - secs),
        url: format!("https://github.com/acme/acme/actions/runs/{number}"),
    };
    let thread = |path: &str, line, resolved, body: &str| Thread {
        path: path.into(),
        line: Some(line),
        resolved,
        outdated: false,
        comments: vec![PrComment {
            id: format!("t{line}"),
            author: "sam-okafor".into(),
            when: iso(HOUR + 30 * MIN),
            body: body.into(),
            url: String::new(),
        }],
        left_out: 0,
    };
    Pr {
        number,
        title: "Sign in with a magic link".into(),
        url: format!("https://github.com/acme/acme/pull/{number}"),
        state: "OPEN".into(),
        is_draft: false,
        review_decision: "CHANGES_REQUESTED".into(),
        mergeable: "MERGEABLE".into(),
        head: branch.into(),
        cross_repository: false,
        head_oid: "0123456789abcdef".into(),
        base: "main".into(),
        checks: vec![
            check("test (ubuntu)", CheckState::Pass, 40 * MIN + 30),
            check("test (macos)", CheckState::Fail, 35 * MIN + 30),
            check("clippy", CheckState::Pass, 45 * MIN + 30),
        ],
        reviews: vec![
            Review {
                id: "r1".into(),
                author: "dana-reyes".into(),
                state: "COMMENTED".into(),
                when: iso(5 * HOUR + 30 * MIN),
                body: "Looks close.".into(),
                url: String::new(),
            },
            Review {
                id: "r2".into(),
                author: "sam-okafor".into(),
                state: "CHANGES_REQUESTED".into(),
                when: iso(2 * HOUR + 30 * MIN),
                body: "The link must expire.".into(),
                url: String::new(),
            },
        ],
        comments: vec![PrComment {
            id: "p1".into(),
            author: "dana-reyes".into(),
            when: iso(50 * MIN + 30),
            body: "Rebased on main.".into(),
            url: String::new(),
        }],
        threads: Ok(Threads {
            list: vec![
                thread("src/auth/link.rs", 42, false, "Use a constant-time compare here."),
                thread("src/auth/mail.rs", 7, true, "Typo in the subject."),
            ],
            total: 2,
            review_urls: Vec::new(),
        }),
    }
}

/// Lays the App out on a screen `w` by `h`, as `tick` does without its other work.
pub(crate) fn lay_out(app: &mut App, w: u16, h: u16) {
    app.lay_out(Rect::new(0, 0, w, h));
}

/// Gives ticket `key` a session with no process, the size of the Claude pane, which shows
/// `screen`; returns what's sent to it.
pub(crate) fn attach(app: &mut App, key: &str, screen: &str) -> Sent {
    let a = app.areas.claude;
    let (session, sent) = Session::fake(a.height, a.width);
    session.output(screen.replace('\n', "\r\n").as_bytes());
    let tv = app.tickets.iter_mut().find(|tv| tv.t.key == key).unwrap();
    tv.session = Some(session);
    sent
}

/// Gives ticket `key` a session that printed `screen` and exited with `code`, and lets the poll
/// notice.
pub(crate) fn attach_ended(app: &mut App, key: &str, screen: &str, code: u32) {
    attach(app, key, screen);
    let tv = app.tickets.iter_mut().find(|tv| tv.t.key == key).unwrap();
    tv.session.as_mut().unwrap().exit(code);
    tv.launch = Some("launch-1".into());
    app.poll_status();
}

/// Gives ticket `key` a session that printed `screen` before its reader panicked, and lets the
/// poll notice.
pub(crate) fn attach_crashed(app: &mut App, key: &str, screen: &str) {
    attach(app, key, screen);
    let tv = app.tickets.iter_mut().find(|tv| tv.t.key == key).unwrap();
    tv.session.as_ref().unwrap().crash();
    tv.launch = Some("launch-1".into());
    app.poll_status();
}

/// As `attach`, with a CLI that goes on after the session is stopped until the `Release` is
/// dropped: what a CLI slow to quit does, with the session's reaper running meanwhile.
pub(crate) fn attach_slow(app: &mut App, key: &str) -> (Sent, Release) {
    let a = app.areas.claude;
    let (session, sent, release) = Session::fake_slow(a.height, a.width);
    let tv = app.tickets.iter_mut().find(|tv| tv.t.key == key).unwrap();
    tv.session = Some(session);
    (sent, release)
}

/// Waits until every stopped session's CLI is reaped: the fakes' reapers end at once, but on a
/// thread, and a test that checks what follows the reap can't race it.
pub(crate) fn reaped(app: &mut App) {
    for reaper in app.tickets.iter_mut().filter_map(|tv| tv.reaper.take()) {
        reaper.wait();
    }
    for (_, reaper) in app.reapers.drain(..) {
        reaper.wait();
    }
}

/// Draws the App on a screen `w` by `h` and returns it as text, one quoted line per row. The
/// spinner shows its first frame and the version 0.0.0, so neither changes a snapshot, and links
/// (OSC 8, see `links::mark`) are left out to keep it readable.
/// A worktree removal for tickets `names` that runs until the sender returned is dropped.
pub(crate) fn removing(app: &mut App, names: &[&str]) -> std::sync::mpsc::Sender<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || while rx.recv().is_ok() {});
    app.removing.push((names.iter().map(|n| n.to_string()).collect(), thread));
    tx
}

pub(crate) fn draw(app: &mut App, w: u16, h: u16) -> String {
    lay_out(app, w, h);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::draw(f, app)).unwrap();
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let fixed = format!("{:<1$}", "v0.0.0", version.len());
    without_links(&term.backend().to_string())
        .replace(&version, &fixed)
        .chars()
        .map(|c| if ui::SPINNER.iter().any(|f| f.starts_with(c)) { '⠋' } else { c })
        .collect()
}

/// As `draw`, followed by the screen's colours: a letter a cell for each pair of foreground and
/// background (`.` for the terminal's own), and what each letter is. A version longer than 0.0.0
/// takes more cells than `draw`'s `v0.0.0`; those show as `.`, so the map doesn't change with it.
pub(crate) fn draw_colours(app: &mut App, w: u16, h: u16) -> String {
    let text = draw(app, w, h);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::draw(f, app)).unwrap();
    let buf = term.backend().buffer();
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let mut extra = std::collections::HashSet::new();
    for y in 0..h {
        let row: Vec<&str> = (0..w).map(|x| buf[(x, y)].symbol()).collect();
        // Cells, not bytes: the box drawing before the version takes several bytes a cell.
        let found = row.windows(version.len()).position(|cells| cells.concat() == version);
        if let Some(start) = found {
            extra.extend((start + "v0.0.0".len()..start + version.len()).map(|x| (x, y)));
        }
    }
    let letter = |i: usize| char::from(b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"[i]);
    let mut pairs = Vec::new();
    let mut map = String::new();
    for y in 0..h {
        map.push('"');
        for x in 0..w {
            let cell = &buf[(x, y)];
            let pair = (cell.fg, cell.bg);
            if pair == (ratatui::style::Color::Reset, ratatui::style::Color::Reset)
                || extra.contains(&(usize::from(x), y))
            {
                map.push('.');
                continue;
            }
            let i = pairs.iter().position(|p| *p == pair).unwrap_or_else(|| {
                pairs.push(pair);
                pairs.len() - 1
            });
            map.push(letter(i));
        }
        map.push_str("\"\n");
    }
    for (i, (fg, bg)) in pairs.iter().enumerate() {
        map.push_str(&format!("{}: {fg:?} on {bg:?}\n", letter(i)));
    }
    format!("{text}\n{map}")
}

fn without_links(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("\x1b]8;") {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('\x07').expect("an OSC 8 sequence ends with BEL");
        rest = &rest[start + end + 1..];
    }
    out + rest
}

/// Waits for the next message a thread of the App sends, such as `Msg::Copied`, and applies it.
pub(crate) fn settle(app: &mut App) {
    let msg =
        app.rx.recv_timeout(Duration::from_secs(10)).expect("no message from the App's threads");
    app.tx.send(msg).unwrap();
    app.drain_messages();
}

/// Answers the open form's connection test with `found` as if it had run, without running it: no
/// keychain is asked and no tracker reached. For screens; `events` drives the real check.
pub(crate) fn answer_check(app: &mut App, found: Result<(&str, &str), String>) {
    let found = found.map(|(n, w)| (n.to_string(), w.to_string()));
    let form = match &mut app.mode {
        super::Mode::Settings(s) => &mut s.form,
        super::Mode::Form(f) => f,
        _ => panic!("no form open"),
    };
    form.checking = Some(u64::MAX);
    app.tx.send(super::sync::Msg::Checked(u64::MAX, super::forms::Check::Account(found))).unwrap();
    app.drain_messages();
}

/// Waits for the open project form's listing of what its account can see, and answers it with
/// `got` (key and name of each) as the tracker would; the listing's own thread reaches no tracker.
pub(crate) fn answer_choices(app: &mut App, got: Result<&[(&str, &str)], &str>) {
    let got = got
        .map(|l| {
            l.iter()
                .map(|(k, n)| crate::tracker::Choice { key: k.to_string(), name: n.to_string() })
                .collect()
        })
        .map_err(String::from);
    let mut before = Vec::new();
    loop {
        let msg = app.rx.recv_timeout(Duration::from_secs(10)).expect("nothing listed");
        if let super::sync::Msg::Choices(id, _) = msg {
            before.into_iter().for_each(|m| app.tx.send(m).unwrap());
            app.tx.send(super::sync::Msg::Choices(id, got)).unwrap();
            app.drain_messages();
            return;
        }
        before.push(msg);
    }
}

/// The toast showing, if any.
pub(crate) fn toast(app: &App) -> Option<&str> {
    app.toast.as_ref().map(|(t, _)| t.as_str())
}

/// The URLs the App opened, in order.
pub(crate) type Opened = Arc<std::sync::Mutex<Vec<String>>>;

/// Gives the App a browser that opens nothing and notes each URL it's given; returns them. Without
/// it, an App from `offline` finds no browser.
pub(crate) fn browser(app: &mut App) -> Opened {
    let opened = Opened::default();
    let notes = opened.clone();
    app.open_url = Arc::new(move |url| {
        notes.lock().unwrap().push(url.into());
        true
    });
    opened
}

/// What a fake session was sent, as text.
pub(crate) fn sent(s: &Sent) -> String {
    String::from_utf8_lossy(&s.bytes()).into_owned()
}

#[test]
fn writes_times_the_way_trackers_do() {
    for secs in [0, 59, 3 * HOUR + 30 * MIN, 400 * 24 * HOUR] {
        let back = crate::util::parse_ts(&iso(secs)).unwrap();
        assert!((crate::util::now() - secs - back).abs() <= 1, "{}", iso(secs));
    }
    assert_eq!(crate::util::ago_str(&iso(3 * HOUR + 30 * MIN)), "3h");
}
