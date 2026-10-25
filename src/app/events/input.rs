use super::*;

// ---- keys and the session ------------------------------------------------------------------

#[test]
fn keys_and_pastes_reach_the_session() {
    let (mut app, to_session) = at_eng_52();
    app.on_key(key(KeyCode::Char('y')));
    app.on_key(key(KeyCode::Enter));
    app.on_key(ctrl('c'));
    app.on_key(key(KeyCode::Esc));
    // The prefix's own letter after it sends the prefix itself.
    prefix(&mut app, KeyCode::Char('a'));
    app.on_paste("one\ntwo");
    assert_eq!(sent(&to_session), "y\r\x03\x1b\x01one\rtwo");
    assert!(matches!(app.mode, Mode::Normal));

    // A CLI that asked for bracketed paste gets the text as it is, marked.
    let s = app.tickets[app.sel].session.as_ref().unwrap();
    s.output(b"\x1b[?2004h");
    app.on_paste("one\ntwo");
    assert!(sent(&to_session).ends_with("\x1b[200~one\ntwo\x1b[201~"));
}

/// A paste that would leave more than `session::QUEUE` waiting for the CLI is dropped, saying
/// so, rather than wait on it; what's typed next still goes.
#[test]
fn a_paste_too_big_for_the_session_is_dropped_with_a_toast() {
    let (mut app, to_session) = at_eng_52();
    app.on_paste(&"x".repeat(crate::session::QUEUE + 1));
    assert_eq!(toast(&app), Some(super::super::input::NOT_SENT));
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(sent(&to_session), "y");
}

#[test]
fn prefix_commands_never_reach_the_session() {
    let (mut app, to_session) = at_eng_52();
    prefix(&mut app, KeyCode::Char(']'));
    assert_eq!(selected(&app), "ENG-53");
    prefix(&mut app, KeyCode::Char(']'));
    assert_eq!(selected(&app), "ENG-61");
    prefix(&mut app, KeyCode::Char('['));
    prefix(&mut app, KeyCode::Char('['));
    assert_eq!(selected(&app), "ENG-52");
    prefix(&mut app, KeyCode::Char('?'));
    assert!(matches!(app.mode, Mode::Help(_)));
    app.on_key(key(KeyCode::Char('x')));
    assert!(matches!(app.mode, Mode::Normal));
    prefix(&mut app, KeyCode::Char('y'));
    assert_eq!(toast(&app), Some("ctrl+a y isn't a command · ctrl+a ? lists them"));
    // Digits once switched to the sidebar's first nine cards; now they're like any other.
    prefix(&mut app, KeyCode::Char('4'));
    assert_eq!(toast(&app), Some("ctrl+a 4 isn't a command · ctrl+a ? lists them"));
    assert_eq!(selected(&app), "ENG-52");
    assert_eq!(sent(&to_session), "");
    // Switching tickets saves which one is selected.
    assert_eq!(saved().selected, index(&app, "ENG-52"));
}

#[test]
fn the_prefix_twice_goes_to_what_waits_on_the_user() {
    let mut app = acme_app();
    // ENG-53 and notes' writer need the user; ENG-53 comes first after ENG-52.
    app.on_key(ctrl('a'));
    app.on_key(ctrl('a'));
    assert_eq!(selected(&app), "ENG-53");
    app.on_key(ctrl('a'));
    app.on_key(key(KeyCode::Tab));
    assert_eq!(selected(&app), "notes-writer");
    // Selecting a ticket in a folded project unfolds it.
    assert!(!app.projects[1].collapsed);
}

#[test]
fn says_why_a_sidebar_turned_on_stays_folded() {
    let mut app = acme_app();
    app.prefs.sidebar_collapsed = true;
    draw(&mut app, 80, 24);
    prefix(&mut app, KeyCode::Char('b'));
    assert!(!app.prefs.sidebar_collapsed);
    assert_eq!(
        toast(&app),
        Some("The sidebar unfolds beside the session from 91 columns wide; this window is 80")
    );
    assert!(!saved().ui.sidebar_collapsed);
}

