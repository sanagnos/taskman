//! The TUI's state (`App`) and what changes it. This file holds the state and the tick that drives
//! it; the behaviour is split by area into child modules, each with its own `impl App` (a child
//! sees App's private fields, so the split needs no accessors):
//!
//! - `ticket`: one ticket or agent as the TUI holds it (`TicketView`)
//! - `poll`: each session's state, from its hooks' status file and from its screen
//! - `sync`: background fetches and worktree creation, and the messages they send back (`Msg`)
//! - `sessions`: starting sessions, their names, handing them to another terminal and back
//! - `sleep`: putting sessions nobody has looked at for a while to sleep, and waking them
//! - `launch`: how a session's CLI starts: its arguments, environment and brief
//! - `input`: keys, the prefix commands, paste; `mouse`: the wheel, selecting, the clipboard
//! - `forms`: fields and form keys; the forms are `add_form`, `settings`, `projects`, `accounts`
//! - `tickets`: adding, closing and renaming tickets and agents, and stacking
//! - `inbox`: the TUI's side of `taskman add` and `taskman close`
//! - `test_support`: an `App` with nothing read or started, for tests; `events`, the tests that
//!   drive it through its keys, mouse and messages
//!
//! Tokens and tracker clients are `tracker::Clients`; drawing is `ui.rs`.

use crate::background;
use crate::selection::{self, Panel, Selection};
use crate::store::{self, Config, Project, Saved, Ticket, UiPrefs};
use crate::tracker::{self, Access};
use crate::ui::{self, Areas};
use crate::{add, github, links, resume, theme, update, worktree};
use launch::link_bin;
use poll::take_leftover_status;
use ratatui::layout::{Position, Rect};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use sync::{Msg, STALE};
use tickets::ClosedCreating;
pub use toasts::{Level, LogView, Toast};

mod accounts;
mod add_form;
#[cfg(test)]
mod events;
mod forms;
pub(crate) mod help;
mod inbox;
mod input;
mod jump;
mod launch;
mod line_edit;
mod mouse;
mod poll;
mod projects;
mod queue;
mod reorder;
mod sessions;
mod settings;
mod sleep;
mod sync;
#[cfg(test)]
pub(crate) mod test_support;
mod ticket;
mod tickets;
pub mod toasts;

pub use forms::{Field, Form, FormKind, List};
pub use launch::{NESTED_ENV, launch_id};
pub use poll::{AgentState, Status};
pub use reorder::Piece;
pub use settings::{Settings, SettingsItem};
pub use sync::Remote;
pub use ticket::{Away, Blocked, TicketView};

/// Stops a Claude background session by its id.
type Stopper = dyn Fn(&str) -> Result<(), String> + Send + Sync;

/// Shows a notification with the setting's ways, returning whether it's left to the terminal.
type Notifier = dyn Fn(crate::notify::Setting, &str) -> bool + Send + Sync;

/// How often the status files are read and the inbox is checked.
const STATUS_EVERY: Duration = Duration::from_millis(300);

/// How far Help is scrolled; `max` and `page` come from the last frame drawn, 0 when it fits.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct HelpView {
    pub top: u16,
    pub max: u16,
    pub page: u16,
}

pub enum Mode {
    Normal,
    Help(HelpView),
    Form(Box<Form>),
    Settings(Box<Settings>),
    ConfirmClose,
    ConfirmDeleteProject(usize),
    ConfirmDeleteAccount(usize),
    ConfirmQuit,
    /// `prefix R` on a session that's working or needs the user, which restarting interrupts.
    ConfirmRestart(Id),
    /// A `taskman add` that adds a project agent or sets an agent's role, waiting for the user.
    ConfirmAgent(Box<AgentAsk>),
    /// A session just handed off, inside tmux: open `taskman resume` in a new window?
    OfferTmux(Id),
    /// The ticket's and the session's links (`prefix l`), the selected one highlighted.
    Links(Box<LinkList>),
    /// The jump box (`prefix /`): the sidebar narrows to what's typed.
    Jump(jump::Jump),
    /// The waiting-on-you queue (`prefix w`).
    Queue(queue::Queue),
    /// Moving the selected card, or its project, in the sidebar (`prefix {` and `}`).
    Move(Box<reorder::Moving>),
    /// Every toast shown (`prefix ~`), newest first.
    Log(LogView),
}

pub struct LinkList {
    pub links: Vec<links::Link>,
    pub sel: usize,
    /// How many of the links, at the top, are the ticket's panels' rather than the session's.
    pub panel: usize,
    /// The list row at the top of the view, moved as it's drawn to keep `sel` in view.
    pub top: usize,
}

