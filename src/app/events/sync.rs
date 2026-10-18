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
