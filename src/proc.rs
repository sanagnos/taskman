//! Running other programs (git, gh, token commands, the updater's tar) from the TUI's threads and
//! commands: with no terminal to prompt on, and a time limit, so a program waiting for input that
//! never comes, or a network that never answers, can't hold a ticket's refreshes, a form or an
//! update forever.

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Runs `cmd` to the end and returns its output, like `Command::output`, except that it:
/// - reads stdin from /dev/null and runs in a session of its own ([`own_session`]), so it can't
///   reach the terminal the TUI owns;
/// - has git, gh and ssh fail rather than prompt for credentials;
/// - gives up after `timeout`, stopping the whole group, with an error of kind `TimedOut`.
///
/// Failing to start keeps `Command::spawn`'s error, so `NotFound` still says it isn't installed.
pub fn run(cmd: &mut Command, timeout: Duration) -> io::Result<Output> {
    run_inner(cmd, None, timeout)
}

/// [`run`], with `input` on stdin instead of /dev/null. The time limit covers writing it too: a
/// program that never reads it is stopped at the limit like any other.
pub fn run_with_input(cmd: &mut Command, input: Vec<u8>, timeout: Duration) -> io::Result<Output> {
    run_inner(cmd, Some(input), timeout)
}

fn run_inner(cmd: &mut Command, input: Option<Vec<u8>>, timeout: Duration) -> io::Result<Output> {
    let deadline = Instant::now() + timeout;
    // Without a terminal, ssh asks with an askpass program instead. Kept for a user who chose one
    // (some desktops set SSH_ASKPASS); otherwise ssh tries a built-in default, a bare X window.
    if ["SSH_ASKPASS", "SSH_ASKPASS_REQUIRE"].iter().all(|v| std::env::var_os(v).is_none()) {
        cmd.env("SSH_ASKPASS_REQUIRE", "never");
    }
    let mut child = own_session(cmd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
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
        if let Some(status) = child.try_wait()? {
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

/// Stops `child`'s whole process group (a hung `git fetch` waits on ssh, say) and says why.
fn stop(child: &mut Child, timeout: Duration) -> io::Error {
    // A system call, not the `kill` program, which minimal images (debian:*-slim) don't have. The
    // child leads a group of its own (`own_session`), so its pid is the group's id, and it
    // isn't reaped until the wait below, so that id can't pass to another group meanwhile. The
    // guard keeps a negated 0, 1 or our own group from signalling anything else.
    // SAFETY: getpgrp takes nothing and can't fail; it reads or writes no memory of ours.
    let ours = unsafe { libc::getpgrp() };
    if let Ok(group) = libc::pid_t::try_from(child.id())
        && group > 1
        && group != ours
    {
        // SAFETY: kill only sends a signal; it reads or writes no memory.
        unsafe { libc::kill(-group, libc::SIGKILL) };
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

    #[test]
    fn a_missing_program_is_not_found() {
        let err =
            run(&mut Command::new("taskman-no-such-program"), Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
