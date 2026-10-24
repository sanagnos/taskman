//! The sessions in the pane: starting them (one copy each, under the owner lock), their names,
//! and handing them to another terminal (`ctrl+a t`, `taskman resume`) and back.

use super::poll::remove_status_at;
use super::sync::Msg;
use super::{AgentState, App, Away, Gone, Id, Level, Mode};
use crate::background;
use crate::session::{self, End, Session};
use crate::store::{self, Ticket};
use crate::{keys, resume, untrusted};
use std::ffi::OsString;
use std::time::{Duration, Instant};

/// How long `ctrl+a t` waits for the CLI to quit before stopping it.
pub(super) const HAND_OFF_WAIT: Duration = Duration::from_secs(5);
/// How often a session's refused clipboard requests of one kind are toasted at most.
pub(super) const CLIPBOARD_WARN_EVERY: Duration = Duration::from_secs(60);
/// What taskman's own variables a tmux window gets, so `taskman resume` there finds the same state,
/// config and CLIs: tmux gives new windows its server's environment, not taskman's.
const TMUX_ENV: [&str; 8] = [
    "XDG_STATE_HOME",
    "XDG_CONFIG_HOME",
    "TASKMAN_CONFIG",
    "TASKMAN_CLAUDE",
    "TASKMAN_CODEX",
    "TASKMAN_NO_AUTOUPDATE",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
];

impl App {
    /// Queues the newest of the sessions' clipboard writes for the terminal (there's one
    /// clipboard, so the others would be overwritten at once), and warns of a request they
    /// refused (`session::Refused`): one a pass, the one that matters most, and once a minute at
    /// most for each session and why, so a program that keeps asking can't flood the toasts.
    pub(super) fn take_clipboards(&mut self) {
        let now = Instant::now();
        let mut newest: Option<(u64, session::ClipboardWrite)> = None;
        let mut warning: Option<(usize, session::Refused)> = None;
        for (i, tv) in self.tickets.iter().enumerate() {
            let Some(s) = &tv.session else { continue };
            let (write, refused) = s.take_clipboard();
            if let Some((at, write)) = write
                && newest.as_ref().is_none_or(|(n, _)| at > *n)
            {
                newest = Some((at, write));
            }
            for why in refused {
                if warn_due(&tv.clipboard_warned, why, now)
                    && warning.is_none_or(|(_, w)| why.rank() < w.rank())
                {
                    warning = Some((i, why));
                }
            }
        }
        self.clipboard_out.extend(newest.map(|(_, write)| write));
        if let Some((i, why)) = warning {
            let tv = &mut self.tickets[i];
            tv.clipboard_warned.retain(|(w, _)| !w.same(why));
            tv.clipboard_warned.push((why, now));
            let msg = why.toast(tv.t.display());
            self.warn(msg);
        }
    }

    pub(super) fn ensure_selected_session(&mut self) {
        let area = self.areas.claude;
        let Some(tv) = self.tickets.get(self.sel) else { return };
        // Selecting a sleeping session starts it again, on the same conversation.
        if tv.dormant() && !tv.waiting() && area.width > 0 {
            self.start_session(self.sel);
        }
        // An agent can only message sessions that are running: once a project's agent that
        // messages runs, start its ticket sessions that can be messaged too, one per tick. A
        // ticket that's away won't start, and finding it every tick would hold up the rest. One
        // asleep stays asleep: waking it would undo the memory it was put to sleep to save, and
        // the agent is told it can't be messaged (`taskman status` says "asleep").
        if area.width > 0
            && let Some(i) = (0..self.tickets.len()).find(|&i| {
                let tv = &self.tickets[i];
                tv.t.agent.is_none()
                    && tv.t.cli.messages_agents()
                    && tv.session.is_none()
                    && tv.status.state == AgentState::NotStarted
                    && !tv.asleep
                    && !tv.waiting()
                    && tv.away.is_none()
                    && self.tickets.iter().any(|a| {
                        a.t.agent.is_some()
                            && a.t.cli.messages_agents()
                            && a.t.repo == tv.t.repo
                            && a.session.as_ref().is_some_and(|s| !s.exited())
                    })
            })
        {
            self.start_session(i);
        }
        // Not while its last CLI is still being reaped: `start_session` would refuse, and the
        // request would be lost.
        if area.width > 0
            && let Some(i) = self.tickets.iter().position(|tv| tv.start && !tv.reaping())
        {
            self.tickets[i].start = false;
            if self.tickets[i].session.is_none()
                && self.tickets[i].status.state == AgentState::NotStarted
            {
                self.start_session(i);
            }
        }
    }

