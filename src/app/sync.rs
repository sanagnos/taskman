//! Work in the background: tracker and PR fetches on their schedule (`Remote`), worktree creation,
//! and the messages threads send back (`Msg`), which `drain_messages` applies on each tick.

use super::forms::Check;
use super::{App, Id, Level, Mode};
use crate::store::{self, Base, Ticket};
use crate::tracker::{Access, Issue, Text};
use crate::{add, github, selection, worktree};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
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
    /// A form's check finished: the form's `checking` id and what it found, or Err when it
    /// panicked.
    Checked(u64, Result<Check, String>),
    /// Text went to the clipboard, or no command could take it; with what the toast calls it.
    Copied(String, selection::Copied, Option<String>),
    /// A notification no desktop command showed: its text, for the terminal (`notify::escape`).
    Notify(String),
    Toast(Level, String),
    /// What a project form's account can see to pick the ticket prefix from: the listing's id
    /// (`Choices`), and the list.
    Choices(u64, Result<Vec<crate::tracker::Choice>, String>),
    /// A project's open issues, for the Add form: the request that listed them, and the list.
    OpenIssues(super::add_form::Listing, Result<Vec<crate::tracker::Listed>, String>),
    /// The multiplexer taskman runs in has the same prefix (`prefix_tip`).
    PrefixTip(String),
    /// The OS keychain to save tokens to, or None: there's none.
    Keychain(Option<crate::keychain::Keychain>),
    /// A session's process tree was looked at (`App::scan`): its ticket, the start it belongs to,
    /// why, and the pids under its CLI.
    Scanned(Id, String, super::sleep::Scan, Result<std::collections::HashSet<u32>, String>),
    /// `claude stop` finished for a session blocked by a background copy: its ticket, the start
    /// it belongs to and the outcome.
    BackgroundStopped(Id, String, Result<(), String>),
}

/// Runs the work of a thread whose message something waits for: a panel loading, a form checking,
/// a session waiting for its worktree. A panic in the work (a bug: in a parser, say) would end the
/// thread without a word and leave that waiting for good, with `r` doing nothing, so the thread
/// reports it instead (`util::caught`). The panic hook has written it to panics.log.
#[derive(Clone)]
pub(super) struct Reporter {
    tx: mpsc::Sender<Msg>,
    #[cfg(test)]
    panics: bool,
}

impl Reporter {
    /// Runs `f`; Err when it panicked.
    pub(super) fn caught<T>(&self, f: impl FnOnce() -> T) -> Result<T, String> {
        #[cfg(test)]
        let panics = self.panics;
        crate::util::caught(move || {
            #[cfg(test)]
            if panics {
                std::panic::panic_any(crate::util::TestPanic);
            }
            f()
        })
    }

    /// `caught`, with an error toast saying that taskman hit a bug `what` (`"fetching ENG-52"`)
    /// when it panicked. Sent before the caller's own message, so the toast is in by the time
    /// what waited stops waiting.
    pub(super) fn reported<T>(&self, what: &str, f: impl FnOnce() -> T) -> Result<T, String> {
        self.caught(f).inspect_err(|_| {
            let msg = format!("taskman hit a bug {what}; details in {}", store::panics_log());
            let _ = self.tx.send(Msg::Toast(Level::Error, msg));
        })
    }

    pub(super) fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
    }

    /// Runs `work` on a thread, which sends the message it returns; when it panicked, the one
    /// `failed` makes of the error instead (see `reported`).
    pub(super) fn spawn(
        self,
        what: String,
        work: impl FnOnce() -> Msg + Send + 'static,
        failed: impl FnOnce(String) -> Msg + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let msg = self.reported(&what, work).unwrap_or_else(failed);
            let _ = self.tx.send(msg);
        })
    }

    /// Looks up the PRs of tickets in `repo` (`pr_lookups`) and sends each one's `Msg::Pr` with
    /// its lookup's id; when the lookup panicked, the failure for each of them.
    pub(super) fn send_lookups(
        &self,
        repo: &Path,
        ids: Vec<(u64, String)>,
        wanted: Vec<github::Wanted>,
    ) {
        let keys: Vec<&str> = ids.iter().map(|(_, k)| k.as_str()).collect();
        let what = format!("looking up the PRs of {}", keys.join(", "));
        let found = match self.reported(&what, || pr_lookups(repo, wanted)) {
            Ok(found) => found,
            Err(e) => ids.iter().map(|_| (Err(e.clone()), None)).collect(),
        };
        for ((n, key), (res, wt)) in ids.into_iter().zip(found) {
            self.send(Msg::Pr(n, (repo.to_path_buf(), key), res, wt));
        }
    }
}

