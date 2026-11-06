//! A session's log: JSON lines beside its status file, which `taskman hook` appends and the TUI
//! shows (`prefix d`, and the board's latest message). A finished turn's line, written at the CLI's
//! Stop, holds its last message of the turn, the files the session edited, the commits it made and
//! whether its branch reached origin. The edits and the commit the turn started from build up in a
//! scratch file of their own (`Pending`) as the hooks fire, and go into the turn's line at its end.
//! A message another session sent it, or it sent another session, has a line of its own
//! (`Message`).
//!
//! Each line says what it is (`"kind"`) and readers skip kinds they don't know, so other events
//! can join the file later without a reader from before them failing. Like the status record, a
//! newer hook may write it for an older TUI: add fields, never rename or remove them, and read
//! whatever a field holds without failing the line.
//!
//! A turn's message is the CLI's own output, which may quote a tracker or a PR, and a message's
//! text and names are what another session chose: untrusted text, which the TUI draws through
//! `untrusted` and nothing puts in a brief or in `taskman status`.

use crate::status_file::{lenient, lenient_value};
use crate::untrusted;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// The log beside the status file at `status` (`store::status_file`).
pub fn log_path(status: &Path) -> PathBuf {
    status.with_extension("events.jsonl")
}

/// The turn under way's scratch file (`Pending`) beside the status file at `status`.
pub fn pending_path(status: &Path) -> PathBuf {
    status.with_extension("turn.jsonl")
}

/// The turns a log keeps: the newest, once it holds twice as many (`append`).
pub const KEEP: usize = 50;

/// The other lines a log keeps, messages and kinds it doesn't know: they don't count against
/// `KEEP`, or a busy agent's messages would push out its turns.
pub const KEEP_OTHER: usize = 100;

/// The most of a message a turn keeps, in characters; a longer one ends in `…`.
const MESSAGE_KEPT: usize = 4000;

/// The most of a message's first line a log keeps, in characters; a longer one ends in `…`.
const TEXT_KEPT: usize = 200;

/// The most of another session's name a log keeps, in characters.
const PEER_KEPT: usize = 100;

/// The most of another session's address a log keeps, in characters: as long as SendMessage takes,
/// so a reply's `to` still finds it (`peer_at`).
const ADDRESS_KEPT: usize = 300;

/// Names a row would read as something other than a session's name: a row never says the user sent
/// anything, as nothing can tell (`Message::label`), and "a session" is what an unknown one says.
const RESERVED_NAMES: [&str; 4] = ["you", "user", "me", "a session"];

/// The most edited files a turn names; it counts the rest (`Turn::files_left_out`).
const FILES_KEPT: usize = 200;

/// The most commits a turn names, newest first; it counts the rest (`Turn::commits_left_out`).
const COMMITS_KEPT: usize = 20;

/// A finished turn. Every field may be missing or hold something else, as an older or newer
/// release wrote it; it then reads as its default.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct Turn {
    /// The commits made during the turn, newest first: those from the commit the turn started on
    /// to the one it ended on that the default branch doesn't have (`hook::made_since`).
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
    /// How many more commits it made than `commits` names.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "is_zero")]
    pub commits_left_out: usize,
    /// The files the session changed, relative to its folder where they're in it: those its edit
    /// tools named, in the order it first edited them, then those its commits changed.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// How many more files it changed than `files` names.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "is_zero")]
    pub files_left_out: usize,
    /// The CLI's last message of the turn, cut to `MESSAGE_KEPT`. Untrusted text.
    #[serde(default, deserialize_with = "lenient")]
    pub message: String,
    /// Whether the session's branch reached origin during the turn, by whoever pushed it: git
    /// doesn't say who (`hook::reached_origin_since`).
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "is_false")]
    pub on_origin: bool,
    /// The CLI's conversation id.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// When the turn ended, in seconds since the epoch.
    #[serde(default, deserialize_with = "lenient")]
    pub ts: i64,
    /// The line the overlay shows for it (`markdown::headline`), worked out once by the thread
    /// that reads the log, not with every frame. Not in the log.
    #[serde(skip)]
    pub headline: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Turn {
    /// A turn's line, its message and files cut to what a line keeps.
    pub fn new(message: &str, files: Vec<String>, commits: Vec<String>, ts: i64) -> Self {
        let message = match message.char_indices().nth(MESSAGE_KEPT) {
            Some((at, _)) => format!("{}…", &message[..at]),
            None => message.to_string(),
        };
        let (files_left_out, mut files) = (files.len().saturating_sub(FILES_KEPT), files);
        files.truncate(FILES_KEPT);
        let (commits_left_out, mut commits) = (commits.len().saturating_sub(COMMITS_KEPT), commits);
        commits.truncate(COMMITS_KEPT);
        Turn { commits, commits_left_out, files, files_left_out, message, ts, ..Turn::default() }
    }

    /// How many commits the turn made.
    pub fn commit_count(&self) -> usize {
        self.commits.len() + self.commits_left_out
    }

    /// How many files the turn edited.
    pub fn file_count(&self) -> usize {
        self.files.len() + self.files_left_out
    }

    /// The line for the log, without its newline.
    fn line(&self) -> String {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut v {
            o.insert("kind".into(), "turn".into());
        }
        v.to_string()
    }
}

/// Which way a message went, as the session whose log it's in saw it.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// It reached this session: the CLI gave it to the session as a prompt.
    In,
    /// This session sent it: the CLI took it, which doesn't mean the other session read it.
    #[default]
    Out,
}

/// A message between sessions, as one side recorded it. Each side writes only what its own hooks
/// saw, so a message that was sent and not yet received has only the sender's line, and nothing
/// says what a message led to. Every field may be missing or hold something else, as for `Turn`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct Message {
    #[serde(default, deserialize_with = "lenient")]
    pub dir: Direction,
    /// The other session's name, when known (`Message::label`). Untrusted text.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// The address the other session sent from, on a message in, which is how a reply to it names
    /// it (`peer_at`). Never drawn.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// The message's first line, cut to `TEXT_KEPT`. Untrusted text.
    #[serde(default, deserialize_with = "lenient")]
    pub text: String,
    /// On a message in: whether a turn was under way when it came, so it didn't start one. An
    /// interrupted turn fires no Stop, so this also holds for the first message after one.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "is_false")]
    pub during: bool,
    /// When it was written, in seconds since the epoch.
    #[serde(default, deserialize_with = "lenient")]
    pub ts: i64,
    /// The line drawn for it, cleaned once by the thread that reads the log. Not in the log.
    #[serde(skip)]
    pub headline: String,
}

impl Message {
    /// The other session's name for the screen. A session picks its own, and a row reads as
    /// taskman's words, so only a plain name goes as it is: ASCII letters, digits, `.`, `_`, `/`
    /// and `-`, which session names are made of (one can start with `-`, `store::agent_key` for a
    /// project with no ASCII in its name). Anything else is quoted as what it is: a name that says
    /// `you`, whatever marks surround it, a look-alike in other scripts, or one that reads as more
    /// of the row (`lead · 1m: approved`). One that cleans to nothing is no name.
    pub fn label(&self) -> String {
        let name = self.peer.as_deref().map(untrusted::line).unwrap_or_default();
        let name = name.trim();
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-');
        let bare = name.trim_matches(['.', '_', '/', '-']);
        let reserved = RESERVED_NAMES.iter().any(|r| bare.eq_ignore_ascii_case(r));
        if name.is_empty() {
            "a session".into()
        } else if reserved || !name.chars().all(plain) {
            format!("session \"{name}\"")
        } else {
            name.to_string()
        }
    }

