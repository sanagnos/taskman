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
       taskman add --agent NAME [--role TEXT] [options]

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
/// `role`, 4 `close`, 5 `new` items.
const INBOX_FORMAT: u32 = 5;

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
            Item::Agent { name, role } => Some((name.as_str(), role.as_deref())),
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

/// What the `alive` file holds: which version runs, and the requests it understands.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Alive {
    version: String,
    inbox: u32,
    /// Whether that TUI holds the state folder's lock while it runs; written from this release
    /// on, as release numbers alone may not tell later (see `holds_lock`).
    #[serde(default)]
    lock: bool,
}

impl Alive {
    fn new() -> Self {
        Alive { version: env!("CARGO_PKG_VERSION").into(), inbox: INBOX_FORMAT, lock: true }
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
    let (needs, flag) = if !req.new_tickets().is_empty() {
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
    /// A release from before the lock is running: it keeps `alive` fresh and takes no lock.
    Running,
    /// It was killed or crashed, leaving `alive` behind, which is now removed.
    Crashed,
}

/// What came before this TUI on the state folder, by the `alive` file; asked once this one holds
/// the lock, so nothing that takes the lock is running. A TUI that takes it removes `alive` as it
/// quits, so a file still there, however fresh, was left by one that didn't quit: a kill -9 or an
/// OOM kill, which leave a heartbeat under `ALIVE` old to a taskman started straight after.
pub fn before() -> Before {
    let Ok(modified) = std::fs::metadata(alive_file()).and_then(|m| m.modified()) else {
        return Before::Quit;
    };
    // One that can't be read (an older build's, cut short) counts as running, as before.
    if running(Some(modified), SystemTime::now()) && !read_alive().is_some_and(|a| a.holds_lock()) {
        return Before::Running;
    }
    stopped();
    Before::Crashed
}

/// Whether the TUI runs on this state folder, for a command leaving it a request: it holds the
/// state folder's lock (`store::locked`) and hasn't removed `alive`, which it writes once it has
/// the lock and removes as it quits, or it keeps `alive` fresh, as a release from before the lock
/// does. Not by the heartbeat's age alone: that's the file's time on the wall clock, which can
/// step forward by seconds (WSL resyncs its clock every half minute or so, by about 2.5 s), and a
/// command would then say taskman isn't running while it is.
pub fn tui_running() -> bool {
    let alive = std::fs::metadata(alive_file()).and_then(|m| m.modified()).ok();
    (alive.is_some() && store::locked()) || running(alive, SystemTime::now())
}

/// The TUI is quitting: commands from now on queue without waiting.
pub fn stopped() {
    let _ = std::fs::remove_file(alive_file());
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
        // An open agent given a role still goes to taskman, which replaces its role.
        if open && a.role.is_none() {
            show(Line::ok(format!("✦ {name} is already open in taskman")));
        } else {
            items.push(Item::Agent { name, role: a.role.clone() });
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
    let req = |items| Request {
        repo: project.repo.clone(),
        from: session.as_deref().map(|s| from(s, me)),
        items,
        cli: a.cli,
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
    project: Option<String>,
    cli: Cli,
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
            Arg::Option("--codex") => a.cli = Cli::Codex,
            Arg::Option("--claude") => a.cli = Cli::Claude,
            Arg::Option("--worktree") => a.worktree = Some(true),
            Arg::Option("--no-worktree") => a.worktree = Some(false),
            Arg::Option("--agent") => a.agent = Some(lex.value()?.to_string()),
            Arg::Option("--role") => {
                a.role = Some(lex.value()?.trim().to_string()).filter(|r| !r.is_empty());
            }
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
            (Cli::Codex, Some(false), Some("acme-web"))
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
        let old = Alive { version: "1.0.0".into(), inbox: 1, lock: true };
        assert_eq!(too_old(&req, &old), None);
        assert!(
            too_old(&stacked, &old)
                .unwrap()
                .starts_with("taskman 1.0.0 is running, which doesn't know --on yet")
        );
        assert_eq!(
            too_old(&stacked, &Alive { version: "1.1.0".into(), inbox: INBOX_FORMAT, lock: true }),
            None
        );
        let agent = |role: Option<&str>| Request {
            items: vec![Item::Agent { name: "tester".into(), role: role.map(String::from) }],
            ..req.clone()
        };
        let stacking = Alive { version: "1.1.0".into(), inbox: 2, lock: true };
        assert_eq!(too_old(&agent(None), &stacking), None);
        assert!(
            too_old(&agent(Some("Test PRs")), &stacking)
                .unwrap()
                .starts_with("taskman 1.1.0 is running, which doesn't know --role yet")
        );
        assert_eq!(
            too_old(
                &agent(Some("Test PRs")),
                &Alive { version: "1.2.0".into(), inbox: INBOX_FORMAT, lock: true }
            ),
            None
        );
        // taskman close needs format 4; a TUI before it can't read the field either.
        let close =
            Request { close: Some(Close { merged: true, keep_worktree: false }), ..req.clone() };
        assert!(
            too_old(&close, &Alive { version: "1.2.0".into(), inbox: 3, lock: true })
                .unwrap()
                .starts_with(
                    "taskman 1.2.0 is running, which doesn't know taskman close yet; restart it"
                )
        );
        assert_eq!(
            too_old(&close, &Alive { version: "1.3.0".into(), inbox: INBOX_FORMAT, lock: true }),
            None
        );
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
            too_old(&req, &Alive { version: "1.3.0".into(), inbox: 4, lock: true })
                .unwrap()
                .starts_with(
                    "taskman 1.3.0 is running, which doesn't know --new from a session yet"
                )
        );
        assert_eq!(
            too_old(&req, &Alive { version: "1.4.0".into(), inbox: INBOX_FORMAT, lock: true }),
            None
        );

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
                Item::Agent { name: "reviewer".into(), role: Some("Review PRs".into()) },
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
        req.items = vec![Item::Agent { name: "ops".into(), role: Some("Deploy on merge".into()) }];
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
        let alive = Alive { version: "1.2.0".into(), inbox: INBOX_FORMAT, lock: true };
        let json = serde_json::to_string(&alive).unwrap();
        assert_eq!(parse_alive(&json), Some(alive));
        // Caught empty or part-way through an older build's rewrite: not a format, so asked again.
        assert_eq!(parse_alive(""), None);
        assert_eq!(parse_alive(&json[..json.len() / 2]), None);
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
        // A release from before the lock keeps `alive` fresh and takes no lock.
        assert!(heartbeat());
        assert!(tui_running());
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
            write(&Alive { version: version.into(), inbox: 5, lock: false });
            assert_eq!(before(), Before::Crashed, "{version}");
        }
        for version in ["0.4.8", "0.4.15"] {
            write(&Alive { version: version.into(), inbox: 2, lock: false });
            assert_eq!(before(), Before::Running, "{version} takes no lock: it may be running");
            assert!(alive_file().exists());
        }
        // Even one of those, once its heartbeat has stopped.
        let f = std::fs::File::options().write(true).open(alive_file()).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(60)).unwrap();
        assert_eq!(before(), Before::Crashed);
        // Older still: an empty file, which says nothing, so may be running.
        std::fs::write(alive_file(), "").unwrap();
        assert_eq!(before(), Before::Running);
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
                Item::Agent { name: "reviewer".into(), role: None },
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