#[test]
fn help_scrolls_and_any_other_key_closes_it() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('?'));
    draw(&mut app, 80, 24);
    let Mode::Help(h) = app.mode else { panic!("no help") };
    assert!(h.max > 0, "help fits 80 by 24");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::End));
    let Mode::Help(h) = app.mode else { panic!("help closed") };
    assert_eq!(h.top, h.max);
    mouse(&mut app, MouseEventKind::ScrollUp, 40, 10);
    let Mode::Help(scrolled) = app.mode else { panic!("help closed") };
    assert_eq!(scrolled.top, h.max - 3);
    app.on_key(key(KeyCode::Char('q')));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn quitting_asks_first_and_saves() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('q'));
    assert!(matches!(app.mode, Mode::ConfirmQuit));
    // A stray key doesn't answer it.
    app.on_key(key(KeyCode::Char('y')));
    assert!(matches!(app.mode, Mode::ConfirmQuit) && !app.quit);
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal) && !app.quit);
    prefix(&mut app, KeyCode::Char('q'));
    app.on_key(key(KeyCode::Enter));
    assert!(app.quit && app.may_quit());
    assert_eq!(saved().tickets.len(), app.tickets.len());
}

/// A signal, or the terminal going, quits through the same path as the dialog's Enter.
#[test]
fn a_quit_requested_without_the_dialog_saves_and_stops_the_sessions() {
    let (mut app, _sent) = at_eng_52();
    app.request_quit();
    assert!(app.quit && app.may_quit());
    // What was on screen counts as seen, as after the dialog, and that is what's saved.
    assert_eq!(app.tickets[app.sel].new_counts(), (0, 0));
    assert_eq!(saved().tickets[app.sel].seen_gh, app.tickets[app.sel].t.seen_gh);
    assert!(app.tickets[app.sel].session.is_some());
    app.stop_sessions();
    assert!(app.tickets.iter().all(|tv| tv.session.is_none()));
    // Asked again, nothing more happens.
    app.request_quit();
    assert!(app.quit);
}

// ---- a worktree that failed ----------------------------------------------------------------

#[test]
fn enter_tries_a_failed_worktree_again() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let (i, in_the_way) = failed_worktree(&mut app);
    let before = app.tickets[i].creation_id;
    std::fs::remove_dir(&in_the_way).unwrap();
    app.on_key(key(KeyCode::Enter));
    let pending = app.tickets[i].t.pending_worktree.as_ref().expect("no longer asked for");
    assert_eq!(pending.error, None);
    // The next tick starts it again.
    app.resume_worktrees();
    assert!(app.tickets[i].creating && app.tickets[i].creation_id > before);
    settle(&mut app);
    let t = &saved().tickets[i];
    assert_eq!(t.cwd, in_the_way);
    assert!(t.created_worktree && t.pending_worktree.is_none() && t.branch.is_some());
    assert!(!app.tickets[i].waiting());
    assert!(toast(&app).is_some_and(|t| t.starts_with("ENG-5 works in ")), "{:?}", toast(&app));
}

// ---- prefix commands -----------------------------------------------------------------------

#[test]
fn prefix_j_toggles_the_tracker_panel_and_says_why_it_does_not_show() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    prefix(&mut app, KeyCode::Char('j'));
    assert!(!app.prefs.show_jira && !saved().ui.show_jira);
    assert_eq!(toast(&app), None);
    prefix(&mut app, KeyCode::Char('j'));
    assert!(app.prefs.show_jira && saved().ui.show_jira);
    assert_eq!(toast(&app), None);
    // Turned on for a ticket whose project has no tracker, it says where to connect one.
    app.projects[0].tracker = None;
    app.prefs.show_jira = false;
    prefix(&mut app, KeyCode::Char('j'));
    assert!(app.prefs.show_jira);
    assert_eq!(
        toast(&app),
        Some("This project has no tracker; connect one in its settings (ctrl+a ,)")
    );
}

/// Where the panel is on but can't show, the first press says why rather than turn off what
/// can't be seen.
#[test]
fn first_prefix_j_on_a_ticket_without_a_tracker_panel_says_why() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    app.projects[0].tracker = None;
    prefix(&mut app, KeyCode::Char('j'));
    assert_eq!(
        toast(&app),
        Some("This project has no tracker; connect one in its settings (ctrl+a ,)")
    );
    assert!(app.prefs.show_jira);
}

