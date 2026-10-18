//! A stopped session's CLI is gone before anything takes its place. Dropping a session hangs its
//! CLI up and reaps it on a thread, and a CLI can take a moment to quit; the lock that keeps a
//! session to one copy is taskman's, not the CLI's. So until the CLI is gone, a restart
//! (`prefix R`) mustn't start the next copy on the same conversation, closing the ticket mustn't
//! remove the worktree the CLI runs in, and a hand-off (`prefix t`) mustn't let `taskman resume`
//! start a second copy. The stub CLI here traps the hangup and goes on for a while, as a CLI busy
//! finishing does; taskman's kill ends it sooner.

mod common;

use common::{Tui, acme, scratch_dir, script, sh, stub_file, stubbed, taskman};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const SID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

/// A CLI that stays for up to 2 s after a hangup, which it notes in `dir/<KEY>.signals`, letting
/// go of its terminal at once as a CLI finishing up does; its pid is in `dir/<KEY>.pid`.
fn slow_cli(dir: &Path) -> PathBuf {
    let text = r#"#!/bin/sh
f="DIR/$TASKMAN_TICKET"
echo $$ > "$f.pid"
hup=0
trap 'hup=1; echo HUP >> "$f.signals"; exec >/dev/null 2>&1 </dev/null' HUP
echo "stub ready for $TASKMAN_TICKET"
i=0
while [ $i -lt 40 ]; do
  sleep 0.05
  [ $hup = 1 ] && i=$((i + 1))
done
"#;
    script(dir, "claude-slow", &text.replace("DIR", &dir.display().to_string()))
}

/// The pid the stub CLI last started for `key` wrote.
fn pid(dir: &Path, key: &str) -> Option<i32> {
    stub_file(dir, key, "pid").trim().parse().ok()
}

