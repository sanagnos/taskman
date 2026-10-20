//! Sessions run `taskman status`, `add` and `move` without a prompt, so in a session taskman
//! started they keep to its project: another project's ticket, board or repository needs a command
//! that leaves the session's environment, which the CLI asks the user about first.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

/// Projects acme (ENG, with ENG-1 open and an agent) and private (SEC), each a Local files board
/// with one ticket in todo. Returns their folders.
fn two_projects(dir: &Path) -> (PathBuf, PathBuf) {
    let acme = common::project(dir, &["ENG-1"]);
    let private = dir.join("private");
    std::fs::create_dir_all(private.join("workplan/todo")).unwrap();
    std::fs::write(private.join("workplan/todo/SEC-7-secret.md"), "# SEC-7 private plans\n")
        .unwrap();
    let tracker = |key| serde_json::json!({"provider": "local", "site": "workplan", "key": key});
    let saved = serde_json::json!({
        "projects": [
            {"name": "acme", "repo": acme, "tracker": tracker("ENG")},
            {"name": "private", "repo": private, "tracker": tracker("SEC")},
        ],
        "tickets": [
            {"key": "ENG-1", "repo": acme, "cwd": acme},
            {"key": "acme-lead", "repo": acme, "cwd": acme, "agent": "lead"},
        ],
    });
    std::fs::write(dir.join("state/taskman/tickets.json"), saved.to_string()).unwrap();
    (acme, private)
}

/// `taskman ARGS` in `session`'s environment as taskman sets it, in acme's folder; no session
/// outside one.
fn run(dir: &Path, acme: &Path, session: Option<&str>, args: &[&str]) -> (bool, String, String) {
    let mut cmd = common::taskman(dir);
    if let Some(s) = session {
        cmd.env("TASKMAN_TICKET", s).env("TASKMAN_REPO", acme);
    }
    let out: Output = cmd.args(args).current_dir(acme).stdin(Stdio::null()).output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.success(), text(&out.stdout), text(&out.stderr))
}

#[test]
fn a_session_reaches_only_its_own_project() {
    let dir = common::scratch_dir("session-scope");
    let (acme, private) = two_projects(&dir);
    let refused = |(ok, out, err): (bool, String, String), what: &str| {
        assert!(!ok, "{what}: {out}\n{err}");
        assert!(!out.contains("private plans"), "{what}: {out}");
        assert!(
            err.contains("that's project private, and this session works in acme")
                && err.contains("`env -u TASKMAN_REPO`"),
            "{what}: {err}"
        );
    };

    for session in ["ENG-1", "acme-lead"] {
        // Its own project's tickets, as before.
        let (_, out, err) = run(&dir, &acme, Some(session), &["status", "ENG-1"]);
        assert!(out.contains("ENG-1 test"), "{session}: {out}\n{err}");
        let (_, out, err) = run(&dir, &acme, Some(session), &["status"]);
        assert!(out.contains("ENG-1"), "{session}: {out}\n{err}");
        // Another project's, by its prefix or by name, is refused.
        refused(run(&dir, &acme, Some(session), &["status", "SEC-7"]), "status SEC-7");
        refused(
            run(&dir, &acme, Some(session), &["status", "--project=private"]),
            "status --project",
        );
        // Without --project, add and move take the session's project, whose prefix SEC-7 hasn't.
        for args in [&["move", "SEC-7", "doing"][..], &["add", "SEC-7"]] {
            let (ok, _, err) = run(&dir, &acme, Some(session), args);
            assert!(!ok && err.contains("doesn't belong to acme's tracker"), "{args:?}: {err}");
        }
        refused(
            run(&dir, &acme, Some(session), &["move", "SEC-7", "doing", "--project", "private"]),
            "move --project",
        );
        refused(
            run(&dir, &acme, Some(session), &["add", "SEC-7", "--project", "private"]),
            "add --project",
        );
    }
    assert!(private.join("workplan/todo/SEC-7-secret.md").exists());
    assert!(
        !dir.join("state/taskman/inbox").exists() || {
            let requests = std::fs::read_dir(dir.join("state/taskman/inbox")).unwrap();
            requests.flatten().all(|e| e.path().extension().is_none_or(|x| x != "json"))
        }
    );

    // Outside the session's environment (a plain shell, or `env -u TASKMAN_REPO`), any project.
    let (_, out, err) = run(&dir, &acme, None, &["status", "SEC-7"]);
    assert!(out.contains("private plans"), "{out}\n{err}");
}