#[test]
fn first_prefix_g_on_a_ticket_without_a_pr_panel_says_why() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    app.github.insert("/src/acme".into(), crate::github::Gh::LoggedOut);
    prefix(&mut app, KeyCode::Char('g'));
    assert_eq!(toast(&app), Some("No PR panel: gh isn't logged in; run gh auth login"));
    assert!(app.prefs.show_gh);
    // Where it does show, the key still turns it off.
    app.github.insert("/src/acme".into(), crate::github::Gh::Ready);
    prefix(&mut app, KeyCode::Char('g'));
    assert!(!app.prefs.show_gh && !saved().ui.show_gh);
}

#[test]
fn prefix_g_toggles_the_pr_panel_and_says_why_it_does_not_show() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    prefix(&mut app, KeyCode::Char('g'));
    assert!(!app.prefs.show_gh && !saved().ui.show_gh);
    prefix(&mut app, KeyCode::Char('g'));
    assert!(app.prefs.show_gh && saved().ui.show_gh);
    assert_eq!(toast(&app), None);
    app.github.insert("/src/acme".into(), crate::github::Gh::LoggedOut);
    app.prefs.show_gh = false;
    prefix(&mut app, KeyCode::Char('g'));
    assert!(app.prefs.show_gh);
    assert_eq!(toast(&app), Some("No PR panel: gh isn't logged in; run gh auth login"));
}

/// On an agent, the panel keys say what they change for tickets, as its board shows in its
/// panels' place; with no board, they refuse rather than flip what can't be seen.
#[test]
fn panel_keys_on_an_agent_say_what_they_change() {
    let mut app = acme_app();
    lay_out(&mut app, 160, 40);
    app.select(index(&app, "acme-lead"));
    prefix(&mut app, KeyCode::Char('j'));
    assert!(!app.prefs.show_jira && saved().ui.show_gh);
    assert_eq!(toast(&app), Some("Tracker panels off for tickets"));
    prefix(&mut app, KeyCode::Char('g'));
    assert_eq!(toast(&app), Some("PR panels off for tickets; the board hides while both are off"));
    prefix(&mut app, KeyCode::Char('g'));
    assert_eq!(toast(&app), Some("PR panels on for tickets; the board shows here"));
    prefix(&mut app, KeyCode::Char('c'));
    assert!(!app.prefs.show_comments);
    assert_eq!(toast(&app), Some("Comments in tracker panels off for tickets"));

    // notes has no tracker and no tickets, so no board.
    app.select(index(&app, "notes-writer"));
    let before = (app.prefs.show_jira, app.prefs.show_comments, app.prefs.show_gh);
    for c in ['j', 'c', 'g'] {
        prefix(&mut app, KeyCode::Char(c));
        assert_eq!(toast(&app), Some("Agents have no panels"));
    }
    assert_eq!((app.prefs.show_jira, app.prefs.show_comments, app.prefs.show_gh), before);
}

#[test]
fn prefix_o_opens_the_tickets_issue() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('o'));
    assert_eq!(toast(&app), Some("Could not open https://linear.app/acme/issue/ENG-52"));
    let opened = browser(&mut app);
    prefix(&mut app, KeyCode::Char('o'));
    assert_eq!(*opened.lock().unwrap(), ["https://linear.app/acme/issue/ENG-52"]);
    app.select(index(&app, "acme-lead"));
    prefix(&mut app, KeyCode::Char('o'));
    assert_eq!(toast(&app), Some("Agents have no ticket to open"));
    prefix(&mut app, KeyCode::Char('p'));
    assert_eq!(toast(&app), Some("Agents have no PR to open"));
    prefix(&mut app, KeyCode::Char('m'));
    assert_eq!(toast(&app), Some("Agents have no comments to mark as read"));
    app.select(index(&app, "ENG-52"));
    app.projects[0].tracker = None;
    prefix(&mut app, KeyCode::Char('o'));
    assert_eq!(toast(&app), Some("This ticket's project has no tracker set"));
    assert_eq!(opened.lock().unwrap().len(), 1);
}

