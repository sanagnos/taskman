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
            .starts_with("Fork of acme-lead, as of now: a new agent in Claude"),
        "{:?}",
        form.note
    );
    type_text(&mut app, "Deep dive");
    app.on_key(key(KeyCode::Tab));
    type_text(&mut app, "Dig into the flaky tests");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    settle(&mut app);

    let fork = &app.tickets[app.sel].t;
    assert_eq!((fork.key.as_str(), fork.agent.as_deref()), ("acme-deep-dive", Some("deep-dive")));
    assert_eq!(fork.role.as_deref(), Some("Dig into the flaky tests"));
    // Claude's own id, chosen up front, and where it comes from.
    let id = fork.session_id.clone().unwrap();
    assert!(id.len() == 36 && id != "sid-lead", "{id}");
    let from = fork.fork.as_ref().unwrap();
    assert_eq!((from.from.as_str(), from.source.as_str()), ("sid-lead", "acme-lead"));
    assert!(from.snapshot_off);
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
        Arc::new(|_| ForkProbe { version: Some("2.0.50".into()), snapshot_off: false });
    has_conversation(&mut app, "ENG-52", "sid-52");
    let before = app.tickets.len();
    fork_with_key(&mut app, "ENG-52", "deep-dive");
    assert_eq!(app.tickets.len(), before);
    assert!(logged_error(
        &app,
        "Forking needs Claude Code 2.0.73 or newer, and this is 2.0.50: update it, then fork again"
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
    hooks_report(&mut app, "acme-codex-dive", "sid-codex-dive");
    let fork = &app.tickets[index(&app, "acme-codex-dive")].t;
    assert_eq!((fork.session_id.as_deref(), fork.fork.as_ref()), (Some("sid-codex-dive"), None));
    assert_eq!(app.tickets[index(&app, "ENG-61")].t.session_id.as_deref(), Some("sid-61"));
    // Saved, so a restart resumes its own.
    let saved = saved();
    let kept = saved.tickets.iter().find(|t| t.key == "acme-codex-dive").unwrap();
    assert_eq!((kept.session_id.as_deref(), kept.fork.as_ref()), (Some("sid-codex-dive"), None));
}

/// Claude's fork reports the id taskman chose for it, which ends its being a fork.
#[test]
fn a_claude_forks_first_report_makes_it_an_agent_like_any_other() {
    let mut app = acme_app();
    has_conversation(&mut app, "acme-lead", "sid-lead");
    fork_with_key(&mut app, "acme-lead", "deep-dive");
    let id = app.tickets[index(&app, "acme-deep-dive")].t.session_id.clone().unwrap();
    attach(&mut app, "acme-deep-dive", "");
    hooks_report(&mut app, "acme-deep-dive", &id);
    let fork = &app.tickets[index(&app, "acme-deep-dive")].t;
    assert_eq!((fork.session_id.as_deref(), fork.fork.as_ref()), (Some(id.as_str()), None));
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
    assert!(screen.contains("conversation as of now"), "{screen}");
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
