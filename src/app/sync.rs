//! Work in the background: tracker and PR fetches on their schedule (`Remote`), worktree creation,
//! and the messages threads send back (`Msg`), which `drain_messages` applies on each tick.

use super::forms::Check;
use super::{App, Id, Mode};
use crate::store::{self, Base, Ticket};
use crate::tracker::{Access, Issue, Text};
use crate::{add, github, selection, worktree};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

// The ticket on screen refreshes often; the others refresh in the background.
const JIRA_SELECTED: Duration = Duration::from_secs(30);
const JIRA_BACKGROUND: Duration = Duration::from_secs(180);
const GH_SELECTED: Duration = Duration::from_secs(20);
const GH_BACKGROUND: Duration = Duration::from_secs(60);
/// Switching to a ticket, or Claude finishing a turn, refreshes data older than this.
pub(super) const STALE: Duration = Duration::from_secs(30);

/// Data fetched in the background. Keeps the last good value when a refresh fails.
pub struct Remote<T> {
    pub data: Option<T>,
    pub error: Option<String>,
    pub loading: bool,
    /// When the last successful fetch finished.
    pub fetched_at: Option<Instant>,
    last_attempt: Option<Instant>,
    /// Refresh on the next tick regardless of the interval.
    pub(super) force: bool,
    /// Whether the forced refresh is one the user is waiting on (and so shows a spinner).
    force_loud: bool,
    /// Whether the fetch in flight shows a spinner. Routine refreshes stay quiet.
    pub loud: bool,
    /// The latest fetch's id (`App::fetches`), which its reply carries (`Msg::Jira`, `Msg::Pr`).
    /// A reply with another id is an earlier fetch's, or an earlier ticket's with this key, and
    /// is dropped (`expects`): otherwise one that lands late overwrites newer data as fresh.
    fetch: u64,
}

impl<T> Remote<T> {
    pub(super) fn new() -> Self {
        Self {
            data: None,
            error: None,
            loading: false,
            fetched_at: None,
            last_attempt: None,
            force: false,
            force_loud: false,
            loud: false,
            fetch: 0,
        }
    }

    pub(super) fn due(&self, every: Duration) -> bool {
        !self.loading && (self.force || self.last_attempt.is_none_or(|t| t.elapsed() >= every))
    }

    /// A fetch with this id starts; its reply is the one to take.
    pub(super) fn start(&mut self, id: u64) {
        self.loading = true;
        // The first load shows a spinner too: the panel has nothing else to show yet.
        self.loud = self.force_loud || self.data.is_none();
        self.force = false;
        self.force_loud = false;
        self.last_attempt = Some(Instant::now());
        self.fetch = id;
    }

    /// Expects this fetch's reply without a spinner: for worktree creation's fetch, which waits
    /// for a token that may never be read, and so can't hold `loading`.
    pub(super) fn expect(&mut self, id: u64) {
        self.fetch = id;
    }

    /// Whether a reply with this id is the latest fetch's.
    pub(super) fn expects(&self, id: u64) -> bool {
        self.fetch == id
    }

