//! Running other programs (git, gh, token commands, the updater's tar) from the TUI's threads and
//! commands: with no terminal to prompt on, and a time limit, so a program waiting for input that
//! never comes, or a network that never answers, can't hold a ticket's refreshes, a form or an
//! update forever. What's still running when taskman quits is stopped with it ([`stop_all`]).

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

/// Runs `cmd` to the end and returns its output, like `Command::output`, except that it:
/// - reads stdin from /dev/null and runs in a session of its own ([`own_session`]), so it can't
///   reach the terminal the TUI owns;
/// - has git, gh and ssh fail rather than prompt for credentials;
/// - gives up after `timeout`, stopping the whole group, with an error of kind `TimedOut`;
/// - is stopped, with its group, if taskman quits first ([`stop_all`]).
///
/// Failing to start keeps `Command::spawn`'s error, so `NotFound` still says it isn't installed.
pub fn run(cmd: &mut Command, timeout: Duration) -> io::Result<Output> {
    run_inner(cmd, None, timeout, true)
}

/// [`run`], with `input` on stdin instead of /dev/null. The time limit covers writing it too: a
/// program that never reads it is stopped at the limit like any other.
pub fn run_with_input(cmd: &mut Command, input: Vec<u8>, timeout: Duration) -> io::Result<Output> {
    run_inner(cmd, Some(input), timeout, true)
}

/// [`run`] for a git that changes worktrees (`worktree add`, `worktree remove`), which the quit
/// waits for and [`stop_all`] leaves to finish if it hasn't: stopped part-way, git leaves a
/// worktree half-made or half-removed.
pub fn run_past_quit(cmd: &mut Command, timeout: Duration) -> io::Result<Output> {
    run_inner(cmd, None, timeout, false)
}

/// The process groups [`run`] started that haven't been reaped, by id, for [`stop_all`].
struct Live {
    groups: Vec<libc::pid_t>,
    /// Helpers being started, not yet in `groups` (`Starting`), and those that finished starting
    /// once `stop_all` had run, until they're reaped: `stop_all` waits for them.
    starting: usize,
    /// `stop_all` has run: nothing starts from now on.
    stopped: bool,
}

static LIVE: Mutex<Live> = Mutex::new(Live { groups: Vec::new(), starting: 0, stopped: false });

/// The live groups. A child is reaped, and its id leaves them, under this lock, so `stop_all`
/// never signals an id that has passed to another group meanwhile.
fn live() -> MutexGuard<'static, Live> {
    LIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// How long [`stop_all`] gives the groups it asked to end before it kills them.
const STOP_GRACE: Duration = Duration::from_millis(250);

/// How long [`stop_all`] waits for helpers still starting: forked, but not yet running the
/// program, so there's nothing to signal. Starting takes milliseconds; this only bounds an exec
/// that hangs, so the watchdog still ends taskman.
const START_WAIT: Duration = Duration::from_secs(2);

/// A helper [`run`] is starting (`Live::starting`), until dropped.
struct Starting;

impl Drop for Starting {
    fn drop(&mut self) {
        live().starting -= 1;
    }
}

/// Stops every group [`run`] started that is still running, and starts none from now on: for
/// taskman quitting, so a gh waiting on a locked keyring, say, doesn't outlive it. Each group is
/// asked to end (SIGTERM) and given [`STOP_GRACE`] before it's killed: git removes its lock files
/// on SIGTERM, but killed, a `git status` refreshing the index or a `git fetch` updating refs
/// leaves its `.lock` behind, and the next commit there fails. A helper still starting is waited
/// for (up to [`START_WAIT`]): between fork and exec it can't be signalled, and with taskman gone
/// it would run on with nothing to stop it.
pub fn stop_all() {
    let mut live = live();
    live.stopped = true;
    for &group in &live.groups {
        signal_group(group, libc::SIGTERM);
    }
    // A group ends when `run` reaps it: its pipes close once all of it has ended. Still in the
    // list, it hasn't been reaped, so its id is still its own to kill. One that finishes starting
    // meanwhile asks itself to end, and is killed at its own grace (`run_inner`).
    let begun = Instant::now();
    let waiting = |live: &Live| {
        let now = begun.elapsed();
        !live.groups.is_empty() && now < STOP_GRACE || live.starting > 0 && now < START_WAIT
    };
    while waiting(&live) {
        drop(live);
        std::thread::sleep(Duration::from_millis(5));
        live = self::live();
    }
    for group in live.groups.drain(..) {
        signal_group(group, libc::SIGKILL);
    }
}

