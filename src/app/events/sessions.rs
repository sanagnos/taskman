use super::*;

// ---- handing a session off, bringing it back, restarting it --------------------------------
//
// Nothing here may start a real CLI: a session that would start finds its claim held by the test
// (`held_outside`), as `taskman resume` in another terminal would hold it. tests/launch.rs runs the
// same with the binary and a stub CLI.

/// Answers the tmux window that's offered inside tmux, without opening one.
fn decline_tmux(app: &mut App) {
    if std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()) {
        assert!(matches!(app.mode, Mode::OfferTmux(_)));
        app.on_key(key(KeyCode::Esc));
    }
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn handing_off_stops_the_session_and_copies_what_continues_it() {
    let (mut app, to_session) = at_eng_52();
    let i = app.sel;
    prefix(&mut app, KeyCode::Char('t'));
    // The CLI is asked to quit and given time to; the pane waits for it.
    assert!(matches!(app.tickets[i].away, Some(Away::Leaving(_))));
    prefix(&mut app, KeyCode::Char('t'));
    assert!(matches!(app.tickets[i].away, Some(Away::Leaving(_))), "asked once");
    assert_eq!(sent(&to_session), "", "hung up, not typed at");

    // It didn't quit in time, so it's stopped.
    app.tickets[i].away = Some(Away::Leaving(ago(HAND_OFF_WAIT.as_secs_f32() + 1.0).unwrap()));
    app.poll_status();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none());
    assert_eq!((tv.away, tv.status.state), (Some(Away::HandedOff), AgentState::NotStarted));
    // The owner file keeps it out of the pane until the user brings it back, after a restart too.
    // The claim goes on the poll after the CLI is reaped, which the fake's reaper does at once,
    // on a thread.
    reaped(&mut app);
    app.poll_status();
    assert_eq!(owner("ENG-52"), resume::Owner::Back);
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["taskman resume ENG-52"]);
    assert!(
        toast(&app)
            .is_some_and(|t| t.contains("`taskman resume ENG-52`: run it in another terminal")),
        "{:?}",
        toast(&app)
    );
    decline_tmux(&mut app);
    app.ensure_selected_session();
    assert!(app.tickets[i].session.is_none(), "started again in the pane");
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("ENG-52 is handed off"), "{screen}");

    // ctrl+a t again offers the command again.
    prefix(&mut app, KeyCode::Char('t'));
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["taskman resume ENG-52"; 2]);
    decline_tmux(&mut app);
}

/// Handed off, the claim goes only once the pane's CLI is gone: `taskman resume` can't start a
/// second copy on the conversation the CLI is still ending, and meanwhile the owner file says
/// so (`Owner::Stopping`) rather than that the session runs elsewhere.
#[test]
fn a_hand_off_keeps_the_claim_until_the_old_cli_is_gone() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let i = app.sel;
    let (_sent, release) = attach_slow(&mut app, "ENG-52");
    prefix(&mut app, KeyCode::Char('t'));
    // It didn't quit in time, so it's stopped: hung up and reaped on a thread, which the CLI
    // holds up by going on for a while.
    app.tickets[i].away = Some(Away::Leaving(ago(HAND_OFF_WAIT.as_secs_f32() + 1.0).unwrap()));
    app.poll_status();
    let tv = &app.tickets[i];
    assert_eq!((tv.away, tv.status.state), (Some(Away::HandedOff), AgentState::NotStarted));
    assert!(tv.session.is_none() && tv.reaping() && tv.hold.is_some());
    assert_eq!(owner("ENG-52"), resume::Owner::Stopping);
    assert!(!resume::Owner::Stopping.elsewhere(), "closing the ticket is still allowed");
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["taskman resume ENG-52"]);
    decline_tmux(&mut app);
    // Polled again while the CLI is still there: still claimed, still handed off.
    app.poll_status();
    assert!(app.tickets[i].hold.is_some());
    assert_eq!(
        (app.tickets[i].away, owner("ENG-52")),
        (Some(Away::HandedOff), resume::Owner::Stopping)
    );
    // The CLI is gone: the next poll lets the claim go, and the ticket stays handed off.
    drop(release);
    reaped(&mut app);
    app.poll_status();
    assert!(app.tickets[i].hold.is_none(), "still claimed after the CLI was gone");
    assert_eq!(
        (app.tickets[i].away, owner("ENG-52")),
        (Some(Away::HandedOff), resume::Owner::Back)
    );
}

