//! How taskman starts and ends, against the binary in a pty with stub sessions: SIGTERM and SIGHUP
//! quit as `prefix q` does, restoring the terminal, stopping the sessions and waiting for a
//! worktree still being removed, which a second signal cuts short; ctrl+z never reaches a session;
//! a panic in one session's reader ends that session alone, and its pane says so; a second taskman
//! on the same state, its lock file removed or not, or a tickets.json this release can't read, is
//! refused and nothing is overwritten, while one started straight after a kill -9 starts. Losing
//! the terminal is tests/hangup.rs.

mod common;

use common::{Tui, acme, project, scratch_dir, sh, slow_git, stub_file, stubbed, taskman};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(60);

#[test]
fn sigterm_quits_like_prefix_q() {
    signal_quits("sigterm", libc::SIGTERM);
}

#[test]
fn sighup_quits_like_prefix_q() {
    signal_quits("sighup", libc::SIGHUP);
}

fn signal_quits(name: &'static str, signal: i32) {
    let dir = scratch_dir(name);
    project(&dir, &["ENG-1"]);
    let mut tui = Tui::start(name, &dir, stubbed(&dir), LIMIT);
    tui.wait_text("stub ready for ENG-1");

    // SAFETY: sends a signal; no memory is involved.
    unsafe { libc::kill(tui.child.id().cast_signed(), signal) };
    let status = tui.wait_exit();
    assert!(status.success(), "{status}");
    tui.wait_until("the session wasn't stopped", || {
        stub_file(&dir, "ENG-1", "signals").contains("HUP")
    });
    // The terminal as it was: off the alternate screen, no mouse reports or bracketed paste. The
    // last of taskman's output may still be on its way through the pty.
    let undone = |set: &str, reset: &str| {
        tui.with(|t| {
            let raw = String::from_utf8_lossy(&t.raw);
            raw.rfind(reset).is_some_and(|r| raw.rfind(set).is_some_and(|s| s < r))
        })
    };
    tui.wait_until("still on the alternate screen", || undone("\x1b[?1049h", "\x1b[?1049l"));
    tui.wait_until("mouse reports still on", || undone("\x1b[?1000h", "\x1b[?1000l"));
    tui.wait_until("bracketed paste still on", || undone("\x1b[?2004h", "\x1b[?2004l"));
    assert!(tui.cooked(), "the terminal was left raw");
}

#[test]
fn a_signal_waits_for_a_removal_like_prefix_q() {
    let dir = scratch_dir("signal-removal").canonicalize().unwrap();
    let (tui, alive) = removing(&dir, "a_signal_waits_for_a_removal_like_prefix_q");
    let mut tui = tui;

    // SAFETY: sends a signal; no memory is involved.
    unsafe { libc::kill(tui.child.id().cast_signed(), libc::SIGTERM) };
    // Off the alternate screen, it says what it waits for; the session is stopped meanwhile.
    tui.wait_text("taskman: Quitting once ENG-9's worktree is removed");
    tui.wait_until("the session wasn't stopped", || {
        stub_file(&dir, "ENG-9", "signals").contains("HUP")
    });
    tui.wait_until("the heartbeat stayed", || !alive.exists());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(tui.child.try_wait().unwrap(), None, "ended before git did");
    assert!(dir.join("wt-ENG-9").exists());

    std::fs::write(dir.join("release-wt-ENG-9"), "").unwrap();
    let status = tui.wait_exit();
    assert!(status.success(), "{status}");
    assert!(!dir.join("wt-ENG-9").exists(), "the worktree stayed");
    assert!(!branches(&dir.join("acme")).contains("ENG-9"), "the branch stayed");
    assert!(tui.cooked(), "the terminal was left raw");
}

#[test]
fn a_second_signal_ends_the_wait() {
    let dir = scratch_dir("signal-twice").canonicalize().unwrap();
    let (tui, alive) = removing(&dir, "a_second_signal_ends_the_wait");
    let mut tui = tui;
    let pid = tui.child.id().cast_signed();

    // SAFETY: sends a signal; no memory is involved.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    tui.wait_text("taskman: Quitting once ENG-9's worktree is removed");
    let again = Instant::now();
    // SAFETY: as above.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let status = tui.wait_exit();
    assert!(again.elapsed() < Duration::from_secs(3), "took {:?} to end", again.elapsed());
    assert!(status.success(), "{status}");
    assert!(!alive.exists(), "taskman ended without removing its heartbeat");
    // git is still held: the removal goes on without taskman (tests/close.rs covers what that
    // means for the ticket), so let it finish before the folder goes.
    assert!(dir.join("wt-ENG-9").exists(), "the removal didn't wait for git");
    std::fs::write(dir.join("release-wt-ENG-9"), "").unwrap();
    tui.wait_until("git never finished the removal", || !dir.join("wt-ENG-9").exists());
}

