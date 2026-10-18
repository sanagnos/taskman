use super::*;

// ---- closing a ticket ----------------------------------------------------------------------

#[test]
fn closing_asks_and_keeps_a_worktree_it_can_not_check() {
    let (mut app, _) = at_eng_52();
    prefix(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    // No git worktree there to check: nothing is offered for removal, and `r` does nothing.
    assert!(app.close_check.is_none());
    app.on_key(key(KeyCode::Char('r')));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.tickets.len(), 7);

    let before = app.sel;
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    assert_eq!(app.sel, before, "the next ticket takes its place");
    assert!(!saved().tickets.iter().any(|t| t.key == "ENG-52"));
    // Nothing holds its session any more.
    assert!(matches!(
        resume::peek(&resume::owner_file("/src/acme".as_ref(), "ENG-52")),
        resume::Owner::Free
    ));
}

/// The dialog says when the worktree couldn't be checked (git failed), and keeps it, as `taskman
/// close` says why in its reply.
#[test]
fn closing_says_when_the_worktree_could_not_be_checked() {
    // A worktree whose repo isn't a git repository: `git worktree list` fails there.
    let root = store::scratch::dir().join("nogit");
    let (repo, wt) = (root.join("acme"), root.join("acme-ENG-52"));
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    let (mut app, _) = at_eng_52();
    let tv = &mut app.tickets[app.sel];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo, wt.clone(), true);
    prefix(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    assert!(app.close_check.is_none());
    assert_eq!(
        app.close_unchecked.as_deref(),
        Some("git worktree failed: not a git repository (or any of the parent directories): .git")
    );
    let screen = draw(&mut app, 80, 24);
    assert!(
        screen.contains("couldn't be checked (git worktree failed: not a git repository"),
        "{screen}"
    );
    // Nothing is offered: `r` does nothing, Enter closes and keeps the folder.
    app.on_key(key(KeyCode::Char('r')));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.close_unchecked.is_none());
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    assert!(wt.exists());
}

#[test]
fn closing_deletes_ignored_files_only_when_asked() {
    // ENG-52 in a worktree of a real repo: pushed and merged, with nothing but an ignored .env.
    let root = store::scratch::dir().join("git");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    sh(&root, "git init -q --bare -b main origin.git && git clone -q origin.git acme 2>/dev/null");
    let (repo, wt) = (root.join("acme"), root.join("acme-ENG-52"));
    sh(
        &repo,
        "echo .env > .gitignore && git add .gitignore && git commit -q -m one && git push -q origin main \
         && git remote set-head origin main && git worktree add -q -b eng-52 ../acme-ENG-52 origin/main",
    );
    sh(&wt, "echo SECRET=1 > .env");
    let (mut app, _) = at_eng_52();
    let tv = &mut app.tickets[app.sel];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo.clone(), wt.clone(), true);

    // The dialog lists it; `r`, which removes only what loses nothing, isn't offered.
    prefix(&mut app, KeyCode::Char('x'));
    let check = app.close_check.as_ref().unwrap();
    assert_eq!((check.ignored.as_slice(), check.safe_but_ignored()), (&[".env".into()][..], true));
    app.on_key(key(KeyCode::Char('r')));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    app.on_key(key(KeyCode::Esc));

    // `d` closes it and removes the worktree with its .env, and the merged branch.
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    settle(&mut app);
    assert_eq!(
        toast(&app).map(String::from),
        Some(format!("Removed {} and branch eng-52", store::tilde(&wt)))
    );
    assert!(!wt.exists());
}

/// A clone `acme` of a bare origin in the test's scratch folder, with ENG-52's worktree beside it
/// on a branch at origin's main: nothing to lose, so closing may remove it. Returns both folders.
fn merged_worktree() -> (std::path::PathBuf, std::path::PathBuf) {
    let root = store::scratch::dir().join("git");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    sh(&root, "git init -q --bare -b main origin.git && git clone -q origin.git acme 2>/dev/null");
    let (repo, wt) = (root.join("acme"), root.join("acme-ENG-52"));
    sh(
        &repo,
        "git commit -q --allow-empty -m one && git push -q origin main && git remote set-head origin main \
         && git worktree add -q -b eng-52 ../acme-ENG-52 origin/main",
    );
    (repo, wt)
}

/// Removal holds the ticket's owner lock, which keeps a new session out of the folder while git
/// deletes it, or doesn't happen: when the lock can't be taken the ticket still closes, but the
/// worktree stays and the toast says why.
#[test]
fn closing_keeps_a_worktree_when_its_session_can_not_be_claimed() {
    let (repo, wt) = merged_worktree();
    let (mut app, _) = at_eng_52();
    let tv = &mut app.tickets[app.sel];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo.clone(), wt.clone(), true);
    break_lock(&repo, "ENG-52");
    prefix(&mut app, KeyCode::Char('x'));
    assert!(app.close_check.as_ref().is_some_and(|c| c.safe()));
    app.on_key(key(KeyCode::Char('r')));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    assert!(app.removing.is_empty(), "removal started without the lock");
    assert!(wt.exists());
    assert!(
        toast(&app).is_some_and(|t| t.starts_with("Kept the worktree: couldn't claim its session")),
        "{:?}",
        toast(&app)
    );
}

