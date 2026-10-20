use super::*;

// ---- messages from threads -----------------------------------------------------------------

/// Ticket `key` as a `Msg` names it.
fn id(app: &App, key: &str) -> crate::app::Id {
    (app.tickets[index(app, key)].t.repo.clone(), key.to_string())
}

/// Starts a tracker fetch for ticket `key` as `schedule_fetches` does, without its thread, and
/// returns the fetch's id, which its reply carries.
fn fetching(app: &mut App, key: &str) -> u64 {
    app.fetches += 1;
    let (n, i) = (app.fetches, index(app, key));
    app.tickets[i].jira.start(n);
    n
}

/// `fetching` for a PR lookup.
fn looking_up(app: &mut App, key: &str) -> u64 {
    app.fetches += 1;
    let (n, i) = (app.fetches, index(app, key));
    app.tickets[i].pr.start(n);
    n
}

#[test]
fn fetched_comments_count_as_new_until_seen() {
    let mut app = acme_app();
    let new_comments = |app: &App| {
        let tv = &app.tickets[index(app, "ENG-61")];
        tv.jira.data.as_ref().unwrap().comments.iter().filter(|c| tv.is_new_jira(&c.id)).count()
    };
    let mut issue = crate::app::test_support::issue("ENG-61");
    issue.comments.truncate(1);
    // The first fetch counts what's there as seen.
    let i = index(&app, "ENG-61");
    app.tickets[i].t.seen_jira = None;
    let n = fetching(&mut app, "ENG-61");
    app.tx.send(Msg::Jira(n, id(&app, "ENG-61"), Ok(issue.clone()))).unwrap();
    app.drain_messages();
    assert_eq!(new_comments(&app), 0);

    let mut newer = issue;
    newer
        .comments
        .insert(0, crate::tracker::Comment { id: "c9".into(), ..newer.comments[0].clone() });
    let n = fetching(&mut app, "ENG-61");
    app.tx.send(Msg::Jira(n, id(&app, "ENG-61"), Ok(newer))).unwrap();
    app.drain_messages();
    assert_eq!(new_comments(&app), 1);

    // A failed refresh keeps what was fetched.
    let n = fetching(&mut app, "ENG-61");
    app.tx.send(Msg::Jira(n, id(&app, "ENG-61"), Err("timed out".into()))).unwrap();
    app.drain_messages();
    let tv = &app.tickets[index(&app, "ENG-61")];
    assert_eq!(tv.jira.error.as_deref(), Some("timed out"));
    assert_eq!(tv.jira.data.as_ref().unwrap().comments.len(), 2);

    // Selecting the ticket, then leaving it, marks them seen.
    app.select(index(&app, "ENG-61"));
    app.select(index(&app, "ENG-52"));
    assert_eq!(app.tickets[index(&app, "ENG-61")].new_count(), 0);
    assert!(
        saved().tickets[index(&app, "ENG-61")]
            .seen_jira
            .as_ref()
            .unwrap()
            .contains(&"c9".to_string())
    );
}

#[test]
fn a_reply_from_before_the_tracker_changed_is_dropped() {
    let mut app = acme_app();
    let i = index(&app, "ENG-52");
    let old = fetching(&mut app, "ENG-52");
    // The project moves to another workspace while the fetch runs: the panel empties and asks
    // the new one.
    let mut project = app.projects[0].clone();
    project.tracker.as_mut().unwrap().site = "acme-labs".into();
    app.update_project(0, project);
    let (tv, hour) = (&app.tickets[i], std::time::Duration::from_secs(3600));
    assert!(tv.jira.data.is_none() && !tv.jira.loading && tv.jira.due(hour));
    // The old workspace's reply lands after that: it isn't this tracker's issue.
    let issue = crate::app::test_support::issue("ENG-52");
    app.tx.send(Msg::Jira(old, id(&app, "ENG-52"), Ok(issue.clone()))).unwrap();
    app.drain_messages();
    let tv = &app.tickets[i];
    assert!(tv.jira.data.is_none() && tv.jira.fetched_at.is_none(), "a stale reply applied");
    // The new workspace's reply is the one shown.
    let new = fetching(&mut app, "ENG-52");
    app.tx.send(Msg::Jira(new, id(&app, "ENG-52"), Ok(issue))).unwrap();
    app.drain_messages();
    let tv = &app.tickets[i];
    assert!(tv.jira.data.is_some() && tv.jira.fetched_at.is_some() && !tv.jira.loading);
}

