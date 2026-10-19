//! `taskman add` checking a key in the tracker: a key the tracker says doesn't exist is refused,
//! and one it can't be asked about is added unchecked.

mod common;

use std::path::Path;
use std::process::{Output, Stdio};

fn add(dir: &Path, repo: &Path, key: &str) -> Output {
    common::taskman(dir)
        .args(["add", key, "--no-worktree"])
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// The requests left in the inbox for taskman, which isn't running.
fn inbox(dir: &Path) -> String {
    std::fs::read_dir(dir.join("state/taskman/inbox"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect()
}

#[test]
fn adds_a_key_unchecked_when_the_token_command_is_missing() {
    let dir = common::scratch_dir("add-token-missing");
    let repo = common::project(&dir, &[]);
    let saved = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "linear", "site": "acme", "key": "ENG"}}],
        "tickets": [],
    });
    std::fs::write(dir.join("state/taskman/tickets.json"), saved.to_string()).unwrap();
    // sh says "...: taskman-test-no-such-command: not found", which once read as the ticket's.
    let config = serde_json::json!({ "accounts": [
        { "id": "linear-1", "provider": "linear", "site": "acme", "token": "cmd:taskman-test-no-such-command" },
    ]});
    std::fs::write(dir.join("config.json"), config.to_string()).unwrap();

    let out = add(&dir, &repo, "ENG-52");
    let (stdout, stderr) =
        (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(
        stderr.starts_with(
            "taskman add: couldn't check the ticket in the tracker, adding it anyway: the token command failed: "
        ) && stderr.contains("not found"),
        "{stderr}"
    );
    assert_eq!(stdout, "taskman isn't running; ENG-52 is added when it starts.\n");
    assert!(inbox(&dir).contains("\"ENG-52\""), "{}", inbox(&dir));
}

#[test]
fn refuses_a_key_the_tracker_doesnt_have() {
    let dir = common::scratch_dir("add-not-found");
    let repo = common::project(&dir, &[]);
    std::fs::write(repo.join("workplan/todo/ENG-1-login.md"), "# Fix the login\n").unwrap();

    let out = add(&dir, &repo, "ENG-9");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .starts_with("taskman add: ENG-9 not found: no ENG-9-….md in "),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(inbox(&dir).is_empty(), "{}", inbox(&dir));

    let out = add(&dir, &repo, "ENG-1");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(inbox(&dir).contains("\"ENG-1\""), "{}", inbox(&dir));
}
