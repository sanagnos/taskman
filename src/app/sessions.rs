//! The sessions in the pane: starting them (one copy each, under the owner lock), their names,
//! and handing them to another terminal (`ctrl+a t`, `taskman resume`) and back.

use super::launch::session_exists;
use super::poll::remove_status_at;
use super::sync::Msg;
use super::{AgentState, App, Away, Id, Mode};
use crate::background;
use crate::resume;
use crate::session::Session;
use crate::store::{self, Cli, Ticket};
use std::ffi::OsString;
use std::time::{Duration, Instant};

/// How long `ctrl+a t` waits for the CLI to quit before stopping it.
pub(super) const HAND_OFF_WAIT: Duration = Duration::from_secs(5);
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
    pub(super) fn ensure_selected_session(&mut self) {
        let area = self.areas.claude;
        let Some(tv) = self.tickets.get(self.sel) else { return };
        // Selecting a sleeping session starts it again, on the same conversation.
        if tv.dormant() && !tv.waiting() && area.width > 0 {
            self.start_session(self.sel);
        }
        // An agent can only message sessions that are running: once a project's Claude agent runs,
        // start its Claude ticket sessions too, one per tick. Codex sessions can't be messaged. A
        // ticket that's away won't start, and finding it every tick would hold up the rest. One
        // asleep stays asleep: waking it would undo the memory it was put to sleep to save, and
        // the agent is told it can't be messaged (`taskman status` says "asleep").
        if area.width > 0
            && let Some(i) = (0..self.tickets.len()).find(|&i| {
                let tv = &self.tickets[i];
                tv.t.agent.is_none()
                    && tv.t.cli == Cli::Claude
                    && tv.session.is_none()
                    && tv.status.state == AgentState::NotStarted
                    && !tv.asleep
                    && !tv.waiting()
                    && tv.away.is_none()
                    && self.tickets.iter().any(|a| {
                        a.t.agent.is_some()
                            && a.t.cli == Cli::Claude
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
        self.tickets[i].lock_error = None;
        let hold = match taken {
            Ok(Ok(hold)) => hold.taskman(),
            Ok(Err(resume::Owner::Closing)) => {
                // Enter tries again (`restart_session`) once the removal is done.
                let msg = resume::closing(self.tickets[i].t.display());
                self.tickets[i].status.state = AgentState::Exited;
                self.toast(msg);
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
                self.toast(msg);
                return;
            }
        };
        let area = self.areas.claude;
        let name = self.qualified(&self.tickets[i].t);
        let launch = super::launch::launch_id();
        let cmd = self.build_command(&self.tickets[i].t, &name, &launch);
        // What the status file says is an earlier start's: at taskman's next start it would pass
        // for the session's latest conversation (`take_leftover_status`).
        remove_status_at(&store::status_file(&self.tickets[i].t.repo, &self.tickets[i].t.key));
        let dirty = self.dirty.clone();
        match Session::spawn(cmd, area.height, area.width, dirty) {
            Ok(s) => {
                let tv = &mut self.tickets[i];
                tv.session = Some(s);
                tv.hold = Some(hold);
                tv.name = Some(name);
                tv.launch = Some(launch);
                tv.watch = super::sleep::Watch::new(Instant::now());
                tv.status.started();
            }
            Err(e) => {
                self.tickets[i].status.state = AgentState::Exited;
                let program = self.tickets[i].t.cli.label().to_lowercase();
                self.toast(format!("Could not start {program}: {e}"));
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

    /// `ctrl+a R`: stops the session; the next tick starts it again, once its CLI is gone.
    pub(super) fn restart_session(&mut self) {
        if let Some(tv) = self.tickets.get_mut(self.sel) {
            tv.stop_session();
            tv.status.state = AgentState::NotStarted;
        }
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
        let (id, job) = ((tv.t.repo.clone(), tv.t.key.clone()), b.id.clone());
        let (stop, tx) = (self.stop_background.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::BackgroundStopped(id, launch, stop(&job)));
        });
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
                return self.toast(format!("{name} is already running in another terminal"));
            }
            Some(Away::HandedOff | Away::Back) => return self.offer_resume(self.sel),
            None => {}
        }
        let unstarted = format!("{name}'s session hasn't started: there's nothing to hand off yet");
        if tv.waiting() {
            return self.toast(unstarted);
        }
        match &tv.session {
            Some(s) if !s.exited() => {
                s.hang_up();
                tv.away = Some(Away::Leaving(Instant::now()));
            }
            _ if tv.t.session_id.as_deref().is_some_and(|sid| session_exists(tv.t.cli, sid)) => {
                self.handed_off(self.sel);
            }
            _ => self.toast(unstarted),
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
            let _ = tx.send(Msg::Toast(format!("Couldn't open a tmux window: {why}")));
        });
    }
}
