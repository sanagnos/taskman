//! One ticket or agent as the TUI holds it (`TicketView`): its session, its state, its fetched
//! tracker and PR panels, and which of their comments are new.

use super::sleep::Watch;
use super::sync::STALE;
use super::{AgentState, Gone, Remote, Status};
use crate::github::{self, Pr, PrState};
use crate::session::{End, Parser, Reaper, Session};
use crate::status_file::Record;
use crate::store::{self, Ticket};
use crate::tracker::Issue;
use crate::worktree;
use crate::{resume, util};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

/// A session that runs, or ran, in another terminal instead of the pane (`ctrl+a t`, `taskman
/// resume`). It's never started in the pane by itself while it's away: Enter brings it back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Away {
    /// `ctrl+a t` asked the CLI to quit; it's handed off once it has.
    Leaving(Instant),
    /// Handed off, and `taskman resume` hasn't picked it up.
    HandedOff,
    /// Running in another terminal; its hooks still report its state.
    Outside,
    /// It ran in another terminal and ended there.
    Back,
}

pub struct TicketView {
    pub t: Ticket,
    pub session: Option<Session>,
    /// Its last session's CLI, still being hung up and reaped (`stop_session`). Until it's gone,
    /// no session starts in its place, its claim isn't let go and its folder isn't removed: see
    /// "Stopping a session" in docs/design.md.
    pub(super) reaper: Option<Reaper>,
    pub status: Status,
    pub jira: Remote<Issue>,
    pub pr: Remote<Option<Pr>>,
    /// When no PR is shown: the newest one closed without merging, for a note.
    pub closed_pr: Option<u64>,
    pub jira_scroll: u16,
    pub gh_scroll: u16,
    /// Modification times of the hook's "Claude touched Jira / GitHub" signal files.
    pub(super) jira_signal: Option<SystemTime>,
    pub(super) gh_signal: Option<SystemTime>,
    /// A Local files ticket's file and its modification time when last read; the panel rereads
    /// it when either changes.
    pub local_file: Option<(PathBuf, Option<SystemTime>)>,
    /// Its worktree is being created; the session starts once it's there.
    pub creating: bool,
    /// The thread creating it.
    pub(super) creation: Option<std::thread::JoinHandle<()>>,
    /// The creation's id, which its result carries (`Msg::Worktree`): a result for an earlier
    /// creation, from before the ticket closed and was added again, isn't this one's.
    pub(super) creation_id: u64,
    /// Added by `taskman add` and not selected since; the sidebar marks it.
    pub new: bool,
    /// An agent `taskman add` just added: its session starts without being selected, so the
    /// agent that added it can message it.
    pub(super) start: bool,
    /// The name its session started with (`--name`), which can be the plain key while another
    /// project's ticket has the same key (see `store::session_name`).
    pub(super) name: Option<String>,
    /// The claim on its session while the pane runs it, so `taskman resume` can't start a second
    /// copy.
    pub(super) hold: Option<resume::Hold>,
    /// Which start of its session the pane runs (`launch::launch_id`), which its hooks' records
    /// name; see `takes`.
    pub(super) launch: Option<String>,
    /// Why its session didn't start the last time it was tried: the owner lock couldn't be taken
    /// (`start_session`), as "<file>: <error>". Enter tries again.
    pub lock_error: Option<String>,
    /// Its session didn't start the last time it was tried because its folder is gone
    /// (`start_session`). Enter makes a ticket's worktree again (`remake_worktree`), or tries
    /// again.
    pub gone: Option<Gone>,
    pub away: Option<Away>,
    /// Its resume was refused because Claude runs the conversation in the background; Enter
    /// stops that copy and resumes here.
    pub background: Option<Blocked>,
    /// The ended session's screen was looked at for `background`.
    pub(super) screen_read: bool,
    /// Why its last session ended, or didn't start, for the pane; None once another is started or
    /// it's stopped.
    pub end: Option<End>,
    /// The ended session's last screen, once the session itself is let go (`release_ended`).
    pub(super) last_screen: Option<Arc<Mutex<Parser>>>,
    /// Another session's conversation id its hooks reported and it wasn't given, once told.
    pub(super) switched: Option<String>,
    /// Stopped to save memory (`sleep`), its state as it was; selecting it starts it again. Not
    /// saved: after a restart nothing runs until selected anyway.
    pub asleep: bool,
    /// What `sleep` looks at: when it was last selected and what runs under its CLI.
    pub(super) watch: Watch,
    /// Comment ids (and `c:`/`r:` PR ids, as in `seen_gh`) that were new when the ticket was
    /// selected or arrived while it was, read by then: the panels keep marking them `●` until
    /// the ticket is left (`leave`). Not saved.
    fresh_jira: Vec<String>,
    fresh_gh: Vec<String>,
    /// When its session's refused clipboard requests were last toasted, by why (`take_clipboards`).
    pub(super) clipboard_warned: Vec<(crate::session::Refused, Instant)>,
}