/// `prefix R` starts the new session only once the old CLI is gone: the lock is taskman's, not
/// the CLI's, so it wouldn't keep a second copy off the same conversation.
#[test]
fn a_restart_waits_until_the_old_cli_is_gone() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let i = app.sel;
    let (_sent, release) = attach_slow(&mut app, "ENG-52");
    let hold = resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-52")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(toast(&app), Some("Restarting Claude session…"));
    assert!(app.tickets[i].session.is_none() && app.tickets[i].reaping());
    // The tick that would start it again finds the CLI still stopping: nothing starts, nothing
    // is said, and the claim is kept for the start to come.
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.away.is_none(), "started over the old CLI");
    assert!(tv.hold.is_some() && tv.status.state == AgentState::NotStarted);
    assert_eq!(owner("ENG-52"), resume::Owner::Taskman(std::process::id()));
    // The CLI is gone: the next tick starts it. (The claim is handed to `taskman resume` here so
    // the test starts no real CLI: the start gets as far as taking the lock.)
    drop(release);
    reaped(&mut app);
    app.tickets[i].hold = None;
    let outside = held_outside("ENG-52");
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].away, Some(Away::Outside), "no start once the CLI was gone");
    drop(outside);
}

/// Closing with `r` removes the worktree only once the CLI that ran in it is gone.
#[test]
fn closing_removes_the_worktree_only_once_its_cli_is_gone() {
    let root = store::scratch::dir().join("git-stopping");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    sh(&root, "git init -q --bare -b main origin.git && git clone -q origin.git acme 2>/dev/null");
    let (repo, wt) = (root.join("acme"), root.join("acme-ENG-52"));
    sh(
        &repo,
        "git commit -q --allow-empty -m one && git push -q origin main && git remote set-head origin main \
         && git worktree add -q -b eng-52 ../acme-ENG-52 origin/main",
    );
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let (_sent, release) = attach_slow(&mut app, "ENG-52");
    let tv = &mut app.tickets[app.sel];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo.clone(), wt.clone(), true);
    prefix(&mut app, KeyCode::Char('x'));
    assert!(app.close_check.as_ref().is_some_and(|c| c.safe()));
    app.on_key(key(KeyCode::Char('r')));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    // The removal runs on a thread, waiting for the CLI: the worktree stays meanwhile.
    assert!(
        app.rx.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
        "removed while the CLI was still running in it"
    );
    assert!(wt.exists());
    drop(release);
    settle(&mut app);
    assert_eq!(
        toast(&app).map(String::from),
        Some(format!("Removed {} and branch eng-52", store::tilde(&wt)))
    );
    assert!(!wt.exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn only_a_started_session_can_be_handed_off() {
    let mut app = acme_app();
    // ENG-70's session has ended, and it never had a conversation to continue.
    app.select(index(&app, "ENG-70"));
    prefix(&mut app, KeyCode::Char('t'));
    assert_eq!(
        toast(&app),
        Some("ENG-70's session hasn't started: there's nothing to hand off yet")
    );
    assert_eq!(app.tickets[app.sel].away, None);
    // One whose worktree is still being made has nothing to hand off either.
    app.tickets[app.sel].creating = true;
    app.tickets[app.sel].t.session_id = Some("sid-70".into());
    prefix(&mut app, KeyCode::Char('t'));
    assert_eq!(app.tickets[app.sel].away, None);
    // With a conversation the CLI still has, it's handed off as it is.
    app.tickets[app.sel].creating = false;
    let transcript = store::state_dir().join(".claude/projects/-src-acme/sid-70.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(transcript, "{}\n").unwrap();
    prefix(&mut app, KeyCode::Char('t'));
    assert_eq!(app.tickets[app.sel].away, Some(Away::HandedOff));
    assert_eq!(owner("ENG-70"), resume::Owner::Back);
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["taskman resume ENG-70"]);
}

#[test]
fn a_session_running_elsewhere_is_never_started_twice() {
    let (mut app, _) = at_eng_52();
    let i = app.sel;
    // `taskman resume` runs it in another terminal.
    let outside = held_outside("ENG-52");
    app.poll_status();
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    assert!(app.tickets[i].session.is_none(), "the pane showed the old screen as live");
    // Stopping the pane's copy left a thread reaping its CLI, and nothing starts while it runs:
    // wait for it, or a start below that should find the claim held is only refused for that.
    if let Some(reaper) = app.tickets[i].take_reaper() {
        reaper.wait();
    }
    app.on_key(key(KeyCode::Enter));
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(
        toast(&app),
        Some("It's running in another terminal: quit it there, then press Enter here")
    );
    prefix(&mut app, KeyCode::Char('t'));
    assert_eq!(toast(&app), Some("ENG-52 is already running in another terminal"));
    assert_eq!(app.tickets[i].away, Some(Away::Outside));

    // It ended there: Enter brings it back.
    drop(outside);
    app.poll_status();
    assert_eq!(app.tickets[i].away, Some(Away::Back));
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("ENG-52 is back"), "{screen}");
    // But it was resumed there again first: the pane starts no second copy.
    let again = held_outside("ENG-52");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    assert!(app.tickets[i].session.is_none());
    // ctrl+a R does the same as Enter on a session that's back.
    drop(again);
    app.poll_status();
    assert_eq!(app.tickets[i].away, Some(Away::Back));
    let again = held_outside("ENG-52");
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    drop(again);
}

