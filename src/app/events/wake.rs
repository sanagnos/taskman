use super::*;
use crate::util;

// ---- `taskman wake` through the inbox ------------------------------------------------------

/// Leaves a `taskman wake` request for `key` whose command waits `wait` seconds more.
fn wake(id: &str, key: &str, wait: i64) {
    request(
        id,
        &serde_json::json!({
            "repo": "/src/acme", "from": "✦ lead", "reply_by": util::now() + wait,
            "items": [{ "kind": "wake", "key": key }],
        }),
    );
}

/// Puts `key` to sleep as taskman does, as far as a wake goes: no session, asleep.
fn asleep(app: &mut App, key: &str) {
    let i = index(app, key);
    let tv = &mut app.tickets[i];
    (tv.session, tv.asleep, tv.status.state) = (None, true, AgentState::Done);
}

fn replied(id: &str) -> bool {
    store::state_dir().join("inbox").join(format!("{id}.reply.json")).exists()
}

/// As `start_session` leaves a ticket it started, as start `launch`, without a CLI.
fn started(app: &mut App, key: &str, launch: &str) {
    attach(app, key, "");
    let i = index(app, key);
    let tv = &mut app.tickets[i];
    (tv.start, tv.launch) = (false, Some(launch.into()));
    tv.status.started();
}

/// A record of `launch`'s SessionStart in `key`'s status file, which the next poll reads.
fn session_start(app: &mut App, key: &str, launch: &str) {
    let path = store::status_file("/src/acme".as_ref(), key);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut r = crate::hook::status_record(
        crate::status_file::HookState::Idle,
        Some("sid-53"),
        None,
        "",
        1,
    );
    r.launch = Some(launch.into());
    std::fs::write(&path, r.to_string()).unwrap();
    // Two writes within the file system's clock tick share a time, which the poll takes for no
    // change.
    let i = index(app, key);
    app.tickets[i].status.mtime = None;
    app.poll_status();
}

/// A sleeping ticket is started without being selected, and the command is answered once its
/// CLI reports its start. Another wake meanwhile shares the start rather than waiting too.
#[test]
fn a_wake_starts_a_sleeping_ticket_and_answers_once_it_is_up() {
    let mut app = acme_app();
    let sel = app.sel;
    let i = index(&app, "ENG-53");
    asleep(&mut app, "ENG-53");
    app.tickets[i].launch = Some("launch-1".into());
    wake("0001", "ENG-53", 20);
    app.take_inbox();
    let tv = &app.tickets[i];
    assert!(tv.start && tv.woken, "started on the next tick, with the grace");
    assert_eq!(app.sel, sel, "the user's view stays");
    assert!(!replied("0001"), "not until it's up");

    // A second wake for it is answered at once; only one waits.
    wake("0002", "ENG-53", 20);
    app.take_inbox();
    let [line] = &reply("0002")[..] else { panic!() };
    assert!(line.ok && line.text.contains("already starting"), "{line:?}");
    assert_eq!(app.waking.len(), 1);

    // Started, but its CLI hasn't reported: still waiting. A record of the start before isn't
    // this one's.
    started(&mut app, "ENG-53", "launch-2");
    session_start(&mut app, "ENG-53", "launch-1");
    app.take_inbox();
    assert!(!replied("0001"));
    session_start(&mut app, "ENG-53", "launch-2");
    app.take_inbox();
    assert_eq!(reply("0001"), [add::Line::ok("ENG-53 is awake; message it as ENG-53")]);
    assert!(app.waking.is_empty());
}

/// The start a wake asks for gets the keep-awake grace though it resumes its conversation, and
/// only that start.
#[test]
fn a_woken_start_gets_the_grace() {
    let mut app = acme_app();
    let i = index(&app, "ENG-53");
    app.tickets[i].t.session_id = Some("sid-53".into());
    crate::store::Cli::Claude.leave_transcript("sid-53");
    let now = std::time::Instant::now();
    let (cmd, watch) = app.starting(i, "ENG-53", "launch-1", now, true);
    assert!(cmd.get_argv().iter().any(|a| a == "--resume") && watch.fresh);
    let (_, watch) = app.starting(i, "ENG-53", "launch-1", now, false);
    assert!(!watch.fresh, "resumed, and not woken");
}

#[test]
fn a_wake_is_answered_at_once_when_there_is_nothing_to_start() {
    let mut app = acme_app();
    attach(&mut app, "ENG-52", "");
    for (id, key, ok, says) in [
        ("0001", "ENG-52", true, "ENG-52 is awake; message it as ENG-52"),
        ("0002", "ENG-61", false, "can't be messaged"),
        ("0003", "acme-lead", false, "agents don't sleep"),
        ("0004", "ENG-99", false, "ENG-99 isn't open in taskman"),
    ] {
        wake(id, key, 20);
        app.take_inbox();
        let [line] = &reply(id)[..] else { panic!("{id}") };
        assert!(line.ok == ok && line.text.contains(says), "{line:?}");
    }
    assert!(app.waking.is_empty());
    assert!(app.tickets.iter().all(|tv| !tv.start && !tv.woken));
}

/// Taken after the command stopped waiting (taskman quit, or hung, meanwhile): nobody waits for
/// that session, so nothing starts.
#[test]
fn a_late_wake_is_dropped() {
    let mut app = acme_app();
    asleep(&mut app, "ENG-53");
    wake("0001", "ENG-53", -1);
    app.take_inbox();
    assert!(!app.tickets[index(&app, "ENG-53")].start);
    assert!(app.waking.is_empty() && !replied("0001"));
}

/// Every wake is answered: still starting just before the command stops waiting, couldn't start,
/// or taskman quitting; none is left waiting.
#[test]
fn a_wake_that_does_not_come_up_is_answered_all_the_same() {
    // Slow: its command is about to stop waiting.
    let mut app = acme_app();
    asleep(&mut app, "ENG-53");
    wake("0001", "ENG-53", 1);
    app.take_inbox();
    let [line] = &reply("0001")[..] else { panic!() };
    assert!(line.ok && line.text.contains("ENG-53 is starting"), "{line:?}");
    assert!(app.waking.is_empty());

    // Tried, and refused: taken in another terminal meanwhile, or failed (the pane says why).
    let mut app = acme_app();
    let i = index(&app, "ENG-53");
    asleep(&mut app, "ENG-53");
    asleep(&mut app, "ENG-52");
    wake("0002", "ENG-53", 20);
    app.take_inbox();
    (app.tickets[i].start, app.tickets[i].away) = (false, Some(Away::Outside));
    app.take_inbox();
    assert!(reply("0002")[0].text.contains("another terminal"));
    wake("0003", "ENG-52", 20);
    app.take_inbox();
    let j = index(&app, "ENG-52");
    (app.tickets[j].start, app.tickets[j].status.state) = (false, AgentState::Exited);
    app.take_inbox();
    assert!(reply("0003")[0].text.contains("couldn't start"));

    // Quitting answers what still waits.
    let mut app = acme_app();
    asleep(&mut app, "ENG-53");
    wake("0004", "ENG-53", 20);
    app.take_inbox();
    assert_eq!(app.waking.len(), 1);
    app.stop_sessions();
    assert_eq!(reply("0004"), [add::Line::err("taskman quit before ENG-53 was up")]);
    assert!(app.waking.is_empty());
}