/// An agent `taskman add` asked for. A session can run `taskman add` without a prompt, and text it
/// read (an issue, a PR comment) can tell it to, so an agent, which starts at once with its role
/// in its system prompt, waits for the user. See `take_inbox`.
pub struct AgentAsk {
    pub req: add::Request,
    /// Whether the agent is open already, so the request only changes its role.
    pub open: bool,
    /// When the dialog opened: keys are ignored for `ASK_ARMS`, so typing meant for a session, an
    /// Enter above all, can't answer a dialog that just appeared.
    pub shown: Instant,
}

/// How long a dialog that opened by itself ignores keys.
pub const ASK_ARMS: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Ticket(usize),
    Project(usize),
}

/// A ticket or agent, by its project's main checkout and its key: a key alone can repeat across
/// projects (see `Ticket::is`).
pub type Id = (PathBuf, String);

pub struct App {
    pub tickets: Vec<TicketView>,
    pub sel: usize,
    pub prefs: UiPrefs,
    /// The fields at the top of tickets.json this build doesn't know, saved back as they were.
    unknown: store::Extra,
    pub prefix: bool,
    pub mode: Mode,
    pub projects: Vec<Project>,
    pub config: Config,
    /// The accounts' tokens, the tracker clients built from them, and Local files boards' listings.
    trackers: tracker::Clients,
    /// Something on screen changed: draw a frame. Not for a session's output, which each session
    /// keeps (`Session::fresh`), so that only the selected one's draws.
    pub dirty: Arc<AtomicBool>,
    /// Given to each session started, to wake the event loop with its output.
    pub wake: crate::session::Wake,
    pub areas: Areas,
    /// The cards and project headers drawn, at the rows they're drawn on, for clicks.
    pub sidebar_hits: Vec<(Rect, Hit)>,
    /// How far the sidebar's list is scrolled; see `ui::sidebar::draw_sidebar`.
    pub sidebar_scroll: ui::SidebarScroll,
    /// A press in the sidebar that may become a drag to reorder it; see `reorder.rs`.
    pub drag: Option<reorder::Drag>,
    /// The project whose header was last clicked or dragged, until a card is selected; see
    /// `current_project`. Its folder, so a reorder or a removal can't point it at another one.
    pub header_project: Option<PathBuf>,
    pub toast: Option<Toast>,
    /// Every toast shown, newest first, up to `toasts::LOG_CAP`; see `prefix ~`.
    pub toast_log: toasts::Log,
    pub quit: bool,
    /// A file to open in the user's editor; the event loop hands it the terminal meanwhile.
    pub edit: Option<PathBuf>,
    /// Ring the terminal bell on the next frame (a background ticket needs you).
    pub bell: bool,
    /// OSC 52 clipboard writes from sessions, to forward to the real terminal.
    pub clipboard_out: Vec<String>,
    /// Notifications left to the terminal (`notify::escape`), to write on the next frame.
    pub notify_out: Vec<String>,
    /// Whether the terminal has the focus, as it last said (focus reports); None until it does.
    pub focused: Option<bool>,
    /// Shows a notification (`notify::send`, run on a thread), returning whether it's left to the
    /// terminal; tests swap it so they never reach the desktop, and record what would have.
    notifier: Arc<Notifier>,
    /// Puts text on the clipboard (`selection::copy`, run on a thread); tests swap it so they
    /// never touch the user's clipboard.
    clipboard: Arc<dyn Fn(&str) -> selection::Copied + Send + Sync>,
    /// Stops a Claude background session by its id (`background::stop`, run on a thread); tests
    /// swap it so they never touch the user's real ones.
    stop_background: Arc<Stopper>,
    /// Opens a URL in the user's browser (`util::open_url`); tests swap it so they never open
    /// one, and record what would have.
    open_url: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// A drag-selection of rendered text in the Claude, Jira or GitHub pane.
    pub selection: Option<Selection>,
    /// The last left click: where, when, and how many in a row (see `mouse::click_count`).
    pub(crate) last_click: Option<(Position, Instant, u8)>,
    /// The panel holding the selection, rendered whole; set as the panel is drawn.
    pub panel: Option<Panel>,
    /// The running binary, which the sessions' hooks run (`Cli::hooks`).
    exe: PathBuf,
    /// Holds a `taskman` link to the running binary, put first on the sessions' PATH.
    bin_dir: Option<PathBuf>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    /// Unit tests: the threads that report back (`sync::Reporter`) panic before their work, as a
    /// bug in it would.
    #[cfg(test)]
    pub(super) panics: bool,
    last_status_poll: Instant,
    /// When `inbox/alive` was last written, which `take_inbox` does at most once a second.
    heartbeat_at: Option<Instant>,
    /// The Add form's project's worktrees, listed once so typing a key doesn't run git.
    form_worktrees: std::cell::RefCell<Option<(PathBuf, github::Worktrees)>>,
    /// Each project's open issues for the Add form, by folder, with when they were asked for.
    open_issues: std::collections::HashMap<PathBuf, (Instant, add_form::OpenIssues)>,
    /// Each project's default branch, read from git once.
    default_branches:
        std::cell::RefCell<std::collections::HashMap<PathBuf, crate::worktree::DefaultBranch>>,
    /// Whether each project's folder can show PRs (`github::probe`), by folder: as saved last
    /// time until the check at start answers, and missing for a folder not checked yet.
    github: std::collections::BTreeMap<PathBuf, github::Gh>,
    /// While the close dialog is open: whether the ticket's worktree can be removed with it.
    pub close_check: Option<worktree::RemoveCheck>,
    /// While the close dialog is open: why the ticket's worktree couldn't be checked (git failed),
    /// so the dialog offers nothing and says so; the worktree stays.
    pub close_unchecked: Option<String>,
    /// Agents `taskman add` asked for, oldest first, each waiting for its dialog.
    asking: std::collections::VecDeque<add::Request>,
    /// `taskman close` requests whose PRs were looked up, by inbox id, waiting until nothing
    /// refers to a ticket by its place in the list (`close_next`).
    closing: std::collections::VecDeque<(String, add::Request)>,
    /// Worktree removals still running, with the names of their tickets: quitting waits for them
    /// (`may_quit`).
    removing: Vec<(Vec<String>, std::thread::JoinHandle<()>)>,
    /// The CLIs of closed tickets still being reaped, by ticket (`keep_reaper`): a ticket added
    /// again takes its own back, so its new session waits for the old CLI as a restart does, and
    /// quitting waits for them all (`stop_sessions`).
    reapers: Vec<(Id, crate::session::Reaper)>,
    /// Tickets closed while git was still creating their worktree, by creation id, until git is
    /// done (`created_after_close`); quitting waits for them (`may_quit`).
    closed_creating: std::collections::HashMap<u64, ClosedCreating>,
    /// The last worktree creation's id (`create_worktree`).
    creations: u64,
    /// The last tracker or PR fetch's id (`Remote::start`), or open issues list's
    /// (`add_form::Listing`), which its reply carries so a late one is told from the latest.
    fetches: u64,
    pub updater: update::Updater,
    /// Why tickets.json and config.json couldn't be saved last time, if they couldn't.
    save_errors: [Option<String>; 2],
    /// The last form check's id.
    checks: u64,
    /// The OS keychain the account form can save a token to, once found (`Keychain::probe`); None
    /// where there's none, as on WSL or a server, and until the check at start is done.
    pub keychain: Option<crate::keychain::Keychain>,
    /// Whether that check has answered: until it has, the account form offers no File, which on a
    /// machine with a keychain would be the wrong place.
    pub keychain_probed: bool,
    /// What the bug report link fills in about the machine (`help::Machine`), once looked up.
    pub machine: Option<help::Machine>,
    /// Whether that lookup has started: it runs once, when Help & feedback first shows.
    machine_asked: bool,
    /// What the terminal said about its colours at start (`theme::ask`), and the theme drawn
    /// with: from that and `config.theme` (`apply_theme`).
    pub probe: theme::Probe,
    pub theme: theme::Theme,
}

