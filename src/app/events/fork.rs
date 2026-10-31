use super::*;
use crate::coding_cli::ForkProbe;
use crate::store::Cli;
use std::sync::Arc;

/// `key`'s session as one that has had a turn: its id, and the transcript its CLI keeps.
fn has_conversation(app: &mut App, key: &str, sid: &str) {
    let i = index(app, key);
    app.tickets[i].t.session_id = Some(sid.into());
    app.tickets[i].t.cli.leave_transcript(sid);
}

/// Forks `key` into an agent named `name` with the Fork form, and waits for the CLI's answer.
fn fork_with_key(app: &mut App, key: &str, name: &str) {
    app.select(index(app, key));
    prefix(app, KeyCode::Char('f'));
    type_text(app, name);
    app.on_key(super::key(KeyCode::Enter));
    settle(app);
}

/// What the hooks of `key`'s session report: conversation `sid`, as they write it.
fn hooks_report(app: &mut App, key: &str, sid: &str) {
    let i = index(app, key);
    app.tickets[i].status.mtime = None;
    let path = store::status_file(&app.tickets[i].t.repo, &app.tickets[i].t.key);
    let record = serde_json::json!({ "state": "done", "session_id": sid, "message": "", "ts": 1 });
    store::write_atomic(&path, record.to_string().as_bytes()).unwrap();
    app.poll_status();
}

#[test]
fn the_key_forks_the_selected_session_into_a_new_agent() {
    let mut app = acme_app();
    has_conversation(&mut app, "acme-lead", "sid-lead");
    app.select(index(&app, "acme-lead"));
    prefix(&mut app, KeyCode::Char('f'));
    let Mode::Form(form) = &app.mode else { panic!("no form") };
    assert_eq!(form.title, "Fork");
    assert!(
        form.note
            .as_deref()
            .unwrap()
            .starts_with("Fork of acme-lead, as of when the fork starts: a new agent in Claude"),
        "{:?}",
        form.note
    );
    type_text(&mut app, "Deep dive");
    app.on_key(key(KeyCode::Tab));
    type_text(&mut app, "Dig into the flaky tests");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    // Asking the CLI can take a few seconds.
    assert_eq!(toast(&app), Some("Forking acme-lead…"));
    settle(&mut app);

    let fork = &app.tickets[app.sel].t;
    assert_eq!((fork.key.as_str(), fork.agent.as_deref()), ("acme-deep-dive", Some("deep-dive")));
    assert_eq!(fork.role.as_deref(), Some("Dig into the flaky tests"));
    // Claude's own id, chosen up front, and where it comes from.
    let id = fork.session_id.clone().unwrap();
    assert!(id.len() == 36 && id != "sid-lead", "{id}");
    let from = fork.fork.as_ref().unwrap();
    assert_eq!((from.from.as_str(), from.source.as_str()), ("sid-lead", "acme-lead"));
    assert!(fork.prompt_snapshot_off, "Claude can be told not to reuse the source's prompt");
    assert!(app.tickets[app.sel].start, "its session starts at once");
    assert_eq!(
        toast(&app),
        Some("Forked acme-lead into ✦ deep-dive: it starts with its conversation so far")
    );
    // Saved, and the source untouched.
    let saved = saved();
    let kept = saved.tickets.iter().find(|t| t.key == "acme-deep-dive").unwrap();
    assert_eq!(kept.fork.as_ref().map(|f| f.from.as_str()), Some("sid-lead"));
    assert_eq!(kept.session_id.as_deref(), Some(id.as_str()));
    let lead = saved.tickets.iter().find(|t| t.key == "acme-lead").unwrap();
    assert_eq!((lead.session_id.as_deref(), lead.fork.as_ref()), (Some("sid-lead"), None));
}

