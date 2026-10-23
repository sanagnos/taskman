use super::*;

// ---- GitHub Issues -------------------------------------------------------------------------

/// `acme_app` with project web on GitHub Issues (acme/web, prefix WEB), whose folder isn't a git
/// repository, so nothing runs git, and whose open issues are listed already, so the Add form
/// doesn't ask gh.
fn web_app() -> App {
    let mut app = acme_app();
    let web = serde_json::json!({
        "name": "web", "repo": "/src/web",
        "tracker": { "provider": "github", "site": "acme/web", "key": "WEB" },
    });
    app.projects.push(serde_json::from_value(web).unwrap());
    app.github.insert("/src/web".into(), crate::github::Gh::NotGit);
    let issue = |number, title: &str, mine| crate::tracker::Listed {
        key: format!("WEB-{number}"),
        title: title.into(),
        mine,
    };
    let list = vec![
        issue(9, "Crash on start", true),
        issue(15, "Fix the login form", false),
        issue(14, "Fix the search page", false),
    ];
    let listed = crate::app::add_form::OpenIssues::Listed(list);
    app.open_issues.insert("/src/web".into(), (std::time::Instant::now(), listed));
    app
}

/// The Add form's error.
fn add_error(app: &App) -> Option<&str> {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.error.as_deref()
}

fn open_in_web(app: &App, key: &str) -> bool {
    app.tickets.iter().any(|tv| tv.t.is(std::path::Path::new("/src/web"), key))
}

#[test]
fn the_add_form_lists_open_issues_and_adds_one_by_its_title() {
    let mut app = web_app();
    let last = app.projects.len() - 1;
    open_ticket_form_on_ticket(&mut app, last);
    assert_eq!(field(&app, "Project"), "web");
    assert_eq!(
        listed(&app),
        [
            "#9 Crash on start · assigned to you",
            "#15 Fix the login form",
            "#14 Fix the search page"
        ]
    );
    type_text(&mut app, "fix");
    assert_eq!(listed(&app), ["#15 Fix the login form", "#14 Fix the search page"]);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(add_error(&app), Some("2 open issues match; type more of the title, or the number"));
    type_text(&mut app, " LOGIN");
    assert_eq!(listed(&app), ["#15 Fix the login form"]);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(open_in_web(&app, "WEB-15"));
    assert!(saved().tickets.iter().any(|t| t.key == "WEB-15" && t.repo.as_os_str() == "/src/web"));
}

#[test]
fn the_add_form_takes_issue_numbers_and_links_and_refuses_pull_requests() {
    let mut app = web_app();
    let last = app.projects.len() - 1;
    open_ticket_form_on_ticket(&mut app, last);
    // Digits list the issues whose number starts with them.
    type_text(&mut app, "1");
    assert_eq!(listed(&app), ["#15 Fix the login form", "#14 Fix the search page"]);
    app.on_key(ctrl('u'));
    type_text(&mut app, "https://github.com/acme/web/pull/45");
    let pr = "acme/web#45 is a pull request; add the issue it closes instead";
    assert_eq!(listed(&app), [pr]);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(add_error(&app), Some(pr));
    app.on_key(ctrl('u'));
    type_text(&mut app, "https://github.com/acme/web/issues/9");
    assert_eq!(listed(&app), ["#9 Crash on start · assigned to you"]);
    app.on_key(ctrl('u'));
    // A closed issue isn't listed, and is added all the same.
    type_text(&mut app, "#3");
    assert_eq!(listed(&app), ["No open issue's number starts with 3; Enter adds it as it is"]);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(open_in_web(&app, "WEB-3"));
}

