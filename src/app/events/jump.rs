use super::*;

// ---- the jump box (`prefix /`) -------------------------------------------------------------

fn jump_matches(app: &App) -> Vec<&str> {
    let Mode::Jump(j) = &app.mode else { panic!("the jump box is not open") };
    app.jump_matches(&j.query).into_iter().map(|i| app.tickets[i].t.key.as_str()).collect()
}

fn highlighted(app: &App) -> usize {
    let Mode::Jump(j) = &app.mode else { panic!("the jump box is not open") };
    j.at
}

#[test]
fn the_jump_box_narrows_the_cards_and_enter_selects_one() {
    let (mut app, to_session) = at_eng_52();
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('/'));
    assert!(matches!(app.mode, Mode::Jump(_)));
    // Nothing typed matches everything, the folded project's agent too.
    assert_eq!(jump_matches(&app).len(), app.tickets.len());
    // Typed keys go to the box, not the session.
    type_text(&mut app, "sign");
    assert_eq!(jump_matches(&app), ["ENG-52", "ENG-53"]);
    assert_eq!(highlighted(&app), 0);
    app.on_key(key(KeyCode::Down));
    assert_eq!(highlighted(&app), 1);
    // The ends hold.
    app.on_key(key(KeyCode::Down));
    assert_eq!(highlighted(&app), 1);
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Up));
    assert_eq!(highlighted(&app), 0);
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(selected(&app), "ENG-53");
    assert_eq!(sent(&to_session), "");
}

#[test]
fn the_jump_box_matches_a_number_an_agent_and_a_project() {
    let mut app = acme_app();
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "61");
    assert_eq!(jump_matches(&app), ["ENG-61"]);
    for _ in 0..2 {
        app.on_key(key(KeyCode::Backspace));
    }
    type_text(&mut app, "RELEASE");
    assert_eq!(jump_matches(&app), ["acme-release-notes-reviewer"]);
    app.on_key(key(KeyCode::Backspace));
    // A folded project's cards are found, and selecting one unfolds the project.
    for _ in 0.."release".len() {
        app.on_key(key(KeyCode::Backspace));
    }
    type_text(&mut app, "notes");
    assert_eq!(jump_matches(&app), ["acme-release-notes-reviewer", "notes-writer"]);
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "notes-writer");
    assert!(!app.projects[1].collapsed);
}

#[test]
fn escape_closes_the_jump_box_and_keeps_the_selection() {
    let (mut app, to_session) = at_eng_52();
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "eng-6");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(selected(&app), "ENG-52");
    // The full sidebar is back, and the box starts empty the next time.
    assert!(draw(&mut app, 120, 30).contains("ENG-53"));
    prefix(&mut app, KeyCode::Char('/'));
    assert_eq!(jump_matches(&app).len(), app.tickets.len());
    assert_eq!(sent(&to_session), "");
}

#[test]
fn the_jump_box_with_no_match_says_so_and_enter_does_nothing() {
    let mut app = acme_app();
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "zzz");
    assert!(jump_matches(&app).is_empty());
    assert!(draw(&mut app, 120, 30).contains("No matches"));
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    assert!(matches!(app.mode, Mode::Jump(_)));
    assert_eq!(selected(&app), "ENG-52");
    // Typing on narrows again from the start.
    app.on_key(key(KeyCode::Backspace));
    app.on_key(key(KeyCode::Backspace));
    app.on_key(key(KeyCode::Backspace));
    type_text(&mut app, "eng-70");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "ENG-70");
}

#[test]
fn the_jump_box_needs_the_sidebar() {
    let mut app = acme_app();
    app.prefs.sidebar_collapsed = true;
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('/'));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("Unfold the sidebar to jump (ctrl+a b)"));
    // Too narrow for the sidebar, it can't show the box either.
    app.prefs.sidebar_collapsed = false;
    draw(&mut app, 80, 24);
    prefix(&mut app, KeyCode::Char('/'));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(toast(&app).unwrap().contains("too narrow"));
}
