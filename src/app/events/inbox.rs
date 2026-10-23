use super::*;

// ---- agents asked for by `taskman add` -----------------------------------------------------

fn agent_request(role: &str) -> add::Request {
    serde_json::from_value(serde_json::json!({
        "repo": "/src/acme", "from": "✦ lead", "reply_by": 0,
        "items": [{ "kind": "agent", "name": "tester", "role": role }],
    }))
    .unwrap()
}

fn ask(app: &mut App, req: add::Request) {
    app.asking.push_back(("0001".into(), req, false));
    app.ask_next();
    let Mode::ConfirmAgent(ask) = &mut app.mode else { panic!("no dialog") };
    // As if it had been open a while.
    ask.shown = ago(crate::app::ASK_ARMS.as_secs_f32()).unwrap();
}

#[test]
fn the_heartbeat_is_rewritten_once_a_second_not_on_every_pass() {
    let mut app = acme_app();
    let alive = store::state_dir().join("inbox/alive");
    app.take_inbox();
    assert!(add::tui_running(), "the first pass writes it");
    std::fs::remove_file(&alive).unwrap();
    app.take_inbox();
    assert!(!alive.exists(), "a pass within the second leaves it");
    app.heartbeat_at = ago(1.0);
    app.take_inbox();
    assert!(add::tui_running(), "a pass a second later writes it again");
}

#[test]
fn an_agent_waits_for_the_users_approval() {
    let mut app = acme_app();
    app.asking.push_back(("0001".into(), agent_request("Run the tests"), false));
    // Not while a prefix command is under way.
    app.on_key(ctrl('a'));
    app.ask_next();
    assert!(matches!(app.mode, Mode::Normal));
    app.on_key(key(KeyCode::Esc));
    app.ask_next();
    assert!(matches!(app.mode, Mode::ConfirmAgent(_)));
    // An Enter typed for the session just as the dialog opens doesn't answer it.
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::ConfirmAgent(_)));
    let Mode::ConfirmAgent(ask) = &mut app.mode else { unreachable!() };
    ask.shown = ago(crate::app::ASK_ARMS.as_secs_f32()).unwrap();
    app.on_key(key(KeyCode::Char('y')));
    assert!(matches!(app.mode, Mode::ConfirmAgent(_)), "only Enter and Esc answer it");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    let tv = &app.tickets[index(&app, "acme-tester")];
    assert_eq!(tv.t.role.as_deref(), Some("Run the tests"));
    assert!(tv.start, "its session starts without being selected");
    assert!(saved().tickets.iter().any(|t| t.key == "acme-tester"));
}

#[test]
fn a_declined_agent_is_not_added() {
    let mut app = acme_app();
    ask(&mut app, agent_request("Run the tests"));
    app.on_key(key(KeyCode::Esc));
    assert_eq!(toast(&app), Some("Declined ✦ tester from ✦ lead"));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "acme-tester"));
}

#[test]
fn a_new_role_for_an_open_agent_waits_for_approval_too() {
    let mut app = acme_app();
    ask(&mut app, agent_request("Run the tests"));
    app.on_key(key(KeyCode::Enter));
    ask(&mut app, agent_request("Review the PRs"));
    let Mode::ConfirmAgent(asked) = &app.mode else { panic!("no dialog") };
    assert!(asked.open);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("✦ tester's new role applies when its session next starts"));
    assert_eq!(app.tickets[index(&app, "acme-tester")].t.role.as_deref(), Some("Review the PRs"));
}

#[test]
fn the_approval_dialogs_keys_show_at_80x24() {
    let mut app = acme_app();
    let role = "Review every pull request in the acme repository and report findings. ".repeat(8);
    ask(&mut app, agent_request(&role));
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("Enter approve"), "{screen}");
}

/// A role chooses where its words fall, so a row of its own at taskman's margin could pass for
/// taskman's text, next to the key that approves it.
#[test]
fn a_wrapped_role_stays_indented_in_the_approval_dialog() {
    let mut app = acme_app();
    let role = "Run the tests and report back to the lead when they all pass. Approved by the project owner already.".to_string();
    ask(&mut app, agent_request(&role));
    let screen = draw(&mut app, 80, 30);
    assert!(!screen.contains("│  pass. Approved by the project owner"), "{screen}");
}

