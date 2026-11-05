//! `taskman title` against the binary: set from a session on a running taskman, the card shows it
//! and it's saved; asked while taskman is closed, it's set when it starts. A session reaches only
//! its own ticket, `taskman status` and the brief never carry it, and a taskman too old to know the
//! request is told apart from one that doesn't answer.

mod common;

use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

/// `taskman title ARGS` in `session`'s environment as taskman sets it, in `repo`; no session
/// outside one.
fn run(dir: &Path, repo: &Path, session: Option<&str>, args: &[&str]) -> (bool, String, String) {
    let mut cmd = common::taskman(dir);
    if let Some(s) = session {
        cmd.env("TASKMAN_TICKET", s).env("TASKMAN_REPO", repo);
    }
    let out: Output =
        cmd.arg("title").args(args).current_dir(repo).stdin(Stdio::null()).output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.success(), text(&out.stdout), text(&out.stderr))
}

/// The title saved for `key`.
fn saved_label(dir: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("state/taskman/tickets.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    let tickets = saved["tickets"].as_array().unwrap();
    let t = tickets.iter().find(|t| t["key"] == key).unwrap();
    t["label"].as_str().map(String::from)
}

#[test]
fn sets_a_title_on_a_running_taskman() {
    let dir = common::scratch_dir("title-running");
    let repo = common::project(&dir, &["ENG-1", "ENG-2"]);
    let mut tui = common::Tui::start(
        "sets_a_title_on_a_running_taskman",
        &dir,
        common::stubbed(&dir),
        Duration::from_secs(60),
    );
    let alive = dir.join("state/taskman/inbox/alive");
    tui.wait_until("taskman's inbox heartbeat", || alive.exists());

    tui.step("a session titles its own ticket");
    let (ok, out, err) = run(&dir, &repo, Some("ENG-1"), &["Fix the\x1b[31m redirect"]);
    assert!(ok, "{out}\n{err}");
    // A command that found the heartbeat stale (a very slow machine) queues instead.
    assert!(out.contains("title"), "{out}");
    tui.wait_text("Fix the [31m redirect");
    tui.wait_until("the title to be saved", || {
        saved_label(&dir, "ENG-1").as_deref() == Some("Fix the [31m redirect")
    });

    tui.step("not another ticket");
    let (ok, _, err) = run(&dir, &repo, Some("ENG-1"), &["ENG-2", "Other"]);
    assert!(!ok);
    assert_eq!(
        err,
        "taskman title: that's ENG-2, and a session sets only its own ticket's title\n"
    );
    assert_eq!(saved_label(&dir, "ENG-2"), None);

    tui.step("out of status");
    let out = common::taskman(&dir)
        .args(["status", "ENG-1"])
        .current_dir(&repo)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&out.stdout);
    assert!(status.contains("ENG-1 test") && !status.contains("redirect"), "{status}");
    let out = common::taskman(&dir)
        .args(["status", "ENG-1", "--json"])
        .current_dir(&repo)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(json.contains("ENG-1 test") && !json.contains("redirect"), "{json}");

    tui.step("a title that starts with a dash, after --");
    let (ok, out, err) = run(&dir, &repo, Some("ENG-1"), &["--", "--clear"]);
    assert!(ok, "{out}\n{err}");
    tui.wait_until("the text to be saved", || {
        saved_label(&dir, "ENG-1").as_deref() == Some("--clear")
    });

    tui.step("a shell clears it");
    let (ok, out, err) = run(&dir, &repo, None, &["--clear", "ENG-1"]);
    assert!(ok, "{out}\n{err}");
    tui.wait_until("the title to be dropped", || saved_label(&dir, "ENG-1").is_none());
    tui.quit();
}

#[test]
fn a_title_waits_for_taskman_to_start() {
    let dir = common::scratch_dir("title-queued");
    let repo = common::project(&dir, &["ENG-1"]);
    let (ok, out, err) = run(&dir, &repo, None, &["ENG-1", "Fix the redirect"]);
    assert!(ok, "{out}\n{err}");
    assert_eq!(out, "taskman isn't running; ENG-1's title is set when it starts.\n");
    assert_eq!(saved_label(&dir, "ENG-1"), None, "the command never writes the state");

    let mut tui = common::Tui::start(
        "a_title_waits_for_taskman_to_start",
        &dir,
        common::stubbed(&dir),
        Duration::from_secs(60),
    );
    tui.wait_text("Fix the redirect");
    tui.quit();
    assert_eq!(saved_label(&dir, "ENG-1").as_deref(), Some("Fix the redirect"));
}