#[test]
fn a_session_with_no_conversation_yet_is_not_forked() {
    let mut app = acme_app();
    app.select(index(&app, "acme-lead"));
    prefix(&mut app, KeyCode::Char('f'));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("lead has no conversation to fork yet: wait for its first turn"));
    // An id whose transcript is gone is no better.
    app.tickets[app.sel].t.session_id = Some("sid-gone".into());
    prefix(&mut app, KeyCode::Char('f'));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn a_fork_needs_a_name_of_its_own() {
    let mut app = acme_app();
    has_conversation(&mut app, "ENG-52", "sid-52");
    let agents = app.tickets.len();
    for (name, error) in [
        ("", "Use letters or digits in the name"),
        ("lead", "✦ lead is already open: fork into another name"),
    ] {
        prefix(&mut app, KeyCode::Char('f'));
        type_text(&mut app, name);
        app.on_key(key(KeyCode::Enter));
        let Mode::Form(form) = &app.mode else { panic!("the form closed for {name:?}") };
        assert_eq!(form.error.as_deref(), Some(error));
        app.on_key(key(KeyCode::Esc));
    }
    // A closed agent's name resumes its own conversation, not a copy.
    app.projects[0].closed_agents.push(store::ClosedAgent {
        name: "tester".into(),
        session_id: "sid-tester".into(),
        cli: Cli::Claude,
        role: None,
        prompt_snapshot_off: false,
        extra: Default::default(),
    });
    prefix(&mut app, KeyCode::Char('f'));
    type_text(&mut app, "tester");
    app.on_key(key(KeyCode::Enter));
    let Mode::Form(form) = &app.mode else { panic!("the form closed") };
    assert!(form.error.as_deref().unwrap().contains("which that name resumes"), "{:?}", form.error);
    assert_eq!(app.tickets.len(), agents);
}

#[test]
fn a_cli_too_old_to_fork_says_so_and_adds_nothing() {
    let mut app = acme_app();
    app.probe_fork =
        Arc::new(|_| Ok(ForkProbe { version: Some("2.0.50".into()), snapshot_off: false }));
    has_conversation(&mut app, "ENG-52", "sid-52");
    let before = app.tickets.len();
    fork_with_key(&mut app, "ENG-52", "deep-dive");
    assert_eq!(app.tickets.len(), before);
    assert!(logged_error(
        &app,
        "Forking needs Claude Code 2.0.73 or newer, and this is 2.0.50: update it, then fork again"
    ));
}

/// Claude didn't answer `--help`: not known whether the fork could keep its own system prompt.
#[test]
fn a_claude_that_gives_no_help_adds_nothing() {
    let mut app = acme_app();
    app.probe_fork = Arc::new(|cli| cli.fork_probe(Some("2.1.314".into()), None));
    has_conversation(&mut app, "acme-lead", "sid-lead");
    let before = app.tickets.len();
    fork_with_key(&mut app, "acme-lead", "deep-dive");
    assert_eq!(app.tickets.len(), before);
    assert!(logged_error(
        &app,
        "Couldn't fork ✦ deep-dive: Claude Code didn't answer --help in time; try again"
    ));
}

/// The probe runs on a thread: a bug that panics it leaves no fork half made.
#[test]
fn a_probe_that_panics_adds_nothing() {
    let mut app = acme_app();
    has_conversation(&mut app, "ENG-52", "sid-52");
    panicking(&mut app);
    let before = app.tickets.len();
    fork_with_key(&mut app, "ENG-52", "deep-dive");
    assert_eq!(app.tickets.len(), before);
    assert!(logged_error(&app, &format!("Couldn't fork ✦ deep-dive: {BUG}")));
}

/// Codex makes the fork's id itself: the hooks report it, and the fork takes it then. The source's
/// own id, which a fork's start could report before its own, is neither taken nor a switch.
#[test]
fn a_codex_fork_takes_the_first_id_of_its_own_its_hooks_report() {
    let mut app = acme_app();
    has_conversation(&mut app, "ENG-61", "sid-61");
    fork_with_key(&mut app, "ENG-61", "codex-dive");
    let i = index(&app, "acme-codex-dive");
    assert_eq!(app.tickets[i].t.cli, Cli::Codex);
    assert_eq!(app.tickets[i].t.session_id, None);
    attach(&mut app, "acme-codex-dive", "");
    hooks_report(&mut app, "acme-codex-dive", "sid-61");
    let fork = &app.tickets[index(&app, "acme-codex-dive")];
    assert_eq!(fork.t.session_id, None);
    assert!(fork.t.fork.is_some() && fork.switched.is_none());
    assert!(!toast(&app).is_some_and(|t| t.contains("switched")), "{:?}", toast(&app));
    Cli::Codex.leave_transcript("sid-codex-dive");
    hooks_report(&mut app, "acme-codex-dive", "sid-codex-dive");
    let fork = &app.tickets[index(&app, "acme-codex-dive")].t;
    assert_eq!((fork.session_id.as_deref(), fork.fork.as_ref()), (Some("sid-codex-dive"), None));
    assert_eq!(app.tickets[index(&app, "ENG-61")].t.session_id.as_deref(), Some("sid-61"));
    // Saved, so a restart resumes its own.
    let saved = saved();
    let kept = saved.tickets.iter().find(|t| t.key == "acme-codex-dive").unwrap();
    assert_eq!((kept.session_id.as_deref(), kept.fork.as_ref()), (Some("sid-codex-dive"), None));
}

