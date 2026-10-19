//! Each session's state: the status files its hooks write, read on every poll, and what taskman
//! infers from the screen when no hook fires (interrupts, answered prompts, silent stops). Local
//! files tickets are reread on the same poll, as their files change.

use super::sessions::HAND_OFF_WAIT;
use super::{App, Away, Blocked};
use crate::background;
use crate::session::Session;
use crate::session_ids::{self, Claim};
use crate::status_file::{self, HookState};
use crate::store::{self, Cli, Ticket};
use crate::tracker::Text;
use crate::{local, resume};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

/// A working CLI redraws constantly. This long without output or hook events means it stopped
/// without saying so (see "Inferred states" in docs/design.md).
const SILENT_STOP: Duration = Duration::from_secs(8);

/// After Esc or ctrl+c on a working session, how long to watch for Claude's "Interrupted" line.
const ESC_WINDOW: Duration = Duration::from_secs(5);

/// A working Claude or Codex never went this long without drawing (the longest seen was 1.65 s),
/// so after a key that interrupts or answers a prompt, this much quiet means it isn't working.
const QUIET_AFTER_KEY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentState {
    NotStarted,
    Idle,
    Working,
    Needs,
    Done,
    Exited,
}

impl From<HookState> for AgentState {
    fn from(s: HookState) -> Self {
        match s {
            HookState::Working => AgentState::Working,
            HookState::Needs => AgentState::Needs,
            HookState::Done => AgentState::Done,
            HookState::Ended => AgentState::Exited,
            HookState::Idle | HookState::Unknown => AgentState::Idle,
        }
    }
}

impl AgentState {
    /// What the sidebar and the pane header call it, in the words `taskman status` uses.
    pub fn word(self) -> &'static str {
        let hook = match self {
            AgentState::NotStarted => return status_file::NOT_STARTED,
            AgentState::Idle => HookState::Idle,
            AgentState::Working => HookState::Working,
            AgentState::Needs => HookState::Needs,
            AgentState::Done => HookState::Done,
            AgentState::Exited => HookState::Ended,
        };
        hook.word()
    }
}

pub struct Status {
    pub state: AgentState,
    pub message: String,
    /// False when the ticket changed to a state worth looking at while it wasn't selected.
    pub seen: bool,
    pub(super) mtime: Option<SystemTime>,
    /// When the last hook event arrived.
    last_event: Option<Instant>,
    /// When the user pressed Esc or ctrl+c while Claude was working.
    esc_at: Option<Instant>,
    /// Whether the session waits on a permission prompt, which one key answers (the hook's
    /// `reason`), rather than on questions or a plan, which only a hook clears.
    pub permission: bool,
    /// When the session began waiting on the user, in seconds since the epoch, from the hook's
    /// record; none when it waits without one having said so (`queue`).
    pub waiting_since: Option<i64>,
    /// What the permission prompt asks to use and run (`Record::tool`, `Record::input`):
    /// untrusted text.
    pub tool: Option<String>,
    pub input: Option<String>,
    /// Whether Tab opened Claude's field for adding to the answer, where digits are text.
    amending: bool,
    /// When a key answered the permission prompt, which no hook reports (`prompt_key`).
    answered_at: Option<Instant>,
    /// How many "Interrupted" lines the screen showed at that key or at Esc, so an old one
    /// doesn't count.
    interrupted_lines: usize,
}

impl Status {
    pub(super) fn new() -> Self {
        Self {
            state: AgentState::NotStarted,
            message: String::new(),
            seen: true,
            mtime: None,
            last_event: None,
            esc_at: None,
            permission: false,
            waiting_since: None,
            tool: None,
            input: None,
            amending: false,
            answered_at: None,
            interrupted_lines: 0,
        }
    }

    /// A key on its way to the session: one that answers a permission prompt makes it working at
    /// once, since no hook says so; Esc or ctrl+c while working starts the interrupt watch.
    /// `interrupted_lines` counts the screen's "Interrupted" lines.
    pub(super) fn key(
        &mut self,
        cli: Cli,
        key: &KeyEvent,
        interrupted_lines: impl FnOnce() -> usize,
    ) {
        if self.state == AgentState::Needs && self.permission {
            match prompt_key(cli, key, self.amending) {
                // Approved or declined; a decline shows as an interrupt (`inferred_state`).
                PromptKey::Answers => {
                    self.state = AgentState::Working;
                    self.answered_at = Some(Instant::now());
                    self.esc_at = None;
                    self.amending = false;
                    self.interrupted_lines = interrupted_lines();
                }
                PromptKey::Amends => self.amending = true,
                PromptKey::Other => {}
            }
        } else if self.state == AgentState::Working && is_interrupt(key) {
            self.esc_at = Some(Instant::now());
            self.answered_at = None;
            self.interrupted_lines = interrupted_lines();
        }
    }

