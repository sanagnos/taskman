use super::*;

// ---- forms and Settings --------------------------------------------------------------------

#[test]
fn the_add_form_opens_on_type_set_to_agent() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(focused(&app), "Type");
    assert_eq!(field(&app, "Type"), "Agent");
    // One arrow key to a ticket, whose open issues are ready under its field.
    app.on_key(key(KeyCode::Left));
    assert_eq!(field(&app, "Type"), "Ticket");
    assert!(!listed(&app).is_empty());
}

#[test]
fn typing_on_the_add_forms_choices_goes_to_its_text_field_and_arrows_still_choose() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    // ←/→ and space on Type still change the choice.
    app.on_key(key(KeyCode::Left));
    assert_eq!(field(&app, "Type"), "Ticket");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Type"), "Agent");
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(field(&app, "Type"), "Ticket");
    assert_eq!(focused(&app), "Type");
    // On any other choice too: CLI.
    focus(&mut app, "CLI");
    type_text(&mut app, "61");
    assert_eq!((focused(&app), field(&app, "Ticket").as_str()), ("Ticket", "61"));
    assert_eq!(field(&app, "CLI"), "Claude");
    // A number made it a ticket; choosing Agent again sticks, keeping what was typed.
    focus(&mut app, "Type");
    app.on_key(key(KeyCode::Right));
    assert_eq!((field(&app, "Type").as_str(), field(&app, "Name").as_str()), ("Agent", "61"));
}

#[test]
fn typing_on_a_choice_elsewhere_changes_nothing() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    let form = settings_form(&app);
    let values: Vec<String> = form.fields.iter().map(|f| f.value.clone()).collect();
    assert_eq!(form.fields[form.focus].label, "Updates");
    type_text(&mut app, "52");
    let form = settings_form(&app);
    assert_eq!(form.fields[form.focus].label, "Updates");
    assert!(form.fields.iter().map(|f| &f.value).eq(values.iter()));
}

#[test]
fn the_add_form_adds_an_agent() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(field(&app, "Project"), "acme");
    // It opens on Type, set to Agent.
    assert_eq!(focused(&app), "Type");
    assert_eq!(field(&app, "Type"), "Agent");
    // Its role, then its name, last as the ticket is.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Role");
    type_text(&mut app, "Keep the guide current");
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Name");
    type_text(&mut app, "Docs Writer");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert_eq!(selected(&app), "acme-docs-writer");
    assert_eq!(app.tickets[app.sel].t.role.as_deref(), Some("Keep the guide current"));
    // The project's agents come before its tickets.
    let order: Vec<&str> = app.members(0).iter().map(|&i| app.tickets[i].t.key.as_str()).collect();
    assert_eq!(order[..3], ["acme-lead", "acme-release-notes-reviewer", "acme-docs-writer"]);
}

#[test]
fn the_add_form_switches_to_a_ticket_already_open() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    switch_to_ticket(&mut app);
    type_text(&mut app, "x");
    app.on_key(ctrl('u'));
    type_text(&mut app, "61");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("ENG-61 is already open; switched to it"));
    assert_eq!(selected(&app), "ENG-61");
    // Esc leaves the form as it was.
    prefix(&mut app, KeyCode::Char('n'));
    switch_to_ticket(&mut app);
    type_text(&mut app, "62");
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.tickets.len(), 7);
}

#[test]
fn renames_an_agent() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('e'));
    assert_eq!(toast(&app), Some("Only agents can be renamed; a ticket is named by its key"));
    app.select(index(&app, "acme-lead"));
    prefix(&mut app, KeyCode::Char('e'));
    app.on_key(ctrl('u'));
    type_text(&mut app, "planner");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert_eq!(selected(&app), "acme-planner");
    assert!(saved().tickets.iter().any(|t| t.agent.as_deref() == Some("planner")));
}

/// Its own name again, after its project was renamed, changes nothing; another agent's is refused.
#[test]
fn a_renamed_projects_agent_keeps_its_key_under_its_own_name() {
    let mut app = acme_app();
    app.projects[0].name = "web".into();
    app.select(index(&app, "acme-lead"));
    rename(&mut app, "lead");
    assert!(matches!(app.mode, Mode::Normal), "{:?}", rename_error(&app));
    assert_eq!(selected(&app), "acme-lead");
    rename(&mut app, "release-notes-reviewer");
    assert_eq!(
        rename_error(&app),
        Some("This project already has an agent called release-notes-reviewer")
    );
}

