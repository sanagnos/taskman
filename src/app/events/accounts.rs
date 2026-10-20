use super::*;

// ---- accounts in Settings ------------------------------------------------------------------

/// An account saved over keeps the fields a newer taskman wrote in it, though the form rebuilds it.
#[test]
fn saving_an_account_keeps_fields_this_build_doesnt_know() {
    let mut app = acme_app();
    app.config.accounts[0].extra =
        serde_json::from_value(serde_json::json!({ "scopes": ["read"] })).unwrap();
    let account = app.config.accounts[0].clone();
    app.keep_account(store::Account { extra: store::Extra::new(), ..account }, Some(0));
    let config: serde_json::Value = serde_json::from_str(&config_file()).unwrap();
    assert_eq!(
        (&config["format"], &config["accounts"][0]["scopes"]),
        (&1.into(), &["read"].into())
    );
}

#[test]
fn adds_a_jira_account_and_saves_where_its_token_comes_from_not_the_token() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    assert_eq!(field(&app, "Provider"), "Jira");
    // Nothing that reads as done already.
    assert_eq!(field(&app, "Token"), "");
    fill(&mut app, "Email", "");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(form_error(&app).as_deref(), Some("Enter the email you sign in to Jira with"));

    // A site to test against that answers nothing: the check fails, but the token is read.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    fill(&mut app, "Site", &closed.to_string());
    fill(&mut app, "Email", "you@example.com");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "cmd:printf %s-%s jira mock");
    app.on_key(key(KeyCode::Enter));
    while matches!(&app.mode, Mode::Settings(s) if s.form.checking.is_some()) {
        settle(&mut app);
    }
    let error = form_error(&app).unwrap();
    assert!(error.ends_with(". Fix it, or press Enter again to save anyway."), "{error}");
    assert!(store::load_config().unwrap().accounts.is_empty(), "saved before the second Enter");
    app.on_key(key(KeyCode::Enter));
    assert!(
        toast(&app)
            .is_some_and(|t| t.starts_with("Saved the account without a working connection"))
    );
    let config = std::fs::read_to_string(store::config_path()).unwrap();
    let saved = store::load_config().unwrap();
    let a = &saved.accounts[1];
    assert_eq!((a.id.as_str(), a.email.as_str()), ("jira-1", "you@example.com"));
    assert_eq!(a.site, format!("https://{closed}"));
    assert_eq!(a.token, "cmd:printf %s-%s jira mock");
    assert!(!config.contains("jira-mock"), "the token itself was saved: {config}");
    // Settings shows the new account, which no project uses yet.
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], crate::app::SettingsItem::Account(1));
    assert_eq!(s.form.note.as_deref(), Some("Not used by any project"));
    assert!(!s.editing);
}

#[test]
fn adds_a_linear_account_once_its_key_is_the_workspaces() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Provider"), "Linear");
    assert_eq!(field(&app, "Token"), "");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(
        form_error(&app).as_deref(),
        Some("Enter the Linear workspace (the part after linear.app/)")
    );
    fill(&mut app, "Site", "globex");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY");

    // The key is another workspace's: said, and not saved until a second Enter.
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "initech")));
    assert_eq!(
        form_error(&app).as_deref(),
        Some(
            "This key belongs to the workspace initech, not globex. Or pick File under Keep in and paste the token there. Fix it, or press Enter again to save anyway."
        )
    );
    assert_eq!(store::load_config().map(|c| c.accounts.len()).unwrap_or(0), 0);

    // Fixed: checked again, and saved.
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY_2");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "globex")));
    assert_eq!(toast(&app), Some("✓ Connected to Linear globex as Dana Reyes"));
    let saved = store::load_config().unwrap();
    assert_eq!(saved.accounts.len(), 2);
    let a = &saved.accounts[1];
    assert_eq!((a.id.as_str(), a.site.as_str()), ("linear-2", "globex"));
    assert_eq!(a.token, "env:TASKMAN_TEST_UNSET_KEY_2");

    // The same account again is the one already there.
    app.on_key(key(KeyCode::Esc));
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    fill(&mut app, "Site", "globex");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY_2");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "globex")));
    assert_eq!(app.config.accounts.len(), 2);
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], crate::app::SettingsItem::Account(1));
}

