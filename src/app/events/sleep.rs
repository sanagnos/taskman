use super::*;
use crate::app::sleep::Scan;
use std::collections::HashSet;
use std::time::{Duration, Instant};

// ---- sleeping sessions ---------------------------------------------------------------------

fn minutes(m: u64) -> Duration {
    Duration::from_secs(m * 60)
}

/// ENG-53 with a session that runs nothing, in `state`, while ENG-52 is selected.
fn running(state: AgentState) -> App {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    attach(&mut app, "ENG-53", SCREEN);
    let i = index(&app, "ENG-53");
    let tv = &mut app.tickets[i];
    tv.status.state = state;
    tv.launch = Some("launch-1".into());
    app
}

fn asleep(app: &App, key: &str) -> bool {
    let tv = &app.tickets[index(app, key)];
    assert_eq!(tv.asleep, tv.session.is_none() && tv.asleep, "asleep with a session");
    tv.asleep
}

#[test]
fn an_idle_unselected_session_sleeps_after_the_time_and_selecting_wakes_it() {
    let mut app = running(AgentState::Done);
    let i = index(&app, "ENG-53");
    app.tickets[i].status.seen = true;
    let start = Instant::now();
    // Not yet: the hour counts from when it was last selected.
    app.sleep_tick(start + minutes(59));
    assert!(app.tickets[i].session.is_some() && !asleep(&app, "ENG-53"));
    app.sleep_tick(start + minutes(61));
    assert!(asleep(&app, "ENG-53"));
    let tv = &app.tickets[i];
    // The state stays as it was, and `taskman status` is told.
    assert_eq!(tv.status.state, AgentState::Done);
    assert!(super::super::ticket::asleep_marker(&tv.t).exists());
    // The claim goes once the CLI is gone, so `taskman resume` can take it meanwhile.
    reaped(&mut app);
    app.poll_status();
    assert!(app.tickets[i].hold.is_none());
    assert!(draw(&mut app, 160, 40).contains("☾"), "the sidebar shows it");

    // Selecting it starts the session again. (It's found held by `taskman resume` here, so no
    // CLI starts in the test.)
    let outside = held_outside("ENG-53");
    app.select(i);
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(!tv.asleep && tv.away == Some(Away::Outside), "no start was tried");
    assert!(!super::super::ticket::asleep_marker(&tv.t).exists());
    drop(outside);
}

#[test]
fn a_session_that_is_busy_or_waiting_on_you_stays_awake() {
    let later = Instant::now() + minutes(600);
    for state in [AgentState::Working, AgentState::Needs, AgentState::Exited] {
        let mut app = running(state);
        app.sleep_tick(later);
        assert!(app.tickets[index(&app, "ENG-53")].session.is_some(), "{state:?} slept");
    }
    // Not the one in front of you, even when it's been quiet since the tick before.
    let mut app = running(AgentState::Idle);
    app.select(index(&app, "ENG-53"));
    app.sleep_tick(later);
    assert!(app.tickets[app.sel].session.is_some());
    // Nor one handed off or being reaped.
    let mut app = running(AgentState::Idle);
    let i = index(&app, "ENG-53");
    app.tickets[i].away = Some(Away::HandedOff);
    app.sleep_tick(later);
    assert!(app.tickets[index(&app, "ENG-53")].session.is_some());
    let (mut app, _release) = {
        let mut app = running(AgentState::Idle);
        let (_, release) = attach_slow(&mut app, "ENG-53");
        (app, release)
    };
    let i = index(&app, "ENG-53");
    app.tickets[i].stop_session();
    assert!(app.tickets[i].reaping());
    app.sleep_tick(later);
    assert!(!asleep(&app, "ENG-53"));
}

#[test]
fn agents_stay_awake() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    attach(&mut app, "acme-lead", SCREEN);
    let i = index(&app, "acme-lead");
    app.tickets[i].status.state = AgentState::Done;
    app.sleep_tick(Instant::now() + minutes(6000));
    assert!(app.tickets[i].session.is_some() && !app.tickets[i].asleep);
}

