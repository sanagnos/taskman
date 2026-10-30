//! `taskman add KEY...`: adds tickets, or a project agent, to the running taskman from any shell
//! or session. The command never writes tickets.json, which the TUI holds in memory and rewrites
//! on every change: it leaves a request in the inbox folder, the TUI adds it through the Add form's
//! code on its next pass and writes a reply, and the command prints the reply. With no TUI running
//! the request waits there, and the next start adds it.

use crate::cli::{self, Arg, Lexer, pick_project, resolve_key};
use crate::status::{self, Here};
use crate::store::{self, Cli, Saved, Ticket};
use crate::tracker::{Client, FetchError, Text};
use crate::{local, untrusted, util};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const USAGE: &str = "Usage: taskman add KEY... [options]
       taskman add --new TITLE [--description TEXT] [options]
       taskman add --agent NAME [--role TEXT] [--fork KEY] [options]

Adds tickets to taskman as the Add form does, with the project's worktree settings, without
switching to them. If taskman isn't running, they're added when it starts. An agent, or a new
role, waits for the user to approve it in taskman; its session then starts at once, so the agent
that asked can message it. So does --new run in a session taskman started: the command waits up
to a minute for the user's answer.

  KEY             a ticket number (52), key (ENG-52) or link; several at once
  --new TITLE     create a ticket titled TITLE with the project's account, then add it: in
                  Linear's team or Jira's project the prefix names (Jira: a Task), the GitHub
                  repository, or a Local files board's todo column (in the project folder, never
                  a worktree's copy); repeatable. A failed create isn't retried: check the
                  tracker before running it again. From a session, taskman shows the user the
                  title and description and creates the ticket only once they approve
  --description TEXT
                  with one --new: the new ticket's description (Markdown)
  --agent NAME    a project agent instead of a ticket
  --role TEXT     with --agent: a standing instruction added to the agent's startup brief, such
                  as \"Test PRs against their tickets\". Not a first message: the agent waits until
                  messaged. On an agent that's already open, it replaces the role from its next
                  session start
  --fork KEY      with --agent: start the agent with a copy of another session's conversation
                  so far (a ticket's key or an agent's name, in the same project), in that
                  session's CLI. The other session carries on as it was; the new agent's first
                  message tells it who it is now
  --codex         run the session in Codex (default: Claude)
  --worktree, --no-worktree
                  create the ticket's worktree or not (default: the project's setting)
  --on KEY        stack on another open ticket: the worktree starts from its branch, and
                  the session opens its PR against that branch
  --project NAME  the project, when the folder or session doesn't say which. In a session
                  taskman started, only the session's own";

/// How long the command waits for the TUI's reply.
const WAIT: Duration = Duration::from_secs(5);
/// How long a session's `--new` waits for the user to approve or decline the ticket: short of
/// Claude Code's two minutes for a command, so the session hears the answer.
const ASK_WAIT: Duration = Duration::from_secs(60);
/// The TUI rewrites the `alive` file once a second; one older than this means it isn't running.
/// So does one further than this in the future: a killed TUI leaves the file, and after the clock
/// steps back it would otherwise look alive until the clock caught up, refusing every start.
const ALIVE: Duration = Duration::from_secs(3);
/// How often, and how many times, a command reads an `alive` file it found empty or cut short
/// before giving up on what it says. An older build rewrites the file in place every 300 ms
/// rather than renaming a new one over it, so a read can catch it between the two.
const ALIVE_RETRY: Duration = Duration::from_millis(50);
const ALIVE_RETRIES: u32 = 10;
/// Replies the command stopped waiting for are removed after this.
const STALE_REPLY: Duration = Duration::from_secs(60);
/// A request is a few lines; a file this big isn't one, and isn't read.
const MAX_REQUEST: u64 = 1024 * 1024;
/// What requests the TUI understands, written in the `alive` file so a command run after an
/// update doesn't ask an older TUI for something it would drop. 2 added `base`, 3 an agent's
/// `role`, 4 `close`, 5 `new` items, 6 an agent's `fork`.
const INBOX_FORMAT: u32 = 6;

/// What the command asks the TUI to add, all to one project. Unknown fields are refused rather
/// than dropped, so a TUI older than the command says so instead of half doing it.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// The project's main checkout, which names it even if it's renamed meanwhile.
    pub repo: PathBuf,
    /// Who asked, for the toast: `✦ planner`, a ticket key, or None from a plain shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub items: Vec<Item>,
    #[serde(default, skip_serializing_if = "Cli::is_default")]
    pub cli: Cli,
    /// Create the tickets' worktrees; None is the project's setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    /// The ticket the tickets stack on (`--on`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Unix time the command stops waiting: a reply after it has no reader, and a request already
    /// past it when taskman takes it was queued while taskman was closed.
    pub reply_by: i64,
    /// Close the items instead of adding them (`taskman close`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<Close>,
}

/// How `taskman close` closes. A taskman before inbox format 4 refuses requests with it, as it
/// doesn't know the field.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(deny_unknown_fields)]
pub struct Close {
    /// Only tickets whose PR has merged; with no items, every ticket in the project.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merged: bool,
    /// Close without removing any worktree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_worktree: bool,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Item {
    Ticket {
        key: String,
        /// The issue's title, when the command could fetch it.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        title: String,
    },
    Agent {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        /// The key of the session to fork the agent from, in the same project (`--fork`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork: Option<String>,
    },
    /// A ticket for the TUI to create in the project's tracker, then add: a session's `--new`,
    /// which waits for the user to approve it.
    New {
        title: String,
        /// Markdown, empty for none.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        description: String,
    },
}

impl Request {
    /// The agent asked for, with its role: the TUI asks the user before adding it.
    pub fn agent(&self) -> Option<(&str, Option<&str>)> {
        self.items.iter().find_map(|i| match i {
            Item::Agent { name, role, .. } => Some((name.as_str(), role.as_deref())),
            Item::Ticket { .. } | Item::New { .. } => None,
        })
    }

    /// The key of the session the agent asked for is forked from.
    pub fn fork(&self) -> Option<&str> {
        self.items.iter().find_map(|i| match i {
            Item::Agent { fork, .. } => fork.as_deref(),
            Item::Ticket { .. } | Item::New { .. } => None,
        })
    }

    /// The tickets to create, each a title and description: the TUI asks the user before creating
    /// them.
    pub fn new_tickets(&self) -> Vec<(&str, &str)> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::New { title, description } => Some((title.as_str(), description.as_str())),
                Item::Ticket { .. } | Item::Agent { .. } => None,
            })
            .collect()
    }
}

/// The reply to a request to create tickets the user declined.
pub fn declined(req: &Request) -> Vec<Line> {
    req.new_tickets()
        .into_iter()
        .map(|(title, _)| {
            Line::err(format!(
                "the user declined creating \"{}\" in taskman; nothing was created",
                untrusted::line(title)
            ))
        })
        .collect()
}

/// The reply to a request for an agent, sent before the user answers: the command doesn't wait
/// that long. `open`: the agent is open, so only its role changes.
pub fn asked(req: &Request, open: bool) -> Vec<Line> {
    let Some((name, role)) = req.agent() else { return Vec::new() };
    if let Some(source) = req.fork() {
        return vec![Line::ok(format!(
            "taskman asks the user to approve ✦ {name}, a fork of {source}; its session starts once they do, and `taskman status` lists it"
        ))];
    }
    vec![Line::ok(match (open, role) {
        (true, _) => {
            format!(
                "taskman asks the user to approve ✦ {name}'s new role; it applies when its session next starts"
            )
        }
        (false, Some(_)) => format!(
            "taskman asks the user to approve ✦ {name} with its role; its session starts once they do, and `taskman status` lists it"
        ),
        (false, None) => format!(
            "taskman asks the user to approve ✦ {name}; its session starts once they do, and `taskman status` lists it"
        ),
    })]
}