#[test]
fn a_session_whose_worktree_is_being_removed_waits() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "ENG-70"));
    app.tickets[app.sel].status.state = AgentState::NotStarted;
    let closing = resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-70"))
        .unwrap()
        .unwrap()
        .closing();
    app.ensure_selected_session();
    assert_eq!(toast(&app).map(String::from), Some(resume::closing("ENG-70")));
    assert_eq!(app.tickets[app.sel].status.state, AgentState::Exited, "Enter tries again");
    assert!(app.tickets[app.sel].session.is_none());
    drop(closing);
}

/// The owner lock fails closed: a lock that can't be taken stops the session starting, since
/// without it a second copy could run on the same conversation.
#[test]
fn a_session_does_not_start_without_its_lock() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let i = index(&app, "ENG-70");
    app.select(i);
    app.tickets[i].status.state = AgentState::NotStarted;
    // A plain file sits where the project's status folder goes, so the lock file can't be made.
    let owner = resume::owner_file("/src/acme".as_ref(), "ENG-70");
    let folder = owner.parent().unwrap();
    std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
    std::fs::write(folder, "").unwrap();
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.hold.is_none(), "started without the lock");
    assert_eq!(tv.status.state, AgentState::Exited);
    assert!(
        toast(&app).is_some_and(|t| t.starts_with("Couldn't take ENG-70's session lock: ")
            && t.ends_with("; fix it, then press Enter to retry")),
        "{:?}",
        toast(&app)
    );
    let screen = draw(&mut app, 80, 24);
    assert!(
        screen.contains("Couldn't take the lock that keeps ENG-70's session to one copy")
            && screen.contains("Fix it, then press Enter to retry."),
        "{screen}"
    );
    // The next tick leaves it alone; Enter tries again, and the lock is taken this time: here
    // found held by `taskman resume`, so no CLI starts in the test.
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].status.state, AgentState::Exited);
    std::fs::remove_file(folder).unwrap();
    let outside = held_outside("ENG-70");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.tickets[i].status.state, AgentState::NotStarted);
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    assert_eq!(app.tickets[i].lock_error, None);
    drop(outside);
}

/// A ticket whose worktree was removed outside taskman doesn't start in another folder: the pane
/// and a toast say what's gone, and Enter makes it again where it was, on its branch.
#[test]
fn a_session_whose_worktree_is_gone_does_not_start_and_enter_makes_it_again() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let (i, repo, folder) = gone_worktree(&mut app, false);
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.hold.is_none(), "started somewhere else");
    assert_eq!((tv.gone, tv.status.state), (Some(Gone::Worktree), AgentState::Exited));
    assert_eq!(resume::peek(&resume::owner_file(&repo, "ENG-5")), resume::Owner::Free);
    let said = format!(
        "ENG-5's folder {} is gone: Enter makes its worktree again on ENG-5, {} x closes the ticket",
        store::tilde(&folder),
        crate::keys::prefix()
    );
    assert_eq!(toast(&app), Some(said.as_str()));
    let screen = draw(&mut app, 80, 24);
    assert!(
        screen.contains("ENG-5's worktree is gone")
            && screen.contains("Press Enter to make it again there, on ENG-5."),
        "{screen}"
    );
    // The next tick leaves it alone.
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].status.state, AgentState::Exited);

    app.on_key(key(KeyCode::Enter));
    assert!(app.tickets[i].creating && app.tickets[i].gone.is_none());
    settle_until(&mut app, |app| !app.tickets[i].creating);
    let t = &saved().tickets[i];
    assert_eq!((t.cwd.as_path(), t.branch.as_deref()), (folder.as_path(), Some("ENG-5")));
    assert!(folder.is_dir() && t.pending_worktree.is_none() && t.created_worktree);
    // The next tick starts it.
    assert_eq!(app.tickets[i].status.state, AgentState::NotStarted);
}

/// Adds ticket ENG-5 to project web (`web_project`), waits for its worktree, and removes it by
/// hand, locked first when `locked`. Returns the ticket, the main checkout and the worktree.
fn gone_worktree(app: &mut App, locked: bool) -> (usize, std::path::PathBuf, std::path::PathBuf) {
    let (p, repo) = web_project(app);
    let added = app.add_ticket(p, "ENG-5".into(), store::Cli::Claude, true, None, true);
    assert!(matches!(added, Ok(crate::app::ticket::Added::New(_))));
    settle(app);
    let folder = app.tickets[app.sel].t.cwd.clone();
    assert_ne!(folder, repo);
    if locked {
        sh(&repo, &format!("git worktree lock '{}'", folder.display()));
    }
    // git still lists it, by its real path rather than the symlinked one.
    std::fs::remove_dir_all(&folder).unwrap();
    (app.sel, repo, folder)
}

