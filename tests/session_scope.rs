//! Sessions run `taskman status`, `add` and `move` without a prompt, so in a session taskman
//! started they keep to its project: another project's ticket, board or repository needs a command
//! that leaves the session's environment, which the CLI's own permission settings decide on.

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
fn a_session_is_refused_every_project() {
    let dir = common::scratch_dir("session-all-projects");
    let (acme, _) = two_projects(&dir);
    // SEC-7 open too, as the view lists open tickets, and a third project on a Jira account whose
    // token command leaves a mark. With no ticket there, nothing asks that Jira for anything.
    let state = dir.join("state/taskman/tickets.json");
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    let private = saved["projects"][1]["repo"].clone();
    let site = "https://127.0.0.1:1";
    saved["tickets"].as_array_mut().unwrap().push(serde_json::json!(
        {"key": "SEC-7", "repo": private, "cwd": private}
    ));
    saved["projects"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "work", "repo": dir.join("work"),
        "tracker": {"provider": "jira", "site": site, "key": "OPS", "account": "jira-1"},
    }));
    std::fs::write(&state, saved.to_string()).unwrap();
    let mark = dir.join("token-read");
    let token = format!("cmd:touch '{}'; echo token", mark.display());
    let config = serde_json::json!({"accounts": [
        {"id": "jira-1", "provider": "jira", "site": site, "email": "ana@example.com", "token": token},
    ]});
    std::fs::write(dir.join("config.json"), config.to_string()).unwrap();

    let run = |env: &[(&str, &Path)], args: &[&str]| {
        let mut cmd = common::taskman(&dir);
        cmd.envs(env.iter().copied()).args(args).current_dir(&acme).stdin(Stdio::null());
        let out = cmd.output().unwrap();
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        (out.status.success(), text(&out.stdout), text(&out.stderr))
    };
    let ticket = ("TASKMAN_TICKET", Path::new("ENG-1"));
    let agent = ("TASKMAN_TICKET", Path::new("acme-lead"));
    let repo = ("TASKMAN_REPO", acme.as_path());
    let all = ["status", "--all-projects"];
    let all_json = ["status", "--all-projects", "--json"];
    for env in [&[ticket, repo][..], &[agent, repo], &[repo]] {
        for args in [&all[..], &all_json] {
            let (ok, out, err) = run(env, args);
            assert!(!ok && out.is_empty(), "{env:?} {args:?}: {out}\n{err}");
            assert!(
                err.contains("`--all-projects` reads every project's tracker")
                    && err.contains("`env -u TASKMAN_REPO`"),
                "{env:?} {args:?}: {err}"
            );
        }
    }
    // Nor does a session whose project has gone get round it.
    let (ok, out, err) = run(&[ticket, ("TASKMAN_REPO", Path::new("/gone"))], &all);
    assert!(!ok && out.is_empty() && err.contains("no longer in taskman"), "{out}\n{err}");
    assert!(!mark.exists(), "a refused command read a token");

    // In acme's folder a plain shell's `status` stays with acme; the flag shows them all.
    let (_, out, err) = run(&[], &["status"]);
    assert!(out.contains("ENG-1") && !out.contains("private plans"), "{out}\n{err}");
    assert!(!mark.exists(), "acme's status read another project's token");
    let (_, out, err) = run(&[], &all);
    assert!(out.contains("ENG-1") && out.contains("private plans"), "{out}\n{err}");
    assert!(out.contains("work  "), "{out}\n{err}");
    assert!(mark.exists(), "the token command is what shows a token was read");

    // `env -u TASKMAN_REPO` alone leaves TASKMAN_TICKET, which the flag doesn't narrow to.
    for session in [ticket, agent] {
        let (_, out, err) = run(&[session], &all);
        assert!(out.contains("ENG-1") && out.contains("private plans"), "{out}\n{err}");
        let (_, out, err) = run(&[session], &["status"]);
        assert!(out.contains("ENG-1") && !out.contains("private plans"), "{out}\n{err}");
    }
}

/// With one project there is nothing else to reach, but the answer doesn't say so.
#[test]
fn a_session_is_refused_every_project_when_there_is_one() {
    let dir = common::scratch_dir("session-all-projects-one");
    let acme = common::project(&dir, &["ENG-1"]);
    let (ok, out, err) = run(&dir, &acme, Some("ENG-1"), &["status", "--all-projects"]);
    assert!(!ok && out.is_empty() && err.contains("`--all-projects` reads"), "{out}\n{err}");
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