fn run_inner(
    cmd: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    stopped_at_quit: bool,
) -> io::Result<Output> {
    let mut deadline = Instant::now() + timeout;
    // Without a terminal, ssh asks with an askpass program instead. Kept for a user who chose one
    // (some desktops set SSH_ASKPASS); otherwise ssh tries a built-in default, a bare X window.
    if ["SSH_ASKPASS", "SSH_ASKPASS_REQUIRE"].iter().all(|v| std::env::var_os(v).is_none()) {
        cmd.env("SSH_ASKPASS_REQUIRE", "never");
    }
    let starting = if stopped_at_quit {
        let mut live = live();
        if live.stopped {
            return Err(io::Error::other("taskman is quitting"));
        }
        live.starting += 1;
        Some(Starting)
    } else {
        None
    };
    let mut child = own_session(cmd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Held until it's reaped when `stop_all` ran while it started: asked to end as the rest
    // were, and killed at its own limit, cut to the same grace, while `stop_all` waits.
    let _late = match starting {
        Some(starting) => {
            let mut live = live();
            let group = group_of(&child);
            if live.stopped {
                if let Some(g) = group {
                    signal_group(g, libc::SIGTERM);
                }
                deadline = deadline.min(Instant::now() + STOP_GRACE);
                Some(starting)
            } else {
                live.groups.extend(group);
                drop(live);
                drop(starting);
                None
            }
        }
        None => None,
    };
    // Written from its own thread, so the wait below keeps the time: stopping the group ends a
    // write that's stuck, and closing stdin when it's done is the program's end of input.
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    // Read both pipes at once, so a program filling one while we wait on the other can't stall.
    let (tx, rx) = mpsc::channel();
    let read = |pipe: Option<Box<dyn Read + Send>>, err: bool| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send((err, buf));
        });
    };
    read(child.stdout.take().map(|p| Box::new(p) as _), false);
    read(child.stderr.take().map(|p| Box::new(p) as _), true);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    // The pipes close when the program (and anything it started) exits.
    for _ in 0..2 {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((false, buf)) => stdout = buf,
            Ok((true, buf)) => stderr = buf,
            Err(_) => return Err(stop(&mut child, timeout)),
        }
    }
    let mut pause = Duration::from_millis(1);
    loop {
        if let Some(status) = reap(&mut child)? {
            return Ok(Output { status, stdout, stderr });
        }
        if Instant::now() >= deadline {
            return Err(stop(&mut child, timeout));
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(50));
    }
}

/// Starts `cmd` in a session of its own, which has no controlling terminal: opening /dev/tty
/// fails, so a program that asks there (ssh for a passphrase or a new host, gpg with pinentry-tty)
/// fails at once instead of writing over the screen and spinning on input the TUI reads. A process
/// group alone isn't enough: it stays in taskman's session, and raw mode leaves TOSTOP off. The
/// child also leads a process group, whose id is its pid, so [`stop`] can end all it started. The
/// `pre_exec` makes std fork rather than posix_spawn, a cost design.md weighs.
pub fn own_session(cmd: &mut Command) -> &mut Command {
    // SAFETY: the closure runs in the child between fork and exec; it allocates nothing and calls
    // only setsid, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })
    }
}

/// `child.try_wait()`, its group leaving the live ones once it has ended.
fn reap(child: &mut Child) -> io::Result<Option<ExitStatus>> {
    let mut live = live();
    let status = child.try_wait();
    if !matches!(status, Ok(None)) {
        let group = group_of(child);
        live.groups.retain(|g| Some(*g) != group);
    }
    status
}

/// The id of the group `child` leads (`own_session`): its pid. The guard keeps a negated 0, 1 or
/// our own group from signalling anything else.
fn group_of(child: &Child) -> Option<libc::pid_t> {
    // SAFETY: getpgrp takes nothing and can't fail; it reads or writes no memory of ours.
    let ours = unsafe { libc::getpgrp() };
    libc::pid_t::try_from(child.id()).ok().filter(|&g| g > 1 && g != ours)
}

/// Sends `signal` to a whole group. A system call, not the `kill` program, which minimal images
/// (debian:*-slim) don't have.
fn signal_group(group: libc::pid_t, signal: libc::c_int) {
    // SAFETY: kill only sends a signal; it reads or writes no memory.
    unsafe { libc::kill(-group, signal) };
}