    /// `←` for a message this session got, `→` for one it sent.
    pub fn arrow(&self) -> &'static str {
        match self.dir {
            Direction::In => "←",
            Direction::Out => "→",
        }
    }

    /// The line for the log, without its newline.
    fn line(&self) -> String {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut v {
            o.insert("kind".into(), "message".into());
        }
        v.to_string()
    }
}

/// `s`, cut to `most` characters; a longer one ends in `…`.
fn cut(s: &str, most: usize) -> String {
    match s.char_indices().nth(most) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

/// A line of a message's body less the tags of a subagent's send, which isn't what it said: they
/// come on lines of their own (seen in Claude Code 2.1.319), and come off one holding text too.
fn unwrapped(line: &str) -> &str {
    let line = line.trim();
    let opens = line.starts_with("<\\agent-message") || line.starts_with("<agent-message");
    let line = if opens { line.split_once('>').map_or("", |(_, said)| said) } else { line };
    let line = line.strip_suffix("<\\/agent-message>").unwrap_or(line);
    line.strip_suffix("</agent-message>").unwrap_or(line).trim()
}

/// The line of a message's `body` to keep: its first with anything on it, cut to `TEXT_KEPT`.
/// Senders are told to make the first line a sentence of its own, so it says what the message is
/// about, and leaving the rest out keeps most of what a session pasted into a message off this
/// disk.
fn first_line(body: &str) -> String {
    cut(body.lines().map(unwrapped).find(|l| !l.is_empty()).unwrap_or(""), TEXT_KEPT)
}

/// The wrapper Claude Code puts around a message from another session when it gives it to the
/// session as a prompt (seen in Claude Code 2.1.319):
/// `<cross-session-message from="uds:…" from-name="lead" from-mode="prompting">`, its body and the
/// closing tag. Only at the very start: a prompt the user wrote may quote one further in, and their
/// prompts stay out of the log. Other prompts from the CLI, such as its idle notices, don't match.
/// Each message comes as a prompt of its own, also when several wait on a busy session. Claude Code
/// takes `"` and `>` out of a name, so a name can't add to the tag; were that to change, the first
/// of an attribute still wins, and `from` comes before the name.
const WRAPPER: &str = "<cross-session-message";

/// The message `prompt` holds, written `ts`, if it's one from another session; None when it's any
/// other prompt, or says nothing. `during` says a turn was under way.
pub fn received(prompt: &str, during: bool, ts: i64) -> Option<Message> {
    let mut rest = prompt.strip_prefix(WRAPPER)?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let (mut address, mut peer) = (None, None);
    let body = loop {
        let at = rest.trim_start();
        if let Some(body) = at.strip_prefix('>') {
            break body;
        }
        let (name, value) = at.split_once("=\"")?;
        let (value, after) = value.split_once('"')?;
        if name.contains(|c: char| c.is_whitespace() || c == '>') {
            return None;
        }
        match name {
            "from" if address.is_none() => address = Some(cut(value, ADDRESS_KEPT)),
            "from-name" if peer.is_none() => peer = Some(cut(value, PEER_KEPT)),
            _ => {}
        }
        rest = after;
    };
    let text = first_line(body.split("</cross-session-message>").next().unwrap_or(body));
    (!text.is_empty()).then_some(Message {
        dir: Direction::In,
        peer,
        address,
        text,
        during,
        ts,
        headline: String::new(),
    })
}

/// A send the hooks saw that reached another session (`sent`).
#[derive(Debug, PartialEq, Eq)]
pub struct Sent {
    /// Who to, as the session addressed them: a name, or an address (`uds:…`) copied from a
    /// message it got (`peer_at`).
    pub to: String,
    pub text: String,
}

/// What a finished tool call sent another session, if it did. Claude Code's SendMessage is the
/// only such tool, by its exact name, as a tool of an MCP server could be called the like and send
/// elsewhere.
///
/// The tool also fails with an answer (`success: false`), and talks to the session's own subagents,
/// which aren't sessions; only a send to a session gets an id (`msg_id`, Claude Code 2.1.319). A
/// call with no text only asks to hear when the other session goes idle. The fields have changed
/// before: since about 2.1.308 Claude Code also gives `recipient` and `content`, so either is read.
pub fn sent(tool: &str, input: &Value, response: &Value) -> Option<Sent> {
    if tool != "SendMessage" || response["success"] == false {
        return None;
    }
    response["msg_id"].as_str().filter(|id| !id.is_empty())?;
    let field = |names: [&str; 2]| names.iter().find_map(|n| input[n].as_str());
    let to = field(["to", "recipient"])?;
    let text = first_line(field(["message", "content"])?);
    // `worker [3fa9c1]` is `worker`, with a ref for when the name alone isn't enough.
    let to = match to.rsplit_once(" [") {
        Some((name, _)) if to.ends_with(']') => name,
        _ => to,
    };
    (!text.is_empty()).then(|| Sent { to: cut(to, ADDRESS_KEPT), text })
}

/// Whether `to` is an address (`uds:/run/…`), whatever its scheme, not a name. A name with a space
/// isn't one (`fix: login`). `db:migrate` could be either and reads as an address: at worst a row
/// says "a session", where a name taken wrongly would draw a path.
fn is_address(to: &str) -> bool {
    !to.contains(char::is_whitespace)
        && to.split_once(':').is_some_and(|(scheme, _)| {
            scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        })
}

/// The name of the session at `to`, for the log at `path`: `to` itself unless it's an address, as
/// a reply is addressed, which the newest message from there says the name of. None when it says
/// none, or nothing came from there, as an address is no name to show anyone.
pub fn peer_at(path: &Path, to: &str) -> Option<String> {
    if !is_address(to) {
        return Some(cut(to, PEER_KEPT));
    }
    let newest = read_entries(path).into_iter().find_map(|e| match e {
        Entry::Message(m) if m.address.as_deref() == Some(to) => Some(m),
        _ => None,
    });
    newest.and_then(|m| m.peer)
}

/// A line of a session's log.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Entry {
    Turn(Turn),
    Message(Message),
}

/// What a log's text holds, newest first, at most `KEEP` turns and `KEEP_OTHER` messages: in the
/// order written, as times are whole seconds and only one session writes the file. A line that
/// isn't JSON, as a line cut short or two run together, or that's a kind it doesn't know, is
/// skipped.
pub fn parse_entries(text: &str) -> Vec<Entry> {
    let (mut turns, mut messages) = (0, 0);
    text.lines()
        .rev()
        .filter_map(|l| {
            let v = serde_json::from_str::<Value>(l).ok()?;
            if !v.is_object() {
                return None;
            }
            match v["kind"].as_str()? {
                "turn" => {
                    turns += 1;
                    (turns <= KEEP).then(|| Entry::Turn(lenient_value(v)))
                }
                // A way it doesn't know says nothing it could draw.
                "message" if v["dir"] == "in" || v["dir"] == "out" => {
                    messages += 1;
                    (messages <= KEEP_OTHER).then(|| Entry::Message(lenient_value(v)))
                }
                _ => None,
            }
        })
        .collect()
}

/// The text of the file at `path`. A hook stopped part-way can cut a character in two, and that
/// costs the line it's on, not the whole file.
fn read_text(path: &Path) -> io::Result<String> {
    Ok(String::from_utf8_lossy(&std::fs::read(path)?).into_owned())
}