#[test]
fn the_add_form_says_when_the_open_issues_could_not_be_listed() {
    let mut app = web_app();
    let failed =
        crate::app::add_form::OpenIssues::Failed("gh isn't logged in; run gh auth login".into());
    app.open_issues.insert("/src/web".into(), (std::time::Instant::now(), failed));
    let last = app.projects.len() - 1;
    open_ticket_form_on_ticket(&mut app, last);
    assert_eq!(
        listed(&app),
        ["Couldn't list the open issues: gh isn't logged in; run gh auth login"]
    );
    type_text(&mut app, "crash");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(add_error(&app), Some("Couldn't list the open issues; type the issue's number"));
    // A list that arrives while the form is open shows at once.
    let listing = app.start_listing(app.projects.len() - 1).unwrap();
    app.open_issues_listed(
        listing,
        Ok(vec![crate::tracker::Listed {
            key: "WEB-9".into(),
            title: "Crash on start".into(),
            mine: false,
        }]),
    );
    assert_eq!(listed(&app), ["#9 Crash on start"]);
}

/// An open issue, as gh lists it.
fn open_issue(number: u64, title: &str) -> crate::tracker::Listed {
    crate::tracker::Listed { key: format!("WEB-{number}"), title: title.into(), mine: false }
}

#[test]
fn a_list_of_open_issues_from_before_the_tracker_changed_is_dropped() {
    let mut app = web_app();
    let p = app.projects.len() - 1;
    app.open_issues.remove(std::path::Path::new("/src/web"));
    let old = app.start_listing(p).unwrap();
    // The project moves to another repository while acme/web's issues are listed, and lists the
    // new one's.
    let mut project = app.projects[p].clone();
    project.tracker.as_mut().unwrap().site = "acme/shop".into();
    app.update_project(p, project);
    let new = app.start_listing(p).unwrap();
    app.tx.send(Msg::OpenIssues(new, Ok(vec![open_issue(4, "Checkout is slow")]))).unwrap();
    // acme/web's list lands last: its numbers aren't acme/shop's issues.
    app.tx.send(Msg::OpenIssues(old, Ok(vec![open_issue(15, "Fix the login form")]))).unwrap();
    app.drain_messages();
    app.mode = Mode::Normal;
    open_ticket_form_on_ticket(&mut app, p);
    assert_eq!(listed(&app), ["#4 Checkout is slow"]);
    type_text(&mut app, "login");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(
        add_error(&app),
        Some("No open issue's title has \"login\"; type the issue's number")
    );
    assert!(!open_in_web(&app, "WEB-15"));
}

#[test]
fn a_list_of_open_issues_for_a_deleted_project_is_dropped() {
    let mut app = web_app();
    let p = app.projects.len() - 1;
    let web = std::path::Path::new("/src/web");
    app.open_issues.remove(web);
    let old = app.start_listing(p).unwrap();
    let project = app.projects[p].clone();
    app.delete_project(p);
    let late = || Ok(vec![open_issue(15, "Fix the login form")]);
    app.tx.send(Msg::OpenIssues(old.clone(), late())).unwrap();
    app.drain_messages();
    assert!(!app.open_issues.contains_key(web));
    // Added again, the project waits on a list of its own; the old one, landing late, isn't it.
    app.projects.push(project);
    let new = app.start_listing(p).unwrap();
    app.tx.send(Msg::OpenIssues(old, late())).unwrap();
    app.tx.send(Msg::OpenIssues(new, Ok(vec![open_issue(9, "Crash on start")]))).unwrap();
    app.drain_messages();
    open_ticket_form_on_ticket(&mut app, p);
    assert_eq!(listed(&app), ["#9 Crash on start"]);
}

// ---- Open issues on every tracker, picked with ↑/↓ ----------------------------------------------

/// An open issue of acme's (`ENG`).
fn eng(n: u64, title: &str, mine: bool) -> crate::tracker::Listed {
    crate::tracker::Listed { key: format!("ENG-{n}"), title: title.into(), mine }
}

/// `acme_app` with the Add form open on project acme, whose open issues are `issues`, listed
/// already so nothing asks the tracker; its folder isn't a git repository, so nothing runs git.
fn adding_in_acme(issues: Vec<crate::tracker::Listed>) -> App {
    let mut app = acme_app();
    app.github.insert("/src/acme".into(), crate::github::Gh::NotGit);
    let listed = crate::app::add_form::OpenIssues::Listed(issues);
    app.open_issues.insert("/src/acme".into(), (std::time::Instant::now(), listed));
    open_ticket_form_on_ticket(&mut app, 0);
    app
}