#[test]
fn a_pr_found_for_a_closed_ticket_is_not_the_reopened_tickets() {
    let mut app = acme_app();
    let old = looking_up(&mut app, "ENG-70");
    // Closed while gh runs, then added again without a worktree: another ticket with the key.
    app.close_at(index(&app, "ENG-70")).unwrap().hold.unwrap();
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-70"));
    request(
        "readd-ENG-70",
        &serde_json::json!({
            "repo": "/src/acme", "worktree": false, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-70" }],
        }),
    );
    app.take_inbox();
    let i = index(&app, "ENG-70");
    assert!(app.tickets[i].t.branch.is_none() && app.tickets[i].pr.data.is_none());
    // The old ticket's lookup lands on the new one: neither its PR nor its branch is taken.
    let found = || {
        Ok(crate::github::Lookup {
            pr: Some(crate::app::test_support::pr(146, "eng-70-export")),
            closed: None,
            branch_gone: false,
        })
    };
    app.tx.send(Msg::Pr(old, id(&app, "ENG-70"), found(), None)).unwrap();
    app.drain_messages();
    let t = &saved().tickets[i];
    assert!(app.tickets[i].pr.data.is_none() && t.branch.is_none(), "a stale lookup applied");
    // The new ticket's own lookup is.
    let new = looking_up(&mut app, "ENG-70");
    app.tx.send(Msg::Pr(new, id(&app, "ENG-70"), found(), None)).unwrap();
    app.drain_messages();
    let pr = app.tickets[i].pr.data.as_ref().and_then(Option::as_ref).map(|p| p.number);
    assert_eq!((pr, saved().tickets[i].branch.as_deref()), (Some(146), Some("eng-70-export")));
}

/// Adds ticket `key` to acme through the inbox, without a worktree, then marks git as creating
/// one for it, as `create_worktree` does without starting git. Returns the creation's id.
fn creating(app: &mut App, key: &str) -> u64 {
    request(
        &format!("add-{key}"),
        &serde_json::json!({
            "repo": "/src/acme", "worktree": false, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": key }],
        }),
    );
    app.take_inbox();
    app.creations += 1;
    let i = index(app, key);
    let tv = &mut app.tickets[i];
    tv.creating = true;
    tv.creation_id = app.creations;
    tv.t.pending_worktree = Some(Default::default());
    app.creations
}

/// What git sends when it has created `key`'s worktree, `made` now or found there already.
fn created(app: &App, n: u64, key: &str, made: bool) -> Msg {
    let c = crate::worktree::Created {
        path: format!("/src/acme-{key}").into(),
        branch: format!("{}-export", key.to_lowercase()),
        note: None,
        stacked: false,
        made,
    };
    Msg::Worktree(n, (app.projects[0].repo.clone(), key.into()), Ok(c), None)
}

#[test]
fn a_created_worktree_becomes_the_tickets_and_taskmans_only_if_git_made_it() {
    let mut app = acme_app();
    let n = creating(&mut app, "ENG-90");
    app.tx.send(created(&app, n, "ENG-90", true)).unwrap();
    app.drain_messages();
    let t = &saved().tickets[index(&app, "ENG-90")];
    assert_eq!(t.cwd, std::path::Path::new("/src/acme-ENG-90"));
    assert_eq!(t.branch.as_deref(), Some("eng-90-export"));
    assert!(t.created_worktree && t.pending_worktree.is_none());
    assert!(!app.tickets[index(&app, "ENG-90")].creating);

    // Already the branch's worktree when git looked: someone else's, which a close must leave.
    let n = creating(&mut app, "ENG-91");
    app.tx.send(created(&app, n, "ENG-91", false)).unwrap();
    app.drain_messages();
    let t = &saved().tickets[index(&app, "ENG-91")];
    assert_eq!(t.cwd, std::path::Path::new("/src/acme-ENG-91"));
    assert!(!t.created_worktree && t.pending_worktree.is_none());
}