/// One line of the reply, per item: what happened, and whether it counts as success.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Line {
    pub ok: bool,
    pub text: String,
}

impl Line {
    pub fn ok(text: impl Into<String>) -> Self {
        Line { ok: true, text: text.into() }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Line { ok: false, text: text.into() }
    }
}

fn inbox() -> PathBuf {
    store::state_dir().join("inbox")
}

fn alive_file() -> PathBuf {
    inbox().join("alive")
}

fn reply_file(id: &str) -> PathBuf {
    inbox().join(format!("{id}.reply.json"))
}

// ---- the TUI's side ---------------------------------------------------------------------

/// What the `alive` file holds: which version runs, its process, and the requests it understands.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Alive {
    version: String,
    inbox: u32,
    /// Whether that TUI holds the state folder's lock while it runs; written from this release
    /// on, as release numbers alone may not tell later (see `holds_lock`).
    #[serde(default)]
    lock: bool,
    /// That TUI's process, written from this release on: the lock alone doesn't say it runs when
    /// the lock file was removed under it, or flock doesn't work in the state folder (some
    /// network folders).
    #[serde(default)]
    pid: Option<u32>,
    /// The machine that TUI runs on, written from this release on: on a state folder shared
    /// between machines, `pid` names a process on that one, which `kill(pid, 0)` here can't check.
    #[serde(default)]
    host: Option<String>,
}

impl Alive {
    fn new() -> Self {
        Alive {
            version: env!("CARGO_PKG_VERSION").into(),
            inbox: INBOX_FORMAT,
            lock: true,
            pid: Some(std::process::id()),
            host: hostname(),
        }
    }

    /// Whether the TUI that wrote this runs on another machine, whose processes `pid_alive` can't
    /// see. One that didn't say (an older release), or this machine's name unknown, counts as
    /// this one's, as before.
    fn elsewhere(&self) -> bool {
        self.host.as_ref().is_some_and(|h| hostname().is_some_and(|ours| *h != ours))
    }

    /// Whether the TUI that wrote this holds the state folder's lock while it runs. Every release
    /// does but 0.4.8 to 0.4.15, from before the public repository, which wrote this file without
    /// taking the lock; earlier ones wrote it empty, which doesn't parse.
    fn holds_lock(&self) -> bool {
        self.lock || !matches!(crate::update::parse(&self.version), Some((0, 4, 8..=15)))
    }
}

/// Says the TUI is running; called once a second from the inbox pass. Writing it also sets its
/// time. Written whole and renamed into place, so a command never reads it empty or half written.
/// False when it couldn't be written.
pub fn heartbeat() -> bool {
    let alive = Alive::new();
    store::write_unsynced(&alive_file(), &serde_json::to_vec(&alive).unwrap_or_default()).is_ok()
}

/// What an `alive` file says; None for one that's empty or cut short, as a read can find an
/// older build's between its truncating and its writing the file.
fn parse_alive(text: &str) -> Option<Alive> {
    serde_json::from_str(text).ok()
}

/// The running TUI's `alive` file. One found empty or cut short is read again a few times, as
/// an older build rewrites it in place; None when it stays so, or when the file is gone (the TUI
/// stopped meanwhile): the command then sends the request rather than refuse it, and a TUI that
/// can't read it says so in its reply.
fn read_alive() -> Option<Alive> {
    for tries in 0..=ALIVE_RETRIES {
        let text = std::fs::read_to_string(alive_file()).ok()?;
        if let Some(alive) = parse_alive(&text) {
            return Some(alive);
        }
        if tries < ALIVE_RETRIES {
            std::thread::sleep(ALIVE_RETRY);
        }
    }
    None
}

/// Why the running TUI can't take this request, if it can't: it's older than the command.
fn too_old(req: &Request, alive: &Alive) -> Option<String> {
    let role = req.items.iter().any(|i| matches!(i, Item::Agent { role: Some(_), .. }));
    let (needs, flag) = if req.fork().is_some() {
        (6, "--fork")
    } else if !req.new_tickets().is_empty() {
        (5, "--new from a session")
    } else if req.close.is_some() {
        (4, "taskman close")
    } else if role {
        (3, "--role")
    } else if req.base.is_some() {
        (2, "--on")
    } else {
        (1, "")
    };
    (alive.inbox < needs).then(|| {
        let v = if alive.version.starts_with(char::is_numeric) {
            format!("taskman {}", alive.version)
        } else {
            "an older taskman".into()
        };
        let again = if req.close.is_some() { "close" } else { "add" };
        format!(
            "{v} is running, which doesn't know {flag} yet; restart it ({} q, then taskman) and {again} again",
            crate::keys::prefix()
        )
    })
}

/// What a TUI that has just taken the state folder's lock finds of the one before it.
#[derive(Debug, PartialEq)]
pub enum Before {
    /// It quit, removing `alive`, or there was none.
    Quit,
    /// Another TUI is running, with its pid when `alive` says it, and its machine when that's
    /// another: a release from before the lock, which keeps `alive` fresh and takes no lock, or one
    /// whose lock this one didn't meet.
    Running(Option<u32>, Option<String>),
    /// It was killed or crashed, leaving `alive` behind, which is now removed.
    Crashed,
}

/// What came before this TUI on the state folder, by the `alive` file; asked once this one holds
/// the lock, so nothing that holds the same lock file is running. A TUI that takes it removes
/// `alive` as it quits, so a file still there was left by one that didn't quit (a kill -9, an OOM
/// kill or a panic, which leave a heartbeat under `ALIVE` old to a taskman started straight
/// after), unless its process still runs: then the lock file was removed and made again under it,
/// or flock doesn't work in the state folder (some network folders), or it runs on another machine
/// that shares the folder, where its pid can't be checked from here.
pub fn before() -> Before {
    let Ok(modified) = std::fs::metadata(alive_file()).and_then(|m| m.modified()) else {
        return Before::Quit;
    };
    if running(Some(modified), SystemTime::now()) {
        match read_alive() {
            // One that can't be read (an older build's, cut short) counts as running, as before.
            None => return Before::Running(None, None),
            Some(a) if !a.holds_lock() => return Before::Running(None, None),
            Some(a) if a.elsewhere() => return Before::Running(a.pid, a.host),
            // Not our own: a killed taskman's pid may be this one's by now.
            Some(Alive { pid: Some(pid), .. }) if pid != std::process::id() && pid_alive(pid) => {
                return Before::Running(Some(pid), None);
            }
            Some(_) => {}
        }
    }
    stopped();
    Before::Crashed
}

/// Whether the TUI runs on this state folder, for a command leaving it a request: it holds the
/// state folder's lock (`store::locked`) and hasn't removed `alive`, which it writes once it has
/// the lock and removes as it quits, or it keeps `alive` fresh, as a release from before the lock
/// does, or one whose lock file was removed, and its process hasn't ended (a crash leaves `alive`
/// fresh for a moment; one on another machine sharing the folder can't be checked, so counts). Not
/// by the heartbeat's age alone: that's the file's time on the wall clock, which can step forward
/// by seconds (WSL resyncs its clock every half minute or so, by about 2.5 s), and a command would
/// then say taskman isn't running while it is.
pub fn tui_running() -> bool {
    let alive = std::fs::metadata(alive_file()).and_then(|m| m.modified()).ok();
    (alive.is_some() && store::locked())
        || (running(alive, SystemTime::now())
            && !read_alive()
                .is_some_and(|a| !a.elsewhere() && a.pid.is_some_and(|p| !pid_alive(p))))
}

