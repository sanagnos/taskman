//! `taskman close` against a running taskman, with real git worktrees: a merged one goes with its
//! branch, one with unpushed work or an ignored .env stays and the reply says why, and a request
//! made while taskman was closed applies when it starts. A ticket whose session runs in another
//! terminal closes neither from the command nor from the close dialog. A closed ticket can't come
//! back while its worktree is still being removed, and a worktree git finishes after its ticket
//! closed doesn't stay behind.

mod common;

use common::{
    Tui, Watchdog, acme, answer_queries, scratch_dir, sh, slow_add_git, slow_git, spawn_in_pty,
    stub_cli, taskman,
};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Reads taskman's terminal output until `stop`, answering its queries.
fn drain(mut master: std::fs::File, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while !stop.load(Ordering::Relaxed) {
            match master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => answer_queries(&mut master, &buf[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    })
}

/// Stops taskman, then the reader: on macOS a process can't finish exiting while output it wrote
/// to the terminal waits unread, so the reader drains it until taskman is gone. A second SIGTERM
/// ends it without waiting for git, which the test may be holding up.
fn stop_taskman(
    mut tui: std::process::Child,
    stop: &AtomicBool,
    reader: std::thread::JoinHandle<()>,
) {
    let pid = tui.id().cast_signed();
    let term = || {
        // SAFETY: sends a signal; no memory is involved.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    };
    term();
    let until = Instant::now() + Duration::from_secs(5);
    let mut again = Some(Instant::now() + Duration::from_millis(500));
    while tui.try_wait().unwrap().is_none() && Instant::now() < until {
        if again.is_some_and(|at| Instant::now() >= at) {
            again = None;
            term();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = tui.kill();
    let _ = tui.wait();
    stop.store(true, Ordering::Relaxed);
    let _ = reader.join();
}

#[test]
fn closes_tickets_and_removes_only_safe_worktrees() {
    // Real paths: git lists worktrees by them, and macOS's temp folder is behind a symlink.
    let dir = scratch_dir("close").canonicalize().unwrap();
    let repo = acme(&dir);
    let (wt1, wt2, wt6) = (dir.join("wt-ENG-1"), dir.join("wt-ENG-2"), dir.join("wt-ENG-6"));
    // ENG-1: pushed and merged into main. ENG-2: a commit that is on no remote.
    sh(
        &repo,
        "git worktree add -q -b ENG-1 ../wt-ENG-1 origin/main && cd ../wt-ENG-1 && git commit -q --allow-empty -m eng1 \
         && git push -q origin ENG-1 ENG-1:main && cd ../acme && git fetch -q",
    );
    sh(
        &repo,
        "git worktree add -q -b ENG-2 ../wt-ENG-2 origin/main && cd ../wt-ENG-2 && git commit -q --allow-empty -m eng2",
    );
    // ENG-6: clean and merged, so the removal check passes; its session runs in another terminal.
    sh(&repo, "git worktree add -q -b ENG-6 ../wt-ENG-6 origin/main");
    // ENG-7: pushed and merged, with nothing but an ignored .env, which `git status` doesn't show
    // and `git worktree remove` would delete.
    let wt7 = dir.join("wt-ENG-7");
    sh(
        &repo,
        "git worktree add -q -b ENG-7 ../wt-ENG-7 origin/main && cd ../wt-ENG-7 && echo .env > .gitignore \
         && git add .gitignore && git commit -q -m eng7 && git push -q origin ENG-7 ENG-7:main \
         && echo SECRET=1 > .env && cd ../acme && git fetch -q",
    );

    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let in_worktree = |key: &str, wt: &Path| serde_json::json!({"key": key, "repo": repo, "cwd": wt, "branch": key, "created_worktree": true});
    let in_checkout = |key: &str| serde_json::json!({"key": key, "repo": repo, "cwd": repo});
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [in_worktree("ENG-1", &wt1), in_worktree("ENG-2", &wt2), in_checkout("ENG-3"), in_checkout("ENG-5"), in_worktree("ENG-6", &wt6), in_worktree("ENG-7", &wt7)],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let open = || std::fs::read_to_string(state.join("tickets.json")).unwrap_or_default();
    let close = |args: &[&str]| -> Output {
        taskman(&dir).arg("close").args(args).current_dir(&repo).output().unwrap()
    };
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();

    // taskman isn't running: the request waits for it.
    let out = close(&["ENG-3"]);
    assert_eq!(text(&out.stdout), "taskman isn't running; ENG-3 closes when it starts.\n");

    let mut cmd = taskman(&dir);
    cmd.env("TASKMAN_CLAUDE", stub_cli(&dir, &dir.join("session.pid")));
    let mut tui = {
        let state = state.clone();
        Tui::start_with(
            "closes_tickets_and_removes_only_safe_worktrees",
            &dir,
            cmd,
            Duration::from_secs(90),
            move || {
                let inbox: Vec<_> = std::fs::read_dir(state.join("inbox"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.file_name())
                    .collect();
                let tickets =
                    std::fs::read_to_string(state.join("tickets.json")).unwrap_or_default();
                let _ = writeln!(std::io::stderr(), "inbox: {inbox:?}\ntickets.json: {tickets}");
            },
        )
    };
    tui.wait_until("the queued close didn't apply at start", || {
        !open().contains("ENG-3") && state.join("inbox/alive").exists()
    });

    tui.step("close ENG-1 ENG-2 4");
    let out = close(&["ENG-1", "ENG-2", "4"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{stdout}{stderr}");
    assert_eq!(lines[0], "ENG-4 isn't open in acme");
    assert_eq!(lines[1], format!("Closed ENG-1. Removed {} and branch ENG-1", wt1.display()));
    assert_eq!(lines[2], format!("Closed ENG-2. Kept {}: it has 1 unpushed commit", wt2.display()));
    assert!(out.status.success(), "{stderr}");
    assert!(!wt1.exists() && wt2.exists());
    let branches =
        Command::new("git").args(["branch", "--list"]).current_dir(&repo).output().unwrap();
    assert!(!text(&branches.stdout).contains("ENG-1") && text(&branches.stdout).contains("ENG-2"));
    assert!(!open().contains("ENG-1") && !open().contains("ENG-2"));

    // Ignored files keep the worktree: only the close dialog offers to delete them.
    tui.step("close ENG-7");
    let out = close(&["ENG-7"]);
    assert_eq!(
        text(&out.stdout),
        format!("Closed ENG-7. Kept {}: it has ignored files: .env\n", wt7.display())
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read_to_string(wt7.join(".env")).unwrap(), "SECRET=1\n");
    assert!(!open().contains("ENG-7"));

    // --merged leaves a ticket whose PR it can't look up (this origin isn't on GitHub).
    tui.step("close --merged ENG-5");
    let out = close(&["--merged", "ENG-5"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("couldn't look up ENG-5's PR, so it stays open"),
        "{}",
        text(&out.stderr)
    );
    assert!(open().contains("ENG-5"));

    // ENG-6 runs in another terminal: `taskman resume` holds its owner file, beside the status
    // file of the session taskman started for ENG-1.
    tui.step("lock ENG-6's owner file");
    let project_status =
        std::fs::read_dir(state.join("status")).unwrap().flatten().next().unwrap().path();
    let owner = std::fs::File::create(project_status.join("ENG-6.owner")).unwrap();
    // SAFETY: flock on a descriptor this test owns; it reads or writes no memory.
    assert_eq!(unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    (&owner).write_all(b"outside 4242").unwrap();
    tui.step("close 6 while it runs elsewhere");
    let out = close(&["6"]);
    assert_eq!(
        text(&out.stderr),
        "taskman close: ENG-6 is running in another terminal; quit it there first\n"
    );
    assert!(!out.status.success());
    // The close dialog doesn't open for it, so r closes and removes nothing.
    let selected =
        || serde_json::from_str::<serde_json::Value>(&open()).unwrap()["selected"].as_u64();
    tui.step("select ENG-6");
    tui.send(b"\x01]");
    tui.wait_until("ENG-6 wasn't selected", || selected() == Some(1));
    tui.step("ctrl+a x, r on ENG-6");
    tui.send(b"\x01x");
    tui.wait_text("ENG-6 is running in another terminal; quit it there first");
    tui.send(b"r");
    // Keys are taken in order: once the quit dialog is up, the r has been taken too.
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\x1b");
    assert!(
        open().contains("ENG-6") && wt6.exists(),
        "the dialog closed a ticket running elsewhere"
    );
    // Back from the other terminal, it closes.
    tui.step("close 6 once it's back");
    drop(owner);
    let out = close(&["6"]);
    assert_eq!(
        text(&out.stdout),
        format!("Closed ENG-6. Removed {} and branch ENG-6\n", wt6.display())
    );

    tui.step("close 5 --keep-worktree");
    let out = close(&["5", "--keep-worktree"]);
    assert_eq!(text(&out.stdout), "Closed ENG-5\n");

    tui.step("quit");
    tui.wait_for("the quit dialog closed", |s| {
        common::find(s, "Quit taskman?").is_none().then_some(())
    });
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

/// A race found in review: a ticket closed while its worktree removal is slow, then added
/// again. Its new session would run in the folder the removal is about to delete; instead adding
/// it is refused until the removal is done. Through `taskman close` and the close dialog's `r`.
#[test]
fn a_ticket_comes_back_only_after_its_worktree_is_removed() {
    let dir = scratch_dir("close-race").canonicalize().unwrap();
    let repo = acme(&dir);
    let (wt7, wt8) = (dir.join("wt-ENG-7"), dir.join("wt-ENG-8"));
    sh(
        &repo,
        "git worktree add -q -b ENG-7 ../wt-ENG-7 origin/main && git worktree add -q -b ENG-8 ../wt-ENG-8 origin/main",
    );
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    for key in ["ENG-7", "ENG-8"] {
        std::fs::write(repo.join(format!("workplan/todo/{key}-race.md")), "# race\n").unwrap();
    }
    slow_git(&dir);

    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let in_worktree = |key: &str, wt: &Path| serde_json::json!({"key": key, "repo": repo, "cwd": wt, "branch": key, "created_worktree": true});
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [in_worktree("ENG-7", &wt7), in_worktree("ENG-8", &wt8)],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let open = || std::fs::read_to_string(state.join("tickets.json")).unwrap_or_default();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let add = |key: &str| -> (bool, String) {
        let out =
            taskman(&dir).args(["add", key, "--no-worktree"]).current_dir(&repo).output().unwrap();
        (out.status.success(), text(&out.stdout) + &text(&out.stderr))
    };
    let wait_for = |what: &str, done: &dyn Fn() -> bool| {
        let until = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < until, "{what}: {}", open());
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    let session_pid = dir.join("session.pid");
    let mut cmd = taskman(&dir);
    cmd.env("TASKMAN_CLAUDE", stub_cli(&dir, &session_pid));
    let release = dir.clone();
    let mut tui = Tui::start_with(
        "a_ticket_comes_back_only_after_its_worktree_is_removed",
        &dir,
        cmd,
        Duration::from_secs(90),
        move || {
            let _ = std::fs::write(release.join("release-wt-ENG-7"), "");
            let _ = std::fs::write(release.join("release-wt-ENG-8"), "");
        },
    );
    tui.step("ENG-7's session starts in its worktree");
    wait_for("ENG-7's session didn't start", &|| {
        session_pid.exists() && state.join("inbox/alive").exists()
    });

    tui.step("close 7, its removal held up");
    let closer = taskman(&dir)
        .args(["close", "7"])
        .current_dir(&repo)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for("the removal never reached git worktree remove", &|| {
        dir.join("removing-wt-ENG-7").exists()
    });
    assert!(!open().contains("ENG-7"), "{}", open());
    tui.step("add ENG-7 during the removal");
    let (ok, said) = add("ENG-7");
    assert!(
        !ok && said.contains("ENG-7's worktree is still being removed; try again in a moment"),
        "{said}"
    );
    assert!(!open().contains("ENG-7"), "added during the removal: {}", open());
    tui.step("let ENG-7's removal finish");
    std::fs::write(dir.join("release-wt-ENG-7"), "").unwrap();
    let out = closer.wait_with_output().unwrap();
    assert_eq!(
        text(&out.stdout),
        format!("Closed ENG-7. Removed {} and branch ENG-7\n", wt7.display())
    );
    assert!(!wt7.exists());
    tui.step("add ENG-7 once it's removed");
    let (ok, said) = add("ENG-7");
    assert!(ok && open().contains("ENG-7"), "{said}");

    // ENG-8, selected now that ENG-7 left the top of the list, through the close dialog.
    tui.step("ctrl+a x, r on ENG-8");
    tui.send(b"\x01x");
    tui.wait_text("Close ENG-8?");
    tui.wait_text("r close and remove");
    tui.send(b"r");
    wait_for("the dialog's removal never reached git worktree remove", &|| {
        dir.join("removing-wt-ENG-8").exists()
    });
    assert!(!open().contains("ENG-8"), "{}", open());
    tui.step("add ENG-8 during the removal");
    let (ok, said) = add("ENG-8");
    assert!(
        !ok && said.contains("ENG-8's worktree is still being removed; try again in a moment"),
        "{said}"
    );
    tui.step("let ENG-8's removal finish");
    std::fs::write(dir.join("release-wt-ENG-8"), "").unwrap();
    wait_for("ENG-8's worktree wasn't removed", &|| !wt8.exists());
    tui.step("add ENG-8 once it's removed");
    let added = || add("ENG-8");
    let until = Instant::now() + Duration::from_secs(5);
    // The removal thread lets go just after git returns.
    let (ok, said) = loop {
        let (ok, said) = added();
        if ok || Instant::now() > until {
            break (ok, said);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(ok && open().contains("ENG-8"), "{said}");

    tui.step("quit");
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

/// The reservation lasts as long as git removes the worktree, not as long as taskman runs.
/// Killed mid-removal, a taskman started again still refuses the ticket until git is done; a
/// normal quit waits for the removal and its reply.
#[test]
fn a_removal_keeps_the_ticket_out_after_taskman_ends() {
    let dir = scratch_dir("close-kill").canonicalize().unwrap();
    let repo = acme(&dir);
    let (wt9, wt10) = (dir.join("wt-ENG-9"), dir.join("wt-ENG-10"));
    sh(
        &repo,
        "git worktree add -q -b ENG-9 ../wt-ENG-9 origin/main && git worktree add -q -b ENG-10 ../wt-ENG-10 origin/main",
    );
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    for key in ["ENG-9", "ENG-10"] {
        std::fs::write(repo.join(format!("workplan/todo/{key}-kill.md")), "# kill\n").unwrap();
    }
    slow_git(&dir);

    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let in_worktree = |key: &str, wt: &Path| serde_json::json!({"key": key, "repo": repo, "cwd": wt, "branch": key, "created_worktree": true});
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [in_worktree("ENG-9", &wt9), in_worktree("ENG-10", &wt10)],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let saved = || {
        serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(state.join("tickets.json")).unwrap(),
        )
        .unwrap()
    };
    let open = || std::fs::read_to_string(state.join("tickets.json")).unwrap_or_default();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let add = |key: &str| -> (bool, String) {
        let out =
            taskman(&dir).args(["add", key, "--no-worktree"]).current_dir(&repo).output().unwrap();
        (out.status.success(), text(&out.stdout) + &text(&out.stderr))
    };
    let wait_for = |what: &str, done: &dyn Fn() -> bool| {
        let until = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < until, "{what}: {}", open());
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let session_pid = dir.join("session.pid");
    let command = || {
        let mut cmd = taskman(&dir);
        cmd.env("TASKMAN_CLAUDE", stub_cli(&dir, &session_pid));
        cmd
    };
    let kill_sessions = {
        let session_pid = session_pid.clone();
        move || {
            let pid = std::fs::read_to_string(&session_pid)
                .ok()
                .and_then(|p| p.trim().parse::<i32>().ok());
            if let Some(pid) = pid {
                // SAFETY: sends a signal; no memory is involved.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    };
    // The first taskman takes no keys and ends by a signal, and the second needs the folder it
    // leaves: a `Tui` would remove it as it's dropped.
    let (tui, master) = spawn_in_pty(command());
    let stop = Arc::new(AtomicBool::new(false));
    let reader = drain(master, stop.clone());
    let watchdog = {
        let (pid, dir, kill_sessions) =
            (tui.id().cast_signed(), dir.clone(), kill_sessions.clone());
        Watchdog::start(
            "a_removal_keeps_the_ticket_out_after_taskman_ends",
            Duration::from_secs(120),
            move || {
                let _ = std::fs::write(dir.join("release-wt-ENG-9"), "");
                let _ = std::fs::write(dir.join("release-wt-ENG-10"), "");
                // SAFETY: sends a signal; no memory is involved.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                kill_sessions();
                let _ = std::fs::remove_dir_all(dir);
            },
        )
    };
    watchdog.step("ENG-9's session starts in its worktree");
    wait_for("ENG-9's session didn't start", &|| {
        session_pid.exists() && state.join("inbox/alive").exists()
    });

    watchdog.step("close 9, its removal held up");
    let mut closer = taskman(&dir)
        .args(["close", "9"])
        .current_dir(&repo)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    wait_for("the removal never reached git worktree remove", &|| {
        dir.join("removing-wt-ENG-9").exists()
    });
    assert!(!open().contains("ENG-9"), "{}", open());

    // taskman ends while git still removes the worktree: the reservation must outlive it.
    watchdog.step("stop taskman during the removal");
    stop_taskman(tui, &stop, reader);
    let _ = closer.kill();
    let _ = closer.wait();
    assert!(wt9.exists(), "the removal went on before it was released");
    drop(watchdog);

    let release = dir.clone();
    let mut tui = Tui::start_with(
        "a_removal_keeps_the_ticket_out_after_taskman_ends",
        &dir,
        command(),
        Duration::from_secs(120),
        move || {
            let _ = std::fs::write(release.join("release-wt-ENG-9"), "");
            let _ = std::fs::write(release.join("release-wt-ENG-10"), "");
        },
    );
    tui.step("start taskman again");
    wait_for("taskman didn't start again", &|| state.join("inbox/alive").exists());
    tui.step("add ENG-9 while the old removal runs");
    let (ok, said) = add("ENG-9");
    assert!(
        !ok && said.contains("ENG-9's worktree is still being removed; try again in a moment"),
        "{said}"
    );
    assert!(!open().contains("ENG-9"), "added during the removal: {}", open());

    tui.step("let ENG-9's removal finish");
    std::fs::write(dir.join("release-wt-ENG-9"), "").unwrap();
    wait_for("ENG-9's worktree wasn't removed", &|| !wt9.exists());
    tui.step("add ENG-9 once it's removed");
    let until = Instant::now() + Duration::from_secs(5);
    // git exits just after it deletes the folder, letting the lock go.
    let (ok, said) = loop {
        let (ok, said) = add("ENG-9");
        if ok || Instant::now() > until {
            break (ok, said);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(ok, "{said}");
    let eng9 = saved()["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "ENG-9")
        .cloned()
        .unwrap();
    assert_eq!(eng9["cwd"].as_str().map(Path::new), Some(repo.as_path()), "{eng9}");

    // A normal quit during a removal waits for it, so the branch goes and the reply comes.
    tui.step("close 10, its removal held up");
    let closer = taskman(&dir)
        .args(["close", "10"])
        .current_dir(&repo)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for("the removal never reached git worktree remove", &|| {
        dir.join("removing-wt-ENG-10").exists()
    });
    tui.step("quit during the removal");
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    tui.wait_text("Quitting once ENG-10's worktree is removed");
    assert!(tui.child.try_wait().unwrap().is_none(), "taskman quit before the removal finished");
    let (ok, said) = add("ENG-10");
    assert!(!ok && said.contains("ENG-10's worktree is still being removed"), "{said}");
    tui.step("let ENG-10's removal finish");
    std::fs::write(dir.join("release-wt-ENG-10"), "").unwrap();
    let out = closer.wait_with_output().unwrap();
    assert_eq!(
        text(&out.stdout),
        format!("Closed ENG-10. Removed {} and branch ENG-10\n", wt10.display())
    );
    tui.step("taskman quits once the removal is done");
    assert!(tui.wait_exit().success());
}

/// Project `acme` in `dir` with an origin, Local files tickets `keys` and none open, and taskman
/// started on it with git held at `worktree add` (`slow_add_git`). Returns the repo and taskman.
fn held_at_add(dir: &Path, name: &'static str, keys: &[&str]) -> (std::path::PathBuf, Tui) {
    let repo = acme(dir);
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    for key in keys {
        std::fs::write(repo.join(format!("workplan/todo/{key}-late.md")), "# late\n").unwrap();
    }
    slow_add_git(dir);
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let mut cmd = taskman(dir);
    cmd.env("TASKMAN_CLAUDE", stub_cli(dir, &dir.join("session.pid")));
    let tui = Tui::start(name, dir, cmd, Duration::from_secs(90));
    tui.wait_until("taskman didn't start", || state.join("inbox/alive").exists());
    (repo, tui)
}

fn branches(repo: &Path) -> String {
    let out = Command::new("git").args(["branch", "--list"]).current_dir(repo).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A ticket closed while git still creates its worktree: once git is done the worktree goes, as
/// nothing ran in it, unless the close asked to keep it.
#[test]
fn a_worktree_created_after_its_ticket_closed_goes() {
    let dir = scratch_dir("close-creating").canonicalize().unwrap();
    let (repo, tui) =
        held_at_add(&dir, "a_worktree_created_after_its_ticket_closed_goes", &["ENG-11", "ENG-12"]);
    let (wt11, wt12) = (dir.join("acme-ENG-11"), dir.join("acme-ENG-12"));
    let open =
        || std::fs::read_to_string(dir.join("state/taskman/tickets.json")).unwrap_or_default();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let run = |args: &[&str]| -> String {
        let out = taskman(&dir).args(args).current_dir(&repo).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}{}", text(&out.stdout), text(&out.stderr));
        text(&out.stdout)
    };
    let gone = "closed while its worktree was being created";

    // Closed while git creates its worktree: once git is done, the worktree goes with its branch.
    run(&["add", "ENG-11"]);
    tui.wait_until("git never reached worktree add for ENG-11", || {
        dir.join("adding-acme-ENG-11").exists()
    });
    assert_eq!(run(&["close", "ENG-11"]), "Closed ENG-11\n");
    assert!(!open().contains("ENG-11"), "{}", open());
    std::fs::write(dir.join("release-acme-ENG-11"), "").unwrap();
    tui.wait_text(&format!("ENG-11 {gone}. Removed"));
    assert!(
        dir.join("added-acme-ENG-11").exists()
            && !wt11.exists()
            && !branches(&repo).contains("ENG-11")
    );

    // --keep-worktree keeps it.
    run(&["add", "ENG-12"]);
    tui.wait_until("git never reached worktree add for ENG-12", || {
        dir.join("adding-acme-ENG-12").exists()
    });
    assert_eq!(run(&["close", "ENG-12", "--keep-worktree"]), "Closed ENG-12\n");
    std::fs::write(dir.join("release-acme-ENG-12"), "").unwrap();
    tui.wait_text(&format!("ENG-12 {gone}. Kept"));
    assert!(wt12.exists() && branches(&repo).contains("ENG-12"), "removed despite --keep-worktree");
}

/// The same from the close dialog, with a quit right after: taskman waits for git, then for the
/// removal, before it ends.
#[test]
fn a_quit_waits_for_a_closed_tickets_worktree() {
    let dir = scratch_dir("close-creating-quit").canonicalize().unwrap();
    let (repo, mut tui) =
        held_at_add(&dir, "a_quit_waits_for_a_closed_tickets_worktree", &["ENG-13"]);
    let wt13 = dir.join("acme-ENG-13");
    let open =
        || std::fs::read_to_string(dir.join("state/taskman/tickets.json")).unwrap_or_default();
    let added = taskman(&dir).args(["add", "ENG-13"]).current_dir(&repo).output().unwrap();
    assert!(added.status.success());
    tui.wait_until("git never reached worktree add", || dir.join("adding-acme-ENG-13").exists());

    // The only ticket, so the selected one.
    tui.send(b"\x01x");
    tui.wait_text("Close ENG-13?");
    tui.send(b"\r");
    tui.wait_until("ENG-13 wasn't closed", || !open().contains("ENG-13"));
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    tui.wait_text("Quitting once ENG-13's worktree is removed");
    assert!(tui.child.try_wait().unwrap().is_none(), "ended before git did");

    std::fs::write(dir.join("release-acme-ENG-13"), "").unwrap();
    assert!(tui.wait_exit().success());
    assert!(
        dir.join("added-acme-ENG-13").exists()
            && !wt13.exists()
            && !branches(&repo).contains("ENG-13")
    );
}