/// The Add form's open-issue list, with its highlight.
fn open_list(app: &App) -> crate::app::List {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.field("Ticket").unwrap().list.clone()
}

/// A field's hint in the open form.
fn hint(app: &App, label: &str) -> String {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.field(label).unwrap().hint.clone()
}

fn open_in_acme(app: &App, key: &str) -> bool {
    app.tickets.iter().any(|tv| tv.t.is(std::path::Path::new("/src/acme"), key))
}

#[test]
fn the_add_form_lists_linear_and_jira_issues_by_their_keys() {
    let issues = || {
        vec![
            eng(64, "Fix the login page", true),
            eng(71, "Export to CSV", false),
            eng(58, "Slow search", false),
        ]
    };
    let mut app = adding_in_acme(issues());
    assert_eq!(
        listed(&app),
        [
            "ENG-64 Fix the login page · assigned to you",
            "ENG-71 Export to CSV",
            "ENG-58 Slow search"
        ]
    );
    assert_eq!(
        hint(&app, "Ticket"),
        "a number (ENG-12), a key, a link, or words from an open issue's title"
    );
    type_text(&mut app, "csv");
    assert_eq!(listed(&app), ["ENG-71 Export to CSV"]);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(open_in_acme(&app, "ENG-71"));

    // On Jira, the same: keys name an issue, open or not.
    let mut app = acme_app();
    let jira = r#"{ "provider": "jira", "site": "https://example.atlassian.net", "key": "ENG" }"#;
    app.projects[0].tracker = Some(serde_json::from_str(jira).unwrap());
    app.github.insert("/src/acme".into(), crate::github::Gh::NotGit);
    let listed_now = crate::app::add_form::OpenIssues::Listed(issues());
    app.open_issues.insert("/src/acme".into(), (std::time::Instant::now(), listed_now));
    open_ticket_form_on_ticket(&mut app, 0);
    type_text(&mut app, "eng-58");
    assert_eq!(listed(&app), ["ENG-58 Slow search"]);
    app.on_key(ctrl('u'));
    type_text(&mut app, "ENG-99");
    assert_eq!(listed(&app), ["ENG-99 isn't an open issue; Enter adds it as it is"]);
    app.on_key(ctrl('u'));
    type_text(&mut app, "6");
    assert_eq!(listed(&app), ["ENG-64 Fix the login page · assigned to you"]);
}

#[test]
fn the_add_form_lists_a_boards_tickets_not_done_and_adds_the_one_picked() {
    let dir = store::state_dir().join("plan");
    let board = crate::local::board(&dir, crate::local::FOLDER);
    crate::local::ensure_columns(&board).unwrap();
    let write = |path: &str, content: &str| std::fs::write(board.join(path), content).unwrap();
    write("todo/WP-1-fix-login.md", "# Fix the login form\n");
    write("doing/WP-2-export.md", "# Export to CSV\n");
    write("done/WP-3-old.md", "# Fix the old login\n");
    let mut app = acme_app();
    let plan = serde_json::json!({
        "name": "plan", "repo": dir,
        "tracker": { "provider": "local", "site": "", "key": "WP" },
    });
    app.projects.push(serde_json::from_value(plan).unwrap());
    app.github.insert(dir.clone(), crate::github::Gh::NotGit);
    let p = app.projects.len() - 1;
    open_ticket_form_on_ticket(&mut app, p);
    // Read from the board in the background, as a tracker's are fetched.
    settle(&mut app);
    assert_eq!(listed(&app), ["WP-1 Fix the login form", "WP-2 Export to CSV"]);
    // A title still makes a new ticket, the list showing the ones it's like.
    type_text(&mut app, "login");
    assert_eq!(listed(&app), ["WP-1 Fix the login form"]);
    assert_eq!(hint(&app, "Ticket"), "new: WP-4 in todo, titled as typed");
    // Picked, the ticket on the board is added instead.
    app.on_key(key(KeyCode::Down));
    assert_eq!(hint(&app, "Ticket"), "workplan/todo/WP-1-fix-login.md in todo");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(app.tickets.iter().any(|tv| tv.t.is(&dir, "WP-1")));
    assert_eq!(crate::local::files(&board).len(), 3);
}

