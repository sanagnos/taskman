//! `taskman resume KEY`: continues a ticket's or agent's session in the current terminal, started
//! as the TUI would start it (`App::launch`): in its folder, with its brief, hooks, name and
//! environment, so the sidebar still shows its state and other sessions can still message it.
//!
//! Also the record that keeps a session to one copy. Whoever runs it, the TUI or this command,
//! holds a lock on a file beside its status file (`owner_file`), and writes who it is there. The
//! system drops the lock however the process ends, so a crash leaves nothing stale to clear; what
//! the file says once nobody holds it tells whether the session last ran outside. Outside, the CLI
//! holds the lock too (`Hold::share`): killing this command must not free a session still running.

use crate::app;
use crate::cli::{self, Arg, Lexer};
use crate::status::{self, Here, Scope};
use crate::store::{self, Saved, Ticket};
use crate::ticket_key;
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn usage() -> String {
    format!(
        "Usage: taskman resume KEY [--project NAME] [--ticket | --agent]

Continues a ticket's or agent's session in this terminal, started as taskman starts it: in its
folder, with its brief, hooks and name, so taskman's sidebar still shows its state and other
sessions can still message it. Hand it off in taskman first ({} t), which copies this command.
Only one copy runs at a time. When you quit it here, select it in taskman and press Enter to
continue it there, with the turns from here.

  KEY      the ticket's key, or an agent's name
  --project NAME
           the project, when the key is open in several
  --ticket, --agent
           which one, when an agent's name is also a ticket's key",
        crate::keys::prefix()
    )
}

/// What the owner file says about a session.
#[derive(Debug, PartialEq)]
pub enum Owner {
    /// Nothing runs it, and it last ran in taskman (or never ran): the TUI may start it.
    Free,
    /// It runs in taskman's pane.
    Taskman(u32),
    /// It runs in another terminal, through `taskman resume`.
    Outside(u32),
    /// Nothing runs it, and it was handed off or last ran outside: it waits for Enter in the TUI.
    Back,
    /// Its ticket was closed and taskman is still removing its worktree: nothing may start it
    /// until that's done, or the removal would delete the folder under the new session.
    Closing,
    /// Handed off (`ctrl+a t`), and the TUI still holds it while the pane's CLI finishes
    /// stopping: `taskman resume` can take it in a moment (`STOPPING_WAIT`).
    Stopping,
}

impl Owner {
    /// Whether another process runs the session: `taskman resume` in another terminal, or another
    /// taskman. Closing the ticket then would leave it running in a worktree that may be removed.
    pub fn elsewhere(&self) -> bool {
        match self {
            Owner::Outside(_) => true,
            Owner::Taskman(pid) => *pid != std::process::id(),
            Owner::Free | Owner::Back | Owner::Closing | Owner::Stopping => false,
        }
    }
}

/// How long `taskman resume` waits for a session just handed off whose CLI the TUI is still
/// stopping (`Owner::Stopping`): that takes a fraction of a second, and the TUI lets go on its
/// next poll.
const STOPPING_WAIT: Duration = Duration::from_secs(3);

const TASKMAN: &str = "taskman";
const OUTSIDE: &str = "outside";
const CLOSING: &str = "closing";
/// Written by the TUI as `ctrl+a t` stops the session in its pane.
pub const HANDED_OFF: &str = "handed-off";

/// The file whose lock says who runs the session of `key` in the project at `repo`.
pub fn owner_file(repo: &Path, key: &str) -> PathBuf {
    owner_file_in(&store::status_dir(), repo, key)
}

/// `owner_file` in the status folder `dir`.
pub fn owner_file_in(dir: &Path, repo: &Path, key: &str) -> PathBuf {
    store::status_file_in(dir, repo, key).with_extension("owner")
}

fn parse(text: &str, held: bool) -> Owner {
    let mut words = text.split_whitespace();
    let (who, pid) =
        (words.next().unwrap_or(""), words.next().and_then(|p| p.parse().ok()).unwrap_or(0));
    match (who, held) {
        (TASKMAN, true) => Owner::Taskman(pid),
        (CLOSING, true) => Owner::Closing,
        (HANDED_OFF, true) => Owner::Stopping,
        (_, true) => Owner::Outside(pid),
        (OUTSIDE | HANDED_OFF, false) => Owner::Back,
        (_, false) => Owner::Free,
    }
}