impl App {
    pub fn new(exe: &Path, mut saved: Saved, config: Config) -> Self {
        let resumed = take_leftover_status(
            &mut saved.tickets,
            |t| store::status_file(&t.repo, &t.key),
            store::legacy_status_file,
            // A session running in another terminal still writes its status file.
            |t| {
                matches!(
                    resume::peek(&resume::owner_file(&t.repo, &t.key)),
                    resume::Owner::Outside(_)
                )
            },
        );
        let mut app = Self::from_saved(exe, saved, config, update::Updater::new(exe));
        app.bin_dir = link_bin(exe);
        app.rebuild_clients();
        app.probe_keychain();
        // The first frame draws from the saved answers; gh can take a second to give its own.
        app.check_github(app.projects.iter().map(|p| p.repo.clone()).collect());
        if resumed {
            app.save();
        }
        app
    }

    /// The state alone, with nothing read or started: no tokens, no PR panels, no session.
    fn from_saved(exe: &Path, mut saved: Saved, config: Config, updater: update::Updater) -> Self {
        let github = std::mem::take(&mut saved.ui.github);
        let (tx, rx) = mpsc::channel();
        let tickets: Vec<TicketView> = saved.tickets.into_iter().map(TicketView::new).collect();
        let sel = saved.selected.min(tickets.len().saturating_sub(1));
        let theme = theme::Theme::new(config.theme, &theme::Probe::default());
        Self {
            tickets,
            sel,
            prefs: saved.ui,
            unknown: saved.extra,
            prefix: false,
            mode: Mode::Normal,
            projects: saved.projects,
            config,
            trackers: Default::default(),
            dirty: Arc::new(AtomicBool::new(true)),
            wake: Default::default(),
            areas: Areas::default(),
            sidebar_hits: Vec::new(),
            sidebar_scroll: ui::SidebarScroll::default(),
            drag: None,
            header_project: None,
            toast: None,
            toast_log: Default::default(),
            quit: false,
            edit: None,
            bell: false,
            clipboard_out: Vec::new(),
            notify_out: Vec::new(),
            focused: None,
            notifier: Arc::new(crate::notify::send),
            clipboard: Arc::new(selection::copy),
            stop_background: Arc::new(background::stop),
            open_url: Arc::new(crate::util::open_url),
            selection: None,
            last_click: None,
            panel: None,
            exe: exe.to_path_buf(),
            bin_dir: None,
            tx,
            rx,
            #[cfg(test)]
            panics: false,
            last_status_poll: Instant::now().checked_sub(STATUS_EVERY).unwrap_or_else(Instant::now),
            heartbeat_at: None,
            form_worktrees: Default::default(),
            open_issues: Default::default(),
            default_branches: Default::default(),
            github,
            close_check: None,
            close_unchecked: None,
            asking: Default::default(),
            closing: Default::default(),
            removing: Vec::new(),
            reapers: Vec::new(),
            closed_creating: Default::default(),
            creations: 0,
            fetches: 0,
            updater,
            save_errors: Default::default(),
            checks: 0,
            keychain: None,
            keychain_probed: false,
            machine: None,
            machine_asked: false,
            probe: theme::Probe::default(),
            theme,
        }
    }

