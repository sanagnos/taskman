use super::*;

// ---- the waiting-on-you queue (`prefix w`) ---------------------------------------------------

/// Makes `key` wait on a permission prompt that began `secs` ago, asking for `input`.
fn waits(app: &mut App, key: &str, secs: i64, input: Option<&str>) {
    let i = index(app, key);
    let st = &mut app.tickets[i].status;
    st.state = AgentState::Needs;
    st.permission = input.is_some();
    st.tool = input.map(|_| "Bash".into());
    st.input = input.map(str::to_string);
    st.waiting_since = Some(crate::util::now() - secs);
}

fn rows(app: &App) -> Vec<&str> {
    app.queue().into_iter().map(|i| app.tickets[i].t.key.as_str()).collect()
}

#[test]
fn the_queue_lists_every_waiting_session_oldest_first() {
    let mut app = acme_app();
    waits(&mut app, "ENG-53", 270, Some("cargo test"));
    waits(&mut app, "notes-writer", 900, None);
    // Waiting, with no time recorded: after the ones that have one.
    waits(&mut app, "ENG-61", 0, None);
    let i = index(&app, "ENG-61");
    app.tickets[i].status.waiting_since = None;
    // A finished turn is news, not a request.
    assert_eq!(app.tickets[index(&app, "acme-lead")].status.state, AgentState::Done);
    assert_eq!(rows(&app), ["notes-writer", "ENG-53", "ENG-61"]);
}

/// The queue shows the session chosen live, where its question is, though it was scrolled back.
#[test]
fn the_queue_returns_the_chosen_session_to_live() {
    let (mut app, _) = at_eng_52();
    waits(&mut app, "ENG-53", 270, Some("cargo test"));
    prefix(&mut app, KeyCode::Char(']'));
    let lines: String = (1..=500).map(|n| format!("line {n}\r\n")).collect();
    attach(&mut app, "ENG-53", &lines);
    prefix(&mut app, KeyCode::PageUp);
    prefix(&mut app, KeyCode::Char('['));
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('w'));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "ENG-53");
    assert_eq!(app.tickets[app.sel].session.as_ref().unwrap().scrollback(), 0);
}

#[test]
fn enter_goes_to_the_chosen_session_and_closes_the_queue() {
    let (mut app, to_session) = at_eng_52();
    waits(&mut app, "ENG-53", 270, Some("cargo test"));
    waits(&mut app, "notes-writer", 900, None);
    draw(&mut app, 120, 30);
    prefix(&mut app, KeyCode::Char('w'));
    assert!(matches!(app.mode, Mode::Queue(_)));
    // Keys go to the list, not the session.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(selected(&app), "ENG-53");
    assert_eq!(sent(&to_session), "");
}

#[test]
fn escape_closes_the_queue_and_keeps_the_selection() {
    let mut app = acme_app();
    waits(&mut app, "ENG-53", 270, None);
    let before = app.sel;
    prefix(&mut app, KeyCode::Char('w'));
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.sel, before);
}

#[test]
fn with_nothing_waiting_the_queue_says_so() {
    let mut app = acme_app();
    for tv in &mut app.tickets {
        tv.status.state = AgentState::Idle;
    }
    prefix(&mut app, KeyCode::Char('w'));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("Nothing needs you"));
}

/// The footer counts a finished turn apart from what needs you, and `prefix w`, which lists only
/// the latter, says where the finished ones are.
#[test]
fn the_footer_and_the_queue_agree_on_a_finished_turn() {
    let mut app = acme_app();
    for tv in &mut app.tickets {
        tv.status.state = AgentState::Idle;
    }
    let i = index(&app, "ENG-61");
    app.tickets[i].status.state = AgentState::Done;
    app.tickets[i].status.seen = false;
    let footer = draw(&mut app, 120, 30).lines().last().unwrap_or_default().to_string();
    assert!(footer.ends_with("● 1 finished \""), "{footer}");
    assert!(!footer.contains("needs you"), "{footer}");
    prefix(&mut app, KeyCode::Char('w'));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("Nothing needs you; 1 finished: ctrl+a ctrl+a goes to it"));
    waits(&mut app, "ENG-53", 270, None);
    let footer = draw(&mut app, 120, 30).lines().last().unwrap_or_default().to_string();
    assert!(footer.ends_with("▲ 1 needs you · ● 1 finished \""), "{footer}");
    assert_eq!(rows(&app), ["ENG-53"]);
}

#[test]
fn a_session_that_stops_waiting_leaves_the_open_queue() {
    let mut app = acme_app();
    waits(&mut app, "ENG-53", 270, None);
    waits(&mut app, "notes-writer", 900, None);
    prefix(&mut app, KeyCode::Char('w'));
    app.on_key(key(KeyCode::Down));
    let i = index(&app, "ENG-53");
    app.tickets[i].status.state = AgentState::Working;
    // The chosen row is gone: Enter takes the last one left.
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "notes-writer");
}