/// Project `acme` in `dir` with ticket ENG-9 in its own worktree, and taskman closing it from the
/// close dialog with git held at `worktree remove` (`slow_git`), so the quit has something to wait
/// for. Returns taskman and its heartbeat file.
fn removing(dir: &Path, name: &'static str) -> (Tui, std::path::PathBuf) {
    let repo = acme(dir);
    sh(&repo, "git worktree add -q -b ENG-9 ../wt-ENG-9 origin/main");
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    std::fs::write(repo.join("workplan/todo/ENG-9-test.md"), "# test\n").unwrap();
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [{"key": "ENG-9", "repo": repo, "cwd": dir.join("wt-ENG-9"), "branch": "ENG-9", "created_worktree": true}],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    // The held-up git goes first on PATH, with the stub desktop commands.
    slow_git(dir);
    let mut tui = Tui::start(name, dir, stubbed(dir), LIMIT);
    tui.wait_text("stub ready for ENG-9");
    let alive = state.join("inbox/alive");
    tui.wait_until("no heartbeat", || alive.exists());

    tui.send(b"\x01x");
    tui.wait_text("Close ENG-9?");
    tui.wait_text("r ");
    tui.send(b"r");
    tui.wait_until("the removal never reached git worktree remove", || {
        dir.join("removing-wt-ENG-9").exists()
    });
    (tui, alive)
}

fn branches(repo: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["branch", "--list"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn ctrl_z_never_reaches_the_session() {
    let dir = scratch_dir("ctrl-z");
    project(&dir, &["ENG-1"]);
    let mut tui = Tui::start("ctrl_z_never_reaches_the_session", &dir, stubbed(&dir), LIMIT);
    tui.wait_text("stub ready for ENG-1");

    // A CLI that got it would stop for good: nothing in its pty can resume it.
    tui.send(b"\x1a");
    tui.send(b"after\r");
    tui.wait_text("got: after");
    assert_eq!(stub_file(&dir, "ENG-1", "signals"), "");
    assert_eq!(stub_file(&dir, "ENG-1", "input"), "after\n");
}

#[test]
fn a_panic_in_one_session_ends_only_that_session() {
    let dir = scratch_dir("panic");
    project(&dir, &["ENG-1", "ENG-2"]);
    let mut cmd = stubbed(&dir);
    cmd.env("TASKMAN_TEST_PANIC", "reader-panic-marker");
    let mut tui = Tui::start("a_panic_in_one_session_ends_only_that_session", &dir, cmd, LIMIT);
    tui.wait_text("stub ready for ENG-1");

    tui.send(b"panic\r");
    let crashed = "taskman stopped reading this session after an error";
    tui.wait_text(crashed);
    // The screen as the reader left it, part-way through the output, under the crash line.
    let text = tui.text();
    assert!(text.contains("stub ready for ENG-1") && text.contains("half-drawn"), "{text}");
    let log = dir.join("state/taskman/panics.log");
    tui.wait_until("nothing in panics.log", || {
        std::fs::read_to_string(&log).is_ok_and(|l| l.contains("test panic"))
    });
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("thread 'reader ENG-1'"), "the thread isn't named:\n{logged}");
    assert!(
        !tui.text().contains("panicked"),
        "the panic was printed over the screen:\n{}",
        tui.text()
    );

    // Its screen, left behind by a reader that panicked holding it, still resizes and draws.
    tui.resize(30, 100);
    tui.wait_until("not drawn again at 30 rows", || {
        tui.text().lines().nth(29).is_some_and(|l| l.contains("menu"))
    });
    tui.wait_text(crashed);
    tui.send(b"\x01]");
    tui.wait_text("stub ready for ENG-2");
    tui.send(b"\x01[");
    tui.wait_text(crashed);
    tui.wait_text("stub ready for ENG-1");

    // The other session and the TUI go on.
    tui.send(b"\x01]");
    tui.wait_text("stub ready for ENG-2");
    tui.send(b"still here\r");
    tui.wait_text("got: still here");
    assert_eq!(tui.child.try_wait().unwrap(), None);
    tui.quit();
}

#[test]
fn a_second_taskman_is_refused() {
    let dir = scratch_dir("second");
    project(&dir, &["ENG-1"]);
    let tickets = dir.join("state/taskman/tickets.json");
    let tui = Tui::start("a_second_taskman_is_refused", &dir, stubbed(&dir), LIMIT);
    tui.wait_text("stub ready for ENG-1");
    // The GitHub check at startup saves what it found whenever it finishes, the session ready or
    // not: compare with the file from after that.
    tui.wait_until("the GitHub check wasn't saved", || {
        std::fs::read_to_string(&tickets).is_ok_and(|t| t.contains("\"not_git\""))
    });
    let saved = std::fs::read_to_string(&tickets).unwrap();

    let second = taskman(&dir).stdin(Stdio::null()).output().unwrap();
    let err = String::from_utf8_lossy(&second.stderr);
    assert_eq!(second.status.code(), Some(1), "{err}");
    assert!(
        err.contains(&format!("another taskman is already running (pid {})", tui.child.id())),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&tickets).unwrap(), saved);
    assert!(tui.text().contains("stub ready for ENG-1"));
}