    /// Saves the state; a failure is toasted, and toasted again until a save works. A change
    /// that's hard to undo, or that lets go of a session's claim, uses `try_save` instead.
    fn save(&mut self) {
        let _ = self.try_save();
        self.show_save_error();
    }

    /// Saves the state, for a change that's undone when it can't be saved, and says so itself:
    /// Err with why. `taskman resume` finds sessions in the saved state, so a session's owner lock
    /// goes, and a folder it could run in is deleted, only once the state no longer lists it.
    fn try_save(&mut self) -> Result<(), String> {
        let saved = Saved {
            projects: self.projects.clone(),
            tickets: self.tickets.iter().map(|tv| tv.t.clone()).collect(),
            selected: self.sel,
            ui: store::UiPrefs {
                github: (self.github.iter())
                    .filter(|(repo, _)| self.projects.iter().any(|p| &p.repo == *repo))
                    .map(|(repo, gh)| (repo.clone(), *gh))
                    .collect(),
                ..self.prefs.clone()
            },
            format: store::Format,
            extra: self.unknown.clone(),
        };
        let path = store::state_dir().join("tickets.json");
        let res = store::save(&saved);
        self.save_errors[0] = res.as_ref().err().map(|e| unsaved(&path, e));
        res.map_err(|e| format!("couldn't save {}: {e}", store::tilde(&path)))
    }

    fn save_config(&mut self) {
        self.save_errors[1] =
            store::save_config(&self.config).err().map(|e| unsaved(&store::config_path(), &e));
        self.show_save_error();
    }

    /// Toasts a failed save, and keeps toasting it until a save works: later toasts would hide a
    /// single one, and changes are being lost meanwhile.
    fn show_save_error(&mut self) {
        let shown =
            self.toast.as_ref().is_some_and(|t| self.save_errors.contains(&Some(t.msg.clone())));
        if !shown && let Some(e) = self.save_errors.iter().flatten().next().cloned() {
            self.error(e);
        }
    }

    /// Reads the tokens not read yet, in the background, and builds the clients projects need
    /// (`tracker::Clients::rebuild`). Runs at startup and whenever accounts or projects change.
    fn rebuild_clients(&mut self) {
        let report = self.reporter();
        self.trackers.rebuild(&self.config, &self.projects, move |a| {
            let what = format!("reading the token of {}", a.label());
            let token = report.reported(&what, || a.resolve_token()).and_then(|t| t);
            report.send(Msg::Token(a.token, token));
        });
    }

    /// Asks, in the background, whether there's an OS keychain to keep tokens in. The Keep in
    /// field waits for the answer (`keychain_probed`), which a panic makes "none".
    fn probe_keychain(&self) {
        self.reporter().spawn(
            "looking for the keychain".into(),
            || {
                let keychain = crate::keychain::Keychain::os();
                Msg::Keychain(keychain.probe().then_some(keychain))
            },
            |_| Msg::Keychain(None),
        );
    }