/// A ticket on a Local files board opens as its file, in the editor.
#[test]
fn prefix_o_edits_a_boards_ticket() {
    let dir = store::state_dir().join("plan");
    let board = crate::local::board(&dir, crate::local::FOLDER);
    crate::local::ensure_columns(&board).unwrap();
    let file = board.join("doing/WP-2-export.md");
    std::fs::write(&file, "# Export to CSV\n").unwrap();
    let mut app = acme_app();
    let plan = serde_json::json!({
        "name": "plan", "repo": dir,
        "tracker": { "provider": "local", "site": "", "key": "WP" },
    });
    app.projects.push(serde_json::from_value(plan).unwrap());
    app.github.insert(dir.clone(), crate::github::Gh::NotGit);
    let p = app.projects.len() - 1;
    for key in ["WP-2", "WP-3"] {
        let added = app.add_ticket(p, key.into(), store::Cli::Claude, false, None, true);
        assert!(added.is_ok(), "{:?}", added.err());
    }
    let opened = browser(&mut app);
    prefix(&mut app, KeyCode::Char('o'));
    assert!(app.edit.is_none(), "WP-3 has no file");
    assert!(toast(&app).is_some_and(|t| t.contains("WP-3")), "{:?}", toast(&app));
    app.select(index(&app, "WP-2"));
    prefix(&mut app, KeyCode::Char('o'));
    assert_eq!(app.edit, Some(file));
    assert!(opened.lock().unwrap().is_empty());
}

#[test]
fn prefix_p_opens_the_tickets_pr() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('p'));
    assert_eq!(toast(&app), Some("Could not open https://github.com/acme/acme/pull/128"));
    let opened = browser(&mut app);
    prefix(&mut app, KeyCode::Char('p'));
    assert_eq!(*opened.lock().unwrap(), ["https://github.com/acme/acme/pull/128"]);
    app.select(index(&app, "ENG-70"));
    prefix(&mut app, KeyCode::Char('p'));
    assert_eq!(toast(&app), Some("No PR found for this ticket yet"));
    assert_eq!(opened.lock().unwrap().len(), 1);
}

/// `prefix r` fetches the ticket's issue and PR again, with a spinner, and checks again whether
/// its folder can show PRs: gh may have been logged in since.
#[test]
fn prefix_r_refreshes_the_ticket_and_checks_github_again() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('r'));
    let tv = &mut app.tickets[app.sel];
    assert!(tv.jira.force && tv.pr.force);
    // Started, as the next tick starts them, both show a spinner.
    tv.jira.start(u64::MAX);
    tv.pr.start(u64::MAX);
    assert!(tv.jira.loud && tv.pr.loud);
    // There's no /src/acme to find a remote in.
    for _ in 0..10 {
        if app.gh("/src/acme".as_ref()) != crate::github::Gh::Ready {
            break;
        }
        settle(&mut app);
    }
    assert_eq!(app.gh("/src/acme".as_ref()), crate::github::Gh::NotGit);
}

#[test]
fn prefix_m_takes_the_new_markers_off() {
    let mut app = acme_app();
    // ENG-53 was read when selected, whatever its panels show; its new comments keep `●`.
    app.prefs.show_gh = false;
    app.select(index(&app, "ENG-53"));
    let tv = &app.tickets[app.sel];
    let pr = tv.pr.data.clone().flatten().unwrap();
    assert_eq!(tv.new_counts(), (0, 0));
    assert!(pr.comments.iter().all(|c| tv.is_new_gh(&format!("c:{}", c.id))));
    assert_eq!(saved().tickets[app.sel].seen_gh, tv.t.seen_gh);
    prefix(&mut app, KeyCode::Char('m'));
    assert_eq!(toast(&app), Some("Marked as read"));
    let tv = &app.tickets[app.sel];
    assert!(!pr.comments.iter().any(|c| tv.is_new_gh(&format!("c:{}", c.id))));
    prefix(&mut app, KeyCode::Char('m'));
    assert_eq!(toast(&app), Some("Nothing new on this ticket"));
}

/// How far the selected session's view is scrolled back.
fn scrolled(app: &App) -> usize {
    app.tickets[app.sel].session.as_ref().unwrap().scrollback()
}

