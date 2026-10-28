use super::*;
use crate::events_log::{self, Turn};

// ---- a session's turns (`prefix d`) ----------------------------------------------------------

/// The log of `key` in the acme project.
fn log(key: &str) -> std::path::PathBuf {
    events_log::log_path(&store::status_file("/src/acme".as_ref(), key))
}

/// Adds a turn to `key`'s log, as its Stop hook would.
fn turn(key: &str, message: &str) {
    events_log::append(&log(key), &Turn::new(message, vec!["src/a.rs".into()], vec![], 1)).unwrap();
}

/// The session the open overlay shows, and its turns' messages once read.
fn shown(app: &App) -> (String, Option<Vec<String>>) {
    let Mode::Turns(t) = &app.mode else { panic!("no turns open") };
    let turns = t.turns.as_ref().map(|ts| ts.iter().map(|t| t.message.clone()).collect());
    (t.id.1.clone(), turns)
}

/// Opens the overlay on the selection and waits for its log to be read.
fn open(app: &mut App) {
    prefix(app, KeyCode::Char('d'));
    settle_until(app, |a| matches!(&a.mode, Mode::Turns(t) if t.turns.is_some()));
}

#[test]
fn the_overlay_lists_the_selected_sessions_turns_newest_first() {
    let (mut app, to_session) = at_eng_52();
    turn("ENG-52", "Fixed the lint failure.");
    turn("ENG-52", "Opened the PR.");
    open(&mut app);
    assert_eq!(
        shown(&app),
        ("ENG-52".into(), Some(vec!["Opened the PR.".into(), "Fixed the lint failure.".into()]))
    );
    // Keys go to the overlay, not the session.
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('x')));
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(sent(&to_session), "");
}

/// The turn selected, whether it's open, and the first row shown.
fn place(app: &App) -> (usize, bool, usize) {
    let Mode::Turns(t) = &app.mode else { panic!("no turns open") };
    (t.at, t.open, t.top)
}

#[test]
fn left_and_right_show_another_session_and_g_goes_to_it() {
    let (mut app, _) = at_eng_52();
    let order: Vec<String> =
        app.ordered(true).into_iter().map(|i| app.tickets[i].t.key.clone()).collect();
    let here = order.iter().position(|k| k == "ENG-52").unwrap();
    let next = &order[(here + 1) % order.len()];
    turn(next, "Answered the review.");
    open(&mut app);
    assert_eq!(shown(&app).1, Some(vec![]), "ENG-52 has no turns yet");
    app.on_key(key(KeyCode::Right));
    settle_until(&mut app, |a| matches!(&a.mode, Mode::Turns(t) if t.turns.is_some()));
    assert_eq!(shown(&app), (next.clone(), Some(vec!["Answered the review.".into()])));
    // The selection stays until g.
    assert_eq!(selected(&app), "ENG-52");
    app.on_key(key(KeyCode::Left));
    assert_eq!(shown(&app).0, "ENG-52");
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Char('g')));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(selected(&app), next);
}

#[test]
fn escape_keeps_the_selection() {
    let (mut app, _) = at_eng_52();
    open(&mut app);
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(selected(&app), "ENG-52");
}

#[test]
fn enter_opens_one_turn_and_escape_closes_it_then_the_list() {
    let (mut app, to_session) = at_eng_52();
    turn("ENG-52", "First.\n\nMore about it.");
    turn("ENG-52", "Second.");
    open(&mut app);
    draw(&mut app, 100, 30);
    assert_eq!(place(&app), (0, false, 0));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(place(&app), (1, true, 0));
    // Choosing another turn closes the open one.
    app.on_key(key(KeyCode::Up));
    assert_eq!(place(&app), (0, false, 0));
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(place(&app), (0, false, 0), "Enter closes it again");
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Esc));
    assert_eq!(place(&app), (0, false, 0), "Esc closes the turn, not the list");
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(sent(&to_session), "");
}

#[test]
fn the_arrows_and_page_keys_choose_a_turn_and_keep_it_in_view() {
    let (mut app, _) = at_eng_52();
    for n in 0..30 {
        turn("ENG-52", &format!("Turn {n}."));
    }
    open(&mut app);
    draw(&mut app, 80, 24);
    let rows = match &app.mode {
        Mode::Turns(t) => t.rows,
        _ => panic!("closed"),
    };
    assert!(rows < 30, "the list scrolls");
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(place(&app).0, 1);
    app.on_key(key(KeyCode::PageDown));
    assert_eq!(place(&app).0, rows);
    draw(&mut app, 80, 24);
    assert_eq!(place(&app).2, 1, "the turn chosen shows at the bottom");
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(place(&app).0, rows - 1);
    app.on_key(key(KeyCode::End));
    draw(&mut app, 80, 24);
    assert_eq!(place(&app), (29, false, 30 - rows));
    mouse(&mut app, MouseEventKind::ScrollUp, 40, 12);
    assert_eq!(place(&app).0, 28);
    app.on_key(key(KeyCode::Home));
    draw(&mut app, 80, 24);
    assert_eq!(place(&app), (0, false, 0));
}

