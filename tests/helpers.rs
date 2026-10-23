//! The programs taskman runs for itself (gh here; git and token commands the same way) can't reach
//! the terminal taskman draws on: one that asks there, as ssh does for a key's passphrase, fails at
//! once rather than writing over the screen or waiting on keys the TUI reads.

mod common;

use common::{Tui, project, scratch_dir, script, sh, stub_tools, stubbed};
use std::path::Path;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// A gh that asks for a passphrase on the terminal, as ssh does, and notes in `dir/tty` whether it
/// could open it.
fn asking_gh(dir: &Path) {
    let text = r#"#!/bin/sh
if [ "$1 $2" = "auth token" ]; then
  if { printf 'Enter passphrase for key: ' > /dev/tty; } 2>/dev/null; then
    echo reachable > 'DIR/tty.part'
  else
    echo unreachable > 'DIR/tty.part'
  fi
  mv 'DIR/tty.part' 'DIR/tty'
fi
echo 'gh stub: no answer' >&2
exit 1
"#;
    script(&stub_tools(dir), "gh", &text.replace("DIR", &dir.display().to_string()));
}

#[test]
fn a_helper_cannot_reach_the_terminal() {
    let dir = scratch_dir("helpers");
    let repo = project(&dir, &["ENG-52"]);
    sh(&repo, "git init -q && git remote add origin https://github.com/acme/web.git");
    asking_gh(&dir);
    let mut cmd = stubbed(&dir);
    cmd.env("GIT_ALLOW_PROTOCOL", "file");
    let mut tui = Tui::start("a_helper_cannot_reach_the_terminal", &dir, cmd, LIMIT);
    tui.wait_text("stub ready for ENG-52");

    tui.step("gh asks on the terminal");
    let tty = dir.join("tty");
    tui.wait_until("gh asked", || tty.exists());
    assert_eq!(std::fs::read_to_string(&tty).unwrap(), "unreachable\n");
    assert!(
        !tui.text().contains("Enter passphrase"),
        "the prompt reached the screen:\n{}",
        tui.text()
    );
    tui.quit();
}

/// A gh that never answers `gh auth token`, as one waiting on a locked keyring doesn't, with a
/// child of its own; it notes both pids in `dir/pids`.
fn hanging_gh(dir: &Path) {
    let text = r#"#!/bin/sh
if [ "$1 $2" = "auth token" ]; then
  sleep 600 &
  echo $$ $! > 'DIR/pids.part'
  mv 'DIR/pids.part' 'DIR/pids'
  exec sleep 600
fi
echo 'gh stub: no answer' >&2
exit 1
"#;
    script(&stub_tools(dir), "gh", &text.replace("DIR", &dir.display().to_string()));
}

/// Whether `pid` has ended: gone, or a zombie its new parent hasn't reaped yet.
fn ended(pid: &str) -> bool {
    let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", pid]).output().unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    stat.trim().is_empty() || stat.trim().starts_with('Z')
}

#[test]
fn a_helper_still_running_is_stopped_at_quit() {
    let dir = scratch_dir("helpers-quit");
    let repo = project(&dir, &["ENG-52"]);
    sh(&repo, "git init -q && git remote add origin https://github.com/acme/web.git");
    hanging_gh(&dir);
    let mut cmd = stubbed(&dir);
    cmd.env("GIT_ALLOW_PROTOCOL", "file");
    let mut tui = Tui::start("a_helper_still_running_is_stopped_at_quit", &dir, cmd, LIMIT);
    tui.wait_text("stub ready for ENG-52");

    tui.step("gh hangs");
    let pids = dir.join("pids");
    tui.wait_until("gh started", || pids.exists());
    let pids = std::fs::read_to_string(&pids).unwrap();
    let pids: Vec<&str> = pids.split_whitespace().collect();
    assert_eq!(pids.len(), 2, "{pids:?}");
    assert!(pids.iter().all(|p| !ended(p)), "gh ended before the quit");

    tui.step("quit");
    tui.quit();
    let until = std::time::Instant::now() + Duration::from_secs(5);
    for pid in pids {
        while !ended(pid) {
            assert!(std::time::Instant::now() < until, "{pid} outlived taskman");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A gh that never answers, with a child of its own; it adds both pids to `dir/pids` and touches
/// `dir/gh-<pid>` once they're there.
fn stuck_gh(dir: &Path) {
    let text = r#"#!/bin/sh
sleep 600 &
echo $$ $! >> 'DIR/pids'
touch "DIR/gh-$$"
exec sleep 600
"#;
    script(&stub_tools(dir), "gh", &text.replace("DIR", &dir.display().to_string()));
}

/// ctrl+c reaches a command run from a shell, not the helpers it started, which run in sessions of
/// their own: the command stops them before it ends, and ends as the signal would have, so the
/// shell sees the interrupt.
#[test]
fn an_interrupted_command_stops_its_helpers() {
    use std::os::unix::process::ExitStatusExt;
    use std::time::Instant;
    let dir = scratch_dir("helpers-interrupt");
    let repo = project(&dir, &["ENG-52"]);
    sh(&repo, "git init -q && git remote add origin https://github.com/acme/web.git");
    stuck_gh(&dir);
    let mut cmd = common::taskman(&dir);
    cmd.args(["status", "ENG-52"])
        .current_dir(&repo)
        .env("GIT_ALLOW_PROTOCOL", "file")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut taskman = cmd.spawn().unwrap();
    let until = Instant::now() + LIMIT;
    let pids = dir.join("pids");
    // Each gh notes its pids before it touches its file, so those it has touched are all there.
    let started = || {
        let pids = std::fs::read_to_string(&pids).unwrap_or_default();
        let gh: Vec<String> =
            pids.lines().filter_map(|l| l.split(' ').next()).map(String::from).collect();
        (!gh.is_empty() && gh.iter().all(|p| dir.join(format!("gh-{p}")).exists())).then_some(pids)
    };
    let pids = loop {
        if let Some(pids) = started() {
            break pids;
        }
        if Instant::now() > until || taskman.try_wait().unwrap().is_some() {
            let _ = taskman.kill();
            panic!("gh never started");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let pids: Vec<&str> = pids.split_whitespace().collect();
    assert!(pids.iter().all(|p| !ended(p)), "gh ended before the interrupt: {pids:?}");

    // As the terminal's ctrl+c: to taskman, whose helpers aren't in its process group.
    let pid = i32::try_from(taskman.id()).unwrap();
    // SAFETY: sends a signal; no memory is involved.
    unsafe { libc::kill(pid, libc::SIGINT) };
    let until = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = taskman.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > until {
            let _ = taskman.kill();
            panic!("taskman status didn't end on SIGINT");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Killed, the background sleep lingers only until its new parent reaps it.
    let until = Instant::now() + Duration::from_secs(5);
    while pids.iter().any(|p| !ended(p)) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    let alive: Vec<&str> = pids.iter().copied().filter(|p| !ended(p)).collect();
    for p in &alive {
        // So a failure leaves nothing running.
        let _ = std::process::Command::new("kill").args(["-9", p]).status();
    }
    assert!(alive.is_empty(), "{alive:?} outlived taskman status");
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
}