    pub fn project_of(&self, t: &Ticket) -> Option<usize> {
        self.projects.iter().position(|p| p.repo == t.repo)
    }

    /// The issue the ticket's PR closes, when its tracker is the repository's own issues.
    pub fn closes(&self, t: &Ticket) -> Option<crate::github::Closes> {
        self.projects.get(self.project_of(t)?)?.tracker.as_ref()?.closes(&t.key)
    }

    pub fn access(&self, t: &Ticket) -> Access {
        self.trackers.access(&self.config, self.project_of(t).map(|p| &self.projects[p]))
    }

    /// The ticket's page on its tracker.
    pub fn issue_url(&self, t: &Ticket) -> Option<String> {
        self.projects.get(self.project_of(t)?)?.tracker.as_ref()?.issue_url(&t.key)
    }

    /// Ticket indices grouped by project, in project order; `visible_only` skips collapsed groups.
    pub fn ordered(&self, visible_only: bool) -> Vec<usize> {
        let mut out = Vec::new();
        for (p, proj) in self.projects.iter().enumerate() {
            if visible_only && proj.collapsed {
                continue;
            }
            out.extend(self.members(p));
        }
        out
    }

    /// The project's agents, then its tickets, in the order they were added.
    pub fn members(&self, p: usize) -> Vec<usize> {
        let mine: Vec<usize> = (0..self.tickets.len())
            .filter(|&i| self.project_of(&self.tickets[i].t) == Some(p))
            .collect();
        let (agents, tickets): (Vec<usize>, Vec<usize>) =
            mine.into_iter().partition(|&i| self.tickets[i].t.agent.is_some());
        agents.into_iter().chain(tickets).collect()
    }

    /// Quits as Enter on the quit dialog does, and as a signal does: the selected ticket's news
    /// counts as seen, the state is saved, and the loop ends once `may_quit` allows.
    pub fn request_quit(&mut self) {
        if self.quit {
            return;
        }
        if let Some(tv) = self.tickets.get_mut(self.sel) {
            tv.mark_seen();
        }
        self.save();
        self.quit = true;
    }

    /// Stops every session, together (`session::stop_all`), so quitting takes a moment in all
    /// rather than one per session, and waits for the CLIs of sessions stopped earlier that are
    /// still being reaped, so none runs on unattended.
    pub fn stop_sessions(&mut self) {
        let sessions = self.tickets.iter_mut().filter_map(|tv| tv.session.take()).collect();
        let reapers = self
            .tickets
            .iter_mut()
            .filter_map(TicketView::take_reaper)
            .chain(self.reapers.drain(..).map(|(_, r)| r))
            .collect();
        crate::session::stop_all(sessions, reapers);
    }

    /// Keeps a closed ticket's reaper, if its CLI is still being reaped, until the ticket comes
    /// back (`take_reaper`) or taskman quits.
    pub(crate) fn keep_reaper(&mut self, id: Id, reaper: Option<crate::session::Reaper>) {
        self.reapers.retain(|(_, r)| !r.done());
        if let Some(r) = reaper.filter(|r| !r.done()) {
            self.reapers.push((id, r));
        }
    }

    /// The reaper of a ticket closed a moment ago and added again, for its new `TicketView`.
    fn take_reaper(&mut self, repo: &Path, key: &str) -> Option<crate::session::Reaper> {
        let i = self.reapers.iter().position(|(id, _)| id.0 == repo && id.1 == key)?;
        Some(self.reapers.remove(i).1)
    }

    /// Whether the event loop may end after a quit: only once no worktree removal runs, which
    /// would otherwise stop between deleting the folder and its branch, and never reply, and no git
    /// still creates the worktree of a ticket that closed meanwhile, which would be left behind.
    /// Until then the toast says what it waits for.
    pub fn may_quit(&mut self) -> bool {
        if !self.quit {
            return false;
        }
        self.removing.retain(|(_, removal)| !removal.is_finished());
        // A thread that had ended at the last check sent its message before that tick's drain;
        // still here, it sent none.
        self.closed_creating.retain(|_, c| !c.ended);
        for c in self.closed_creating.values_mut() {
            c.ended = c.thread.as_ref().is_none_or(|t| t.is_finished());
        }
        // A closed ticket's worktree git still creates is removed once git is done.
        let creating = self.closed_creating.values().filter(|c| !c.keep).map(|c| c.key.as_str());
        let names: Vec<&str> =
            self.removing.iter().flat_map(|(n, _)| n).map(String::as_str).chain(creating).collect();
        if names.is_empty() {
            return true;
        }
        let text = quit_waits_for(&names);
        self.toast(text);
        false
    }