/// Eight open issues: five shown at a time.
fn eight() -> Vec<crate::tracker::Listed> {
    (1..=8).map(|n| eng(n, &format!("Task {n}"), false)).collect()
}

#[test]
fn arrows_move_a_highlight_through_the_open_issues_and_scroll_past_five() {
    let mut app = adding_in_acme(eight());
    let lines = |first: u64, last: u64| (first..=last).map(|n| format!("ENG-{n} Task {n}"));
    let window = |first: u64, more: &str| lines(first, first + 4).chain([more.to_string()]);
    assert_eq!(listed(&app), window(1, "↓ 3 more; type to narrow").collect::<Vec<_>>());
    assert_eq!(open_list(&app).highlighted(), None);
    // The first ↓ highlights the first issue; the field keeps the focus.
    app.on_key(key(KeyCode::Down));
    assert_eq!((open_list(&app).at, open_list(&app).highlighted()), (Some(0), Some(0)));
    assert_eq!(focused(&app), "Ticket");
    for _ in 0..5 {
        app.on_key(key(KeyCode::Down));
    }
    // Past the fifth, the window scrolls to keep it in view, its counts above and below.
    assert_eq!(open_list(&app).at, Some(5));
    let mid = "↑ 1 more · ↓ 2 more; type to narrow";
    assert_eq!(listed(&app), window(2, mid).collect::<Vec<_>>());
    assert_eq!(open_list(&app).highlighted(), Some(4));
    for _ in 0..2 {
        app.on_key(key(KeyCode::Down));
    }
    // The last; ↓ past it leaves the field (`arrows_past_a_list_leave_the_field_and_pick_nothing`).
    assert_eq!(open_list(&app).at, Some(7));
    assert_eq!(listed(&app), window(4, "↑ 3 more; type to narrow").collect::<Vec<_>>());
    // ↑ moves back up; the window moves only once the highlight leaves it.
    app.on_key(key(KeyCode::Up));
    assert_eq!(listed(&app)[0], "ENG-4 Task 4");
    for _ in 0..4 {
        app.on_key(key(KeyCode::Up));
    }
    assert_eq!(open_list(&app).at, Some(2));
    assert_eq!(listed(&app), window(3, "↑ 2 more · ↓ 1 more; type to narrow").collect::<Vec<_>>());
    assert_eq!(open_list(&app).highlighted(), Some(0));
    // Enter adds the highlighted issue, though nothing was typed.
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(open_in_acme(&app, "ENG-3"));
}

#[test]
fn arrows_past_a_list_leave_the_field_and_pick_nothing() {
    let mut app = adding_in_acme(eight());
    assert_eq!(focused(&app), "Ticket");
    type_text(&mut app, "task");
    // ↑ on the first issue goes back to what's typed; ↑ again to the field above, Base, and on
    // up through the fields, the typed text kept.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Up));
    assert_eq!((focused(&app), open_list(&app).at), ("Ticket", None));
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Base");
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Worktree");
    assert_eq!(field(&app, "Ticket"), "task");
    // ↓ comes back down to the field, then into the list; Ticket is last, so ↓ at its last
    // issue stays there rather than wrapping round to the top.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Ticket");
    for _ in 0..8 {
        app.on_key(key(KeyCode::Down));
    }
    assert_eq!((focused(&app), open_list(&app).at), ("Ticket", Some(7)));
    app.on_key(key(KeyCode::Down));
    assert_eq!((focused(&app), open_list(&app).at), ("Ticket", Some(7)));
    // Tab leaves a highlighted list too, and keeps the issue picked, as before.
    app.on_key(ctrl('u'));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Tab));
    assert_eq!(focused(&app), "Project");
    app.on_key(key(KeyCode::Enter));
    assert!(open_in_acme(&app, "ENG-2"), "{:?}", toast(&app));
}

