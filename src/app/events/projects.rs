use super::*;

// ---- the project forms: sign in, then pick the project or team -----------------------------

/// The labels of the Settings form's fields shown.
fn shown(app: &App) -> Vec<&'static str> {
    settings_form(app).fields.iter().filter(|f| !f.hidden).map(|f| f.label).collect()
}

fn hint_of(app: &App, label: &str) -> String {
    settings_form(app).field(label).unwrap().hint.clone()
}

fn listed_under(app: &App, label: &str) -> Vec<String> {
    settings_form(app).field(label).unwrap().list.lines.clone()
}

/// The project saved last, as tickets.json has it.
fn saved_tracker() -> store::Tracker {
    saved().projects.last().and_then(|p| p.tracker.clone()).unwrap()
}

#[test]
fn a_new_jira_project_signs_in_then_lists_the_projects_to_pick_from() {
    let mut app = offline(Saved::default());
    let stub = with_keychain(&mut app);
    new_project(&mut app, "Jira");
    // The site, then a new account's own fields in the same form: no second form.
    assert_eq!(
        shown(&app),
        ["Path", "Tracker", "Site", "Account", "Email", "Keep in", "Token", "Project"]
    );
    assert_eq!(field(&app, "Account"), "+ new account");
    assert!(hint_of(&app, "Account").starts_with("taskman never saves your token in its files"));
    assert!(hint_of(&app, "Project").starts_with("listed once taskman can sign in"));
    fill(&mut app, "Site", "example.atlassian.net");
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Token", "ATATT-mock-1");
    // Nothing is asked while the sign-in is typed; leaving the token lists the projects.
    assert!(settings_form(&app).choices.is_none());
    app.on_key(key(KeyCode::Tab));
    assert_eq!(settings_form(&app).fields[settings_form(&app).focus].label, "Project");
    assert_eq!(listed_under(&app, "Project"), ["Listing the projects this account can see…"]);
    answer_choices(&mut app, Ok(&ENG_OPS));
    assert_eq!(listed_under(&app, "Project"), ["ENG  Engineering", "OPS  Operations"]);
    // Typing narrows them, by key or name; ↓ picks.
    type_text(&mut app, "oper");
    assert_eq!(listed_under(&app, "Project"), ["OPS  Operations"]);
    app.on_key(ctrl('u'));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert!(config_file().is_empty());

    // Enter: the token goes to the keychain, read back and tested; then both are saved.
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "")));
    assert_eq!(
        toast(&app),
        Some("Added project web · ✓ Connected to example.atlassian.net as Dana Reyes")
    );
    let a = &store::load_config().unwrap().accounts[0];
    assert_eq!(
        (a.id.as_str(), a.email.as_str(), a.site.as_str(), a.token.as_str()),
        ("jira-1", "dana@example.com", "https://example.atlassian.net", "keychain:jira-1")
    );
    assert_eq!(crate::keychain::Keychain::os().read("jira-1"), Ok("ATATT-mock-1".into()));
    assert!(!config_file().contains("ATATT"));
    let calls = std::fs::read_to_string(stub.join("calls")).unwrap();
    assert!(!calls.contains("ATATT"), "the token was an argument: {calls}");
    let t = saved_tracker();
    assert_eq!(
        (t.provider, t.site.as_str(), t.key.as_str(), t.account.as_deref()),
        (store::Provider::Jira, "https://example.atlassian.net", "OPS", Some("jira-1"))
    );
    // And the Add form opens on it.
    assert!(matches!(&app.mode, Mode::Form(f) if f.value("Project") == "web"));
}

#[test]
fn a_saved_account_that_fits_is_picked_and_lists_at_once() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    assert_eq!(
        shown(&app),
        ["Path", "Tracker", "Workspace", "Account", "Keep in", "Token", "Team"]
    );
    fill(&mut app, "Workspace", "acme");
    // acme's key is for this workspace: picked, and its fields go.
    assert_eq!(field(&app, "Account"), "acme · env:ACME_LINEAR_TOKEN");
    assert_eq!(shown(&app), ["Path", "Tracker", "Workspace", "Account", "Team"]);
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Ok(&ENG_OPS));
    fill(&mut app, "Team", "ops");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Added project web"));
    let t = saved_tracker();
    assert_eq!(
        (t.site.as_str(), t.key.as_str(), t.account.as_deref()),
        ("acme", "OPS", Some("linear-1"))
    );
    assert_eq!(app.config.accounts.len(), 1);
}

#[test]
fn a_saved_account_picked_brings_its_workspace() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    assert_eq!(field(&app, "Account"), "+ new account");
    choose(&mut app, "Account", "acme · env:ACME_LINEAR_TOKEN");
    assert_eq!(field(&app, "Workspace"), "acme");
}