/// A gone worktree git keeps locked isn't made again: the pane says how to unlock it, and
/// `ctrl+a M` runs the session in the main checkout instead.
#[test]
fn a_gone_worktree_that_cant_be_made_again_can_run_in_the_main_checkout() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let (i, repo, folder) = gone_worktree(&mut app, true);
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].gone, Some(Gone::Worktree));
    app.on_key(key(KeyCode::Enter));
    settle_until(&mut app, |app| !app.tickets[i].creating);
    let error = app.tickets[i].t.pending_worktree.as_ref().and_then(|w| w.error.clone());
    assert!(
        error.as_deref().is_some_and(|e| e.contains("locked worktree; unlock it")),
        "{error:?}"
    );
    assert!(!folder.exists());
    prefix(&mut app, KeyCode::Char('M'));
    let t = &saved().tickets[i];
    assert_eq!(t.cwd, repo);
    assert!(t.pending_worktree.is_none() && !t.created_worktree);
    assert_eq!(Gone::of(t), None);
}

/// An agent whose project folder is gone (moved, say) doesn't start either; Enter tries again.
#[test]
fn a_session_whose_project_folder_is_gone_does_not_start() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let i = app.tickets.iter().position(|tv| tv.t.agent.as_deref() == Some("lead")).unwrap();
    app.select(i);
    app.tickets[i].status.state = AgentState::NotStarted;
    // /src/acme isn't there in a test.
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.hold.is_none(), "started somewhere else");
    assert_eq!((tv.gone, tv.status.state), (Some(Gone::Project), AgentState::Exited));
    assert!(
        toast(&app).is_some_and(|t| t.starts_with(
            "acme's folder /src/acme is gone, so lead's session didn't start: put the folder back"
        )),
        "{:?}",
        toast(&app)
    );
    app.on_key(key(KeyCode::Enter));
    assert_eq!((app.tickets[i].gone, app.tickets[i].status.state), (None, AgentState::NotStarted));
    assert!(!app.tickets[i].creating);
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].gone, Some(Gone::Project));
}

/// A status file record as `taskman hook` writes it, from start `launch` of the session.
fn record(state: &str, sid: &str, launch: Option<&str>) -> String {
    let state = crate::status_file::HookState::from_word(state);
    let mut r = crate::hook::status_record(state, Some(sid), None, "", 1);
    r.launch = launch.map(str::to_string);
    r.to_string()
}

fn write_status(key: &str, text: &str) -> std::path::PathBuf {
    let path = store::status_file("/src/acme".as_ref(), key);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

/// The hooks say which start of the session wrote each record, and the pane takes only its own
/// start's: a SessionEnd hook runs on the hangup that stops a CLI, so its record can land after
/// the restart's new session is running, or after the ticket closed and was added again.
#[test]
fn a_status_record_counts_only_from_the_sessions_own_start() {
    let (mut app, _) = at_eng_52();
    let i = app.sel;
    app.tickets[i].launch = Some("1-7".into());
    app.tickets[i].status.state = AgentState::Idle;
    let reread = |app: &mut App| {
        app.tickets[i].status.mtime = None;
        app.poll_status();
    };
    // The start before this one ends late: not this session's, and not its conversation.
    write_status("ENG-52", &record("ended", "sid-old", Some("1-6")));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::Idle);
    assert_eq!(app.tickets[i].t.session_id, None);
    // Its own start's records count, as do an older hook's, which name no start.
    write_status("ENG-52", &record("done", "sid-52", Some("1-7")));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::Done);
    assert_eq!(app.tickets[i].t.session_id.as_deref(), Some("sid-52"));
    write_status("ENG-52", &record("working", "sid-52", None));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::Working);
    // Restarted (`prefix R`, which asks first while it's working), nothing runs in the pane
    // yet: the old start's end doesn't make the session that's about to start look ended.
    prefix(&mut app, KeyCode::Char('R'));
    app.on_key(key(KeyCode::Enter));
    write_status("ENG-52", &record("ended", "sid-52", Some("1-7")));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::NotStarted);
    // Closed and added again, the ticket has no session: the old one's end isn't its own, and
    // the old conversation isn't its to resume.
    app.tickets[i].hold = None;
    app.tickets[i].t.session_id = None;
    write_status("ENG-52", &record("ended", "sid-old", Some("1-6")));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::NotStarted);
    assert_eq!(app.tickets[i].t.session_id, None);
    // Running in another terminal, its hooks' records are the session's, whatever start they
    // name.
    let outside = held_outside("ENG-52");
    write_status("ENG-52", &record("needs", "sid-out", Some("4242-1")));
    reread(&mut app);
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    assert_eq!(app.tickets[i].status.state, AgentState::Needs);
    assert_eq!(app.tickets[i].t.session_id.as_deref(), Some("sid-out"));
    drop(outside);
}