/// Opens the Rename form on the selected agent and submits `name`.
fn rename(app: &mut App, name: &str) {
    prefix(app, KeyCode::Char('e'));
    app.on_key(ctrl('u'));
    type_text(app, name);
    app.on_key(key(KeyCode::Enter));
}

/// The Rename form's error, when it stayed open with one.
fn rename_error(app: &App) -> Option<&str> {
    match &app.mode {
        Mode::Form(form) => form.error.as_deref(),
        _ => None,
    }
}

/// Renamed, the agent's new name is claimed before the rename is saved and kept for the restart,
/// so `taskman resume` under it can't start a second copy while the old CLI is still stopping; the
/// old name's claim goes with the save, since nothing can resume the old name any more.
#[test]
fn a_rename_claims_the_new_name_before_saving_and_lets_the_old_one_go() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "acme-lead"));
    let i = app.sel;
    let (_sent, release) = attach_slow(&mut app, "acme-lead");
    let hold =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-lead")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    rename(&mut app, "planner");
    assert_eq!(toast(&app), Some("Renamed ✦ lead to ✦ planner"));
    assert!(matches!(app.mode, Mode::Normal));
    let tv = &app.tickets[i];
    assert_eq!((tv.t.key.as_str(), tv.t.agent.as_deref()), ("acme-planner", Some("planner")));
    assert_eq!(tv.status.state, AgentState::NotStarted);
    // The old CLI is still stopping; the new name is claimed, and kept for the start to come.
    assert!(tv.session.is_none() && tv.reaping() && tv.hold.is_some());
    assert_eq!(owner("acme-planner"), resume::Owner::Taskman(std::process::id()));
    assert!(
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-planner")).unwrap().is_err(),
        "taskman resume could take the new name"
    );
    // The old name is saved away, and its claim and owner file are gone.
    let keys: Vec<String> = saved().tickets.iter().map(|t| t.key.clone()).collect();
    assert!(keys.contains(&"acme-planner".to_string()) && !keys.contains(&"acme-lead".to_string()));
    assert_eq!(owner("acme-lead"), resume::Owner::Free);
    assert!(!resume::owner_file("/src/acme".as_ref(), "acme-lead").exists());
    // The tick that would start it finds the CLI still stopping: nothing starts, the claim stays.
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.away.is_none() && tv.hold.is_some(), "started too soon");
    // The CLI is gone: the next tick starts it under that claim. (The claim is handed to `taskman
    // resume` here so the test starts no real CLI: the start gets as far as taking the lock.)
    drop(release);
    reaped(&mut app);
    app.tickets[i].hold = None;
    let outside = held_outside("acme-planner");
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].away, Some(Away::Outside), "no start once the CLI was gone");
    drop(outside);
}

/// A name whose session another terminal runs can't be taken, nor given up: the rename is refused
/// with nothing changed, either way.
#[test]
fn a_rename_is_refused_while_either_name_runs_elsewhere() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "acme-lead"));
    let i = app.sel;
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    let hold =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-lead")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    // A session under the new name runs in another terminal.
    let outside = held_outside("acme-planner");
    rename(&mut app, "planner");
    assert_eq!(
        rename_error(&app),
        Some("planner is running in another terminal; quit it there first")
    );
    let tv = &app.tickets[i];
    assert_eq!(tv.t.key, "acme-lead");
    assert!(tv.session.is_some() && !tv.reaping() && tv.hold.is_some(), "the session was stopped");
    assert_eq!(owner("acme-lead"), resume::Owner::Taskman(std::process::id()));
    assert!(saved().tickets.iter().any(|t| t.key == "acme-lead"));
    drop(outside);
    app.on_key(key(KeyCode::Esc));
    // The agent's own session was taken by another terminal since the last poll.
    app.select(index(&app, "acme-release-notes-reviewer"));
    let j = app.sel;
    assert!(app.tickets[j].hold.is_none());
    let outside = held_outside("acme-release-notes-reviewer");
    rename(&mut app, "planner");
    assert_eq!(rename_error(&app), Some("It's running in another terminal: quit it there first"));
    assert_eq!(app.tickets[j].t.key, "acme-release-notes-reviewer");
    assert!(
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-planner")).unwrap().is_ok(),
        "the new name was claimed"
    );
    drop(outside);
}