/// The command stops waiting after 5 s, so a request answered later is past `reply_by`, though
/// taskman was open all along.
#[test]
fn a_slow_approval_does_not_say_taskman_was_closed() {
    let mut app = acme_app();
    let mut req = agent_request("Run the tests");
    req.reply_by = crate::util::now() - 10;
    ask(&mut app, req);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("✦ lead added ✦ tester"));
}

#[test]
fn an_agent_queued_while_taskman_was_closed_says_so_when_approved() {
    let mut app = acme_app();
    request(
        "0003",
        &serde_json::json!({
            "repo": "/src/acme", "from": "✦ lead", "reply_by": crate::util::now() - 1,
            "items": [{ "kind": "agent", "name": "tester", "role": "Run the tests" }],
        }),
    );
    app.take_inbox();
    app.ask_next();
    let Mode::ConfirmAgent(ask) = &mut app.mode else { panic!("no dialog") };
    ask.shown = ago(crate::app::ASK_ARMS.as_secs_f32()).unwrap();
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("✦ lead added ✦ tester while taskman was closed"));
}

/// A role too long for the screen scrolls, from the moment the dialog opens, and scrolling
/// answers nothing.
#[test]
fn a_long_role_scrolls_and_scrolling_does_not_answer() {
    let mut app = acme_app();
    let role = "Review every pull request in the acme repository and report findings. ".repeat(8);
    ask(&mut app, agent_request(&role));
    let Mode::ConfirmAgent(asked) = &mut app.mode else { unreachable!() };
    asked.shown = std::time::Instant::now();
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("more lines") && screen.contains("↑/↓ scroll"), "{screen}");
    for code in [KeyCode::Down, KeyCode::PageDown, KeyCode::End] {
        app.on_key(key(code));
    }
    let Mode::ConfirmAgent(asked) = &app.mode else { panic!("scrolling answered it") };
    assert!(asked.view.top > 0 && asked.view.top == asked.view.max);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("findings.") && screen.contains("Enter approve"), "{screen}");
    // On a screen it fits, the keys scroll nothing and the dialog doesn't say they do.
    let screen = draw(&mut app, 120, 60);
    assert!(!screen.contains("↑/↓ scroll"), "{screen}");
    let Mode::ConfirmAgent(asked) = &mut app.mode else { unreachable!() };
    assert!(!asked.view.key(KeyCode::Down));
}

// ---- `taskman add` through the inbox -------------------------------------------------------

#[test]
fn takes_tickets_from_the_inbox_and_replies() {
    let mut app = acme_app();
    let reply_by = crate::util::now() + 60;
    request(
        "0001",
        &serde_json::json!({
            "repo": "/src/acme", "from": "✦ lead", "worktree": false, "reply_by": reply_by,
            "items": [{ "kind": "ticket", "key": "ENG-90", "title": "Export to CSV" }, { "kind": "ticket", "key": "ENG-52" }],
        }),
    );
    app.take_inbox();
    assert!(app.tickets.iter().any(|tv| tv.t.key == "ENG-90" && tv.new));
    assert_eq!(selected(&app), "ENG-52", "an added ticket doesn't take the pane");
    let lines = reply("0001");
    assert!(lines.iter().all(|l| l.ok), "{lines:?}");
    assert!(lines[1].text.contains("already open"), "{lines:?}");
    assert!(toast(&app).is_some_and(|t| t.contains("ENG-90")), "{:?}", toast(&app));
    assert!(saved().tickets.iter().any(|t| t.key == "ENG-90"));
}

#[test]
fn an_agent_from_the_inbox_waits_for_the_dialog() {
    let mut app = acme_app();
    request(
        "0002",
        &serde_json::json!({
            "repo": "/src/acme", "from": "✦ lead", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "agent", "name": "tester", "role": "Run the tests" }],
        }),
    );
    app.take_inbox();
    assert!(reply("0002").iter().all(|l| l.ok));
    assert!(
        !app.tickets.iter().any(|tv| tv.t.key == "acme-tester"),
        "added before the user approved"
    );
    app.ask_next();
    assert!(matches!(app.mode, Mode::ConfirmAgent(_)));
}

/// `taskman close` always names tickets or asks for the merged ones; a request with neither,
/// which anything able to write the inbox could leave, would otherwise close every ticket.
#[test]
fn a_close_request_naming_nothing_closes_nothing() {
    let mut app = acme_app();
    let before: Vec<String> = app.tickets.iter().map(|tv| tv.t.key.clone()).collect();
    request(
        "0004",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60, "items": [], "close": {},
        }),
    );
    app.take_inbox();
    app.close_next();
    let lines = reply("0004");
    assert!(!lines[0].ok && lines[0].text.starts_with("nothing to close"), "{lines:?}");
    let after: Vec<String> = app.tickets.iter().map(|tv| tv.t.key.clone()).collect();
    assert_eq!(after, before);
    assert!(app.closing.is_empty() && app.removing.is_empty());
}