    /// The session started (again): nothing its last run left counts.
    pub(super) fn started(&mut self) {
        self.state = AgentState::Idle;
        self.message.clear();
        self.mtime = None;
        self.last_event = None;
        self.esc_at = None;
        self.permission = false;
        self.amending = false;
        self.answered_at = None;
    }

    /// A hook wrote the status file: whatever a key or the screen suggested is settled by it.
    fn hook_fired(&mut self) {
        self.last_event = Some(Instant::now());
        self.esc_at = None;
        self.answered_at = None;
        self.amending = false;
    }

    /// For a working session no hook has stopped: the state the screen shows instead
    /// (`inferred_state`), applied and returned when it changed. `quiet` is how long the session
    /// hasn't drawn, `interrupted_lines` counts the screen's "Interrupted" lines, and `selected`
    /// whether the user is looking at it.
    fn infer(
        &mut self,
        quiet: Duration,
        interrupted_lines: impl FnOnce() -> usize,
        selected: bool,
    ) -> Option<AgentState> {
        let watching =
            self.answered_at.is_some() || self.esc_at.is_some_and(|at| at.elapsed() < ESC_WINDOW);
        let seen = Seen {
            quiet,
            since_event: self.last_event.map(|t| t.elapsed()),
            since_interrupt: self.esc_at.map(|t| t.elapsed()),
            since_answer: self.answered_at.map(|t| t.elapsed()),
            interrupted: watching && interrupted_lines() > self.interrupted_lines,
        };
        let state = inferred_state(&seen)?;
        self.state = state;
        self.esc_at = None;
        self.answered_at = None;
        self.seen = selected || seen.interrupted;
        Some(state)
    }
}

/// What a key does to a permission prompt the session waits on (see "Inferred states" in
/// docs/design.md).
#[derive(Debug, PartialEq)]
enum PromptKey {
    /// It answers the prompt: approves, or declines, which ends the turn.
    Answers,
    /// Claude's Tab: a field for adding to the answer, where digits and letters are text.
    Amends,
    /// Browsing the options, typing, or a key the prompt ignores.
    Other,
}

fn prompt_key(cli: Cli, key: &KeyEvent, amending: bool) -> PromptKey {
    if is_interrupt(key) {
        return PromptKey::Answers;
    }
    if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT) {
        return PromptKey::Other;
    }
    if key.code == KeyCode::Enter {
        return PromptKey::Answers;
    }
    if amending {
        return PromptKey::Other;
    }
    match (cli, key.code) {
        // Claude's options are numbered and a digit picks one at once.
        (Cli::Claude, KeyCode::Char('1'..='9')) => PromptKey::Answers,
        (Cli::Claude, KeyCode::Tab) => PromptKey::Amends,
        // Codex's yes and no. Its other shortcuts (p, a) and digits are on some prompts only, and
        // one that isn't an option does nothing; Codex redraws its prompt every second, so the
        // quiet check couldn't undo the switch. Those answers show when the next hook fires.
        (Cli::Codex, KeyCode::Char('y' | 'n')) => PromptKey::Answers,
        _ => PromptKey::Other,
    }
}