/// Claude's fork reports the id taskman chose for it, which ends its being a fork once Claude has
/// written that conversation down: until then, a start would have nothing to resume.
#[test]
fn a_claude_forks_first_report_makes_it_an_agent_like_any_other() {
    let mut app = acme_app();
    has_conversation(&mut app, "acme-lead", "sid-lead");
    fork_with_key(&mut app, "acme-lead", "deep-dive");
    let id = app.tickets[index(&app, "acme-deep-dive")].t.session_id.clone().unwrap();
    attach(&mut app, "acme-deep-dive", "");
    // The source's id first, as a fork's start might report it: neither taken nor a switch.
    hooks_report(&mut app, "acme-deep-dive", "sid-lead");
    let fork = &app.tickets[index(&app, "acme-deep-dive")];
    assert_eq!(fork.t.session_id.as_deref(), Some(id.as_str()));
    assert!(fork.t.fork.is_some() && fork.switched.is_none());
    hooks_report(&mut app, "acme-deep-dive", &id);
    let fork = &app.tickets[index(&app, "acme-deep-dive")].t;
    assert_eq!(fork.session_id.as_deref(), Some(id.as_str()));
    assert!(fork.fork.is_some(), "no transcript of its own yet");
    assert!(saved().tickets.iter().find(|t| t.key == "acme-deep-dive").unwrap().fork.is_some());
    Cli::Claude.leave_transcript(&id);
    hooks_report(&mut app, "acme-deep-dive", &id);
    let fork = &app.tickets[index(&app, "acme-deep-dive")].t;
    assert_eq!((fork.session_id.as_deref(), fork.fork.as_ref()), (Some(id.as_str()), None));
    // Its history still carries the source's recorded prompt: every start turns it off.
    assert!(fork.prompt_snapshot_off);
}

/// A fork closed and added again resumes its own conversation, which still carries the source's
/// recorded system prompt: its starts still turn recording off.
#[test]
fn a_fork_added_again_still_keeps_its_own_system_prompt() {
    let mut app = acme_app();
    has_conversation(&mut app, "acme-lead", "sid-lead");
    fork_with_key(&mut app, "acme-lead", "deep-dive");
    let id = app.tickets[index(&app, "acme-deep-dive")].t.session_id.clone().unwrap();
    attach(&mut app, "acme-deep-dive", "");
    Cli::Claude.leave_transcript(&id);
    hooks_report(&mut app, "acme-deep-dive", &id);
    app.select(index(&app, "acme-deep-dive"));
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Enter));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "acme-deep-dive"));
    assert!(saved().projects[0].closed_agents[0].prompt_snapshot_off);

    let added = app.add_agent(0, "deep-dive", Cli::Claude, None, true);
    assert!(matches!(added, Ok(crate::app::ticket::Added::New(_))));
    let t = app.tickets[index(&app, "acme-deep-dive")].t.clone();
    let argv: Vec<String> = app
        .build_command(&t, "acme-deep-dive", "start")
        .get_argv()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let resumed = argv.iter().position(|a| a == "--resume").map(|i| &argv[i + 1]);
    assert_eq!(resumed, Some(&id), "{argv:?}");
    let off = argv.iter().position(|a| a == "--system-prompt-snapshot").map(|i| &argv[i + 1]);
    assert_eq!(off.map(String::as_str), Some("off"), "{argv:?}");
}