/// Stops `child`'s whole process group (a hung `git fetch` waits on ssh, say) and says why.
fn stop(child: &mut Child, timeout: Duration) -> io::Error {
    // The child isn't reaped until the wait below, so its group's id can't pass to another group
    // meanwhile; it leaves the live ones first, so `stop_all` doesn't signal it after.
    if let Some(group) = group_of(child) {
        live().groups.retain(|g| *g != group);
        signal_group(group, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("it didn't finish within {}s", timeout.as_secs()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, timeout: Duration) -> io::Result<Output> {
        run(Command::new("sh").args(["-c", script]), timeout)
    }

    #[test]
    fn returns_output_and_status() {
        let out = sh("echo out; echo err >&2; exit 3", Duration::from_secs(10)).unwrap();
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn stdin_is_empty_and_prompts_are_off() {
        // `cat` would wait on a terminal's stdin forever.
        let out = sh("cat; echo $GIT_TERMINAL_PROMPT$GH_PROMPT_DISABLED", Duration::from_secs(10))
            .unwrap();
        assert_eq!(out.stdout, b"01\n");
    }

    /// Re-run in a terminal of its own, as the test harness has none: there, the test can open
    /// /dev/tty and a program `run` starts can't.
    #[test]
    fn a_program_cannot_reach_the_terminal() {
        if std::env::var_os("TASKMAN_PROC_IN_PTY").is_some() {
            let ours = std::fs::OpenOptions::new().write(true).open("/dev/tty").is_ok();
            let probe = "test -e /dev/tty && : > /dev/tty && echo reachable";
            let out = sh(probe, Duration::from_secs(10)).unwrap();
            println!("ours={ours} theirs={}.", String::from_utf8_lossy(&out.stdout).trim());
            return;
        }
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let (mut m, mut s) = (-1, -1);
        // SAFETY: openpty writes two descriptors into `m` and `s`; the null name, termios and
        // window size are optional.
        let r = unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(r, 0, "openpty: {}", io::Error::last_os_error());
        // SAFETY: openpty succeeded, so both are open descriptors that nothing else owns.
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        // SAFETY: sets a flag on a descriptor this test owns; it reads or writes no memory.
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "proc::tests::a_program_cannot_reach_the_terminal", "--nocapture"])
            .env("TASKMAN_PROC_IN_PTY", "1")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        // SAFETY: the closure runs in the child between fork and exec; it allocates nothing and
        // calls only setsid and ioctl, which are async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                #[cfg_attr(
                    target_os = "macos",
                    expect(
                        clippy::cast_lossless,
                        reason = "TIOCSCTTY is a u32 on macOS and ioctl's request type on Linux, \
                                  where From would be a useless conversion"
                    )
                )]
                let request = libc::TIOCSCTTY as _;
                if libc::setsid() < 0 || libc::ioctl(0, request, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().unwrap();
        // Our copies of the terminal's end closed, reading stops when the test's do (EIO).
        drop(cmd);
        let mut out = Vec::new();
        let _ = std::fs::File::from(master).read_to_end(&mut out);
        assert!(child.wait().unwrap().success());
        let out = String::from_utf8_lossy(&out);
        assert!(out.contains("ours=true theirs=."), "{out}");
    }

    #[test]
    fn gives_up_and_stops_what_it_started() {
        let dir = std::env::temp_dir().join(format!("taskman-proc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pids = dir.join("pids");
        let start = Instant::now();
        // The background sleep is the shell's child, as ssh is git's, and holds the pipes open.
        let script = format!("sleep 30 & echo $$ $! > '{}'; sleep 30", pids.display());
        let err = sh(&script, Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
        let pids: Vec<libc::pid_t> = std::fs::read_to_string(&pids)
            .unwrap()
            .split_whitespace()
            .map(|p| p.parse().unwrap())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(pids.len(), 2);
        // Killed, the background sleep lingers only until its new parent reaps it.
        let gone = Instant::now() + Duration::from_secs(5);
        for pid in pids {
            // SAFETY: signal 0 sends nothing, only checks the process is there.
            while unsafe { libc::kill(pid, 0) } == 0 {
                assert!(Instant::now() < gone, "{pid} is still running");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn a_descendant_holding_the_output_is_stopped_at_the_limit() {
        let start = Instant::now();
        // The shell is done at once, but what it left in the background still holds stdout.
        let err = sh("sleep 10 & echo ready", Duration::from_millis(300)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(3), "took {:?}", start.elapsed());
    }

    #[test]
    fn writes_all_the_input_to_a_slow_reader() {
        // More than a pipe holds, so the write has to wait for the reader.
        let input: Vec<u8> = (0..1u32 << 20).map(|i| (i % 251) as u8).collect();
        let cmd = &mut Command::new("sh");
        let out = run_with_input(
            cmd.args(["-c", "sleep 0.3; cat"]),
            input.clone(),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(out.status.success());
        assert!(out.stdout == input, "got {} of {} bytes", out.stdout.len(), input.len());
    }

    #[test]
    fn input_nobody_reads_is_stopped_at_the_limit() {
        let start = Instant::now();
        let cmd = &mut Command::new("sh");
        let err = run_with_input(
            cmd.args(["-c", "sleep 10"]),
            vec![b'x'; 1 << 20],
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(3), "took {:?}", start.elapsed());
    }

    /// Waits for `pid` to end; false if it's still running after 5s.
    fn ends(pid: libc::pid_t) -> bool {
        let gone = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 sends nothing, only checks the process is there.
        while unsafe { libc::kill(pid, 0) } == 0 {
            if Instant::now() >= gone {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// Where a test that calls `stop_all` runs it: in a process of its own (`alone`).
    const ALONE: &str = "TASKMAN_PROC_STOP_ALL";

    /// Runs `test` again in a process of its own, as `stop_all` stops every other test's programs
    /// too, with a scratch folder in `ALONE`, and returns what it printed.
    fn alone(test: &str) -> String {
        // One each: the tests that call it run at once.
        let name = test.rsplit("::").next().unwrap_or(test);
        let dir = std::env::temp_dir().join(format!("taskman-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(ALONE, &dir)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The pids `sh` wrote to `file`, once there are `n`.
    fn pids(file: &std::path::Path, n: usize) -> Vec<libc::pid_t> {
        let started = Instant::now() + Duration::from_secs(5);
        loop {
            let pids: Vec<libc::pid_t> = std::fs::read_to_string(file)
                .unwrap_or_default()
                .split_whitespace()
                .map(|p| p.parse().unwrap())
                .collect();
            if pids.len() >= n {
                return pids;
            }
            assert!(Instant::now() < started, "{} never had {n} pids", file.display());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn stop_all_ends_what_is_running_except_worktree_changes() {
        let Some(dir) = std::env::var_os(ALONE).map(std::path::PathBuf::from) else {
            let out = alone("proc::tests::stop_all_ends_what_is_running_except_worktree_changes");
            assert!(out.contains("stopped=true kept=true later=true"), "{out}");
            return;
        };
        let (pids_at, kept) = (dir.join("pids"), dir.join("kept"));
        // A helper with a child of its own, as gh's credential helper is, and a worktree change.
        let script = format!("sleep 60 & echo $$ $! > '{}'; sleep 60", pids_at.display());
        let helper = std::thread::spawn(move || sh(&script, Duration::from_secs(60)));
        let script = format!("sleep 0.5; echo done > '{}'", kept.display());
        let change = std::thread::spawn(move || {
            run_past_quit(Command::new("sh").args(["-c", &script]), Duration::from_secs(60))
        });
        let pids = pids(&pids_at, 2);
        stop_all();
        let stopped =
            pids.into_iter().all(ends) && helper.join().unwrap().is_ok_and(|o| !o.status.success());
        let kept = change.join().unwrap().is_ok_and(|o| o.status.success())
            && std::fs::read_to_string(&kept).is_ok_and(|k| k == "done\n");
        // One that would start once taskman is quitting doesn't.
        let later = sh("echo started", Duration::from_secs(60)).is_err();
        println!("stopped={stopped} kept={kept} later={later}");
    }

    /// git cleans up its lock files on SIGTERM, so a helper is asked to end before it's killed:
    /// one that ends then isn't killed, and one that doesn't is, after `STOP_GRACE`.
    #[test]
    fn stop_all_asks_before_it_kills() {
        let Some(dir) = std::env::var_os(ALONE).map(std::path::PathBuf::from) else {
            let out = alone("proc::tests::stop_all_asks_before_it_kills");
            assert!(out.contains("polite=term:true stubborn=term:true waited=true"), "{out}");
            return;
        };
        let start = |name: &str, on_term: &str| {
            let (pid, file) = (dir.join(format!("{name}.pid")), dir.join(name));
            let script = format!(
                "trap 'echo term > \"{}\"; {on_term}' TERM; echo $$ > '{}'; \
                 while :; do sleep 0.05; done",
                file.display(),
                pid.display()
            );
            let helper = std::thread::spawn(move || sh(&script, Duration::from_secs(60)));
            (pids(&pid, 1)[0], file, helper)
        };
        let (polite, polite_file, polite_helper) = start("polite", "exit 0");
        let (stubborn, stubborn_file, stubborn_helper) = start("stubborn", ":");
        let begun = Instant::now();
        stop_all();
        let waited = begun.elapsed();
        let ended = |pid, file: std::path::PathBuf, helper: std::thread::JoinHandle<_>| {
            let out: io::Result<Output> = helper.join().unwrap();
            let term = std::fs::read_to_string(file).unwrap_or_default();
            (ends(pid), term.trim().to_string(), out.is_ok_and(|o| o.status.success()))
        };
        let polite = ended(polite, polite_file, polite_helper);
        let stubborn = ended(stubborn, stubborn_file, stubborn_helper);
        assert!(polite.0 && stubborn.0, "polite {polite:?}, stubborn {stubborn:?}");
        let waited = waited >= STOP_GRACE && waited < Duration::from_secs(3);
        // The polite one exited 0 from its trap; the stubborn one was killed.
        println!(
            "polite={}:{} stubborn={}:{} waited={waited}",
            polite.1, polite.2, stubborn.1, !stubborn.2
        );
    }

    /// A helper still starting when taskman quits, past the fork and not yet exec'd: `stop_all`
    /// waits for it, so it doesn't go on to run with taskman gone and nothing to stop it.
    #[test]
    fn stop_all_waits_for_a_helper_still_starting() {
        let Some(dir) = std::env::var_os(ALONE).map(std::path::PathBuf::from) else {
            let out = alone("proc::tests::stop_all_waits_for_a_helper_still_starting");
            let pid: libc::pid_t = out
                .split_whitespace()
                .find_map(|w| w.strip_prefix("starting="))
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(|| panic!("no pid in {out}"));
            // Left running, it leads a session of its own once `own_session` has run in it.
            let until = Instant::now() + Duration::from_secs(5);
            let survived = loop {
                // SAFETY: signal 0 sends nothing, only checks the process is there.
                if unsafe { libc::kill(pid, 0) } != 0 {
                    break false;
                }
                // SAFETY: getpgid only reads the process's group id.
                if unsafe { libc::getpgid(pid) } == pid {
                    break true;
                }
                assert!(Instant::now() < until, "{pid} neither ended nor started");
                std::thread::sleep(Duration::from_millis(5));
            };
            // SAFETY: sends a signal; no memory is involved.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            assert!(!survived, "the helper outlived taskman");
            return;
        };
        let started = dir.join("started");
        let file = std::fs::File::create(&started).unwrap();
        std::thread::spawn(move || {
            use std::os::fd::AsRawFd;
            let mut cmd = Command::new("sleep");
            cmd.arg("30");
            // Before `own_session`'s: a child descheduled between fork and exec, which `run`
            // hasn't heard back from.
            // SAFETY: the closure runs in the child between fork and exec; it allocates nothing
            // and calls only getpid, write and nanosleep, which are async-signal-safe.
            unsafe {
                cmd.pre_exec(move || {
                    let pid = libc::getpid();
                    let size = std::mem::size_of_val(&pid);
                    libc::write(file.as_raw_fd(), (&raw const pid).cast(), size);
                    let second = libc::timespec { tv_sec: 1, tv_nsec: 0 };
                    libc::nanosleep(&raw const second, std::ptr::null_mut());
                    Ok(())
                });
            }
            let _ = run(&mut cmd, Duration::from_secs(60));
        });
        let until = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            if let Ok(b) = std::fs::read(&started)
                && let Ok(b) = <[u8; 4]>::try_from(b.as_slice())
            {
                break libc::pid_t::from_ne_bytes(b);
            }
            assert!(Instant::now() < until, "the helper never forked");
            std::thread::sleep(Duration::from_millis(5));
        };
        println!("starting={pid}");
        stop_all();
        // As taskman ends after `stop_all`: `run`'s thread doesn't outlive it.
        std::process::exit(0);
    }

    #[test]
    fn a_missing_program_is_not_found() {
        let err =
            run(&mut Command::new("taskman-no-such-program"), Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