/// The lock is what keeps a session to one copy, so a name whose lock can't be taken (the state
/// folder can't be written, say) isn't taken either: refused, saying why, with nothing changed.
#[test]
fn a_rename_is_refused_when_the_new_name_can_not_be_claimed() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "acme-lead"));
    let i = app.sel;
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    let hold =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-lead")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    break_lock("/src/acme".as_ref(), "acme-planner");
    rename(&mut app, "planner");
    let error = rename_error(&app).unwrap_or_default();
    assert!(error.starts_with("Couldn't take planner's session lock: "), "{error}");
    assert!(error.ends_with(" can be written"), "{error}");
    let tv = &app.tickets[i];
    assert_eq!(tv.t.key, "acme-lead");
    assert!(tv.session.is_some() && !tv.reaping() && tv.hold.is_some(), "the session was stopped");
    assert!(saved().tickets.iter().any(|t| t.key == "acme-lead"));
}

/// A rename that can't be saved changes nothing: until it's saved, `taskman resume` finds the agent
/// under its old name, so that name stays claimed and its session runs on. Submitted again once the
/// state can be saved, it renames.
#[test]
fn a_rename_that_can_not_be_saved_changes_nothing() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "acme-lead"));
    let i = app.sel;
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    let hold =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-lead")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    // An agent closed under the new name, which the rename would drop.
    let closed = store::ClosedAgent {
        name: "planner".into(),
        session_id: "sid-planner".into(),
        cli: store::Cli::Claude,
        role: None,
        prompt_snapshot_off: false,
        extra: Default::default(),
    };
    app.projects[0].closed_agents.push(closed);
    let blocked = block_saves();
    rename(&mut app, "planner");
    let error = rename_error(&app).unwrap_or_default();
    assert!(error.starts_with("✦ lead keeps its name: couldn't save "), "{error}");
    let tv = &app.tickets[i];
    assert_eq!((tv.t.key.as_str(), tv.t.agent.as_deref()), ("acme-lead", Some("lead")));
    assert!(tv.session.is_some() && !tv.reaping() && tv.hold.is_some(), "the session was stopped");
    assert_eq!(owner("acme-lead"), resume::Owner::Taskman(std::process::id()));
    assert!(
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-planner")).unwrap().is_ok(),
        "the new name stayed claimed"
    );
    assert_eq!(app.projects[0].closed_agents.len(), 1);

    std::fs::remove_dir_all(&blocked).unwrap();
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Renamed ✦ lead to ✦ planner"));
    assert!(saved().tickets.iter().any(|t| t.key == "acme-planner"));
    assert_eq!(owner("acme-lead"), resume::Owner::Free);
}

#[test]
/// Submitting the General form sets the prefix for the whole process (`keys::set_prefix`), and
/// every test running at once draws and reads keys with it. This one leaves it at ctrl+a; a test
/// that changed it would flip the hints in the snapshots and the prefix in other tests.
fn settings_saves_the_general_form() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    let before = field(&app, "Updates");
    app.on_key(key(KeyCode::Right));
    assert_ne!(field(&app, "Updates"), before);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Saved"));
    assert_eq!(crate::keys::prefix(), crate::keys::Prefix::default());
    assert!(!app.config.auto_update);
    assert!(!store::load_config().unwrap().auto_update);
    // Esc closes Settings.
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
}

