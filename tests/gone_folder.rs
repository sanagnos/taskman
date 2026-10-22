//! A session whose folder is gone (a worktree deleted by hand, a project folder moved) must not
//! start somewhere else: portable_pty runs a command whose folder isn't there in $HOME instead.

mod common;

use common::{Tui, project, scratch_dir, sh, stub_file, stubbed, taskman};
use std::process::Stdio;
use std::time::Duration;

/// Sets ticket ENG-1's folder and branch in the saved state, as taskman saves them.
fn set_folder(dir: &std::path::Path, cwd: &std::path::Path, branch: &str) {
    let path = dir.join("state/taskman/tickets.json");
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    saved["tickets"][0]["cwd"] = cwd.to_str().unwrap().into();
    saved["tickets"][0]["branch"] = branch.into();
    std::fs::write(&path, saved.to_string()).unwrap();
}

/// What `taskman resume ENG-1` says on its way out.
fn resume(dir: &std::path::Path) -> (Option<i32>, String) {
    let out = taskman(dir)
        .args(["resume", "ENG-1"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn a_session_whose_worktree_is_gone_does_not_start_until_it_is_made_again() {
    let dir = scratch_dir("gone-worktree");
    let repo = project(&dir, &["ENG-1"]);
    sh(&repo, "git init -q -b main && git add -A && git commit -q -m one");
    let gone = dir.join("repo-ENG-1");
    sh(&repo, "git worktree add -q ../repo-ENG-1 -b ENG-1-test");
    set_folder(&dir, &gone, "ENG-1-test");
    std::fs::remove_dir_all(&gone).unwrap();

    // taskman resume refuses, rather than failing as though the CLI weren't installed.
    let (code, said) = resume(&dir);
    assert_eq!(code, Some(1), "{said}");
    assert!(
        said.contains(&format!(
            "ENG-1's folder {} is gone: select ENG-1 in taskman",
            gone.display()
        )),
        "{said}"
    );

    // Started with the saved state, as after a restart: the pane says why, and no CLI runs.
    let mut tui = Tui::start("gone-worktree", &dir, stubbed(&dir), Duration::from_secs(60));
    tui.wait_text("ENG-1's worktree is gone");
    tui.wait_text("Press Enter to make it again there, on ENG-1-test.");
    assert_eq!(stub_file(&dir, "ENG-1", "pid"), "", "a CLI started");

    // Enter makes it again, on its branch, and the session starts in it.
    tui.send(b"\r");
    tui.wait_until("ENG-1's session started", || !stub_file(&dir, "ENG-1", "cwd").is_empty());
    let real = std::fs::canonicalize(&gone).unwrap();
    assert_eq!(stub_file(&dir, "ENG-1", "cwd").trim(), real.to_str().unwrap());
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&gone)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ENG-1-test");
    tui.quit();
}

#[test]
fn a_session_whose_project_folder_is_gone_does_not_start() {
    let dir = scratch_dir("gone-project");
    let repo = project(&dir, &["ENG-1"]);
    std::fs::rename(&repo, dir.join("repo-moved")).unwrap();

    let (code, said) = resume(&dir);
    assert_eq!(code, Some(1), "{said}");
    assert!(
        said.contains(&format!("acme's folder {} is gone: put it back", repo.display())),
        "{said}"
    );

    let mut tui = Tui::start("gone-project", &dir, stubbed(&dir), Duration::from_secs(60));
    tui.wait_text("acme's folder is gone");
    tui.wait_text("Put it back, then press Enter to start ENG-1's session.");
    assert_eq!(stub_file(&dir, "ENG-1", "pid"), "", "a CLI started");

    // Put back, Enter starts it there.
    std::fs::rename(dir.join("repo-moved"), &repo).unwrap();
    tui.send(b"\r");
    tui.wait_until("ENG-1's session started", || !stub_file(&dir, "ENG-1", "cwd").is_empty());
    let real = std::fs::canonicalize(&repo).unwrap();
    assert_eq!(stub_file(&dir, "ENG-1", "cwd").trim(), real.to_str().unwrap());
    tui.quit();
}