#[test]
fn arrows_past_the_project_list_leave_the_field() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    fill(&mut app, "Workspace", "acme");
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Ok(&ENG_OPS));
    focus(&mut app, "Team");
    let at = |app: &App| settings_form(app).field("Team").unwrap().list.at;
    let focused = |app: &App| settings_form(app).fields[settings_form(app).focus].label;
    // ↑ out of the top: back to what's typed, then the field above.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Up));
    assert_eq!((focused(&app), at(&app)), ("Team", None));
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Account");
    // Team is the form's last field: ↓ at the bottom of its list stays there, nothing taken.
    focus(&mut app, "Team");
    type_text(&mut app, "o");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!((focused(&app), at(&app)), ("Team", Some(0)));
    assert_eq!(field(&app, "Team"), "o");
    // Tab out of a highlighted list takes the one highlighted.
    focus(&mut app, "Team");
    app.on_key(ctrl('u'));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Tab));
    assert_eq!((focused(&app), field(&app, "Team").as_str()), ("Path", "OPS"));
}

#[test]
fn typing_drops_the_highlight_and_arrows_elsewhere_move_between_fields() {
    let mut app = adding_in_acme(eight());
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(open_list(&app).at, Some(1));
    // Typing narrows the list and starts again, nothing highlighted.
    type_text(&mut app, "task");
    assert_eq!((open_list(&app).at, open_list(&app).top), (None, 0));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(add_error(&app), Some("8 open issues match; type more of the title, or the number"));
    type_text(&mut app, " 7");
    app.on_key(key(KeyCode::Down));
    assert_eq!(open_list(&app).highlighted(), Some(0));
    app.on_key(key(KeyCode::Backspace));
    assert_eq!(open_list(&app).at, None);
    // With no issue listed, ↓ and ↑ move between fields as before.
    app.on_key(ctrl('u'));
    type_text(&mut app, "nothing like it");
    assert_eq!(listed(&app), ["No open issue's title has those words"]);
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Base");
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Ticket");
    app.on_key(ctrl('u'));
    assert_eq!(open_list(&app).picks, 8);
    // Away from the Ticket field, they move between fields though the list shows.
    app.on_key(key(KeyCode::BackTab));
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Worktree");
    assert_eq!(open_list(&app).at, None);
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Ticket");
    assert_eq!(open_list(&app).at, None);
}

#[test]
fn github_issues_are_added_and_closed_through_the_inbox() {
    let mut app = web_app();
    request(
        "web-add",
        &serde_json::json!({
            "repo": "/src/web", "from": "✦ lead", "worktree": false, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "WEB-12", "title": "Login fails with SSO" }],
        }),
    );
    app.take_inbox();
    assert!(reply("web-add").iter().all(|l| l.ok));
    assert!(open_in_web(&app, "WEB-12"));
    // No worktree and nothing merged asked for, so it closes without a PR lookup.
    request(
        "web-close",
        &serde_json::json!({
            "repo": "/src/web", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "WEB-12" }], "close": {},
        }),
    );
    app.take_inbox();
    app.close_next();
    app.drain_messages();
    assert!(!open_in_web(&app, "WEB-12"));
    let lines = reply("web-close");
    assert!(lines[0].ok && lines[0].text.starts_with("Closed WEB-12"), "{lines:?}");
    assert!(!saved().tickets.iter().any(|t| t.key == "WEB-12"));
}