/// Project web (`web_project`) with ENG-5's worktree made by hand, on the branch taskman would
/// give it. Returns the project and the worktree.
fn hand_made_worktree(app: &mut App) -> (usize, std::path::PathBuf) {
    let (p, repo) = web_project(app);
    let wt = crate::worktree::folder_path(crate::worktree::BESIDE, &repo, "ENG-5");
    sh(&repo, &format!("git worktree add -q -b ENG-5 {}", wt.display()));
    (p, wt)
}

/// The close dialog for the selected ticket offers no removal, and Enter leaves worktree `wt`.
fn closes_keeping(app: &mut App, wt: &std::path::Path) {
    prefix(app, KeyCode::Char('x'));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    assert!(app.close_check.is_none() && app.close_unchecked.is_none());
    app.on_key(key(KeyCode::Enter));
    assert!(!saved().tickets.iter().any(|t| t.key == "ENG-5"));
    assert!(wt.join(".git").exists());
}

#[test]
fn a_hand_made_worktree_found_as_the_ticket_is_added_stays_when_it_closes() {
    let mut app = acme_app();
    let (p, wt) = hand_made_worktree(&mut app);
    let added = app.add_ticket(p, "ENG-5".into(), store::Cli::Claude, true, None, true);
    assert!(matches!(added, Ok(crate::app::ticket::Added::New(_))));
    // Where git lists it: its real path.
    let t = &saved().tickets[app.sel];
    assert_eq!((&t.cwd, t.branch.as_deref()), (&wt.canonicalize().unwrap(), Some("ENG-5")));
    assert!(!t.created_worktree && t.pending_worktree.is_none());
    closes_keeping(&mut app, &wt);
}

#[test]
fn a_hand_made_worktree_found_by_git_stays_when_the_ticket_closes() {
    // Made by hand where git failed, then tried again: git finds it already the branch's.
    let mut app = acme_app();
    let (i, wt) = failed_worktree(&mut app);
    std::fs::remove_dir(&wt).unwrap();
    sh(
        &app.projects.last().unwrap().repo,
        &format!("git worktree add -q -b ENG-5 {}", wt.display()),
    );
    app.on_key(key(KeyCode::Enter));
    app.resume_worktrees();
    settle(&mut app);
    let t = &saved().tickets[i];
    assert_eq!(t.cwd, wt);
    assert!(!t.created_worktree && t.pending_worktree.is_none());
    closes_keeping(&mut app, &wt);
}

#[test]
fn the_folder_is_marked_taskmans_before_git_runs() {
    let mut app = acme_app();
    let (p, repo) = web_project(&mut app);
    let wt = crate::worktree::folder_path(crate::worktree::BESIDE, &repo, "ENG-5");
    let added = app.add_ticket(p, "ENG-5".into(), store::Cli::Claude, true, None, true);
    assert!(matches!(added, Ok(crate::app::ticket::Added::New(_))));
    // A quit now doesn't stop git; the next start finds this.
    let pending = saved().tickets[app.sel].pending_worktree.clone().expect("not pending");
    assert_eq!(pending.making, Some(wt.clone()));
    settle(&mut app);
    let t = &saved().tickets[app.sel];
    assert_eq!(t.cwd, wt);
    assert!(t.created_worktree && t.pending_worktree.is_none());
}

#[test]
fn a_worktree_git_finished_after_a_quit_is_still_taskmans() {
    let mut app = acme_app();
    // As `create_worktree` saved it before a quit, and the worktree git finished after.
    let (p, wt) = hand_made_worktree(&mut app);
    let added = app.add_ticket(p, "ENG-5".into(), store::Cli::Claude, false, None, true);
    assert!(matches!(added, Ok(crate::app::ticket::Added::New(_))));
    let making = Some(wt.clone());
    app.tickets[app.sel].t.pending_worktree =
        Some(store::PendingWorktree { making, ..Default::default() });

    app.resume_worktrees();
    settle(&mut app);
    let t = &saved().tickets[app.sel];
    assert_eq!(t.cwd, wt);
    assert!(t.created_worktree && t.pending_worktree.is_none());
    prefix(&mut app, KeyCode::Char('x'));
    let check = app.close_check.as_ref().expect("removal not considered");
    assert_eq!(check.path, wt);
}