/// This machine's name, which `alive` notes; None when it can't be read.
fn hostname() -> Option<String> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let mut buf = [0u8; 256];
        // SAFETY: writes at most `buf.len()` bytes into `buf`, which is ours.
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
        // Cut short, the name may not end with a NUL, and then isn't trusted.
        let name = std::ffi::CStr::from_bytes_until_nul(&buf).ok().filter(|_| ok)?;
        Some(name.to_string_lossy().into_owned()).filter(|n| !n.is_empty())
    })
    .clone()
}

/// Whether process `pid` exists: `kill(pid, 0)` finds it, ours or another user's (EPERM).
fn pid_alive(pid: u32) -> bool {
    // 0 and what doesn't fit a pid_t would name a group of processes, not one.
    let Some(pid) = i32::try_from(pid).ok().filter(|&p| p > 0) else { return false };
    // SAFETY: signal 0 only checks that the process exists; nothing is sent, no memory is involved.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The TUI is quitting: commands from now on queue without waiting.
pub fn stopped() {
    let _ = std::fs::remove_file(alive_file());
}

/// `main` is ending, however it ends; called before the state folder's lock goes. As `stopped`,
/// except on a panic, which leaves `alive` for the next start to say this one crashed: its pid
/// ends with the process, so the file says nothing runs from then on.
pub fn ending() {
    if !std::thread::panicking() {
        stopped();
    }
}

/// Takes the waiting requests, oldest first, with their ids. Each is claimed by renaming it, so
/// two taskmans running at once never both add it. A request that can't be read is answered
/// with the reason.
pub fn take() -> Vec<(String, Request)> {
    let dir = inbox();
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut names = Vec::new();
    for e in entries.flatten() {
        let Ok(name) = e.file_name().into_string() else { continue };
        if name.ends_with(".reply.json") {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age > STALE_REPLY);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        } else if let Some(id) = name.strip_suffix(".json")
            // Regular files only (`file_type` follows no link): opening a pipe would wait here,
            // on the UI thread, for a writer that never comes, and a link could name any file.
            && e.file_type().is_ok_and(|t| t.is_file())
        {
            names.push(id.to_string());
        }
    }
    names.sort();
    let mut out = Vec::new();
    for id in names {
        let taken = dir.join(format!("{id}.taken"));
        if std::fs::rename(dir.join(format!("{id}.json")), &taken).is_err() {
            continue;
        }
        let text = read_regular(&taken);
        let _ = std::fs::remove_file(&taken);
        match text.map(|t| serde_json::from_str(&t)) {
            Ok(Ok(req)) => out.push((id, req)),
            Ok(Err(e)) => write_reply(
                &id,
                &[Line::err(format!(
                    "taskman couldn't read the request ({e}); is it an older release? Restart it"
                ))],
            ),
            Err(e) => {
                write_reply(&id, &[Line::err(format!("taskman couldn't read the request: {e}"))]);
            }
        }
    }
    out
}

/// Reads an inbox file, which must be a regular file of a request's size. It's opened without
/// following a link or waiting on a pipe, then checked, so nothing put in the folder meanwhile
/// can hold up the thread reading it.
fn read_regular(path: &Path) -> Result<String, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    let meta = f.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("it isn't a regular file".into());
    }
    if meta.len() > MAX_REQUEST {
        return Err(format!("it's {} bytes, far more than a request", meta.len()));
    }
    let mut text = String::new();
    f.read_to_string(&mut text).map_err(|e| e.to_string())?;
    Ok(text)
}

/// Answers a request, unless the command has stopped waiting.
pub fn reply(id: &str, req: &Request, lines: &[Line]) {
    if !queued(req, util::now()) {
        write_reply(id, lines);
    }
}

fn write_reply(id: &str, lines: &[Line]) {
    if let Ok(json) = serde_json::to_vec(lines) {
        let _ = store::write_atomic(&reply_file(id), &json);
    }
}

/// The command stopped waiting: taskman wasn't running when it asked, or didn't answer in time.
pub fn queued(req: &Request, now: i64) -> bool {
    now > req.reply_by
}

// ---- the command ------------------------------------------------------------------------

pub fn run(args: &[String]) -> i32 {
    cli::run("add", USAGE, args, |args| add(args).map(|ok| i32::from(!ok)))
}

/// Prints a line per ticket; returns whether every one was added, queued or already open.
fn add(args: &[String]) -> Result<bool, String> {
    let a = parse_args(args)?;
    let saved = store::load()?;
    let (session, session_repo) = store::session_env();
    let me = session
        .as_deref()
        .and_then(|s| store::session_ticket(&saved.tickets, s, session_repo.as_deref()));
    let p = pick_project(a.project.as_deref(), &a.inputs, me, &saved, Here::current().as_ref())?;
    cli::confine(p, cli::session_project(&saved, session_repo.as_deref())?, &saved)?;
    let project = &saved.projects[p];
    let base = a.on.as_deref().map(|on| base_key(on, p, &saved)).transpose()?;
    let codex = std::env::var(status::CODEX_NO_NETWORK).is_ok_and(|v| v == "1");
    let mut all_ok = true;
    let mut show = |line: Line| {
        all_ok &= line.ok;
        // A reply can quote the ticket's title from the tracker.
        let text = untrusted::clean(&line.text);
        if line.ok {
            cli::out!("{text}");
        } else {
            eprintln!("taskman add: {text}");
        }
    };

    let mut items = Vec::new();
    // Tickets `--new` created, which must not be lost if the request then can't be sent.
    let mut created: Vec<String> = Vec::new();
    // A session's `--new`: the TUI asks the user, as text the session read may be what asked for
    // the ticket, and its description can carry anything the session can read.
    let mut asks = Vec::new();
    if let Some(name) = &a.agent {
        let name = store::slug(name);
        if name.is_empty() {
            return Err("use letters or digits in the agent's name".into());
        }
        let open = store::find_agent(&saved.tickets, &project.repo, &name).is_some();
        let fork = match &a.fork {
            Some(input) => {
                if open {
                    return Err(format!(
                        "✦ {name} is already open in taskman; fork into another name"
                    ));
                }
                let source = fork_source(input, p, &saved)?;
                if a.cli.is_some_and(|c| c != source.cli) {
                    return Err(format!(
                        "{} runs in {}, and a fork runs in the same CLI; drop --{}",
                        source.display(),
                        source.cli.label(),
                        a.cli.unwrap_or_default().label().to_lowercase()
                    ));
                }
                Some(source)
            }
            None => None,
        };
        // An open agent given a role still goes to taskman, which replaces its role.
        if open && a.role.is_none() {
            show(Line::ok(format!("✦ {name} is already open in taskman")));
        } else {
            let fork_key = fork.map(|t| t.key.clone());
            items.push(Item::Agent { name, role: a.role.clone(), fork: fork_key });
        }
    } else {
        if project.tracker.is_none() {
            return Err(format!("{}, or add an agent with --agent NAME", project.agents_only()));
        }
        let client = Client::for_project(&store::load_config()?, project);
        let mut warned = false;
        if !a.new.is_empty() && session_repo.is_some() {
            for title in &a.new {
                if title.is_empty() {
                    return Err("give the new ticket a title".into());
                }
                let description = a.description.clone().unwrap_or_default();
                asks.push(Item::New { title: title.clone(), description });
            }
        } else if !a.new.is_empty() {
            let c = match &client {
                Ok(Some(c)) => c,
                Ok(None) => {
                    return Err(format!("{} has no tracker to create tickets in", project.name));
                }
                Err(e) => return Err(format!("couldn't create the ticket: {e}")),
            };
            let prefix = project.tracker.as_ref().map(|t| t.key.clone()).unwrap_or_default();
            let description = a.description.as_deref().unwrap_or("");
            for title in &a.new {
                match c.create(&prefix, title, description) {
                    // The key was checked as one; the link is the tracker's.
                    Ok(made) => {
                        cli::out!("Created {}: {}", made.key, untrusted::clean(&made.at));
                        created.push(made.key.clone());
                        items.push(Item::Ticket { key: made.key, title: title.trim().into() });
                    }
                    Err(e) => {
                        let cut_off = e.cut_off;
                        show(Line::err(status::explain_sandbox(
                            local::sandbox_hint(e, codex),
                            cut_off,
                            codex,
                        )));
                    }
                }
            }
        }
        for input in &a.inputs {
            let key = match resolve_key(input, p, &saved.projects) {
                Ok(k) => k,
                Err(e) => {
                    show(Line::err(e));
                    continue;
                }
            };
            if items.iter().any(|i| matches!(i, Item::Ticket { key: k, .. } if *k == key)) {
                continue;
            }
            if base.as_ref() == Some(&key) {
                show(Line::err(format!("{key} can't stack on itself")));
                continue;
            }
            if is_open(&saved, &project.repo, &key) {
                show(Line::ok(format!("{key} is already open in taskman")));
                continue;
            }
            // A key that doesn't exist is refused, so a typo doesn't add a dead ticket; when the
            // tracker can't be asked, the key is added unchecked.
            let checked = match &client {
                Ok(Some(c)) => c.fetch(&key, Text::Markdown),
                Ok(None) => Err(FetchError::Other(String::new())),
                Err(e) => Err(e.clone()),
            };
            let title = match checked {
                Ok(issue) => issue.summary,
                Err(FetchError::NotFound(e)) => {
                    show(Line::err(e));
                    continue;
                }
                Err(e) => {
                    if project.tracker.is_some() && !std::mem::replace(&mut warned, true) {
                        eprintln!(
                            "taskman add: couldn't check the ticket in the tracker, adding it anyway: {}",
                            untrusted::clean(&status::explain_fetch(e, codex))
                        );
                    }
                    String::new()
                }
            };
            items.push(Item::Ticket { key, title });
        }
    }
    // A fork runs in its source's CLI.
    let source_cli = a.fork.as_ref().and_then(|f| fork_source(f, p, &saved).ok()).map(|t| t.cli);
    let req = |items| Request {
        repo: project.repo.clone(),
        from: session.as_deref().map(|s| from(s, me)),
        items,
        cli: source_cli.or(a.cli).unwrap_or_default(),
        worktree: a.worktree,
        base: base.clone(),
        reply_by: 0,
        close: None,
    };
    if !items.is_empty() {
        let lines = submit(req(items), WAIT, codex).map_err(|e| not_added(e, &created))?;
        for line in lines {
            show(line);
        }
    }
    if !asks.is_empty() {
        for line in submit(req(asks), ASK_WAIT, codex)? {
            show(line);
        }
    }
    Ok(all_ok)
}