/// What a PR lookup found for a ticket, and the worktree git found for it.
pub(super) type LookedUp = (Result<github::Lookup, String>, Option<(PathBuf, String)>);

/// Looks up the PRs of tickets in `repo`, in order: one that `closes` each one's issue, else on
/// its worktree's branch, else on the branch it remembers; each with the worktree git found for
/// it. git lists the worktrees once, and gh lists the PRs of them all in one query.
pub(super) fn pr_lookups(repo: &Path, mut wanted: Vec<github::Wanted>) -> Vec<LookedUp> {
    let list = github::worktrees(repo).unwrap_or_default();
    let worktrees: Vec<_> = wanted.iter().map(|w| github::pick_worktree(&list, &w.key)).collect();
    for (w, wt) in wanted.iter_mut().zip(&worktrees) {
        if let Some((_, b)) = wt {
            w.branch = Some(b.clone());
        }
    }
    let found = github::fetch_all(repo, &wanted, |args| github::gh(repo, args));
    found.into_iter().zip(worktrees).collect()
}

impl App {
    /// Starts the threads that report back even when their work panics.
    pub(super) fn reporter(&self) -> Reporter {
        Reporter {
            tx: self.tx.clone(),
            #[cfg(test)]
            panics: self.panics,
        }
    }

    /// Applies what the background threads sent. On the UI thread, each tick, and at a quit that
    /// waits for a worktree without the event loop (`main::shutdown`).
    pub fn drain_messages(&mut self) {
        let mut changed = false;
        let mut toasts = Vec::new();
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
                        changed |= self.github.insert(tv.t.repo.clone(), gh) != Some(gh);
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
                            toasts.push((Level::Warn, t));
                        }
                        continue;
                    };
                    let key = self.qualified(&self.tickets[i].t);
                    let tv = &mut self.tickets[i];
                    tv.creating = false;
                    tv.creation = None;
                    changed = true;
                    toasts.push(match res {
                        Ok(c) => {
                            let level = if c.note.is_some() { Level::Warn } else { Level::Info };
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
                            // Not when the folder already was the branch's worktree, unless
                            // taskman had started it there: someone else's, which closing the
                            // ticket must leave.
                            tv.t.created_worktree = c.made;
                            tv.t.pending_worktree = None;
                            (level, msg)
                        }
                        // The session waits rather than editing the shared checkout; see
                        // `draw_claude`. git leaves no worktree when it fails, so whatever is in
                        // the folder by the next try isn't taskman's.
                        Err(e) => {
                            let msg = format!("Couldn't create {key}'s worktree: {e}");
                            let pending = tv.t.pending_worktree.get_or_insert_default();
                            (pending.error, pending.making) = (Some(e), None);
                            (Level::Error, msg)
                        }
                    });
                }
                Msg::Removed(res) => {
                    toasts.push(match res {
                        Ok(done) => (Level::Info, done),
                        Err(e) => (Level::Warn, format!("Kept the worktree: {e}")),
                    });
                }
                Msg::Close(id, req) => self.closing.push_back((id, req)),
                Msg::Closed(text) => toasts.push((Level::Info, text)),
                Msg::Notify(text) => {
                    let tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
                    self.notify_out.push(crate::notify::escape(&text, tmux));
                }
                Msg::Copied(text, copied, label) => {
                    if copied == selection::Copied::Osc52 {
                        use base64::Engine;
                        self.clipboard_out
                            .push(base64::engine::general_purpose::STANDARD.encode(&text));
                    }
                    toasts.push((
                        Level::Info,
                        match label {
                            Some(what) => selection::described(&copied, &what),
                            None => selection::toast(&copied, &text),
                        },
                    ));
                }
                Msg::Toast(level, msg) => toasts.push((level, msg)),
                Msg::PrefixTip(tip) => {
                    self.prefs.prefix_tip_shown = true;
                    self.save();
                    // Shown once, at start, so it stays longer than a toast's 4s.
                    self.toast_for(tip, Duration::from_secs(8));
                }
                // Saved, for the next start's first frame.
                Msg::Gh(repo, gh) => changed |= self.github.insert(repo, gh) != Some(gh),
                Msg::Keychain(k) => self.keychain_probed(k),
                Msg::Scanned(id, launch, kind, found) => self.scanned(&id, &launch, kind, found),
                Msg::BackgroundStopped(id, launch, res) => {
                    self.background_stopped(&id, &launch, res);
                }
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
                    match check {
                        Ok(check) => {
                            form.checked = Some(check);
                            self.submit_form();
                        }
                        // Enter checks again.
                        Err(e) => form.error = Some(e),
                    }
                }
            }
        }
        if changed {
            self.save();
        }
        for (level, t) in toasts {
            self.toast_at(level, t);
        }
    }

    pub(super) fn schedule_fetches(&mut self) {
        let access: Vec<Access> = self.tickets.iter().map(|tv| self.access(&tv.t)).collect();
        let report = self.reporter();
        for ((i, tv), access) in self.tickets.iter_mut().enumerate().zip(access) {
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
                let id: Id = (tv.t.repo.clone(), tv.t.key.clone());
                let (what, failed) = (format!("fetching {}", id.1), id.clone());
                report.clone().spawn(
                    what,
                    move || {
                        let res = client.fetch(&id.1, Text::Plain).map_err(String::from);
                        Msg::Jira(n, id, res)
                    },
                    move |e| Msg::Jira(n, failed, Err(e)),
                );
            }
        }
        for (repo, batch) in self.due_lookups() {
            let (mut ids, mut wanted) = (Vec::new(), Vec::new());
            for i in batch {
                let closes = self.closes(&self.tickets[i].t);
                self.fetches += 1;
                let n = self.fetches;
                let tv = &mut self.tickets[i];
                tv.pr.start(n);
                ids.push((n, tv.t.key.clone()));
                let branch = tv.t.branch.clone();
                wanted.push(github::Wanted { key: tv.t.key.clone(), branch, closes });
            }
            let report = report.clone();
            std::thread::spawn(move || report.send_lookups(&repo, ids, wanted));
        }
    }

    /// The tickets whose PRs to look up now, by repository: those due, and in a repository with
    /// one due, those three quarters of the way there. They share the repository's query, so
    /// tickets that fell out of step (refreshed when selected, or on a hook's signal) fall back in.
    fn due_lookups(&self) -> Vec<(PathBuf, Vec<usize>)> {
        let every = |i: usize| if i == self.sel { GH_SELECTED } else { GH_BACKGROUND };
        let can = |tv: &super::TicketView| {
            tv.t.agent.is_none()
                && self.github.get(&tv.t.repo).is_none_or(|g| *g == github::Gh::Ready)
        };
        let mut out: Vec<(PathBuf, Vec<usize>)> = Vec::new();
        for (i, tv) in self.tickets.iter().enumerate() {
            if can(tv) && tv.pr.due(every(i)) && !out.iter().any(|(r, _)| *r == tv.t.repo) {
                out.push((tv.t.repo.clone(), Vec::new()));
            }
        }
        for (i, tv) in self.tickets.iter().enumerate() {
            if let Some((_, batch)) = out.iter_mut().find(|(r, _)| *r == tv.t.repo)
                && can(tv)
                && tv.pr.due(every(i) * 3 / 4)
            {
                batch.push(i);
            }
        }
        out
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
            for (repo, gh) in github::probe(&repos) {
                let _ = tx.send(Msg::Gh(repo, gh));
            }
        });
    }

    /// Whether the folder can show PRs. Not yet checked counts as ready, for fetches and worktrees:
    /// a failed fetch says otherwise. The PR panel waits for the check (`panels`).
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
        let prefs = &self.projects[p].worktrees;
        let provider = self.projects[p].tracker.as_ref().map(|t| t.provider);
        let (key, repo, tx) = (tv.t.key.clone(), tv.t.repo.clone(), self.tx.clone());
        let folder = worktree::folder_path(prefs.folder_format(), &repo, &key);
        let format = prefs.branch_format().to_string();
        // Saved below, before git can run: git runs in its own process group, so a quit doesn't
        // stop it, and the next try finds the worktree it finished.
        let pending = tv.t.pending_worktree.get_or_insert_default();
        let base = pending.base.clone();
        let ours = pending.making.as_ref().is_some_and(|f| worktree::same_path(f, &folder));
        pending.making = Some(folder.clone());
        let started = match &access {
            Access::Client(c) if c.remote() => {
                tv.jira.start(fetch);
                true
            }
            // With the token still being read there may be no fetch, so no spinner; and a fetch
            // in flight keeps its claim, since its reply is what clears `loading`.
            _ if !tv.jira.loading => {
                tv.jira.expect(fetch);
                false
            }
            _ => false,
        };
        self.save();
        let report = self.reporter();
        let thread = std::thread::spawn(move || {
            let mut fetched = false;
            // No toast of its own: the failed worktree's says it (`drain_messages`).
            let res = report.caught(|| {
                let issue = match later.map_or(access, |f| f()) {
                    Access::Client(client) => Some(
                        report
                            .reported(&format!("fetching {key}"), || {
                                client.fetch(&key, Text::Plain)
                            })
                            .and_then(|r| r.map_err(String::from)),
                    ),
                    _ => None,
                };
                let (title, tracker_branch) = match &issue {
                    Some(Ok(i)) => (i.summary.clone(), i.branch.clone()),
                    _ => Default::default(),
                };
                if let Some(res) = issue {
                    let _ = tx.send(Msg::Jira(fetch, (repo.clone(), key.clone()), res));
                    fetched = true;
                }
                let branch =
                    worktree::branch_name(&format, provider, &key, &title, &tracker_branch);
                let base = base.as_ref().map(|b| b.branch.as_str());
                worktree::create(&repo, &folder, &branch, base, ours)
            });
            // A panic before the fetch replied would leave its spinner on.
            if let Err(e) = &res
                && started
                && !fetched
            {
                let _ = tx.send(Msg::Jira(fetch, (repo.clone(), key.clone()), Err(e.clone())));
            }
            let _ = tx.send(Msg::Worktree(n, (repo, key), res.and_then(|r| r), base));
        });
        self.tickets[i].creation = Some(thread);
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
    fn looks_up_a_repositorys_prs_together() {
        let mut app = crate::app::test_support::acme_app();
        let acme = PathBuf::from("/src/acme");
        let i = |app: &App, key: &str| app.tickets.iter().position(|tv| tv.t.key == key).unwrap();
        let (eng52, eng53, eng61, eng70) =
            (i(&app, "ENG-52"), i(&app, "ENG-53"), i(&app, "ENG-61"), i(&app, "ENG-70"));
        assert_eq!(app.sel, eng52);
        // Never looked up: every ticket at once, and no agent.
        assert_eq!(app.due_lookups(), [(acme.clone(), vec![eng52, eng53, eng61, eng70])]);
        let looked =
            |app: &mut App, i: usize, secs: f32| app.tickets[i].pr.last_attempt = ago(secs);
        for t in [eng53, eng61, eng70] {
            looked(&mut app, t, 10.0);
        }
        looked(&mut app, eng52, 5.0);
        assert!(app.due_lookups().is_empty());
        // The selected ticket is due after 20 s; others join it from 45 s, three quarters of
        // their minute, so they fall back in step.
        looked(&mut app, eng52, 25.0);
        assert_eq!(app.due_lookups(), [(acme.clone(), vec![eng52])]);
        looked(&mut app, eng61, 50.0);
        assert_eq!(app.due_lookups(), [(acme.clone(), vec![eng52, eng61])]);
        // A background ticket that's due takes the selected one along from 15 s.
        looked(&mut app, eng52, 16.0);
        looked(&mut app, eng61, 61.0);
        assert_eq!(app.due_lookups(), [(acme.clone(), vec![eng52, eng61])]);
        looked(&mut app, eng52, 5.0);
        assert_eq!(app.due_lookups(), [(acme.clone(), vec![eng61])]);
        // Not while one is in flight, nor while gh can't answer.
        app.tickets[eng61].pr.start(1);
        assert!(app.due_lookups().is_empty());
        app.tickets[eng53].pr.request(false);
        app.github.insert(acme, github::Gh::LoggedOut);
        assert!(app.due_lookups().is_empty());
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
