//! `taskman add` checking a key in the tracker: a key the tracker says doesn't exist is refused,
//! and one it can't be asked about is added unchecked. A session's `--new` waits for the user to
//! approve the ticket in taskman, and creates nothing when they decline.

mod common;

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

fn add(dir: &Path, repo: &Path, key: &str) -> Output {
    common::taskman(dir)
        .args(["add", key, "--no-worktree"])
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// The requests left in the inbox for taskman, which isn't running.
fn inbox(dir: &Path) -> String {
    std::fs::read_dir(dir.join("state/taskman/inbox"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect()
}

#[test]
fn adds_a_key_unchecked_when_the_token_command_is_missing() {
    let dir = common::scratch_dir("add-token-missing");
    let repo = common::project(&dir, &[]);
    let saved = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "linear", "site": "acme", "key": "ENG"}}],
        "tickets": [],
    });
    std::fs::write(dir.join("state/taskman/tickets.json"), saved.to_string()).unwrap();
    // sh says "...: taskman-test-no-such-command: not found", which once read as the ticket's.
    let config = serde_json::json!({ "accounts": [
        { "id": "linear-1", "provider": "linear", "site": "acme", "token": "cmd:taskman-test-no-such-command" },
    ]});
    std::fs::write(dir.join("config.json"), config.to_string()).unwrap();

    let out = add(&dir, &repo, "ENG-52");
    let (stdout, stderr) =
        (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(
        stderr.starts_with(
            "taskman add: couldn't check the ticket in the tracker, adding it anyway: the token command failed: "
        ) && stderr.contains("not found"),
        "{stderr}"
    );
    assert_eq!(stdout, "taskman isn't running; ENG-52 is added when it starts.\n");
    assert!(inbox(&dir).contains("\"ENG-52\""), "{}", inbox(&dir));
}

#[test]
fn refuses_a_key_the_tracker_doesnt_have() {
    let dir = common::scratch_dir("add-not-found");
    let repo = common::project(&dir, &[]);
    std::fs::write(repo.join("workplan/todo/ENG-1-login.md"), "# Fix the login\n").unwrap();

    let out = add(&dir, &repo, "ENG-9");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .starts_with("taskman add: ENG-9 not found: no ENG-9-….md in "),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(inbox(&dir).is_empty(), "{}", inbox(&dir));

    let out = add(&dir, &repo, "ENG-1");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(inbox(&dir).contains("\"ENG-1\""), "{}", inbox(&dir));
}

/// The tickets on the project's Local files board.
fn board(repo: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(repo.join("workplan/todo"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    files
}

/// `taskman add --new` as the lead agent's session runs it, started without waiting for it.
fn session_creates(dir: &Path, repo: &Path, title: &str) -> std::process::Child {
    common::taskman(dir)
        .args(["add", "--new", title, "--description", "API_KEY=sk-mock-123", "--no-worktree"])
        .current_dir(repo)
        .env("TASKMAN_REPO", repo)
        .env("TASKMAN_TICKET", "acme-lead")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn a_sessions_new_ticket_waits_for_the_user() {
    let dir = common::scratch_dir("add-new-asks");
    let repo = common::project(&dir, &[]);
    let mut tui = common::Tui::start(
        "a_sessions_new_ticket_waits_for_the_user",
        &dir,
        common::stubbed(&dir),
        Duration::from_secs(60),
    );
    let alive = dir.join("state/taskman/inbox/alive");
    tui.wait_until("taskman's inbox heartbeat", || alive.exists());
    // The dialog ignores keys for its first 1.5 s, so a key sent sooner is lost.
    let armed = |tui: &common::Tui, shown: Instant| {
        tui.wait_until("the dialog to take keys", || shown.elapsed() > Duration::from_millis(1700));
    };

    tui.step("declining a ticket");
    let declined = session_creates(&dir, &repo, "Build log");
    tui.wait_text("acme-lead asks to create a ticket in Local files");
    let shown = Instant::now();
    tui.wait_text("API_KEY=sk-mock-123");
    assert!(board(&repo).is_empty(), "created before the user answered: {:?}", board(&repo));
    armed(&tui, shown);
    tui.send(b"\x1b");
    let out = declined.wait_with_output().unwrap();
    let said = |out: &Output| {
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    };
    // A command that found taskman's heartbeat stale (a very slow machine) queues instead.
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "taskman add: the user declined creating \"Build log\" in taskman; nothing was created\n"
    );
    tui.wait_text("Declined creating \"Build log\" from acme-lead");
    assert!(board(&repo).is_empty(), "{:?}", board(&repo));

    tui.step("approving one");
    let approved = session_creates(&dir, &repo, "Export to CSV");
    tui.wait_text("acme-lead asks to create a ticket in Local files");
    let shown = Instant::now();
    tui.wait_text("Export to CSV");
    assert!(board(&repo).is_empty(), "{:?}", board(&repo));
    armed(&tui, shown);
    tui.send(b"\r");
    let out = approved.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(board(&repo), ["ENG-1-export-to-csv.md"], "{}", said(&out));
    assert!(stdout.starts_with("Created ENG-1: "), "{stdout}");
    assert!(stdout.contains("\nAdded ENG-1 \"Export to CSV\" to acme"), "{stdout}");
    let file = std::fs::read_to_string(repo.join("workplan/todo/ENG-1-export-to-csv.md")).unwrap();
    assert!(file.contains("API_KEY=sk-mock-123"), "{file}");
    tui.quit();
}