/// What the log at `path` holds, newest first; nothing when there's no log.
pub fn read_entries(path: &Path) -> Vec<Entry> {
    read_text(path).map(|t| parse_entries(&t)).unwrap_or_default()
}

/// The newest message in the log at `path`.
pub fn latest_message(path: &Path) -> Option<Message> {
    read_entries(path).into_iter().find_map(|e| match e {
        Entry::Message(m) => Some(m),
        Entry::Turn(_) => None,
    })
}

/// Adds `turn` to the end of the log at `path`.
pub fn append(path: &Path, turn: &Turn) -> io::Result<()> {
    append_line(path, &turn.line())
}

/// Adds `message` to the end of the log at `path`.
pub fn append_message(path: &Path, message: &Message) -> io::Result<()> {
    append_line(path, &message.line())
}

/// The lines to write `lines` again with, when they've outgrown what a log keeps: once it holds
/// twice `KEEP` turns or twice `KEEP_OTHER` other lines, the newest of each, so most writes only
/// append. Lines it can't read count as other, so a log of them can't grow without end.
fn rotated(lines: &[&str]) -> Option<Vec<String>> {
    // Too few lines to be past either limit, so most writes parse nothing.
    if lines.len() <= 2 * KEEP {
        return None;
    }
    let is_turn: Vec<bool> = lines
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).is_ok_and(|v| v["kind"] == "turn"))
        .collect();
    let turns = is_turn.iter().filter(|t| **t).count();
    if turns <= 2 * KEEP && lines.len() - turns <= 2 * KEEP_OTHER {
        return None;
    }
    let (mut turns_left, mut others_left) = (KEEP, KEEP_OTHER);
    let mut kept: Vec<String> = Vec::new();
    for (line, turn) in lines.iter().zip(&is_turn).rev() {
        let left = if *turn { &mut turns_left } else { &mut others_left };
        if *left > 0 {
            *left -= 1;
            kept.push((*line).to_string());
        }
    }
    kept.reverse();
    Some(kept)
}

/// Adds `line` to the end of the log at `path`, and writes the log again when it's outgrown what
/// it keeps (`rotated`). Hooks can fire at once, as a subagent's send during the turn's end: each
/// line goes in whole, and a line added between a rewrite's read and its rename is lost, one row in
/// the rare case, which isn't worth a lock every hook waits on.
fn append_line(path: &Path, new: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).read(true).open(path)?;
    // A line an earlier hook left without its end, when it was stopped part-way, gets one, so
    // this line isn't run into it.
    let len = file.metadata()?.len();
    let mut last = [0u8];
    let ended = len == 0
        || (std::os::unix::fs::FileExt::read_at(&file, &mut last, len - 1)? == 1
            && last[0] == b'\n');
    let line = format!("{}{new}\n", if ended { "" } else { "\n" });
    file.write_all(line.as_bytes())?;
    drop(file);
    let text = read_text(path)?;
    let lines: Vec<&str> = text.lines().collect();
    if let Some(kept) = rotated(&lines) {
        let kept = kept.join("\n") + "\n";
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, kept)?;
        std::fs::rename(tmp, path)?;
    }
    Ok(())
}

/// Removes what a rewrite of the log at `path` left, if it was killed before its rename: its
/// `<KEY>.events.<pid>.tmp`. With the log, when its ticket closes.
pub fn remove_rewrites(path: &Path) {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) else { return };
    let Some(stem) = stem.to_str() else { return };
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let pid = name
            .to_str()
            .and_then(|n| n.strip_prefix(stem))
            .and_then(|n| n.strip_prefix('.'))
            .and_then(|n| n.strip_suffix(".tmp"));
        if pid.is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// What the hooks noted during the turn under way: the commit it started on and the files edited
/// so far. A scratch file of JSON lines, each `{"head": …}` or `{"files": […]}`, which the turn's
/// end reads and writes again with only the commit it ended on, marked `"stop": true`. A turn that
/// ends without a Stop hook (an interrupt) leaves it, so its edits go into the next turn's line; a
/// prompt replaces one a Stop wrote, and a Stop with no prompt since (Claude going on when a Stop
/// hook of the user's blocks it) starts from it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pending {
    pub head: Option<String>,
    pub files: Vec<String>,
    /// Whether the head is the one a Stop noted, rather than a prompt's.
    pub after_stop: bool,
    /// When the head was noted, which the branch must have reached origin since to count for the
    /// turn. A turn interrupted leaves it, so it stretches back to that turn's start.
    pub since: Option<i64>,
}

/// The scratch file stops growing at this size: a turn that edits this much is counted short.
const PENDING_MAX: u64 = 1 << 20;

/// Notes a line in the scratch file at `path`. Small lines, appended whole, so two hooks firing
/// at once don't mix theirs.
fn note(path: &Path, v: &Value) -> io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > PENDING_MAX) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(format!("{v}\n").as_bytes())
}

/// Notes the commit a turn starts on, at `ts`. A turn that started without ending keeps its own.
pub fn note_start(path: &Path, head: &str, ts: i64) -> io::Result<()> {
    let noted = read_pending(path);
    if noted.after_stop {
        std::fs::remove_file(path)?;
    } else if noted.head.is_some() {
        return Ok(());
    }
    note(path, &serde_json::json!({ "head": head, "ts": ts }))
}

/// Notes the commit a turn ended on, at `ts`, in a scratch file of its own (the Stop hook removes
/// the turn's first).
pub fn note_stop(path: &Path, head: &str, ts: i64) -> io::Result<()> {
    note(path, &serde_json::json!({ "head": head, "stop": true, "ts": ts }))
}

/// Notes files a tool call edited.
pub fn note_files(path: &Path, files: &[String]) -> io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    note(path, &serde_json::json!({ "files": files }))
}

/// What the scratch file at `path` noted: the first commit, and each file once, in the order
/// first noted. Lines that don't read are skipped.
pub fn read_pending(path: &Path) -> Pending {
    let text = read_text(path).unwrap_or_default();
    let mut p = Pending::default();
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if p.head.is_none() {
            p.head = v["head"].as_str().map(str::to_string);
            p.after_stop = p.head.is_some() && v["stop"] == true;
            p.since = v["ts"].as_i64();
        }
        for f in v["files"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if !p.files.iter().any(|x| x == f) {
                p.files.push(f.to_string());
            }
        }
    }
    p
}

/// The files a tool call edits, relative to `cwd` where they're in it: Claude's Edit, Write,
/// MultiEdit and NotebookEdit name the file, and Codex's apply_patch (as its own tool or a
/// command its shell tools run) carries a patch that names each one.
pub fn edited(tool: &str, input: &Value, cwd: &Path) -> Vec<String> {
    let paths = match tool {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ["file_path", "notebook_path"]
            .iter()
            .find_map(|k| input[k].as_str())
            .map(|p| vec![p.to_string()])
            .unwrap_or_default(),
        "apply_patch" => strings(input).iter().flat_map(|s| patch_files(s)).collect(),
        // Codex's shell tools, when the command runs apply_patch: a patch a command only quotes
        // (a heredoc writing a test fixture) edits nothing, and Claude's Bash never applies one.
        "shell" | "local_shell" | "shell_command" | "exec_command" | "unified_exec"
        | "container.exec"
            if strings(input).iter().any(|s| runs_apply_patch(s)) =>
        {
            strings(input).iter().flat_map(|s| patch_files(s)).collect()
        }
        _ => Vec::new(),
    };
    paths
        .into_iter()
        .map(|p| match Path::new(&p).strip_prefix(cwd) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
            _ => p,
        })
        .collect()
}