    pub(super) fn finish(&mut self, res: Result<T, String>) {
        self.loading = false;
        match res {
            Ok(v) => {
                self.data = Some(v);
                self.error = None;
                self.fetched_at = Some(Instant::now());
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Refresh now. `loud` shows a spinner: for refreshes the user asked for or is waiting on.
    pub(super) fn request(&mut self, loud: bool) {
        self.force = true;
        self.force_loud |= loud;
    }

    pub(super) fn request_if_older_than(&mut self, age: Duration, loud: bool) {
        if self.fetched_at.is_none_or(|t| t.elapsed() >= age) {
            self.request(loud);
        }
    }
}

pub(super) enum Msg {
    /// A tracker fetch finished: the fetch's id (`Remote::start`), its ticket and what it found.
    Jira(u64, Id, Result<Issue, String>),
    /// A PR lookup finished: the lookup's id, its ticket, what it found and the worktree git
    /// found for the ticket.
    Pr(u64, Id, Result<github::Lookup, String>, Option<(PathBuf, String)>),
    /// A worktree was created, or failed: the creation's id (`TicketView::creation_id`), its
    /// ticket, and the base it was asked to start from.
    Worktree(u64, Id, Result<worktree::Created, String>, Option<Base>),
    Removed(Result<String, String>),
    /// A `taskman close` request's PR lookups finished: it can close (`close_next`).
    Close(String, add::Request),
    /// A `taskman close` request finished; the toast.
    Closed(String),
    /// Whether a project's folder can show PRs, checked again.
    Gh(PathBuf, github::Gh),
    /// An account's token, read from its source (`env:`, `cmd:` or `keychain:`).
    Token(String, Result<String, String>),
    /// A form's check finished: the form's `checking` id and what it found.
    Checked(u64, Check),
    /// Text went to the clipboard, or no command could take it; with what the toast calls it.
    Copied(String, selection::Copied, Option<String>),
    Toast(String),
    /// What a project form's account can see to pick the ticket prefix from: the listing's id
    /// (`Choices`), and the list.
    Choices(u64, Result<Vec<crate::tracker::Choice>, String>),
    /// A project's open issues, for the Add form: the request that listed them, and the list.
    OpenIssues(super::add_form::Listing, Result<Vec<crate::tracker::Listed>, String>),
    /// The multiplexer taskman runs in has the same prefix (`prefix_tip`).
    PrefixTip(String),
    /// There's an OS keychain to save tokens to.
    Keychain(crate::keychain::Keychain),
    /// A session's process tree was looked at (`App::scan`): its ticket, the start it belongs to,
    /// why, and the pids under its CLI.
    Scanned(Id, String, super::sleep::Scan, Result<std::collections::HashSet<u32>, String>),
}

/// Looks ticket `key`'s PR up: one that `closes` its issue, else on its worktree's branch, else on
/// `branch`; with the worktree git found for it.
pub(super) fn pr_lookup(
    repo: &Path,
    key: &str,
    branch: Option<String>,
    closes: Option<&github::Closes>,
) -> (Result<github::Lookup, String>, Option<(PathBuf, String)>) {
    let wt = github::find_worktree(repo, key);
    let branch = wt.as_ref().map(|(_, b)| b.clone()).or(branch);
    (github::fetch(repo, branch.as_deref(), key, closes), wt)
}

impl App {
    /// Applies what the background threads sent. On the UI thread, each tick, and at a quit that
    /// waits for a worktree without the event loop (`main::shutdown`).
    pub fn drain_messages(&mut self) {
        let mut changed = false;
        let mut toast = None;
        while let Ok(msg) = self.rx.try_recv() {
            self.dirty.store(true, Ordering::Relaxed);
            match msg {
                // Matched by fetch, not key alone: a reply that lands after the tracker changed,
                // or after the ticket closed and was added again, is older than what the panel
                // shows, or another workspace's or ticket's.
                Msg::Jira(n, (repo, key), res) => {
                    let Some(tv) = self.tickets.iter_mut().find(|tv| tv.t.is(&repo, &key)) else {
                        continue;
                    };
                    if !tv.jira.expects(n) {
                        continue;
                    }
                    tv.jira.finish(res);
                    changed |= tv.baseline_seen();
                }
                Msg::Pr(n, (repo, key), res, worktree) => {
                    let Some(tv) = self.tickets.iter_mut().find(|tv| tv.t.is(&repo, &key)) else {
                        continue;
                    };
                    if !tv.pr.expects(n) {
                        continue;
                    }
                    // gh was removed or logged out since the last check: hide the panel until
                    // the next.
                    if let Err(e) = &res
                        && let Some(gh) = github::Gh::from_error(e)
                    {
                        self.github.insert(tv.t.repo.clone(), gh);
                    }
                    changed |= tv.take_pr(res, worktree);
                }
                Msg::Worktree(n, (repo, key), res, base) => {
                    let closed = self.closed_creating.remove(&n);
                    // Matched by creation, not key: the ticket may have closed and been added
                    // again since, and this worktree isn't the new one's.
                    let mine = |tv: &super::TicketView| tv.creating && tv.creation_id == n;
                    let Some(i) = self.tickets.iter().position(mine) else {
                        // Gone without a close (its project was deleted, say): it stays.
                        let keep = closed.is_none_or(|c| c.keep);
                        if let Ok(c) = res
                            && let Some(t) = self.created_after_close(repo, key, c, keep)
                        {
                            toast = Some(t);
                        }
                        continue;
                    };
                    let key = self.qualified(&self.tickets[i].t);
                    let tv = &mut self.tickets[i];
                    tv.creating = false;
                    tv.creation = None;
                    changed = true;
                    toast = Some(match res {
                        Ok(c) => {
                            let note = c.note.map(|n| format!("; {n}")).unwrap_or_default();
                            let on = base
                                .as_ref()
                                .filter(|_| c.stacked)
                                .map(|b| format!(", stacked on {}", b.key))
                                .unwrap_or_default();
                            let msg = format!(
                                "{key} works in {} on {}{on}{note}",
                                store::tilde(&c.path),
                                c.branch
                            );
                            if c.stacked {
                                tv.t.base = base;
                            }
                            tv.t.cwd = c.path;
                            tv.t.branch = Some(c.branch);
                            // Not when the folder already was the branch's worktree: someone
                            // else's, which closing the ticket must leave.
                            tv.t.created_worktree = c.made;
                            tv.t.pending_worktree = None;
                            msg
                        }
                        // The session waits rather than editing the shared checkout;
                        // see `draw_claude`.
                        Err(e) => {
                            let msg = format!("Couldn't create {key}'s worktree: {e}");
                            tv.t.pending_worktree.get_or_insert_default().error = Some(e);
                            msg
                        }
                    });
                }
                Msg::Removed(res) => {
                    toast = Some(res.unwrap_or_else(|e| format!("Kept the worktree: {e}")));
                }
                Msg::Close(id, req) => self.closing.push_back((id, req)),
                Msg::Closed(text) => toast = Some(text),
                Msg::Copied(text, copied, label) => {
                    if copied == selection::Copied::Osc52 {
                        use base64::Engine;
                        self.clipboard_out
                            .push(base64::engine::general_purpose::STANDARD.encode(&text));
                    }
                    toast = Some(match label {
                        Some(what) => selection::described(&copied, &what),
                        None => selection::toast(&copied, &text),
                    });
                }
                Msg::Toast(msg) => toast = Some(msg),
                Msg::PrefixTip(tip) => {
                    self.prefs.prefix_tip_shown = true;
                    self.save();
                    // Shown once, at start, so it stays longer than a toast's 4s.
                    self.toast = Some((tip, Instant::now() + Duration::from_secs(8)));
                }
                Msg::Gh(repo, gh) => _ = self.github.insert(repo, gh),
                Msg::Keychain(k) => self.keychain = Some(k),
                Msg::Scanned(id, launch, kind, found) => self.scanned(&id, &launch, kind, found),
                Msg::Token(source, token) => {
                    self.trackers.token_read(&source, token, &self.config, &self.projects);
                    let uses = |app: &Self, t: &Ticket| {
                        let tracker =
                            app.project_of(t).and_then(|p| app.projects[p].tracker.as_ref());
                        tracker
                            .and_then(|t| app.config.account_for(t))
                            .is_some_and(|i| app.config.accounts[i].token == source)
                    };
                    let stale: Vec<usize> = (0..self.tickets.len())
                        .filter(|&i| uses(self, &self.tickets[i].t))
                        .collect();
                    for i in stale {
                        self.tickets[i].jira.request(false);
                    }
                }
                Msg::OpenIssues(listing, res) => self.open_issues_listed(listing, res),
                Msg::Choices(id, got) => self.choices_listed(id, got),
                Msg::Checked(id, check) => {
                    let form = match &mut self.mode {
                        Mode::Form(f) => f,
                        Mode::Settings(s) => &mut s.form,
                        _ => continue,
                    };
                    if form.checking != Some(id) {
                        continue;
                    }
                    form.checking = None;
                    form.checked = Some(check);
                    self.submit_form();
                }
            }
        }
        if changed {
            self.save();
        }
        if let Some(t) = toast {
            self.toast(t);
        }
    }

    pub(super) fn schedule_fetches(&mut self) {
        let access: Vec<(Access, Option<github::Closes>)> =
            self.tickets.iter().map(|tv| (self.access(&tv.t), self.closes(&tv.t))).collect();
        for ((i, tv), (access, closes)) in self.tickets.iter_mut().enumerate().zip(access) {
            let selected = i == self.sel;
            if tv.t.agent.is_some() {
                continue;
            }
            if let Access::Client(client) = access
                && client.remote()
                && tv.jira.due(if selected { JIRA_SELECTED } else { JIRA_BACKGROUND })
            {
                self.fetches += 1;
                let n = self.fetches;
                tv.jira.start(n);
                let (repo, key, tx) = (tv.t.repo.clone(), tv.t.key.clone(), self.tx.clone());
                std::thread::spawn(move || {
                    let res = client.fetch(&key, Text::Plain);
                    let _ = tx.send(Msg::Jira(n, (repo, key), res));
                });
            }
            if self.github.get(&tv.t.repo).is_none_or(|g| *g == github::Gh::Ready)
                && tv.pr.due(if selected { GH_SELECTED } else { GH_BACKGROUND })
            {
                self.fetches += 1;
                let n = self.fetches;
                tv.pr.start(n);
                let (repo, key, branch, tx) =
                    (tv.t.repo.clone(), tv.t.key.clone(), tv.t.branch.clone(), self.tx.clone());
                std::thread::spawn(move || {
                    let (res, wt) = pr_lookup(&repo, &key, branch, closes.as_ref());
                    let _ = tx.send(Msg::Pr(n, (repo, key), res, wt));
                });
            }
        }
    }

    pub(super) fn refresh_selected(&mut self) {
        // A token that failed (a locked password manager, say) is read again.
        self.rebuild_clients();
        if let Some(tv) = self.tickets.get_mut(self.sel) {
            tv.jira.request(true);
            tv.pr.request(true);
        }
        let repos = self.selected().map(|tv| vec![tv.t.repo.clone()]).unwrap_or_default();
        self.check_github(repos);
    }

    /// Checks again whether these folders can show PRs, in the background: gh may have been
    /// installed or logged in, or a remote added, since.
    pub(super) fn check_github(&self, repos: Vec<PathBuf>) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            for repo in repos {
                let gh = github::probe(&repo);
                let _ = tx.send(Msg::Gh(repo, gh));
            }
        });
    }