    /// The tickets whose worktrees a quit would wait for now (see `may_quit`), for the quit
    /// dialog to name.
    pub fn quit_would_wait_for(&self) -> Vec<&str> {
        let creating = self
            .closed_creating
            .values()
            .filter(|c| !c.keep && c.thread.as_ref().is_some_and(|t| !t.is_finished()));
        let removing = self.removing.iter().filter(|(_, removal)| !removal.is_finished());
        removing
            .flat_map(|(n, _)| n)
            .map(String::as_str)
            .chain(creating.map(|c| c.key.as_str()))
            .collect()
    }

    pub fn selected(&self) -> Option<&TicketView> {
        self.tickets.get(self.sel)
    }

    /// Whether the selected session has written anything since its pane was last drawn. The
    /// others' output isn't drawn for: the sidebar learns of it from the status poll.
    pub fn selected_fresh(&self) -> bool {
        self.selected()
            .and_then(|tv| tv.session.as_ref())
            .is_some_and(crate::session::Session::fresh)
    }

    /// Takes what calls for a frame (`dirty`, the selected session's output), as one is drawn.
    pub fn take_redraw(&self) {
        self.dirty.store(false, Ordering::Relaxed);
        if let Some(s) = self.selected().and_then(|tv| tv.session.as_ref()) {
            s.drawn();
        }
    }

    pub fn tick(&mut self, screen: Rect) {
        self.lay_out(screen);
        self.follow_selection();
        self.follow_drag();
        self.drain_messages();
        if self.last_status_poll.elapsed() >= STATUS_EVERY {
            self.last_status_poll = Instant::now();
            self.poll_status();
            self.take_inbox();
        }
        self.ask_next();
        self.close_next();
        self.schedule_fetches();
        self.resume_worktrees();
        if self.updater.poll(self.config.auto_update) {
            self.dirty.store(true, Ordering::Relaxed);
        }
        self.ensure_selected_session();
        self.sleep_tick(Instant::now());
        for tv in &self.tickets {
            if let Some(s) = &tv.session {
                self.clipboard_out.extend(s.take_clipboard());
            }
        }
        if let Some(tv) = self.tickets.get_mut(self.sel) {
            tv.status.seen = true;
            tv.new = false;
        }
        self.expire_toast();
    }

    /// Lays the screen out and fits the selected session to its pane. From `tick`, with the
    /// terminal's size, and from `ui::draw`, with the frame's: the terminal can shrink between the
    /// two, and the frame's buffer is then the new size while the session's pane and its links
    /// are written into it directly, past its end with the old layout.
    pub fn lay_out(&mut self, screen: Rect) {
        let areas = ui::layout(screen, &self.prefs, self.panels());
        if areas != self.areas {
            // A selection is held in screen cells, which now show something else.
            self.selection = None;
            self.areas = areas;
        }
        if let Some(s) = self.tickets.get_mut(self.sel).and_then(|tv| tv.session.as_mut()) {
            s.resize(areas.claude.height, areas.claude.width);
        }
    }

    /// Which of the tracker and PR panels show beside the selected session: only what its
    /// project has connected, and only what `j` and `g` leave on. An agent's board takes the
    /// column once its project has a tracker or tickets.
    pub fn panels(&self) -> (bool, bool) {
        let Some(tv) = self.selected() else { return (false, false) };
        let tracker = self.project_of(&tv.t).is_some_and(|p| self.projects[p].tracker.is_some());
        if tv.t.agent.is_some() {
            let board = self.agent_board(tv);
            return (board && self.prefs.show_jira, board && self.prefs.show_gh);
        }
        (
            tracker && self.prefs.show_jira,
            self.github.get(&tv.t.repo) == Some(&github::Gh::Ready) && self.prefs.show_gh,
        )
    }

    /// Whether agent `tv`'s project has a board to show in its panels' place: a tracker, or
    /// tickets.
    pub fn agent_board(&self, tv: &TicketView) -> bool {
        let tracker = self.project_of(&tv.t).is_some_and(|p| self.projects[p].tracker.is_some());
        tracker || self.tickets.iter().any(|x| x.t.repo == tv.t.repo && x.t.agent.is_none())
    }

    /// Selects ticket `i`: by key, number or click, so the sidebar shows its card even when it's
    /// selected already and the wheel had scrolled it out of view.
    pub fn select(&mut self, i: usize) {
        if i >= self.tickets.len() {
            return;
        }
        self.header_project = None;
        self.sidebar_scroll.asked = true;
        if i != self.sel {
            self.selection = None;
            if let Some(prev) = self.tickets.get_mut(self.sel) {
                prev.leave();
            }
            self.sel = i;
            if let Some(p) = self.project_of(&self.tickets[i].t) {
                self.projects[p].collapsed = false;
            }
            let tv = &mut self.tickets[i];
            tv.status.seen = true;
            tv.new = false;
            // Read once it's in front of the user, whatever its panels show; a refresh that
            // lands while it stays selected is read too (`mark_selected`).
            tv.mark_seen();
            tv.jira.request_if_older_than(STALE, true);
            tv.pr.request_if_older_than(STALE, true);
            self.save();
        }
    }