/// Whether `pid` is a running process: not gone, and not a zombie waiting to be reaped, which
/// `kill -0` would still count.
fn alive(pid: i32) -> bool {
    let out = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// taskman with the slow CLI and the desktop stubs, on the project in `dir`.
fn slow(dir: &Path) -> Command {
    let mut cmd = stubbed(dir);
    cmd.env("TASKMAN_CLAUDE", slow_cli(dir)).env("CLAUDE_CONFIG_DIR", dir.join("claude-config"));
    cmd
}

fn quit(tui: &mut Tui) {
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

/// `prefix R` starts the new session only once the old CLI is gone: a second copy on the same
/// conversation would interleave its turns with the first's.
#[test]
fn a_restart_starts_the_next_cli_only_once_the_old_one_is_gone() {
    let dir = scratch_dir("stopping-restart");
    common::project(&dir, &["ENG-1"]);
    let mut tui = Tui::start(
        "a_restart_starts_the_next_cli_only_once_the_old_one_is_gone",
        &dir,
        slow(&dir),
        Duration::from_secs(60),
    );
    tui.wait_text("stub ready for ENG-1");
    let first = pid(&dir, "ENG-1").unwrap();

    tui.send(b"\x01R");
    // Checked from the restart on: as long as the old CLI runs, none replaces it. Each check
    // reads what it guards first and whether the CLI runs second: a CLI that's gone stays gone,
    // so a failure means both held at once.
    loop {
        let seen = pid(&dir, "ENG-1");
        if !alive(first) {
            break;
        }
        assert_eq!(seen, Some(first), "a second CLI started while the first was still running");
        std::thread::sleep(Duration::from_millis(5));
    }
    tui.wait_until("no CLI started once the old one was gone", || {
        pid(&dir, "ENG-1").is_some_and(|p| p != first)
    });
    assert!(stub_file(&dir, "ENG-1", "signals").contains("HUP"), "the old CLI wasn't hung up");
    quit(&mut tui);
}

/// Closing removes the worktree only once the CLI that ran in it is gone, from the close dialog
/// and from `taskman close` alike.
#[test]
fn closing_removes_the_worktree_only_once_its_cli_is_gone() {
    // Real paths: git lists worktrees by them, and macOS's temp folder is behind a symlink.
    let dir = scratch_dir("stopping-close").canonicalize().unwrap();
    let repo = acme(&dir);
    let (wt1, wt2) = (dir.join("wt-ENG-1"), dir.join("wt-ENG-2"));
    // Fresh from main: nothing to lose, so the removal is offered and runs.
    sh(
        &repo,
        "git worktree add -q -b ENG-1 ../wt-ENG-1 origin/main && git worktree add -q -b ENG-2 ../wt-ENG-2 origin/main",
    );
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let in_worktree = |key: &str, wt: &Path| serde_json::json!({"key": key, "repo": repo, "cwd": wt, "branch": key, "created_worktree": true});
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [in_worktree("ENG-1", &wt1), in_worktree("ENG-2", &wt2)],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let mut tui = Tui::start(
        "closing_removes_the_worktree_only_once_its_cli_is_gone",
        &dir,
        slow(&dir),
        Duration::from_secs(90),
    );
    tui.wait_text("stub ready for ENG-1");
    let cli = pid(&dir, "ENG-1").unwrap();

    // The dialog's `r`.
    tui.send(b"\x01x");
    tui.wait_text("r close and remove");
    tui.send(b"r");
    loop {
        let there = wt1.exists();
        if !alive(cli) {
            break;
        }
        assert!(there, "the worktree was removed while its CLI was still running in it");
        std::thread::sleep(Duration::from_millis(5));
    }
    tui.wait_until("the worktree wasn't removed once the CLI was gone", || !wt1.exists());

    // `taskman close`, on ENG-2, selected now.
    tui.wait_text("stub ready for ENG-2");
    let cli = pid(&dir, "ENG-2").unwrap();
    tui.wait_until("taskman isn't taking commands", || state.join("inbox/alive").exists());
    let closer = taskman(&dir)
        .args(["close", "ENG-2"])
        .current_dir(&repo)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    loop {
        let there = wt2.exists();
        if !alive(cli) {
            break;
        }
        assert!(there, "the worktree was removed while its CLI was still running in it");
        std::thread::sleep(Duration::from_millis(5));
    }
    tui.wait_until("the worktree wasn't removed once the CLI was gone", || !wt2.exists());
    let out = closer.wait_with_output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("Closed ENG-2. Removed {} and branch ENG-2\n", wt2.display())
    );
    quit(&mut tui);
}

/// Handed off, the session stays claimed until the pane's CLI is gone, so `taskman resume` can't
/// start a second copy meanwhile; run then, it waits its turn rather than refusing.
#[test]
fn a_hand_off_keeps_the_session_claimed_until_the_old_cli_is_gone() {
    let dir = scratch_dir("stopping-handoff");
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    std::fs::write(repo.join("workplan/todo/ENG-1-test.md"), "# test\n").unwrap();
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    // With a conversation to continue, so `taskman resume` has something to start.
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [{"key": "ENG-1", "repo": repo, "cwd": repo, "session_id": SID}],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let transcripts = dir.join("claude-config/projects/acme");
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join(format!("{SID}.jsonl")), "{}\n").unwrap();
    let mut tui = Tui::start(
        "a_hand_off_keeps_the_session_claimed_until_the_old_cli_is_gone",
        &dir,
        slow(&dir),
        Duration::from_secs(60),
    );
    tui.wait_text("stub ready for ENG-1");
    let first = pid(&dir, "ENG-1").unwrap();
    let owner = std::fs::read_dir(state.join("status"))
        .unwrap()
        .flatten()
        .map(|d| d.path().join("ENG-1.owner"))
        .find(|p| p.exists())
        .expect("no owner file");
    let claimed = || {
        let f = std::fs::File::open(&owner).unwrap();
        // SAFETY: flock on a descriptor this closure owns; closing it lets go.
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 }
    };

    // The CLI lets go of its terminal on the hangup and goes on: the pane sees it ended and
    // hands the session off, but keeps it claimed until the CLI is gone.
    tui.send(b"\x01t");
    tui.wait_text("ENG-1 is handed off");
    loop {
        let held = claimed();
        if !alive(first) {
            break;
        }
        assert!(held, "the session was let go while its CLI was still running");
        std::thread::sleep(Duration::from_millis(5));
    }
    // `taskman resume` then runs it, waiting the moment the TUI takes to let go if it must.
    let resume = slow(&dir)
        .args(["resume", "ENG-1"])
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tui.wait_until("taskman resume didn't start the session", || {
        pid(&dir, "ENG-1").is_some_and(|p| p != first)
    });
    let second = pid(&dir, "ENG-1").unwrap();
    tui.wait_text("ENG-1 is running in another terminal");
    // SAFETY: sends a signal; no memory is involved.
    unsafe { libc::kill(second, libc::SIGKILL) };
    let out = resume.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ENG-1 is back"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    quit(&mut tui);
}