/// A session that ended because its conversation runs as a Claude background session.
pub struct Blocked {
    /// The job's id, as `background::job_id` took it from the screen.
    pub id: String,
    /// What Claude said, for the pane (untrusted: print it as it is, cleaned).
    pub message: String,
    /// `claude stop` is running.
    pub stopping: bool,
    /// Why it failed the last time.
    pub error: Option<String>,
}

/// What adding a ticket or agent found: it was already open, or is new; its index either way.
pub(super) enum Added {
    Open(usize),
    New(usize),
}

impl TicketView {
    pub(super) fn new(t: Ticket) -> Self {
        Self {
            t,
            session: None,
            reaper: None,
            status: Status::new(),
            jira: Remote::new(),
            pr: Remote::new(),
            closed_pr: None,
            jira_scroll: 0,
            gh_scroll: 0,
            jira_signal: None,
            gh_signal: None,
            local_file: None,
            creating: false,
            creation: None,
            creation_id: 0,
            new: false,
            start: false,
            name: None,
            hold: None,
            launch: None,
            lock_error: None,
            gone: None,
            away: None,
            background: None,
            screen_read: false,
            end: None,
            last_screen: None,
            switched: None,
            asleep: false,
            watch: Watch::new(Instant::now()),
            fresh_jira: Vec::new(),
            fresh_gh: Vec::new(),
            clipboard_warned: Vec::new(),
        }
    }

    /// Its session waits for a worktree: being created, or failed until the user retries or
    /// chooses the main checkout.
    pub fn waiting(&self) -> bool {
        self.creating || self.t.pending_worktree.is_some()
    }

    /// Stops its session, if it has one: the CLI is hung up and reaped on a thread
    /// (`Session::stop`), kept as `reaper` while it runs. A session only starts once the last
    /// one's CLI is gone (`start_session`), so there's at most one to keep.
    pub(super) fn stop_session(&mut self) {
        self.rouse();
        self.background = None;
        self.screen_read = false;
        self.end = None;
        self.last_screen = None;
        if let Some(reaper) = self.session.take().and_then(Session::stop) {
            self.reaper = Some(reaper);
        }
    }

    /// Lets go of its ended session but for its last screen, which the pane goes on showing with
    /// why it ended: the history is most of a session's memory, tens of MB in a long one.
    pub(super) fn release_ended(&mut self) {
        let Some(s) = self.session.take() else { return };
        self.last_screen = Some(Arc::new(Mutex::new(s.last_screen())));
        if let Some(reaper) = s.stop() {
            self.reaper = Some(reaper);
        }
    }

    /// What the pane shows of the session: its screen while it runs and once it has ended (the
    /// last one, once it's let go). None when a message stands in for it.
    pub fn screen(&self) -> Option<&Arc<Mutex<Parser>>> {
        if self.background.is_some() {
            return None;
        }
        match &self.session {
            Some(s) => Some(&s.parser),
            None => self.last_screen.as_ref(),
        }
    }

    /// Its session has ended (`end` says why, once the poll has seen it), and the pane shows its
    /// last screen.
    pub fn ended(&self) -> bool {
        match &self.session {
            Some(s) => s.exited(),
            None => self.last_screen.is_some(),
        }
    }

