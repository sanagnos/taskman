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

// ---- an approval Codex's reviewer may answer (`Status::review`) ---------------------------------

/// Writes the record Codex's PermissionRequest hook writes for `key`, and polls it.
fn asks_approval(app: &mut App, key: &str) {
    let mut rec = crate::hook::status_record(
        crate::status_file::HookState::Needs,
        Some("sid-1"),
        None,
        "Run the tests",
        1,
    );
    rec.reason = Some(crate::status_file::Reason::Permission);
    let path = store::status_file("/src/acme".as_ref(), key);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, rec.to_string()).unwrap();
    let i = index(app, key);
    app.tickets[i].status.mtime = None;
    app.poll_status();
}

/// Draws `screen` over what the session showed.
fn shows(app: &App, key: &str, screen: &str) {
    let s = app.tickets[index(app, key)].session.as_ref().unwrap();
    s.output(format!("\x1b[2J\x1b[H{}", screen.replace('\n', "\r\n")).as_bytes());
}

const REVIEWING: &str =
    "› Run the tests\n\n• Reviewing approval request (2s • esc to interrupt)\n  └ cargo test\n";
const CODEX_PROMPT: &str = "  Would you like to run the following command?\n\n  $ cargo test\n\n› 1. Yes, proceed (y)\n  2. No, and tell Codex what to do differently (esc)\n\n  Press enter to confirm or esc to cancel\n";

#[test]
fn an_approval_codexs_reviewer_decides_neither_rings_nor_notifies() {
    let mut app = acme_app();
    let shown = notifications(&mut app);
    app.focused = Some(false);
    lay_out(&mut app, 160, 40);
    attach(&mut app, "ENG-61", REVIEWING);
    asks_approval(&mut app, "ENG-61");
    let i = index(&app, "ENG-61");
    // The hook has fired, and Codex's reviewer has the request: working, with no bell.
    for _ in 0..3 {
        app.poll_status();
        assert_eq!(app.tickets[i].status.state, AgentState::Working);
    }
    // The reviewer approved it: the line goes and the command runs, with no hook till it ends.
    shows(&app, "ENG-61", "• Working (5s • esc to interrupt)\n");
    app.poll_status();
    assert_eq!(app.tickets[i].status.state, AgentState::Working);
    assert!(!app.bell && shown.lock().unwrap().is_empty() && app.notify_out.is_empty());
}

#[test]
fn an_approval_codex_asks_you_rings_and_notifies() {
    let mut app = acme_app();
    let shown = notifications(&mut app);
    app.focused = Some(false);
    lay_out(&mut app, 160, 40);
    // No reviewer: Codex's prompt shows as the hook fires.
    attach(&mut app, "ENG-61", CODEX_PROMPT);
    asks_approval(&mut app, "ENG-61");
    let i = index(&app, "ENG-61");
    assert_eq!(app.tickets[i].status.state, AgentState::Needs);
    assert!(app.bell && !app.tickets[i].status.seen);
    settle(&mut app);
    assert_eq!(*shown.lock().unwrap(), ["ENG-61 needs you: a permission prompt"]);
    // The reviewer handed it over: its line goes, and Codex's prompt shows.
    app.bell = false;
    shows(&app, "ENG-61", REVIEWING);
    asks_approval(&mut app, "ENG-61");
    app.poll_status();
    assert_eq!(app.tickets[i].status.state, AgentState::Working);
    shows(&app, "ENG-61", CODEX_PROMPT);
    app.poll_status();
    assert_eq!(app.tickets[i].status.state, AgentState::Needs);
    assert!(app.bell);
    // Claude has no reviewer: its prompt needs you as the hook fires, whatever the screen shows.
    app.bell = false;
    attach(&mut app, "ENG-53", REVIEWING);
    let j = index(&app, "ENG-53");
    app.tickets[j].status.state = AgentState::Working;
    asks_approval(&mut app, "ENG-53");
    assert_eq!(app.tickets[j].status.state, AgentState::Needs);
    assert!(app.bell);
}
