//! The TUI's side of `taskman add` and `taskman close`: requests taken from the inbox folder on
//! each status poll, agents waiting for the user's approval, closes waiting until nothing refers to
//! a ticket by its place in the list, and the replies (see `add.rs` and `close.rs` for the
//! commands).

use super::sync::{Msg, pr_lookup};
use super::ticket::{Added, merged_head, own_worktree};
use super::tickets::Closed;
use super::{AgentAsk, App, FormKind, Mode, parse_key};
use crate::session::Reaper;
use crate::store::{self, Base, Cli, Ticket};
use crate::{add, close, resume, untrusted, util, worktree};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How often `inbox/alive` is rewritten. Commands count the TUI as running for 3 s after the last
/// write (`add::ALIVE`), which leaves room for a slow pass; the inbox itself is still read on every
/// status poll.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);

/// Whether the pass at `now` rewrites `inbox/alive`, last written at `last`: the first pass does,
/// then one at least `HEARTBEAT_EVERY` later. What it holds doesn't change while taskman runs, so
/// the write is only for its time.
fn heartbeat_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= HEARTBEAT_EVERY)
}

/// The reply for agent `t`, just added by `taskman add` to `project`: its session starts at once,
/// named `session`.
fn added_agent_line(t: &Ticket, project: &str, session: &str) -> String {
    let start = format!("Added agent ✦ {} to {project}. Its session is starting", t.display());
    match t.cli {
        Cli::Claude => format!("{start}: message it as {session} once ListAgents lists it"),
        Cli::Codex => {
            format!("{start} in Codex, which can't be messaged; the user talks to it in taskman")
        }
    }
}

impl App {
    /// Adds what `taskman add` asked for and answers it. Runs on every status poll, and at the
    /// first tick for requests queued while taskman was closed.
    pub(super) fn take_inbox(&mut self) {
        let now = Instant::now();
        // A write that failed is tried again on the next pass.
        if heartbeat_due(self.heartbeat_at, now) && add::heartbeat() {
            self.heartbeat_at = Some(now);
        }
        let mut toasts = Vec::new();
        for (id, req) in add::take() {
            if req.close.is_some() {
                self.close_requested(id, req);
                continue;
            }
            if req.agent().is_some() {
                add::reply(&id, &req, &add::asked(&req, self.agent_open(&req)));
                self.asking.push_back(req);
                continue;
            }
            let (lines, toast) = self.add_requested(&req);
            add::reply(&id, &req, &lines);
            toasts.extend(toast);
            self.dirty.store(true, Ordering::Relaxed);
        }
        if !toasts.is_empty() {
            self.toast(toasts.join(" · "));
        }
    }

    /// Starts on what `taskman close` asked for. The tickets it depends on get a fresh PR lookup
    /// first, as their panels may be a minute old or, at startup, empty: whether a PR merged
    /// decides `--merged`, and its head is what the removal check counts as pushed after a squash
    /// merge. `close_next` then closes them.
    fn close_requested(&mut self, id: String, req: add::Request) {
        let close = req.close.unwrap_or_default();
        // The command sends tickets or `--merged`, but anything can write to the inbox, and a
        // request with neither would close every ticket in the project.
        if req.items.is_empty() && !close.merged {
            let e =
                "nothing to close: the request names no ticket and doesn't ask for the merged ones";
            add::reply(&id, &req, &[add::Line::err(e)]);
            return;
        }
        let mut lookups = Vec::new();
        let projects = &self.projects;
        for tv in &mut self.tickets {
            let asked = || {
                req.items
                    .iter()
                    .any(|i| matches!(i, add::Item::Ticket { key, .. } if *key == tv.t.key))
                    || (req.items.is_empty() && close.merged)
            };
            if tv.t.repo != req.repo || tv.t.agent.is_some() || !asked() {
                continue;
            }
            if close.merged || (!close.keep_worktree && own_worktree(tv)) {
                self.fetches += 1;
                let n = self.fetches;
                tv.pr.start(n);
                let closes = projects
                    .iter()
                    .find(|p| p.repo == tv.t.repo)
                    .and_then(|p| p.tracker.as_ref()?.closes(&tv.t.key));
                lookups.push((n, tv.t.repo.clone(), tv.t.key.clone(), tv.t.branch.clone(), closes));
            }
        }
        if lookups.is_empty() {
            self.closing.push_back((id, req));
            return;
        }
        let (tx, report) = (self.tx.clone(), self.reporter());
        std::thread::spawn(move || {
            std::thread::scope(|s| {
                for (n, repo, key, branch, closes) in lookups {
                    let (tx, report) = (tx.clone(), report.clone());
                    s.spawn(move || {
                        let what = format!("looking up {key}'s PR");
                        let (res, wt) = report
                            .reported(&what, || pr_lookup(&repo, &key, branch, closes.as_ref()))
                            .unwrap_or_else(|e| (Err(e), None));
                        let _ = tx.send(Msg::Pr(n, (repo, key), res, wt));
                    });
                }
            });
            // After the lookups' messages, so the panels hold what they found.
            let _ = tx.send(Msg::Close(id, req));
        });
    }

