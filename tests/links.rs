//! Links a session prints reach the terminal taskman runs in: the cells showing an http(s) link are
//! drawn inside OSC 8, so the terminal opens them on a click, and `prefix l` lists them and opens
//! one through `$BROWSER` (a stub here, so no browser window opens). A `file:` link isn't passed
//! on, nor opened. A plain URL in the session's text is listed too. So is a URL in the ticket's
//! description, which the tracker panel passes on the same way, listed first.

mod common;

use common::{Tui, find, scratch_dir, script, taskman};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const URL: &str = "https://example.com/artifact/abc123";
const SPEC: &str = "https://example.com/spec/eng-1";
const LIMIT: Duration = Duration::from_secs(60);

#[test]
fn links_reach_the_terminal_and_the_list_opens_them() {
    let dir = scratch_dir("links");
    let opened = dir.join("opened.log");
    let mut tui = Tui::start(
        "links_reach_the_terminal_and_the_list_opens_them",
        &dir,
        start(&dir, &opened),
        LIMIT,
    );
    let screen = |tui: &Tui| tui.with(|t| t.screen().clone());

    // Waiting for the session's three lines on screen.
    let (col, row) = tui.wait_for("the session's links", |s| {
        find(s, "/etc/hosts")?;
        find(s, "example.com/plain")?;
        find(s, "the artifact")
    });
    let s = screen(&tui);
    let link = s.cell_hyperlink(row, col + 4).expect("the artifact is linked");
    assert_eq!(link.uri(), URL);
    assert_eq!(link.id(), Some("azmyb0"));
    assert_eq!(s.cell_hyperlink(row, col - 1), None);
    // Sent once around the link's text, not around each of its cells.
    let raw = tui.with(|t| String::from_utf8_lossy(&t.raw).into_owned());
    let open = format!("\x1b]8;id=azmyb0;{URL}\x07");
    let runs: Vec<&str> =
        raw.split(&open).skip(1).map(|s| s.split("\x1b]8;;\x07").next().unwrap()).collect();
    assert!(!runs.is_empty() && runs.iter().all(|r| strip(r) == "the artifact"), "{runs:?}");
    let (col, row) = find(&s, "/etc/hosts").expect("the file link is on screen");
    assert_eq!(s.cell_hyperlink(row, col), None, "a file: link was passed on");

    // Waiting for the description's link in the tracker panel.
    let (col, row) = tui.wait_text(&format!("Spec: {SPEC}"));
    let s = screen(&tui);
    for c in col + 6..col + 6 + u16::try_from(SPEC.len()).unwrap() {
        let link = s.cell_hyperlink(row, c).expect("the description's URL is linked");
        assert_eq!(link.uri(), SPEC);
        assert!(link.id().is_some_and(|id| id.starts_with("taskman-")));
    }
    assert_eq!(s.cell_hyperlink(row, col), None, "Spec: isn't part of the link");

    // Listing the links.
    tui.send(b"\x01l");
    // Both groups' headings, and the file link listed as not opening, all in one drawing: the
    // dialog may reach the terminal over several reads.
    tui.wait_for("the list of links", |s| {
        find(s, "This ticket")?;
        find(s, "doesn't open")?;
        find(s, "The session, newest first")
    });
    // The ticket's links come first.
    tui.send(b"\r");
    tui.wait_text("Opened https://example.com/spec");
    wait_opened(&tui, &opened, 1);

    // Opening the session's links. Newest first: the plain URL, found in the text.
    tui.send(b"\x01l");
    tui.wait_text("The session, newest first");
    tui.send(b"\x1b[B\r");
    tui.wait_text("Opened https://example.com/plain");
    wait_opened(&tui, &opened, 2);
    tui.send(b"\x01l");
    tui.wait_text("The session, newest first");
    // The file link doesn't open.
    tui.send(b"\x1b[B\x1b[B\r");
    tui.wait_text("Only http(s) links open");
    tui.send(b"\x1b[B\r");
    tui.wait_text("Opened https://example.com/artifact");
    assert_eq!(wait_opened(&tui, &opened, 3), [SPEC, "https://example.com/plain?x=1", URL]);
    tui.quit();
}

/// taskman in a pty with one ticket, whose session prints two links and sleeps; `$BROWSER` notes
/// the URL it's given in `opened`.
fn start(dir: &Path, opened: &Path) -> Command {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    std::fs::write(repo.join("workplan/todo/ENG-1-test.md"), format!("# test\n\nSpec: {SPEC}\n"))
        .unwrap();
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [{"key": "ENG-1", "repo": repo, "cwd": repo}],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    // The bytes Claude Code 2.1.296 prints for a Markdown link, and a file link.
    let cli = format!(
        "#!/bin/sh\necho $$ > '{}/ENG-1.pid'\n\
         printf 'Here is \\033]8;id=azmyb0;{URL}\\a\\033[94mthe artifact\\033[22G\\033]8;;\\a\\033[39mand more\\r\\n'\n\
         printf 'See \\033]8;;file:///etc/hosts\\a/etc/hosts\\033]8;;\\a\\r\\n'\n\
         printf 'And (https://example.com/plain?x=1).\\r\\n'\n\
         exec sleep 300\n",
        dir.display()
    );
    let browser = format!("#!/bin/sh\necho \"$1\" >> '{}'\n", opened.display());
    let mut cmd = taskman(dir);
    cmd.env("TASKMAN_CLAUDE", script(dir, "claude", &cli))
        .env("BROWSER", script(dir, "browser", &browser));
    cmd
}

/// The URLs `$BROWSER` was given, once it has been given `n`.
fn wait_opened(tui: &Tui, opened: &Path, n: usize) -> Vec<String> {
    let urls = || -> Vec<String> {
        std::fs::read_to_string(opened).unwrap_or_default().lines().map(Into::into).collect()
    };
    tui.wait_until(&format!("$BROWSER wasn't given {n} URLs"), || urls().len() >= n);
    urls()
}

/// `s` without its escapes: CSI sequences, and OSCs ended by BEL.
fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('\x1b') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = if rest.starts_with("\x1b]") {
            rest.find('\x07')
        } else {
            rest[1..].find(|c: char| c.is_ascii_alphabetic()).map(|e| e + 1)
        };
        rest = &rest[end.map_or(rest.len(), |e| e + 1)..];
    }
    out + rest
}