/// The new account form offers each provider that takes an account, in `Provider::ALL`'s order,
/// and shows each one's own rows: its site's hint, an email only where it signs in with one, its
/// variable suggested and the page that makes its token.
#[test]
fn the_account_form_walks_each_provider_that_takes_an_account() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    let offered: Vec<_> = store::Provider::ALL.into_iter().filter(|p| p.needs_account()).collect();
    let labels: Vec<_> = offered.iter().map(|p| p.label().to_string()).collect();
    assert_eq!(settings_form(&app).field("Provider").unwrap().choices, labels);
    choose(&mut app, "Keep in", "Variable or command");
    focus(&mut app, "Provider");
    for p in offered.iter().chain(&offered[..1]) {
        let form = settings_form(&app);
        assert_eq!(form.value("Provider"), p.label());
        assert_eq!(form.field("Site").unwrap().hint, p.account_site_hint(), "{p:?}");
        let email = form.field("Email").unwrap();
        assert_eq!(email.hidden, !p.signs_in_with_email(), "{p:?}");
        if !email.hidden {
            assert_eq!(email.hint, p.email_hint());
        }
        let read_from = form.field("Read from").unwrap();
        assert_eq!(read_from.value, format!("env:{}", p.token_var()));
        let page = p.token_page("").unwrap();
        assert!(read_from.links.iter().any(|(_, _, url)| *url == page), "{p:?}");
        app.on_key(key(KeyCode::Right));
    }
}

#[test]
fn a_second_linear_workspace_gets_a_key_of_its_own() {
    let mut app = acme_app();
    app.config.accounts[0].token = "env:LINEAR_API_KEY".into();
    app.keychain = Some(crate::keychain::Keychain::os());
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Provider"), "Linear");
    // The keychain first; picking a variable suggests one of its own.
    assert_eq!(field(&app, "Keep in"), "Keychain");
    assert_eq!(field(&app, "Read from"), "");
    focus(&mut app, "Keep in");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Read from"), "env:LINEAR_API_KEY_2");
    fill(&mut app, "Site", "globex-eu");
    assert_eq!(field(&app, "Read from"), "env:LINEAR_API_KEY_GLOBEX_EU");
    // A source the user typed stays.
    fill(&mut app, "Read from", "cmd:pass show linear");
    fill(&mut app, "Site", "initech");
    assert_eq!(field(&app, "Read from"), "cmd:pass show linear");
    // Back to Jira, a default token follows the provider.
    fill(&mut app, "Read from", "env:LINEAR_API_KEY");
    focus(&mut app, "Provider");
    app.on_key(key(KeyCode::Left));
    assert_eq!(field(&app, "Provider"), "Jira");
    assert_eq!(field(&app, "Read from"), "env:JIRA_API_TOKEN");
}

#[test]
fn without_a_keychain_a_file_comes_first_then_a_variable() {
    let mut app = acme_app();
    app.config.accounts[0].token = "env:LINEAR_API_KEY".into();
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    fill(&mut app, "Site", "globex");
    let form = settings_form(&app);
    assert_eq!(
        form.note.as_deref(),
        Some("taskman never saves your token in config.json, only where to read it from.")
    );
    let keep_in = form.field("Keep in").unwrap();
    assert!(!keep_in.hidden);
    assert_eq!(
        (keep_in.value.as_str(), &keep_in.choices[..]),
        ("File", &["File".to_string(), "Variable or command".into()][..])
    );
    assert_eq!(keep_in.hint, "←/→: a file only you can read, or a variable or command of yours");
    let token = form.field("Token").unwrap();
    assert!(token.masked && !token.hidden && form.field("Read from").unwrap().hidden);
    assert_eq!(
        token.hint,
        format!(
            "paste it here; taskman saves it in a file only you can read, in {}",
            crate::token_file::shown_dir()
        )
    );
    // Picking a variable suggests one of its own.
    choose(&mut app, "Keep in", "Variable or command");
    let read_from = settings_form(&app).field("Read from").unwrap();
    assert_eq!(read_from.value, "env:LINEAR_API_KEY_GLOBEX");
    assert!(read_from.hint.starts_with("env:LINEAR_API_KEY_GLOBEX reads"), "{}", read_from.hint);
    // Where to get a key, and the guide's other ways.
    let links: Vec<&str> = read_from.links.iter().map(|(_, l, _)| l.as_str()).collect();
    assert_eq!(links, ["https://linear.app/globex/settings/account/security", "Storing a token"]);
}