#[test]
fn a_list_that_fails_says_why_and_takes_a_key_typed() {
    let mut app = offline(Saved::default());
    new_project(&mut app, "Jira");
    fill(&mut app, "Site", "example.atlassian.net");
    fill(&mut app, "Email", "dana@example.com");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_TOKEN");
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Err("Jira request failed: network unreachable"));
    let hint = hint_of(&app, "Project");
    assert!(
        hint.starts_with("Couldn't list the projects: Jira request failed: network unreachable.")
            && hint.contains("Type its key (ENG for ENG-12) to save without the list"),
        "{hint}"
    );
    assert!(listed_under(&app, "Project").is_empty());
    type_text(&mut app, "eng");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "")));
    assert_eq!(saved_tracker().key, "ENG");
    assert_eq!(store::load_config().unwrap().accounts[0].token, "env:TASKMAN_TEST_UNSET_TOKEN");
}

#[test]
fn a_connection_that_fails_saves_on_a_second_enter() {
    let mut app = offline(Saved::default());
    new_project(&mut app, "Jira");
    fill(&mut app, "Site", "example.atlassian.net");
    fill(&mut app, "Email", "dana@example.com");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_TOKEN");
    fill(&mut app, "Project", "ENG");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Err("Jira rejected the credentials (check the email and API token)"));
    assert_eq!(
        form_error(&app).as_deref(),
        Some(
            "Jira rejected the credentials (check the email and API token). Or pick File under Keep in and paste the token there. Fix it, or press Enter again to save anyway."
        )
    );
    assert!(config_file().is_empty(), "saved before the second Enter");
    app.on_key(key(KeyCode::Enter));
    let t = toast(&app).unwrap();
    assert!(
        t.starts_with("Added project web · Saved the account without a working connection"),
        "{t}"
    );
    assert_eq!(saved_tracker().account.as_deref(), Some("jira-1"));
}

#[test]
fn a_key_none_of_the_listed_is_saved_on_a_second_enter() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    fill(&mut app, "Workspace", "acme");
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Ok(&ENG_OPS));
    fill(&mut app, "Team", "QA");
    assert_eq!(
        hint_of(&app, "Team"),
        "none of the teams this account can see; Enter saves QA as typed"
    );
    submit(&mut app);
    assert_eq!(
        form_error(&app).as_deref(),
        Some(
            "QA isn't one of the 2 teams this account can see. Pick one from the list, or press Enter again to save anyway."
        )
    );
    app.on_key(key(KeyCode::Enter));
    assert_eq!(saved_tracker().key, "QA");
}

#[test]
fn the_linear_token_page_follows_the_workspace_typed() {
    let page = |app: &App| settings_form(app).field("Read from").unwrap().links[0].2.clone();
    // In a project form, from its Workspace.
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    assert_eq!(page(&app), "https://linear.app/<workspace>/settings/account/security");
    fill(&mut app, "Workspace", "globex");
    assert_eq!(page(&app), "https://linear.app/globex/settings/account/security");
    app.on_key(key(KeyCode::Backspace));
    assert_eq!(page(&app), "https://linear.app/globe/settings/account/security");
    // In an account form, from its Site.
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    assert_eq!(page(&app), "https://linear.app/<workspace>/settings/account/security");
    fill(&mut app, "Site", "globex");
    assert_eq!(page(&app), "https://linear.app/globex/settings/account/security");
}

#[test]
fn a_new_linear_account_is_the_workspaces_and_lists_its_teams() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    fill(&mut app, "Workspace", "globex");
    assert_eq!(field(&app, "Account"), "+ new account");
    // No email for Linear, and a key of its own for a second workspace.
    assert_eq!(
        shown(&app),
        ["Path", "Tracker", "Workspace", "Account", "Keep in", "Token", "Team"]
    );
    choose(&mut app, "Keep in", "Variable or command");
    assert_eq!(
        shown(&app),
        ["Path", "Tracker", "Workspace", "Account", "Keep in", "Read from", "Team"]
    );
    assert!(hint_of(&app, "Read from").starts_with("env:LINEAR_API_KEY reads"));
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY");
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Ok(&[("WEB", "Website")]));
    // The only team is picked.
    assert_eq!(field(&app, "Team"), "WEB");
    assert_eq!(hint_of(&app, "Team"), "Website · ↑/↓ for another");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "initech")));
    assert_eq!(
        form_error(&app).as_deref(),
        Some(
            "This key belongs to the workspace initech, not globex. Or pick File under Keep in and paste the token there. Fix it, or press Enter again to save anyway."
        )
    );
    // Another key: the list was for the old one, so it goes, and the team typed stays.
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY_2");
    assert!(settings_form(&app).choices.is_none());
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "globex")));
    assert_eq!(toast(&app), Some("Added project web · ✓ Connected to Linear globex as Dana Reyes"));
    let a = &store::load_config().unwrap().accounts[1];
    assert_eq!(
        (a.id.as_str(), a.site.as_str(), a.token.as_str()),
        ("linear-2", "globex", "env:TASKMAN_TEST_UNSET_KEY_2")
    );
    let t = saved_tracker();
    assert_eq!(
        (t.site.as_str(), t.key.as_str(), t.account.as_deref()),
        ("globex", "WEB", Some("linear-2"))
    );
}