/// The fork's id, chosen and saved before it starts, is the fork's: the source's hooks reporting
/// it (Claude's race, in the folder they share) is a switch, and the source keeps its own.
#[test]
fn the_source_never_takes_its_forks_conversation() {
    let mut app = acme_app();
    has_conversation(&mut app, "acme-lead", "sid-lead");
    fork_with_key(&mut app, "acme-lead", "deep-dive");
    let id = app.tickets[index(&app, "acme-deep-dive")].t.session_id.clone().unwrap();
    attach(&mut app, "acme-lead", "");
    hooks_report(&mut app, "acme-lead", &id);
    assert_eq!(app.tickets[index(&app, "acme-lead")].t.session_id.as_deref(), Some("sid-lead"));
    assert_eq!(
        toast(&app),
        Some(
            format!(
                "lead's Claude switched onto deep-dive's conversation (a Claude Code bug). Restart lead ({} R) to give it its own.",
                crate::keys::prefix()
            )
            .as_str()
        )
    );
}

/// Forking a ticket: the fork is an agent in the main checkout, and remembers the ticket's
/// worktree and branch, which its first message leaves to the ticket.
#[test]
fn a_tickets_fork_runs_in_the_main_checkout() {
    let mut app = acme_app();
    has_conversation(&mut app, "ENG-52", "sid-52");
    fork_with_key(&mut app, "ENG-52", "deep-dive");
    let fork = &app.tickets[index(&app, "acme-deep-dive")].t;
    assert_eq!((fork.cwd.to_str(), fork.branch.as_deref()), (Some("/src/acme"), None));
    let from = fork.fork.as_ref().unwrap();
    assert_eq!(from.source, "ENG-52");
    assert_eq!(from.worktree.as_deref(), Some(std::path::Path::new("/src/acme-ENG-52")));
    assert_eq!(from.branch.as_deref(), Some("eng-52-login"));
}

/// The user moved on while the CLI was asked: the fork is added without taking the selection.
#[test]
fn a_fork_leaves_the_selection_where_the_user_moved_it() {
    let mut app = acme_app();
    has_conversation(&mut app, "acme-lead", "sid-lead");
    app.select(index(&app, "acme-lead"));
    prefix(&mut app, KeyCode::Char('f'));
    type_text(&mut app, "deep-dive");
    app.on_key(key(KeyCode::Enter));
    app.select(index(&app, "ENG-53"));
    settle(&mut app);
    assert_eq!(selected(&app), "ENG-53");
    assert!(app.tickets[index(&app, "acme-deep-dive")].new, "marked + new instead");
}

/// The source's transcript went before the fork started: it starts as a new agent, and says so.
#[test]
fn a_fork_whose_source_is_gone_starts_fresh_and_says_so() {
    let mut app = acme_app();
    has_conversation(&mut app, "ENG-61", "sid-61");
    fork_with_key(&mut app, "ENG-61", "deep-dive");
    let i = index(&app, "acme-deep-dive");
    // Still there: nothing to say.
    app.fork_source_gone(i);
    assert!(app.tickets[i].t.fork.is_some());
    std::fs::remove_dir_all(Cli::Codex.home().join("sessions")).unwrap();
    app.fork_source_gone(i);
    assert_eq!(app.tickets[i].t.fork, None);
    assert_eq!(toast(&app), Some("✦ deep-dive's source conversation is gone, so it starts fresh"));
    let saved = saved();
    assert!(saved.tickets.iter().find(|t| t.key == "acme-deep-dive").unwrap().fork.is_none());
}