    /// Marks the selected ticket's comments and reviews read, as `select` does: for a refresh that
    /// landed while it's selected, or a selection that moved without `select` (the ticket closed
    /// or its project removed). Returns whether anything changed, to save.
    pub(super) fn mark_selected(&mut self) -> bool {
        self.tickets.get_mut(self.sel).is_some_and(TicketView::mark_seen)
    }

    /// Moves through visible tickets in sidebar order.
    fn step(&mut self, dir: isize) {
        let order = self.ordered(true);
        if order.is_empty() {
            return;
        }
        let n = order.len().cast_signed();
        let i = match order.iter().position(|&i| i == self.sel) {
            Some(here) => (here.cast_signed() + dir).rem_euclid(n),
            None => 0,
        };
        self.select(order[i.cast_unsigned()]);
    }

    /// The project `ctrl+a n` and `ctrl+a ,` open on: the one whose header was last clicked or
    /// dragged, so an empty project can be picked too, else the selected card's.
    pub fn current_project(&self) -> Option<usize> {
        let header = self.header_project.as_ref();
        header
            .and_then(|repo| self.projects.iter().position(|p| &p.repo == repo))
            .or_else(|| self.tickets.get(self.sel).and_then(|tv| self.project_of(&tv.t)))
    }

    pub fn toggle_project(&mut self, p: usize) {
        let proj = &mut self.projects[p];
        proj.collapsed = !proj.collapsed;
        self.save();
    }
}

/// The toast while a quit waits for the worktree removals of tickets `names`.
fn quit_waits_for(names: &[&str]) -> String {
    match names {
        [one] => format!("Quitting once {one}'s worktree is removed"),
        many => format!("Quitting once the worktrees of {} are removed", many.join(", ")),
    }
}

fn unsaved(path: &Path, e: &anyhow::Error) -> String {
    format!("Couldn't save {}: {e}; changes are kept only until taskman quits", store::tilde(path))
}

#[cfg(test)]
mod tests {
    use super::{quit_waits_for, take_leftover_status};
    use crate::store::{self, Saved, Ticket};

    #[test]
    fn says_which_removals_a_quit_waits_for() {
        assert_eq!(quit_waits_for(&["ENG-52"]), "Quitting once ENG-52's worktree is removed");
        assert_eq!(
            quit_waits_for(&["ENG-52", "ENG-53"]),
            "Quitting once the worktrees of ENG-52, ENG-53 are removed"
        );
    }