/// Adding a ticket or agent clears what a hook of its last session wrote after it closed, so
/// at taskman's next start it can't pass for the new one's latest conversation.
#[test]
fn adding_a_ticket_or_agent_clears_what_its_last_session_left() {
    let mut app = acme_app();
    let path = write_status("ENG-80", &record("ended", "sid-80", Some("1-2")));
    std::fs::write(path.with_extension("gh"), "1").unwrap();
    app.add_ticket(0, "ENG-80".into(), store::Cli::Claude, false, None, false).unwrap();
    assert!(!path.exists() && !path.with_extension("gh").exists());
    let path = write_status("acme-tester", &record("ended", "sid-tester", Some("1-3")));
    app.add_agent(0, "tester", store::Cli::Claude, None, false).unwrap();
    assert!(!path.exists());
}

#[test]
fn restarting_stops_the_session_and_keeps_its_claim() {
    let (mut app, _) = at_eng_52();
    let i = app.sel;
    let hold = resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-52")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(toast(&app), Some("Restarting Claude session…"));
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.status.state == AgentState::NotStarted);
    // The next start takes over the pane's claim rather than finding it held, by taskman itself.
    assert!(tv.hold.is_some());
    assert_eq!(owner("ENG-52"), resume::Owner::Taskman(std::process::id()));

    // Enter on an ended session restarts it too.
    let j = index(&app, "ENG-61");
    app.select(j);
    app.tickets[j].status.state = AgentState::Exited;
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.tickets[j].status.state, AgentState::NotStarted);
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(toast(&app), Some("Restarting Codex session…"));
}

/// A restart cuts short a turn under way or a question to the user, so `prefix R` asks first
/// then; Esc keeps the session as it was.
#[test]
fn restarting_a_session_that_works_or_needs_you_asks_first() {
    let (mut app, to_session) = at_eng_52();
    let i = app.sel;
    for state in [AgentState::Working, AgentState::Needs] {
        app.tickets[i].status.state = state;
        prefix(&mut app, KeyCode::Char('R'));
        assert!(
            matches!(&app.mode, Mode::ConfirmRestart((_, key)) if key == "ENG-52"),
            "{state:?}"
        );
        // A stray key doesn't answer it, and doesn't reach the session.
        app.on_key(key(KeyCode::Char('y')));
        assert!(matches!(app.mode, Mode::ConfirmRestart(_)));
        app.on_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
        let tv = &app.tickets[i];
        assert!(tv.session.is_some() && tv.status.state == state, "{state:?}");
    }
    prefix(&mut app, KeyCode::Char('R'));
    // The turn ends while the dialog is open: it says so, and Enter still restarts.
    app.tickets[i].status.state = AgentState::Done;
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("Its turn has ended, so nothing is cut short."), "{screen}");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("Restarting Claude session…"));
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.status.state == AgentState::NotStarted);
    assert_eq!(sent(&to_session), "", "the keys answered the dialog");
}

/// The dialog holds the ticket, not its place in the list: Enter restarts that one, and nothing
/// once it's gone.
#[test]
fn the_restart_dialog_restarts_the_ticket_it_asked_about() {
    let (mut app, _) = at_eng_52();
    let i = app.sel;
    app.tickets[i].status.state = AgentState::Working;
    prefix(&mut app, KeyCode::Char('R'));
    // Something else is selected by the time it's answered.
    app.select(index(&app, "ENG-53"));
    app.on_key(key(KeyCode::Enter));
    assert!(app.tickets[i].session.is_none());
    assert_eq!(app.tickets[i].status.state, AgentState::NotStarted);

    let (mut app, _) = at_eng_52();
    app.tickets[app.sel].status.state = AgentState::Working;
    prefix(&mut app, KeyCode::Char('R'));
    app.mode = Mode::ConfirmRestart(("/src/acme".into(), "ENG-99".into()));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.tickets[app.sel].session.is_some(), "restarted another ticket");
}