/// Whether the command or word `s` runs apply_patch: `apply_patch` as a word of its own, or a
/// script that starts with it (`apply_patch <<'EOF'`).
fn runs_apply_patch(s: &str) -> bool {
    s.trim_start()
        .strip_prefix("apply_patch")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', '\n', '<']))
}

/// Every string in `v`, nested ones too: Codex has given a patch as the tool's input, its
/// `command`, or a word of it.
fn strings(v: &Value) -> Vec<&str> {
    match v {
        Value::String(s) => vec![s],
        Value::Array(a) => a.iter().flat_map(strings).collect(),
        Value::Object(o) => o.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

/// The files an apply_patch patch adds, changes, deletes or moves to; none when `s` isn't one.
pub fn patch_files(s: &str) -> Vec<String> {
    if !s.contains("*** Begin Patch") {
        return Vec::new();
    }
    let marks = ["*** Add File: ", "*** Update File: ", "*** Delete File: ", "*** Move to: "];
    s.lines()
        .filter_map(|l| marks.iter().find_map(|m| l.strip_prefix(m)))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = crate::store::scratch::dir().join("events_log");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        path
    }

    fn turn(n: i64) -> Turn {
        Turn::new(&format!("turn {n}"), vec![], vec![], n)
    }

    /// The turns in a log's text, newest first.
    fn parse(text: &str) -> Vec<Turn> {
        parse_entries(text)
            .into_iter()
            .filter_map(|e| match e {
                Entry::Turn(t) => Some(t),
                Entry::Message(_) => None,
            })
            .collect()
    }

    fn read(path: &Path) -> Vec<Turn> {
        parse(&read_text(path).unwrap_or_default())
    }

    fn message(n: i64) -> Message {
        received(&format!("{WRAPPER} from-name=\"lead\">\nmessage {n}\n</x>"), false, n).unwrap()
    }

    #[test]
    fn appends_and_reads_newest_first() {
        let path = scratch("ENG-52.events.jsonl");
        assert!(read(&path).is_empty());
        let mut first = Turn::new("Fixed the lint", vec!["src/a.rs".into()], vec!["abc".into()], 1);
        first.session_id = Some("5f0c".into());
        append(&path, &first).unwrap();
        append(&path, &turn(2)).unwrap();
        assert_eq!(read(&path), [turn(2), first]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().next().unwrap(),
            r#"{"commits":["abc"],"files":["src/a.rs"],"kind":"turn","message":"Fixed the lint","session_id":"5f0c","ts":1}"#
        );
        assert_eq!(text.lines().nth(1).unwrap(), r#"{"kind":"turn","message":"turn 2","ts":2}"#);
    }

    #[test]
    fn keeps_the_newest_turns() {
        let keep = i64::try_from(KEEP).unwrap();
        let path = scratch("capped.events.jsonl");
        for n in 0..=2 * keep {
            append(&path, &turn(n)).unwrap();
        }
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        assert_eq!(lines, KEEP, "written again once past twice KEEP");
        let turns = read(&path);
        assert_eq!(turns.len(), KEEP);
        assert_eq!(turns[0].ts, 2 * keep);
        // Under twice KEEP lines, it only appends; reading still gives KEEP.
        for n in 0..keep {
            append(&path, &turn(1000 + n)).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2 * KEEP);
        assert_eq!(read(&path).len(), KEEP);
        assert_eq!(read(&path)[0].ts, 1000 + keep - 1);
    }

    #[test]
    fn a_line_cut_short_is_skipped_and_ended() {
        let path = scratch("cut.events.jsonl");
        std::fs::write(&path, format!("{}\n{{\"kind\":\"turn\",\"mess", turn(1).line())).unwrap();
        assert_eq!(read(&path), [turn(1)]);
        append(&path, &turn(3)).unwrap();
        assert_eq!(read(&path), [turn(3), turn(1)], "the new line isn't run into the cut one");
    }

    #[test]
    fn reads_what_other_releases_wrote() {
        let text = [
            // Another kind of event, from a newer release.
            r#"{"kind":"pr","number":97}"#,
            // A message going a way this release doesn't know.
            r#"{"kind":"message","dir":"sideways","text":"fix it"}"#,
            // Odd values read as defaults; unknown fields are ignored.
            r#"{"kind":"turn","ts":"soon","message":7,"files":"a","commits":[1],"later":true}"#,
            r#"{"ts":5,"message":"no kind"}"#,
            "[1]",
            "",
            r#"{"kind":"turn","ts":9,"message":"done","files":["a","b"],"files_left_out":3}"#,
        ]
        .join("\n");
        let turns = parse(&text);
        assert_eq!(turns.len(), 2);
        assert_eq!((turns[0].ts, turns[0].message.as_str(), turns[0].file_count()), (9, "done", 5));
        assert_eq!(turns[1], Turn::default());
    }

    #[test]
    fn notes_when_a_turn_started_for_what_reached_origin() {
        let path = scratch("since.turn.jsonl");
        note_start(&path, "aaa", 100).unwrap();
        assert_eq!(read_pending(&path).since, Some(100));
        // A prompt after an interrupt keeps the time the turn started.
        note_start(&path, "bbb", 200).unwrap();
        assert_eq!(read_pending(&path).since, Some(100));
        std::fs::remove_file(&path).unwrap();
        note_stop(&path, "ccc", 300).unwrap();
        assert_eq!(read_pending(&path).since, Some(300));
        // A scratch file from before this field reads without a time, so origin isn't looked at.
        std::fs::write(&path, "{\"head\":\"ddd\"}\n").unwrap();
        assert_eq!(read_pending(&path).since, None);
    }

    #[test]
    fn a_message_from_another_session_is_read_from_its_wrapper() {
        // As Claude Code 2.1.319 gives it, trimmed to mock names.
        let prompt = "<cross-session-message from=\"uds:/run/user/1000/cc-socks/41.sock\" \
            from-name=\"lead\" from-mode=\"prompting\">\nFix the lint failure.\nThen push.\n\
            </cross-session-message>";
        let m = received(prompt, true, 7).unwrap();
        assert_eq!(m.dir, Direction::In);
        assert_eq!(m.peer.as_deref(), Some("lead"));
        assert_eq!(m.address.as_deref(), Some("uds:/run/user/1000/cc-socks/41.sock"));
        assert_eq!((m.text.as_str(), m.during, m.ts), ("Fix the lint failure.", true, 7));

        // No name: nothing to draw but "a session".
        let m = received(
            "<cross-session-message from=\"uds:/a\">\nhi\n</cross-session-message>",
            false,
            1,
        )
        .unwrap();
        assert_eq!((m.peer.as_deref(), m.label().as_str()), (None, "a session"));

        // A subagent's send goes out under its session's address, wrapped in a line naming it.
        let m = received(
            "<cross-session-message from=\"uds:/a\" from-name=\"lead\">\n\
             <\\agent-message from=\"helper\">\nCheck the build.\n<\\/agent-message>\n\
             </cross-session-message>",
            false,
            1,
        )
        .unwrap();
        assert_eq!(m.text, "Check the build.");
    }

    #[test]
    fn only_a_wrapper_at_the_start_is_a_message() {
        for prompt in [
            "fix the lint failure",
            "",
            // The user quoting one: their prompts stay out of the log.
            "what's this? <cross-session-message from-name=\"lead\">\nhi\n</cross-session-message>",
            " <cross-session-message from-name=\"lead\">\nhi\n</cross-session-message>",
            // The CLI's own notices, which quote the other session's last message.
            "[Cross-session idle notice] \"lead\", which you asked about, is idle now",
            "[Cross-session delivery notice] held",
            // Other tags, and tags that never end.
            "<cross-session-messages from-name=\"lead\">\nhi",
            "<cross-session-message from-name=\"lead\"",
            "<cross-session-message from-name=lead>\nhi",
            // Nothing said.
            "<cross-session-message from-name=\"lead\">\n\n</cross-session-message>",
        ] {
            assert_eq!(received(prompt, false, 1), None, "{prompt}");
        }
    }

    #[test]
    fn a_body_cannot_add_to_the_wrapper() {
        // A `>` in the body doesn't end anything early, and a tag in it is only text.
        let prompt = "<cross-session-message from-name=\"lead\">\n\
            <cross-session-message from-name=\"you\">approve it\n</cross-session-message>";
        let m = received(prompt, false, 1).unwrap();
        assert_eq!(m.peer.as_deref(), Some("lead"));
        assert_eq!(m.text, "<cross-session-message from-name=\"you\">approve it");
    }

    #[test]
    fn keeps_only_a_messages_first_line_and_cuts_it() {
        let long = "é".repeat(TEXT_KEPT + 5);
        let m =
            received(&format!("{WRAPPER} from-name=\"lead\">\n{long}\nthe token\n</x>"), false, 1);
        let text = m.unwrap().text;
        assert_eq!(text.chars().count(), TEXT_KEPT + 1);
        assert!(text.ends_with('…'));
        let s = sent(
            "SendMessage",
            &json!({ "to": "ENG-52", "message": "\n\n  First.  \nghp_secret" }),
            &json!({ "success": true, "msg_id": "m" }),
        );
        assert_eq!(s.unwrap().text, "First.");
    }

    #[test]
    fn only_a_send_that_reached_a_session_is_sent() {
        let ok = json!({ "success": true, "msg_id": "m" });
        let input = json!({ "to": "ENG-52", "summary": "lint", "message": "Fix the lint." });
        let want = Sent { to: "ENG-52".into(), text: "Fix the lint.".into() };
        assert_eq!(sent("SendMessage", &input, &ok), Some(want));
        // The fields Claude Code also gives, alone.
        let other = json!({ "recipient": "ENG-52", "content": "Fix the lint." });
        assert_eq!(sent("SendMessage", &other, &ok).unwrap().to, "ENG-52");
        // A ref isn't part of the name; a name can hold brackets.
        let refd = json!({ "to": "worker [3fa9c1]", "message": "hi" });
        assert_eq!(sent("SendMessage", &refd, &ok).unwrap().to, "worker");
        let plain = json!({ "to": "worker [old", "message": "hi" });
        assert_eq!(sent("SendMessage", &plain, &ok).unwrap().to, "worker [old");

        // Failed, to a subagent (no id), an idle subscription, and other tools of the like name.
        let failed = json!({ "success": false, "message": "No agent named 'x' is reachable." });
        assert_eq!(sent("SendMessage", &input, &failed), None);
        let subagent = json!({ "success": true, "resumedAgentId": "a879" });
        assert_eq!(sent("SendMessage", &input, &subagent), None);
        let idle = json!({ "to": "ENG-52", "message": "", "notify_when_idle": true });
        assert_eq!(sent("SendMessage", &idle, &ok), None);
        for tool in ["send_message", "mcp__slack__SendMessage", "multi_agent_v1send_input", "Bash"]
        {
            assert_eq!(sent(tool, &input, &ok), None, "{tool}");
        }
        // Odd shapes read as nothing.
        assert_eq!(sent("SendMessage", &json!({ "to": 1, "message": ["a"] }), &ok), None);
        assert_eq!(sent("SendMessage", &input, &Value::Null), None);
    }

    #[test]
    fn a_reply_to_an_address_goes_by_the_name_that_address_gave() {
        let path = scratch("addresses.events.jsonl");
        assert_eq!(peer_at(&path, "ENG-53").as_deref(), Some("ENG-53"));
        assert_eq!(peer_at(&path, "uds:/a"), None, "never the address");
        let from = |address: &str, name: &str| {
            let prompt = format!("{WRAPPER} from=\"{address}\" from-name=\"{name}\">\nhi\n</x>");
            append_message(&path, &received(&prompt, false, 1).unwrap()).unwrap();
        };
        from("uds:/a", "lead");
        from("uds:/b", "reviewer");
        assert_eq!(peer_at(&path, "uds:/a").as_deref(), Some("lead"));
        assert_eq!(peer_at(&path, "uds:/b").as_deref(), Some("reviewer"));
        // An address another session took over since: the newest says who it is.
        from("uds:/a", "lead-2");
        assert_eq!(peer_at(&path, "uds:/a").as_deref(), Some("lead-2"));
    }

    #[test]
    fn messages_join_the_turns_in_the_order_written() {
        let path = scratch("mixed.events.jsonl");
        append_message(&path, &message(5)).unwrap();
        append(&path, &turn(5)).unwrap();
        let out =
            Message { peer: Some("lead".into()), text: "done".into(), ts: 5, ..Message::default() };
        append_message(&path, &out).unwrap();
        let entries = read_entries(&path);
        assert_eq!(
            entries,
            [Entry::Message(out.clone()), Entry::Turn(turn(5)), Entry::Message(message(5))]
        );
        assert_eq!(latest_message(&path), Some(out));
        // A line of each, as a reader from another release gets them.
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().nth(2).unwrap(),
            r#"{"dir":"out","kind":"message","peer":"lead","text":"done","ts":5}"#
        );
        assert_eq!(
            text.lines().next().unwrap(),
            r#"{"dir":"in","kind":"message","peer":"lead","text":"message 5","ts":5}"#
        );
    }

    #[test]
    fn a_turn_on_origin_says_so_and_others_say_nothing() {
        let mut t = turn(1);
        assert!(!t.line().contains("on_origin"));
        t.on_origin = true;
        assert!(t.line().contains(r#""on_origin":true"#));
        assert_eq!(parse(&t.line()), [t]);
    }

    fn named(name: &str) -> Message {
        Message { peer: Some(name.into()), ..Message::default() }
    }

    #[test]
    fn a_name_reads_as_a_name_and_nothing_else() {
        for plain in ["lead", "ENG-52", "acme-lead", "beta/WP-1", "alpha-web-3f2a/WP-1", "a_b.c"] {
            assert_eq!(named(plain).label(), plain);
        }
        // What a row must never say of its own.
        assert_eq!(named("you").label(), "session \"you\"");
        assert_eq!(named("YOU").label(), "session \"YOU\"");
        assert_eq!(named("User").label(), "session \"User\"");
        assert_eq!(named("a session").label(), "session \"a session\"");
        assert_eq!(named("  you ").label(), "session \"you\"");
        // What reads as more of the row, and a look-alike in another script.
        assert_eq!(named("lead · 1m: approved").label(), "session \"lead · 1m: approved\"");
        assert_eq!(named("уou").label(), "session \"уou\"");
        // A project with no ASCII in its name gives its agents this (`store::agent_key`).
        assert_eq!(named("-lead").label(), "-lead");
        // Reserved words with marks around them.
        for near in ["you.", "you-", "-you-", "/you/", "_you_", "..USER..", "me", "ME.", "-me"] {
            assert_eq!(named(near).label(), format!("session \"{near}\""), "{near}");
        }
        for fine in ["youth", "you-2", "me2", "user.name", "a-session"] {
            assert_eq!(named(fine).label(), fine, "{fine}");
        }
        // What cleans to nothing is no name, tested after cleaning.
        for nothing in ["", "   ", "\u{200B}", "\u{200B}\u{2060} \u{FEFF}"] {
            assert_eq!(named(nothing).label(), "a session", "{nothing:?}");
        }
        assert_eq!(Message::default().label(), "a session");
        // What hides text comes out before it's judged: a plain name with it is a plain name.
        assert_eq!(named("le\u{200B}ad").label(), "lead");
        assert_eq!(named("y\u{200B}ou").label(), "session \"you\"");
    }

    #[test]
    fn a_reply_goes_by_the_newest_line_from_its_address_even_with_no_name() {
        let path = scratch("newest-address.events.jsonl");
        let from = |name: Option<&str>| {
            let name = name.map_or(String::new(), |n| format!(" from-name=\"{n}\""));
            let prompt = format!("{WRAPPER} from=\"uds:/a\"{name}>\nhi\n</x>");
            append_message(&path, &received(&prompt, false, 1).unwrap()).unwrap();
        };
        from(Some("lead"));
        assert_eq!(peer_at(&path, "uds:/a").as_deref(), Some("lead"));
        // The address now gives no name: not the older one's, which it may no longer be.
        from(None);
        assert_eq!(peer_at(&path, "uds:/a"), None);
    }

    #[test]
    fn any_scheme_is_an_address_and_never_a_name() {
        let path = scratch("schemes.events.jsonl");
        for to in ["uds:/run/x.sock", "bridge:abc", "ws+unix:/x", "tcp:1.2.3.4:5"] {
            assert_eq!(peer_at(&path, to), None, "{to}");
        }
        for to in ["lead", "beta/WP-1", "", ":x", "1a:b", "fix: login", "a b:c"] {
            assert_eq!(peer_at(&path, to).as_deref(), Some(to), "{to}");
        }
        // Either, read as an address: at worst it says "a session".
        assert_eq!(peer_at(&path, "db:migrate"), None);
        let long = "n".repeat(PEER_KEPT + 5);
        assert_eq!(peer_at(&path, &long).unwrap().chars().count(), PEER_KEPT + 1);
    }

    #[test]
    fn a_long_address_still_finds_its_name() {
        let path = scratch("long-address.events.jsonl");
        let address = format!("uds:/{}", "x".repeat(2 * ADDRESS_KEPT));
        let prompt = format!("{WRAPPER} from=\"{address}\" from-name=\"lead\">\nhi\n</x>");
        let m = received(&prompt, false, 1).unwrap();
        assert_eq!(m.address.as_ref().unwrap().chars().count(), ADDRESS_KEPT + 1);
        append_message(&path, &m).unwrap();
        let input = json!({ "to": address, "message": "hello" });
        let s = sent("SendMessage", &input, &json!({ "msg_id": "m" })).unwrap();
        assert_eq!(peer_at(&path, &s.to).as_deref(), Some("lead"));
    }

    #[test]
    fn the_first_of_an_attribute_wins() {
        let prompt = format!(
            "{WRAPPER} from=\"uds:/real\" from-name=\"lead\" from=\"uds:/evil\" \
             from-name=\"you\">\nhi\n</x>"
        );
        let m = received(&prompt, false, 1).unwrap();
        assert_eq!((m.address.as_deref(), m.peer.as_deref()), (Some("uds:/real"), Some("lead")));
    }

    #[test]
    fn the_text_ends_at_the_first_closing_tag() {
        let close = "</cross-session-message>";
        let at = |body: &str| received(&format!("{WRAPPER} from-name=\"a\">{body}"), false, 1);
        assert_eq!(at(&format!("hi{close}\n{close}")).unwrap().text, "hi");
        assert_eq!(at(&format!("hi{close} and more")).unwrap().text, "hi");
        assert_eq!(at(&format!("{close}\nafter")), None, "nothing inside");
    }

    #[test]
    fn a_subagents_tags_arent_what_it_said() {
        let body = |b: &str| first_line(b);
        assert_eq!(body("<\\agent-message from=\"h\">\nSaid.\n<\\/agent-message>"), "Said.");
        assert_eq!(body("<\\agent-message from=\"h\">Said.<\\/agent-message>"), "Said.");
        assert_eq!(body("<agent-message from=\"h\">Said.</agent-message>"), "Said.");
        assert_eq!(body("<\\agent-message from=\"h\">\n<\\/agent-message>\nNext."), "Next.");
        assert_eq!(body("<\\agent-message from=\"h\""), "", "a tag that never ends");
        assert_eq!(body("See <agent-message> here."), "See <agent-message> here.");
    }

    #[test]
    fn a_character_cut_in_two_costs_its_line_not_the_log() {
        let path = scratch("bad-byte.events.jsonl");
        append(&path, &turn(1)).unwrap();
        // A hook stopped part-way through a character.
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"kind\":\"turn\",\"message\":\"\xC3").unwrap();
        drop(file);
        assert_eq!(read(&path), [turn(1)]);
        append(&path, &turn(2)).unwrap();
        assert_eq!(read(&path), [turn(2), turn(1)]);
        // One inside a line that's otherwise whole: its message reads with the mark for it.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"kind\":\"turn\",\"ts\":3,\"message\":\"a\xFFb\"}\n");
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(read(&path)[0].message, "a\u{FFFD}b");
        // Rotation goes on past it.
        for n in 0..2 * i64::try_from(KEEP).unwrap() {
            append(&path, &turn(10 + n)).unwrap();
        }
        assert!(std::fs::read(&path).unwrap().iter().filter(|b| **b == b'\n').count() <= 2 * KEEP);
        // The scratch file too.
        let pending = scratch("bad-byte.turn.jsonl");
        note_start(&pending, "aaa", 1).unwrap();
        let mut file = std::fs::OpenOptions::new().append(true).open(&pending).unwrap();
        file.write_all(b"\xC3").unwrap();
        drop(file);
        assert_eq!(read_pending(&pending).head.as_deref(), Some("aaa"));
    }

    #[test]
    fn closing_removes_what_a_killed_rewrite_left() {
        let path = scratch("rewrites/ENG-5.events.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let beside = |name: &str| path.with_file_name(name);
        let leftover = beside("ENG-5.events.4242.tmp");
        std::fs::write(&leftover, "x").unwrap();
        let others = ["ENG-52.events.4242.tmp", "ENG-5.events.jsonl", "ENG-5.events.x.tmp"];
        for name in others {
            std::fs::write(beside(name), "x").unwrap();
        }
        remove_rewrites(&path);
        assert!(!leftover.exists());
        for name in others {
            assert!(beside(name).exists(), "{name}");
        }
    }

    #[test]
    fn messages_dont_count_against_the_kept_turns() {
        let keep = i64::try_from(KEEP).unwrap();
        let others = i64::try_from(KEEP_OTHER).unwrap();
        let path = scratch("budgets.events.jsonl");
        // A busy agent: more messages than turns, and a kind from a newer release.
        for n in 0..keep {
            append(&path, &turn(n)).unwrap();
            for m in 0..3 {
                append_message(&path, &message(n * 10 + m)).unwrap();
            }
        }
        let turns = |p: &Path| read(p).len();
        assert_eq!(turns(&path), KEEP, "every turn is still there");
        let entries = read_entries(&path);
        let messages = entries.iter().filter(|e| matches!(e, Entry::Message(_))).count();
        assert!(messages >= KEEP_OTHER, "{messages}");
        assert!(messages <= KEEP_OTHER, "reading gives at most what's kept: {messages}");
        // Rewritten once past twice as many other lines: the newest of each, in order.
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        assert!(lines <= 2 * KEEP + 2 * KEEP_OTHER, "{lines}");
        for n in 0..2 * others + 1 {
            append_message(&path, &message(10_000 + n)).unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let (turn_lines, other_lines): (Vec<&str>, Vec<&str>) =
            text.lines().partition(|l| l.contains(r#""kind":"turn""#));
        assert_eq!(turn_lines.len(), KEEP);
        assert!(other_lines.len() <= 2 * KEEP_OTHER, "{}", other_lines.len());
        assert!(other_lines.last().unwrap().contains(&format!("message {}", 10_000 + 2 * others)));
        assert_eq!(read(&path)[0].ts, keep - 1);
    }

    #[test]
    fn rotation_keeps_the_newest_of_each_and_kinds_it_doesnt_know() {
        let mut lines: Vec<String> = Vec::new();
        for n in 0..=2 * KEEP_OTHER {
            lines.push(format!("{{\"kind\":\"pr\",\"number\":{n}}}"));
        }
        for n in 0..KEEP + 1 {
            lines.push(turn(i64::try_from(n).unwrap()).line());
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let kept = rotated(&refs).unwrap();
        let (turns, others): (Vec<&String>, Vec<&String>) =
            kept.iter().partition(|l| l.contains(r#""kind":"turn""#));
        assert_eq!((turns.len(), others.len()), (KEEP, KEEP_OTHER));
        assert!(others.last().unwrap().contains(&format!("\"number\":{}", 2 * KEEP_OTHER)));
        assert!(others[0].contains(&format!("\"number\":{}", KEEP_OTHER + 1)));
        // Under both limits, left alone.
        let few: Vec<&str> = refs[..2 * KEEP].to_vec();
        assert_eq!(rotated(&few), None);
    }

    #[test]
    fn cuts_long_messages_and_file_lists() {
        let t = Turn::new(
            &"é".repeat(MESSAGE_KEPT + 5),
            (0..250).map(|n| n.to_string()).collect(),
            vec![],
            1,
        );
        assert_eq!(t.message.chars().count(), MESSAGE_KEPT + 1);
        assert!(t.message.ends_with('…'));
        assert_eq!((t.files.len(), t.files_left_out, t.file_count()), (FILES_KEPT, 50, 250));
        let t = Turn::new("", vec![], (0..25).map(|n| n.to_string()).collect(), 1);
        assert_eq!((t.commits.len(), t.commits_left_out, t.commit_count()), (COMMITS_KEPT, 5, 25));
        assert_eq!(t.commits[0], "0", "the newest kept");
    }

    #[test]
    fn notes_a_turn_as_it_goes() {
        let path = scratch("ENG-52.turn.jsonl");
        assert_eq!(read_pending(&path), Pending::default());
        note_start(&path, "aaa", 1).unwrap();
        note_files(&path, &["src/a.rs".into()]).unwrap();
        note_files(&path, &[]).unwrap();
        // A prompt after an interrupt keeps the commit the turn started on.
        note_start(&path, "bbb", 2).unwrap();
        note_files(&path, &["src/b.rs".into(), "src/a.rs".into()]).unwrap();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"fi").unwrap();
        assert_eq!(
            read_pending(&path),
            Pending {
                head: Some("aaa".into()),
                files: vec!["src/a.rs".into(), "src/b.rs".into()],
                after_stop: false,
                since: Some(1),
            }
        );
        // What a Stop leaves: the commit it ended on, which the next prompt replaces.
        std::fs::remove_file(&path).unwrap();
        note_stop(&path, "ccc", 3).unwrap();
        note_files(&path, &["src/c.rs".into()]).unwrap();
        let after = read_pending(&path);
        assert_eq!((after.head.as_deref(), after.after_stop), (Some("ccc"), true));
        assert_eq!(after.files, ["src/c.rs"]);
        note_start(&path, "ddd", 4).unwrap();
        assert_eq!(
            read_pending(&path),
            Pending { head: Some("ddd".into()), files: vec![], after_stop: false, since: Some(4) }
        );
    }

    #[test]
    fn finds_the_files_a_tool_edits() {
        let cwd = Path::new("/src/acme");
        let edit = json!({ "file_path": "/src/acme/src/main.rs", "old_string": "a" });
        assert_eq!(edited("Edit", &edit, cwd), ["src/main.rs"]);
        assert_eq!(edited("Write", &json!({ "file_path": "/tmp/x" }), cwd), ["/tmp/x"]);
        let nb = json!({ "notebook_path": "/src/acme/a.ipynb" });
        assert_eq!(edited("NotebookEdit", &nb, cwd), ["a.ipynb"]);
        assert!(edited("Read", &edit, cwd).is_empty());
        assert!(edited("Bash", &json!({ "command": "cargo test" }), cwd).is_empty());
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-a\n+b\n*** Add File: /src/acme/new.rs\n+x\n*** Delete File: old.rs\n*** Update File: a.rs\n*** Move to: b.rs\n*** End Patch";
        let want = ["src/lib.rs", "new.rs", "old.rs", "a.rs", "b.rs"];
        assert_eq!(edited("apply_patch", &json!({ "input": patch }), cwd), want);
        assert_eq!(edited("apply_patch", &json!(patch), cwd), want);
        assert_eq!(edited("shell", &json!({ "command": ["apply_patch", patch] }), cwd), want);
        let script = format!("apply_patch <<'EOF'\n{patch}\nEOF");
        assert_eq!(edited("exec_command", &json!({ "cmd": ["bash", "-lc", script] }), cwd), want);
        // A patch a command only writes out, as a test's fixture, edits nothing.
        let fixture = format!("cat > tests/fixtures/a.patch <<'EOF'\n{patch}\nEOF");
        assert!(edited("Bash", &json!({ "command": fixture }), cwd).is_empty());
        assert!(edited("shell", &json!({ "command": ["bash", "-lc", fixture] }), cwd).is_empty());
        assert!(edited("shell", &json!({ "command": ["apply_patches.sh"] }), cwd).is_empty());
        // Patch lines without the patch's start are just text.
        assert!(patch_files("*** Update File: a.rs").is_empty());
    }

    fn text() -> impl Strategy<Value = String> {
        "\\PC{0,12}"
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// What's written reads back the same.
        #[test]
        fn reads_back_what_it_writes(
            message in text(),
            files in prop::collection::vec(text(), 0..4),
            commits in prop::collection::vec("[0-9a-f]{7,40}", 0..3),
            ts in any::<i64>(),
        ) {
            let t = Turn::new(&message, files, commits, ts);
            prop_assert_eq!(parse(&t.line()), vec![t]);
        }

        /// Other processes write the log, and a hook can stop part-way: whatever the text, it
        /// reads, and every line that's a turn gives one.
        #[test]
        fn reads_any_text(lines in prop::collection::vec(prop_oneof![
            "\\PC{0,30}",
            text().prop_map(|m| turn(1).line().replace("turn 1", &m)),
            (0usize..40).prop_map(|n| turn(2).line().chars().take(n).collect()),
        ], 0..8)) {
            let text = lines.join("\n");
            let whole = lines.iter().filter(|l| {
                serde_json::from_str::<Value>(l).is_ok_and(|v| v["kind"] == "turn")
            }).count();
            prop_assert_eq!(parse(&text).len(), whole.min(KEEP));
        }

        /// A message reads back the same, whatever its names and text hold.
        #[test]
        fn a_message_reads_back(
            peer in prop::option::of(text()),
            address in prop::option::of(text()),
            text in text(),
            during in any::<bool>(),
            ts in any::<i64>(),
            out in any::<bool>(),
        ) {
            let dir = if out { Direction::Out } else { Direction::In };
            let m = Message { dir, peer, address, text, during, ts, headline: String::new() };
            prop_assert_eq!(parse_entries(&m.line()), vec![Entry::Message(m)]);
        }

        /// Whatever other processes leave in the log, each line whole gives its row, newest first,
        /// and each line it can't use gives none. The numbers are each line's time.
        #[test]
        fn reads_any_mix_of_lines(kinds in prop::collection::vec(0u8..6, 0..12)) {
            let mut lines = Vec::new();
            let mut want = Vec::new();
            for (i, kind) in kinds.iter().enumerate() {
                let n = i64::try_from(i).unwrap() + 1;
                lines.push(match kind {
                    0 => { want.push(n); turn(n).line() }
                    1 => { want.push(n); message(n).line() }
                    // Fields that aren't what they should be read as their defaults.
                    2 => {
                        want.push(0);
                        let odd =
                            r#"{"kind":"message","dir":"in","peer":[],"text":{},"ts":"soon"}"#;
                        odd.to_string()
                    }
                    3 => r#"{"kind":"message","dir":7,"text":"no way to draw it"}"#.to_string(),
                    4 => format!("not json {n}"),
                    // A line cut short, by a hook stopped part-way.
                    _ => {
                        let whole = message(n).line();
                        whole.chars().take(whole.chars().count() - 1).collect()
                    }
                });
            }
            want.reverse();
            let got: Vec<i64> = parse_entries(&lines.join("\n"))
                .iter()
                .map(|e| match e {
                    Entry::Turn(t) => t.ts,
                    Entry::Message(m) => m.ts,
                })
                .collect();
            prop_assert_eq!(got, want);
        }

        /// A rewrite keeps each budget, the newest of each and their order, and happens only once
        /// a log is past one of its limits.
        #[test]
        fn rotation_keeps_the_newest_of_each_in_order(
            is_turn in prop::collection::vec(prop::bool::weighted(0.3), 0..500),
        ) {
            let lines: Vec<String> = is_turn
                .iter()
                .enumerate()
                .map(|(i, turned)| {
                    let n = i64::try_from(i).unwrap();
                    if *turned { turn(n).line() } else { message(n).line() }
                })
                .collect();
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let turns = is_turn.iter().filter(|t| **t).count();
            let others = is_turn.len() - turns;
            let past = turns > 2 * KEEP || others > 2 * KEEP_OTHER;
            let kept = rotated(&refs);
            prop_assert_eq!(kept.is_some(), past);
            if let Some(kept) = kept {
                let at: Vec<usize> =
                    kept.iter().map(|k| lines.iter().position(|l| l == k).unwrap()).collect();
                prop_assert!(at.windows(2).all(|w| w[0] < w[1]), "in the order written");
                let want = |of_turns: bool, budget: usize| -> Vec<usize> {
                    let all: Vec<usize> = (0..lines.len())
                        .filter(|i| is_turn[*i] == of_turns)
                        .collect();
                    all[all.len().saturating_sub(budget)..].to_vec()
                };
                let got = |of_turns: bool| -> Vec<usize> {
                    at.iter().copied().filter(|i| is_turn[*i] == of_turns).collect()
                };
                prop_assert_eq!(got(true), want(true, KEEP));
                prop_assert_eq!(got(false), want(false, KEEP_OTHER));
            }
        }

        /// A prompt that doesn't start with the wrapper is never a message, whatever it quotes:
        /// the user's own prompts stay out of the log.
        #[test]
        fn a_prompt_is_never_taken_for_a_message_unless_it_starts_as_one(
            before in "[^<]\\PC{0,20}",
            after in "\\PC{0,40}",
        ) {
            let prompt = format!("{before}{WRAPPER} from-name=\"lead\">\n{after}");
            prop_assert_eq!(received(&prompt, false, 1), None);
        }

        /// A wrapper as Claude Code writes it, whatever its fields hold: they come back within
        /// their cuts, and the text is one line of what was said.
        #[test]
        fn a_wrapper_gives_back_its_fields(
            address in "[^\"]{0,400}",
            name in "[^\">]{0,150}",
            body in "(\\PC|\n){0,300}",
        ) {
            let prompt = format!(
                "{WRAPPER} from=\"{address}\" from-name=\"{name}\" from-mode=\"prompting\">\n\
                 said {body}"
            );
            let m = received(&prompt, false, 1);
            prop_assert!(m.is_some());
            let m = m.unwrap();
            prop_assert_eq!(m.address, Some(cut(&address, ADDRESS_KEPT)));
            prop_assert_eq!(m.peer, Some(cut(&name, PEER_KEPT)));
            prop_assert!(m.text.starts_with("said") && !m.text.contains('\n'));
            prop_assert!(m.text.chars().count() <= TEXT_KEPT + 1);
        }

        /// Whatever the prompt, it never panics, and what comes out is one line within the cuts.
        #[test]
        fn a_wrapper_parses_whatever_it_holds(
            attrs in "\\PC{0,40}",
            body in "(\\PC|\n){0,300}",
        ) {
            for prompt in [format!("{WRAPPER} {attrs}>{body}"), format!("{WRAPPER} {attrs}")] {
                if let Some(m) = received(&prompt, false, 1) {
                    prop_assert!(!m.text.contains('\n') && !m.text.is_empty());
                    prop_assert!(m.text.chars().count() <= TEXT_KEPT + 1);
                    let name = m.peer.as_ref().map_or(0, |p| p.chars().count());
                    prop_assert!(name <= PEER_KEPT + 1);
                }
            }
        }

        /// Whatever a name holds, a row can't be made to say a reserved name of its own, or to
        /// hold a line break.
        #[test]
        fn a_label_stays_a_label(name in "(\\PC|\n|you|user|a session){0,12}") {
            let label = named(&name).label();
            prop_assert!(!label.contains('\n'));
            prop_assert!(!RESERVED_NAMES.iter().any(|r| label.eq_ignore_ascii_case(r))
                || label == "a session" && untrusted::line(&name).trim().is_empty());
        }

        /// Whatever a send holds, what's kept is one line within the cut, and never from a send
        /// that failed.
        #[test]
        fn a_send_keeps_one_line(to in "\\PC{0,40}", body in "(\\PC|\n){0,300}") {
            let input = json!({ "to": to, "message": body });
            let ok = json!({ "success": true, "msg_id": "m" });
            if let Some(s) = sent("SendMessage", &input, &ok) {
                prop_assert!(!s.text.contains('\n') && !s.text.is_empty());
                prop_assert!(s.text.chars().count() <= TEXT_KEPT + 1);
            }
            let failed = json!({ "success": false, "msg_id": "m" });
            prop_assert_eq!(sent("SendMessage", &input, &failed), None);
        }
    }
}