/// The terminal said white; Settings → General → Theme says dark instead, drawn so at once, saved,
/// and told to the sessions running.
#[test]
fn settings_theme_overrides_the_terminal() {
    use crate::theme::{Probe, Setting};
    let mut app = acme_app();
    lay_out(&mut app, 160, 40);
    attach(&mut app, "ENG-52", "");
    app.set_probe(Probe { fg: Some([0; 3]), bg: Some([255; 3]), colorfgbg: None });
    assert!(app.theme.light);
    let told = |app: &App| {
        let tv = app.tickets.iter().find(|tv| tv.t.key == "ENG-52").unwrap();
        tv.session.as_ref().unwrap().parser.lock().unwrap().callbacks().colours.bg
    };
    assert_eq!(told(&app), [255; 3]);

    app.open_settings(crate::app::SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    for _ in 0..5 {
        app.on_key(key(KeyCode::Down));
    }
    assert_eq!(field(&app, "Theme"), "Follow the terminal");
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Theme"), "Dark");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Saved"));
    assert!(!app.theme.light);
    assert_eq!(told(&app), [0x1e; 3]);
    assert_eq!(app.config.theme, Setting::Dark);
    assert_eq!(store::load_config().unwrap().theme, Setting::Dark);
}

#[test]
fn settings_moves_through_its_list() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::Project(0), false);
    app.on_key(key(KeyCode::Down));
    assert_eq!(field(&app, "Name"), "notes");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], crate::app::SettingsItem::Account(0));
    // An account a project uses can't go.
    app.on_key(ctrl('d'));
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(
        s.form.error.as_deref(),
        Some("Used by acme. Pick another account in their settings first.")
    );
}

#[test]
fn deleting_a_project_closes_its_tickets() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::Project(1), false);
    app.on_key(ctrl('d'));
    assert!(matches!(app.mode, Mode::ConfirmDeleteProject(1)));
    draw(&mut app, 80, 24);
    // Esc goes back to Settings, the project kept.
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Settings(_)));
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Settings(_)));
    assert_eq!(app.projects.len(), 1);
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "notes-writer"));
    assert_eq!(saved().projects.len(), 1);
}

/// A project whose removal can't be saved stays, its tickets where they were, their sessions
/// running and claimed: `taskman resume` still finds them.
#[test]
fn a_project_removal_that_can_not_be_saved_keeps_it() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    // Among acme's tickets, so putting it back keeps the order.
    let writer = app.tickets.remove(index(&app, "notes-writer"));
    app.tickets.insert(2, writer);
    app.sel = 4;
    let _sent = attach(&mut app, "notes-writer", SCREEN);
    let hold = resume::take(&resume::owner_file("/src/notes".as_ref(), "notes-writer"));
    app.tickets[2].hold = Some(hold.unwrap().unwrap().taskman());
    let keys = |app: &App| app.tickets.iter().map(|tv| tv.t.key.clone()).collect::<Vec<_>>();
    let before = keys(&app);
    let blocked = block_saves();
    app.open_settings(crate::app::SettingsItem::Project(1), false);
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Enter));
    let error = toast(&app).unwrap_or_default();
    assert!(error.starts_with("Kept project notes: couldn't save "), "{error}");
    assert_eq!(app.projects.len(), 2);
    assert_eq!((keys(&app), app.sel), (before, 4));
    let tv = &app.tickets[2];
    assert!(tv.session.is_some() && tv.hold.is_some(), "the session was stopped");
    std::fs::remove_dir_all(&blocked).unwrap();
}

const RAW_TOKEN_HINT: &str = "Enter where to read the token from, like env:JIRA_API_TOKEN or cmd:op read …, not the token itself";

#[test]
fn the_account_form_never_saves_a_token_it_has_no_site_to_test() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::NewAccount, true);
    assert_eq!(field(&app, "Provider"), "Jira");
    fill(&mut app, "Email", "dana@example.com");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "jira-raw-secret");
    submit(&mut app);
    assert_eq!(settings_form(&app).error.as_deref(), Some(RAW_TOKEN_HINT));
    submit(&mut app);
    assert_eq!(settings_form(&app).error.as_deref(), Some(RAW_TOKEN_HINT));
    assert_eq!(app.config.accounts.len(), 1);
    assert!(!config_file().contains("jira-raw-secret"));
    // A source saves, and the token typed before isn't in the file.
    fill(&mut app, "Read from", "env:JIRA_API_TOKEN");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved the Jira account (add a site to test it)"));
    let saved = config_file();
    assert!(saved.contains("env:JIRA_API_TOKEN") && !saved.contains("jira-raw-secret"), "{saved}");
}

