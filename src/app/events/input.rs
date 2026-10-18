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

#[test]
fn prefix_commands_never_reach_the_session() {
    let (mut app, to_session) = at_eng_52();
    prefix(&mut app, KeyCode::Char('4'));
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
    assert_eq!(app.tickets[app.sel].new_count(), 0);
    assert_eq!(saved().tickets[app.sel].seen_gh, app.tickets[app.sel].t.seen_gh);
    assert!(app.tickets[app.sel].session.is_some());
    app.stop_sessions();
    assert!(app.tickets.iter().all(|tv| tv.session.is_none()));
    // Asked again, nothing more happens.
    app.request_quit();
    assert!(app.quit);
}