    /// Puts its session to sleep: stopped as a restart stops it, but its state stays as it was
    /// and `taskman status` says it's asleep, until it starts again. The marker is written first,
    /// so an agent never reads the state the stopped CLI's SessionEnd hook leaves.
    pub(super) fn sleep(&mut self) {
        let marker = asleep_marker(&self.t);
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&marker, b"");
        self.stop_session();
        self.asleep = true;
    }

    /// No longer asleep: it started again, was handed off or stopped some other way.
    pub(super) fn rouse(&mut self) {
        if std::mem::take(&mut self.asleep) {
            let _ = std::fs::remove_file(asleep_marker(&self.t));
        }
    }

    /// Whether its session is down and the user hasn't asked for that: not started yet or asleep.
    pub(super) fn dormant(&self) -> bool {
        self.session.is_none() && (self.status.state == AgentState::NotStarted || self.asleep)
    }

    /// Whether its last session's CLI is still on its way out: hung up, but not reaped yet.
    pub fn reaping(&self) -> bool {
        self.reaper.as_ref().is_some_and(|r| !r.done())
    }

    /// Takes its reaper, for whoever must wait for the CLI off the UI thread.
    pub(super) fn take_reaper(&mut self) -> Option<Reaper> {
        self.reaper.take().filter(|r| !r.done())
    }

    /// Handed off (`ctrl+a t`) or asleep: lets its claim go once its CLI is gone, not before, so
    /// `taskman resume` can't start a second copy on the conversation the pane's CLI is still
    /// ending. A hand-off's owner file says `handed-off` already, which held reads as
    /// `Owner::Stopping`. From `handed_off` and then from every poll, until it's let go.
    pub(super) fn release_handed_off(&mut self) {
        if (self.away == Some(Away::HandedOff) || self.asleep) && !self.reaping() {
            self.hold = None;
        }
    }

    fn jira_ids(&self) -> Option<Vec<String>> {
        self.jira.data.as_ref().map(|i| i.comments.iter().map(|c| c.id.clone()).collect())
    }

    fn gh_ids(&self) -> Option<Vec<String>> {
        self.pr.data.as_ref().map(|pr| match pr {
            Some(pr) => pr
                .comments
                .iter()
                .map(|c| format!("c:{}", c.id))
                .chain(pr.latest_reviews().into_iter().map(|r| format!("r:{}", r.id)))
                .collect(),
            None => Vec::new(),
        })
    }

    /// Whether the panels mark a Jira comment `●`: not read yet, or read on this visit.
    pub fn is_new_jira(&self, id: &str) -> bool {
        self.fresh_jira.iter().any(|s| s == id) || unseen(self.t.seen_jira.as_ref(), id)
    }

    /// `prefixed_id` is `c:<comment id>` or `r:<review id>`.
    pub fn is_new_gh(&self, prefixed_id: &str) -> bool {
        self.fresh_gh.iter().any(|s| s == prefixed_id)
            || unseen(self.t.seen_gh.as_ref(), prefixed_id)
    }

    /// Tracker comments, and PR comments and reviews, that arrived since the ticket was last
    /// selected.
    pub fn new_counts(&self) -> (usize, usize) {
        let count = |ids: Option<Vec<String>>, seen: Option<&Vec<String>>| {
            ids.unwrap_or_default().iter().filter(|id| unseen(seen, id)).count()
        };
        (
            count(self.jira_ids(), self.t.seen_jira.as_ref()),
            count(self.gh_ids(), self.t.seen_gh.as_ref()),
        )
    }

    /// The first fetch for a ticket counts everything already there as seen.
    pub(super) fn baseline_seen(&mut self) -> bool {
        let mut changed = false;
        if self.t.seen_jira.is_none()
            && let Some(ids) = self.jira_ids()
        {
            self.t.seen_jira = Some(ids);
            changed = true;
        }
        if self.t.seen_gh.is_none()
            && let Some(ids) = self.gh_ids()
        {
            self.t.seen_gh = Some(ids);
            changed = true;
        }
        changed
    }

    /// Marks every comment and review loaded as read, whatever the panels show: the ticket is
    /// selected. The ones that were new stay marked `●` until it's left. Returns whether
    /// anything changed, to save.
    pub(super) fn mark_seen(&mut self) -> bool {
        let (jira, gh) = (self.jira_ids(), self.gh_ids());
        mark(&mut self.t.seen_jira, &mut self.fresh_jira, jira)
            | mark(&mut self.t.seen_gh, &mut self.fresh_gh, gh)
    }

    /// Leaves the ticket: what arrived while it was selected is read too, and its `●` markers
    /// go. Returns whether anything changed, to save.
    pub(super) fn leave(&mut self) -> bool {
        let changed = self.mark_seen();
        self.clear_fresh();
        changed
    }

    /// Takes the `●` markers off what was read on this visit (`ctrl+a m`); false when none had
    /// one.
    pub(super) fn clear_fresh(&mut self) -> bool {
        let any = !self.fresh_jira.is_empty() || !self.fresh_gh.is_empty();
        self.fresh_jira.clear();
        self.fresh_gh.clear();
        any
    }

    /// Whether a status record is this session's. A CLI runs its SessionEnd hook on the hangup
    /// that stops it, so a record of an earlier start can land after a restart's new session is
    /// running, or after the ticket closed and was added again; taken, it would show the new
    /// session as ended, or hand the new ticket the old conversation to resume. So the hooks
    /// write the start they belong to (`TASKMAN_LAUNCH`), and the pane takes only its own start's
    /// records. Outside (`taskman resume`), or back from there, whatever the hooks write is the
    /// session's; with no session anywhere, nothing is.
    pub(super) fn takes(&self, rec: &Record) -> bool {
        match (&self.session, &rec.launch) {
            (Some(_), Some(launch)) => self.launch.as_ref() == Some(launch),
            // An older hook writes no start.
            (Some(_), None) => true,
            (None, _) => self.away.is_some(),
        }
    }

    /// What the session's hook reported, `selected` saying whether the user is looking at it.
    /// Returns whether it started waiting on the user (`App::started_waiting`).
    pub(super) fn hook_reported(&mut self, rec: Record, selected: bool) -> bool {
        let mut state = AgentState::from(rec.state);
        // Codex runs the hook before its reviewer, which may answer the request instead of the
        // user: working until the screen shows whose it is (`Status::review`).
        if state == AgentState::Needs && rec.permission() && self.t.cli.reviews_approvals() {
            state = AgentState::Working;
            self.status.hold();
        }
        let mut waits = false;
        if state != self.status.state {
            // Claude finished a turn: whatever it did may have changed the ticket or PR.
            if state == AgentState::Done {
                self.jira.request_if_older_than(STALE, false);
                self.pr.request_if_older_than(STALE, false);
            }
            if !selected && matches!(state, AgentState::Needs | AgentState::Done) {
                self.status.seen = false;
            }
            waits = state == AgentState::Needs;
            self.status.state = state;
            // A repeated "needs" (Claude's reminder after a prompt) keeps the first one's time.
            self.status.waiting_since =
                (state == AgentState::Needs).then(|| if rec.ts > 0 { rec.ts } else { util::now() });
        }
        self.status.permission = rec.permission();
        self.status.tool = rec.tool;
        self.status.input = rec.input;
        self.status.message = rec.message;
        waits
    }

    /// What a PR lookup found (`pr_lookup`), with the worktree git found for the ticket: shows
    /// the PR, adopts an open PR's branch (not a fork's, whose name its author chose) or forgets
    /// a remembered branch that's gone, and moves a
    /// session that never started into the worktree, unless taskman created one for the ticket
    /// already. Returns whether the ticket changed.
    pub(super) fn take_pr(
        &mut self,
        res: Result<github::Lookup, String>,
        worktree: Option<(PathBuf, String)>,
    ) -> bool {
        let mut changed = false;
        let res = res.map(|found| {
            // Adopt an open PR's branch; forget a remembered branch that's gone. A fork's branch
            // isn't the ticket's: anyone can open a PR that says it closes the issue.
            if let Some(p) = &found.pr
                && p.state == PrState::Open
                && !p.cross_repository
                && self.t.branch.is_none()
            {
                self.t.branch = Some(p.head.clone());
                changed = true;
            }
            if found.branch_gone && worktree.is_none() && self.t.branch.is_some() {
                self.t.branch = None;
                changed = true;
            }
            // Keep the note after the branch is forgotten, until a PR replaces it.
            if found.pr.is_some() {
                self.closed_pr = None;
            } else if found.closed.is_some() {
                self.closed_pr = found.closed;
            }
            found.pr
        });
        self.pr.finish(res);
        changed |= self.baseline_seen();
        if let Some((path, branch)) = worktree {
            // The worktree taskman created stays the ticket's, whatever git finds for the key
            // later (a workflow's `change/ENG-52`, which the lookup prefers): closing offers to
            // remove the ticket's worktree with its branch, and that must never be one taskman
            // didn't make. So neither the folder nor the branch moves there, and
            // `created_worktree` never comes to describe another folder.
            let another = self.t.created_worktree && !worktree::same_path(&self.t.cwd, &path);
            if !another && self.t.branch.as_deref() != Some(branch.as_str()) {
                self.t.branch = Some(branch);
                changed = true;
            }
            // A session that has never started can start in the worktree directly.
            if !another
                && self.session.is_none()
                && self.t.session_id.is_none()
                && self.t.cwd != path
            {
                self.t.cwd = path;
                changed = true;
            }
        }
        changed
    }

    /// Whether its session is running a turn or waiting on the user, which a restart would cut
    /// short: `prefix R` asks first.
    pub fn busy(&self) -> bool {
        self.session.as_ref().is_some_and(|s| !s.exited())
            && matches!(self.status.state, AgentState::Working | AgentState::Needs)
    }

    pub fn needs_attention(&self) -> bool {
        // Outside, it can still be waiting on you; handed off or back, it waits for Enter alone.
        match self.away {
            Some(Away::Outside) if self.status.state == AgentState::Exited => return false,
            Some(Away::Leaving(_) | Away::HandedOff | Away::Back) => return false,
            _ => {}
        }
        match self.status.state {
            AgentState::Needs => true,
            AgentState::Done | AgentState::Exited => !self.status.seen,
            _ => false,
        }
    }

    /// A title worth showing: the tracker summary, or Claude's window title without its status
    /// glyph. None when there's only something generic like "Claude Code".
    pub fn title(&self) -> Option<String> {
        if let Some(i) = &self.jira.data
            && !i.summary.trim().is_empty()
        {
            return Some(i.summary.clone());
        }
        let raw = self.window_title();
        let t = raw.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
        let generic = ["Claude Code", "Claude", "Continued conversation", "New conversation"];
        // Claude titles the window with the session's --name, which the card already shows.
        let own_name = t.eq_ignore_ascii_case(&self.t.key);
        (!t.is_empty() && !own_name && !generic.iter().any(|g| t.eq_ignore_ascii_case(g)))
            .then(|| t.to_string())
    }

    /// The session's window title, without the folder Codex adds: it titles the window
    /// "<thread name> | <folder>", or just the folder before the first turn.
    pub fn window_title(&self) -> String {
        let raw = self.session.as_ref().map(|s| s.title()).unwrap_or_default();
        let dir =
            self.t.cwd.file_name().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        strip_folder(&raw, &dir).to_string()
    }
}