#[test]
fn a_failed_worktree_leaves_its_folder_unclaimed() {
    let mut app = acme_app();
    let (i, _) = failed_worktree(&mut app);
    let pending = saved().tickets[i].pending_worktree.clone().expect("no longer asked for");
    assert!(pending.error.is_some() && pending.making.is_none());
}

#[test]
fn a_worktree_finished_after_its_ticket_closed_is_not_the_reopened_tickets() {
    let mut app = acme_app();
    let n = creating(&mut app, "ENG-90");
    // Closed, keeping the worktree, while git still runs; then added again without one.
    request(
        "close-ENG-90",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-90" }], "close": { "keep_worktree": true },
        }),
    );
    app.take_inbox();
    app.close_next();
    app.drain_messages();
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-90"));
    request(
        "readd-ENG-90",
        &serde_json::json!({
            "repo": "/src/acme", "worktree": false, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-90" }],
        }),
    );
    app.take_inbox();

    app.tx.send(created(&app, n, "ENG-90", true)).unwrap();
    app.drain_messages();
    // The new ticket stays in the main checkout, with nothing to remove when it closes.
    let t = &saved().tickets[index(&app, "ENG-90")];
    assert_eq!(t.cwd, std::path::Path::new("/src/acme"));
    assert_eq!(t.branch, None);
    assert!(!t.created_worktree);
    // The old creation goes as its close asked: kept.
    assert!(app.closed_creating.is_empty());
    assert!(
        toast(&app)
            .is_some_and(|t| t
                .ends_with("closed while its worktree was being created. Kept /src/acme-ENG-90")),
        "{:?}",
        toast(&app)
    );
}

/// The worktree git finished after its ticket closed goes only under the owner lock too.
#[test]
fn a_worktree_finished_after_its_ticket_closed_stays_when_its_session_can_not_be_claimed() {
    let mut app = acme_app();
    let n = creating(&mut app, "ENG-90");
    request(
        "close-ENG-90",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-90" }], "close": {},
        }),
    );
    app.take_inbox();
    app.close_next();
    app.drain_messages();
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-90"));
    // The state folder stops taking locks before git finishes.
    break_lock("/src/acme".as_ref(), "ENG-90");
    app.tx.send(created(&app, n, "ENG-90", true)).unwrap();
    app.drain_messages();
    assert!(app.removing.is_empty(), "removal started without the lock");
    let expected = "ENG-90 closed while its worktree was being created. \
                    Kept /src/acme-ENG-90: couldn't claim its session";
    assert!(toast(&app).is_some_and(|t| t.starts_with(expected)), "{:?}", toast(&app));
}

/// A session never falls back to the main checkout on its own: one whose worktree git couldn't
/// make waits, saying why, until the user retries (Enter) or chooses the main checkout (prefix M).
#[test]
fn a_session_whose_worktree_failed_waits_for_the_user() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let (i, in_the_way) = failed_worktree(&mut app);
    let error = format!(
        "{} already exists; move it, or change Folder in Project settings",
        in_the_way.display()
    );
    assert_eq!(toast(&app), Some(format!("Couldn't create ENG-5's worktree: {error}").as_str()));
    let t = &saved().tickets[i];
    let pending = t.pending_worktree.as_ref().expect("the worktree is no longer asked for");
    assert_eq!(pending.error.as_deref(), Some(error.as_str()));
    assert_eq!((&t.cwd, t.branch.as_deref(), t.created_worktree), (&t.repo, None, false));
    assert!(!app.tickets[i].creating && app.tickets[i].waiting());

    // Held as `taskman resume` holds it, a start that goes wrong finds the claim taken rather
    // than running a CLI in the main checkout.
    let mut outside = resume::take(&resume::owner_file(&t.repo, "ENG-5")).unwrap().unwrap();
    outside.write("outside 4242");
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.away.is_none(), "a session started without its worktree");
    assert_eq!(tv.status.state, AgentState::NotStarted);
    let screen = draw(&mut app, 120, 30);
    assert!(
        screen.contains("Couldn't create a worktree for ENG-5")
            && screen.contains("already exists; move it")
            && screen.contains("Press Enter to try again once it's fixed."),
        "{screen}"
    );
}