/// What runs under the CLI that its hooks don't report keeps it awake; the look at the process
/// tree comes back as a message.
#[test]
fn work_in_the_process_tree_keeps_a_session_awake() {
    let mut app = running(AgentState::Idle);
    let i = index(&app, "ENG-53");
    let id = (app.tickets[i].t.repo.clone(), "ENG-53".to_string());
    let ids = |pids: &[u32]| pids.iter().copied().collect::<HashSet<u32>>();
    app.tickets[i].watch.baseline = Some(ids(&[10, 11]));
    app.tickets[i].watch.selected = Instant::now().checked_sub(minutes(90)).unwrap();
    // A dev server started since the session settled.
    app.scanned(&id, "launch-1", Scan::Check, Ok(ids(&[10, 11, 12])));
    assert!(app.tickets[i].session.is_some() && app.tickets[i].watch.busy.is_some());
    // A look at an earlier start, or one that failed, decides nothing.
    app.scanned(&id, "launch-0", Scan::Check, Ok(ids(&[10, 11])));
    app.scanned(&id, "launch-1", Scan::Check, Err("ps: gone".into()));
    assert!(app.tickets[i].session.is_some());
    // Its own servers are not work, and it was asked again at the time of the answer: it
    // sleeps unless it was selected meanwhile.
    app.select(i);
    app.scanned(&id, "launch-1", Scan::Check, Ok(ids(&[10, 11])));
    assert!(app.tickets[i].session.is_some(), "selected meanwhile");
    app.select(index(&app, "ENG-52"));
    app.tickets[i].watch.selected = Instant::now().checked_sub(minutes(90)).unwrap();
    app.scanned(&id, "launch-1", Scan::Check, Ok(ids(&[11])));
    assert!(asleep(&app, "ENG-53"));
}

#[test]
fn what_runs_when_it_settles_is_its_own() {
    let mut app = running(AgentState::Idle);
    let i = index(&app, "ENG-53");
    let id = (app.tickets[i].t.repo.clone(), "ENG-53".to_string());
    app.scanned(&id, "launch-1", Scan::Baseline, Ok([7, 8].into()));
    assert_eq!(app.tickets[i].watch.baseline, Some([7, 8].into()));
}

/// Past the cap, the idle sessions selected longest ago sleep, whatever the time says.
#[test]
fn past_the_cap_the_longest_unselected_go_to_sleep() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.config.sleep_after = None;
    app.config.max_awake = Some(2);
    let now = Instant::now() + minutes(1);
    for (key, idle) in [("ENG-52", 0), ("ENG-53", 20), ("ENG-61", 50), ("ENG-70", 30)] {
        attach(&mut app, key, SCREEN);
        let i = index(&app, key);
        let tv = &mut app.tickets[i];
        tv.status.state = AgentState::Idle;
        tv.watch.selected = now.checked_sub(minutes(idle)).unwrap();
    }
    app.sleep_tick(now);
    let awake: Vec<_> = ["ENG-52", "ENG-53", "ENG-61", "ENG-70"]
        .into_iter()
        .filter(|k| app.tickets[index(&app, k)].session.is_some())
        .collect();
    assert_eq!(awake, ["ENG-52", "ENG-53"], "the selected and the most recent stay");
    // With neither setting there is nothing to do.
    let mut app = running(AgentState::Idle);
    app.config.sleep_after = None;
    app.sleep_tick(Instant::now() + minutes(6000));
    assert!(app.tickets[index(&app, "ENG-53")].session.is_some());
}

/// A project agent running starts its tickets' sessions so they can be messaged, but not
/// the ones put to sleep: waking them would undo what sleeping saved.
#[test]
fn an_agents_auto_start_leaves_sleeping_sessions_alone() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    attach(&mut app, "acme-lead", SCREEN);
    let (awake, sleeping) = (index(&app, "ENG-53"), index(&app, "ENG-70"));
    // ENG-53 would start (it's found held by `taskman resume` here, so no CLI starts).
    app.tickets[awake].status.state = AgentState::NotStarted;
    let outside = held_outside("ENG-53");
    let tv = &mut app.tickets[sleeping];
    (tv.status.state, tv.asleep) = (AgentState::NotStarted, true);
    for _ in 0..3 {
        app.ensure_selected_session();
    }
    assert_eq!(app.tickets[awake].away, Some(Away::Outside), "the control was not started");
    let tv = &app.tickets[sleeping];
    assert!(tv.session.is_none() && tv.asleep && tv.away.is_none(), "a sleeping ticket woke");
    drop(outside);
}

#[test]
fn settings_save_the_sleep_time_and_the_cap() {
    let mut app = acme_app();
    app.open_settings(super::super::SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(field(&app, "Sleep idle sessions after"), "1 hour");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Sleep idle sessions after"), "2 hours");
    app.on_key(key(KeyCode::Down));
    assert_eq!(field(&app, "Keep awake at most"), "no limit");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Keep awake at most"), "2 sessions");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Saved"));
    assert_eq!((app.config.sleep_after, app.config.max_awake), (Some(7200), Some(2)));
    let config = store::load_config().unwrap();
    assert_eq!((config.sleep_after, config.max_awake), (Some(7200), Some(2)));
    // Never is the last preset, two on from 2 hours.
    app.open_settings(super::super::SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Sleep idle sessions after"), "never");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.config.sleep_after, None);
    assert_eq!(store::load_config().unwrap().sleep_after, None);
    // Esc closes Settings; a value set by hand in the config file is kept.
    app.on_key(key(KeyCode::Esc));
    app.config.sleep_after = Some(5);
    app.open_settings(super::super::SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(field(&app, "Sleep idle sessions after"), "5 s");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.config.sleep_after, Some(5));
}