/// ENG-52 with `n` lines of output, most of them in its history.
fn with_history(n: usize) -> (App, crate::session::fake::Sent) {
    let (app, to_session) = at_eng_52();
    let lines: String = (1..=n).map(|n| format!("line {n}\r\n")).collect();
    app.tickets[app.sel].session.as_ref().unwrap().output(lines.as_bytes());
    (app, to_session)
}

#[test]
fn page_up_and_down_scroll_the_session_or_reach_one_that_wants_the_mouse() {
    let (mut app, to_session) = with_history(100);
    // A page less a row, which stays in view.
    let page = usize::from(app.areas.claude.height) - 1;
    prefix(&mut app, KeyCode::PageUp);
    prefix(&mut app, KeyCode::PageUp);
    assert_eq!(scrolled(&app), 2 * page);
    prefix(&mut app, KeyCode::PageDown);
    assert_eq!(scrolled(&app), page);
    assert_eq!(sent(&to_session), "");
    // A CLI that turned on mouse reporting scrolls itself: it gets the keys, with the prefix or
    // without.
    let s = app.tickets[app.sel].session.as_ref().unwrap();
    s.output(b"\x1b[?1000h");
    prefix(&mut app, KeyCode::PageUp);
    prefix(&mut app, KeyCode::PageDown);
    app.on_key(key(KeyCode::PageUp));
    assert_eq!(sent(&to_session), "\x1b[5~\x1b[6~\x1b[5~");
    assert!(!app.scrolling_back());
}

/// After `prefix PgUp`, PgUp/PgDn, ↑/↓ and Home/End go on scrolling without the prefix, and
/// none of them reaches the session; End, Esc or scrolling back to the bottom ends it.
#[test]
fn keyboard_scrollback_keeps_going_without_the_prefix() {
    let (mut app, to_session) = with_history(10_200);
    let page = usize::from(app.areas.claude.height) - 1;
    let held = {
        let s = app.tickets[app.sel].session.as_ref().unwrap();
        crate::session::lock(&s.parser).screen().scrollback_len()
    };
    prefix(&mut app, KeyCode::PageUp);
    app.on_key(key(KeyCode::PageUp));
    app.on_key(key(KeyCode::PageUp));
    assert_eq!(scrolled(&app), 3 * page);
    app.on_key(key(KeyCode::PageDown));
    app.on_key(key(KeyCode::Up));
    assert_eq!(scrolled(&app), 2 * page + 1);
    app.on_key(key(KeyCode::Down));
    assert_eq!(scrolled(&app), 2 * page);
    app.on_key(key(KeyCode::Home));
    assert_eq!(scrolled(&app), held);
    // Past the top it stays there.
    app.on_key(key(KeyCode::PageUp));
    assert_eq!(scrolled(&app), held);
    assert!(app.scrolling_back());
    app.on_key(key(KeyCode::Esc));
    assert_eq!(scrolled(&app), 0);
    assert!(!app.scrolling_back());
    // Esc went no further: Claude takes it to interrupt.
    assert_eq!(sent(&to_session), "");
    // Over, the keys reach the session again.
    app.on_key(key(KeyCode::PageUp));
    assert_eq!(sent(&to_session), "\x1b[5~");

    // End returns to the bottom too, and so does paging down to it.
    let mut expected = "\x1b[5~".to_string();
    for end in [vec![KeyCode::End], vec![KeyCode::Up, KeyCode::Down, KeyCode::PageDown]] {
        prefix(&mut app, KeyCode::PageUp);
        for code in end {
            app.on_key(key(code));
        }
        assert_eq!(scrolled(&app), 0);
        assert!(!app.scrolling_back());
        app.on_key(key(KeyCode::Up));
        expected += "\x1b[A";
        assert_eq!(sent(&to_session), expected);
    }
    assert!(matches!(app.mode, Mode::Normal));
}