/// Whether `id` arrived after `seen` was first filled; nothing is new before the first fetch.
fn unseen(seen: Option<&Vec<String>>, id: &str) -> bool {
    seen.is_some_and(|seen| !seen.iter().any(|s| s == id))
}

/// Adds the `ids` loaded to `seen`, and those it didn't have to `fresh`; a first fetch (`seen`
/// None) is all seen, none of it fresh. Added to, never replaced: a fetch that loads fewer (the
/// lookup settling on another PR that mentions the key for a while) would otherwise make
/// the ones it left out new again when they come back.
fn mark(seen: &mut Option<Vec<String>>, fresh: &mut Vec<String>, ids: Option<Vec<String>>) -> bool {
    let Some(ids) = ids else { return false };
    let Some(seen) = seen else {
        *seen = Some(ids);
        return true;
    };
    let new: Vec<String> = ids.into_iter().filter(|id| !seen.contains(id)).collect();
    if new.is_empty() {
        return false;
    }
    fresh.extend(new.iter().cloned());
    seen.extend(new);
    true
}

fn strip_folder<'a>(title: &'a str, dir: &str) -> &'a str {
    if !dir.is_empty() && title.trim() == dir {
        return "";
    }
    title.strip_suffix(&format!(" | {dir}")).unwrap_or(title)
}