/// At startup, a status file a fork's last start left: the source's id isn't taken, the fork's
/// own is, even the one taskman chose for it, and that ends its being a fork once its transcript
/// is there.
#[test]
fn leftover_status_gives_a_fork_its_own_id_only() {
    let mut fork = crate::app::test_support::agent("deep-dive", Cli::Claude, None);
    fork.session_id = Some("sid-fork".into());
    fork.fork = Some(
        serde_json::from_value(serde_json::json!({ "from": "sid-lead", "source": "acme-lead" }))
            .unwrap(),
    );
    let mut tickets = vec![fork];
    let dir = store::state_dir().join("leftover-fork");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("acme-deep-dive.json");
    let left = |sid: &str| {
        let record =
            serde_json::json!({ "state": "done", "session_id": sid, "message": "", "ts": 5 });
        std::fs::write(&file, record.to_string()).unwrap();
    };
    let take = |tickets: &mut Vec<store::Ticket>| {
        crate::app::poll::take_leftover_status(
            tickets,
            |_| file.clone(),
            |_| dir.join("x"),
            |_| false,
        )
    };
    left("sid-lead");
    assert!(!take(&mut tickets));
    assert_eq!(tickets[0].session_id.as_deref(), Some("sid-fork"));
    assert!(tickets[0].fork.is_some());
    left("sid-fork");
    assert!(!take(&mut tickets), "no transcript of its own yet");
    assert!(tickets[0].fork.is_some());
    Cli::Claude.leave_transcript("sid-fork");
    left("sid-fork");
    assert!(take(&mut tickets), "the same id, but the fork is over");
    assert_eq!(
        (tickets[0].session_id.as_deref(), tickets[0].fork.as_ref()),
        (Some("sid-fork"), None)
    );
}

fn fork_request(id: &str, fork: &str) {
    request(
        id,
        &serde_json::json!({
            "repo": "/src/acme", "from": "✦ lead", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "agent", "name": "deep-dive", "fork": fork }],
        }),
    );
}

#[test]
fn a_sessions_fork_waits_for_the_users_approval() {
    let mut app = acme_app();
    has_conversation(&mut app, "ENG-52", "sid-52");
    let before = app.tickets.len();
    fork_request("0010", "ENG-52");
    app.take_inbox();
    let lines = reply("0010");
    assert_eq!(
        lines,
        [add::Line::ok(
            "taskman asks the user to approve ✦ deep-dive, a fork of ENG-52; its session starts once they do, and `taskman status` lists it"
        )]
    );
    app.ask_next();
    let Mode::ConfirmAgent(ask) = &mut app.mode else { panic!("no dialog") };
    ask.shown = ago(crate::app::ASK_ARMS.as_secs_f32()).unwrap();
    let screen = draw(&mut app, 100, 30);
    assert!(screen.contains("✦ lead asks to fork ENG-52 into agent ✦ deep-dive"), "{screen}");
    assert!(screen.contains("conversation as of then"), "{screen}");
    assert_eq!(app.tickets.len(), before, "nothing before the user approves");
    app.on_key(key(KeyCode::Enter));
    settle(&mut app);
    let fork = &app.tickets[index(&app, "acme-deep-dive")];
    assert_eq!(fork.t.fork.as_ref().map(|f| f.from.as_str()), Some("sid-52"));
    assert!(fork.start);
    assert_eq!(toast(&app), Some("✦ lead forked ENG-52 into ✦ deep-dive"));
    // Not selected: the user was elsewhere.
    assert_ne!(selected(&app), "acme-deep-dive");
}

#[test]
fn a_declined_fork_adds_nothing() {
    let mut app = acme_app();
    has_conversation(&mut app, "ENG-52", "sid-52");
    let before = app.tickets.len();
    fork_request("0011", "ENG-52");
    app.take_inbox();
    app.ask_next();
    let Mode::ConfirmAgent(ask) = &mut app.mode else { panic!("no dialog") };
    ask.shown = ago(crate::app::ASK_ARMS.as_secs_f32()).unwrap();
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.tickets.len(), before);
    assert_eq!(toast(&app), Some("Declined ✦ deep-dive from ✦ lead"));
}

/// A fork that can't be made is answered at once, with no dialog for the user.
#[test]
fn a_fork_that_cant_be_made_is_refused_without_asking() {
    let mut app = acme_app();
    fork_request("0012", "ENG-52");
    app.take_inbox();
    let lines = reply("0012");
    assert!(!lines[0].ok && lines[0].text.contains("no conversation to fork yet"), "{lines:?}");
    fork_request("0013", "ENG-99");
    app.take_inbox();
    assert_eq!(reply("0013"), [add::Line::err("ENG-99 isn't open in taskman")]);
    app.ask_next();
    assert!(matches!(app.mode, Mode::Normal));
}
