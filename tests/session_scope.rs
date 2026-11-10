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
            err.contains("this session works in acme: a session keeps to its own project")
                && err.contains("`env -u TASKMAN_REPO`")
                && !err.contains("private"),
            "{what}: {err}"
        );
    };

    for session in ["ENG-1", "acme-lead"] {
        // Its own project's tickets, as before.
        let (_, out, err) = run(&dir, &acme, Some(session), &["status", "ENG-1"]);
        assert!(out.contains("ENG-1 test"), "{session}: {out}\n{err}");
        let (_, out, err) = run(&dir, &acme, Some(session), &["status"]);
        assert!(out.contains("ENG-1"), "{session}: {out}\n{err}");
        // A key with another prefix than the session's tracker's is refused, nothing read.
        let (ok, out, err) = run(&dir, &acme, Some(session), &["status", "SEC-7"]);
        assert!(!ok && out.is_empty(), "{session}: {out}\n{err}");
        assert!(
            err.starts_with("taskman status: SEC-7 doesn't belong to acme's tracker (ENG). ")
                && err.contains("`env -u TASKMAN_REPO`"),
            "{err}"
        );
        // By name, it's refused.
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

/// What a session is told about anything outside its project is the same, output and exit status,
/// whether another project has the name or the ticket prefix asked for, two have, or none has: so
/// nothing it runs without a prompt says which projects taskman has, or whether there are others.
#[test]
fn a_sessions_answers_dont_depend_on_the_other_projects() {
    let dir = common::scratch_dir("session-sweep");
    let (acme, private) = two_projects(&dir);
    // SEC-7 open in private, and two more projects that share a slug and a prefix.
    let state = dir.join("state/taskman/tickets.json");
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    saved["tickets"].as_array_mut().unwrap().push(serde_json::json!(
        {"key": "SEC-7", "repo": private, "cwd": private}
    ));
    for name in ["Side Project", "side-project"] {
        let repo = dir.join(name);
        std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
        std::fs::write(repo.join("workplan/todo/DUP-3-shared.md"), "# DUP-3 shared\n").unwrap();
        saved["projects"].as_array_mut().unwrap().push(serde_json::json!({
            "name": name, "repo": repo,
            "tracker": {"provider": "local", "site": "workplan", "key": "DUP"},
        }));
    }
    std::fs::write(&state, saved.to_string()).unwrap();

    // The exit status, and what was printed, with `asked` (the name or key) written as `ASKED`.
    let answer = |session: Option<&str>, args: &[&str], asked: &str| {
        let mut cmd = common::taskman(&dir);
        if let Some(s) = session {
            cmd.env("TASKMAN_TICKET", s).env("TASKMAN_REPO", &acme);
        }
        let out = cmd.args(args).current_dir(&acme).stdin(Stdio::null()).output().unwrap();
        let text = |b: &[u8]| String::from_utf8_lossy(b).replace(asked, "ASKED");
        (out.status.code(), text(&out.stdout), text(&out.stderr))
    };
    // Nothing of the other projects: their names, their prefixes, their tickets.
    let others = ["private", "Side Project", "side-project", "SEC", "DUP", "shared"];
    let silent = |what: &str, (_, out, err): &(Option<i32>, String, String)| {
        let said: Vec<_> = others.iter().filter(|o| out.contains(*o) || err.contains(*o)).collect();
        assert!(said.is_empty(), "{what} says {said:?}:\n{out}\n{err}");
    };
    // Another project's name, one two projects' slug matches, and nobody's.
    let names = ["private", "Side_Project", "nobody"];
    let named: [&[&str]; 9] = [
        &["status", "--project", "NAME"],
        &["status", "--project", "NAME", "--json"],
        &["status", "ENG-1", "--project", "NAME"],
        &["status", "ENG-1", "--json", "--project=NAME"],
        &["add", "ENG-9", "--project", "NAME"],
        &["move", "ENG-1", "doing", "--project", "NAME"],
        &["title", "ENG-1", "Fix login", "--project", "NAME"],
        &["close", "ENG-1", "--project", "NAME"],
        &["resume", "ENG-1", "--project", "NAME"],
    ];
    // Another project's key (open there), one with a prefix two projects use, and nobody's.
    let keys = ["SEC-7", "DUP-3", "ZZZ-7"];
    let keyed: [&[&str]; 9] = [
        &["status", "KEY"],
        &["status", "KEY", "--json"],
        &["add", "KEY"],
        &["add", "ENG-9", "--on", "KEY"],
        &["add", "--agent", "tester", "--fork", "KEY"],
        &["move", "KEY", "doing"],
        &["title", "KEY", "Fix login"],
        &["close", "KEY"],
        &["resume", "KEY"],
    ];
    let put = |args: &[&str], hole: &str, with: &str| -> Vec<String> {
        args.iter().map(|a| a.replace(hole, with)).collect()
    };
    for session in ["ENG-1", "acme-lead"] {
        for (templates, hole, asked) in [(&named, "NAME", &names), (&keyed, "KEY", &keys)] {
            for template in templates {
                let answers: Vec<_> = asked
                    .iter()
                    .map(|a| {
                        let args = put(template, hole, a);
                        let args: Vec<&str> = args.iter().map(String::as_str).collect();
                        answer(Some(session), &args, a)
                    })
                    .collect();
                let what = format!("{session}: taskman {}", template.join(" "));
                assert_ne!(answers[0].0, Some(0), "{what}: {answers:#?}");
                assert_eq!(answers[0], answers[1], "{what}");
                assert_eq!(answers[0], answers[2], "{what}");
                silent(&what, &answers[0]);
                // `title` has a refusal of its own, which offers no way round; every other says
                // how to go wider, as a plain shell would have answered otherwise.
                if hole == "NAME" && template[0] != "title" {
                    let refusal = "this session works in acme: a session keeps to its own project";
                    assert!(answers[0].1.is_empty() && answers[0].2.contains(refusal), "{what}");
                }
                // `--fork` names a session in the folder's project from a plain shell too.
                let wider = answers[0].2.contains("`env -u TASKMAN_REPO`");
                let differs = template[0] != "title" && !template.contains(&"--fork");
                assert_eq!(wider, differs, "{what}: {answers:#?}");
            }
        }
        // What it's shown of its own project names no other either, as text or JSON.
        for args in [&["status"][..], &["status", "--json"], &["status", "ENG-1"], &["status", "7"]]
        {
            silent(
                &format!("{session}: taskman {}", args.join(" ")),
                &answer(Some(session), args, "ASKED"),
            );
        }
    }
    // Nothing was moved or asked of the running taskman.
    assert!(private.join("workplan/todo/SEC-7-secret.md").exists());
    assert!(acme.join("workplan/todo/ENG-1-test.md").exists());

    // From the user's own shell, the lists and hints as before.
    let (code, _, err) = answer(None, &["status", "--project", "nobody"], "ASKED");
    assert_eq!(code, Some(1));
    assert!(err.contains("taskman has acme, private, Side Project, side-project"), "{err}");
    let (_, _, err) = answer(None, &["status", "--project", "Side_Project"], "ASKED");
    assert!(err.contains("could mean projects Side Project or side-project"), "{err}");
    let (_, _, err) = answer(None, &["add", "SEC-7", "--project", "acme"], "ASKED");
    assert!(err.contains("for project private, pass --project private"), "{err}");
    let (_, out, err) = answer(None, &["status", "SEC-7"], "ASKED");
    assert!(out.contains("private plans"), "{out}\n{err}");
}

/// In a session `--project` takes the session's own project by its exact name only: another
/// spelling may be another project's (`ACME` beside `acme`), so every command refuses it, in the
/// same words with or without such a project, and acts on nothing.
#[test]
fn a_session_takes_its_own_project_by_its_exact_name_only() {
    let dir = common::scratch_dir("session-own-name");
    let (acme, _) = two_projects(&dir);
    type Answer = (Option<i32>, String, String);
    let answer = |args: &[String]| -> Answer {
        let mut cmd = common::taskman(&dir);
        cmd.env("TASKMAN_TICKET", "ENG-1").env("TASKMAN_REPO", &acme);
        let out = cmd.args(args).current_dir(&acme).stdin(Stdio::null()).output().unwrap();
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        (out.status.code(), text(&out.stdout), text(&out.stderr))
    };
    let with = |template: &[&str], name: &str| -> Vec<String> {
        template.iter().map(|a| a.replace("NAME", name)).collect()
    };
    // Read-only, or with nothing to do: safe to run with the exact name too.
    let reads: [&[&str]; 4] = [
        &["status", "--project", "NAME"],
        &["status", "ENG-1", "--json", "--project", "NAME"],
        &["status", "SEC-7", "--project", "NAME"],
        &["add", "ENG-1", "--project", "NAME"],
    ];
    // Ones that would act on acme's ENG-1 if taken: run only with spellings that must be refused.
    let acts: [&[&str]; 4] = [
        &["move", "ENG-1", "doing", "--project", "NAME"],
        &["title", "ENG-1", "Fix login", "--project", "NAME"],
        &["close", "ENG-1", "--project", "NAME"],
        &["resume", "ENG-1", "--project", "NAME"],
    ];
    let variants = ["ACME", "Acme", "acme!", " acme ", "acme "];
    let run_all = || {
        let exact: Vec<Answer> = reads.iter().map(|t| answer(&with(t, "acme"))).collect();
        let refused: Vec<Vec<Answer>> = reads
            .iter()
            .chain(&acts)
            .map(|t| variants.iter().map(|v| answer(&with(t, v))).collect())
            .collect();
        (exact, refused)
    };
    let (exact, refused) = run_all();
    // The exact name is its own project: its overview, its ticket.
    assert_eq!(exact[0].0, Some(0), "{exact:#?}");
    assert!(exact[0].1.contains("ENG-1") && exact[1].1.contains("\"key\": \"ENG-1\""));
    // Every other spelling is refused, the same for each, nothing printed but the refusal.
    for (template, answers) in reads.iter().chain(&acts).zip(&refused) {
        let what = template.join(" ");
        assert_eq!(answers[0].0, Some(1), "{what}: {answers:#?}");
        assert!(answers[0].1.is_empty(), "{what}: {answers:#?}");
        assert!(answers.iter().all(|a| a == &answers[0]), "{what}: {answers:#?}");
    }
    assert!(acme.join("workplan/todo/ENG-1-test.md").exists(), "a refused move moved it");
    // Projects named like some of those spellings change nothing in its session.
    let state = dir.join("state/taskman/tickets.json");
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    for name in ["ACME", "acme!"] {
        let repo = dir.join(format!("look-alike {name}"));
        std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
        saved["projects"].as_array_mut().unwrap().push(serde_json::json!({
            "name": name, "repo": repo,
            "tracker": {"provider": "local", "site": "workplan", "key": "ENG"},
        }));
    }
    std::fs::write(&state, saved.to_string()).unwrap();
    assert_eq!(run_all(), (exact, refused));
    assert!(acme.join("workplan/todo/ENG-1-test.md").exists(), "a refused move moved it");
    // From the user's own shell they're told apart, as before.
    let mut cmd = common::taskman(&dir);
    let out = cmd.args(["status", "--project", "Acme"]).current_dir(&acme).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("could mean projects acme or ACME"), "{err}");
}