    /// Whether the folder can show PRs. Not yet checked counts as ready: a failed fetch
    /// says otherwise.
    pub fn gh(&self, repo: &Path) -> github::Gh {
        self.github.get(repo).copied().unwrap_or(github::Gh::Ready)
    }

    /// Creates the ticket's pending worktree in the background; its session waits for it. The
    /// tracker fetch for the branch name fills the tracker panel too.
    pub(super) fn create_worktree(&mut self, p: usize, i: usize) {
        let access = self.access(&self.tickets[i].t);
        let later = match access {
            Access::Reading => self.trackers.access_later(&self.config, &self.projects[p]),
            _ => None,
        };
        self.creations += 1;
        let n = self.creations;
        self.fetches += 1;
        let fetch = self.fetches;
        let tv = &mut self.tickets[i];
        tv.creating = true;
        tv.creation_id = n;
        let base = tv.t.pending_worktree.as_ref().and_then(|w| w.base.clone());
        let prefs = &self.projects[p].worktrees;
        let provider = self.projects[p].tracker.as_ref().map(|t| t.provider);
        let (key, repo, tx) = (tv.t.key.clone(), tv.t.repo.clone(), self.tx.clone());
        let folder = worktree::folder_path(prefs.folder_format(), &repo, &key);
        let format = prefs.branch_format().to_string();
        match &access {
            Access::Client(c) if c.remote() => tv.jira.start(fetch),
            // With the token still being read there may be no fetch, so no spinner; and a fetch
            // in flight keeps its claim, since its reply is what clears `loading`.
            _ if !tv.jira.loading => tv.jira.expect(fetch),
            _ => {}
        }
        tv.creation = Some(std::thread::spawn(move || {
            let issue = match later.map_or(access, |f| f()) {
                Access::Client(client) => Some(client.fetch(&key, Text::Plain)),
                _ => None,
            };
            let (title, tracker_branch) = match &issue {
                Some(Ok(i)) => (i.summary.clone(), i.branch.clone()),
                _ => Default::default(),
            };
            if let Some(res) = issue {
                let _ = tx.send(Msg::Jira(fetch, (repo.clone(), key.clone()), res));
            }
            let branch = worktree::branch_name(&format, provider, &key, &title, &tracker_branch);
            let res =
                worktree::create(&repo, &folder, &branch, base.as_ref().map(|b| b.branch.as_str()));
            let _ = tx.send(Msg::Worktree(n, (repo, key), res, base));
        }));
    }

