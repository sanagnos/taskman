use super::*;
use crate::app::test_support::notifications;
use crate::notify;

// ---- a session that starts waiting on you (`App::started_waiting`) ------------------------------

/// Makes `key` start waiting on a permission prompt, as the status poll does.
fn starts_waiting(app: &mut App, key: &str) {
    let i = index(app, key);
    app.tickets[i].status.state = AgentState::Needs;
    app.tickets[i].status.permission = true;
    app.started_waiting(i);
}

#[test]
fn a_session_that_starts_waiting_notifies_you_while_you_are_away() {
    let mut app = acme_app();
    let shown = notifications(&mut app);
    // Focused: the sidebar shows it, and the bell rings for one out of sight.
    app.focused = Some(true);
    starts_waiting(&mut app, "ENG-53");
    assert!(app.bell && shown.lock().unwrap().is_empty());
    // Away: the session on screen too, which no bell is for.
    app.bell = false;
    app.focused = Some(false);
    starts_waiting(&mut app, "ENG-52");
    settle(&mut app);
    assert!(!app.bell);
    assert_eq!(*shown.lock().unwrap(), ["ENG-52 needs you: a permission prompt"]);
    // No desktop command showed it: it goes to the terminal on the next frame.
    let escape = app.notify_out.pop().unwrap();
    assert!(escape.contains("\x1b]9;taskman · ENG-52 needs you: a permission prompt\x07"));
    // An agent goes by its name.
    starts_waiting(&mut app, "acme-lead");
    settle(&mut app);
    assert_eq!(shown.lock().unwrap()[1], "lead needs you: a permission prompt");
}

#[test]
fn without_focus_reports_only_a_session_out_of_sight_notifies() {
    let mut app = acme_app();
    let shown = notifications(&mut app);
    assert_eq!(app.focused, None);
    starts_waiting(&mut app, "ENG-52");
    assert!(shown.lock().unwrap().is_empty());
    starts_waiting(&mut app, "ENG-53");
    settle(&mut app);
    assert_eq!(*shown.lock().unwrap(), ["ENG-53 needs you: a permission prompt"]);
}

#[test]
fn notifications_turned_off_send_none() {
    let mut app = acme_app();
    let shown = notifications(&mut app);
    app.config.notify = notify::Setting::Off;
    app.focused = Some(false);
    starts_waiting(&mut app, "ENG-53");
    // The bell still rings.
    assert!(app.bell);
    assert!(shown.lock().unwrap().is_empty() && app.notify_out.is_empty());
}