/// Any other key ends keyboard scrollback and goes to the session, as typing in a scrolled-back
/// view always has; so does a paste. The prefix's commands leave it be.
#[test]
fn keyboard_scrollback_ends_with_typing_and_the_key_goes_on() {
    let (mut app, to_session) = with_history(500);
    prefix(&mut app, KeyCode::PageUp);
    app.on_key(key(KeyCode::Char('g')));
    assert_eq!(scrolled(&app), 0);
    assert!(!app.scrolling_back());
    assert_eq!(sent(&to_session), "g");
    // A key with a modifier is the CLI's too.
    prefix(&mut app, KeyCode::PageUp);
    app.on_key(ctrl('c'));
    assert_eq!((scrolled(&app), sent(&to_session).as_str()), (0, "g\x03"));

    prefix(&mut app, KeyCode::PageUp);
    prefix(&mut app, KeyCode::Char('m'));
    assert!(app.scrolling_back(), "a command ended it");
    app.on_paste("hi");
    assert_eq!(scrolled(&app), 0);
    assert!(!app.scrolling_back());
    assert_eq!(sent(&to_session), "g\x03hi");
}

/// The wheel scrolls without starting keyboard scrollback, and ends it at the bottom: the arrows
/// then go to the session, for Claude's own history of prompts.
#[test]
fn the_wheel_neither_starts_keyboard_scrollback_nor_keeps_it_past_the_bottom() {
    let (mut app, to_session) = with_history(500);
    let c = app.areas.claude;
    mouse(&mut app, MouseEventKind::ScrollUp, c.x + 1, c.y + 1);
    assert_eq!(scrolled(&app), 3);
    app.on_key(key(KeyCode::Up));
    assert_eq!((scrolled(&app), sent(&to_session).as_str()), (0, "\x1b[A"));

    prefix(&mut app, KeyCode::PageUp);
    let page = scrolled(&app);
    for _ in 0..page.div_ceil(3) {
        mouse(&mut app, MouseEventKind::ScrollDown, c.x + 1, c.y + 1);
    }
    assert_eq!(scrolled(&app), 0);
    mouse(&mut app, MouseEventKind::ScrollUp, c.x + 1, c.y + 1);
    app.on_key(key(KeyCode::Up));
    assert_eq!((scrolled(&app), sent(&to_session).as_str()), (0, "\x1b[A\x1b[A"));
}

/// Keyboard scrollback belongs to the session it scrolled: another ticket's keys go to its own
/// session, and coming back finds it still scrolled back, the keys still scrolling it.
#[test]
fn keyboard_scrollback_stays_with_its_session() {
    let (mut app, to_52) = with_history(500);
    prefix(&mut app, KeyCode::PageUp);
    let back = scrolled(&app);
    prefix(&mut app, KeyCode::Char(']'));
    assert_eq!(selected(&app), "ENG-53");
    let to_53 = attach(&mut app, "ENG-53", SCREEN);
    assert!(!app.scrolling_back());
    app.on_key(key(KeyCode::PageUp));
    assert_eq!(sent(&to_53), "\x1b[5~");
    prefix(&mut app, KeyCode::Char('['));
    assert_eq!(selected(&app), "ENG-52");
    assert!(app.scrolling_back());
    app.on_key(key(KeyCode::Up));
    assert_eq!(scrolled(&app), back + 1);
    assert_eq!(sent(&to_52), "");
}

/// Keyboard scrollback starts only where `prefix PgUp` left the view scrolled back: not after
/// `prefix PgDn` to the bottom, a history with nothing in it, or a clear of the history. The
/// wheel scrolling back later leaves the arrows and Esc to the session.
#[test]
fn keyboard_scrollback_is_not_left_armed_at_the_bottom() {
    let (mut app, to_session) = with_history(500);
    let c = app.areas.claude;
    let wheel_up = |app: &mut App| mouse(app, MouseEventKind::ScrollUp, c.x + 1, c.y + 1);
    let mut expected = String::new();
    let mut arrow_reaches_the_session = |app: &mut App, why: &str| {
        wheel_up(app);
        assert!(!app.scrolling_back(), "{why}");
        app.on_key(key(KeyCode::Up));
        expected += "\x1b[A";
        assert_eq!(sent(&to_session), expected, "{why}");
    };
    prefix(&mut app, KeyCode::PageUp);
    prefix(&mut app, KeyCode::PageDown);
    arrow_reaches_the_session(&mut app, "after prefix PgDn to the bottom");
    prefix(&mut app, KeyCode::PageDown);
    arrow_reaches_the_session(&mut app, "after prefix PgDn at the bottom");
    prefix(&mut app, KeyCode::PageUp);
    let s = app.tickets[app.sel].session.as_ref().unwrap();
    s.output(b"\x1b[3J");
    assert_eq!(scrolled(&app), 0);
    let s = app.tickets[app.sel].session.as_ref().unwrap();
    s.output("line\r\n".repeat(50).as_bytes());
    arrow_reaches_the_session(&mut app, "after a clear of the history");

    // No history at all: nothing to scroll back to.
    let (mut app, to_session) = at_eng_52();
    prefix(&mut app, KeyCode::PageUp);
    assert_eq!(scrolled(&app), 0);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(sent(&to_session), "\x1b");
}