    fn start_session(&mut self, i: usize) {
        // Never in the main checkout in place of a worktree the user asked for, and never while
        // it's away: running elsewhere, or handed off until the user brings it back.
        if self.tickets[i].waiting() || self.tickets[i].away.is_some() {
            return;
        }
        // Nor while its last CLI is still being reaped (a restart, say): the lock is taskman's,
        // not the CLI's, so it wouldn't keep a second copy off the same conversation. The next
        // tick tries again, within a moment.
        if self.tickets[i].reaping() {
            return;
        }
        // Starting wakes it, or it runs elsewhere now: either way it isn't asleep.
        self.tickets[i].rouse();
        // A restart keeps the pane's claim: taking it again would find it held, by taskman itself.
        let path = resume::owner_file(&self.tickets[i].t.repo, &self.tickets[i].t.key);
        let taken = match self.tickets[i].hold.take() {
            Some(hold) => Ok(Ok(hold)),
            None => resume::take(&path),
        };
        (self.tickets[i].lock_error, self.tickets[i].gone) = (None, None);
        let hold = match taken {
            Ok(Ok(hold)) => hold,
            Ok(Err(resume::Owner::Closing)) => {
                // Enter tries again (`restart_session`) once the removal is done.
                let msg = resume::closing(self.tickets[i].t.display());
                self.tickets[i].status.state = AgentState::Exited;
                self.warn(msg);
                return;
            }
            Ok(Err(_)) => {
                self.tickets[i].away = Some(Away::Outside);
                return;
            }
            // The lock is what keeps a session to one copy: without it, `taskman resume` could
            // start a second on the same conversation. So a lock that can't be taken (a state
            // folder that can't be written, say) stops the session starting; the pane says why,
            // and Enter tries again (`restart_session`).
            Err(e) => {
                let tv = &mut self.tickets[i];
                tv.status.state = AgentState::Exited;
                tv.status.seen = i == self.sel;
                tv.lock_error = Some(format!("{}: {e}", store::tilde(&path)));
                let msg = format!(
                    "Couldn't take {}'s session lock: {e}; fix it, then press Enter to retry",
                    tv.t.display()
                );
                self.error(msg);
                return;
            }
        };
        // Never in a folder that's gone, a worktree removed by hand or a project moved: the CLI
        // would start in the home folder and take it for the ticket's checkout. Checked under the
        // claim, so a session running in another terminal shows as that; let go of unchanged. The
        // pane says why, and Enter makes the worktree again (`remake_worktree`) or tries again.
        self.tickets[i].gone = Gone::of(&self.tickets[i].t);
        if let Some(gone) = self.tickets[i].gone {
            drop(hold);
            let tv = &mut self.tickets[i];
            tv.status.state = AgentState::Exited;
            tv.status.seen = i == self.sel;
            let msg = self.gone_toast(i, gone);
            self.error(msg);
            return;
        }
        let hold = hold.taskman();
        let area = self.areas.claude;
        let name = self.qualified(&self.tickets[i].t);
        let launch = super::launch::launch_id();
        let cmd = self.build_command(&self.tickets[i].t, &name, &launch);
        // What the status file says is an earlier start's: at taskman's next start it would pass
        // for the session's latest conversation (`take_leftover_status`).
        remove_status_at(&store::status_file(&self.tickets[i].t.repo, &self.tickets[i].t.key));
        let wake = self.wake.clone();
        let (scrollback, colours) = (self.config.scrollback, self.theme.colours);
        match Session::spawn(cmd.clone(), &name, area.height, area.width, scrollback, wake, colours)
        {
            Ok(s) => {
                let tv = &mut self.tickets[i];
                tv.session = Some(s);
                tv.end = None;
                tv.last_screen = None;
                tv.hold = Some(hold);
                tv.name = Some(name);
                tv.launch = Some(launch);
                tv.watch = super::sleep::Watch::new(Instant::now());
                tv.status.started();
            }
            // The pane says why; the toast only points there from another ticket.
            Err(e) => {
                let tv = &mut self.tickets[i];
                let (install, var) = (tv.t.cli.product(), tv.t.cli.program_var());
                tv.end = Some(End::StartFailed(session::start_error(&cmd, &e, install, var)));
                tv.status.state = AgentState::Exited;
                tv.status.seen = i == self.sel;
                if i != self.sel {
                    let msg =
                        format!("{}'s {} session didn't start", tv.t.display(), tv.t.cli.label());
                    self.error(msg);
                }
            }
        }
    }