#[test]
fn an_open_turn_longer_than_the_view_scrolls_before_moving_on() {
    let (mut app, _) = at_eng_52();
    turn("ENG-52", "Older.");
    let long: Vec<String> = (0..40).map(|n| format!("- step {n}")).collect();
    turn("ENG-52", &long.join("\n"));
    open(&mut app);
    draw(&mut app, 80, 24);
    app.on_key(key(KeyCode::Enter));
    draw(&mut app, 80, 24);
    let (rows, open_rows) = match &app.mode {
        Mode::Turns(t) => (t.rows, t.open_rows),
        _ => panic!("closed"),
    };
    assert_eq!(open_rows, 41, "its 40 items and a blank row");
    assert_eq!(place(&app), (0, true, 0), "its row stays at the top");
    // Through the message a row at a time, a page at a time, and the wheel, never past its end.
    app.on_key(key(KeyCode::Down));
    assert_eq!(place(&app), (0, true, 1));
    app.on_key(key(KeyCode::PageDown));
    assert_eq!(place(&app), (0, true, rows));
    mouse(&mut app, MouseEventKind::ScrollDown, 40, 12);
    assert_eq!(place(&app), (0, true, rows + 3));
    app.on_key(key(KeyCode::End));
    app.on_key(key(KeyCode::Home));
    app.on_key(key(KeyCode::Enter));
    draw(&mut app, 80, 24);
    for _ in 0..100 {
        mouse(&mut app, MouseEventKind::ScrollDown, 40, 12);
    }
    let last = 42 - rows;
    assert_eq!(place(&app), (0, true, last), "the wheel stops at its end");
    app.on_key(key(KeyCode::PageUp));
    assert_eq!(place(&app), (0, true, last - (rows - 1)));
    app.on_key(key(KeyCode::PageDown));
    // Its end in view, Down goes on to the next turn, which closes it.
    app.on_key(key(KeyCode::Down));
    assert_eq!((place(&app).0, place(&app).1), (1, false));
    draw(&mut app, 80, 24);
    assert_eq!(place(&app), (1, false, 0));
}

#[test]
fn a_turn_that_ends_while_the_overlay_is_open_shows() {
    let (mut app, _) = at_eng_52();
    turn("ENG-52", "First.");
    open(&mut app);
    // A filesystem's times can be a second apart at best: make the change one it sees.
    let Mode::Turns(t) = &mut app.mode else { panic!() };
    t.stamp = None;
    // The turn open stays selected and open as the new one comes in above it.
    app.on_key(key(KeyCode::Enter));
    turn("ENG-52", "Second.");
    app.poll_turns();
    settle(&mut app);
    assert_eq!(shown(&app).1, Some(vec!["Second.".into(), "First.".into()]));
    assert_eq!((place(&app).0, place(&app).1), (1, true));
}

#[test]
fn a_change_to_the_log_is_seen_by_its_time() {
    let (mut app, _) = at_eng_52();
    turn("ENG-52", "First.");
    open(&mut app);
    // Nothing changed: the poll starts no read.
    app.poll_turns();
    assert!(app.rx.recv_timeout(std::time::Duration::from_millis(300)).is_err(), "read again");
    turn("ENG-52", "Second.");
    // A filesystem's times can be a second apart: make the change one it sees, a minute on.
    let file = std::fs::File::options().append(true).open(log("ENG-52")).unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60)).unwrap();
    app.poll_turns();
    settle_until(&mut app, |a| shown(a).1.is_some_and(|t| t.len() == 2));
    assert_eq!(shown(&app).1, Some(vec!["Second.".into(), "First.".into()]));
}

#[test]
fn renaming_an_agent_moves_its_log() {
    let mut app = acme_app();
    turn("acme-lead", "Planned the batch.");
    app.select(index(&app, "acme-lead"));
    prefix(&mut app, KeyCode::Char('e'));
    app.on_key(ctrl('u'));
    type_text(&mut app, "planner");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "acme-planner");
    assert!(!log("acme-lead").exists());
    open(&mut app);
    assert_eq!(shown(&app), ("acme-planner".into(), Some(vec!["Planned the batch.".into()])));
}

#[test]
fn closing_a_ticket_removes_its_log() {
    let (mut app, _) = at_eng_52();
    turn("ENG-52", "Done.");
    let pending = events_log::pending_path(&store::status_file("/src/acme".as_ref(), "ENG-52"));
    events_log::note_files(&pending, &["a.rs".into()]).unwrap();
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Enter));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    assert!(!log("ENG-52").exists());
    assert!(!pending.exists());
}

#[test]
fn a_start_of_taskman_keeps_the_log() {
    let mut app = acme_app();
    turn("ENG-52", "Done.");
    let mut tickets: Vec<_> = app.tickets.iter().map(|tv| tv.t.clone()).collect();
    crate::app::poll::take_leftover_status(
        &mut tickets,
        |t| store::status_file(&t.repo, &t.key),
        store::legacy_status_file,
        |_| false,
    );
    open(&mut app);
    assert_eq!(shown(&app).1, Some(vec!["Done.".into()]));
}