#[test]
fn a_shell_variable_saves_as_env() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::NewAccount, true);
    fill(&mut app, "Email", "dana@example.com");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", " $JIRA_API_TOKEN ");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved the Jira account (add a site to test it)"));
    let saved = store::load_config().unwrap();
    assert_eq!(saved.accounts[1].token, "env:JIRA_API_TOKEN");
    assert!(!config_file().contains("$JIRA_API_TOKEN"));
    // Anything but a plain name is refused, not run, and the form keeps it as typed.
    app.open_settings(crate::app::SettingsItem::NewAccount, true);
    fill(&mut app, "Email", "dana@example.com");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "$(printf jira-raw-secret)");
    submit(&mut app);
    assert_eq!(settings_form(&app).error.as_deref(), Some(RAW_TOKEN_HINT));
    assert_eq!(field(&app, "Read from"), "$(printf jira-raw-secret)");
    assert_eq!(store::load_config().unwrap().accounts.len(), 2);
}

#[test]
fn a_shell_variable_shows_as_env_in_the_form() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::NewAccount, true);
    // A site that answers nothing keeps the form open on the failed check.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    fill(&mut app, "Site", &closed.to_string());
    fill(&mut app, "Email", "dana@example.com");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "${JIRA_API_TOKEN}");
    submit(&mut app);
    assert!(settings_form(&app).error.is_some());
    assert_eq!(field(&app, "Read from"), "env:JIRA_API_TOKEN");
}

#[test]
fn saving_anyway_never_saves_a_token() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::Account(0), true);
    assert_eq!(field(&app, "Provider"), "Linear");
    fill(&mut app, "Read from", "lin_api_raw_secret");
    // Save anyway is for a connection that failed: Enter again still refuses, and no test ran.
    for _ in 0..2 {
        submit(&mut app);
        assert_eq!(
            settings_form(&app).error.as_deref(),
            Some(
                "Enter where to read the token from, like env:LINEAR_API_KEY or cmd:op read …, not the token itself"
            )
        );
    }
    assert_eq!(app.config.accounts[0].token, "env:ACME_LINEAR_TOKEN");
    assert!(!config_file().contains("lin_api_raw_secret"));
}

#[test]
fn an_account_saved_with_a_token_says_what_to_change() {
    let mut app = acme_app();
    let old = serde_json::json!({ "id": "jira-1", "provider": "jira", "email": "dana@example.com", "token": "jira-raw-secret" });
    app.config.accounts.push(serde_json::from_value(old).unwrap());
    app.open_settings(crate::app::SettingsItem::Account(1), false);
    let error = settings_form(&app).error.clone().unwrap_or_default();
    assert_eq!(error, format!("{RAW_TOKEN_HINT}, so config.json no longer keeps it"));
    // Using it says so too, without the token.
    let used = app.config.accounts[1].resolve_token().unwrap_err();
    assert!(used.contains("env:VAR or cmd:COMMAND") && !used.contains("jira-raw-secret"), "{used}");
}

/// A bug that panics a form's check still ends it: the form says so, and Enter checks again.
#[test]
fn a_check_that_panics_says_so_and_enter_checks_again() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    fill(&mut app, "Site", "globex");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY");
    panicking(&mut app);
    let checking = |app: &App| settings_form(app).checking.is_some();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Enter));
    assert!(checking(&app));
    settle_until(&mut app, |app| !checking(app));
    assert_eq!(form_error(&app).as_deref(), Some(BUG));
    let log = "~/.local/state/taskman/panics.log";
    assert!(logged_error(
        &app,
        &format!("taskman hit a bug testing the connection; details in {log}")
    ));
    assert!(app.config.accounts.len() == 1, "saved without the check");
    app.on_key(key(KeyCode::Enter));
    assert!(checking(&app));
    settle_until(&mut app, |app| !checking(app));
}

// ---- text fields: a cursor ------------------------------------------------------------------

/// The focused field's value, with `|` where its cursor is.
fn with_cursor(app: &App) -> String {
    let form = match &app.mode {
        Mode::Form(f) => f,
        Mode::Settings(s) => &s.form,
        _ => panic!("no form open"),
    };
    let f = &form.fields[form.focus];
    let mut value = f.value.clone();
    value.insert(f.cursor(), '|');
    value
}