    /// What other sessions message ticket `i`'s session by: the name it started with while it
    /// runs, else the one it would start with now.
    pub fn session_name(&self, i: usize) -> String {
        let tv = &self.tickets[i];
        match &tv.name {
            Some(name) if tv.session.is_some() => name.clone(),
            _ => self.qualified(&tv.t),
        }
    }

    /// The ticket's key, as `project/KEY` while another project has the same key: the name its
    /// session starts with, and how toasts and replies name it. Never one another running session
    /// has (`store::session_name`).
    pub(super) fn qualified(&self, t: &Ticket) -> String {
        let running: Vec<&str> = self
            .tickets
            .iter()
            .filter(|tv| tv.session.is_some() && !tv.t.is(&t.repo, &t.key))
            .filter_map(|tv| tv.name.as_deref())
            .collect();
        store::session_name(t, &self.projects, self.tickets.iter().map(|tv| &tv.t), &running)
    }

    /// `ctrl+a R`: stops ticket `i`'s session; the next tick starts it again, once its CLI is
    /// gone.
    pub(super) fn restart_session(&mut self, i: usize) {
        if let Some(tv) = self.tickets.get_mut(i) {
            tv.stop_session();
            tv.status.state = AgentState::NotStarted;
        }
    }

    /// `restart_session`, saying so.
    pub(super) fn restart_with_toast(&mut self, i: usize) {
        let Some(tv) = self.tickets.get(i) else { return };
        let cli = tv.t.cli.label();
        self.restart_session(i);
        self.toast(format!("Restarting {cli} session…"));
    }

    /// What `start_session` says when ticket `i`'s folder is `gone`: what's missing, and what to
    /// do about it.
    fn gone_toast(&self, i: usize, gone: Gone) -> String {
        let t = &self.tickets[i].t;
        let what = gone.what(t, &self.project_name(t));
        match gone {
            Gone::Worktree => {
                let on = t.branch.as_deref().map(|b| format!(" on {}", untrusted::line(b)));
                format!(
                    "{what}: Enter makes its worktree again{}, {} x closes the ticket",
                    on.unwrap_or_default(),
                    keys::prefix()
                )
            }
            Gone::Project => format!(
                "{what}, so {}'s session didn't start: put the folder back and press Enter, or \
                 delete the project in Settings and add it where it is now",
                t.display()
            ),
        }
    }

    /// The name of `t`'s project, for messages.
    pub(crate) fn project_name(&self, t: &Ticket) -> String {
        self.project_of(t)
            .map_or_else(|| store::project_name(&t.repo), |p| self.projects[p].name.clone())
    }