/// A site that answers nothing, so a connection test fails at once and no tracker is reached.
fn closed_site() -> String {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string()
}

#[test]
fn saves_a_pasted_token_to_the_keychain_and_only_its_name_to_config() {
    let mut app = acme_app();
    let stub = with_keychain(&mut app);
    let label = crate::keychain::Keychain::os().label();
    prefix(&mut app, KeyCode::Char('A'));
    let form = settings_form(&app);
    assert_eq!(
        form.note.as_deref(),
        Some("taskman never saves your token in its files, only where to read it from.")
    );
    assert_eq!(field(&app, "Keep in"), "Keychain");
    assert!(form.field("Read from").unwrap().hidden);
    let token = form.field("Token").unwrap();
    assert!(token.masked && !token.hidden);
    assert_eq!(token.hint, format!("paste it here; taskman puts it in your {label}"));
    assert_eq!(token.links[0].1, "https://id.atlassian.com/manage-profile/security/api-tokens");
    fill(&mut app, "Site", &closed_site());
    fill(&mut app, "Email", "dana@example.com");

    // Not a token: refused before anything runs.
    fill(&mut app, "Token", "two words");
    submit(&mut app);
    let error = form_error(&app).unwrap();
    assert!(error.starts_with("That doesn't look like a token") && !error.contains("two"));
    assert!(!stub.join("calls").exists());

    // Saved to the keychain, read back and tested; the test fails, and the form forgets the token.
    fill(&mut app, "Token", "ATATT-mock-1");
    submit(&mut app);
    let error = form_error(&app).unwrap();
    assert!(error.ends_with(". Fix it, or press Enter again to save anyway."), "{error}");
    assert_eq!(field(&app, "Token"), "");
    assert!(store::load_config().unwrap().accounts.is_empty(), "saved before the second Enter");
    let k = crate::keychain::Keychain::os();
    assert_eq!(k.read("jira-1"), Ok("ATATT-mock-1".into()));
    submit(&mut app);
    let a = &store::load_config().unwrap().accounts[1];
    assert_eq!((a.id.as_str(), a.token.as_str()), ("jira-1", "keychain:jira-1"));
    assert!(!config_file().contains("ATATT"));
    let calls = std::fs::read_to_string(stub.join("calls")).unwrap();
    assert!(!calls.contains("ATATT"), "the token was an argument: {calls}");

    // Its settings say where the token is; Enter saves it again without pasting it.
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], crate::app::SettingsItem::Account(1));
    let form = settings_form(&app);
    assert_eq!(field(&app, "Keep in"), "Keychain");
    assert_eq!(
        form.field("Token").unwrap().hint,
        format!("in your {label}; paste a new one to replace it")
    );
    app.on_key(key(KeyCode::Enter));
    submit(&mut app);
    assert!(form_error(&app).unwrap().ends_with("press Enter again to save anyway."));
    // A new one replaces it, under the same name.
    fill(&mut app, "Token", "ATATT-mock-2");
    submit(&mut app);
    submit(&mut app);
    assert_eq!(k.read("jira-1"), Ok("ATATT-mock-2".into()));
    assert_eq!(store::load_config().unwrap().accounts[1].token, "keychain:jira-1");
    assert_eq!(app.config.accounts.len(), 2);
}