/// Why the request couldn't be sent, with the tickets `--new` created for it: they're in the
/// tracker but not in taskman, and are added by key, not created again.
fn not_added(e: String, created: &[String]) -> String {
    if created.is_empty() {
        return e;
    }
    format!(
        "{e}\nCreated but not added: {}. Add them with taskman add {} rather than creating them again",
        created.join(", "),
        created.join(" ")
    )
}

/// Leaves `req` in the inbox and returns the TUI's reply, waiting up to `wait` for it; without a
/// reply, what to print about the request waiting there.
pub(crate) fn submit(
    mut req: Request,
    wait_for: Duration,
    codex: bool,
) -> Result<Vec<Line>, String> {
    // With taskman closed nothing waits, so a taskman starting a moment later doesn't reply.
    let alive = tui_running();
    req.reply_by =
        if alive { util::now() + wait_for.as_secs().cast_signed() + 1 } else { util::now() - 1 };
    if alive && let Some(e) = read_alive().and_then(|a| too_old(&req, &a)) {
        return Err(e);
    }
    let id = send(&req, codex)?;
    let reply = if alive { wait(&id, wait_for) } else { None };
    Ok(reply.unwrap_or_else(|| waiting_lines(&req, alive)))
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    /// Ticket numbers, keys or links, as typed.
    inputs: Vec<String>,
    agent: Option<String>,
    role: Option<String>,
    /// `--fork`: the session to fork the agent from, as typed.
    fork: Option<String>,
    project: Option<String>,
    /// None unless asked for: a fork's is its source's.
    cli: Option<Cli>,
    worktree: Option<bool>,
    /// `--on`: the ticket to stack on, as typed.
    on: Option<String>,
    /// `--new`: titles of tickets to create.
    new: Vec<String>,
    /// `--description`: the one new ticket's description.
    description: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut lex = Lexer::new(args, USAGE);
    while let Some(arg) = lex.next()? {
        match arg {
            Arg::Option("--codex") => a.cli = Some(Cli::Codex),
            Arg::Option("--claude") => a.cli = Some(Cli::Claude),
            Arg::Option("--worktree") => a.worktree = Some(true),
            Arg::Option("--no-worktree") => a.worktree = Some(false),
            Arg::Option("--agent") => a.agent = Some(lex.value()?.to_string()),
            Arg::Option("--role") => {
                a.role = Some(lex.value()?.trim().to_string()).filter(|r| !r.is_empty());
            }
            Arg::Option("--fork") => a.fork = Some(lex.value()?.trim().to_string()),
            Arg::Option("--project") => a.project = Some(lex.value()?.to_string()),
            Arg::Option("--on") => a.on = Some(lex.value()?.to_string()),
            Arg::Option("--new") => a.new.push(lex.value()?.trim().to_string()),
            Arg::Option("--description") => a.description = Some(lex.value()?.to_string()),
            Arg::Option(_) => return Err(lex.unknown()),
            Arg::Word(w) => a.inputs.push(w.to_string()),
        }
    }
    if a.agent.is_some() && !(a.inputs.is_empty() && a.new.is_empty()) {
        return Err("--agent adds one agent; add tickets with another taskman add".into());
    }
    if a.description.is_some() && a.new.len() != 1 {
        return Err(if a.new.is_empty() {
            "--description goes with --new: taskman add --new TITLE --description TEXT".into()
        } else {
            "--description is for one new ticket; create the others with another taskman add".into()
        });
    }
    if a.role.is_some() && a.agent.is_none() {
        return Err("--role is for agents: taskman add --agent NAME --role TEXT".into());
    }
    if a.fork.is_some() && a.agent.is_none() {
        return Err("--fork starts a new agent: taskman add --agent NAME --fork KEY".into());
    }
    if a.agent.is_some() && a.worktree.is_some() {
        return Err("agents run in the main checkout; drop --worktree and --no-worktree".into());
    }
    if a.on.is_some() {
        if a.agent.is_some() {
            return Err("agents run in the main checkout, so they can't stack; drop --on".into());
        }
        if a.worktree == Some(false) {
            return Err("--on starts the ticket's worktree from another ticket's branch; drop --no-worktree".into());
        }
        a.worktree = Some(true);
    }
    if a.agent.is_none() && a.inputs.is_empty() && a.new.is_empty() {
        return Err(format!("name a ticket to add, like taskman add ENG-52\n\n{USAGE}"));
    }
    Ok(a)
}

/// The key `--on` names: a ticket open in project `p` with a branch of its own. The TUI checks
/// again as it adds, in case that changed meanwhile.
fn base_key(input: &str, p: usize, saved: &Saved) -> Result<String, String> {
    let key = resolve_key(input, p, &saved.projects)?;
    let project = &saved.projects[p];
    match saved
        .tickets
        .iter()
        .find(|t| t.key.eq_ignore_ascii_case(&key) && t.repo == project.repo && t.agent.is_none())
    {
        None => {
            Err(format!("{key} isn't open in {}; add it first, then stack on it", project.name))
        }
        Some(t) if t.branch.is_none() => Err(format!(
            "{key} has no branch yet to stack on; wait for its worktree, or add without --on"
        )),
        Some(t) => Ok(t.key.clone()),
    }
}