    /// Enter on ticket `i`, whose session didn't start because its folder is gone: a worktree
    /// removed outside taskman is made again where it was, on its branch, and the session starts
    /// in it once git is done. Anything else (the folder is back, or it was the project's) tries
    /// again.
    pub(super) fn remake_worktree(&mut self, i: usize) {
        let Some(tv) = self.tickets.get_mut(i) else { return };
        tv.stop_session();
        tv.gone = None;
        tv.status.state = AgentState::NotStarted;
        let t = &self.tickets[i].t;
        let Some(p) = self.project_of(t).filter(|_| Gone::of(t) == Some(Gone::Worktree)) else {
            return;
        };
        let t = &mut self.tickets[i].t;
        // A cwd that's empty or relative was never a worktree taskman made: the project's
        // settings say where one goes.
        let folder = Some(t.cwd.clone()).filter(|f| f.is_absolute());
        t.pending_worktree = Some(store::PendingWorktree {
            base: t.base.clone(),
            folder,
            branch: t.branch.clone(),
            ..Default::default()
        });
        self.create_worktree(p, i);
    }

    /// Enter on a session that ended because Claude runs its conversation in the background:
    /// stops that copy, off the UI thread. `background_stopped` resumes it here.
    pub(super) fn stop_background_copy(&mut self) {
        let Some(tv) = self.tickets.get_mut(self.sel) else { return };
        let (Some(b), Some(launch)) = (&mut tv.background, tv.launch.clone()) else { return };
        if b.stopping {
            return;
        }
        b.stopping = true;
        b.error = None;
        let id: Id = (tv.t.repo.clone(), tv.t.key.clone());
        let (job, stop) = (b.id.clone(), self.stop_background.clone());
        let failed = (id.clone(), launch.clone());
        self.reporter().spawn(
            "stopping the background copy".into(),
            move || Msg::BackgroundStopped(id, launch, stop(&job)),
            move |e| Msg::BackgroundStopped(failed.0, failed.1, Err(e)),
        );
    }

    /// `claude stop` finished. A session that started again meanwhile isn't the one it was for.
    pub(super) fn background_stopped(&mut self, id: &Id, launch: &str, res: Result<(), String>) {
        let Some(i) = self.tickets.iter().position(|tv| tv.t.is(&id.0, &id.1)) else { return };
        let tv = &mut self.tickets[i];
        if tv.launch.as_deref() != Some(launch) {
            return;
        }
        let Some(b) = &mut tv.background else { return };
        b.stopping = false;
        match res {
            // Resumes as Enter does on any ended session: the next tick starts it.
            Ok(()) => {
                tv.stop_session();
                tv.status.state = AgentState::NotStarted;
            }
            Err(e) => {
                b.error = Some(format!(
                    "{e}. Run `{}` yourself, then press Enter.",
                    background::by_hand(&b.id)
                ));
            }
        }
    }

    /// Enter on a session that's handed off or back: resumes it in the pane, with whatever it did
    /// outside. Started now, as the next poll would mark it back again.
    pub(super) fn bring_back(&mut self) {
        let Some(tv) = self.tickets.get_mut(self.sel) else { return };
        tv.away = None;
        tv.stop_session();
        tv.status.state = AgentState::NotStarted;
        self.start_session(self.sel);
    }

    /// `ctrl+a t`: stops the session in the pane, so it can continue in another terminal with
    /// `taskman resume`. The CLI is asked to quit and given time to; see `handed_off`.
    pub(super) fn hand_off(&mut self) {
        let Some(tv) = self.tickets.get_mut(self.sel) else { return };
        let name = tv.t.display().to_string();
        match tv.away {
            Some(Away::Leaving(_)) => return,
            Some(Away::Outside) => {
                return self.warn(format!("{name} is already running in another terminal"));
            }
            Some(Away::HandedOff | Away::Back) => return self.offer_resume(self.sel),
            None => {}
        }
        let unstarted = format!("{name}'s session hasn't started: there's nothing to hand off yet");
        if tv.waiting() {
            return self.warn(unstarted);
        }
        match &tv.session {
            Some(s) if !s.exited() => {
                s.hang_up();
                tv.away = Some(Away::Leaving(Instant::now()));
            }
            _ if tv.t.session_id.as_deref().is_some_and(|sid| tv.t.cli.has_conversation(sid)) => {
                self.handed_off(self.sel);
            }
            _ => self.warn(unstarted),
        }
    }