/// A workflow's worktree on `change/ENG-70` is what the lookup prefers for the key, but the one
/// taskman created stays the ticket's: a refresh never moves the ticket there, so closing can't
/// offer the workflow's folder and branch for removal.
#[test]
fn a_pr_lookup_never_moves_a_ticket_out_of_the_worktree_taskman_made() {
    let mut app = acme_app();
    let i = index(&app, "ENG-70");
    let tv = &mut app.tickets[i];
    (tv.session, tv.t.session_id) = (None, None);
    assert!(crate::app::ticket::own_worktree(tv));
    let lookup = || Ok(crate::github::Lookup { pr: None, closed: None, branch_gone: false });
    let found = || Some((std::path::PathBuf::from("/src/wf/ENG-70"), "change/ENG-70".to_string()));
    let n = looking_up(&mut app, "ENG-70");
    app.tx.send(Msg::Pr(n, id(&app, "ENG-70"), lookup(), found())).unwrap();
    app.drain_messages();
    let tv = &app.tickets[i];
    assert_eq!(
        (tv.t.cwd.as_path(), tv.t.branch.as_deref(), tv.t.created_worktree),
        (std::path::Path::new("/src/acme-ENG-70"), Some("eng-70-work"), true)
    );
    // Without one of its own, the ticket moves there, and the folder isn't taskman's to remove.
    let tv = &mut app.tickets[i];
    (tv.t.cwd, tv.t.created_worktree) = ("/src/acme".into(), false);
    let n = looking_up(&mut app, "ENG-70");
    app.tx.send(Msg::Pr(n, id(&app, "ENG-70"), lookup(), found())).unwrap();
    app.drain_messages();
    let t = saved().tickets.into_iter().find(|t| t.key == "ENG-70").unwrap();
    assert_eq!(
        (t.cwd.as_path(), t.branch.as_deref(), t.created_worktree),
        (std::path::Path::new("/src/wf/ENG-70"), Some("change/ENG-70"), false)
    );
    assert!(!crate::app::ticket::own_worktree(&app.tickets[i]));
}

#[test]
fn the_links_list_opens_with_the_tickets_links() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('l'));
    let Mode::Links(list) = &app.mode else { panic!("no links list: {:?}", toast(&app)) };
    assert_eq!(list.links[0].url, "https://linear.app/acme/issue/ENG-52");
    assert!(list.links.iter().any(|l| l.url == "https://github.com/acme/acme/pull/128"));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Char('c')));
    assert!(matches!(app.mode, Mode::Normal));
    settle(&mut app);
    assert_eq!(app.clipboard_out.len(), 1);
}

#[test]
fn the_pr_panel_draws_from_the_saved_gh_check_until_the_new_one_answers() {
    use crate::github::Gh;
    let acme = std::path::PathBuf::from("/src/acme");
    // A folder never checked draws without the panel, and `g` says it's coming.
    let mut app = offline(crate::app::test_support::acme());
    draw(&mut app, 160, 40);
    assert!(!app.panels().1);
    prefix(&mut app, KeyCode::Char('g'));
    prefix(&mut app, KeyCode::Char('g'));
    assert_eq!(toast(&app), Some("The PR panel shows once gh is checked, in a moment"));
    app.tx.send(Msg::Gh(acme.clone(), Gh::Ready)).unwrap();
    // A folder no project has any more isn't saved.
    app.tx.send(Msg::Gh("/src/gone".into(), Gh::Ready)).unwrap();
    app.drain_messages();
    assert!(app.panels().1);
    assert_eq!(saved().ui.github, [(acme.clone(), Gh::Ready)].into());

    // The next start draws the panel from what was saved, and hides it once gh says otherwise.
    let mut app = offline(saved());
    assert!(app.panels().1);
    app.tx.send(Msg::Gh(acme.clone(), Gh::LoggedOut)).unwrap();
    app.drain_messages();
    assert!(!app.panels().1);
    assert_eq!(saved().ui.github, [(acme, Gh::LoggedOut)].into());
}

// ---- threads that panic --------------------------------------------------------------------