/// The wheel and a paste in one session leave another's keyboard scrollback as it was.
#[test]
fn one_session_leaves_anothers_keyboard_scrollback_alone() {
    let (mut app, _) = with_history(500);
    prefix(&mut app, KeyCode::PageUp);
    let back = scrolled(&app);
    prefix(&mut app, KeyCode::Char(']'));
    let lines: String = (1..=500).map(|n| format!("line {n}\r\n")).collect();
    attach(&mut app, "ENG-53", &lines);
    let c = app.areas.claude;
    mouse(&mut app, MouseEventKind::ScrollUp, c.x + 1, c.y + 1);
    mouse(&mut app, MouseEventKind::ScrollDown, c.x + 1, c.y + 1);
    app.on_paste("hi");
    prefix(&mut app, KeyCode::Char('['));
    assert_eq!(selected(&app), "ENG-52");
    assert!(app.scrolling_back());
    assert_eq!(scrolled(&app), back);
}

/// `prefix prefix` to a session waiting on you shows it live, where its question is.
#[test]
fn going_to_a_waiting_session_returns_it_to_live() {
    let (mut app, _) = with_history(10);
    prefix(&mut app, KeyCode::Char(']'));
    assert_eq!(selected(&app), "ENG-53");
    let lines: String = (1..=500).map(|n| format!("line {n}\r\n")).collect();
    attach(&mut app, "ENG-53", &lines);
    prefix(&mut app, KeyCode::PageUp);
    prefix(&mut app, KeyCode::Char('['));
    app.on_key(ctrl('a'));
    app.on_key(ctrl('a'));
    assert_eq!(selected(&app), "ENG-53");
    assert_eq!(scrolled(&app), 0);
    assert!(!app.scrolling_back());
}

/// `prefix a` sends the prefix, and like any key typed it returns the view to the bottom.
#[test]
fn sending_the_prefix_ends_keyboard_scrollback() {
    let (mut app, to_session) = with_history(500);
    prefix(&mut app, KeyCode::PageUp);
    prefix(&mut app, KeyCode::Char('a'));
    assert_eq!((scrolled(&app), sent(&to_session).as_str()), (0, "\x01"));
    assert!(!app.scrolling_back());
}

/// A pane that changes size keeps its view on the same rows, still scrolling by key: toggling a
/// panel changes its width, which moves no rows, and a taller window takes rows out of the
/// history.
#[test]
fn a_resize_keeps_the_place_in_the_history() {
    let (mut app, _) = with_history(500);
    let top_row = |app: &App| {
        let s = app.tickets[app.sel].session.as_ref().unwrap();
        let p = crate::session::lock(&s.parser);
        p.screen().rows(0, 40).next().unwrap()
    };
    prefix(&mut app, KeyCode::PageUp);
    app.on_key(key(KeyCode::PageUp));
    let (back, top) = (scrolled(&app), top_row(&app));
    prefix(&mut app, KeyCode::Char('b'));
    lay_out(&mut app, 80, 24);
    assert_eq!((scrolled(&app), top_row(&app)), (back, top.clone()));
    lay_out(&mut app, 80, 30);
    assert_eq!(top_row(&app), top);
    assert!(app.scrolling_back());
}