fn with(code: KeyCode, modifiers: KeyModifiers) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, modifiers)
}

#[test]
fn text_fields_edit_at_the_cursor() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "docs writer");
    assert_eq!((focused(&app), with_cursor(&app).as_str()), ("Name", "docs writer|"));
    // ←/→ move the cursor in text; they change no choice.
    for _ in 0..7 {
        app.on_key(key(KeyCode::Left));
    }
    assert_eq!(with_cursor(&app), "docs| writer");
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Backspace));
    type_text(&mut app, "-x");
    assert_eq!(with_cursor(&app), "docs-x|writer");
    app.on_key(key(KeyCode::Backspace));
    app.on_key(key(KeyCode::Delete));
    assert_eq!(with_cursor(&app), "docs-|riter");
    app.on_key(with(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(with_cursor(&app), "|docs-riter");
    app.on_key(with(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(with_cursor(&app), "docs|-riter");
    app.on_key(key(KeyCode::End));
    app.on_key(ctrl('w'));
    assert_eq!(with_cursor(&app), "docs-|");
    // A paste goes in at the cursor too.
    app.on_key(key(KeyCode::Home));
    app.on_paste("acme-");
    assert_eq!(with_cursor(&app), "acme-|docs-");
    app.on_key(ctrl('u'));
    assert_eq!(with_cursor(&app), "|");
    assert_eq!((focused(&app), field(&app, "Type").as_str()), ("Name", "Agent"));
}

/// Branch's presets, listed under it while it's focused. One picked puts the cursor at the end, as
/// any change but the keys' does.
#[test]
fn branchs_presets_are_listed_under_it_to_pick() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::Project(0), true);
    answer_choices(&mut app, Ok(&[("ENG", "Engineering")]));
    focus(&mut app, "Branch");
    let presets = crate::worktree::BRANCH_PRESETS.map(String::from);
    let listed = |app: &App| settings_form(app).field("Branch").unwrap().list.lines.clone();
    assert_eq!(listed(&app), presets);
    // ←/→ move the cursor rather than cycle the presets.
    app.on_key(key(KeyCode::Left));
    assert_eq!(with_cursor(&app), "{tracker|}");
    // ↓ highlights one, and Enter puts it in the field, to edit or save.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(with_cursor(&app), "{KEY}-{slug}|");
    assert!(toast(&app).is_none(), "saved on picking");
    assert_eq!(listed(&app), presets);
    app.on_key(key(KeyCode::Home));
    type_text(&mut app, "fix/");
    assert_eq!(with_cursor(&app), "fix/|{KEY}-{slug}");
    // Leaving with one highlighted takes it; leaving drops the list.
    for _ in 0..3 {
        app.on_key(key(KeyCode::Down));
    }
    app.on_key(key(KeyCode::Tab));
    assert_eq!(field(&app, "Branch"), "feature/{KEY}-{slug}");
    assert!(focused(&app) != "Branch" && listed(&app).is_empty());
    // ↑ from the first preset goes back to what's typed, and on to the field above, taking none.
    focus(&mut app, "Branch");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Up));
    assert_eq!(field(&app, "Branch"), "feature/{KEY}-{slug}");
    assert_eq!(focused(&app), "New tickets");
}

/// A token's cursor moves as any text's, but its word moves stop at nothing in it.
#[test]
fn a_masked_fields_word_moves_go_to_its_ends() {
    let mut app = acme_app();
    with_keychain(&mut app);
    prefix(&mut app, KeyCode::Char('A'));
    fill(&mut app, "Token", "ATATT-mock-1");
    app.on_key(key(KeyCode::Left));
    app.on_key(key(KeyCode::Backspace));
    assert_eq!(with_cursor(&app), "ATATT-mock|1");
    app.on_key(with(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(with_cursor(&app), "|ATATT-mock1");
    app.on_key(with(KeyCode::Right, KeyModifiers::CONTROL));
    app.on_key(ctrl('w'));
    assert_eq!(with_cursor(&app), "|");
}