/// Idle, its turn done or ended, a session has nothing a restart would cut short: `prefix R`
/// restarts it at once.
#[test]
fn restarting_a_session_with_nothing_under_way_doesnt_ask() {
    for state in [AgentState::Idle, AgentState::Done, AgentState::Exited] {
        let (mut app, _) = at_eng_52();
        app.tickets[app.sel].status.state = state;
        prefix(&mut app, KeyCode::Char('R'));
        assert!(matches!(app.mode, Mode::Normal), "{state:?}");
        assert_eq!(toast(&app), Some("Restarting Claude session…"));
        assert!(app.tickets[app.sel].session.is_none(), "{state:?}");
    }
    // Its CLI gone, a session still marked working has nothing left to lose.
    let (mut app, _) = at_eng_52();
    let tv = &mut app.tickets[app.sel];
    tv.status.state = AgentState::Working;
    tv.session.as_mut().unwrap().exit(1);
    prefix(&mut app, KeyCode::Char('R'));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("Restarting Claude session…"));
}

#[test]
fn a_session_keeps_the_name_it_started_with() {
    let (mut app, _) = at_eng_52();
    let i = app.sel;
    app.tickets[i].name = Some("acme/ENG-52".into());
    assert_eq!(app.session_name(i), "acme/ENG-52", "other sessions message it by that name");
    app.tickets[i].session = None;
    assert_eq!(app.session_name(i), "ENG-52", "the name it would start with now");
}

#[test]
fn a_running_agent_starts_its_projects_claude_sessions() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    attach(&mut app, "acme-lead", SCREEN);
    app.select(index(&app, "acme-lead"));
    // ENG-61 hasn't started, but runs in Codex, which can't be messaged.
    let (eng53, eng61) = (index(&app, "ENG-53"), index(&app, "ENG-61"));
    app.tickets[eng61].status.state = AgentState::NotStarted;
    let _held61 = held_outside("ENG-61");
    app.ensure_selected_session();
    assert_eq!(app.tickets[eng61].away, None, "tried to start a Codex session for the agent");
    // ENG-53 hasn't started either: it's tried, and found running elsewhere.
    app.tickets[eng53].status.state = AgentState::NotStarted;
    let _held53 = held_outside("ENG-53");
    app.ensure_selected_session();
    assert_eq!(app.tickets[eng53].away, Some(Away::Outside));
}

// ---- starting sessions ---------------------------------------------------------------------

/// The project agent runs, ENG-52 hasn't started and is away as `how`, and ENG-53 hasn't started:
/// the agent's tickets start one per tick, so ENG-53 should get its turn.
fn agent_starts_past(how: Away) {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    attach(&mut app, "acme-lead", SCREEN);
    app.select(index(&app, "acme-lead"));
    let eng52 = index(&app, "ENG-52");
    app.tickets[eng52].status.state = AgentState::NotStarted;
    app.tickets[eng52].away = Some(how);
    // ENG-53's claim is held as `taskman resume` holds it, so trying to start it marks it Outside
    // and no CLI runs.
    let eng53 = index(&app, "ENG-53");
    app.tickets[eng53].status.state = AgentState::NotStarted;
    let mut held =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-53")).unwrap().unwrap();
    held.write("outside 4242");
    for _ in 0..5 {
        app.ensure_selected_session();
    }
    assert_eq!(app.tickets[eng53].away, Some(Away::Outside), "ENG-53 was never tried");
    assert!(app.tickets[eng52].session.is_none(), "ENG-52 started while away");
    drop(held);
}

#[test]
fn a_handed_off_ticket_does_not_stop_an_agent_starting_the_others() {
    // Handed off, it waits for the user to bring it back.
    agent_starts_past(Away::HandedOff);
}

#[test]
fn a_ticket_running_outside_does_not_stop_an_agent_starting_the_others() {
    // Running in another terminal before its first hook said what it's doing.
    agent_starts_past(Away::Outside);
}

// ---- a conversation Claude runs in the background -------------------------------------------

const BACKGROUND: &str = "That session is running in the background (a1b2c3d4). Run `claude attach a1b2c3d4` to open it, or `claude stop a1b2c3d4` first to resume it here.";

fn ended_with(app: &mut App, screen: &str) -> usize {
    attach_ended(app, "ENG-52", screen, 1);
    app.sel
}

#[test]
fn enter_stops_the_background_copy_and_resumes() {
    let (mut app, _) = at_eng_52();
    let stopped = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = stopped.clone();
    app.stop_background = std::sync::Arc::new(move |id| {
        log.lock().unwrap().push(id.to_string());
        Ok(())
    });
    let i = ended_with(&mut app, BACKGROUND);
    let tv = &app.tickets[i];
    assert_eq!(tv.status.state, AgentState::Exited);
    assert_eq!(tv.background.as_ref().map(|b| b.id.as_str()), Some("a1b2c3d4"));
    assert!(draw(&mut app, 100, 24).contains("running in the background (a1b2c3d4)"));

    app.on_key(key(KeyCode::Enter));
    assert!(app.tickets[i].background.as_ref().unwrap().stopping);
    // Asked once: another Enter while it runs does nothing more.
    app.on_key(key(KeyCode::Enter));
    settle(&mut app);
    assert_eq!(*stopped.lock().unwrap(), ["a1b2c3d4"]);
    let tv = &app.tickets[i];
    assert!(tv.background.is_none() && tv.session.is_none(), "stopped, ready to resume");
    assert_eq!(tv.status.state, AgentState::NotStarted);
}