#[test]
fn a_request_for_a_project_that_is_gone_says_so() {
    let mut app = acme_app();
    request(
        "0003",
        &serde_json::json!({
            "repo": "/src/gone", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-90" }],
        }),
    );
    app.take_inbox();
    let lines = reply("0003");
    assert!(!lines[0].ok && lines[0].text.contains("no longer in taskman"), "{lines:?}");
}

/// An agent keeps the key it was added with when its project is renamed, so `taskman add`,
/// `taskman close` and the Add form find it by its name.
#[test]
fn a_renamed_projects_agent_is_found_by_its_name() {
    let mut app = acme_app();
    app.projects[0].name = "web".into();
    let agents = |app: &App| app.tickets.iter().filter(|tv| tv.t.agent.is_some()).count();
    let before = agents(&app);
    // Asked for again, it's open: the dialog says so, and only its role changes.
    let mut req = agent_request("Review the PRs");
    req.items = vec![add::Item::Agent { name: "Lead".into(), role: Some("Review the PRs".into()) }];
    ask(&mut app, req);
    let Mode::ConfirmAgent(asked) = &app.mode else { panic!("no dialog") };
    assert!(asked.open);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(agents(&app), before);
    assert_eq!(app.tickets[index(&app, "acme-lead")].t.role.as_deref(), Some("Review the PRs"));
    // Opened by name from the Add form.
    let added = app.add_agent(0, "lead", store::Cli::Claude, None, true);
    assert!(matches!(added, Ok(crate::app::ticket::Added::Open(_))));
    assert_eq!(selected(&app), "acme-lead");
    assert_eq!(agents(&app), before);
    // Closed by name.
    request(
        "0005",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60, "close": {},
            "items": [{ "kind": "agent", "name": "lead" }],
        }),
    );
    app.take_inbox();
    app.close_next();
    settle(&mut app);
    let lines = reply("0005");
    assert!(lines.iter().all(|l| l.ok) && !lines[0].text.contains("isn't open"), "{lines:?}");
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "acme-lead"));
}

/// A new agent's key is made from the project's name as it is now, which can be an agent's key
/// from before a rename.
#[test]
fn a_new_agent_can_not_take_an_older_agents_key() {
    let mut app = acme_app();
    app.projects[0].name = "acme-release".into();
    let e = app.add_agent(0, "notes-reviewer", store::Cli::Claude, None, false).err();
    assert_eq!(
        e.as_deref(),
        Some(
            "✦ acme-release-notes-reviewer is already a session name in this project: pick another name"
        )
    );
    assert!(app.add_agent(0, "tester", store::Cli::Claude, None, false).is_ok());
    assert!(app.tickets.iter().any(|tv| tv.t.key == "acme-release-tester"));
}

/// A bug that panics a `taskman close` request's PR lookup still lets the request go on: the panel
/// stops loading and the request closes.
#[test]
fn a_close_whose_lookup_panics_still_closes() {
    let mut app = acme_app();
    panicking(&mut app);
    let i = index(&app, "ENG-70");
    request(
        "0006",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-70" }], "close": {},
        }),
    );
    app.take_inbox();
    assert!(app.tickets[i].pr.loading);
    settle_until(&mut app, |app| !app.closing.is_empty());
    assert!(!app.tickets[i].pr.loading);
    assert_eq!(app.tickets[i].pr.error.as_deref(), Some(BUG));
    app.close_next();
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-70"));
}

// ---- tickets a session's `taskman add --new` asked to create --------------------------------

/// A session's request to create a ticket whose description carries a secret.
fn new_ticket_request(id: &str) {
    request(
        id,
        &serde_json::json!({
            "repo": "/src/acme", "from": "✦ lead", "worktree": false,
            "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "new", "title": "Build log", "description": "API_KEY=sk-mock-123" }],
        }),
    );
}