    /// tickets.json as older versions saved it, before tickets were told apart by project.
    const SAVED_BY_KEY: &str = r#"{
  "projects": [
    { "name": "acme", "repo": "/src/acme", "tracker": { "provider": "linear", "site": "acme", "key": "ENG" }, "collapsed": false,
      "closed_agents": [ { "name": "tester", "session_id": "sid-tester", "role": "Test PRs" } ] },
    { "name": "notes", "repo": "/src/notes", "tracker": { "provider": "local", "site": "workplan", "key": "WP" }, "collapsed": true }
  ],
  "tickets": [
    { "key": "ENG-52", "repo": "/src/acme", "cwd": "/src/acme-ENG-52", "branch": "eng-52-login", "session_id": "sid-52",
      "seen_jira": ["c1"], "seen_gh": ["r:9"], "created_worktree": true },
    { "key": "ENG-53", "repo": "/src/acme", "cwd": "/src/acme-ENG-53", "cli": "codex", "branch": "eng-53", "session_id": "sid-53",
      "seen_jira": null, "seen_gh": null, "base": { "key": "ENG-52", "branch": "eng-52-login" } },
    { "key": "acme-lead", "repo": "/src/acme", "cwd": "/src/acme", "branch": null, "session_id": "sid-lead",
      "seen_jira": null, "seen_gh": null, "agent": "lead", "role": "Coordinate the tickets" },
    { "key": "WP-1", "repo": "/src/notes", "cwd": "/src/notes", "branch": null, "session_id": null, "seen_jira": null,
      "seen_gh": null, "pending_worktree": { "error": "fetch failed" } }
  ],
  "selected": 2,
  "ui": { "sidebar_collapsed": false, "show_jira": true, "show_comments": true, "show_gh": false, "codex_explained": true }
}"#;

    #[test]
    fn upgrades_saved_state_and_status_files_from_0_4_16() {
        let dir = std::env::temp_dir().join(format!("taskman-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("status")).unwrap();
        std::fs::write(dir.join("tickets.json"), SAVED_BY_KEY).unwrap();
        let mut saved: Saved = store::read_json(&dir.join("tickets.json")).unwrap();

        // Every ticket and agent loads as it was, conversations included.
        let ids: Vec<_> =
            saved.tickets.iter().map(|t| (t.key.as_str(), t.session_id.as_deref())).collect();
        assert_eq!(
            ids,
            [
                ("ENG-52", Some("sid-52")),
                ("ENG-53", Some("sid-53")),
                ("acme-lead", Some("sid-lead")),
                ("WP-1", None)
            ]
        );
        assert_eq!(saved.projects[0].closed_agents[0].session_id, "sid-tester");
        assert_eq!(saved.tickets[1].base.as_ref().map(|b| b.key.as_str()), Some("ENG-52"));

        // The status folder as older versions left it: by key alone. ENG-52's conversation changed
        // after the last save (taskman was killed, say); the agent's file has what was saved.
        let status = dir.join("status");
        let old = |key: &str| store::legacy_status_file_in(&status, key);
        let record = |sid: &str, ts: i64| {
            crate::hook::status_record(crate::status_file::HookState::Done, Some(sid), None, "", ts)
                .to_string()
        };
        std::fs::write(old("ENG-52"), record("sid-52-new", 20)).unwrap();
        std::fs::write(old("ENG-52").with_extension("gh"), "1").unwrap();
        std::fs::write(old("acme-lead"), record("sid-lead", 10)).unwrap();
        let in_dir = |t: &Ticket| store::status_file_in(&status, &t.repo, &t.key);
        assert!(take_leftover_status(&mut saved.tickets, in_dir, old, |_| false));
        let ids: Vec<_> = saved.tickets.iter().map(|t| t.session_id.as_deref()).collect();
        assert_eq!(ids, [Some("sid-52-new"), Some("sid-53"), Some("sid-lead"), None]);
        assert!(
            !old("ENG-52").exists()
                && !old("ENG-52").with_extension("gh").exists()
                && !old("acme-lead").exists()
        );

        // Saved again, nothing else changed.
        let mut before: serde_json::Value = serde_json::from_str(SAVED_BY_KEY).unwrap();
        before["tickets"][0]["session_id"] = "sid-52-new".into();
        let after = serde_json::to_value(&saved).unwrap();
        for (a, b) in
            after["tickets"].as_array().unwrap().iter().zip(before["tickets"].as_array().unwrap())
        {
            for (k, v) in b.as_object().unwrap() {
                assert_eq!(&a[k], v, "{k} of {}", b["key"]);
            }
        }
        assert_eq!(after["projects"][0]["closed_agents"], before["projects"][0]["closed_agents"]);

        // Each session resumes its conversation under the name it had: its key, still unique.
        let t = &saved.tickets[0];
        let name = store::session_name(t, &saved.projects, &saved.tickets, &[]);
        let args = t.cli.args(&name, "Brief", t.session_id.as_deref(), &[]);
        assert_eq!(args[..4], ["--resume", "sid-52-new", "--name", "ENG-52"]);
        let codex = &saved.tickets[1];
        let args = codex.cli.args(&codex.key, "Brief", codex.session_id.as_deref(), &[]);
        assert_eq!(args[args.len() - 2..], ["resume", "sid-53"]);

        // Once another project has the key, a file by key alone could be either's: it's cleared
        // and never taken.
        saved.tickets.push(
            serde_json::from_value(
                serde_json::json!({ "key": "WP-1", "repo": "/src/acme", "cwd": "/src/acme" }),
            )
            .unwrap(),
        );
        std::fs::write(old("WP-1"), record("sid-wp", 30)).unwrap();
        std::fs::create_dir_all(in_dir(&saved.tickets[4]).parent().unwrap()).unwrap();
        std::fs::write(in_dir(&saved.tickets[4]), record("sid-acme-wp", 40)).unwrap();
        assert!(take_leftover_status(&mut saved.tickets, in_dir, old, |_| false));
        assert_eq!(saved.tickets[3].session_id, None);
        assert_eq!(saved.tickets[4].session_id.as_deref(), Some("sid-acme-wp"));
        assert!(!old("WP-1").exists());

        // The agent's last session switched onto ENG-52's conversation (a Claude Code race): the
        // agent keeps its own, so the two never resume one conversation.
        std::fs::write(in_dir(&saved.tickets[2]), record("sid-52-new", 50)).unwrap();
        assert!(!take_leftover_status(&mut saved.tickets, in_dir, old, |_| false));
        assert_eq!(saved.tickets[2].session_id.as_deref(), Some("sid-lead"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
