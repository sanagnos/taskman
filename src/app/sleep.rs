//! Sleeping sessions: a session nobody has looked at for a while is stopped to free the memory of
//! its CLI and its MCP servers, and starts again, on the same conversation, when it's selected.
//! The decisions are pure functions (`due`, `Facts`) so they're tested without a session; the rest
//! looks at a session's process tree off the UI thread, since its hooks don't report background
//! work, and stops it through `TicketView::sleep`. See "Sleeping sessions" in docs/design.md.

use super::sync::Msg;
use super::{AgentState, App, Id};
use crate::proctree;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How long after it starts a session's process tree is taken as its own (the CLI's MCP servers
/// and helpers), unless the setting is shorter than four times this (`settle`).
const SETTLE: Duration = Duration::from_secs(30);

/// How long before looking again at a session that had work running, unless the setting is shorter
/// than twice this (`recheck`).
const RECHECK: Duration = Duration::from_secs(60);

/// What the Settings presets for sleeping offer, in seconds; None never.
const AFTER: [Option<u64>; 6] = [Some(900), Some(1800), Some(3600), Some(7200), Some(14_400), None];

/// What the presets for the cap offer; None for no limit.
const CAP: [Option<usize>; 10] =
    [None, Some(2), Some(3), Some(4), Some(5), Some(6), Some(8), Some(10), Some(15), Some(20)];

/// What taskman keeps about a running session to tell whether it can sleep.
pub(super) struct Watch {
    /// When the session started, or the user last had it selected: sleeping is counted from the
    /// later.
    pub(super) selected: Instant,
    pub(super) started: Instant,
    /// The CLI's descendants once it had settled: its own servers, not work in progress.
    pub(super) baseline: Option<HashSet<u32>>,
    /// A look at the process tree is running.
    pub(super) scanning: bool,
    /// When the process tree last showed work running.
    pub(super) busy: Option<Instant>,
}

impl Watch {
    pub(super) fn new(now: Instant) -> Self {
        Self { selected: now, started: now, baseline: None, scanning: false, busy: None }
    }
}

/// Why a process tree is looked at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Scan {
    /// The session has settled: what runs under it now is its own.
    Baseline,
    /// The session is due to sleep: anything else under it is work in progress.
    Check,
}

/// What decides whether one session may sleep, apart from the process tree.
#[derive(Clone, Copy, Debug)]
pub(super) struct Facts {
    pub state: AgentState,
    /// It has a CLI that hasn't ended.
    pub running: bool,
    /// Agents stay awake: see "Sleeping sessions" in docs/design.md.
    pub agent: bool,
    pub selected: bool,
    /// In another terminal, or being handed there.
    pub away: bool,
    /// Its last CLI is still being reaped.
    pub reaping: bool,
    /// Its process tree has a baseline, so what's new under it can be told.
    pub settled: bool,
    /// Since it started or was last selected.
    pub idle: Duration,
}

impl Facts {
    /// Whether the session may sleep as far as it itself goes, however long it's been idle: it
    /// waits on you or on nothing, and nothing else depends on it. Never one that's working,
    /// needs you, is handed off or still being reaped.
    fn restful(&self) -> bool {
        self.running
            && self.settled
            && !self.agent
            && !self.selected
            && !self.away
            && !self.reaping
            && matches!(self.state, AgentState::Idle | AgentState::Done)
    }
}

