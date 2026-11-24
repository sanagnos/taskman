//! The TUI's side of `taskman events`: what it last saw of each ticket's PRs and session
//! (`Feed`), and the changes it notes in the project's events file (`App::note_events`). Each is
//! noted once, as it's seen: the first look at a ticket's PRs (a lookup) or its session (a status
//! poll) after taskman starts, or the ticket is added, is only the baseline, so a start floods
//! nothing. Agents have no events: every agent's brief says to follow them, so two agents would
//! wake each other with each turn's end, without end.

use super::{AgentState, App, TicketView};
use crate::events::{self, Event, Line, PrSnap, Prs};
use crate::github::Lookup;
use crate::{store, util};
use std::path::PathBuf;

/// What a ticket's events were last worked out from. Not saved: a restart starts from what it
/// finds, and `taskman events --follow` says to look at `taskman status` for what came between.
#[derive(Default)]
pub(super) struct Feed {
    /// Its PRs as the lookups found them; None before the first lookup that counts.
    prs: Option<Prs>,
    /// Its session's state at the last status poll; None before the first.
    state: Option<AgentState>,
    /// Changes seen and not written yet.
    pending: Vec<Event>,
}

impl TicketView {
    /// The PR changes since the last lookup that counted, from this one's: only lookups of the
    /// checkout's own repository's PRs count (`Lookup::elsewhere`).
    pub(super) fn feed_pr(&mut self, found: &Lookup) {
        if self.t.agent.is_some() {
            return;
        }
        // Not the checkout's repository's PRs, or it can't be told: a number in an event must be
        // one `gh pr view` finds in the checkout, and another repository's would name another PR.
        if found.elsewhere {
            return;
        }
        let shown = found.pr.as_ref().map(PrSnap::of);
        match &mut self.feed.prs {
            Some(prs) => self.feed.pending.extend(prs.see(shown, &found.weighed)),
            None => {
                let mut prs = Prs::default();
                prs.see(shown, &found.weighed);
                self.feed.prs = Some(prs);
            }
        }
    }

    /// The session's change since the last status poll, as one event at most however often its
    /// state moved in between. A ticket's only.
    fn feed_state(&mut self) {
        if self.t.agent.is_some() {
            return;
        }
        let now = self.status.state;
        if let Some(before) = self.feed.state.replace(now)
            && before != now
            && let Some(e) = events::session_event(now, self.asleep)
        {
            self.feed.pending.push(e);
        }
    }
}
impl App {
    /// Writes the changes seen since the last call to each project's events file; `polled` after
    /// a status poll, which the session states are compared at. A write that fails is warned of
    /// once until one succeeds, and its events dropped, and a file that can't be moved aside once
    /// until one is: the TUI goes on either way.
    pub(super) fn note_events(&mut self, polled: bool) {
        let ts = util::now();
        let mut by_project: Vec<(PathBuf, Vec<Line>)> = Vec::new();
        for tv in &mut self.tickets {
            if polled {
                tv.feed_state();
            }
            if tv.feed.pending.is_empty() {
                continue;
            }
            let pending = std::mem::take(&mut tv.feed.pending);
            // A key the file can't hold isn't a ticket's or agent's as taskman makes them.
            let Some(key) = events::Key::new(&tv.t.key) else { continue };
            let lines = pending.into_iter().map(|event| Line { ts, key: key.clone(), event });
            match by_project.iter_mut().find(|(repo, _)| *repo == tv.t.repo) {
                Some((_, all)) => all.extend(lines),
                None => by_project.push((tv.t.repo.clone(), lines.collect())),
            }
        }
        for (repo, lines) in by_project {
            let path = events::path(&repo);
            let rotation = match events::append(&path, &lines) {
                Ok(rotation) => {
                    self.events_failed = false;
                    rotation
                }
                Err(e) if !self.events_failed => {
                    self.events_failed = true;
                    self.warn(format!(
                        "Couldn't note changes for `taskman events` in {}: {e}",
                        store::tilde(&path)
                    ));
                    continue;
                }
                Err(_) => continue,
            };
            match rotation {
                events::Rotation::Failed(e) if !self.events_unrotated => {
                    self.events_unrotated = true;
                    self.warn(format!(
                        "Couldn't move {} aside, so it grows until it can: {e}",
                        store::tilde(&path)
                    ));
                }
                events::Rotation::Done => self.events_unrotated = false,
                _ => {}
            }
        }
    }
}
