use super::*;

// ---- the find box (`prefix /`) -------------------------------------------------------------

fn find_matches(app: &App) -> Vec<&str> {
    let Mode::Find(j) = &app.mode else { panic!("the find box is not open") };
    app.find_matches(&j.query).into_iter().map(|i| app.tickets[i].t.key.as_str()).collect()
}

fn highlighted(app: &App) -> usize {
    let Mode::Find(j) = &app.mode else { panic!("the find box is not open") };
    j.at
}

#[test]
fn the_find_box_narrows_the_cards_and_enter_selects_one() {
    let (mut app, to_session) = at_eng_52();
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('/'));
    assert!(matches!(app.mode, Mode::Find(_)));
    // Nothing typed matches everything, the folded project's agent too.
    assert_eq!(find_matches(&app).len(), app.tickets.len());
    // Typed keys go to the box, not the session.
    type_text(&mut app, "sign");
    assert_eq!(find_matches(&app), ["ENG-52", "ENG-53"]);
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
fn the_find_box_matches_a_number_an_agent_and_a_project() {
    let mut app = acme_app();
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "61");
    assert_eq!(find_matches(&app), ["ENG-61"]);
    for _ in 0..2 {
        app.on_key(key(KeyCode::Backspace));
    }
    type_text(&mut app, "RELEASE");
    assert_eq!(find_matches(&app), ["acme-release-notes-reviewer"]);
    app.on_key(key(KeyCode::Backspace));
    // A folded project's cards are found, and selecting one unfolds the project.
    for _ in 0.."release".len() {
        app.on_key(key(KeyCode::Backspace));
    }
    type_text(&mut app, "notes");
    assert_eq!(find_matches(&app), ["acme-release-notes-reviewer", "notes-writer"]);
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "notes-writer");
    assert!(!app.projects[1].collapsed);
}

#[test]
fn escape_closes_the_find_box_and_keeps_the_selection() {
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
    assert_eq!(find_matches(&app).len(), app.tickets.len());
    assert_eq!(sent(&to_session), "");
}

#[test]
fn the_find_box_with_no_match_says_so_and_enter_does_nothing() {
    let mut app = acme_app();
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "zzz");
    assert!(find_matches(&app).is_empty());
    assert!(draw(&mut app, 120, 30).contains("No matches"));
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    assert!(matches!(app.mode, Mode::Find(_)));
    assert_eq!(selected(&app), "ENG-52");
    // Typing on narrows again from the start.
    app.on_key(key(KeyCode::Backspace));
    app.on_key(key(KeyCode::Backspace));
    app.on_key(key(KeyCode::Backspace));
    type_text(&mut app, "eng-70");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "ENG-70");
}

/// A folded sidebar shows no titles to narrow, so the box is a picker over the screen, with the
/// same keys.
#[test]
fn with_the_sidebar_folded_the_find_box_is_a_picker() {
    let (mut app, to_session) = at_eng_52();
    assert!(app.areas.sidebar_folded, "80 columns fold the sidebar");
    prefix(&mut app, KeyCode::Char('/'));
    assert!(matches!(app.mode, Mode::Find(_)));
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("╭ Find "), "{screen}");
    type_text(&mut app, "writer");
    assert_eq!(find_matches(&app), ["notes-writer"]);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("› ✦ writer") && !screen.contains("ENG-53"), "{screen}");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(selected(&app), "notes-writer");
    assert!(!draw(&mut app, 80, 24).contains("╭ Find "));
    assert_eq!(sent(&to_session), "");

    // Folded with the prefix key in a wide window, the same; Esc keeps the selection.
    app.prefs.sidebar_collapsed = true;
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('/'));
    assert!(draw(&mut app, 120, 30).contains("╭ Find "));
    type_text(&mut app, "sign");
    app.on_key(key(KeyCode::Down));
    assert_eq!(highlighted(&app), 1);
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(selected(&app), "notes-writer");
}

/// The picker lists as many cards as fit and scrolls to keep the highlighted one in view.
#[test]
fn the_find_picker_scrolls_to_the_highlighted_card() {
    let mut app = crate::app::test_support::long_sidebar(20);
    draw(&mut app, 80, 24);
    prefix(&mut app, KeyCode::Char('/'));
    // The last card is an agent of the second project, at the end of a list longer than the box.
    assert_eq!(find_matches(&app).last(), Some(&"notes-writer"));
    let screen = draw(&mut app, 80, 24);
    assert!(!screen.contains("✦ writer") && screen.contains("more"), "{screen}");
    for _ in 0..app.tickets.len() {
        app.on_key(key(KeyCode::Down));
    }
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("› ✦ writer") && screen.contains("↑ "), "{screen}");
    // Back up one, the list stays where it is rather than going back to the top.
    app.on_key(key(KeyCode::Up));
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("› ENG-90") && screen.contains("✦ writer"), "{screen}");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "notes-writer");
}

#[test]
fn the_find_picker_with_no_match_says_so_and_enter_does_nothing() {
    let (mut app, to_session) = at_eng_52();
    prefix(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "zzz");
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("╭ Find ") && screen.contains("No matches"), "{screen}");
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    assert!(matches!(app.mode, Mode::Find(_)));
    assert_eq!(selected(&app), "ENG-52");
    assert_eq!(sent(&to_session), "");
}

/// Widening the window while the picker is open moves the box into the sidebar, keeping what's
/// typed and the highlight; narrowing it again brings the picker back.
#[test]
fn a_resize_while_finding_keeps_the_highlight() {
    let (mut app, to_session) = at_eng_52();
    prefix(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "sign");
    app.on_key(key(KeyCode::Down));
    assert!(draw(&mut app, 80, 24).contains("╭ Find "));
    let screen = draw(&mut app, 160, 40);
    assert!(!app.areas.sidebar_folded && !screen.contains("╭ Find "), "{screen}");
    assert!(screen.contains(" / sign"), "{screen}");
    assert_eq!(highlighted(&app), 1);
    assert!(draw(&mut app, 80, 24).contains("› ENG-53"));
    draw(&mut app, 160, 40);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "ENG-53");
    assert_eq!(sent(&to_session), "");
}