/// Renamed, an agent's session restarts under its new name once the old CLI is gone; the new
/// name is claimed before the rename is saved, so `taskman resume` under it can't start a second
/// copy on the conversation meanwhile, and is refused as for any session the TUI holds.
#[test]
fn a_rename_claims_the_new_name_before_the_old_cli_is_gone() {
    let dir = scratch_dir("stopping-rename");
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    // With a conversation to continue, so `taskman resume` has something to start.
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo}],
        "tickets": [{"key": "acme-lead", "repo": repo, "cwd": repo, "agent": "lead", "session_id": SID}],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let transcripts = dir.join("claude-config/projects/acme");
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join(format!("{SID}.jsonl")), "{}\n").unwrap();
    let mut tui = Tui::start(
        "a_rename_claims_the_new_name_before_the_old_cli_is_gone",
        &dir,
        slow(&dir),
        Duration::from_secs(60),
    );
    tui.wait_text("stub ready for acme-lead");
    let first = pid(&dir, "acme-lead").unwrap();

    tui.send(b"\x01e");
    tui.wait_text("Rename agent");
    tui.send(b"\x15");
    tui.send(b"planner");
    tui.send(b"\r");
    tui.wait_text("Renamed ✦ lead to ✦ planner");
    // The moment the rename is saved, `taskman resume` asks for the new name, while the old CLI
    // is still on its way out.
    let resume = slow(&dir)
        .args(["resume", "planner", "--agent"])
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    loop {
        let seen = pid(&dir, "acme-planner");
        if !alive(first) {
            break;
        }
        assert_eq!(
            seen, None,
            "a CLI started under the new name while the old one was still running"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(stub_file(&dir, "acme-lead", "signals").contains("HUP"), "the old CLI wasn't hung up");
    // Gone: the pane starts the session under the new name, as a restart does.
    tui.wait_text("stub ready for acme-planner");
    let second = pid(&dir, "acme-planner").unwrap();
    assert_ne!(second, first);
    // `taskman resume` was refused, the session being the TUI's.
    let resume_pid = i32::try_from(resume.id()).unwrap();
    tui.wait_until("taskman resume didn't give up", || !alive(resume_pid));
    let out = resume.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("planner is running in taskman"), "{err}");
    assert_eq!(pid(&dir, "acme-planner"), Some(second), "taskman resume started a CLI");
    quit(&mut tui);
}

/// A rename that can't be saved changes nothing: tickets.json still names the agent by its old
/// name, so `taskman resume` would find it there, and the old name stays claimed while the pane's
/// CLI runs on. Saving fails here as on a full disk: `write_atomic` can't create its temporary file.
#[test]
fn a_rename_that_can_not_be_saved_keeps_the_old_name_claimed() {
    let dir = scratch_dir("stopping-rename-unsaved");
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo}],
        "tickets": [{"key": "acme-lead", "repo": repo, "cwd": repo, "agent": "lead", "session_id": SID}],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let transcripts = dir.join("claude-config/projects/acme");
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join(format!("{SID}.jsonl")), "{}\n").unwrap();
    let mut tui = Tui::start(
        "a_rename_that_can_not_be_saved_keeps_the_old_name_claimed",
        &dir,
        slow(&dir),
        Duration::from_secs(60),
    );
    tui.wait_text("stub ready for acme-lead");
    let first = pid(&dir, "acme-lead").unwrap();
    let in_the_way = state.join("tickets.tmp");
    std::fs::create_dir_all(in_the_way.join("in-the-way")).unwrap();

    tui.send(b"\x01e");
    tui.wait_text("Rename agent");
    tui.send(b"\x15");
    tui.send(b"planner");
    tui.send(b"\r");
    // Refused, or (as before the fix) renamed with the old name let go: `taskman resume` says.
    let done = ["✦ lead keeps its name: couldn't save", "Renamed ✦ lead"];
    tui.wait_for("the rename's answer", |s| {
        done.iter().any(|d| s.contents().contains(d)).then_some(())
    });
    // The saved state still has the old name, which the pane's session must keep.
    let resume = slow(&dir)
        .args(["resume", "lead", "--agent"])
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let resume_pid = i32::try_from(resume.id()).unwrap();
    let started = || pid(&dir, "acme-lead").filter(|&p| p != first);
    tui.wait_until("taskman resume neither gave up nor started", || {
        !alive(resume_pid) || started().is_some()
    });
    if let Some(second) = started() {
        // Its stub runs until hung up: end it before failing.
        // SAFETY: kill only sends a signal, to the stub's pid.
        unsafe { libc::kill(second, libc::SIGKILL) };
        panic!("taskman resume started a second CLI on the conversation");
    }
    let out = resume.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("lead is running in taskman"), "{err}");
    tui.wait_text("✦ lead keeps its name: couldn't save");
    assert!(alive(first), "the pane's CLI was stopped");
    assert_eq!(pid(&dir, "acme-planner"), None, "a CLI started under the new name");

    std::fs::remove_dir_all(&in_the_way).unwrap();
    tui.send(b"\x1b");
    tui.wait_for("the form closed", |s| (!s.contents().contains("Rename agent")).then_some(()));
    quit(&mut tui);
}