/// A claim on a session, held until it's dropped or the process ends.
pub struct Hold {
    file: std::fs::File,
    path: PathBuf,
    /// The ticket closed (`closing`): letting go removes the owner file, as nothing will look at
    /// it again.
    closed: bool,
}

impl Hold {
    /// Records who holds it, or with `HANDED_OFF` just before letting it go, what to do next.
    pub fn write(&mut self, what: &str) {
        let f = &mut self.file;
        let _ = f.set_len(0).and_then(|()| f.rewind()).and_then(|()| f.write_all(what.as_bytes()));
    }

    /// Claimed by the TUI, for its pane.
    pub fn taskman(mut self) -> Self {
        self.write(&format!("{TASKMAN} {}", std::process::id()));
        self
    }

    /// Kept by the TUI while it removes a closed ticket's worktree (`Owner::Closing`); letting go
    /// then removes the file, which `take` allows for.
    pub fn closing(mut self) -> Self {
        self.write(&format!("{CLOSING} {}", std::process::id()));
        self.closed = true;
        self
    }

    /// Gives the process `cmd` starts its own copy of the claim, so the session stays claimed while
    /// either runs: this one can be killed while the CLI goes on, or the git removing a closed
    /// ticket's worktree. A copy is the same open file, so its lock is the same lock on Linux and
    /// macOS alike, and the system lets it go when the last process holding it ends.
    pub fn share(&self, cmd: &mut std::process::Command) {
        let fd = self.file.as_raw_fd();
        // SAFETY: runs in the child between fork and exec, and only calls fcntl, which is
        // async-signal-safe. Only the child's descriptor loses close-on-exec: another thread
        // starting a process now doesn't get one.
        unsafe {
            cmd.pre_exec(move || match libc::fcntl(fd, libc::F_SETFD, 0) {
                -1 => Err(std::io::Error::last_os_error()),
                _ => Ok(()),
            });
        }
    }
}