/// The sessions (by place in `facts`) to put to sleep: the restful ones unselected for `after`,
/// and, past `cap` sessions running, the longest-unselected restful ones until it's met.
pub(super) fn due(facts: &[Facts], after: Option<Duration>, cap: Option<usize>) -> Vec<usize> {
    let mut out: Vec<usize> = (0..facts.len())
        .filter(|&i| facts[i].restful() && after.is_some_and(|a| facts[i].idle >= a))
        .collect();
    if let Some(cap) = cap {
        let excess = facts.iter().filter(|f| f.running).count().saturating_sub(cap);
        let mut oldest: Vec<usize> = (0..facts.len()).filter(|&i| facts[i].restful()).collect();
        // Longest idle first; the earlier in the list among equals, so the order is stable.
        oldest.sort_by_key(|&i| (std::cmp::Reverse(facts[i].idle), i));
        out.extend(oldest.into_iter().take(excess));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// How long after it starts a session's process tree is taken as its own.
fn settle(after: Option<Duration>) -> Duration {
    after.map_or(SETTLE, |a| SETTLE.min(a / 4))
}

/// How long before a session that had work running is looked at again.
fn recheck(after: Option<Duration>) -> Duration {
    after.map_or(RECHECK, |a| RECHECK.min(a / 2))
}

/// Whether anything runs under a session that didn't when it settled: its descendants now, minus
/// its baseline. A session without a baseline is not known to be free of work.
fn working(baseline: Option<&HashSet<u32>>, now: &HashSet<u32>) -> bool {
    baseline.is_none_or(|b| !now.is_subset(b))
}

/// A time setting's words: "30 min", "2 hours", "never".
pub(super) fn after_label(secs: Option<u64>) -> String {
    match secs {
        None => "never".into(),
        Some(s) if s % 3600 == 0 => match s / 3600 {
            1 => "1 hour".into(),
            h => format!("{h} hours"),
        },
        Some(s) if s % 60 == 0 => format!("{} min", s / 60),
        Some(s) => format!("{s} s"),
    }
}

/// The values the sleep setting's field cycles through: the presets, and `current` when it's none
/// of them (set by hand in the config file).
pub(super) fn after_choices(current: Option<u64>) -> Vec<Option<u64>> {
    let mut v = AFTER.to_vec();
    if !v.contains(&current) {
        v.push(current);
    }
    v
}

pub(super) fn cap_label(cap: Option<usize>) -> String {
    cap.map_or_else(|| "no limit".into(), |n| format!("{n} sessions"))
}

pub(super) fn cap_choices(current: Option<usize>) -> Vec<Option<usize>> {
    let mut v = CAP.to_vec();
    if !v.contains(&current) {
        v.push(current);
    }
    v
}

impl App {
    /// Called each tick: looks at which sessions have settled and which are due to sleep, and has
    /// their process trees looked at (`scan`). `now` is when the tick is.
    pub(super) fn sleep_tick(&mut self, now: Instant) {
        if let Some(tv) = self.tickets.get_mut(self.sel) {
            tv.watch.selected = now;
        }
        let after = self.config.sleep_after.map(Duration::from_secs);
        let cap = self.config.max_awake;
        if after.is_none() && cap.is_none() {
            return;
        }
        let settle = settle(after);
        for i in 0..self.tickets.len() {
            let tv = &self.tickets[i];
            let Some(pid) = tv.session.as_ref().filter(|s| !s.exited()).map(|s| s.pid()) else {
                continue;
            };
            if tv.watch.baseline.is_none()
                && !tv.watch.scanning
                && now.saturating_duration_since(tv.watch.started) >= settle
            {
                match pid {
                    Some(pid) => self.scan(i, Scan::Baseline, pid),
                    // No process behind it (a test's fake): nothing runs under it.
                    None => self.tickets[i].watch.baseline = Some(HashSet::new()),
                }
            }
        }
        for i in due(&self.facts(now), after, cap) {
            let tv = &self.tickets[i];
            let recent =
                tv.watch.busy.is_some_and(|at| now.saturating_duration_since(at) < recheck(after));
            if tv.watch.scanning || recent {
                continue;
            }
            match tv.session.as_ref().and_then(|s| s.pid()) {
                Some(pid) => self.scan(i, Scan::Check, pid),
                None => self.put_to_sleep(i),
            }
        }
    }

    fn facts(&self, now: Instant) -> Vec<Facts> {
        self.tickets
            .iter()
            .enumerate()
            .map(|(i, tv)| Facts {
                state: tv.status.state,
                running: tv.session.as_ref().is_some_and(|s| !s.exited()),
                agent: tv.t.agent.is_some(),
                selected: i == self.sel,
                away: tv.away.is_some(),
                reaping: tv.reaping(),
                settled: tv.watch.baseline.is_some(),
                idle: now.saturating_duration_since(tv.watch.selected),
            })
            .collect()
    }

    /// Looks at session `i`'s process tree on a thread, for `Msg::Scanned`.
    fn scan(&mut self, i: usize, kind: Scan, pid: u32) {
        let tv = &mut self.tickets[i];
        let Some(launch) = tv.launch.clone() else { return };
        tv.watch.scanning = true;
        let id: Id = (tv.t.repo.clone(), tv.t.key.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Scanned(id, launch, kind, proctree::descendants(pid)));
        });
    }

    /// What a look at a process tree found (`scan`). A session that has started again since, or
    /// stopped, isn't the one looked at. A look that failed changes nothing: the session stays
    /// awake until the next.
    pub(super) fn scanned(
        &mut self,
        id: &Id,
        launch: &str,
        kind: Scan,
        found: Result<HashSet<u32>, String>,
    ) {
        let now = Instant::now();
        let Some(i) = self.tickets.iter().position(|tv| tv.t.is(&id.0, &id.1)) else { return };
        let tv = &mut self.tickets[i];
        if tv.launch.as_deref() != Some(launch) || tv.session.is_none() {
            return;
        }
        tv.watch.scanning = false;
        let Ok(found) = found else { return };
        match kind {
            Scan::Baseline => tv.watch.baseline = Some(found),
            Scan::Check if working(tv.watch.baseline.as_ref(), &found) => {
                tv.watch.busy = Some(now);
            }
            Scan::Check => {
                // Asked again now: it may have been selected, or started working, meanwhile.
                let after = self.config.sleep_after.map(Duration::from_secs);
                if due(&self.facts(now), after, self.config.max_awake).contains(&i) {
                    self.put_to_sleep(i);
                }
            }
        }
    }

    /// Stops session `i` the way a restart does (`TicketView::stop_session`), leaving it asleep:
    /// shown so, and started again, on the same conversation, when it's selected.
    fn put_to_sleep(&mut self, i: usize) {
        self.tickets[i].sleep();
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentState::{Done, Exited, Idle, Needs, Working};

    const HOUR: Duration = Duration::from_secs(3600);

    fn idle_for(mins: u64) -> Facts {
        Facts {
            state: Idle,
            running: true,
            agent: false,
            selected: false,
            away: false,
            reaping: false,
            settled: true,
            idle: Duration::from_secs(mins * 60),
        }
    }

    #[test]
    fn an_idle_session_sleeps_once_unselected_for_the_time() {
        let sessions = [idle_for(59), idle_for(60), idle_for(300)];
        assert_eq!(due(&sessions, Some(HOUR), None), [1, 2]);
        assert!(due(&sessions, None, None).is_empty(), "never");
        // Both a finished turn and nothing yet are restful.
        let done = Facts { state: Done, ..idle_for(90) };
        assert_eq!(due(&[done], Some(HOUR), None), [0]);
    }

    #[test]
    fn only_a_restful_session_sleeps() {
        let long = idle_for(600);
        let not = [
            Facts { state: Working, ..long },
            Facts { state: Needs, ..long },
            Facts { state: Exited, ..long },
            Facts { state: AgentState::NotStarted, ..long },
            Facts { agent: true, ..long },
            Facts { selected: true, ..long },
            Facts { away: true, ..long },
            Facts { reaping: true, ..long },
            Facts { settled: false, ..long },
            Facts { running: false, ..long },
        ];
        for f in not {
            assert!(due(&[f], Some(HOUR), Some(1)).is_empty(), "{f:?}");
        }
    }

    #[test]
    fn past_the_cap_the_longest_unselected_sleep_first() {
        // Five running; three allowed: the two idle longest go, whatever the time.
        let s = [idle_for(5), idle_for(40), idle_for(10), idle_for(20), idle_for(1)];
        assert_eq!(due(&s, None, Some(3)), [1, 3]);
        assert!(due(&s, None, Some(5)).is_empty());
        assert!(due(&s, None, None).is_empty());
        // The time and the cap together are one list, each session once.
        assert_eq!(due(&s, Some(Duration::from_secs(30 * 60)), Some(3)), [1, 3]);
        assert_eq!(due(&s, Some(Duration::from_secs(15 * 60)), Some(4)), [1, 3]);
        // Sessions that can't sleep count as running but are never picked.
        let mut s = s.to_vec();
        s[1].state = Working;
        s[3].selected = true;
        assert_eq!(due(&s, None, Some(3)), [0, 2]);
    }

    #[test]
    fn the_cap_counts_only_what_runs() {
        let mut s = vec![idle_for(5), idle_for(40), idle_for(10)];
        s[2].running = false;
        assert_eq!(due(&s, None, Some(2)), [] as [usize; 0]);
        assert_eq!(due(&s, None, Some(1)), [1]);
    }

    #[test]
    fn new_descendants_are_work_in_progress() {
        let base: HashSet<u32> = [10, 11].into();
        assert!(!working(Some(&base), &[10, 11].into()));
        assert!(!working(Some(&base), &[10].into()), "a server that ended is no work");
        assert!(working(Some(&base), &[10, 11, 12].into()));
        assert!(working(Some(&base), &[12].into()));
        assert!(!working(Some(&HashSet::new()), &HashSet::new()));
        assert!(working(None, &HashSet::new()), "no baseline, no certainty");
    }

    #[test]
    fn settling_is_short_when_the_setting_is() {
        assert_eq!(settle(Some(HOUR)), SETTLE);
        assert_eq!(settle(None), SETTLE);
        assert_eq!(settle(Some(Duration::from_secs(8))), Duration::from_secs(2));
        assert_eq!(recheck(Some(HOUR)), RECHECK);
        assert_eq!(recheck(None), RECHECK);
        assert_eq!(recheck(Some(Duration::from_secs(8))), Duration::from_secs(4));
    }

    #[test]
    fn the_settings_read_back_as_written() {
        let words: Vec<_> = AFTER.iter().map(|a| after_label(*a)).collect();
        assert_eq!(words, ["15 min", "30 min", "1 hour", "2 hours", "4 hours", "never"]);
        assert_eq!(after_label(Some(90)), "90 s");
        assert_eq!(after_label(Some(120)), "2 min");
        assert_eq!(cap_label(None), "no limit");
        assert_eq!(cap_label(Some(4)), "4 sessions");
        // A value set by hand in the config file stays choosable.
        assert_eq!(after_choices(Some(5)).last(), Some(&Some(5)));
        assert_eq!(after_choices(Some(3600)).len(), AFTER.len());
        assert_eq!(cap_choices(Some(7)).last(), Some(&Some(7)));
        // No two presets share a label, or the field couldn't tell them apart.
        let all: HashSet<_> = words.iter().collect();
        assert_eq!(all.len(), words.len());
    }
}