#[test]
fn a_github_issues_tracker_takes_its_repository_from_the_remote() {
    let dir = store::state_dir().join("web");
    std::fs::create_dir_all(&dir).unwrap();
    // An SSH alias, as people with a work and a personal account set up, is github.com to gh.
    sh(&dir, "git init -q && git remote add origin git@github.com-work:acme/web.git");
    let mut app = acme_app();
    let web = serde_json::json!({ "name": "web", "repo": dir });
    app.projects.push(serde_json::from_value(web).unwrap());
    let p = app.projects.len() - 1;
    app.open_settings(crate::app::SettingsItem::Project(p), true);
    choose(&mut app, "Tracker", "GitHub Issues");
    assert!(settings_form(&app).field("Site").unwrap().hidden);
    assert!(settings_form(&app).field("Account").unwrap().hidden);
    assert_eq!(settings_form(&app).value("Ticket prefix"), "WEB");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved web"));
    let t = app.projects[p].tracker.clone().unwrap();
    assert_eq!(
        (t.provider, t.site.as_str(), t.key.as_str()),
        (store::Provider::Github, "acme/web", "WEB")
    );
    assert_eq!(
        app.issue_url(
            &serde_json::from_value(
                serde_json::json!({ "key": "WEB-12", "repo": dir, "cwd": dir })
            )
            .unwrap()
        )
        .as_deref(),
        Some("https://github.com/acme/web/issues/12")
    );
    // Without a GitHub remote it can't be saved, and the form says why.
    sh(&dir, "git remote remove origin");
    // Saving left the form; Enter edits it again.
    app.on_key(key(KeyCode::Enter));
    submit(&mut app);
    let e = settings_form(&app).error.clone().unwrap_or_default();
    assert!(
        e.starts_with(
            "GitHub Issues reads the repository from the project's GitHub remote, and it has none"
        ),
        "{e}"
    );
}

#[test]
fn an_empty_start_takes_the_keys_bare_and_after_the_prefix() {
    // The welcome screen names the prefix form, which works anywhere; bare keys still work here.
    let empty = || offline(Saved::default());
    for press in [
        |app: &mut App| app.on_key(key(KeyCode::Char('P'))),
        |app: &mut App| prefix(app, KeyCode::Char('P')),
    ] {
        let mut app = empty();
        press(&mut app);
        assert!(matches!(app.mode, Mode::Settings(_)));
    }
    let with_a_project = || offline(Saved { tickets: vec![], ..crate::app::test_support::acme() });
    for press in [
        |app: &mut App| app.on_key(key(KeyCode::Char('n'))),
        |app: &mut App| prefix(app, KeyCode::Char('n')),
    ] {
        let mut app = with_a_project();
        press(&mut app);
        assert!(matches!(app.mode, Mode::Form(_)));
    }
}

// ---- Typing on Type ------------------------------------------------------------------------

#[test]
fn a_number_typed_on_type_fills_the_ticket() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(focused(&app), "Type");
    type_text(&mut app, "52");
    assert_eq!(focused(&app), "Ticket");
    assert_eq!(field(&app, "Type"), "Ticket");
    assert_eq!(field(&app, "Ticket"), "52");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("ENG-52 is already open; switched to it"));
}

#[test]
fn a_name_typed_on_type_fills_the_agents_name() {
    let mut saved = crate::app::test_support::acme();
    saved.projects.retain(|p| p.tracker.is_none());
    saved.tickets.clear();
    let mut app = offline(saved);
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(focused(&app), "Type");
    type_text(&mut app, "lead");
    assert_eq!(focused(&app), "Name");
    assert_eq!(field(&app, "Type"), "Agent");
    assert_eq!(field(&app, "Name"), "lead");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert_eq!(selected(&app), "notes-lead");
}