impl Drop for Hold {
    /// Lets go now, for every copy: processes the CLI started may have one, and would otherwise
    /// keep the session claimed after it ends.
    fn drop(&mut self) {
        // Removed while still held, so whoever takes it next makes a new file (see `take`).
        if self.closed {
            let _ = std::fs::remove_file(&self.path);
        }
        // SAFETY: flock on a descriptor this value owns; it reads or writes no memory.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Why a session can't start, or its ticket be added again, while its worktree is being removed.
pub fn closing(name: &str) -> String {
    format!("{name}'s worktree is still being removed; try again in a moment")
}

/// Claims the session at `path`: Ok(Err) with who runs it when someone does.
pub fn take(path: &Path) -> std::io::Result<Result<Hold, Owner>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A closed ticket's hold removes the file as it lets go (`Hold::closing`), so the file locked
    // here may no longer be the one at `path`, opened just before it went: then it's taken again,
    // or two could hold the same session, one on the old file and one on a new one.
    for _ in 0..5 {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // `peek` holds a shared lock for a moment, so a refusal is tried again before it counts.
        let mut locked = false;
        for _ in 0..5 {
            // SAFETY: flock on a descriptor this function owns; it reads or writes no memory.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                locked = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !locked {
            let mut text = String::new();
            let _ = file.read_to_string(&mut text);
            return Ok(Err(parse(&text, true)));
        }
        if same_file(&file, path) {
            return Ok(Ok(Hold { file, path: path.to_path_buf(), closed: false }));
        }
    }
    Err(std::io::Error::other(format!("{} keeps being replaced", store::tilde(path))))
}

/// Whether `file` is still the file at `path`.
fn same_file(file: &std::fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

/// Who runs the session at `path`, without claiming it.
pub fn peek(path: &Path) -> Owner {
    let Ok(mut file) = std::fs::File::open(path) else { return Owner::Free };
    let mut text = String::new();
    let _ = file.read_to_string(&mut text);
    // A process forking at the moment the lock is let go shares the file until it execs, so a
    // held lock is tried again before it counts: taskman forks for git, gh and the clipboard.
    for _ in 0..3 {
        // SAFETY: flock on a descriptor this function owns; it reads or writes no memory. Shared,
        // so two peeks at once don't see each other; closing the file lets it go.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            return parse(&text, false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    parse(&text, true)
}

/// Whether a process runs the session whose owner file is `path`: taskman's pane or `taskman
/// resume`. What commands go by, as only the TUI knows its panes.
pub fn runs(path: &Path) -> bool {
    matches!(peek(path), Owner::Taskman(_) | Owner::Outside(_))
}

/// Why `taskman resume` can't take a session someone else runs.
fn refusal(name: &str, owner: &Owner) -> String {
    match owner {
        Owner::Closing => closing(name),
        Owner::Stopping => {
            format!("{name}'s session in taskman is still stopping; try again in a moment")
        }
        Owner::Taskman(_) => {
            format!(
                "{name} is running in taskman: press {} t there to hand it off",
                crate::keys::prefix()
            )
        }
        Owner::Outside(pid) if *pid > 0 => {
            format!("{name} is already running in another terminal (pid {pid})")
        }
        _ => format!("{name} is already running in another terminal"),
    }
}

/// The command a hand-off copies: `taskman resume KEY`, with `--project` when another project has
/// the key open too. An agent goes by its name, which is shorter than its key. `--agent` when the
/// name could be a ticket's key, and `--ticket` when an agent goes by the ticket's key, so it
/// never picks the other.
pub fn command_line<'a>(
    t: &Ticket,
    project: &str,
    all: impl IntoIterator<Item = &'a Ticket>,
) -> String {
    let name = t.agent.as_deref().unwrap_or(&t.key);
    let mut line = format!("taskman resume {name}");
    let all: Vec<&Ticket> = all.into_iter().collect();
    let shared = all.iter().any(|o| {
        o.repo != t.repo
            && (o.key.eq_ignore_ascii_case(&t.key) || t.agent.is_some() && o.agent == t.agent)
    });
    if shared {
        line += &format!(" --project {}", quote(project));
    }
    if t.agent.is_some() && ticket_key::parse(name).is_some() {
        line += " --agent";
    } else if t.agent.is_none() && all.iter().any(|o| is_agent(o, &t.key)) {
        line += " --ticket";
    }
    line
}

/// Whether `t` is an agent that `name` names: by its name or its key.
fn is_agent(t: &Ticket, name: &str) -> bool {
    t.agent
        .as_deref()
        .is_some_and(|a| store::slug(a) == store::slug(name) || t.key.eq_ignore_ascii_case(name))
}

/// One shell word, quoted only when it needs it.
pub fn quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

pub fn run(args: &[String]) -> i32 {
    cli::run("resume", &usage(), args, resume)
}

/// Which kind of session the name means, from `--ticket` or `--agent`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Ticket,
    Agent,
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    key: String,
    project: Option<String>,
    kind: Option<Kind>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let text = usage();
    let mut lex = Lexer::new(args, &text);
    while let Some(arg) = lex.next()? {
        match arg {
            Arg::Option("--project") => a.project = Some(lex.value()?.to_string()),
            Arg::Option(o @ ("--ticket" | "--agent")) => {
                let kind = if o == "--ticket" { Kind::Ticket } else { Kind::Agent };
                if a.kind.is_some_and(|k| k != kind) {
                    return Err("pass --ticket or --agent, not both".into());
                }
                a.kind = Some(kind);
            }
            Arg::Option(_) => return Err(lex.unknown()),
            Arg::Word(w) if a.key.is_empty() => a.key = w.to_string(),
            Arg::Word(w) => return Err(format!("one session at a time; got {w} too")),
        }
    }
    if a.key.is_empty() {
        return Err(format!("which session? Pass a ticket's key or an agent's name\n\n{text}"));
    }
    Ok(a)
}

/// The ticket or agent `key` names: a ticket through `taskman status`'s lookup, and an agent by its
/// name or key. An agent's name can look like a ticket's key (tester-1), so both are looked for
/// unless `kind` says which; when both are open, it's for the user to pick.
fn find<'a>(
    key: &str,
    kind: Option<Kind>,
    scope: Scope,
    saved: &'a Saved,
) -> Result<&'a Ticket, String> {
    let ticket = match ticket_key::parse(key) {
        Some(k) if kind != Some(Kind::Agent) => {
            Some(status::locate(&k, saved, scope).and_then(|(t, project)| {
                t.ok_or_else(|| {
                    format!("{k} isn't open in {}: add it in taskman first", project.name)
                })
            }))
        }
        _ => None,
    };
    let agent = match kind {
        Some(Kind::Ticket) => Ok(None),
        _ => find_agent(key, scope, saved),
    };
    let project = |t: &Ticket| {
        saved
            .projects
            .iter()
            .find(|p| p.repo == t.repo)
            .map_or_else(|| store::tilde(&t.repo), |p| p.name.clone())
    };
    match (ticket, agent) {
        (Some(Ok(t)), Ok(Some(a))) => Err(format!(
            "{key} is both ticket {} in {} and agent {} in {}: pass --ticket or --agent",
            t.key,
            project(t),
            a.display(),
            project(a)
        )),
        (Some(Ok(t)), Err(_)) => Err(format!(
            "{key} is both ticket {} in {} and an agent's name: pass --ticket or --agent",
            t.key,
            project(t)
        )),
        (Some(Ok(t)), Ok(None)) | (_, Ok(Some(t))) => Ok(t),
        (_, Err(e)) | (Some(Err(e)), Ok(None)) => Err(e),
        (None, Ok(None)) if kind == Some(Kind::Agent) => {
            Err(format!("no agent named {key} is open in taskman"))
        }
        (None, Ok(None)) if kind == Some(Kind::Ticket) => {
            Err(format!("{key} isn't a ticket's key"))
        }
        (None, Ok(None)) => Err(format!("no ticket or agent named {key} is open in taskman")),
    }
}