/// The same from `taskman close`: the reply names the folder and says why it stays.
#[test]
fn a_close_request_keeps_a_worktree_when_its_session_can_not_be_claimed() {
    let (repo, wt) = merged_worktree();
    let mut app = acme_app();
    app.projects[0].repo = repo.clone();
    let i = index(&app, "ENG-52");
    let tv = &mut app.tickets[i];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo.clone(), repo.clone(), false);
    request(
        "close-ENG-52",
        &serde_json::json!({
            "repo": repo, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-52" }], "close": {},
        }),
    );
    app.take_inbox();
    // The worktree becomes taskman's once the request is queued, so no PR lookup ran for it
    // (that would need gh); closing reads the ticket as it is then.
    let tv = &mut app.tickets[i];
    (tv.t.cwd, tv.t.created_worktree) = (wt.clone(), true);
    break_lock(&repo, "ENG-52");
    app.close_next();
    app.drain_messages();
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    assert!(app.removing.is_empty(), "removal started without the lock");
    assert!(wt.exists());
    let lines = reply("close-ENG-52");
    let expected = format!("Closed ENG-52. Kept {}: couldn't claim its session", store::tilde(&wt));
    assert!(lines[0].ok && lines[0].text.starts_with(&expected), "{}", lines[0].text);
}

#[test]
fn closing_an_agent_remembers_its_conversation() {
    let mut app = acme_app();
    let i = index(&app, "acme-lead");
    app.tickets[i].t.session_id = Some("sid-lead".into());
    app.select(i);
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Enter));
    let closed = &saved().projects[0].closed_agents;
    assert_eq!(closed.len(), 1);
    assert_eq!((closed[0].name.as_str(), closed[0].session_id.as_str()), ("lead", "sid-lead"));
    assert_eq!(closed[0].role.as_deref(), Some("Coordinate the tickets"));
}

#[test]
fn a_ticket_running_in_another_terminal_does_not_close() {
    let mut app = acme_app();
    // `taskman resume` holds the claim on ENG-52's session.
    let outside =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-52")).unwrap().unwrap();
    prefix(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(toast(&app).is_some_and(|t| t.contains("another terminal")), "{:?}", toast(&app));
    assert_eq!(app.tickets.len(), 7);
    drop(outside);
}

/// Claims the pane's session of agent `key`, as a session the pane started holds it.
fn claimed(app: &mut App, key: &str) {
    let hold = resume::take(&resume::owner_file("/src/acme".as_ref(), key)).unwrap().unwrap();
    let i = index(app, key);
    app.tickets[i].hold = Some(hold.taskman());
}

/// A close that can't be saved leaves the ticket open: until the state no longer lists it, `taskman
/// resume` finds it, so its session stays running and claimed.
#[test]
fn a_close_that_can_not_be_saved_leaves_the_ticket_open() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let i = index(&app, "acme-lead");
    app.tickets[i].t.session_id = Some("sid-lead".into());
    app.select(i);
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    claimed(&mut app, "acme-lead");
    let blocked = block_saves();
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Enter));
    let error = toast(&app).unwrap_or_default();
    assert!(error.starts_with("lead stays open: couldn't save "), "{error}");
    assert_eq!((app.tickets.len(), selected(&app)), (7, "acme-lead"));
    let tv = &app.tickets[app.sel];
    assert!(tv.session.is_some() && !tv.reaping() && tv.hold.is_some(), "the session was stopped");
    assert_eq!(owner("acme-lead"), resume::Owner::Taskman(std::process::id()));
    assert!(app.projects[0].closed_agents.is_empty());
    std::fs::remove_dir_all(&blocked).unwrap();
}

/// The same from `taskman close`: the reply says it stays open, and why.
#[test]
fn a_close_request_that_can_not_be_saved_leaves_the_ticket_open() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    claimed(&mut app, "acme-lead");
    request(
        "close-lead",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "agent", "name": "lead" }], "close": {},
        }),
    );
    app.take_inbox();
    let blocked = block_saves();
    app.close_next();
    app.drain_messages();
    let lines = reply("close-lead");
    assert!(!lines[0].ok, "{}", lines[0].text);
    assert!(lines[0].text.starts_with("✦ lead stays open: couldn't save "), "{}", lines[0].text);
    let tv = &app.tickets[index(&app, "acme-lead")];
    assert!(tv.session.is_some() && tv.hold.is_some(), "the session was stopped");
    assert_eq!(owner("acme-lead"), resume::Owner::Taskman(std::process::id()));
    std::fs::remove_dir_all(&blocked).unwrap();
}

// ---- saving --------------------------------------------------------------------------------

#[test]
fn a_failed_save_keeps_showing_until_one_works() {
    let mut app = offline(crate::app::test_support::acme());
    let file = block_saves();
    prefix(&mut app, KeyCode::Char('c'));
    let error = toast(&app).unwrap().to_string();
    assert!(error.starts_with("Couldn't save ") && error.contains("tickets.json"), "{error}");
    assert!(error.ends_with("changes are kept only until taskman quits"), "{error}");

    // Another toast hides it for a while; once that one goes, it's back.
    prefix(&mut app, KeyCode::Char('y'));
    assert_ne!(toast(&app), Some(error.as_str()));
    app.toast = None;
    app.show_save_error();
    assert_eq!(toast(&app), Some(error.as_str()));

    std::fs::remove_dir_all(&file).unwrap();
    prefix(&mut app, KeyCode::Char('c'));
    assert!(app.save_errors.iter().all(Option::is_none));
    app.toast = None;
    app.show_save_error();
    assert_eq!(toast(&app), None);
    assert!(saved().ui.show_comments, "the change made while saving failed is saved too");
}