/// The session `--fork` names in project `p`: an agent by its name, or a ticket by its number or
/// key, with a conversation to copy. The TUI checks again as it forks.
fn fork_source<'a>(input: &str, p: usize, saved: &'a Saved) -> Result<&'a Ticket, String> {
    let project = &saved.projects[p];
    let source = match store::find_agent(&saved.tickets, &project.repo, input) {
        Some(agent) => agent,
        None => {
            let key = resolve_key(input, p, &saved.projects).map_err(|_| {
                format!("{input} isn't an agent or ticket open in {}", project.name)
            })?;
            saved
                .tickets
                .iter()
                .find(|t| t.repo == project.repo && t.key.eq_ignore_ascii_case(&key))
                .ok_or_else(|| format!("{key} isn't open in {}", project.name))?
        }
    };
    // The hooks write the latest id to the status file a moment before the TUI saves it.
    let latest = crate::status_file::read(&store::status_file(&source.repo, &source.key))
        .and_then(|r| r.session_id);
    if !latest.iter().chain(&source.session_id).any(|sid| source.cli.has_conversation(sid)) {
        return Err(format!(
            "{} has no conversation to fork yet: start its session and wait for its first turn",
            source.display()
        ));
    }
    Ok(source)
}

/// Whether project `repo` has `key` open; another project's ticket with the same key doesn't count.
fn is_open(saved: &Saved, repo: &Path, key: &str) -> bool {
    saved.tickets.iter().any(|t| t.repo == repo && t.key.eq_ignore_ascii_case(key))
}

/// How the toast names the session asking (`TASKMAN_TICKET`, which is `me`): an agent by its
/// name, a ticket by its key.
pub(crate) fn from(session: &str, me: Option<&Ticket>) -> String {
    match me.and_then(|t| t.agent.as_ref()) {
        Some(name) => format!("✦ {name}"),
        None => session.to_string(),
    }
}

/// Leaves the request in the inbox; returns its id. Ids sort in the order they were made.
fn send(req: &Request, codex: bool) -> Result<String, String> {
    let nanos =
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let id = format!("{nanos:020}-{}", std::process::id());
    let json = serde_json::to_vec_pretty(req).map_err(|e| e.to_string())?;
    store::write_atomic(&inbox().join(format!("{id}.json")), &json).map_err(|e| {
        let hint = if codex {
            "; Codex's sandbox blocks writing there: run taskman add outside it"
        } else {
            ""
        };
        format!("couldn't write to {}: {e}{hint}", store::tilde(&inbox()))
    })?;
    Ok(id)
}

/// Whether the TUI that last wrote `alive` at `alive` is running at `now`: the file is within
/// `ALIVE` of now, either way.
fn running(alive: Option<SystemTime>, now: SystemTime) -> bool {
    alive.is_some_and(|m| now.duration_since(m).unwrap_or_else(|e| e.duration()) < ALIVE)
}