#[test]
fn without_a_keychain_saves_a_pasted_token_to_a_private_file_and_the_command_to_config() {
    use std::os::unix::fs::PermissionsExt;
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    assert_eq!(field(&app, "Keep in"), "File");
    fill(&mut app, "Site", &closed_site());
    fill(&mut app, "Email", "dana@example.com");

    // Not a token: refused before anything is written.
    fill(&mut app, "Token", "two words");
    submit(&mut app);
    let error = form_error(&app).unwrap();
    assert!(error.starts_with("That doesn't look like a token") && !error.contains("two"));
    assert!(!crate::token_file::dir().exists());

    // Saved to the file and tested, reading it as the clients will; the test fails, and the form
    // forgets the token.
    fill(&mut app, "Token", "ATATT-mock-1");
    submit(&mut app);
    let error = form_error(&app).unwrap();
    assert!(error.ends_with(". Fix it, or press Enter again to save anyway."), "{error}");
    assert!(!error.contains("Or pick"), "a file needs no other way: {error}");
    assert_eq!(field(&app, "Token"), "");
    assert!(store::load_config().unwrap().accounts.is_empty(), "saved before the second Enter");
    let path = crate::token_file::path("jira-1");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ATATT-mock-1\n");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    submit(&mut app);
    let a = &store::load_config().unwrap().accounts[1];
    assert_eq!(
        (a.id.as_str(), a.token.as_str()),
        ("jira-1", crate::token_file::source("jira-1").as_str())
    );
    assert!(a.token.starts_with("cmd:cat '"), "{}", a.token);
    assert!(!config_file().contains("ATATT"));

    // Its settings say where the token is; a new one replaces it, in the same file.
    let form = settings_form(&app);
    assert_eq!(field(&app, "Keep in"), "File");
    assert!(form.field("Read from").unwrap().hidden);
    assert_eq!(
        form.field("Token").unwrap().hint,
        "in a file in ~/.config/taskman/tokens/; paste a new one to replace it"
    );
    app.on_key(key(KeyCode::Enter));
    fill(&mut app, "Token", "ATATT-mock-2");
    submit(&mut app);
    submit(&mut app);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ATATT-mock-2\n");
    assert_eq!(
        store::load_config().unwrap().accounts[1].token,
        crate::token_file::source("jira-1")
    );
    assert_eq!(app.config.accounts.len(), 2);
}

#[test]
fn with_a_keychain_no_file_is_offered() {
    let mut app = acme_app();
    with_keychain(&mut app);
    prefix(&mut app, KeyCode::Char('A'));
    let keep_in = settings_form(&app).field("Keep in").unwrap();
    assert_eq!(keep_in.choices, ["Keychain", "Variable or command"]);
    assert_eq!(keep_in.value, "Keychain");
}

#[test]
fn an_unset_variable_offers_a_file_where_there_is_no_keychain() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    fill(&mut app, "Site", &closed_site());
    fill(&mut app, "Email", "dana@example.com");
    choose(&mut app, "Keep in", "Variable or command");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_TOKEN");
    submit(&mut app);
    let error = form_error(&app).unwrap();
    assert!(
        error.starts_with("TASKMAN_TEST_UNSET_TOKEN isn't set in the shell taskman started from")
            && error.ends_with(
                ". Or pick File under Keep in and paste the token there. Fix it, or press Enter again to save anyway."
            ),
        "{error}"
    );
}

/// The keychain check answers `keychain`, as the thread at start does.
fn probe_answers(app: &mut App, keychain: Option<crate::keychain::Keychain>) {
    app.tx.send(Msg::Keychain(keychain)).unwrap();
    app.drain_messages();
}

/// Opened before the keychain check answers: no File to save to by mistake on a machine that has
/// a keychain, only a source to type, as before there was File.
#[test]
fn before_the_keychain_check_answers_no_file_is_offered() {
    let mut app = acme_app();
    app.keychain_probed = false;
    prefix(&mut app, KeyCode::Char('A'));
    let form = settings_form(&app);
    assert_eq!(
        form.note.as_deref(),
        Some("taskman never saves your token in its files, only where to read it from.")
    );
    let keep_in = form.field("Keep in").unwrap();
    assert!(keep_in.hidden && keep_in.choices == ["Variable or command"]);
    assert!(form.field("Token").unwrap().hidden);
    assert_eq!(form.field("Read from").unwrap().value, "");
    // A pasted token can't go anywhere yet: Enter asks for a source.
    fill(&mut app, "Email", "dana@example.com");
    submit(&mut app);
    assert!(form_error(&app).unwrap().starts_with("Enter where to read the token from"));
    assert!(!crate::token_file::dir().exists());
}

#[test]
fn the_check_finding_no_keychain_offers_a_file_in_a_form_open_already() {
    let mut app = acme_app();
    app.keychain_probed = false;
    prefix(&mut app, KeyCode::Char('A'));
    probe_answers(&mut app, None);
    let form = settings_form(&app);
    assert_eq!(
        form.note.as_deref(),
        Some("taskman never saves your token in config.json, only where to read it from.")
    );
    let keep_in = form.field("Keep in").unwrap();
    assert!(!keep_in.hidden);
    assert_eq!(
        (keep_in.value.as_str(), &keep_in.choices[..]),
        ("File", &["File".to_string(), "Variable or command".into()][..])
    );
    assert!(!form.field("Token").unwrap().hidden && form.field("Read from").unwrap().hidden);
}

