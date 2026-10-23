//! A session runs as one copy (`resume::Hold`): restarting it in the pane keeps it claimed,
//! killing `taskman resume` while its CLI runs doesn't free it for a second copy, and a lock that
//! can't be taken stops the session starting at all.

mod common;

use common::{Tui, Watchdog, answer_queries, scratch_dir, spawn_in_pty, stub_cli, taskman};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

/// One project with ticket ENG-1 in `dir`, whose session `SID` has a transcript, run by a stub CLI
/// that writes its pid to `session.pid`. Returns the repo.
fn fixture(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    std::fs::write(repo.join("workplan/todo/ENG-1-test.md"), "# test\n").unwrap();
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [{"key": "ENG-1", "repo": repo, "cwd": repo, "session_id": SID}],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let transcripts = dir.join("claude-config/projects/acme");
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join(format!("{SID}.jsonl")), "{}\n").unwrap();
    repo
}

fn command(dir: &Path) -> Command {
    let mut cmd = taskman(dir);
    cmd.env("TASKMAN_CLAUDE", stub_cli(dir, &dir.join("session.pid")))
        .env("CLAUDE_CONFIG_DIR", dir.join("claude-config"));
    cmd
}

/// The pid the stub CLI last wrote.
fn session(dir: &Path) -> Option<i32> {
    std::fs::read_to_string(dir.join("session.pid")).ok()?.trim().parse().ok()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// ENG-1's owner file, once something has made it.
fn owner_file(dir: &Path) -> Option<PathBuf> {
    let status = dir.join("state/taskman/status");
    std::fs::read_dir(status)
        .ok()?
        .flatten()
        .map(|d| d.path().join("ENG-1.owner"))
        .find(|p| p.exists())
}

fn locked(path: &Path) -> bool {
    let f = std::fs::File::open(path).unwrap();
    // SAFETY: flock on a descriptor this function owns; closing it lets go.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 }
}

/// Waits up to `limit` for `done`, reading and answering taskman's output on `master` if given.
fn wait_for(
    limit: Duration,
    mut master: Option<&mut std::fs::File>,
    mut done: impl FnMut() -> bool,
) -> bool {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if done() {
            return true;
        }
        if let Some(m) = master.as_deref_mut() {
            let mut buf = [0u8; 65536];
            if let Ok(n) = m.read(&mut buf) {
                answer_queries(m, &buf[..n]);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

fn kill(pid: Option<i32>) {
    if let Some(pid) = pid.filter(|p| *p > 0) {
        // SAFETY: sends a signal; no memory is involved.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

#[test]
fn restart_starts_a_new_copy_that_owns_the_session() {
    let dir = scratch_dir("restart");
    fixture(&dir);
    let (mut tui, mut master) = spawn_in_pty(command(&dir));
    let pids = std::sync::Arc::new(std::sync::Mutex::new(vec![i32::try_from(tui.id()).unwrap()]));
    let (p, d) = (pids.clone(), dir.clone());
    let watchdog = Watchdog::start("restart", Duration::from_secs(60), move || {
        p.lock().unwrap().iter().for_each(|pid| kill(Some(*pid)));
        kill(session(&d));
    });

    watchdog.step("starting the session");
    assert!(
        wait_for(Duration::from_secs(10), Some(&mut master), || session(&dir).is_some()),
        "the session didn't start"
    );
    let first = session(&dir).unwrap();
    pids.lock().unwrap().push(first);

    watchdog.step("restarting it");
    master.write_all(b"\x01R").unwrap();
    let restarted = wait_for(Duration::from_secs(10), Some(&mut master), || {
        session(&dir).is_some_and(|p| p != first)
    });
    let second = session(&dir).unwrap();
    pids.lock().unwrap().push(second);
    let owner = owner_file(&dir).expect("no owner file");
    let (held, record) = (locked(&owner), std::fs::read_to_string(&owner).unwrap());

    watchdog.step("ending taskman");
    // Its terminal closed first: on macOS a process ending with output nobody reads waits for it
    // to drain, even killed.
    drop(master);
    let _ = tui.kill();
    let _ = tui.wait();
    kill(Some(first));
    kill(Some(second));
    drop(watchdog);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(restarted, "restarting stopped the session and started nothing");
    assert!(held, "the restarted session's owner file isn't locked");
    assert_eq!(record, format!("taskman {}", tui.id()));
}

#[test]
fn killing_resume_leaves_the_session_to_its_cli() {
    let dir = scratch_dir("resume-killed");
    let repo = fixture(&dir);
    let resume = || {
        let mut cmd = command(&dir);
        cmd.args(["resume", "ENG-1"]).current_dir(&repo).stdin(Stdio::null()).stdout(Stdio::null());
        cmd
    };
    let d = dir.clone();
    let watchdog =
        Watchdog::start("resume-killed", Duration::from_secs(60), move || kill(session(&d)));

    watchdog.step("resuming it");
    let mut first = resume().stderr(Stdio::null()).spawn().unwrap();
    assert!(
        wait_for(Duration::from_secs(10), None, || session(&dir).is_some()),
        "the session didn't start"
    );
    let cli = session(&dir).unwrap();
    // `taskman resume` names the CLI in the owner file once it has started it, which may be after
    // the CLI wrote its pid: killed before, it would leave its own pid there.
    let owner = owner_file(&dir).expect("no owner file");
    let named = wait_for(Duration::from_secs(10), None, || {
        std::fs::read_to_string(&owner).is_ok_and(|r| r == format!("outside {cli}"))
    });

    watchdog.step("killing taskman resume");
    first.kill().unwrap();
    first.wait().unwrap();
    assert!(alive(cli), "the CLI ended with taskman resume");

    watchdog.step("resuming it again");
    let mut second = resume().stderr(Stdio::piped()).spawn().unwrap();
    let ended = wait_for(Duration::from_secs(10), None, || second.try_wait().unwrap().is_some());
    let started_again = session(&dir).filter(|p| *p != cli);
    if !ended {
        let _ = second.kill();
        kill(started_again);
    }
    let second = second.wait_with_output().unwrap();
    let refused = String::from_utf8_lossy(&second.stderr).into_owned();

    watchdog.step("resuming it once the CLI ends");
    kill(Some(cli));
    // A killed process lets go of the claim as it ends, a moment after the signal.
    let freed = wait_for(Duration::from_secs(10), None, || !locked(&owner));
    let mut third = resume().stderr(Stdio::null()).spawn().unwrap();
    let resumed =
        wait_for(Duration::from_secs(10), None, || session(&dir).is_some_and(|p| p != cli));
    let _ = third.kill();
    let _ = third.wait();
    kill(session(&dir).filter(|p| *p != cli));
    drop(watchdog);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(named, "taskman resume didn't name its CLI in the owner file");
    assert_eq!(started_again, None, "a second copy started while the first CLI ran");
    assert_eq!(second.status.code(), Some(1), "{refused}");
    assert!(
        refused.contains(&format!("already running in another terminal (pid {cli})")),
        "{refused}"
    );
    assert!(freed && resumed, "the session stayed claimed after its CLI ended");
}

/// `taskman resume` hands the terminal to the CLI: a ctrl+c there is the CLI's, and resume waits on
/// for it rather than ending, while a SIGTERM still ends it, leaving the CLI the session.
#[test]
fn ctrl_c_in_a_resumed_session_is_its_clis() {
    use std::os::unix::process::ExitStatusExt;
    let dir = scratch_dir("resume-interrupt");
    let repo = fixture(&dir);
    let mut resume = command(&dir)
        .args(["resume", "ENG-1"])
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = i32::try_from(resume.id()).unwrap();
    let d = dir.clone();
    let watchdog = Watchdog::start("resume-interrupt", Duration::from_secs(60), move || {
        kill(Some(pid));
        kill(session(&d));
    });

    watchdog.step("resuming it");
    assert!(
        wait_for(Duration::from_secs(10), None, || session(&dir).is_some()),
        "the session didn't start"
    );
    let cli = session(&dir).unwrap();
    // Named once the CLI has started, after the hand-off.
    let owner = owner_file(&dir).expect("no owner file");
    let named = wait_for(Duration::from_secs(10), None, || {
        std::fs::read_to_string(&owner).is_ok_and(|r| r == format!("outside {cli}"))
    });

    watchdog.step("ctrl+c");
    // As the terminal sends it, to the process group in front; here to resume alone.
    // SAFETY: sends a signal; no memory is involved.
    unsafe { libc::kill(pid, libc::SIGINT) };
    // An interrupt that ended it would within a moment: stopping no helpers takes none.
    let interrupted =
        wait_for(Duration::from_secs(1), None, || resume.try_wait().unwrap().is_some());

    watchdog.step("SIGTERM");
    // SAFETY: sends a signal; no memory is involved.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let ended = wait_for(Duration::from_secs(10), None, || resume.try_wait().unwrap().is_some());
    let status = if ended { resume.wait().ok() } else { None };
    let cli_runs = alive(cli);
    kill(Some(pid));
    let _ = resume.wait();
    kill(Some(cli));
    drop(watchdog);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(named, "taskman resume didn't name its CLI in the owner file");
    assert!(!interrupted, "ctrl+c ended taskman resume while its CLI ran");
    assert_eq!(status.and_then(|s| s.signal()), Some(libc::SIGTERM), "{status:?}");
    assert!(cli_runs, "the CLI ended with taskman resume");
}

/// The owner lock fails closed: while its file can't be made (here a plain file sits where the
/// status folder goes), the session doesn't start and the pane says why; once it can, Enter starts
/// the session under the lock, and `taskman resume` is refused as usual.
#[test]
fn a_session_waits_until_its_lock_can_be_taken() {
    let dir = scratch_dir("lock-fails-closed");
    let repo = fixture(&dir);
    let status = dir.join("state/taskman/status");
    std::fs::write(&status, "").unwrap();
    let resume = || {
        let out = command(&dir)
            .args(["resume", "ENG-1"])
            .current_dir(&repo)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let mut tui = Tui::start("lock-fails-closed", &dir, command(&dir), Duration::from_secs(60));
    tui.wait_text("Couldn't take the lock that keeps ENG-1's session to one copy");
    tui.wait_text("Fix it, then press Enter to retry.");
    assert_eq!(session(&dir), None, "a CLI started without the lock");
    let (code, said) = resume();
    assert!(code == Some(1) && said.contains("couldn't open"), "{said}");

    // Fixed: Enter starts it, under the lock.
    std::fs::remove_file(&status).unwrap();
    tui.send(b"\r");
    tui.wait_until("the session didn't start", || session(&dir).is_some());
    let owner = owner_file(&dir).expect("no owner file");
    assert!(locked(&owner), "the session's owner file isn't locked");
    assert_eq!(std::fs::read_to_string(&owner).unwrap(), format!("taskman {}", tui.child.id()));
    let (code, said) = resume();
    assert!(
        code == Some(1) && said.contains("ENG-1 is running in taskman: press ctrl+a t there"),
        "{said}"
    );

    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}