/// The file beside a ticket's status file that says its session is asleep, which `taskman status`
/// reads: the hooks' file can't, as the stopped CLI's SessionEnd hook writes "ended" into it.
pub(crate) fn asleep_marker(t: &Ticket) -> PathBuf {
    store::status_file(&t.repo, &t.key).with_extension("asleep")
}

/// Whether closing `tv` may remove its worktree: one taskman created, never the main checkout.
pub(super) fn own_worktree(tv: &TicketView) -> bool {
    tv.t.created_worktree && tv.t.cwd != tv.t.repo
}

/// The head commit of the ticket's merged PR, which vouches for the commits it merged.
pub(super) fn merged_head(tv: &TicketView) -> Option<String> {
    match &tv.pr.data {
        Some(Some(pr)) if pr.state == PrState::Merged && !pr.head_oid.is_empty() => {
            Some(pr.head_oid.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{ago, ticket};
    use crate::github::Lookup;
    use crate::status_file::parse;
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn strips_codex_folder_from_titles() {
        assert_eq!(strip_folder("⠇ Fix login | acme", "acme"), "⠇ Fix login");
        assert_eq!(strip_folder("acme", "acme"), "");
        assert_eq!(strip_folder("✳ Fix login", "acme"), "✳ Fix login");
        assert_eq!(strip_folder("Fix login", ""), "Fix login");
    }

    fn pr(state: &str, head: &str) -> Pr {
        Pr {
            number: 15,
            title: String::new(),
            url: String::new(),
            state: crate::github::PrState::parse(state),
            is_draft: false,
            review_decision: crate::github::Decision::Unset,
            mergeable: crate::github::Mergeable::Unknown,
            head: head.into(),
            cross_repository: false,
            head_oid: String::new(),
            base: "main".into(),
            checks: vec![],
            reviews: vec![],
            comments: vec![],
            threads: Ok(Default::default()),
            stamp: Default::default(),
        }
    }

    fn found(pr: Option<Pr>, closed: Option<u64>, branch_gone: bool) -> Result<Lookup, String> {
        Ok(Lookup { pr, closed, branch_gone })
    }

    #[test]
    fn adopts_and_forgets_branches_from_pr_lookups() {
        let mut tv = TicketView::new(ticket());
        // An open PR's branch becomes the ticket's.
        assert!(tv.take_pr(found(Some(pr("OPEN", "eng-52-login")), None, false), None));
        assert_eq!(tv.t.branch.as_deref(), Some("eng-52-login"));
        assert_eq!(tv.pr.data.as_ref().map(|p| p.as_ref().map(|p| p.number)), Some(Some(15)));
        // Once it has one, another PR's head doesn't replace it.
        assert!(!tv.take_pr(found(Some(pr("OPEN", "other")), None, false), None));
        assert_eq!(tv.t.branch.as_deref(), Some("eng-52-login"));
        // A failed lookup keeps what the panel showed.
        assert!(!tv.take_pr(Err("gh: offline".into()), None));
        assert!(tv.pr.data.as_ref().is_some_and(Option::is_some));
        assert_eq!(tv.pr.error.as_deref(), Some("gh: offline"));
        // The branch is gone and its PR was closed: forget the branch, keep a note of the PR,
        // even after the branch is forgotten, until a PR replaces it.
        assert!(tv.take_pr(found(None, Some(14), true), None));
        assert_eq!((tv.t.branch.as_deref(), tv.closed_pr), (None, Some(14)));
        assert!(!tv.take_pr(found(None, None, false), None));
        assert_eq!(tv.closed_pr, Some(14));
        assert!(tv.take_pr(found(Some(pr("OPEN", "eng-52-again")), None, false), None));
        assert_eq!(tv.closed_pr, None);
        // A worktree git found names the branch, even one that looks gone, and a session that
        // never started moves into it.
        let wt = || Some((PathBuf::from("/src/acme-ENG-52"), "eng-52-wt".to_string()));
        assert!(tv.take_pr(found(None, None, true), wt()));
        assert_eq!(tv.t.branch.as_deref(), Some("eng-52-wt"));
        assert_eq!(tv.t.cwd, Path::new("/src/acme-ENG-52"));
        assert!(!tv.take_pr(found(None, None, true), wt()), "already there");
        // One that ran stays where its conversation is.
        let mut tv = TicketView::new(Ticket { session_id: Some("sid".into()), ..ticket() });
        assert!(tv.take_pr(found(None, None, false), wt()));
        assert_eq!(
            (tv.t.cwd.as_path(), tv.t.branch.as_deref()),
            (Path::new("/src/acme"), Some("eng-52-wt"))
        );
    }

    /// A PR from a fork names a branch its author chose, and anyone can open one saying it closes
    /// the issue: it shows in the panel, but the ticket doesn't take its branch.
    #[test]
    fn leaves_a_forks_branch_alone() {
        let mut tv = TicketView::new(ticket());
        let fork = Pr { cross_repository: true, ..pr("OPEN", "eng-52-fix;$(id)") };
        tv.take_pr(found(Some(fork), None, false), None);
        assert_eq!(tv.t.branch, None);
        assert_eq!(tv.pr.data.as_ref().map(|p| p.as_ref().map(|p| p.number)), Some(Some(15)));
        // The same repository's PR is adopted as before.
        assert!(tv.take_pr(found(Some(pr("OPEN", "eng-52-login")), None, false), None));
        assert_eq!(tv.t.branch.as_deref(), Some("eng-52-login"));
    }

    /// A worktree a workflow adds on `change/ENG-52` is what the lookup prefers, but the one
    /// taskman created for the ticket stays the ticket's: closing may remove that one, with its
    /// branch, and must never remove the workflow's.
    #[test]
    fn a_worktree_taskman_created_stays_the_tickets() {
        let own = Ticket {
            cwd: "/src/acme-ENG-52".into(),
            branch: Some("eng-52-login".into()),
            created_worktree: true,
            ..ticket()
        };
        let mut tv = TicketView::new(own.clone());
        // The first lookup counts what's there as seen; the worktree's effect is what's checked.
        tv.take_pr(found(None, None, false), None);
        let other = || Some((PathBuf::from("/src/wf/ENG-52"), "change/ENG-52".to_string()));
        assert!(!tv.take_pr(found(None, None, false), other()));
        assert_eq!(
            (tv.t.cwd.as_path(), tv.t.branch.as_deref(), tv.t.created_worktree),
            (Path::new("/src/acme-ENG-52"), Some("eng-52-login"), true)
        );
        // Its own worktree, listed again with a renamed branch, is still its own.
        let renamed = || Some((PathBuf::from("/src/acme-ENG-52"), "eng-52-login-v2".to_string()));
        assert!(tv.take_pr(found(None, None, false), renamed()));
        assert_eq!(tv.t.branch.as_deref(), Some("eng-52-login-v2"));
        assert!(tv.t.created_worktree);
        // Without one of its own, the ticket moves there, and the folder isn't taskman's.
        let mut tv =
            TicketView::new(Ticket { created_worktree: false, cwd: "/src/acme".into(), ..own });
        assert!(tv.take_pr(found(None, None, false), other()));
        assert_eq!(
            (tv.t.cwd.as_path(), tv.t.branch.as_deref(), tv.t.created_worktree),
            (Path::new("/src/wf/ENG-52"), Some("change/ENG-52"), false)
        );
    }

    fn comment(id: &str) -> crate::tracker::Comment {
        crate::tracker::Comment {
            id: id.into(),
            author: String::new(),
            created: String::new(),
            body: String::new(),
        }
    }

    fn with_activity(comments: &[&str], reviews: &[(&str, &str)]) -> Pr {
        let mut p = pr("OPEN", "eng-52");
        p.comments = comments
            .iter()
            .map(|id| crate::github::PrComment {
                id: id.to_string(),
                author: String::new(),
                when: String::new(),
                body: String::new(),
                url: String::new(),
            })
            .collect();
        p.reviews = reviews
            .iter()
            .map(|(id, author)| crate::github::Review {
                id: id.to_string(),
                author: author.to_string(),
                state: crate::github::ReviewState::Commented,
                when: String::new(),
                body: String::new(),
                url: String::new(),
            })
            .collect();
        p
    }

    #[test]
    fn counts_what_arrived_since_the_panels_last_showed() {
        let mut tv = TicketView::new(ticket());
        assert_eq!(tv.new_counts(), (0, 0), "nothing fetched, nothing new");
        // The first fetch counts what's there as seen.
        tv.jira.finish(Ok(Issue {
            comments: vec![comment("c1"), comment("c2")],
            ..Default::default()
        }));
        tv.pr.finish(Ok(Some(with_activity(&["p1"], &[("r1", "alice")]))));
        assert!(tv.baseline_seen());
        assert!(!tv.baseline_seen(), "only once");
        assert_eq!(tv.new_counts(), (0, 0));
        // A comment on each side, and a review from someone new.
        tv.jira.finish(Ok(Issue {
            comments: vec![comment("c3"), comment("c1"), comment("c2")],
            ..Default::default()
        }));
        tv.pr.finish(Ok(Some(with_activity(&["p2", "p1"], &[("r1", "alice"), ("r2", "bob")]))));
        assert_eq!(tv.new_counts(), (1, 2));
        assert!(tv.is_new_jira("c3") && !tv.is_new_jira("c1"));
        assert!(tv.is_new_gh("c:p2") && tv.is_new_gh("r:r2") && !tv.is_new_gh("r:r1"));
        // Selected: all read at once, and what was new keeps its marker until the ticket is left.
        assert!(tv.mark_seen());
        assert!(!tv.mark_seen(), "nothing more to mark");
        assert_eq!(tv.new_counts(), (0, 0));
        assert!(tv.is_new_jira("c3") && tv.is_new_gh("c:p2") && tv.is_new_gh("r:r2"));
        assert!(!tv.is_new_jira("c1") && !tv.is_new_gh("r:r1"));
        // A comment arriving while it's selected is read too, and marked as well.
        tv.jira.finish(Ok(Issue {
            comments: vec![comment("c4"), comment("c3"), comment("c1"), comment("c2")],
            ..Default::default()
        }));
        assert!(tv.mark_seen());
        assert_eq!(tv.new_counts(), (0, 0));
        assert!(tv.is_new_jira("c4") && tv.is_new_jira("c3"));
        // Leaving takes the markers off; ctrl+a m does it while staying.
        assert!(!tv.leave());
        assert!(!tv.is_new_jira("c4") && !tv.is_new_gh("c:p2"));
        assert!(!tv.clear_fresh(), "no markers left");
    }

    /// A read item never comes back as new, whatever a refresh loads in between: the lookup
    /// settling on another PR for a while, or a fetch that loads fewer.
    #[test]
    fn what_was_read_stays_read_across_refreshes() {
        let mut tv = TicketView::new(ticket());
        tv.jira.finish(Ok(Issue { comments: vec![comment("c1")], ..Default::default() }));
        tv.pr.finish(Ok(Some(with_activity(&["p1"], &[("r1", "alice")]))));
        tv.baseline_seen();
        tv.jira.finish(Ok(Issue {
            comments: vec![comment("c2"), comment("c1")],
            ..Default::default()
        }));
        tv.pr.finish(Ok(Some(with_activity(&["p2", "p1"], &[("r1", "alice")]))));
        assert_eq!(tv.new_counts(), (1, 1));
        tv.leave();
        // Another PR, then none, then the first again; a page of comments without the old ones.
        tv.pr.finish(Ok(Some(with_activity(&["q1"], &[("s1", "carol")]))));
        assert_eq!(tv.new_counts(), (0, 2), "the other PR's are new to the user");
        tv.leave();
        tv.pr.finish(Ok(None));
        tv.jira.finish(Ok(Issue { comments: vec![comment("c2")], ..Default::default() }));
        tv.leave();
        tv.jira.finish(Ok(Issue {
            comments: vec![comment("c2"), comment("c1")],
            ..Default::default()
        }));
        tv.pr.finish(Ok(Some(with_activity(&["p2", "p1"], &[("r1", "alice")]))));
        assert_eq!(tv.new_counts(), (0, 0));
        assert!(!tv.is_new_jira("c1") && !tv.is_new_gh("c:p1") && !tv.is_new_gh("r:r1"));
    }

    #[test]
    fn hooks_set_the_state_and_who_should_look() {
        let rec = |state: &str| {
            parse(&format!(r#"{{"state":"{state}","message":"Run the build","ts":1}}"#)).unwrap()
        };
        // Out of sight, waiting on the user: unseen, and it starts waiting.
        let mut tv = TicketView::new(ticket());
        assert!(tv.hook_reported(rec("needs"), false));
        assert_eq!(
            (tv.status.state, tv.status.seen, tv.status.message.as_str()),
            (AgentState::Needs, false, "Run the build")
        );
        // On screen, the user sees it already; it waits all the same.
        let mut tv = TicketView::new(ticket());
        assert!(tv.hook_reported(rec("needs"), true));
        assert!(tv.status.seen);
        // The same state again is the same wait, and changes nothing about who looked.
        tv.status.seen = true;
        assert!(!tv.hook_reported(rec("needs"), false));
        assert!(tv.status.seen);
        // A finished turn: unseen, not waiting on the user, and the panels refresh what's older than STALE.
        let mut tv = TicketView::new(ticket());
        tv.jira.start(1);
        tv.jira.finish(Ok(Issue::default()));
        tv.pr.start(2);
        tv.pr.finish(Ok(None));
        tv.pr.fetched_at = ago(STALE.as_secs_f32() + 1.0);
        let hour = Duration::from_secs(3600);
        assert!(!tv.hook_reported(rec("done"), false));
        assert_eq!((tv.status.state, tv.status.seen), (AgentState::Done, false));
        assert!(!tv.jira.due(hour) && tv.pr.due(hour));
        // Only a permission prompt is one a key answers.
        let permission =
            parse(r#"{"state":"needs","reason":"permission","message":"","ts":1}"#).unwrap();
        tv.hook_reported(permission, false);
        assert!(tv.status.permission);
        // The wait runs from the first "needs", and the prompt's command is kept.
        let asks = |ts: i64| {
            parse(&format!(
                r#"{{"state":"needs","reason":"permission","tool":"Bash","input":"cargo test","message":"","ts":{ts}}}"#
            ))
            .unwrap()
        };
        let mut tv = TicketView::new(ticket());
        tv.hook_reported(asks(100), false);
        tv.hook_reported(asks(106), false);
        assert_eq!(tv.status.waiting_since, Some(100));
        assert_eq!(tv.status.input.as_deref(), Some("cargo test"));
        tv.hook_reported(rec("working"), false);
        assert_eq!((tv.status.waiting_since, tv.status.input), (None, None));
    }
}