/// The agent `name` names in `scope`: Ok(None) when there's none, Err when several projects have
/// one and the scope doesn't pick.
fn find_agent<'a>(
    name: &str,
    scope: Scope,
    saved: &'a Saved,
) -> Result<Option<&'a Ticket>, String> {
    let agents: Vec<&Ticket> = saved
        .tickets
        .iter()
        .filter(|t| is_agent(t, name))
        .filter(|t| !matches!(scope, Scope::Named(repo) if repo != t.repo))
        .collect();
    let here = match scope {
        Scope::Here(repo) => agents.iter().find(|t| t.repo == repo),
        _ => None,
    };
    match (&agents[..], here) {
        (_, Some(t)) | ([t], None) => Ok(Some(t)),
        ([], None) => Ok(None),
        (_, None) => {
            let names: Vec<&str> = agents
                .iter()
                .filter_map(|t| saved.projects.iter().find(|p| p.repo == t.repo))
                .map(|p| p.name.as_str())
                .collect();
            Err(format!("{name} is open in {}: pass --project NAME", names.join(" and ")))
        }
    }
}

/// Runs the session; returns the CLI's exit code.
fn resume(args: &[String]) -> Result<i32, String> {
    let a = parse_args(args)?;
    let saved = store::load()?;
    let config = store::load_config()?;
    let named = a.project.as_deref().map(|n| cli::project_named(&saved.projects, n)).transpose()?;
    let here = Here::current();
    let t = find(&a.key, a.kind, status::scope(named, None, None, &saved, here.as_ref()), &saved)?
        .clone();
    let name = t.display().to_string();
    if t.pending_worktree.is_some() {
        return Err(format!(
            "{name}'s worktree isn't made yet: its session hasn't started, so there's nothing to resume"
        ));
    }
    // Spawned there, the CLI would fail as though it weren't installed.
    if let Some(gone) = app::Gone::of(&t) {
        let project = saved.projects.iter().find(|p| p.repo == t.repo);
        let project = project.map_or_else(|| store::project_name(&t.repo), |p| p.name.clone());
        let what = gone.what(&t, &project);
        return Err(match gone {
            app::Gone::Worktree => {
                format!(
                    "{what}: select {name} in taskman and press Enter to make its worktree again"
                )
            }
            app::Gone::Project => format!(
                "{what}: put it back, or delete the project in taskman's Settings and add it again where it is now"
            ),
        });
    }
    let path = owner_file(&t.repo, &t.key);
    // Just handed off, the TUI holds it until the pane's CLI is gone: a moment, waited for here,
    // as the hand-off's tmux window runs this right away.
    let until = std::time::Instant::now() + STOPPING_WAIT;
    let mut hold = loop {
        match take(&path).map_err(|e| format!("couldn't open {}: {e}", store::tilde(&path)))? {
            Ok(hold) => break hold,
            Err(Owner::Stopping) if std::time::Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(o) => return Err(refusal(&name, &o)),
        }
    };
    // Found in the state read before the claim was taken, which the TUI may have saved since:
    // it lets a renamed agent's old name (and a closed ticket) go only after saving, so what's
    // read now, under the claim, is current. Gone or another session's, the claim was a stale
    // name's, and goes with the error.
    let saved = store::load()?;
    let named = a.project.as_deref().map(|n| cli::project_named(&saved.projects, n)).transpose()?;
    let scope = status::scope(named, None, None, &saved, here.as_ref());
    let t = match find(&a.key, a.kind, scope, &saved) {
        Ok(now) if now.is(&t.repo, &t.key) => now.clone(),
        Ok(_) => return Err(format!("{name} changed in taskman just now; try again")),
        Err(e) => return Err(e),
    };
    let exe =
        std::env::current_exe().map_err(|e| format!("couldn't find taskman's own binary: {e}"))?;
    let Some(launch) = app::App::resume_launch(&exe, saved, config, &t) else {
        return Err(format!(
            "{name}'s session hasn't started yet, so there's nothing to resume: start it in taskman, then hand it off with {} t",
            crate::keys::prefix()
        ));
    };
    hold.write(&format!("{OUTSIDE} {}", std::process::id()));
    let mut cmd = std::process::Command::new(&launch.program);
    cmd.args(&launch.args).current_dir(&launch.cwd);
    for (k, v) in &launch.env {
        cmd.env(k, v);
    }
    cmd.env("TASKMAN_LAUNCH", app::launch_id());
    for k in app::NESTED_ENV {
        cmd.env_remove(k);
    }
    hold.share(&mut cmd);
    // The CLI gets the terminal's ctrl+c and the hangup when the window closes, and this process
    // waits for it to end before letting go of the session: ignored here, reset by exec there.
    crate::proc::hand_off_terminal();
    for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP] {
        let _ = signal_hook::flag::register(sig, std::sync::Arc::default());
    }
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", launch.program))?;
    // The CLI is what runs the session, and holds it after this process if this one is killed.
    hold.write(&format!("{OUTSIDE} {}", child.id()));
    let status = match child.wait() {
        Ok(status) => status,
        Err(e) => {
            // It may still run: its copy keeps the claim, which letting go would free.
            std::mem::forget(hold);
            return Err(format!("lost {}: {e}", launch.program));
        }
    };
    drop(hold);
    eprintln!(
        "taskman: {name} is back. Select it in taskman and press Enter to continue it there."
    );
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Project;

    #[test]
    fn knows_when_another_process_runs_the_session() {
        assert!(Owner::Outside(4242).elsewhere());
        assert!(Owner::Taskman(std::process::id() + 1).elsewhere());
        assert!(!Owner::Taskman(std::process::id()).elsewhere());
        assert!(!Owner::Free.elsewhere() && !Owner::Back.elsewhere());
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("taskman-resume-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_who_runs_a_session() {
        assert_eq!(parse("taskman 12", true), Owner::Taskman(12));
        assert_eq!(parse("outside 34", true), Owner::Outside(34));
        assert_eq!(parse("", true), Owner::Outside(0));
        assert_eq!(parse(HANDED_OFF, true), Owner::Stopping);
        // Nobody holds it: where it last ran decides.
        assert_eq!(parse("taskman 12", false), Owner::Free);
        assert_eq!(parse("", false), Owner::Free);
        assert_eq!(parse("outside 34", false), Owner::Back);
        assert_eq!(parse(HANDED_OFF, false), Owner::Back);
    }

    #[test]
    fn one_copy_at_a_time_in_both_directions() {
        let path = scratch("owner").join("ENG-52.owner");
        assert_eq!(peek(&path), Owner::Free);
        // The TUI starts it: `taskman resume` is refused, and told to hand it off.
        let tui = take(&path).unwrap().ok().unwrap().taskman();
        assert_eq!(peek(&path), Owner::Taskman(std::process::id()));
        let refused = take(&path).unwrap().err().unwrap();
        assert_eq!(
            refusal("ENG-52", &refused),
            "ENG-52 is running in taskman: press ctrl+a t there to hand it off"
        );
        // ctrl+a t: the TUI says it's handed off, holding on while the pane's CLI finishes
        // stopping, which `taskman resume` waits for, and then lets go.
        let mut tui = tui;
        tui.write(HANDED_OFF);
        assert_eq!(peek(&path), Owner::Stopping);
        assert_eq!(take(&path).unwrap().err(), Some(Owner::Stopping));
        assert!(!Owner::Stopping.elsewhere());
        assert_eq!(
            refusal("ENG-52", &Owner::Stopping),
            "ENG-52's session in taskman is still stopping; try again in a moment"
        );
        drop(tui);
        assert_eq!(peek(&path), Owner::Back);
        // It runs outside: the TUI can't start it.
        let mut outside = take(&path).unwrap().ok().unwrap();
        outside.write("outside 4242");
        assert_eq!(take(&path).unwrap().err(), Some(Owner::Outside(4242)));
        assert_eq!(peek(&path), Owner::Outside(4242));
        // It ends, however it ends: it's back, and the TUI can take it again.
        drop(outside);
        assert_eq!(peek(&path), Owner::Back);
        let tui = take(&path).unwrap().ok().unwrap().taskman();
        drop(tui);
        assert_eq!(peek(&path), Owner::Free);
    }

    #[test]
    fn nothing_starts_a_session_while_its_worktree_is_removed() {
        assert_eq!(parse("closing 12", true), Owner::Closing);
        assert_eq!(parse("closing 12", false), Owner::Free);
        let path = scratch("closing").join("ENG-52.owner");
        // Closing keeps the pane's hold rather than letting go of it.
        let closing = take(&path).unwrap().ok().unwrap().taskman().closing();
        assert_eq!(peek(&path), Owner::Closing);
        assert!(!peek(&path).elsewhere());
        let refused = take(&path).unwrap().err().unwrap();
        assert_eq!(
            refusal("ENG-52", &refused),
            "ENG-52's worktree is still being removed; try again in a moment"
        );
        // The removal is done: the ticket is gone, and its file with it.
        drop(closing);
        assert!(!path.exists());
        assert_eq!(peek(&path), Owner::Free);
        assert!(take(&path).unwrap().is_ok());
    }

    /// A closed ticket's file goes as its hold lets go: a claim on the file opened just before
    /// isn't a claim at all, so it's taken again on the new file, and only one can hold it.
    #[test]
    fn a_removed_owner_file_is_never_held_twice() {
        let path = scratch("replaced").join("ENG-52.owner");
        let closing = take(&path).unwrap().ok().unwrap().closing();
        let stale = std::fs::File::open(&path).unwrap();
        drop(closing);
        // SAFETY: flock on a descriptor this test owns.
        assert_eq!(unsafe { libc::flock(stale.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        assert!(!same_file(&stale, &path));
        let hold = take(&path).unwrap().ok().unwrap();
        assert!(same_file(&hold.file, &path));
        assert!(take(&path).unwrap().is_err());
    }

    #[test]
    fn letting_go_frees_it_from_every_copy() {
        let path = scratch("copies").join("ENG-52.owner");
        let hold = take(&path).unwrap().ok().unwrap();
        // What a process the CLI started would keep.
        // SAFETY: dup of a descriptor the hold owns; the copy is closed below.
        let copy = unsafe { libc::dup(hold.file.as_raw_fd()) };
        assert!(copy >= 0);
        drop(hold);
        assert_eq!(peek(&path), Owner::Free);
        assert!(take(&path).unwrap().is_ok());
        // SAFETY: the copy made above, closed once.
        unsafe { libc::close(copy) };
    }

    fn ticket(key: &str, repo: &str, agent: Option<&str>) -> Ticket {
        let mut t: Ticket =
            serde_json::from_value(serde_json::json!({ "key": key, "repo": repo, "cwd": repo }))
                .unwrap();
        t.agent = agent.map(str::to_string);
        t
    }

    fn project(name: &str, repo: &str, prefix: &str) -> Project {
        serde_json::from_value(serde_json::json!({
            "name": name, "repo": repo,
            "tracker": { "provider": "linear", "site": "", "key": prefix },
        }))
        .unwrap()
    }

    #[test]
    fn finds_tickets_and_agents() {
        let saved = Saved {
            projects: vec![project("web", "/w", "ENG"), project("api", "/a", "ENG")],
            tickets: vec![
                ticket("ENG-1", "/w", None),
                ticket("ENG-2", "/w", None),
                ticket("ENG-2", "/a", None),
                ticket("web-lead", "/w", Some("lead")),
                ticket("api-lead", "/a", Some("lead")),
                ticket("api-tester", "/a", Some("tester")),
            ],
            ..Default::default()
        };
        let key = |k: &str, named: Option<&str>, here: Option<&str>| {
            let scope = match (named, here) {
                (Some(p), _) => Scope::Named(Path::new(p)),
                (None, Some(p)) => Scope::Here(Path::new(p)),
                (None, None) => Scope::Anywhere,
            };
            find(k, None, scope, &saved).map(|t| (t.repo.clone(), t.key.clone()))
        };
        let at = |r: &str, k: &str| Ok((PathBuf::from(r), k.to_string()));
        assert_eq!(key("eng-1", None, None), at("/w", "ENG-1"));
        assert!(key("ENG-2", None, None).unwrap_err().contains("open in web and api"));
        assert_eq!(key("ENG-2", Some("/a"), None), at("/a", "ENG-2"));
        assert_eq!(key("ENG-2", None, Some("/w")), at("/w", "ENG-2"));
        assert!(key("ENG-1", Some("/a"), None).unwrap_err().contains("isn't open in api"));
        assert!(key("ENG-9", None, None).unwrap_err().contains("isn't open"));
        // Agents by name or key.
        assert_eq!(key("tester", None, None), at("/a", "api-tester"));
        assert_eq!(key("web-lead", None, None), at("/w", "web-lead"));
        assert!(key("lead", None, None).unwrap_err().contains("--project"));
        assert_eq!(key("lead", Some("/a"), None), at("/a", "api-lead"));
        assert_eq!(key("lead", None, Some("/w")), at("/w", "web-lead"));
        assert!(key("nobody", None, None).is_err());
    }

    /// acme has an agent named tester-1, which looks like a ticket's key, and ops has
    /// TESTER-1 open.
    fn tester_1() -> Saved {
        Saved {
            projects: vec![project("acme", "/acme", "ENG"), project("ops", "/ops", "TESTER")],
            tickets: vec![
                ticket("ENG-52", "/acme", None),
                ticket("acme-tester-1", "/acme", Some("tester-1")),
                ticket("acme-lead", "/acme", Some("lead")),
                ticket("TESTER-1", "/ops", None),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn finds_an_agent_named_like_a_ticket() {
        let mut saved = tester_1();
        saved.tickets.retain(|t| t.key != "TESTER-1");
        let key = |k: &str, kind: Option<Kind>, named: Option<&str>| {
            let scope = named.map_or(Scope::Anywhere, |p| Scope::Named(Path::new(p)));
            find(k, kind, scope, &saved).map(|t| t.key.clone())
        };
        // No ticket TESTER-1 is open: the agent, however it's asked for.
        assert_eq!(key("tester-1", None, Some("/acme")), Ok("acme-tester-1".into()));
        assert_eq!(key("tester-1", None, None), Ok("acme-tester-1".into()));
        assert_eq!(key("TESTER-1", None, None), Ok("acme-tester-1".into()));
        assert_eq!(key("tester-1", Some(Kind::Agent), None), Ok("acme-tester-1".into()));
        assert_eq!(key("acme-tester-1", None, None), Ok("acme-tester-1".into()));
        // Asked for as a ticket, it isn't one.
        assert!(
            key("tester-1", Some(Kind::Ticket), None)
                .unwrap_err()
                .contains("TESTER-1 isn't open in ops")
        );
        // Another project has no such agent.
        assert!(
            key("tester-1", None, Some("/ops")).unwrap_err().contains("TESTER-1 isn't open in ops")
        );
    }

    #[test]
    fn asks_which_when_a_ticket_and_an_agent_match() {
        let saved = tester_1();
        let key = |k: &str, kind: Option<Kind>, named: Option<&str>| {
            let scope = named.map_or(Scope::Anywhere, |p| Scope::Named(Path::new(p)));
            find(k, kind, scope, &saved).map(|t| (t.repo.clone(), t.key.clone()))
        };
        let at = |r: &str, k: &str| Ok((PathBuf::from(r), k.to_string()));
        assert_eq!(
            key("tester-1", None, None),
            Err("tester-1 is both ticket TESTER-1 in ops and agent tester-1 in acme: pass --ticket or --agent".into())
        );
        assert_eq!(key("tester-1", Some(Kind::Ticket), None), at("/ops", "TESTER-1"));
        assert_eq!(key("tester-1", Some(Kind::Agent), None), at("/acme", "acme-tester-1"));
        // --project picks when only one of them is in that project.
        assert_eq!(key("tester-1", None, Some("/acme")), at("/acme", "acme-tester-1"));
        assert_eq!(key("tester-1", None, Some("/ops")), at("/ops", "TESTER-1"));
        // Both in one project: --project alone doesn't pick.
        let mut saved = tester_1();
        saved.tickets.push(ticket("TESTER-1", "/acme", None));
        let both = find("tester-1", None, Scope::Named(Path::new("/acme")), &saved).unwrap_err();
        assert!(both.contains("both ticket TESTER-1 in acme and agent tester-1 in acme"), "{both}");
        let agent =
            find("tester-1", Some(Kind::Agent), Scope::Named(Path::new("/acme")), &saved).unwrap();
        assert_eq!(agent.key, "acme-tester-1");
    }

    #[test]
    fn finds_plain_tickets_and_agents_as_before() {
        let saved = tester_1();
        let key = |k: &str, kind: Option<Kind>| {
            find(k, kind, Scope::Anywhere, &saved).map(|t| t.key.clone())
        };
        assert_eq!(key("eng-52", None), Ok("ENG-52".into()));
        assert_eq!(key("ENG-52", Some(Kind::Ticket)), Ok("ENG-52".into()));
        assert_eq!(key("lead", None), Ok("acme-lead".into()));
        assert_eq!(key("lead", Some(Kind::Agent)), Ok("acme-lead".into()));
        assert!(key("lead", Some(Kind::Ticket)).unwrap_err().contains("isn't a ticket's key"));
        assert!(key("ENG-52", Some(Kind::Agent)).unwrap_err().contains("no agent named ENG-52"));
        assert!(key("ENG-9", None).unwrap_err().contains("ENG-9 isn't open in acme"));
    }

    #[test]
    fn copies_a_command_that_picks_the_agent_or_the_ticket() {
        let saved = tester_1();
        let line =
            |i: usize, project: &str| command_line(&saved.tickets[i], project, &saved.tickets);
        assert_eq!(line(0, "acme"), "taskman resume ENG-52");
        assert_eq!(line(1, "acme"), "taskman resume tester-1 --agent");
        assert_eq!(line(2, "acme"), "taskman resume lead");
        assert_eq!(line(3, "ops"), "taskman resume TESTER-1 --ticket");
        // Each copied command finds the session it was copied from.
        for (i, t) in saved.tickets.iter().enumerate() {
            let words: Vec<String> =
                line(i, "acme").split(' ').skip(2).map(str::to_string).collect();
            let a = parse_args(&words).unwrap();
            let found = find(&a.key, a.kind, Scope::Anywhere, &saved).unwrap();
            assert!(found.is(&t.repo, &t.key), "{}", line(i, "acme"));
        }
    }

    #[test]
    fn copies_a_command_that_names_one_session() {
        let all = vec![
            ticket("ENG-1", "/w", None),
            ticket("ENG-2", "/w", None),
            ticket("ENG-2", "/a", None),
            ticket("web-lead", "/w", Some("lead")),
            ticket("api-lead", "/a", Some("lead")),
            ticket("api-tester", "/a", Some("tester")),
        ];
        assert_eq!(command_line(&all[0], "web", &all), "taskman resume ENG-1");
        assert_eq!(command_line(&all[1], "web", &all), "taskman resume ENG-2 --project web");
        assert_eq!(
            command_line(&all[2], "my api", &all),
            "taskman resume ENG-2 --project 'my api'"
        );
        assert_eq!(command_line(&all[3], "web", &all), "taskman resume lead --project web");
        assert_eq!(command_line(&all[5], "api", &all), "taskman resume tester");
    }

    #[test]
    fn parses_arguments() {
        let args = |a: &[&str]| parse_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let ok = |key: &str, project: Option<&str>, kind| {
            Ok(Args { key: key.into(), project: project.map(str::to_string), kind })
        };
        assert_eq!(args(&["ENG-1"]), ok("ENG-1", None, None));
        assert_eq!(args(&["ENG-1", "--project", "web"]), ok("ENG-1", Some("web"), None));
        assert_eq!(args(&["--project=web", "lead"]), ok("lead", Some("web"), None));
        assert_eq!(args(&["tester-1", "--agent"]), ok("tester-1", None, Some(Kind::Agent)));
        assert_eq!(
            args(&["--ticket", "tester-1", "--ticket"]),
            ok("tester-1", None, Some(Kind::Ticket))
        );
        assert!(args(&["tester-1", "--ticket", "--agent"]).is_err());
        assert!(args(&[]).is_err());
        assert!(args(&["ENG-1", "ENG-2"]).is_err());
        assert!(args(&["--json", "ENG-1"]).unwrap_err().starts_with("unknown option --json\n\n"));
        assert_eq!(args(&["lead", "--project="]), Err("--project needs a value".into()));
        assert_eq!(args(&["lead", "--agent=yes"]), Err("--agent takes no value".into()));
    }
}