    /// Closes the `taskman close` requests whose lookups finished, unless a dialog or form refers
    /// to a ticket by its place in the list, which closing one would shift.
    pub(super) fn close_next(&mut self) {
        let holds_index = match &self.mode {
            Mode::ConfirmClose => true,
            Mode::Form(f) => matches!(f.kind, FormKind::RenameAgent(_)),
            _ => false,
        };
        while !holds_index && let Some((id, req)) = self.closing.pop_front() {
            self.close_now(&id, req);
        }
    }

    /// Closes a request's tickets as the close dialog does: each stops and leaves the list now,
    /// then a thread runs the removal check on the worktrees taskman created and removes those it
    /// passes (`close::remove_worktree`), and replies.
    fn close_now(&mut self, id: &str, req: add::Request) {
        let close = req.close.unwrap_or_default();
        let Some(p) = self.projects.iter().position(|p| p.repo == req.repo) else {
            let e = format!("the project in {} is no longer in taskman", store::tilde(&req.repo));
            add::reply(id, &req, &[add::Line::err(e)]);
            return;
        };
        let project = self.projects[p].name.clone();
        // What to close, by name for the reply and key: the items, else (`--merged` alone) every
        // ticket.
        let targets: Vec<(String, Option<String>)> = if req.items.is_empty() {
            let open =
                self.tickets.iter().filter(|tv| tv.t.repo == req.repo && tv.t.agent.is_none());
            open.map(|tv| (tv.t.key.clone(), Some(tv.t.key.clone()))).collect()
        } else {
            req.items
                .iter()
                .map(|item| match item {
                    // The command checks it too, but anything can write to the inbox.
                    add::Item::Ticket { key, .. }
                        if parse_key(key).as_deref() != Some(key.as_str()) =>
                    {
                        (untrusted::line(key), None)
                    }
                    add::Item::Ticket { key, .. } => (key.clone(), Some(key.clone())),
                    add::Item::Agent { name, .. } => {
                        let key = self.tickets.iter().find(|tv| tv.t.is_agent(&req.repo, name));
                        (format!("✦ {}", store::slug(name)), key.map(|tv| tv.t.key.clone()))
                    }
                })
                .collect()
        };
        enum Step {
            Done(close::Outcome),
            Remove {
                repo: PathBuf,
                path: PathBuf,
                merged: Option<String>,
                hold: resume::Hold,
                reaper: Option<Reaper>,
            },
        }
        enum Plan {
            Done(close::Outcome),
            Remove { repo: PathBuf, path: PathBuf, merged: Option<String> },
        }
        let mut steps = Vec::new();
        for (name, key) in targets {
            let i = key.and_then(|k| self.tickets.iter().position(|tv| tv.t.is(&req.repo, &k)));
            let Some(i) = i else {
                steps.push((name, Step::Done(close::Outcome::NotOpen)));
                continue;
            };
            let tv = &self.tickets[i];
            let merged = matches!(&tv.pr.data, Some(Some(pr)) if pr.state == "MERGED");
            if close.merged && tv.t.agent.is_none() && !merged {
                let not = match &tv.pr.error {
                    Some(e) => close::Outcome::Unchecked(e.clone()),
                    None => close::Outcome::NotMerged(
                        tv.pr.data.as_ref().and_then(|d| d.as_ref()).map(|pr| pr.number),
                    ),
                };
                steps.push((name, Step::Done(not)));
                continue;
            }
            let (repo, path) = (tv.t.repo.clone(), tv.t.cwd.clone());
            // Still being created, it's in the main checkout until git finishes.
            let keep_new = (close.keep_worktree && tv.creating).then_some(tv.creation_id);
            let plan = if tv.t.agent.is_some() || path == repo {
                Plan::Done(close::Outcome::Closed(close::Worktree::None))
            } else if !own_worktree(tv) {
                Plan::Done(close::Outcome::Closed(close::Worktree::NotOurs(path)))
            } else if close.keep_worktree {
                Plan::Done(close::Outcome::Closed(close::Worktree::Asked(path)))
            } else {
                Plan::Remove { repo, path, merged: merged_head(tv) }
            };
            let id = (req.repo.clone(), self.tickets[i].t.key.clone());
            match self.close_at(i) {
                Ok(Closed { hold, reaper }) => {
                    if let Some(c) = keep_new.and_then(|id| self.closed_creating.get_mut(&id)) {
                        c.keep = true;
                    }
                    let step = match (plan, hold) {
                        (Plan::Remove { repo, path, merged }, Ok(hold)) => {
                            Step::Remove { repo, path, merged, hold, reaper }
                        }
                        (Plan::Done(o), _) => {
                            self.keep_reaper(id, reaper);
                            Step::Done(o)
                        }
                        // Closed, but nothing reserves the folder: it stays, as `close_at` says.
                        (Plan::Remove { path, .. }, Err(why)) => {
                            self.keep_reaper(id, reaper);
                            Step::Done(close::Outcome::Closed(close::Worktree::Kept(path, why)))
                        }
                    };
                    steps.push((name, step));
                }
                Err(o) => steps.push((name, Step::Done(o))),
            }
        }
        let (tx, id, from) = (self.tx.clone(), id.to_string(), req.from.clone());
        let finish = move |steps: Vec<(String, Step)>| {
            let done: Vec<(String, close::Outcome)> = steps
                .into_iter()
                .map(|(name, step)| match step {
                    Step::Done(o) => (name, o),
                    Step::Remove { repo, path, merged, hold, reaper } => {
                        // The CLI ran in the folder: gone first, then git deletes it.
                        if let Some(reaper) = reaper {
                            reaper.wait();
                        }
                        let w = close::remove_worktree(&repo, &path, merged.as_deref(), &hold);
                        drop(hold);
                        (name, close::Outcome::Closed(w))
                    }
                })
                .collect();
            let lines: Vec<add::Line> =
                done.iter().map(|(n, o)| close::line(n, &project, o)).collect();
            add::reply(&id, &req, &lines);
            if let Some(t) = close::toast(from.as_deref(), &done) {
                let _ = tx.send(Msg::Closed(t));
            }
        };
        // git runs only for worktrees to remove; the rest answers at once.
        let removed: Vec<String> = steps
            .iter()
            .filter(|(_, s)| matches!(s, Step::Remove { .. }))
            .map(|(n, _)| n.clone())
            .collect();
        if !removed.is_empty() {
            self.removing.push((removed, std::thread::spawn(move || finish(steps))));
        } else {
            finish(steps);
        }
    }