/// On a project with a tracker, a title typed on Type is an agent's name, and the hint says how
/// to get a ticket instead; on a board, ← to Ticket makes the title a new ticket.
#[test]
fn a_title_on_a_board_is_a_name_until_type_is_ticket() {
    let dir = store::state_dir().join("plan");
    let board = crate::local::board(&dir, crate::local::FOLDER);
    crate::local::ensure_columns(&board).unwrap();
    let mut saved = crate::app::test_support::acme();
    saved.projects.retain(|p| p.tracker.is_none());
    saved.tickets.clear();
    let mut app = offline(saved);
    let plan = serde_json::json!({
        "name": "plan", "repo": dir,
        "tracker": { "provider": "local", "site": "", "key": "WP" },
    });
    app.projects.insert(0, serde_json::from_value(plan).unwrap());
    app.github.insert(dir.clone(), crate::github::Gh::NotGit);
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(field(&app, "Project"), "plan");
    settle(&mut app);
    type_text(&mut app, "Fix login");
    assert_eq!((focused(&app), field(&app, "Type").as_str()), ("Name", "Agent"));
    assert_eq!(hint(&app, "Name"), "an agent's name, e.g. oss-planner; ←/→ on Type for a ticket");
    focus(&mut app, "Type");
    app.on_key(key(KeyCode::Left));
    assert_eq!(field(&app, "Ticket"), "Fix login");
    assert_eq!(hint(&app, "Ticket"), "new: WP-1 in todo, titled as typed");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(app.tickets.iter().any(|tv| tv.t.is(&dir, "WP-1")));
    assert!(app.tickets.iter().all(|tv| tv.t.agent.is_none()), "made an agent");
    let todo: Vec<_> = std::fs::read_dir(board.join("todo")).unwrap().flatten().collect();
    assert_eq!(todo.len(), 1);
}

/// A key made by typing in the middle of a name switches to a ticket too, with the cursor where
/// it was; and what's typed on a choice goes in at that cursor.
#[test]
fn a_key_typed_mid_name_switches_to_a_ticket_keeping_the_cursor() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "eng7");
    assert_eq!((focused(&app), field(&app, "Type").as_str()), ("Name", "Agent"));
    app.on_key(key(KeyCode::Left));
    type_text(&mut app, "-");
    assert_eq!((focused(&app), field(&app, "Type").as_str()), ("Ticket", "Ticket"));
    let cursor = |app: &App| {
        let Mode::Form(f) = &app.mode else { panic!("no form open") };
        f.field("Ticket").unwrap().cursor()
    };
    assert_eq!((field(&app, "Ticket").as_str(), cursor(&app)), ("eng-7", 4));
    focus(&mut app, "CLI");
    type_text(&mut app, "1");
    assert_eq!(focused(&app), "Ticket");
    assert_eq!((field(&app, "Ticket").as_str(), cursor(&app)), ("eng-17", 5));
}

#[test]
fn a_key_or_link_for_the_name_switches_to_a_ticket_and_another_key_stays_a_name() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "phase-2");
    assert_eq!((focused(&app), field(&app, "Type").as_str()), ("Name", "Agent"));
    app.on_key(ctrl('u'));
    // The project's prefix makes it a ticket, as the key is typed.
    type_text(&mut app, "eng-7");
    assert_eq!((focused(&app), field(&app, "Type").as_str()), ("Ticket", "Ticket"));
    assert_eq!(field(&app, "Ticket"), "eng-7");
    // A paste on Type lands in the field too.
    app.on_key(key(KeyCode::Esc));
    prefix(&mut app, KeyCode::Char('n'));
    app.on_paste("https://linear.app/acme/issue/ENG-7/fix-it");
    assert_eq!((focused(&app), field(&app, "Type").as_str()), ("Ticket", "Ticket"));
    assert_eq!(field(&app, "Ticket"), "https://linear.app/acme/issue/ENG-7/fix-it");
    app.on_key(key(KeyCode::Esc));
    prefix(&mut app, KeyCode::Char('n'));
    app.on_paste("reviewer");
    assert_eq!((focused(&app), field(&app, "Name").as_str()), ("Name", "reviewer"));
}

/// A bug that panics the listing still ends it: the form says the issues couldn't be listed,
/// rather than listing them for good.
#[test]
fn a_listing_of_open_issues_that_panics_says_so() {
    let mut app = acme_app();
    app.github.insert("/src/acme".into(), crate::github::Gh::NotGit);
    panicking(&mut app);
    open_ticket_form_on_ticket(&mut app, 0);
    let loading = |app: &App| {
        let listing = app.open_issues.get(std::path::Path::new("/src/acme"));
        matches!(listing, Some((_, crate::app::add_form::OpenIssues::Loading(_))))
    };
    assert!(loading(&app));
    settle_until(&mut app, |app| !loading(app));
    assert_eq!(listed(&app), [format!("Couldn't list the open issues: {BUG}")]);
}