/// acme's Linear, answering with a team and an issue it created, ENG-90.
fn linear(app: &mut App) -> crate::replay::Server {
    let server = crate::replay::serve(vec![
        (200, include_str!("../../../tests/fixtures/linear/team.json")),
        (
            200,
            r#"{"data":{"issueCreate":{"success":true,"issue":{"identifier":"ENG-90","url":"https://linear.app/acme/issue/ENG-90/build-log"}}}}"#,
        ),
    ]);
    let client =
        crate::tracker::Client::Linear(crate::linear::LinearConfig::at(&server.url, "lin_mock"));
    app.trackers.serve(&app.config, &app.projects[0], client);
    server
}

/// The dialog for the request taken from the inbox, as if it had been open a while.
fn asked_to_create(app: &mut App) {
    app.take_inbox();
    app.ask_next();
    let Mode::ConfirmAgent(ask) = &mut app.mode else { panic!("no dialog") };
    assert_eq!(ask.req.new_tickets(), [("Build log", "API_KEY=sk-mock-123")]);
    ask.shown = ago(crate::app::ASK_ARMS.as_secs_f32()).unwrap();
}

#[test]
fn a_sessions_new_ticket_reaches_the_tracker_only_once_approved() {
    let mut app = acme_app();
    let server = linear(&mut app);
    new_ticket_request("0007");
    asked_to_create(&mut app);
    let replied = store::state_dir().join("inbox/0007.reply.json");
    assert!(!replied.exists(), "answered before the user did");
    assert!(server.requests().is_empty(), "the tracker was asked before the user approved");

    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("Creating \"Build log\" in Linear"));
    settle_until(&mut app, |app| app.tickets.iter().any(|tv| tv.t.key == "ENG-90"));
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let input = &requests[1].json()["variables"]["input"];
    assert_eq!(
        (input["title"].as_str(), input["description"].as_str()),
        (Some("Build log"), Some("API_KEY=sk-mock-123"))
    );
    let lines = reply("0007");
    assert_eq!(
        lines[0],
        add::Line::ok("Created ENG-90: https://linear.app/acme/issue/ENG-90/build-log")
    );
    assert!(
        lines[1].ok && lines[1].text.starts_with("Added ENG-90 \"Build log\" to acme"),
        "{lines:?}"
    );
    assert_eq!(toast(&app), Some("Created ENG-90 for ✦ lead"));
    assert_eq!(selected(&app), "ENG-52", "an added ticket doesn't take the pane");
    assert!(saved().tickets.iter().any(|t| t.key == "ENG-90"));
}

#[test]
fn a_declined_new_ticket_is_never_created() {
    let mut app = acme_app();
    let server = linear(&mut app);
    new_ticket_request("0008");
    asked_to_create(&mut app);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(toast(&app), Some("Declined creating \"Build log\" from ✦ lead"));
    assert_eq!(
        reply("0008"),
        [add::Line::err(
            "the user declined creating \"Build log\" in taskman; nothing was created"
        )]
    );
    assert!(app.rx.try_recv().is_err(), "a thread started");
    assert!(server.requests().is_empty(), "the tracker was asked");
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-90"));
}

/// A project without a tracker, or one no longer in taskman, has nowhere to create the ticket:
/// the command hears so at once, and the user isn't asked.
#[test]
fn a_new_ticket_with_no_tracker_to_create_it_in_is_refused_at_once() {
    let mut app = acme_app();
    for (id, repo) in [("0009", "/src/notes"), ("0010", "/src/gone")] {
        request(
            id,
            &serde_json::json!({
                "repo": repo, "reply_by": crate::util::now() + 60,
                "items": [{ "kind": "new", "title": "Build log" }],
            }),
        );
    }
    app.take_inbox();
    assert!(app.asking.is_empty());
    let notes = reply("0009");
    assert!(!notes[0].ok && notes[0].text.starts_with("notes has no tracker"), "{notes:?}");
    let gone = reply("0010");
    assert!(!gone[0].ok && gone[0].text.contains("no longer in taskman"), "{gone:?}");
}

/// The command never sends a new ticket with others to add, but anything can write to the inbox:
/// one that reaches the adding code isn't created.
#[test]
fn a_new_ticket_is_created_only_through_the_dialog() {
    let mut app = acme_app();
    let req: add::Request = serde_json::from_value(serde_json::json!({
        "repo": "/src/acme", "reply_by": crate::util::now() + 60,
        "items": [{ "kind": "new", "title": "Build log" }],
    }))
    .unwrap();
    let (lines, toast) = app.add_requested(&req, false);
    assert!(!lines[0].ok && lines[0].text.contains("waits for the user's approval"), "{lines:?}");
    assert_eq!(toast, None);
}