#[test]
fn github_issues_and_local_files_suggest_a_prefix() {
    let mut app = offline(Saved::default());
    let dir = store::state_dir().join("web");
    std::fs::create_dir_all(&dir).unwrap();
    sh(&dir, "git init -q && git remote add origin git@github.com:acme/storefront.git");
    let board = crate::local::board(&dir, crate::local::FOLDER);
    crate::local::ensure_columns(&board).unwrap();
    std::fs::write(board.join("todo/OPS-3-fix-login.md"), "# Fix login\n").unwrap();
    prefix(&mut app, KeyCode::Char('P'));
    fill(&mut app, "Path", &dir.display().to_string());
    // GitHub Issues: from the repository's name; no account and nothing listed.
    choose(&mut app, "Tracker", "GitHub Issues");
    assert_eq!(field(&app, "Ticket prefix"), "STO");
    assert_eq!(shown(&app), ["Path", "Tracker", "Ticket prefix"]);
    // Local files: the prefix the board's tickets use already.
    choose(&mut app, "Tracker", "Local files");
    assert_eq!(field(&app, "Ticket prefix"), "OPS");
    // One typed stays.
    fill(&mut app, "Ticket prefix", "WP");
    choose(&mut app, "Tracker", "GitHub Issues");
    assert_eq!(field(&app, "Ticket prefix"), "WP");
}

#[test]
fn a_projects_settings_list_its_teams_and_change_the_prefix() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::Project(0), false);
    assert_eq!(field(&app, "Team"), "ENG");
    assert!(settings_form(&app).choices.is_none(), "listed before editing");
    // Editing: its account can sign in, so the teams are listed at once.
    app.on_key(key(KeyCode::Enter));
    answer_choices(&mut app, Ok(&ENG_OPS));
    assert_eq!(hint_of(&app, "Team"), "Engineering");
    focus(&mut app, "Team");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    // Leaving the field takes the one highlighted.
    app.on_key(key(KeyCode::Tab));
    assert_eq!(field(&app, "Team"), "OPS");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved acme"));
    let t = saved().projects[0].tracker.clone().unwrap();
    assert_eq!((t.key.as_str(), t.account.as_deref()), ("OPS", Some("linear-1")));
}

/// Fields a newer taskman wrote survive a project saved from its settings, which rebuilds the
/// project, and the save that writes the rest of tickets.json with it.
#[test]
fn saving_a_project_keeps_fields_this_build_doesnt_know() {
    use serde_json::{Value, json};
    let unknown = |v: Value| -> store::Extra { serde_json::from_value(v).unwrap() };
    let mut app = acme_app();
    app.unknown = unknown(json!({ "layout": { "sidebar": 30 } }));
    let acme = &mut app.projects[0];
    acme.extra = unknown(json!({ "colour": "teal" }));
    acme.worktrees.extra = unknown(json!({ "sparse": ["src"] }));
    acme.tracker.as_mut().unwrap().extra = unknown(json!({ "team_id": "t-1" }));
    app.tickets[2].t.extra = unknown(json!({ "priority": 2 }));
    app.prefs.extra = unknown(json!({ "theme": "light" }));
    app.open_settings(super::super::SettingsItem::Project(0), true);
    answer_choices(&mut app, Ok(&[("ENG", "Engineering")]));
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved acme"));
    let file = std::fs::read_to_string(store::state_dir().join("tickets.json")).unwrap();
    let saved: Value = serde_json::from_str(&file).unwrap();
    assert_eq!(saved["format"], 1);
    assert_eq!(saved["layout"], json!({ "sidebar": 30 }));
    let acme = &saved["projects"][0];
    assert_eq!((&acme["colour"], &acme["tracker"]["team_id"]), (&json!("teal"), &json!("t-1")));
    assert_eq!(acme["worktrees"], json!({ "sparse": ["src"] }));
    assert_eq!(saved["tickets"][2]["priority"], 2);
    assert_eq!(saved["ui"]["theme"], "light");
}

/// The Branch hint names whose branch names it takes, written as English does.
#[test]
fn the_branch_hint_names_the_trackers_own_names() {
    let mut app = acme_app();
    let local =
        serde_json::json!({ "provider": "local", "site": "", "folder": "workplan", "key": "WP" });
    app.projects[1].tracker = Some(serde_json::from_value(local).unwrap());
    for (p, own) in [(0, "(Linear's own)"), (1, "(Local files' own)")] {
        app.open_settings(crate::app::SettingsItem::Project(p), false);
        let hint = hint_of(&app, "Branch");
        assert!(hint.contains(own), "{hint}");
    }
}