    /// Starts creating the worktrees still pending: asked for before taskman last quit, or retried
    /// after a failure.
    pub(super) fn resume_worktrees(&mut self) {
        for i in 0..self.tickets.len() {
            let tv = &self.tickets[i];
            if tv.creating || tv.t.pending_worktree.as_ref().is_none_or(|w| w.error.is_some()) {
                continue;
            }
            match self.project_of(&tv.t) {
                Some(p) => self.create_worktree(p, i),
                None => {
                    let w = self.tickets[i].t.pending_worktree.get_or_insert_default();
                    w.error = Some("its project is gone".into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::ago;

    #[test]
    fn refreshes_remote_data_when_due_and_keeps_the_last_good_value() {
        let minute = Duration::from_secs(60);
        let mut r: Remote<u32> = Remote::new();
        assert!(r.due(minute), "never fetched");
        r.start(1);
        assert!(
            r.loading && r.loud,
            "the first load shows a spinner: there's nothing else to show"
        );
        assert!(!r.due(Duration::ZERO), "not while a fetch runs");
        r.finish(Ok(1));
        assert_eq!((r.data, r.error.as_deref(), r.loading), (Some(1), None, false));
        assert!(!r.due(minute) && r.due(Duration::ZERO));
        // Routine refreshes stay quiet, and a failed one keeps the value and says why.
        r.start(2);
        assert!(!r.loud);
        r.finish(Err("offline".into()));
        assert_eq!((r.data, r.error.as_deref()), (Some(1), Some("offline")));
        // Asked for, it's due at once; a loud request shows a spinner, once.
        r.request(false);
        assert!(r.due(minute));
        r.request(true);
        r.start(3);
        assert!(r.loud);
        r.finish(Ok(2));
        assert_eq!((r.data, r.error.as_deref()), (Some(2), None));
        r.start(4);
        assert!(!r.loud);
        r.finish(Ok(2));
        // Switching to a ticket refreshes only data older than asked.
        r.request_if_older_than(minute, true);
        assert!(!r.due(minute));
        r.fetched_at = ago(120.0);
        r.request_if_older_than(minute, true);
        assert!(r.due(minute));
    }

    #[test]
    fn takes_only_the_latest_fetches_reply() {
        let mut r: Remote<u32> = Remote::new();
        assert!(!r.expects(1), "nothing started: no reply is wanted");
        r.start(1);
        assert!(r.expects(1));
        // Another fetch started over it (a close's lookup, say): the first's reply is stale.
        r.start(2);
        assert!(!r.expects(1) && r.expects(2));
        // A fetch that may never report is expected without a spinner.
        r.finish(Ok(1));
        r.expect(3);
        assert!(!r.loading && r.expects(3) && !r.expects(2));
        // Reset (the tracker changed), nothing from before is wanted.
        r = Remote::new();
        assert!(!r.expects(3));
    }
}