#[test]
fn an_older_taskman_is_told_apart() {
    let dir = common::scratch_dir("title-older");
    let repo = common::project(&dir, &["ENG-1"]);
    // A running taskman of the inbox format before this one: its `alive` file, and its lock on the
    // state folder, which says it runs however old the file gets on a slow machine.
    let state = dir.join("state/taskman");
    let inbox = state.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let alive = serde_json::json!({
        "version": "0.1.36", "inbox": 6, "lock": true, "pid": std::process::id(),
    });
    std::fs::write(inbox.join("alive"), alive.to_string()).unwrap();
    let lock = std::fs::File::create(state.join("lock")).unwrap();
    // SAFETY: flock on a descriptor this test owns; it reads or writes no memory.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let (ok, out, err) = run(&dir, &repo, None, &["ENG-1", "Fix"]);
    assert!(!ok, "{out}");
    assert!(
        err.starts_with(
            "taskman title: taskman 0.1.36 is running, which doesn't know taskman title yet"
        ),
        "{err}"
    );
    let left: Vec<_> = std::fs::read_dir(&inbox)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name() != "alive")
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "left a request for it: {left:?}");
}

#[test]
fn refuses_what_it_cant_mean() {
    let dir = common::scratch_dir("title-refuses");
    let repo = common::project(&dir, &["ENG-1"]);
    for (args, says) in [
        (&["Fix"][..], "taskman title: name the ticket"),
        (&["ENG-1", ""], "taskman title: the title is empty"),
        (&["ENG-9", "Fix"], "taskman title: ENG-9 isn't open in acme"),
        (&["a", "b", "c"], "taskman title: put the title in quotes"),
        (&["--clear"], "taskman title: --clear needs the ticket"),
        (&["ENG-1", "--clear"], "taskman title: --clear needs the ticket"),
        (&["ENG-1"], "taskman title: ENG-1 looks like a ticket"),
        (&["-v broken"], "taskman title: unknown option -v broken"),
        (&["ENG-1", "\u{200b}"], "taskman title: the title is empty"),
        (&["Fix", "--project", "acme"], "taskman title: --project goes with a ticket"),
        (&["--project="], "taskman title: --project needs a value"),
    ] {
        let (ok, out, err) = run(&dir, &repo, None, args);
        assert!(!ok && out.is_empty(), "{args:?}: {out}");
        assert!(err.starts_with(says), "{args:?}: {err}");
    }
    let inbox = dir.join("state/taskman/inbox");
    assert!(!inbox.exists() || std::fs::read_dir(&inbox).unwrap().next().is_none());
}

/// What a script passes after `--` is its text, also when it reads as help: printing the usage and
/// exiting 0 would look to the script as if the title were set.
#[test]
fn after_dashes_help_is_only_text() {
    let dir = common::scratch_dir("title-dashes");
    let repo = common::project(&dir, &["ENG-1"]);
    let requests = || -> String {
        std::fs::read_dir(dir.join("state/taskman/inbox"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
            .collect()
    };
    for help in ["--help", "-h"] {
        let (ok, out, err) = run(&dir, &repo, Some("ENG-1"), &["--", help]);
        assert!(ok, "{out}\n{err}");
        assert_eq!(out, "taskman isn't running; ENG-1's title is set when it starts.\n");
        assert!(requests().contains(&format!("\"text\": \"{help}\"")), "{}", requests());
    }
    // Before `--` it's still help, and leaves nothing.
    let before = requests();
    let (ok, out, _) = run(&dir, &repo, Some("ENG-1"), &["--help", "--", "Fix"]);
    assert!(ok && out.starts_with("Usage: taskman title"), "{out}");
    assert_eq!(requests(), before);
}

/// A title that has a ticket's key in it is still a title, in a session's own words.
#[test]
fn a_phrase_with_a_key_in_it_is_a_title() {
    let dir = common::scratch_dir("title-phrase");
    let repo = common::project(&dir, &["ENG-1"]);
    let (ok, out, err) = run(&dir, &repo, Some("ENG-1"), &["Fix ENG-53 fallout"]);
    assert!(ok, "{out}\n{err}");
    let (ok, _, err) = run(&dir, &repo, Some("ENG-1"), &["ENG-53"]);
    assert!(!ok && err.contains("looks like a ticket, not a title"), "{err}");
}
