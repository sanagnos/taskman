//! Toasts: how long each level stays, what an error does to later ones, and the log.

use super::*;
use crate::app::toasts::{LOG_CAP, Level};
use std::time::{Duration, Instant};

/// Makes the toast showing `secs` seconds old, and runs the clock the tick runs.
fn age_toast(app: &mut App, secs: u64) {
    let at = Instant::now().checked_sub(Duration::from_secs(secs)).unwrap();
    app.toast.as_mut().unwrap().at = at;
    app.expire_toast();
}

#[test]
fn an_info_toast_clears_after_four_seconds() {
    let mut app = acme_app();
    app.toast("Marked as read");
    age_toast(&mut app, 3);
    assert_eq!(toast(&app), Some("Marked as read"));
    age_toast(&mut app, 5);
    assert_eq!(toast(&app), None);
}

#[test]
fn an_error_stays_past_five_seconds_and_clears_on_the_next_key() {
    let mut app = acme_app();
    app.error("Couldn't write the history");
    age_toast(&mut app, 5);
    assert_eq!(toast(&app), Some("Couldn't write the history"));
    age_toast(&mut app, 600);
    assert_eq!(toast(&app), Some("Couldn't write the history"));
    app.on_key(key(KeyCode::Esc));
    assert_eq!(toast(&app), None);
    // The key that raises an error doesn't clear it.
    app.on_key(ctrl('a'));
    app.on_key(key(KeyCode::Char('%')));
    assert_eq!(toast(&app).map(|t| t.contains("isn't a command")), Some(true));
}

#[test]
fn a_warning_clears_like_an_info_toast() {
    let mut app = acme_app();
    app.warn("No PR found for this ticket yet");
    age_toast(&mut app, 5);
    assert_eq!(toast(&app), None);
}

#[test]
fn a_later_toast_does_not_hide_an_error_but_is_logged() {
    let mut app = acme_app();
    app.error("Couldn't create ENG-70's worktree");
    app.toast("Marked as read");
    app.warn("No links in this session");
    assert_eq!(toast(&app), Some("Couldn't create ENG-70's worktree"));
    // Another error does replace it.
    app.error("Couldn't write the history");
    assert_eq!(toast(&app), Some("Couldn't write the history"));
    let log: Vec<_> = app.toast_log.iter().map(|l| (l.level, l.msg.as_str())).collect();
    assert_eq!(
        log,
        [
            (Level::Error, "Couldn't write the history"),
            (Level::Warn, "No links in this session"),
            (Level::Info, "Marked as read"),
            (Level::Error, "Couldn't create ENG-70's worktree"),
        ]
    );
}

#[test]
fn the_log_keeps_the_last_200_and_one_entry_for_a_repeat() {
    let mut app = acme_app();
    for n in 0..LOG_CAP + 25 {
        app.toast(format!("Toast {n}"));
    }
    assert_eq!(app.toast_log.len(), LOG_CAP);
    assert_eq!(app.toast_log.front().unwrap().msg, format!("Toast {}", LOG_CAP + 24));
    assert_eq!(app.toast_log.back().unwrap().msg, "Toast 25");
    app.toast("Toast again");
    app.toast("Toast again");
    assert_eq!(app.toast_log.len(), LOG_CAP);
    assert_eq!(app.toast_log[1].msg, format!("Toast {}", LOG_CAP + 24));
}

#[test]
fn prefix_tilde_lists_the_notices_and_any_other_key_closes_it() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('~'));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(toast(&app), Some("Nothing has been shown yet"));
    prefix(&mut app, KeyCode::Char('~'));
    assert!(matches!(app.mode, Mode::Log(_)), "the first toast is in the log");
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("Nothing has been shown yet"), "{screen}");
    app.on_key(key(KeyCode::Char('x')));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn prefix_tilde_with_shift_held_lists_the_notices_too() {
    // A kitty keyboard protocol terminal may report ~ with SHIFT; the loop normalizes it.
    let mut app = acme_app();
    app.toast("Marked as read");
    app.on_key(ctrl('a'));
    app.on_key(crate::keys::normalize(crossterm::event::KeyEvent::new(
        KeyCode::Char('~'),
        KeyModifiers::SHIFT,
    )));
    assert!(matches!(app.mode, Mode::Log(_)), "{:?}", toast(&app));
}

#[test]
fn prefix_shift_l_no_longer_lists_the_notices() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('L'));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(toast(&app).is_some_and(|t| t.contains("L isn't a command")), "{:?}", toast(&app));
}

#[test]
fn the_log_scrolls_when_it_does_not_fit() {
    let mut app = acme_app();
    for n in 0..60 {
        app.toast(format!("Toast {n}"));
    }
    prefix(&mut app, KeyCode::Char('~'));
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("Toast 59") && !screen.contains("Toast 0 "), "{screen}");
    assert!(screen.contains("of 60"), "{screen}");
    app.on_key(key(KeyCode::End));
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("Toast 0") && screen.contains("47-60 of 60"), "{screen}");
    assert!(matches!(app.mode, Mode::Log(_)));
}

#[test]
fn a_failed_background_job_toasts_an_error() {
    let mut app = acme_app();
    app.tx.send(Msg::Toast(Level::Error, "Couldn't open a tmux window: no server".into())).unwrap();
    app.drain_messages();
    assert_eq!(app.toast.as_ref().unwrap().level, Level::Error);
}
