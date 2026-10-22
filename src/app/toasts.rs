//! The toast in the footer, and the log of every toast shown (`prefix ~`).

use super::App;
use crate::util;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How many toasts the log keeps.
pub const LOG_CAP: usize = 200;

/// How long an info or warning toast stays; an error stays until the next key.
pub const SHOWN: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// What just happened, as the user asked.
    Info,
    /// Something didn't go as asked, but nothing is lost.
    Warn,
    /// Something failed, or work is at risk.
    Error,
}

pub struct Toast {
    pub msg: String,
    pub level: Level,
    pub at: Instant,
    /// How long a toast other than an error stays.
    pub hold: Duration,
}

pub struct Logged {
    pub level: Level,
    pub msg: String,
    /// Unix seconds.
    pub ts: i64,
}

impl App {
    pub fn toast(&mut self, msg: impl Into<String>) {
        self.show(Level::Info, msg.into(), SHOWN);
    }

    pub fn warn(&mut self, msg: impl Into<String>) {
        self.show(Level::Warn, msg.into(), SHOWN);
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        self.show(Level::Error, msg.into(), SHOWN);
    }

    /// A toast that stays longer than `SHOWN`.
    pub fn toast_for(&mut self, msg: impl Into<String>, hold: Duration) {
        self.show(Level::Info, msg.into(), hold);
    }

    pub fn toast_at(&mut self, level: Level, msg: impl Into<String>) {
        self.show(level, msg.into(), SHOWN);
    }

    fn show(&mut self, level: Level, msg: String, hold: Duration) {
        // The same message twice in a row is one entry (a save error is toasted again until a
        // save works).
        if self.toast_log.front().is_none_or(|l| l.msg != msg || l.level != level) {
            self.toast_log.push_front(Logged { level, msg: msg.clone(), ts: util::now() });
            self.toast_log.truncate(LOG_CAP);
        }
        // An error stays until a key; what comes meanwhile is in the log.
        if self.toast.as_ref().is_some_and(|t| t.level == Level::Error && level < Level::Error) {
            return;
        }
        self.toast = Some(Toast { msg, level, at: Instant::now(), hold });
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// A key dismisses an error toast, before the key does anything that may toast again.
    pub(super) fn dismiss_error(&mut self) {
        if self.toast.as_ref().is_some_and(|t| t.level == Level::Error) {
            self.toast = None;
        }
    }

    /// Clears a toast that has been shown long enough; an error stays for a key.
    pub(super) fn expire_toast(&mut self) {
        if self.toast.as_ref().is_some_and(|t| t.level != Level::Error && t.at.elapsed() > t.hold) {
            self.toast = None;
            self.dirty.store(true, Ordering::Relaxed);
        }
        if self.toast.is_none() {
            self.show_save_error();
        }
    }

    pub(super) fn open_log(&mut self) {
        if self.toast_log.is_empty() {
            self.toast("Nothing has been shown yet");
        } else {
            self.mode = super::Mode::Log(super::Scroll::default());
        }
    }
}

/// "now", "12s", "5m": how long ago, for the log.
pub fn age(ts: i64) -> String {
    let s = (util::now() - ts).max(0);
    match s {
        0..5 => "now".into(),
        5..60 => format!("{s}s"),
        _ => util::ago(ts),
    }
}

pub type Log = VecDeque<Logged>;