/// With its lock file removed while it runs (or a state folder shared where flock doesn't reach),
/// a second taskman takes a lock of its own, but the first one's heartbeat names a process that
/// runs: refused all the same, and the heartbeat left for the first.
#[test]
fn a_second_taskman_is_refused_when_the_lock_file_is_gone() {
    let dir = scratch_dir("lock-gone");
    project(&dir, &["ENG-1"]);
    let alive = dir.join("state/taskman/inbox/alive");
    let mut tui = Tui::start("lock-gone", &dir, stubbed(&dir), LIMIT);
    tui.wait_text("stub ready for ENG-1");
    std::fs::remove_file(dir.join("state/taskman/lock")).unwrap();

    let second = taskman(&dir).stdin(Stdio::null()).output().unwrap();
    let err = String::from_utf8_lossy(&second.stderr);
    assert_eq!(second.status.code(), Some(1), "{err}");
    assert!(
        err.contains(&format!("another taskman is already running (pid {})", tui.child.id())),
        "{err}"
    );
    assert!(alive.exists());
    assert!(tui.text().contains("stub ready for ENG-1"));
    tui.quit();
}

/// Killed (kill -9, an OOM kill), taskman leaves its heartbeat behind, fresh for a few seconds.
/// One started straight after holds the lock, so nothing else is running: it starts, and says the
/// last one didn't quit cleanly.
#[test]
fn a_taskman_started_straight_after_a_kill_starts() {
    let dir = scratch_dir("killed");
    project(&dir, &["ENG-1"]);
    let alive = dir.join("state/taskman/inbox/alive");
    let mut killed = Tui::start("killed", &dir, stubbed(&dir), LIMIT);
    killed.wait_text("stub ready for ENG-1");
    // SAFETY: sends a signal; no memory is involved.
    unsafe { libc::kill(killed.child.id().cast_signed(), libc::SIGKILL) };
    killed.wait_exit();
    let age = std::fs::metadata(&alive).unwrap().modified().unwrap().elapsed().unwrap();
    assert!(age < Duration::from_secs(2), "the heartbeat is {age:?} old: not fresh enough to test");

    let mut tui = Tui::start("killed-again", &dir, stubbed(&dir), LIMIT);
    tui.wait_text("taskman didn't quit cleanly last time");
    tui.wait_text("stub ready for ENG-1");
    tui.quit();
    assert!(!alive.exists());
}

#[test]
fn a_tickets_json_it_cant_read_is_refused_and_kept() {
    let invalid: &[u8] = b"{\"projects\": [";
    // Valid JSON from a newer release, with a tracker this one doesn't know.
    let newer: &[u8] = br#"{"projects": [{"name": "acme", "repo": "/tmp/acme", "tracker": {"provider": "fogbugz"}}]}"#;
    // Not text at all.
    let binary: &[u8] = b"\x00\xff\xfe taskman\x80\x81";
    for text in [invalid, newer, binary] {
        let dir = scratch_dir("bad-tickets");
        let state = dir.join("state/taskman");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("tickets.json"), text).unwrap();

        let out = taskman(&dir).stdin(Stdio::null()).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{err}");
        assert!(err.contains("tickets.json") && err.contains("It was left as it is"), "{err}");
        assert!(err.contains("or move it aside to start without it"), "{err}");
        assert_eq!(std::fs::read(state.join("tickets.json")).unwrap(), text);
        // Refused before it took the terminal.
        assert!(!String::from_utf8_lossy(&out.stdout).contains("\x1b[?1049h"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
