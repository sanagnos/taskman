//! The commands read their arguments alike: an empty `--project=` is refused with the same message
//! by each, and `taskman update` prints its usage for `--help` rather than updating, and refuses
//! what it doesn't know.

mod common;

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