#[test]
fn a_failed_stop_says_so_and_gives_the_command() {
    let (mut app, _) = at_eng_52();
    app.stop_background = std::sync::Arc::new(|_| Err("no such job".into()));
    let i = ended_with(&mut app, BACKGROUND);
    app.on_key(key(KeyCode::Enter));
    settle(&mut app);
    let b = app.tickets[i].background.as_ref().expect("still offered");
    assert!(!b.stopping);
    assert_eq!(
        b.error.as_deref(),
        Some("no such job. Run `claude stop a1b2c3d4` yourself, then press Enter.")
    );
    assert_eq!(app.tickets[i].status.state, AgentState::Exited, "not resumed");
    assert!(draw(&mut app, 100, 24).contains("claude stop a1b2c3d4"));
}

#[test]
fn other_endings_offer_no_stop() {
    let (mut app, _) = at_eng_52();
    let i = ended_with(&mut app, "Goodbye");
    assert!(app.tickets[i].background.is_none());
    // Only an id-shaped token is taken from the screen.
    let (mut app, _) = at_eng_52();
    let i = ended_with(&mut app, "running in the background (a; rm -rf ~)");
    assert!(app.tickets[i].background.is_none());
}

// ---- a session that ended -------------------------------------------------------------------

/// An exited session's pane keeps its screen, with how it ended under it; its words can still be
/// selected and copied, and Enter starts it again.
#[test]
fn an_exited_session_keeps_its_screen_and_says_how_it_ended() {
    use crate::session::{End, Exit};
    let (mut app, _) = at_eng_52();
    attach_ended(&mut app, "ENG-52", "Error: the login expired", 2);
    let i = app.sel;
    assert_eq!(app.tickets[i].end, Some(End::Exited(Some(Exit::Code(2)))));
    assert_eq!(app.tickets[i].status.state, AgentState::Exited);
    let drawn = draw(&mut app, 80, 24);
    assert!(drawn.contains("Error: the login expired"), "{drawn}");
    assert!(drawn.contains("Claude exited with code 2 · Enter to resume"), "{drawn}");

    select_top_row(&mut app, 5);
    draw(&mut app, 80, 24);
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["Error"]);
    app.on_key(key(KeyCode::Esc));

    app.on_key(key(KeyCode::Enter));
    let tv = &app.tickets[i];
    assert_eq!(tv.status.state, AgentState::NotStarted, "Enter doesn't resume it");
    assert!(tv.session.is_none() && tv.end.is_none());
}

/// A session whose reader panicked says so, not that its CLI exited, and keeps drawing the screen
/// as the reader left it; an agent's says it's an agent that closes.
#[test]
fn a_session_whose_reader_crashed_says_so() {
    let mut app = acme_app();
    let i = index(&app, "acme-lead");
    app.select(i);
    draw(&mut app, 80, 24);
    attach_crashed(&mut app, "acme-lead", "half a screen");
    assert_eq!(app.tickets[i].end, Some(crate::session::End::ReaderCrashed));
    let drawn = draw(&mut app, 120, 24);
    assert!(drawn.contains("half a screen"), "{drawn}");
    assert!(drawn.contains("taskman stopped reading this session after an error"), "{drawn}");
    assert!(drawn.contains("ctrl+a x to close the agent"), "{drawn}");
    assert!(!drawn.contains("Claude exited"), "{drawn}");
}

/// A bug that panics `claude stop`'s thread still ends the wait: the pane says so, and Enter tries
/// again.
#[test]
fn a_stop_that_panics_says_so_and_enter_tries_again() {
    let (mut app, _) = at_eng_52();
    let i = ended_with(&mut app, BACKGROUND);
    panicking(&mut app);
    let stopping = |app: &App| app.tickets[i].background.as_ref().unwrap().stopping;
    app.on_key(key(KeyCode::Enter));
    assert!(stopping(&app));
    settle_until(&mut app, |app| !stopping(app));
    let b = app.tickets[i].background.as_ref().expect("still offered");
    let expected = format!("{BUG}. Run `claude stop a1b2c3d4` yourself, then press Enter.");
    assert_eq!(b.error.as_deref(), Some(expected.as_str()));
    app.on_key(key(KeyCode::Enter));
    assert!(stopping(&app));
    settle_until(&mut app, |app| !stopping(app));
}

// ---- what calls for a frame --------------------------------------------------------------------