#[test]
fn the_check_finding_a_keychain_offers_it_in_a_form_open_already() {
    let mut app = acme_app();
    app.keychain_probed = false;
    crate::keychain::stub();
    prefix(&mut app, KeyCode::Char('A'));
    probe_answers(&mut app, Some(crate::keychain::Keychain::os()));
    let form = settings_form(&app);
    assert_eq!(
        form.note.as_deref(),
        Some("taskman never saves your token in its files, only where to read it from.")
    );
    let keep_in = form.field("Keep in").unwrap();
    assert!(!keep_in.hidden);
    assert_eq!(
        (keep_in.value.as_str(), &keep_in.choices[..]),
        ("Keychain", &["Keychain".to_string(), "Variable or command".into()][..])
    );
    assert!(!form.field("Token").unwrap().hidden && form.field("Read from").unwrap().hidden);
}

/// A source typed while the check was out stays: the answer only adds the choices.
#[test]
fn a_source_typed_before_the_check_answers_stays() {
    for keychain in [false, true] {
        let mut app = acme_app();
        app.keychain_probed = false;
        crate::keychain::stub();
        prefix(&mut app, KeyCode::Char('A'));
        fill(&mut app, "Read from", "cmd:pass show jira");
        probe_answers(&mut app, keychain.then(crate::keychain::Keychain::os));
        let form = settings_form(&app);
        let keep_in = form.field("Keep in").unwrap();
        assert_eq!(keep_in.value, "Variable or command");
        assert_eq!(keep_in.choices.len(), 2);
        assert!(!keep_in.hidden && form.field("Token").unwrap().hidden);
        assert_eq!(form.field("Read from").unwrap().value, "cmd:pass show jira");
    }
}

#[test]
fn a_jira_account_with_no_site_is_saved_to_the_keychain_untested() {
    let mut app = acme_app();
    with_keychain(&mut app);
    prefix(&mut app, KeyCode::Char('A'));
    fill(&mut app, "Email", "dana@example.com");
    submit(&mut app);
    assert_eq!(form_error(&app).as_deref(), Some("Paste the token into the Token field"));
    fill(&mut app, "Token", "ATATT-mock");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved the Jira account (add a site to test it)"));
    assert_eq!(store::load_config().unwrap().accounts[1].token, "keychain:jira-1");
    assert_eq!(crate::keychain::Keychain::os().read("jira-1"), Ok("ATATT-mock".into()));
}

#[test]
fn a_keychain_that_fails_saves_nothing() {
    let mut app = acme_app();
    let stub = with_keychain(&mut app);
    std::fs::write(stub.join("fail"), "The keyring is locked").unwrap();
    prefix(&mut app, KeyCode::Char('A'));
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Token", "ATATT-mock");
    // No "save anyway": Enter again tries again.
    for _ in 0..2 {
        submit(&mut app);
        let error = form_error(&app).unwrap();
        assert!(
            error.contains("The keyring is locked")
                && error.ends_with(". Fix it, or pick Variable or command under Keep in."),
            "{error}"
        );
    }
    assert_eq!(field(&app, "Token"), "ATATT-mock", "kept to try again");
    assert!(store::load_config().map(|c| c.accounts.len()).unwrap_or(0) < 2);
}

#[test]
fn an_unset_variable_says_how_to_set_it_and_offers_the_keychain() {
    let mut app = acme_app();
    with_keychain(&mut app);
    prefix(&mut app, KeyCode::Char('A'));
    fill(&mut app, "Site", &closed_site());
    fill(&mut app, "Email", "dana@example.com");
    focus(&mut app, "Keep in");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Read from"), "env:JIRA_API_TOKEN");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_TOKEN");
    submit(&mut app);
    let error = form_error(&app).unwrap();
    assert!(
        error.starts_with("TASKMAN_TEST_UNSET_TOKEN isn't set in the shell taskman started from: add export TASKMAN_TEST_UNSET_TOKEN=… to ")
            && error.ends_with(
                ". Or pick Keychain under Keep in and paste the token there. Fix it, or press Enter again to save anyway."
            ),
        "{error}"
    );
}