fn is_interrupt(key: &KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// What taskman sees of a session the hooks left working, as times since each thing happened.
struct Seen {
    /// Since the session last drew anything.
    quiet: Duration,
    since_event: Option<Duration>,
    /// Since Esc or ctrl+c, while it was working.
    since_interrupt: Option<Duration>,
    /// Since a key answered a permission prompt.
    since_answer: Option<Duration>,
    /// Whether a new "Interrupted" line showed up after either key.
    interrupted: bool,
}

/// The state of a working session that no hook reported, if it changed: the inferences in
/// "Inferred states" in docs/design.md.
fn inferred_state(s: &Seen) -> Option<AgentState> {
    let quiet_since = |at: Option<Duration>| {
        at.is_some_and(|t| t >= QUIET_AFTER_KEY) && s.quiet >= QUIET_AFTER_KEY
    };
    if s.interrupted {
        return Some(AgentState::Done);
    }
    // An approved prompt sets Claude's spinner going at once. Quiet means the key didn't answer
    // and the prompt still waits, so it needs the user again, never "your turn".
    if s.since_answer.is_some() {
        return quiet_since(s.since_answer).then_some(AgentState::Needs);
    }
    // Interrupted before it wrote anything, Claude prints no "Interrupted" line and falls quiet.
    if s.since_interrupt.is_some_and(|t| t < ESC_WINDOW) && quiet_since(s.since_interrupt) {
        return Some(AgentState::Done);
    }
    (s.quiet >= SILENT_STOP && s.since_event.is_none_or(|t| t >= SILENT_STOP))
        .then_some(AgentState::Done)
}

/// At startup, before any session starts: the status files a previous run left describe sessions
/// no longer running, so they go, but first each ticket takes the latest session id from them, in
/// case its conversation changed after taskman last saved (a crash, say). A file at the old path,
/// by key alone, is from a session an older taskman started; it's the ticket's only when no other
/// project has that key. Returns whether a session id changed.
pub(super) fn take_leftover_status(
    tickets: &mut [Ticket],
    status: impl Fn(&Ticket) -> PathBuf,
    legacy: impl Fn(&str) -> PathBuf,
    outside: impl Fn(&Ticket) -> bool,
) -> bool {
    let mut changed = false;
    for i in 0..tickets.len() {
        let t = &tickets[i];
        let paths = [status(t), legacy(&t.key)];
        let unique = tickets.iter().filter(|o| o.key == t.key).count() == 1;
        let latest = paths[..if unique { 2 } else { 1 }]
            .iter()
            .filter_map(|p| status_file::read(p))
            .filter(|r| r.session_id.is_some())
            .max_by_key(|r| r.ts);
        // A session running in another terminal keeps its file, so its state shows at once.
        let keep = outside(t);
        for p in &paths[usize::from(keep)..] {
            remove_status_at(p);
        }
        if let Some(sid) = latest.and_then(|r| r.session_id)
            && tickets[i].session_id.as_ref() != Some(&sid)
            && session_ids::claim(&*tickets, &tickets[i], &sid) == Claim::Adopt
        {
            tickets[i].session_id = Some(sid);
            changed = true;
        }
    }
    changed
}

/// Removes a closed ticket's status files, and its owner file unless another terminal runs it: that
/// one keeps its claim, so a ticket added again can't start a second copy. Stop its session first.
pub(super) fn remove_status_files(t: &Ticket) {
    remove_status_at(&store::status_file(&t.repo, &t.key));
    let owner = resume::owner_file(&t.repo, &t.key);
    if matches!(resume::peek(&owner), resume::Owner::Free | resume::Owner::Back) {
        let _ = std::fs::remove_file(owner);
    }
}

/// Removes a status file and the refresh signal files and the asleep marker beside it.
pub(super) fn remove_status_at(path: &Path) {
    let beside = ["jira", "gh", "asleep"].map(|ext| path.with_extension(ext));
    for p in std::iter::once(path.to_path_buf()).chain(beside) {
        let _ = std::fs::remove_file(p);
    }
}

impl App {
    pub(super) fn poll_status(&mut self) {
        let mut changed = false;
        let mut handed_off = Vec::new();
        let mut reported = Vec::new();
        for (i, tv) in self.tickets.iter_mut().enumerate() {
            if let Some(Away::Leaving(at)) = tv.away {
                if tv.session.as_mut().is_none_or(Session::ended) || at.elapsed() >= HAND_OFF_WAIT {
                    handed_off.push(i);
                }
            } else if tv.hold.is_some() {
                // Handed off with its CLI still stopping: the claim goes once the CLI is gone.
                tv.release_handed_off();
            } else {
                // Whether another terminal runs it, or ran it and ended (see `resume`).
                let away = match resume::peek(&resume::owner_file(&tv.t.repo, &tv.t.key)) {
                    resume::Owner::Outside(_) | resume::Owner::Taskman(_) => Some(Away::Outside),
                    resume::Owner::Back if tv.away == Some(Away::HandedOff) => {
                        Some(Away::HandedOff)
                    }
                    resume::Owner::Back => Some(Away::Back),
                    // `start_session` says why it can't start yet. Stopping is this TUI's own
                    // claim, kept above until the handed-off CLI is gone: nothing new.
                    resume::Owner::Free | resume::Owner::Closing => None,
                    resume::Owner::Stopping => tv.away,
                };
                if away != tv.away {
                    if away == Some(Away::Outside) {
                        // An ended session's screen in the pane would look like the live one.
                        tv.stop_session();
                    }
                    tv.away = away;
                    self.dirty.store(true, Ordering::Relaxed);
                }
            }
            let path = store::status_file(&tv.t.repo, &tv.t.key);
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            if mtime.is_some() && mtime != tv.status.mtime {
                tv.status.mtime = mtime;
                let record = status_file::read(&path);
                // A hook of an earlier start can write late (see `takes`): not this session's.
                if let Some(mut rec) = record.filter(|rec| tv.takes(rec)) {
                    tv.status.hook_fired();
                    if let Some(sid) = rec.session_id.take()
                        && tv.t.session_id.as_deref() != Some(sid.as_str())
                    {
                        reported.push((i, sid));
                    }
                    self.bell |= tv.hook_reported(rec, i == self.sel);
                    self.dirty.store(true, Ordering::Relaxed);
                }
            }
            // The hook touches these when Claude uses a Jira tool, or pushes / runs gh.
            let jira_sig =
                std::fs::metadata(path.with_extension("jira")).and_then(|m| m.modified()).ok();
            if jira_sig.is_some() && jira_sig != tv.jira_signal {
                tv.jira_signal = jira_sig;
                tv.jira.request(true);
            }
            let gh_sig =
                std::fs::metadata(path.with_extension("gh")).and_then(|m| m.modified()).ok();
            if gh_sig.is_some() && gh_sig != tv.gh_signal {
                tv.gh_signal = gh_sig;
                tv.pr.request(true);
            }
            // Interrupts and answered prompts fire no hook: infer them from the screen.
            if tv.status.state == AgentState::Working
                && let Some(s) = &tv.session
                && let Some(state) =
                    tv.status.infer(s.quiet_for(), || s.screen_count("Interrupted"), i == self.sel)
            {
                if state == AgentState::Needs && i != self.sel {
                    self.bell = true;
                }
                self.dirty.store(true, Ordering::Relaxed);
            }
            if tv.session.as_ref().is_some_and(|s| s.exited())
                && tv.status.state != AgentState::Exited
            {
                tv.status.state = AgentState::Exited;
                tv.status.seen = i == self.sel;
            }
            // Ended in the pane: `taskman resume` may take it now.
            if let Some(s) = tv.session.as_mut().filter(|s| s.exited()) {
                // Each poll until the exit's code is known.
                let end = s.end();
                if end != tv.end {
                    tv.end = end;
                    self.dirty.store(true, Ordering::Relaxed);
                }
                tv.hold = None;
                // Not from the state change: a hook may have marked it ended already. Once per
                // ending: the screen only changes with a new session.
                if tv.background.is_none() && !tv.screen_read {
                    tv.screen_read = true;
                    tv.background = tv
                        .session
                        .as_ref()
                        .filter(|_| tv.t.cli == Cli::Claude)
                        .map(|s| s.screen_text())
                        .and_then(|screen| {
                            Some(Blocked {
                                id: background::job_id(&screen)?,
                                message: background::message(&screen),
                                stopping: false,
                                error: None,
                            })
                        });
                }
            }
        }
        for i in handed_off {
            self.handed_off(i);
        }
        for (i, sid) in reported {
            match session_ids::claim(self.tickets.iter().map(|tv| &tv.t), &self.tickets[i].t, &sid)
            {
                Claim::Adopt => {
                    self.tickets[i].t.session_id = Some(sid);
                    changed = true;
                }
                Claim::Conflict { other } if self.tickets[i].switched.as_ref() != Some(&sid) => {
                    let msg = session_ids::switched(&self.tickets[i].t, other, &sid);
                    self.tickets[i].switched = Some(sid);
                    self.warn(msg);
                }
                Claim::Conflict { .. } => {}
            }
        }
        changed |= self.poll_local();
        if changed {
            self.save();
        }
    }

    /// Rereads Local files tickets whose file moved or changed. The files are local, so this runs
    /// on every status poll instead of on a refresh timer: a board is listed again only when one
    /// of its folders changed (`local::Cache`), and a file read again when its own time changed.
    fn poll_local(&mut self) -> bool {
        let mut changed = false;
        for i in 0..self.tickets.len() {
            if self.tickets[i].t.agent.is_some() {
                continue;
            }
            let Some(board) = self.access(&self.tickets[i].t).board().map(Path::to_path_buf) else {
                continue;
            };
            let files = self.trackers.board_listing(&board);
            let tv = &mut self.tickets[i];
            let forced = std::mem::take(&mut tv.jira.force);
            match local::find_in(files, &tv.t.key, &board) {
                Ok(file) => {
                    let mtime = std::fs::metadata(&file.path).and_then(|m| m.modified()).ok();
                    let seen = Some((file.path.clone(), mtime));
                    if tv.local_file == seen && !forced {
                        continue;
                    }
                    let res = local::read_no_follow(&file.path)
                        .map(|text| local::parse(&text, &file, Text::Plain.render()))
                        .map_err(|e| {
                            format!("couldn't read {}: {e}", local::rel(&board, &file.path))
                        });
                    tv.jira.finish(res);
                    tv.local_file = seen;
                    changed |= tv.baseline_seen();
                }
                Err(e) => {
                    if tv.jira.error.as_ref() == Some(&e) && tv.jira.data.is_none() {
                        continue;
                    }
                    // The panel shows why, not the file as it was before it went.
                    tv.jira.data = None;
                    tv.jira.error = Some(e);
                    tv.local_file = None;
                }
            }
            self.dirty.store(true, Ordering::Relaxed);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{ago, key};

    /// A session waiting on what the hook last wrote.
    fn waiting(record: &str) -> Status {
        let rec = status_file::parse(record).unwrap();
        let mut s = Status::new();
        (s.state, s.permission) = (rec.state.into(), rec.permission());
        s
    }

    #[test]
    fn keys_that_answer_a_permission_prompt() {
        use KeyCode::*;
        let ctrl_c = KeyEvent::new(Char('c'), KeyModifiers::CONTROL);
        for k in [key(Enter), key(Esc), ctrl_c, key(Char('1')), key(Char('4'))] {
            assert_eq!(prompt_key(Cli::Claude, &k, false), PromptKey::Answers, "{k:?}");
        }
        // Browsing the options, and keys Claude's prompt has no use for.
        for k in [
            key(Up),
            key(Down),
            key(Left),
            key(Right),
            key(Char('y')),
            KeyEvent::new(Enter, KeyModifiers::SHIFT),
        ] {
            assert_eq!(prompt_key(Cli::Claude, &k, false), PromptKey::Other, "{k:?}");
        }
        // Tab opens a field where "1n" is text, not answers (Claude 2.1.296).
        assert_eq!(prompt_key(Cli::Claude, &key(Tab), false), PromptKey::Amends);
        for k in [key(Char('1')), key(Char('n')), key(Tab)] {
            assert_eq!(prompt_key(Cli::Claude, &k, true), PromptKey::Other, "{k:?}");
        }
        assert_eq!(prompt_key(Cli::Claude, &key(Enter), true), PromptKey::Answers);
        // Codex: y, n (which declines and fires Interrupt), Enter, Esc; not digits or
        // other letters.
        for k in [key(Enter), key(Esc), key(Char('y')), key(Char('n'))] {
            assert_eq!(prompt_key(Cli::Codex, &k, false), PromptKey::Answers, "{k:?}");
        }
        for k in
            [key(Char('1')), key(Char('p')), key(Char('a')), key(Char('Y')), key(Down), key(Tab)]
        {
            assert_eq!(prompt_key(Cli::Codex, &k, false), PromptKey::Other, "{k:?}");
        }
    }

    #[test]
    fn answering_a_permission_prompt_means_working() {
        for (cli, k) in [
            (Cli::Claude, key(KeyCode::Enter)),
            (Cli::Claude, key(KeyCode::Char('2'))),
            (Cli::Codex, key(KeyCode::Char('y'))),
        ] {
            let mut s = waiting(
                r#"{"state":"needs","reason":"permission","message":"Run the build","ts":1}"#,
            );
            s.key(cli, &key(KeyCode::Down), || 0);
            assert_eq!(s.state, AgentState::Needs);
            s.key(cli, &k, || 1);
            assert_eq!((s.state, s.interrupted_lines), (AgentState::Working, 1));
            assert!(s.answered_at.is_some());
        }
        // After Tab, typing a digit is text; Enter sends the answer.
        let mut s = waiting(r#"{"state":"needs","reason":"permission","message":"","ts":1}"#);
        s.key(Cli::Claude, &key(KeyCode::Tab), || 0);
        s.key(Cli::Claude, &key(KeyCode::Char('1')), || 0);
        assert_eq!(s.state, AgentState::Needs);
        s.key(Cli::Claude, &key(KeyCode::Enter), || 0);
        assert_eq!(s.state, AgentState::Working);
    }

    #[test]
    fn answering_questions_keeps_needs_you() {
        // Claude's AskUserQuestion with three questions and its plan approval, Codex's
        // request_user_input with three: every answer but the last fires no hook, and the last
        // fires PostToolUse. No key clears them; the hook does.
        for record in [
            r#"{"state":"needs","reason":"question","message":"Claude is asking you a question","ts":1}"#,
            r#"{"state":"needs","reason":"question","message":"Claude Code needs your approval for the plan","ts":1}"#,
            // Written by a release before `reason`: treated as a question.
            r#"{"state":"needs","message":"Claude needs your permission to use Bash","ts":1}"#,
        ] {
            for cli in [Cli::Claude, Cli::Codex] {
                let mut s = waiting(record);
                for k in [
                    KeyCode::Enter,
                    KeyCode::Char('2'),
                    KeyCode::Right,
                    KeyCode::Char('y'),
                    KeyCode::Enter,
                    KeyCode::Esc,
                ] {
                    s.key(cli, &key(k), || 0);
                    assert_eq!(s.state, AgentState::Needs, "{record} {k:?}");
                }
            }
        }
    }

    fn seen(quiet: f32) -> Seen {
        Seen {
            quiet: Duration::from_secs_f32(quiet),
            since_event: None,
            since_interrupt: None,
            since_answer: None,
            interrupted: false,
        }
    }

    fn secs(t: f32) -> Option<Duration> {
        Some(Duration::from_secs_f32(t))
    }

    #[test]
    fn approved_prompts_stay_working_and_never_end_the_turn() {
        // Approved: Claude's spinner draws at least every 1.35 s while the tool runs, Codex's
        // every 0.66 s.
        for quiet in [0.05, 1.35, 1.65] {
            assert_eq!(inferred_state(&Seen { since_answer: secs(180.0), ..seen(quiet) }), None);
        }
        // Just pressed, the prompt having been quiet before it: too soon to tell.
        assert_eq!(inferred_state(&Seen { since_answer: secs(0.3), ..seen(20.0) }), None);
        // The key didn't answer and the prompt still waits in silence: needs you again, however
        // long, never "your turn".
        for t in [2.0, 9.0, 600.0] {
            let s = Seen { since_answer: secs(t), since_event: secs(t + 5.0), ..seen(t) };
            assert_eq!(inferred_state(&s), Some(AgentState::Needs));
        }
        // Declined: Claude prints "Interrupted" and fires no hook.
        assert_eq!(
            inferred_state(&Seen { since_answer: secs(0.3), interrupted: true, ..seen(0.2) }),
            Some(AgentState::Done)
        );
    }

    #[test]
    fn interrupts_show_promptly() {
        // After output, "Interrupted" shows 115-270 ms after Esc or ctrl+c.
        let esc = |t: f32, quiet: f32| Seen {
            since_interrupt: secs(t),
            since_event: secs(30.0),
            ..seen(quiet)
        };
        assert_eq!(
            inferred_state(&Seen { interrupted: true, ..esc(0.3, 0.1) }),
            Some(AgentState::Done)
        );
        // Before any output there's no such line: the prompt goes back to the input, then quiet.
        assert_eq!(inferred_state(&esc(1.0, 0.9)), None);
        assert_eq!(inferred_state(&esc(2.1, 2.0)), Some(AgentState::Done));
        // Esc that didn't interrupt (closing a menu, say): Claude goes on drawing.
        assert_eq!(inferred_state(&esc(4.0, 0.5)), None);
        // An old "Interrupted" line doesn't count (`Status::interrupted_lines`), and past the
        // window the silence rule decides.
        assert_eq!(inferred_state(&esc(6.0, 3.0)), None);
        assert_eq!(inferred_state(&esc(9.0, 8.5)), Some(AgentState::Done));
    }

    #[test]
    fn working_sessions_draw_more_often_than_the_silence_rule_allows() {
        // The longest gaps between draws of a working CLI: Claude running a tool 1.35 s, in a
        // network stall 1.43 s, compacting 0.23 s, with a subagent 1.65 s; Codex 0.66 s and 0.57 s.
        for quiet in [1.35, 1.43, 0.23, 1.65, 0.66, 0.57] {
            assert_eq!(inferred_state(&Seen { since_event: secs(120.0), ..seen(quiet) }), None);
        }
        assert!(QUIET_AFTER_KEY.as_secs_f32() > 1.65 && SILENT_STOP > QUIET_AFTER_KEY);
        // Stopped without a hook: quiet, and no hook either, for SILENT_STOP.
        assert_eq!(
            inferred_state(&Seen { since_event: secs(8.0), ..seen(8.0) }),
            Some(AgentState::Done)
        );
        assert_eq!(inferred_state(&Seen { since_event: secs(2.0), ..seen(8.0) }), None);
        assert_eq!(inferred_state(&seen(7.9)), None);
    }

    #[test]
    fn reads_status_files_across_releases() {
        let parse_status = status_file::parse;
        // The format taskman hook has written since v0.1; a background update must not break it.
        let rec = parse_status(r#"{"state":"needs","session_id":"5f0c-11","message":"Claude needs your permission to use Bash","ts":1760000000}"#).unwrap();
        assert_eq!(AgentState::from(rec.state), AgentState::Needs);
        assert_eq!(rec.session_id.as_deref(), Some("5f0c-11"));
        assert_eq!(rec.message, "Claude needs your permission to use Bash");
        // What this release's hook writes, and a newer hook's extra fields.
        let ours = crate::hook::status_record(
            HookState::Done,
            Some("5f0c-11"),
            Some("beta/WP-1"),
            "",
            1760000000,
        )
        .to_string();
        assert_eq!(AgentState::from(parse_status(&ours).unwrap().state), AgentState::Done);
        let newer =
            parse_status(r#"{"state":"working","session_id":null,"message":"","ts":1,"cwd":"/r"}"#)
                .unwrap();
        assert_eq!((newer.state.into(), newer.session_id), (AgentState::Working, None));
        // `reason`: only a permission prompt is one a key answers.
        assert!(
            parse_status(r#"{"state":"needs","reason":"permission","message":"","ts":1}"#)
                .unwrap()
                .permission()
        );
        assert!(
            !parse_status(r#"{"state":"needs","reason":"question","message":"","ts":1}"#)
                .unwrap()
                .permission()
        );
        assert!(!rec.permission());
    }

    #[test]
    fn infers_interrupts_answers_and_silent_stops() {
        let working =
            || Status { state: AgentState::Working, last_event: ago(30.0), ..Status::new() };
        let ms = Duration::from_millis;
        // Not watching for an interrupt, the screen isn't even read; drawing, it's still working.
        let mut s = working();
        assert_eq!(s.infer(ms(100), || unreachable!("not watching"), false), None);
        assert_eq!(s.state, AgentState::Working);
        // Esc, then a new "Interrupted" line: done, and seen, since the user did it.
        let mut s = working();
        s.key(Cli::Claude, &key(KeyCode::Esc), || 1);
        assert_eq!(s.infer(ms(100), || 1, false), None, "only the old line");
        assert_eq!(s.infer(ms(100), || 2, false), Some(AgentState::Done));
        assert!(s.seen && s.esc_at.is_none());
        // Esc before any output prints no line: quiet after it means stopped, which the user
        // sees only if it's on screen.
        for selected in [false, true] {
            let mut s = Status { esc_at: ago(2.5), ..working() };
            assert_eq!(s.infer(ms(2500), || 0, selected), Some(AgentState::Done));
            assert_eq!(s.seen, selected);
        }
        // An answered prompt that stays quiet still waits: needs you again.
        let mut s = Status { state: AgentState::Needs, permission: true, ..Status::new() };
        s.key(Cli::Claude, &key(KeyCode::Enter), || 0);
        assert_eq!(s.state, AgentState::Working);
        s.answered_at = ago(3.0);
        assert_eq!(s.infer(ms(3000), || 0, false), Some(AgentState::Needs));
        assert!(s.answered_at.is_none());
        // Stopped without a hook: SILENT_STOP of quiet with no hook either.
        let mut s = Status { last_event: ago(9.0), ..working() };
        assert_eq!(s.infer(ms(9000), || 0, false), Some(AgentState::Done));
        assert!(!s.seen);
        let mut s = Status { last_event: ago(1.0), ..working() };
        assert_eq!(s.infer(ms(9000), || 0, false), None);
    }
}