    /// The CLI quit after `ctrl+a t`, or took too long and is stopped: the pane won't start it
    /// again until the user brings it back, and the command that continues it is offered. The
    /// claim says it's handed off and goes once the CLI is gone (`release_handed_off`), so
    /// `taskman resume` takes over only then.
    pub(super) fn handed_off(&mut self, i: usize) {
        let tv = &mut self.tickets[i];
        tv.stop_session();
        let path = resume::owner_file(&tv.t.repo, &tv.t.key);
        if tv.hold.is_none() {
            tv.hold = resume::take(&path).ok().and_then(Result::ok);
        }
        if let Some(hold) = &mut tv.hold {
            hold.write(resume::HANDED_OFF);
        }
        tv.away = Some(Away::HandedOff);
        tv.status.state = AgentState::NotStarted;
        tv.release_handed_off();
        self.offer_resume(i);
    }

    /// Copies the command that continues ticket `i`'s session elsewhere and, inside tmux, offers a
    /// window running it.
    fn offer_resume(&mut self, i: usize) {
        let line = self.resume_line(i);
        self.copy_as(line.clone(), Some(format!("`{line}`: run it in another terminal")));
        let in_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
        if in_tmux && matches!(self.mode, Mode::Normal) {
            let t = &self.tickets[i].t;
            self.mode = Mode::OfferTmux((t.repo.clone(), t.key.clone()));
        }
    }

    /// `taskman resume KEY` for ticket `i`, with `--project` when the key needs it.
    pub fn resume_line(&self, i: usize) -> String {
        let t = &self.tickets[i].t;
        let project =
            self.project_of(t).map(|p| self.projects[p].name.as_str()).unwrap_or_default();
        resume::command_line(t, project, self.tickets.iter().map(|tv| &tv.t))
    }

    /// Runs `taskman resume` for ticket `id` in a new tmux window, in the session's folder.
    pub(super) fn open_in_tmux(&mut self, id: &Id) {
        let Some(i) = self.tickets.iter().position(|tv| tv.t.is(&id.0, &id.1)) else { return };
        let mut line = self.resume_line(i);
        // The link to the running binary: the window's PATH may have no taskman, or another one.
        if let Some(bin) = &self.bin_dir {
            line = line.replacen(
                "taskman",
                &resume::quote(&bin.join("taskman").display().to_string()),
                1,
            );
        }
        let t = &self.tickets[i].t;
        let mut cmd = std::process::Command::new("tmux");
        cmd.args(["new-window", "-n"]).arg(t.display()).arg("-c").arg(&t.cwd);
        for var in TMUX_ENV {
            if let Some(v) = std::env::var_os(var) {
                let mut pair = OsString::from(format!("{var}="));
                pair.push(v);
                cmd.arg("-e").arg(pair);
            }
        }
        cmd.arg(line);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let why = match crate::proc::run(&mut cmd, Duration::from_secs(5)) {
                Ok(out) if out.status.success() => return,
                Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
                Err(e) => e.to_string(),
            };
            let _ = tx.send(Msg::Toast(Level::Warn, format!("Couldn't open a tmux window: {why}")));
        });
    }
}

/// Whether to warn that a session's clipboard request was refused for `why`, at `now`: not when
/// `warned` (when it was last warned of each) has the same reason within `CLIPBOARD_WARN_EVERY`.
pub(super) fn warn_due(
    warned: &[(session::Refused, Instant)],
    why: session::Refused,
    now: Instant,
) -> bool {
    warned.iter().all(|(w, at)| !w.same(why) || now.duration_since(*at) >= CLIPBOARD_WARN_EVERY)
}