#[test]
fn only_the_selected_sessions_output_calls_for_a_frame() {
    let (mut app, _) = at_eng_52();
    attach(&mut app, "ENG-61", "in the background");
    app.take_redraw();
    assert!(!app.selected_fresh());

    let session = |app: &App, key: &str| {
        let tv = app.tickets.iter().find(|tv| tv.t.key == key).unwrap();
        tv.session.as_ref().unwrap().output(b"more\r\n");
    };
    session(&app, "ENG-61");
    assert!(!app.selected_fresh(), "a background session's output draws nothing");
    session(&app, "ENG-52");
    assert!(app.selected_fresh());
    app.take_redraw();
    assert!(!app.selected_fresh(), "drawn");

    // Selected, the other's output is drawn at once.
    app.sel = app.tickets.iter().position(|tv| tv.t.key == "ENG-61").unwrap();
    assert!(app.selected_fresh());
}

// ---- what a session sends the clipboard ------------------------------------------------------

/// Feeds `output` to `key`'s session as if its CLI printed it.
fn print(app: &App, key: &str, output: &[u8]) {
    let tv = app.tickets.iter().find(|tv| tv.t.key == key).unwrap();
    tv.session.as_ref().unwrap().output(output);
}

#[test]
fn a_sessions_clipboard_write_is_forwarded_and_a_read_answered_with_nothing_and_a_warning() {
    let (mut app, to_session) = at_eng_52();
    print(&app, "ENG-52", b"\x1b]52;c;b25l\x07\x1b]52;p;dHdv\x07");
    app.take_clipboards();
    assert_eq!(decoded(&app.clipboard_out), ["two"], "the last of a burst");
    assert_eq!(app.clipboard_out[0].target, "p", "for the selection it named");
    assert_eq!(toast(&app), None);

    app.clipboard_out.clear();
    print(&app, "ENG-52", b"\x1b]52;c;?\x07");
    app.take_clipboards();
    assert!(app.clipboard_out.is_empty());
    assert_eq!(sent(&to_session), "\x1b]52;c;\x07", "an empty clipboard");
    assert_eq!(toast(&app), Some("ENG-52 asked to read your clipboard; taskman refused"));
    assert_eq!(app.toast.as_ref().unwrap().level, crate::app::Level::Warn);

    // Asking again is answered, but not toasted again for a while; another refusal is.
    let logged = app.toast_log.len();
    app.toast = None;
    print(&app, "ENG-52", b"\x1b]52;c;?\x07\x1b]52;c;?\x07");
    app.take_clipboards();
    assert_eq!(sent(&to_session), "\x1b]52;c;\x07".repeat(3));
    assert_eq!((toast(&app), app.toast_log.len()), (None, logged));
    let over = "A".repeat(crate::session::CLIPBOARD_MAX + 4);
    print(&app, "ENG-52", format!("\x1b]52;c;{over}\x07").as_bytes());
    app.take_clipboards();
    assert_eq!(toast(&app), Some("ENG-52 copied 750 KB, more than tmux takes; not copied"));
    assert!(app.clipboard_out.is_empty());
}

#[test]
fn the_newest_clipboard_write_of_any_session_is_the_one_forwarded_with_one_warning() {
    let (mut app, _sent) = at_eng_52();
    attach(&mut app, "ENG-70", SCREEN);
    print(&app, "ENG-52", b"\x1b]52;c;b25l\x07\x1b]52;c;?\x07");
    print(&app, "ENG-70", b"\x1b]52;c;dHdv\x07\x1b]52;z;eA==\x07");
    app.take_clipboards();
    assert_eq!(decoded(&app.clipboard_out), ["two"]);
    // The read matters most of what was refused; the rest is left out of this pass.
    let logged = app.toast_log.len();
    assert_eq!(toast(&app), Some("ENG-52 asked to read your clipboard; taskman refused"));
    print(&app, "ENG-52", b"\x1b]52;c;b25l\x07");
    app.take_clipboards();
    assert_eq!(decoded(&app.clipboard_out), ["two", "one"]);
    assert_eq!(app.toast_log.len(), logged);
}

#[test]
fn a_refused_clipboard_request_is_warned_of_once_a_minute_for_each_reason() {
    use crate::app::sessions::{CLIPBOARD_WARN_EVERY, warn_due};
    use crate::session::Refused;
    let start = std::time::Instant::now();
    let warned = [(Refused::Read, start), (Refused::TooLarge(10), start)];
    assert!(!warn_due(&warned, Refused::Read, start + CLIPBOARD_WARN_EVERY / 2));
    assert!(!warn_due(&warned, Refused::TooLarge(20), start), "whatever the size");
    assert!(warn_due(&warned, Refused::Malformed, start));
    assert!(warn_due(&warned, Refused::Read, start + CLIPBOARD_WARN_EVERY));
    assert!(warn_due(&[], Refused::Read, start));
}