/// Scrolled to the top of a history that has dropped rows, the pane says where the rest is, in
/// the CLI's words, and stops saying it when scrolled down; none of it reaches the session.
#[test]
fn the_top_of_a_trimmed_history_points_to_the_clis_transcript() {
    let (mut app, to_session) = at_eng_52();
    let hint = "Older output: ctrl+o shows Claude's full transcript";
    let to_top = |app: &mut App| {
        prefix(app, KeyCode::PageUp);
        app.on_key(key(KeyCode::Home));
    };
    let s = app.tickets[app.sel].session.as_ref().unwrap();
    let lines: String = (1..=100).map(|n| format!("line {n}\r\n")).collect();
    s.output(lines.as_bytes());
    to_top(&mut app);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("line 1 ") && !screen.contains(hint), "{screen}");

    let s = app.tickets[app.sel].session.as_ref().unwrap();
    let lines: String =
        (101..=crate::store::SCROLLBACK + 200).map(|n| format!("line {n}\r\n")).collect();
    s.output(lines.as_bytes());
    to_top(&mut app);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains(hint) && !screen.contains("line 1 "), "{screen}");
    prefix(&mut app, KeyCode::PageDown);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("lines back") && !screen.contains(hint), "{screen}");
    assert_eq!(sent(&to_session), "");

    // Codex's key for a Codex session.
    app.tickets[app.sel].t.cli = crate::store::Cli::Codex;
    to_top(&mut app);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("Older output: ctrl+t shows Codex's full transcript"), "{screen}");
}

#[test]
fn the_links_list_opens_the_link_picked() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('l'));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("No browser found: set $BROWSER, or c copies the link"));
    let opened = browser(&mut app);
    prefix(&mut app, KeyCode::Char('l'));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("Opened https://linear.app/acme/issue/ENG-52"));
    assert_eq!(*opened.lock().unwrap(), ["https://linear.app/acme/issue/ENG-52"]);
}

#[test]
fn the_links_list_stops_at_both_ends_and_keeps_the_choice_in_view() {
    let mut app = acme_app();
    let links = (0..30)
        .map(|k| crate::links::Link { url: format!("https://example.com/{k:02}"), label: None })
        .collect();
    app.mode = Mode::Links(Box::new(crate::app::LinkList { links, sel: 0, panel: 0, top: 0 }));
    let sel = |app: &App| match &app.mode {
        Mode::Links(list) => (list.sel, list.top),
        _ => panic!("links closed"),
    };
    // ↑ on the first link stays there.
    app.on_key(key(KeyCode::Up));
    assert_eq!(sel(&app), (0, 0));
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("› https://example.com/00"), "{screen}");
    assert!(screen.contains("↓ 24 more"), "{screen}");
    // ↓ down the list: each link shows as it's reached, and ↓ on the last stays there.
    for k in 1..=30 {
        app.on_key(key(KeyCode::Down));
        let screen = draw(&mut app, 80, 24);
        let k = k.min(29);
        assert_eq!(sel(&app).0, k);
        assert!(screen.contains(&format!("› https://example.com/{k:02}")), "{k}:\n{screen}");
    }
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("↑ 24 more") && screen.matches(" more").count() == 1, "{screen}");
    // Back up one: that link was in view already, so the list stays put.
    let top = sel(&app).1;
    app.on_key(key(KeyCode::Up));
    let screen = draw(&mut app, 80, 24);
    assert_eq!(sel(&app), (28, top));
    assert!(screen.contains("https://example.com/29"), "{screen}");
}

#[test]
fn the_links_list_shows_a_groups_heading_with_its_first_link() {
    let mut app = acme_app();
    let links = (0..30)
        .map(|k| crate::links::Link { url: format!("https://example.com/{k:02}"), label: None })
        .collect();
    app.mode = Mode::Links(Box::new(crate::app::LinkList { links, sel: 0, panel: 15, top: 0 }));
    app.on_key(key(KeyCode::End));
    draw(&mut app, 80, 24);
    // Back up to the session's first link, out of view: its heading comes into view above it.
    for _ in 0..14 {
        app.on_key(key(KeyCode::Up));
        draw(&mut app, 80, 24);
    }
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("The session, newest first"), "{screen}");
    assert!(screen.contains("› https://example.com/15"), "{screen}");
}