/// A bug that panics a fetch ends its thread, but the panels still hear of it: they stop loading,
/// keep what they had and say what happened, an error toast points to panics.log, and `r` fetches
/// again.
#[test]
fn a_fetch_that_panics_fails_and_r_fetches_again() {
    let mut app = acme_app();
    panicking(&mut app);
    let i = index(&app, "ENG-52");
    let fetching = |app: &App| (app.tickets[i].jira.loading, app.tickets[i].pr.loading);
    app.schedule_fetches();
    assert_eq!(fetching(&app), (true, true));
    settle_until(&mut app, |app| fetching(app) == (false, false));
    let tv = &app.tickets[i];
    assert_eq!((tv.jira.error.as_deref(), tv.pr.error.as_deref()), (Some(BUG), Some(BUG)));
    assert!(tv.jira.data.is_some() && tv.pr.data.is_some(), "what was fetched stays");
    let log = "~/.local/state/taskman/panics.log";
    assert!(logged_error(&app, &format!("taskman hit a bug fetching ENG-52; details in {log}")));
    let lookup = "looking up the PRs of ENG-52, ENG-53, ENG-61, ENG-70";
    assert!(logged_error(&app, &format!("taskman hit a bug {lookup}; details in {log}")));

    prefix(&mut app, KeyCode::Char('r'));
    app.schedule_fetches();
    assert_eq!(fetching(&app), (true, true));
    settle_until(&mut app, |app| fetching(app) == (false, false));
}

/// A repository's PR lookups share a thread, so a bug that panics it fails every ticket it was
/// looking up: none is left loading, each keeps its PR, one toast names them, and the next
/// refresh looks them all up again.
#[test]
fn a_repositorys_lookup_that_panics_fails_each_of_its_tickets() {
    let mut app = acme_app();
    panicking(&mut app);
    let keys = ["ENG-52", "ENG-53", "ENG-61", "ENG-70"];
    let looking = |app: &App| keys.map(|k| app.tickets[index(app, k)].pr.loading);
    let had = keys.map(|k| app.tickets[index(&app, k)].pr.data.is_some());
    app.schedule_fetches();
    assert_eq!(looking(&app), [true; 4]);
    settle_until(&mut app, |app| looking(app) == [false; 4]);
    for (k, had) in keys.iter().zip(had) {
        let tv = &app.tickets[index(&app, k)];
        assert_eq!(tv.pr.error.as_deref(), Some(BUG), "{k}");
        assert_eq!(tv.pr.data.is_some(), had, "{k} keeps what it had");
    }
    let bugs = app.toast_log.iter().filter(|l| l.msg.contains("looking up the PRs")).count();
    assert_eq!(bugs, 1, "one toast for the repository");
    for k in keys {
        let i = index(&app, k);
        app.tickets[i].pr.request(false);
    }
    app.schedule_fetches();
    assert_eq!(looking(&app), [true; 4]);
    settle_until(&mut app, |app| looking(app) == [false; 4]);
}

/// A bug that panics git's thread still ends the wait for the worktree: the session says it
/// couldn't be created, the tracker fetch it started stops loading, and Enter tries again.
#[test]
fn a_worktree_creation_that_panics_fails_and_enter_tries_again() {
    let mut app = acme_app();
    panicking(&mut app);
    let i = index(&app, "ENG-70");
    app.select(i);
    app.tickets[i].t.pending_worktree = Some(Default::default());
    app.create_worktree(0, i);
    assert!(app.tickets[i].creating && app.tickets[i].jira.loading);
    settle_until(&mut app, |app| !app.tickets[i].creating);
    let tv = &app.tickets[i];
    assert!(!tv.jira.loading, "the tracker fetch is still loading");
    assert_eq!(tv.jira.error.as_deref(), Some(BUG));
    let pending = tv.t.pending_worktree.as_ref().expect("the worktree is no longer asked for");
    assert_eq!(pending.error.as_deref(), Some(BUG));
    assert_eq!(toast(&app), Some(format!("Couldn't create ENG-70's worktree: {BUG}").as_str()));
    let screen = draw(&mut app, 120, 30);
    assert!(
        screen.contains("Couldn't create a worktree for ENG-70")
            && screen.contains("taskman hit a bug"),
        "{screen}"
    );

    app.on_key(key(KeyCode::Enter));
    app.resume_worktrees();
    assert!(app.tickets[i].creating);
    settle_until(&mut app, |app| !app.tickets[i].creating);
}