#[test]
fn deleting_an_account_can_delete_its_keychain_item() {
    let mut app = acme_app();
    with_keychain(&mut app);
    let k = crate::keychain::Keychain::os();
    k.store("jira-1", "ATATT-mock").unwrap();
    let account = serde_json::json!({ "id": "jira-1", "provider": "jira", "email": "dana@example.com",
                                      "token": "keychain:jira-1" });
    app.config.accounts.push(serde_json::from_value(account).unwrap());
    app.open_settings(crate::app::SettingsItem::Account(1), false);
    app.on_key(ctrl('d'));
    assert!(matches!(app.mode, Mode::ConfirmDeleteAccount(1)));
    // Enter keeps the token: Esc here, and `d` deletes both.
    app.on_key(key(KeyCode::Esc));
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Char('d')));
    assert_eq!(toast(&app), Some("Deleted account dana@example.com · keychain"));
    assert_eq!(store::load_config().unwrap().accounts.len(), 1);
    settle(&mut app);
    assert_eq!(
        toast(&app).map(String::from),
        Some(format!(
            "Deleted account dana@example.com · keychain and its token in your {}",
            k.label()
        ))
    );
    assert!(k.read("jira-1").is_err());
    // An account without an item of its own has no `d`.
    let account = serde_json::json!({ "id": "jira-2", "provider": "jira", "email": "dana@example.com",
                                      "token": "env:JIRA_API_TOKEN" });
    app.config.accounts.push(serde_json::from_value(account).unwrap());
    app.open_settings(crate::app::SettingsItem::Account(1), false);
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(app.mode, Mode::ConfirmDeleteAccount(1)));
}

#[test]
fn edits_an_account_in_place() {
    let mut app = acme_app();
    app.open_settings(crate::app::SettingsItem::Account(0), false);
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.form.note.as_deref(), Some("Used by acme"));
    app.on_key(key(KeyCode::Enter));
    // The provider is fixed; a token source is what changes.
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Provider"), "Linear");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "acme")));
    assert_eq!(toast(&app), Some("✓ Connected to Linear acme as Dana Reyes"));
    let saved = store::load_config().unwrap();
    assert_eq!(saved.accounts.len(), 1);
    assert_eq!(saved.accounts[0].id, "linear-1", "acme's tracker names it by its id");
    assert_eq!(saved.accounts[0].token, "env:TASKMAN_TEST_UNSET_KEY");
    assert_eq!(app.account_users(0), ["acme"]);
}

#[test]
fn deletes_an_account_no_project_uses() {
    let mut app = acme_app();
    let unused = serde_json::json!({ "id": "jira-1", "provider": "jira", "site": "https://example.atlassian.net",
                                     "email": "you@example.com", "token": "env:JIRA_API_TOKEN" });
    app.config.accounts.push(serde_json::from_value(unused).unwrap());
    app.open_settings(crate::app::SettingsItem::Account(1), false);
    app.on_key(ctrl('d'));
    assert!(matches!(app.mode, Mode::ConfirmDeleteAccount(1)));
    draw(&mut app, 80, 24);
    // Esc keeps it.
    app.on_key(key(KeyCode::Esc));
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], crate::app::SettingsItem::Account(1));
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Deleted account you@example.com · env:JIRA_API_TOKEN"));
    assert_eq!(store::load_config().unwrap().accounts.len(), 1);
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], crate::app::SettingsItem::Account(0));
}

// ---- threads that panic --------------------------------------------------------------------

/// A bug that panics the keychain check counts as no keychain, so the Keep in field stops waiting.
#[test]
fn a_keychain_check_that_panics_finds_none() {
    let mut app = acme_app();
    app.keychain_probed = false;
    panicking(&mut app);
    app.probe_keychain();
    settle_until(&mut app, |app| app.keychain_probed);
    assert!(app.keychain.is_none());
}

/// A bug that panics a token's read fails it, rather than leaving it being read for good.
#[test]
fn a_token_read_that_panics_fails() {
    let mut app = acme_app();
    panicking(&mut app);
    app.trackers.forget("env:ACME_LINEAR_TOKEN");
    app.rebuild_clients();
    let access = |app: &App| app.trackers.access(&app.config, app.projects.first());
    assert!(matches!(access(&app), crate::tracker::Access::Reading));
    settle_until(&mut app, |app| !matches!(access(app), crate::tracker::Access::Reading));
    assert!(matches!(access(&app), crate::tracker::Access::Failed(e) if e == BUG));
}