    /// Opens the dialog for the next agent `taskman add` asked for, once nothing else has the keys.
    pub(super) fn ask_next(&mut self) {
        if !matches!(self.mode, Mode::Normal) || self.prefix {
            return;
        }
        let Some(req) = self.asking.pop_front() else { return };
        let open = self.agent_open(&req);
        self.selection = None;
        self.mode = Mode::ConfirmAgent(Box::new(AgentAsk { req, open, shown: Instant::now() }));
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Whether the agent request `req` asks for is open already.
    fn agent_open(&self, req: &add::Request) -> bool {
        req.agent()
            .is_some_and(|(name, _)| self.tickets.iter().any(|tv| tv.t.is_agent(&req.repo, name)))
    }

    /// The user's answer to an agent `taskman add` asked for. The command was answered when the
    /// request came in, so this only toasts.
    pub(super) fn answer_agent(&mut self, req: &add::Request, yes: bool) {
        let name = req.agent().map(|(n, _)| store::slug(n)).unwrap_or_default();
        if !yes {
            let from = req.from.as_ref().map_or(String::new(), |f| format!(" from {f}"));
            self.toast(format!("Declined ✦ {name}{from}"));
            return;
        }
        let (lines, toast) = self.add_requested(req);
        match lines.iter().find(|l| !l.ok) {
            Some(err) => self.error(format!("✦ {name}: {}", untrusted::line(&err.text))),
            None => {
                self.toast(toast.unwrap_or_else(|| {
                    format!("✦ {name}'s new role applies when its session next starts")
                }));
            }
        }
    }

    /// Adds a request's tickets or agent; returns the reply and what the toast says.
    fn add_requested(&mut self, req: &add::Request) -> (Vec<add::Line>, Option<String>) {
        let Some(p) = self.projects.iter().position(|p| p.repo == req.repo) else {
            let e = format!("the project in {} is no longer in taskman", store::tilde(&req.repo));
            return (
                req.items.iter().map(|_| add::Line::err(e.clone())).collect(),
                Some(format!("taskman add: {e}")),
            );
        };
        let mut added = Vec::new();
        let mut lines = Vec::new();
        let base = match req.base.as_deref().map(|b| self.base_named(p, b)).transpose() {
            Ok(b) => b,
            Err(e) => {
                return (
                    req.items.iter().map(|_| add::Line::err(e.clone())).collect(),
                    Some(format!("taskman add: {e}")),
                );
            }
        };
        for item in &req.items {
            lines.push(match item {
                add::Item::Ticket { .. } if self.projects[p].tracker.is_none() => {
                    add::Line::err(self.projects[p].agents_only())
                }
                // The command checks it too, but anything can write to the inbox.
                add::Item::Ticket { key, .. }
                    if parse_key(key).as_deref() != Some(key.as_str()) =>
                {
                    add::Line::err(format!("{} isn't a ticket key", untrusted::line(key)))
                }
                add::Item::Ticket { key, title } => {
                    let proj = &self.projects[p];
                    let create = req.worktree.unwrap_or_else(|| proj.worktrees.creates());
                    match self.add_ticket(p, key.clone(), req.cli, create, base.clone(), false) {
                        Ok(Added::Open(_)) => {
                            add::Line::ok(format!("{key} is already open in taskman"))
                        }
                        Ok(Added::New(i)) => {
                            added.push(self.qualified(&self.tickets[i].t));
                            add::Line::ok(self.added_line(p, i, title, base.as_ref()))
                        }
                        Err(e) => add::Line::err(e),
                    }
                }
                add::Item::Agent { name, role } => {
                    match self.add_agent(p, name, req.cli, role.clone(), false) {
                        Ok(Added::Open(i)) => {
                            let tv = &self.tickets[i];
                            let role = if role.is_some() {
                                "; its new role applies when its session next starts"
                            } else {
                                ""
                            };
                            add::Line::ok(format!(
                                "✦ {} is already open in taskman{role}",
                                tv.t.display()
                            ))
                        }
                        Ok(Added::New(i)) => {
                            self.tickets[i].start = true;
                            let t = &self.tickets[i].t;
                            added.push(format!("✦ {}", t.display()));
                            add::Line::ok(added_agent_line(
                                t,
                                &self.projects[p].name,
                                &self.session_name(i),
                            ))
                        }
                        Err(e) => add::Line::err(e),
                    }
                }
            });
        }
        let toast = (!added.is_empty()).then(|| {
            let who = req.from.as_ref().map_or("Added".to_string(), |f| format!("{f} added"));
            let queued =
                if add::queued(req, util::now()) { " while taskman was closed" } else { "" };
            format!("{who} {}{queued}", added.join(", "))
        });
        (lines, toast)
    }

    /// The reply for ticket `i`, just added to project `p`: where it works, and for an agent
    /// asking, when it can message the session.
    fn added_line(&self, p: usize, i: usize, title: &str, base: Option<&Base>) -> String {
        let (tv, proj) = (&self.tickets[i], &self.projects[p]);
        let title = if title.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", util::truncate(title, 60))
        };
        let place = if tv.creating {
            format!(
                "creating worktree {}",
                store::tilde(&worktree::folder_path(
                    proj.worktrees.folder_format(),
                    &proj.repo,
                    &tv.t.key
                ))
            )
        } else if tv.t.cwd != tv.t.repo {
            format!("in its worktree {}", store::tilde(&tv.t.cwd))
        } else {
            "in the main checkout".into()
        };
        let mut line = format!("Added {}{title} to {}; {place}", tv.t.key, proj.name);
        if let Some(b) = base {
            line += &match tv.creating {
                true => {
                    format!(", stacked on {} from its branch {}", b.key, untrusted::line(&b.branch))
                }
                false => format!(" (it had a worktree already, so it isn't stacked on {})", b.key),
            };
        }
        // Its session starts now, so the project's agents can reach it.
        let agent_runs = self.tickets.iter().any(|a| {
            a.t.agent.is_some()
                && a.t.cli == Cli::Claude
                && a.t.repo == proj.repo
                && a.session.as_ref().is_some_and(|s| !s.exited())
        });
        if agent_runs && tv.t.cli == Cli::Claude {
            line += &format!(
                ". Its session is starting: message it as {} once ListAgents lists it",
                self.session_name(i)
            );
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::agent;

    #[test]
    fn the_heartbeat_is_written_at_most_once_a_second() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert!(heartbeat_due(None, start));
        // Status polls 300 ms apart write it on the first and then once a second has passed.
        let mut last = None;
        let written: Vec<u64> = (0..10)
            .map(|i| i * 300)
            .filter(|&ms| {
                let due = heartbeat_due(last, at(ms));
                if due {
                    last = Some(at(ms));
                }
                due
            })
            .collect();
        assert_eq!(written, [0, 1200, 2400]);
        assert!(!heartbeat_due(Some(at(0)), at(999)));
        assert!(heartbeat_due(Some(at(0)), at(1000)));
    }

    #[test]
    fn says_when_an_added_agent_can_be_messaged() {
        assert_eq!(
            added_agent_line(&agent("tester", Cli::Claude, None), "acme", "acme-tester"),
            "Added agent ✦ tester to acme. Its session is starting: message it as acme-tester once ListAgents lists it"
        );
        assert!(
            added_agent_line(&agent("tester", Cli::Codex, None), "acme", "acme-tester")
                .contains("can't be messaged")
        );
    }
}
