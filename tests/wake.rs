//! `taskman wake` against the binary: a session wakes another ticket's session on a running
//! taskman, which starts it in its own pane without switching the view and answers once the CLI
//! reports its start; asked again, it's awake; with taskman closed, the command refuses.

mod common;

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

/// `taskman wake ARGS` in `session`'s environment as taskman sets it, in `repo`.
fn run(dir: &Path, repo: &Path, session: &str, args: &[&str]) -> (bool, String, String) {
    let mut cmd = common::taskman(dir);
    cmd.env("TASKMAN_TICKET", session).env("TASKMAN_REPO", repo);
    let out: Output =
        cmd.arg("wake").args(args).current_dir(repo).stdin(Stdio::null()).output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.success(), text(&out.stdout), text(&out.stderr))
}

#[test]
fn a_session_wakes_another_tickets_session() {
    let dir = common::scratch_dir("wake");
    let repo = common::project(&dir, &["ENG-1", "ENG-2"]);
    let mut cmd = common::stubbed(&dir);
    cmd.env("TASKMAN_TEST_SESSION_START", "1");
    let mut tui = common::Tui::start(
        "a_session_wakes_another_tickets_session",
        &dir,
        cmd,
        Duration::from_secs(60),
    );
    let alive = dir.join("state/taskman/inbox/alive");
    tui.wait_until("taskman's inbox heartbeat", || alive.exists());
    tui.wait_text("stub ready for ENG-1");
    let started = |key: &str| dir.join(format!("{key}.pid")).exists();
    assert!(!started("ENG-2"), "nothing starts it before the wake");

    tui.step("ENG-1's session wakes ENG-2");
    let (ok, out, err) = run(&dir, &repo, "ENG-1", &["ENG-2"]);
    assert!(ok, "{out}\n{err}");
    assert!(out.contains("ENG-2 is awake; message it as ENG-2"), "{out}");
    assert!(started("ENG-2"));
    // The view stays on ENG-1's session.
    let screen = tui.text();
    assert!(screen.contains("stub ready for ENG-1") && !screen.contains("stub ready for ENG-2"));

    tui.step("again: it's awake already");
    let (ok, out, err) = run(&dir, &repo, "ENG-1", &["2"]);
    assert!(ok, "{out}\n{err}");
    assert!(out.contains("ENG-2 is awake"), "{out}");

    tui.step("with taskman closed, nothing to wake");
    tui.quit();
    let (ok, out, err) = run(&dir, &repo, "ENG-1", &["ENG-2"]);
    assert!(!ok, "{out}");
    assert!(err.contains("taskman isn't running"), "{err}");
}
