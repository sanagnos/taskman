//! The commands read their arguments alike: an empty `--project=` is refused with the same message
//! by each, and `taskman update` prints its usage for `--help` rather than updating, and refuses
//! what it doesn't know. A first argument that isn't a command is refused rather than starting the
//! TUI, and with no terminal at all taskman says it needs one. Help and usage piped into a reader
//! that stops early (`| head`) end quietly.

mod common;

use std::os::unix::process::CommandExt;
use std::process::{Output, Stdio};

fn run(dir: &std::path::Path, args: &[&str]) -> Output {
    common::taskman(dir).args(args).stdin(Stdio::null()).output().unwrap()
}

#[test]
fn every_command_refuses_an_empty_project() {
    let dir = common::scratch_dir("arguments-project");
    for args in [
        &["add", "52", "--project="][..],
        &["close", "52", "--project="],
        &["status", "--project="],
        &["resume", "lead", "--project="],
        &["move", "3", "review", "--project="],
    ] {
        let out = run(&dir, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("taskman {}: --project needs a value\n", args[0]),
            "{args:?}"
        );
    }
}

#[test]
fn update_prints_its_usage_and_refuses_unknown_options() {
    let dir = common::scratch_dir("arguments-update");
    // A development build refuses to update itself, so any other output means --help ran the
    // update.
    for help in ["--help", "-h"] {
        let out = run(&dir, &["update", help]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("Usage: taskman update"));
        assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let out = run(&dir, &["update", "--frce"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("taskman update: unknown option --frce\n\nUsage:"), "{err}");
}

#[test]
fn help_into_a_closed_pipe_ends_quietly() {
    let dir = common::scratch_dir("arguments-pipe");
    for args in [&["--help"][..], &["--version"], &["status", "--help"], &["update", "-h"]] {
        // The reader is gone before taskman writes, as when `head` has read all it wants.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let out = common::taskman(&dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?}: {err}");
        assert!(err.is_empty(), "{args:?}: {err}");
    }
    assert!(!dir.join("state/taskman/panics.log").exists());
}

#[test]
fn an_unknown_command_is_refused() {
    let dir = common::scratch_dir("arguments-unknown");
    for arg in ["stauts", "-v", "--frobnicate", ""] {
        let out = run(&dir, &[arg]);
        assert_eq!(out.status.code(), Some(2), "{arg:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("taskman: unknown command {arg:?}; taskman --help lists them\n"),
        );
        assert!(out.stdout.is_empty(), "{arg:?}");
    }
    // Refused before it looked at the state folder.
    assert!(!dir.join("state").exists());
    // `help` is `--help`.
    let help = run(&dir, &["help"]);
    assert!(help.status.success());
    assert_eq!(help.stdout, run(&dir, &["--help"]).stdout);
}

/// As with `ssh host taskman`, no -t: no controlling terminal, and stdin isn't one.
#[test]
fn with_no_terminal_it_says_it_needs_one() {
    let dir = common::scratch_dir("arguments-no-terminal");
    let mut cmd = common::taskman(&dir);
    // SAFETY: setsid only changes the child's session, between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let out = cmd.stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "taskman needs a terminal: run it in one (ssh -t for a remote machine)\n"
    );
    // Nothing left that says it runs: a command run now queues, and the next taskman starts.
    assert!(!dir.join("state/taskman/inbox/alive").exists());
    assert!(!dir.join("state/taskman/panics.log").exists());
}