fn wait(id: &str, wait_for: Duration) -> Option<Vec<Line>> {
    let path = reply_file(id);
    let until = Instant::now() + wait_for;
    while Instant::now() < until {
        if let Ok(text) = read_regular(&path) {
            let _ = std::fs::remove_file(&path);
            return serde_json::from_str(&text).ok();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// What to print for a request no reply came for: it stays queued.
fn waiting_lines(req: &Request, alive: bool) -> Vec<Line> {
    let waits = |what: String| {
        Line::ok(if alive {
            format!("taskman didn't answer in time; {what} when it does.")
        } else {
            format!("taskman isn't running; {what} when it starts.")
        })
    };
    match req.close {
        Some(c) if req.items.is_empty() && c.merged => {
            vec![waits("it closes the project's merged tickets".into())]
        }
        Some(c) => req
            .items
            .iter()
            .map(|item| {
                let name = match item {
                    Item::Ticket { key, .. } => key.clone(),
                    Item::Agent { name, .. } => format!("✦ {name}"),
                    Item::New { title, .. } => untrusted::line(title),
                };
                waits(if c.merged {
                    format!("{name} closes if its PR has merged")
                } else {
                    format!("{name} closes")
                })
            })
            .collect(),
        None => req
            .items
            .iter()
            .map(|item| match item {
                Item::Ticket { key, .. } => waits(format!("{key} is added")),
                Item::Agent { name, .. } => waits(format!("it asks the user to approve ✦ {name}")),
                // taskman has the request: the user hasn't answered.
                Item::New { title, .. } if alive => Line::ok(format!(
                    "The user hasn't answered yet. If they approve \"{}\", taskman creates and adds it; `taskman status` lists it then.",
                    untrusted::line(title)
                )),
                Item::New { title, .. } => waits(format!(
                    "it asks the user to approve creating \"{}\"",
                    untrusted::line(title)
                )),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::saved;

    fn args(s: &str) -> Result<Args, String> {
        parse_args(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn parses_arguments() {
        assert_eq!(
            args("ENG-52 53"),
            Ok(Args { inputs: vec!["ENG-52".into(), "53".into()], ..Default::default() })
        );
        let a = args("52 --codex --no-worktree --project acme-web").unwrap();
        assert_eq!(
            (a.cli, a.worktree, a.project.as_deref()),
            (Some(Cli::Codex), Some(false), Some("acme-web"))
        );
        assert_eq!(args("52 --worktree --project=acme").unwrap().worktree, Some(true));
        assert_eq!(args("--agent reviewer").unwrap().agent.as_deref(), Some("reviewer"));
        // A link with a query string isn't an option.
        assert_eq!(
            args("https://example.atlassian.net/jira?selectedIssue=ENG-52").unwrap().inputs.len(),
            1
        );
        assert!(args("").unwrap_err().starts_with("name a ticket"));
        assert!(args("52 --force").unwrap_err().starts_with("unknown option --force"));
        assert_eq!(args("52 --project"), Err("--project needs a value".into()));
        assert_eq!(args("52 --project="), Err("--project needs a value".into()));
        assert_eq!(args("52 --codex=no"), Err("--codex takes no value".into()));
        assert!(args("--agent reviewer 52").is_err());
        assert!(args("--agent reviewer --no-worktree").is_err());
        // --on creates the worktree it stacks.
        let on = args("53 --on 52").unwrap();
        assert_eq!((on.on.as_deref(), on.worktree), (Some("52"), Some(true)));
        assert!(args("53 --on 52 --no-worktree").unwrap_err().contains("drop --no-worktree"));
        assert!(args("--agent reviewer --on 52").unwrap_err().contains("drop --on"));
        // --description goes with one --new.
        let a = args("--new Fix --description Steps").unwrap();
        assert_eq!((a.new, a.description.as_deref()), (vec!["Fix".to_string()], Some("Steps")));
        assert!(args("52 --description Steps").unwrap_err().contains("goes with --new"));
        assert!(args("--new A --new B --description C").unwrap_err().contains("one new ticket"));
    }

    #[test]
    fn names_the_tickets_it_created_but_couldnt_add() {
        assert_eq!(not_added("couldn't write".into(), &[]), "couldn't write");
        assert_eq!(
            not_added("couldn't write".into(), &["ENG-53".into(), "ENG-54".into()]),
            "couldn't write\nCreated but not added: ENG-53, ENG-54. Add them with taskman add ENG-53 ENG-54 rather than creating them again"
        );
    }

    #[test]
    fn stacks_only_on_open_tickets_with_a_branch() {
        let mut s = saved();
        assert_eq!(
            base_key("52", 0, &s),
            Err("ENG-52 has no branch yet to stack on; wait for its worktree, or add without --on"
                .into())
        );
        s.tickets[0].branch = Some("eng-52-login".into());
        assert_eq!(base_key("eng-52", 0, &s), Ok("ENG-52".into()));
        assert!(base_key("51", 0, &s).unwrap_err().starts_with("ENG-51 isn't open in acme-web"));
        assert!(base_key("API-1", 0, &s).unwrap_err().contains("doesn't belong"));
    }

    #[test]
    fn refuses_what_an_older_taskman_would_drop() {
        let req = Request {
            repo: "/src/acme-web".into(),
            from: None,
            items: vec![],
            cli: Cli::Claude,
            worktree: None,
            base: None,
            reply_by: 0,
            close: None,
        };
        let stacked = Request { base: Some("ENG-51".into()), ..req.clone() };
        let old = alive("1.0.0", 1);
        assert_eq!(too_old(&req, &old), None);
        assert!(
            too_old(&stacked, &old)
                .unwrap()
                .starts_with("taskman 1.0.0 is running, which doesn't know --on yet")
        );
        assert_eq!(too_old(&stacked, &alive("1.1.0", INBOX_FORMAT)), None);
        let agent = |role: Option<&str>| Request {
            items: vec![Item::Agent {
                name: "tester".into(),
                role: role.map(String::from),
                fork: None,
            }],
            ..req.clone()
        };
        let stacking = alive("1.1.0", 2);
        assert_eq!(too_old(&agent(None), &stacking), None);
        assert!(
            too_old(&agent(Some("Test PRs")), &stacking)
                .unwrap()
                .starts_with("taskman 1.1.0 is running, which doesn't know --role yet")
        );
        assert_eq!(too_old(&agent(Some("Test PRs")), &alive("1.2.0", INBOX_FORMAT)), None);
        // taskman close needs format 4; a TUI before it can't read the field either.
        let close =
            Request { close: Some(Close { merged: true, keep_worktree: false }), ..req.clone() };
        assert!(too_old(&close, &alive("1.2.0", 3)).unwrap().starts_with(
            "taskman 1.2.0 is running, which doesn't know taskman close yet; restart it"
        ));
        assert_eq!(too_old(&close, &alive("1.3.0", INBOX_FORMAT)), None);
        let json = serde_json::to_value(&close).unwrap();
        assert_eq!(json["close"], serde_json::json!({ "merged": true }));
        assert_eq!(serde_json::from_value::<Request>(json).unwrap(), close);
        let mut item = serde_json::to_value(agent(Some("Test PRs"))).unwrap();
        item["items"][0]["later"] = true.into();
        assert!(serde_json::from_value::<Request>(item).is_err());
        // An older TUI fails to read a field it doesn't know, rather than dropping it.
        let mut json = serde_json::to_value(&stacked).unwrap();
        json["later"] = true.into();
        assert!(serde_json::from_value::<Request>(json).is_err());
    }

    /// A session's `--new` is a request for the TUI to create the ticket once the user approves;
    /// a TUI before format 5 would add none of it, so it's refused before it's sent.
    #[test]
    fn a_sessions_new_ticket_goes_to_taskman_to_create() {
        let req = Request {
            repo: "/src/acme-web".into(),
            from: Some("ENG-52".into()),
            items: vec![Item::New {
                title: "Build log".into(),
                description: "API_KEY=sk-mock-123".into(),
            }],
            cli: Cli::Claude,
            worktree: None,
            base: None,
            reply_by: 100,
            close: None,
        };
        assert_eq!(req.new_tickets(), [("Build log", "API_KEY=sk-mock-123")]);
        assert_eq!(req.agent(), None);
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json["items"][0],
            serde_json::json!({ "kind": "new", "title": "Build log", "description": "API_KEY=sk-mock-123" })
        );
        assert_eq!(serde_json::from_value::<Request>(json).unwrap(), req);
        let bare: Item = serde_json::from_str(r#"{"kind":"new","title":"Fix"}"#).unwrap();
        assert_eq!(bare, Item::New { title: "Fix".into(), description: String::new() });

        assert!(
            too_old(&req, &alive("1.3.0", 4)).unwrap().starts_with(
                "taskman 1.3.0 is running, which doesn't know --new from a session yet"
            )
        );
        assert_eq!(too_old(&req, &alive("1.4.0", INBOX_FORMAT)), None);

        assert_eq!(
            declined(&req),
            [Line::err("the user declined creating \"Build log\" in taskman; nothing was created")]
        );
        assert_eq!(
            waiting_lines(&req, true),
            [Line::ok(
                "The user hasn't answered yet. If they approve \"Build log\", taskman creates and adds it; `taskman status` lists it then."
            )]
        );
        assert_eq!(
            waiting_lines(&req, false),
            [Line::ok(
                "taskman isn't running; it asks the user to approve creating \"Build log\" when it starts."
            )]
        );
    }

    #[test]
    fn forks_only_into_a_new_agent_from_a_session_with_a_conversation() {
        let a = args("--agent deep-dive --fork planner --role Dig").unwrap();
        assert_eq!((a.fork.as_deref(), a.cli), (Some("planner"), None));
        assert_eq!(
            args("--fork planner").unwrap_err(),
            "--fork starts a new agent: taskman add --agent NAME --fork KEY"
        );
        assert_eq!(args("--agent x --fork"), Err("--fork needs a value".into()));

        let mut saved = saved();
        // Found by an agent's name or a ticket's number, in the project only.
        let no_conversation = fork_source("planner", 1, &saved).unwrap_err();
        assert_eq!(
            no_conversation,
            "planner has no conversation to fork yet: start its session and wait for its first turn"
        );
        saved.tickets[1].session_id = Some("sid-planner".into());
        Cli::Claude.leave_transcript("sid-planner");
        assert_eq!(fork_source("planner", 1, &saved).unwrap().key, "acme-api-planner");
        saved.tickets[0].session_id = Some("sid-planner".into());
        assert_eq!(fork_source("52", 0, &saved).unwrap().key, "ENG-52");
        assert_eq!(fork_source("53", 0, &saved).unwrap_err(), "ENG-53 isn't open in acme-web");
        assert!(fork_source("planner", 0, &saved).is_err(), "another project's agent");
    }

    #[test]
    fn says_what_a_fork_waits_for() {
        let req = Request {
            repo: "/src/acme".into(),
            from: None,
            items: vec![Item::Agent {
                name: "deep-dive".into(),
                role: None,
                fork: Some("ENG-52".into()),
            }],
            cli: Cli::Claude,
            worktree: None,
            base: None,
            reply_by: 0,
            close: None,
        };
        assert_eq!(req.fork(), Some("ENG-52"));
        assert_eq!(
            asked(&req, false),
            [Line::ok(
                "taskman asks the user to approve ✦ deep-dive, a fork of ENG-52; its session starts once they do, and `taskman status` lists it"
            )]
        );
        // An older taskman would add a plain agent: it's refused instead.
        assert!(too_old(&req, &alive("0.1.35", 5)).unwrap().contains("doesn't know --fork yet"));
        assert_eq!(too_old(&req, &alive("0.1.36", INBOX_FORMAT)), None);
    }

    #[test]
    fn takes_a_role_only_for_an_agent() {
        let a = parse_args(&[
            "--agent".into(),
            "tester".into(),
            "--role".into(),
            " Test PRs against their tickets ".into(),
        ])
        .unwrap();
        assert_eq!(
            (a.agent.as_deref(), a.role.as_deref()),
            (Some("tester"), Some("Test PRs against their tickets"))
        );
        assert_eq!(args("--agent tester --role=Test").unwrap().role.as_deref(), Some("Test"));
        assert!(args("52 --role Test").unwrap_err().starts_with("--role is for agents"));
        assert!(args("--role Test").unwrap_err().starts_with("--role is for agents"));
        assert_eq!(args("--agent tester --role"), Err("--role needs a value".into()));
        assert_eq!(args("--agent tester").unwrap().role, None);
    }

    #[test]
    fn knows_what_is_already_open_and_who_asks() {
        let s = saved();
        let web = Path::new("/src/acme-web");
        assert!(is_open(&s, web, "ENG-52"));
        assert!(is_open(&s, web, "eng-52"));
        assert!(!is_open(&s, web, "ENG-53"));
        // Another project's ticket with the same key isn't this one.
        assert!(!is_open(&s, Path::new("/src/acme-api"), "ENG-52"));
        let me = |key: &str| store::session_ticket(&s.tickets, key, None);
        assert_eq!(from("acme-api-planner", me("acme-api-planner")), "✦ planner");
        assert_eq!(from("ENG-52", me("ENG-52")), "ENG-52");
    }

    #[test]
    fn encodes_requests_and_replies() {
        let req = Request {
            repo: "/src/acme-web".into(),
            from: Some("✦ planner".into()),
            items: vec![
                Item::Ticket { key: "ENG-52".into(), title: "Fix login".into() },
                Item::Agent {
                    name: "reviewer".into(),
                    role: Some("Review PRs".into()),
                    fork: None,
                },
            ],
            cli: Cli::Codex,
            worktree: Some(false),
            base: Some("ENG-51".into()),
            reply_by: 100,
            close: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json["items"][0],
            serde_json::json!({ "kind": "ticket", "key": "ENG-52", "title": "Fix login" })
        );
        assert_eq!(
            json["items"][1],
            serde_json::json!({ "kind": "agent", "name": "reviewer", "role": "Review PRs" })
        );
        assert_eq!(json["cli"], "codex");
        assert_eq!(serde_json::from_value::<Request>(json).unwrap(), req);
        // Defaults stay out of the file, and read back the same.
        let plain = Request {
            from: None,
            cli: Cli::Claude,
            worktree: None,
            base: None,
            items: vec![Item::Ticket { key: "ENG-52".into(), title: String::new() }],
            ..req
        };
        let text = serde_json::to_string(&plain).unwrap();
        assert_eq!(
            text,
            r#"{"repo":"/src/acme-web","items":[{"kind":"ticket","key":"ENG-52"}],"reply_by":100}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), plain);
        let lines = vec![Line::ok("Added ENG-52"), Line::err("ENG-99 not found")];
        assert_eq!(
            serde_json::from_str::<Vec<Line>>(&serde_json::to_string(&lines).unwrap()).unwrap(),
            lines
        );
    }

    #[test]
    fn says_an_agent_waits_for_the_user() {
        let mut req = Request {
            repo: "/src/acme".into(),
            from: Some("ENG-52".into()),
            items: vec![Item::Ticket { key: "ENG-52".into(), title: String::new() }],
            cli: Cli::Claude,
            worktree: None,
            base: None,
            reply_by: 100,
            close: None,
        };
        assert_eq!(req.agent(), None);
        assert!(asked(&req, false).is_empty());
        req.items = vec![Item::Agent {
            name: "ops".into(),
            role: Some("Deploy on merge".into()),
            fork: None,
        }];
        assert_eq!(req.agent(), Some(("ops", Some("Deploy on merge"))));
        assert_eq!(
            asked(&req, false)[0].text,
            "taskman asks the user to approve ✦ ops with its role; its session starts once they do, and `taskman status` lists it"
        );
        assert_eq!(
            asked(&req, true)[0].text,
            "taskman asks the user to approve ✦ ops's new role; it applies when its session next starts"
        );
    }

    #[test]
    fn takes_only_regular_files_of_a_requests_size() {
        let dir = inbox();
        std::fs::create_dir_all(&dir).unwrap();
        let req =
            r#"{"repo":"/src/acme","items":[{"kind":"ticket","key":"ENG-52"}],"reply_by":100}"#;
        std::fs::write(dir.join("0001.json"), req).unwrap();
        // A pipe: opened the usual way, it waits for a writer that never comes.
        let fifo = std::ffi::CString::new(dir.join("0002.json").to_str().unwrap()).unwrap();
        // SAFETY: mkfifo reads the path, a NUL-terminated string, and creates the pipe.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        // A link to a request elsewhere, and a file far too big to be one.
        std::fs::write(dir.join("elsewhere.txt"), req).unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere.txt"), dir.join("0003.json")).unwrap();
        let big = vec![b' '; usize::try_from(MAX_REQUEST).unwrap() + 1];
        std::fs::write(dir.join("0004.json"), big).unwrap();

        let start = Instant::now();
        let taken = take();
        assert!(start.elapsed() < Duration::from_secs(5), "take waited on the pipe");
        let ids: Vec<&str> = taken.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["0001"]);
        // The pipe and the link stay, untouched; the big file goes, answered.
        use std::os::unix::fs::FileTypeExt;
        assert!(std::fs::symlink_metadata(dir.join("0002.json")).unwrap().file_type().is_fifo());
        assert!(std::fs::symlink_metadata(dir.join("0003.json")).unwrap().is_symlink());
        assert!(dir.join("elsewhere.txt").exists() && !dir.join("0004.json").exists());
        let reply: Vec<Line> =
            serde_json::from_str(&std::fs::read_to_string(reply_file("0004")).unwrap()).unwrap();
        assert!(reply[0].text.contains("far more than a request"), "{:?}", reply[0].text);
        assert!(!reply_file("0002").exists() && !reply_file("0003").exists());
    }

    #[test]
    fn reads_the_alive_file_as_the_tui_writes_it() {
        let alive = Alive::new();
        let json = serde_json::to_string(&alive).unwrap();
        assert_eq!(parse_alive(&json), Some(alive));
        // Caught empty or part-way through an older build's rewrite: not a format, so asked again.
        assert_eq!(parse_alive(""), None);
        assert_eq!(parse_alive(&json[..json.len() / 2]), None);
        // From before `lock`, the pid and the host were written.
        let old = parse_alive(r#"{"version":"0.1.28","inbox":5}"#).unwrap();
        assert_eq!((old.lock, old.pid, old.host.as_deref()), (false, None, None));
        let old = parse_alive(r#"{"version":"0.1.30","inbox":5,"lock":true,"pid":7}"#).unwrap();
        assert_eq!((old.pid, old.host.as_deref(), old.elsewhere()), (Some(7), None, false));
    }

    /// What a TUI of `version` that takes the lock writes, with no pid or host.
    fn alive(version: &str, inbox: u32) -> Alive {
        Alive { version: version.into(), inbox, lock: true, pid: None, host: None }
    }

    /// A pid that's gone, as a killed taskman's is once its parent has seen it end.
    fn ended_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        child.id()
    }

    /// A command counts taskman as running while it holds the state folder's lock and `alive` is
    /// there, however old the file looks: its time is read on the wall clock, which can step
    /// forward by seconds.
    #[test]
    fn taskman_runs_while_it_holds_the_lock() {
        let age = |secs| {
            let f = std::fs::File::options().write(true).open(alive_file()).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
        };
        assert!(!tui_running(), "nothing there");
        let lock = store::lock().unwrap();
        assert!(heartbeat());
        assert!(tui_running());
        age(3600);
        assert!(tui_running(), "a heartbeat that looks old, after a step of the clock");
        stopped();
        assert!(!tui_running(), "quitting: it has removed `alive`, and requests queue");
        drop(lock);
        assert!(heartbeat());
        age(3600);
        let until = Instant::now() + Duration::from_secs(5);
        while tui_running() {
            assert!(Instant::now() < until, "a crashed taskman's old heartbeat counts as running");
            std::thread::sleep(Duration::from_millis(10));
        }
        // A release from before the lock keeps `alive` fresh and takes no lock, and so does one
        // whose lock file was removed.
        assert!(heartbeat());
        assert!(tui_running());
        // One that crashed a moment ago: fresh, and its process gone.
        let crashed = Alive { pid: Some(ended_pid()), ..Alive::new() };
        store::write_unsynced(&alive_file(), &serde_json::to_vec(&crashed).unwrap()).unwrap();
        assert!(!tui_running());
    }

    /// A taskman that has just taken the lock trusts it over a heartbeat left by one that was
    /// killed (kill -9, an OOM kill) a moment ago, but not over one from before the lock.
    #[test]
    fn a_heartbeat_left_by_a_killed_taskman_is_cleared() {
        let write = |alive: &Alive| {
            store::write_unsynced(&alive_file(), &serde_json::to_vec(alive).unwrap()).unwrap();
        };
        let _lock = store::lock().unwrap();
        assert_eq!(before(), Before::Quit, "nothing there");
        assert!(heartbeat());
        assert_eq!(before(), Before::Crashed, "fresh, from a release that takes the lock");
        assert!(!alive_file().exists());
        // Releases out before `lock` was written, by their number.
        for version in ["0.1.28", "0.4.16", "0.4.34"] {
            write(&Alive { lock: false, ..alive(version, 5) });
            assert_eq!(before(), Before::Crashed, "{version}");
        }
        for version in ["0.4.8", "0.4.15"] {
            write(&Alive { lock: false, ..alive(version, 2) });
            let running = Before::Running(None, None);
            assert_eq!(before(), running, "{version} takes no lock: it may be running");
            assert!(alive_file().exists());
        }
        // Even one of those, once its heartbeat has stopped.
        let f = std::fs::File::options().write(true).open(alive_file()).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(60)).unwrap();
        assert_eq!(before(), Before::Crashed);
        // Older still: an empty file, which says nothing, so may be running.
        std::fs::write(alive_file(), "").unwrap();
        assert_eq!(before(), Before::Running(None, None));
    }

    /// A taskman whose lock file was removed while it ran (or that flock doesn't reach) is still
    /// running by its heartbeat's pid; one whose pid is gone, or is this process's, was killed.
    #[test]
    fn a_fresh_heartbeat_from_a_running_process_refuses() {
        let write = |pid| {
            let alive = Alive { pid: Some(pid), ..Alive::new() };
            store::write_unsynced(&alive_file(), &serde_json::to_vec(&alive).unwrap()).unwrap();
        };
        let _lock = store::lock().unwrap();
        // Init always runs: ours to signal or not (EPERM), it counts.
        write(1);
        assert_eq!(before(), Before::Running(Some(1), None));
        assert!(alive_file().exists());
        write(ended_pid());
        assert_eq!(before(), Before::Crashed);
        assert!(!alive_file().exists());
        write(std::process::id());
        assert_eq!(before(), Before::Crashed, "a killed taskman's pid, now this one's");
        // Once its heartbeat has stopped, the pid may be another program's by now.
        write(1);
        let f = std::fs::File::options().write(true).open(alive_file()).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(60)).unwrap();
        assert_eq!(before(), Before::Crashed);
        assert!(!pid_alive(0) && !pid_alive(u32::MAX), "process groups, not a process");
    }

    /// On a state folder shared between machines, a fresh heartbeat from another one names a
    /// process there, which can't be checked from here: it counts as running, whatever its pid
    /// is here, while one from this machine is checked as before.
    #[test]
    fn a_fresh_heartbeat_from_another_machine_counts_as_running() {
        let write = |pid, host: &str| {
            let alive = Alive { pid: Some(pid), host: Some(host.into()), ..Alive::new() };
            store::write_unsynced(&alive_file(), &serde_json::to_vec(&alive).unwrap()).unwrap();
        };
        let here = hostname().expect("this machine's name");
        let there = format!("{here}-other");
        assert!(!Alive::new().elsewhere());
        // Gone here, or this process's own: neither says anything about the other machine.
        let gone = ended_pid();
        write(gone, &there);
        assert!(tui_running(), "no lock here: by the heartbeat alone");
        let _lock = store::lock().unwrap();
        assert_eq!(before(), Before::Running(Some(gone), Some(there.clone())));
        assert!(alive_file().exists());
        write(std::process::id(), &there);
        assert_eq!(before(), Before::Running(Some(std::process::id()), Some(there.clone())));
        // This machine's, its process gone: a crash, as before.
        write(ended_pid(), &here);
        assert_eq!(before(), Before::Crashed);
        // Once its heartbeat has stopped, the other machine's taskman has too.
        write(1, &there);
        let f = std::fs::File::options().write(true).open(alive_file()).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(60)).unwrap();
        assert_eq!(before(), Before::Crashed);
    }

    /// `main` ending removes `alive`, but not on a panic, so the next start says it crashed.
    #[test]
    fn a_panic_leaves_the_heartbeat() {
        struct Running;
        impl Drop for Running {
            fn drop(&mut self) {
                ending();
            }
        }
        crate::util::quiet_test_panics();
        assert!(heartbeat());
        // On this thread: another would have a scratch state folder of its own.
        let panicked = std::panic::catch_unwind(|| {
            let _running = Running;
            std::panic::panic_any(crate::util::TestPanic);
        });
        assert!(panicked.is_err());
        assert!(alive_file().exists());
        drop(Running);
        assert!(!alive_file().exists());
    }

    #[test]
    fn queues_when_taskman_is_closed_or_silent() {
        let now = SystemTime::now();
        assert!(running(Some(now - Duration::from_secs(1)), now));
        assert!(!running(Some(now - Duration::from_secs(10)), now));
        assert!(!running(None, now));
        // Left by a killed TUI before the clock stepped back: stale, not running.
        assert!(running(Some(now + Duration::from_secs(1)), now));
        assert!(!running(Some(now + Duration::from_secs(60)), now));
        let req = Request {
            repo: "/src/acme-web".into(),
            from: None,
            items: vec![
                Item::Ticket { key: "ENG-52".into(), title: String::new() },
                Item::Agent { name: "reviewer".into(), role: None, fork: None },
            ],
            cli: Cli::Claude,
            worktree: None,
            base: None,
            reply_by: 100,
            close: None,
        };
        assert_eq!(
            waiting_lines(&req, false),
            vec![
                Line::ok("taskman isn't running; ENG-52 is added when it starts."),
                Line::ok(
                    "taskman isn't running; it asks the user to approve ✦ reviewer when it starts."
                )
            ]
        );
        assert!(waiting_lines(&req, true)[0].text.starts_with("taskman didn't answer"));
        let close =
            Request { close: Some(Close { merged: true, keep_worktree: false }), ..req.clone() };
        assert_eq!(
            waiting_lines(&close, false)[0].text,
            "taskman isn't running; ENG-52 closes if its PR has merged when it starts."
        );
        assert_eq!(
            waiting_lines(&Request { items: vec![], ..close }, false),
            vec![Line::ok(
                "taskman isn't running; it closes the project's merged tickets when it starts."
            )]
        );
        // The TUI answers only while the command still waits; after that, the add was queued.
        assert!(!queued(&req, 100));
        assert!(queued(&req, 101));
    }

    mod options {
        use super::parse_args;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]

            /// Every option with a value reads `--x=VALUE` as `--x VALUE`, valid or not.
            #[test]
            fn values_after_equals_read_the_same(
                option in prop::sample::select(
                    &["--agent", "--role", "--project", "--on", "--new", "--description"][..],
                ),
                value in "\\PC{0,12}",
                inputs in prop::collection::vec("[0-9]{1,3}|ENG-[0-9]{1,3}|--codex|--no-worktree", 0..3),
            ) {
                let words = |option: Vec<String>| {
                    inputs.iter().cloned().chain(option).collect::<Vec<_>>()
                };
                prop_assert_eq!(
                    parse_args(&words(vec![format!("{option}={value}")])),
                    parse_args(&words(vec![option.to_string(), value.clone()]))
                );
            }
        }
    }
}
