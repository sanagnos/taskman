//! `taskman hook <state>`: run by the Claude Code and Codex hooks in sessions taskman launched.
//! Records the session's state and id to the ticket's status file, which the TUI polls, and each
//! finished turn, and each message it sends or gets, to its log (`events_log`).
//! It must never block or fail loudly, so every error is swallowed.

use crate::events_log;
use crate::git;
use crate::status_file::{self, HookState, Reason, Record};
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The `--settings` JSON that wires every relevant hook event to `taskman hook`, and lets
/// `taskman status`, `taskman add` and `taskman move` run without a prompt.
pub fn settings_json(exe: &Path) -> String {
    let cmd = |state: &str| {
        json!([{ "matcher": "*", "hooks": [{
            "type": "command",
            "command": format!("{} hook {state}", shell_quote(exe)),
            "timeout": 5
        }]}])
    };
    json!({ "hooks": {
        "SessionStart": cmd("idle"),
        "UserPromptSubmit": cmd("working"),
        "PreToolUse": cmd("working"),
        "PostToolUse": cmd("working"),
        "Notification": cmd("needs"),
        "PermissionRequest": cmd("needs"),
        "Stop": cmd("done"),
        "SessionEnd": cmd("ended"),
    },
    // The briefs point sessions here. status only reads; move moves a Local files ticket between
    // columns; add opens tickets, but an agent or a role, which becomes a system prompt, waits
    // for the user to approve it in taskman (`App::take_inbox`), as text a session read may be
    // what asked for it. All three keep to the session's own project (`cli::confine`), since the
    // rules can't tell arguments apart. `taskman close` is left out on purpose: it stops sessions and removes
    // worktrees, so the session's own permission settings decide. So is `taskman title`: what it
    // writes shows on the user's card, so where Claude asks, the prompt shows them the text first,
    // and they can allow it for good.
    "permissions": { "allow": ["Bash(taskman status:*)", "Bash(taskman add:*)", "Bash(taskman move:*)"] }})
    .to_string()
}

/// `p` as one shell word: hooks run their command through a shell, and a path can hold a quote.
fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// Codex hook events and the state each reports. Codex has no Notification event: approvals
/// come as PermissionRequest, and interrupts have their own event. Nothing fires when the user
/// approves (see "Inferred states" in docs/design.md).
const CODEX_HOOKS: [(&str, &str, &str); 8] = [
    ("SessionStart", "session_start", "idle"),
    ("UserPromptSubmit", "user_prompt_submit", "working"),
    ("PreToolUse", "pre_tool_use", "working"),
    ("PostToolUse", "post_tool_use", "working"),
    ("PermissionRequest", "permission_request", "needs"),
    ("Stop", "stop", "done"),
    ("Interrupt", "interrupt", "done"),
    ("SessionEnd", "session_end", "ended"),
];

/// Codex clamps Interrupt and SessionEnd hooks to 3 s (with a warning above that), so all use 3.
const CODEX_HOOK_TIMEOUT: u64 = 3;

/// `-c` overrides that add the status hooks to a Codex session, and mark them trusted.
///
/// Codex skips hooks it hasn't trusted. Trust is a `hooks.state` entry holding the hook's
/// hash, and Codex reads those from `-c` too, so taskman trusts its own hooks for the session
/// without editing `~/.codex/config.toml` or bypassing trust for the user's other hooks. The key
/// and hash follow Codex's `hook_key` and `hook_hash` (codex-rs/hooks); if Codex changes them,
/// the hooks stop running and the sidebar no longer follows the session.
pub fn codex_config(exe: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut state = serde_json::Map::new();
    for (event, label, st) in CODEX_HOOKS {
        let command = format!("{} hook {st}", shell_quote(exe));
        out.push(format!(
            "hooks.{event}=[{{hooks=[{{type=\"command\",command={},timeout={CODEX_HOOK_TIMEOUT}}}]}}]",
            json!(command)
        ));
        state.insert(
            format!("/<session-flags>/config.toml:{label}:0:0"),
            json!(codex_hook_hash(label, &command)),
        );
    }
    let entries: Vec<String> =
        state.iter().map(|(k, h)| format!("{}={{trusted_hash={h}}}", json!(k))).collect();
    out.push(format!("hooks.state={{{}}}", entries.join(",")));
    out
}

/// Codex's trust hash for a single command hook: sha256 of its normalized form as compact JSON
/// with sorted keys (serde_json's map is ordered).
fn codex_hook_hash(event_label: &str, command: &str) -> String {
    use sha2::{Digest, Sha256};
    let identity = json!({
        "event_name": event_label,
        "hooks": [{ "type": "command", "command": command, "timeout": CODEX_HOOK_TIMEOUT, "async": false }],
    });
    let hash = Sha256::digest(identity.to_string().as_bytes());
    format!("sha256:{}", hash.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

pub fn run(state_arg: &str) {
    let Some(path) = std::env::var_os("TASKMAN_STATUS_FILE") else { return };
    let mut input = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);

    let path = Path::new(&path);
    if v["hook_event_name"] == "PostToolUse" {
        // Tell the TUI to refresh a panel right away when Claude may have changed what it shows.
        let tool = v["tool_name"].as_str().unwrap_or("");
        let command = v["tool_input"]["command"].as_str().unwrap_or("");
        if touches_jira(tool, command) {
            touch(&path.with_extension("jira"));
        }
        if touches_github(tool, command) {
            touch(&path.with_extension("gh"));
        }
    }
    let prev = status_file::read(path).unwrap_or_default();
    let Some(entry) = entry_for(HookState::from_word(state_arg), &v, &prev) else { return };
    let name = std::env::var("TASKMAN_SESSION_NAME").ok();
    let mut out = status_record(
        entry.state,
        v["session_id"].as_str(),
        name.as_deref(),
        &entry.message,
        crate::util::now(),
    );
    out.reason = entry.reason;
    out.tool = entry.tool;
    out.input = entry.input;
    // Which start of the session this hook belongs to, so the TUI can tell a record of an
    // earlier one, written late (see `app::TicketView::takes`).
    out.launch = std::env::var("TASKMAN_LAUNCH").ok().filter(|l| !l.is_empty());
    let _ = status_file::write(path, &out);
    // After the status record, so the git calls a turn's end makes never hold up "done": nothing
    // reads the log for it, and a CLI ends a hook that takes too long (Codex after 3 s).
    if logs_messages(std::env::var("TASKMAN_CLI").ok().as_deref()) {
        log_message(path, &v);
    }
    log_turn(path, &v, session_dir(std::env::var_os("TASKMAN_CWD"), &v).as_deref());
}

/// Whether the session's CLI, from `TASKMAN_CLI` (a session an older taskman started has none),
/// has messages to log. So a Codex prompt that merely starts as one stays out of the log.
fn logs_messages(cli: Option<&str>) -> bool {
    cli.is_none_or(|c| crate::store::Cli::from_label(c).messages_agents())
}

/// The session's own folder: the one taskman started it in (`TASKMAN_CWD`, given as `started`),
/// not where its shell has `cd`'d to, so a look into another worktree doesn't credit that branch's
/// commits to it. A session an older taskman started has only the hook's `cwd`.
fn session_dir(started: Option<std::ffi::OsString>, v: &Value) -> Option<PathBuf> {
    started
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| v["cwd"].as_str().map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
}

/// How long a hook waits for one git call. Tests run real git on machines that may be busy, and a
/// hook's limit isn't what they're about.
const GIT_WAIT: Duration =
    if cfg!(test) { Duration::from_secs(60) } else { Duration::from_millis(800) };

/// How long a turn's end waits for all its git calls together: the CLIs give a hook 3 s (Codex) or
/// 5 s, and a hook they end hasn't written the turn's line, which matters more than what git says.
const STOP_WAIT: Duration =
    if cfg!(test) { Duration::from_secs(300) } else { Duration::from_millis(2500) };

/// How a git call came out.
#[derive(Debug, PartialEq)]
enum Git {
    /// What it printed.
    Out(String),
    /// It ran and said no.
    Failed,
    /// It took too long or couldn't run, which says nothing about what was asked.
    NoAnswer,
}

/// Runs `git args` in `dir`, by the time `until` at the latest and within `GIT_WAIT`.
fn git_call(dir: &Path, args: &[&str], until: Instant) -> Git {
    let left = until.saturating_duration_since(Instant::now()).min(GIT_WAIT);
    if left.is_zero() {
        return Git::NoAnswer;
    }
    match git::run(dir, args, left) {
        Ok(out) if out.status.success() => {
            Git::Out(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        Ok(_) => Git::Failed,
        Err(_) => Git::NoAnswer,
    }
}

/// What `git args` printed in `dir`; None when it failed or gave no answer.
fn git_out(dir: &Path, args: &[&str], until: Instant) -> Option<String> {
    match git_call(dir, args, until) {
        Git::Out(out) => Some(out),
        Git::Failed | Git::NoAnswer => None,
    }
}

/// Adds to the session's log a line for a message another session sent it, which comes as a prompt,
/// or one it sent, which is a tool call. From the hooks' own input only.
fn log_message(status: &Path, v: &Value) {
    let log = events_log::log_path(status);
    let now = crate::util::now();
    match v["hook_event_name"].as_str().unwrap_or("") {
        "UserPromptSubmit" => {
            // Before `log_turn` notes this prompt: was a turn under way when it came?
            let noted = events_log::read_pending(&events_log::pending_path(status));
            let during = noted.head.is_some() && !noted.after_stop;
            let prompt = v["prompt"].as_str().unwrap_or("");
            if let Some(m) = events_log::received(prompt, during, now) {
                let _ = events_log::append_message(&log, &m);
            }
        }
        "PostToolUse" => {
            let tool = v["tool_name"].as_str().unwrap_or("");
            if let Some(s) = events_log::sent(tool, &v["tool_input"], &v["tool_response"]) {
                let m = events_log::Message {
                    peer: events_log::peer_at(&log, &s.to),
                    text: s.text,
                    ts: now,
                    ..events_log::Message::default()
                };
                let _ = events_log::append_message(&log, &m);
            }
        }
        _ => {}
    }
}

/// Adds to the session's log (`events_log`) for a hook event in the session working in `dir`: a
/// prompt notes the commit the turn starts on, an edit the files, and the turn's end writes its
/// line from those and the CLI's last message. From the hooks' own input only, never the CLI's
/// transcript, whose format is its own.
fn log_turn(status: &Path, v: &Value, dir: Option<&Path>) {
    let pending = events_log::pending_path(status);
    let log = events_log::log_path(status);
    let now = crate::util::now();
    match v["hook_event_name"].as_str().unwrap_or("") {
        "UserPromptSubmit" => {
            if let Some(h) = dir.and_then(head) {
                let _ = events_log::note_start(&pending, &h, now);
            }
        }
        "PostToolUse" => {
            let tool = v["tool_name"].as_str().unwrap_or("");
            let edited = events_log::edited(tool, &v["tool_input"], dir.unwrap_or(Path::new("")));
            let _ = events_log::note_files(&pending, &edited);
        }
        "Stop" => {
            let noted = events_log::read_pending(&pending);
            let until = Instant::now() + STOP_WAIT;
            // First, as it's two quick calls where a big repository's log can take all the time.
            let on_origin = match (noted.since, dir) {
                (Some(since), Some(dir)) => reached_origin_since(dir, since, until),
                _ => false,
            };
            let mut files = noted.files;
            let mut commits = Vec::new();
            if let (Some(from), Some(dir)) = (noted.head, dir)
                && let Some(made) = made_since(dir, &from, until)
            {
                // What the commits changed, so files a shell command or formatter wrote count
                // too once they're committed.
                for f in made.files {
                    if !files.contains(&f) {
                        files.push(f);
                    }
                }
                commits = made.commits;
            }
            // Claude's and Codex's Stop both carry it (Claude Code 2.1.309, Codex 0.163).
            let message = v["last_assistant_message"].as_str().unwrap_or("");
            let mut turn = events_log::Turn::new(message, files, commits, now);
            turn.session_id = v["session_id"].as_str().map(str::to_string);
            turn.on_origin = on_origin;
            if events_log::append(&log, &turn).is_ok() {
                // A Stop hook of the user's own can keep Claude going with no new prompt
                // (`stop_hook_active`): the commit it ends on starts what follows, until a prompt
                // starts a turn of its own.
                let _ = std::fs::remove_file(&pending);
                if let Some(h) = dir.and_then(|d| head_by(d, until)) {
                    let _ = events_log::note_stop(&pending, &h, now);
                }
            }
        }
        _ => {}
    }
}

/// The commit checked out in `dir`; None outside a repository or before its first commit.
fn head(dir: &Path) -> Option<String> {
    head_by(dir, Instant::now() + GIT_WAIT)
}

/// `head`, by the time `until`.
fn head_by(dir: &Path, until: Instant) -> Option<String> {
    let id = git_out(dir, &["rev-parse", "--verify", "-q", "HEAD"], until)?.trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// Whether the branch checked out in `dir` reached origin after `since`, in seconds since the
/// epoch, from what git recorded: a push updates `refs/remotes/origin/<branch>` and its reflog says
/// `update by push`, which a dry run, a refused push or a fetch doesn't. So no command is parsed,
/// which would take `echo "git push"` for one and miss a push from a script, and Claude and Codex
/// are alike.
///
/// It says the branch got there, not who pushed it: worktrees share that reflog, so a push from the
/// main checkout or another session counts, and so does one after an interrupted turn, which leaves
/// `since` at its start. It can also miss one, as the branch must still be this ref when the turn
/// ends. A second is the reflog's finest, so one in the very second `since` counts for neither side
/// of it, rather than for both turns.
fn reached_origin_since(dir: &Path, since: i64, until: Instant) -> bool {
    let Some(branch) = git_out(dir, &["symbolic-ref", "-q", "HEAD"], until) else { return false };
    let Some(branch) = branch.trim().strip_prefix("refs/heads/").map(str::to_string) else {
        return false;
    };
    let remote = format!("refs/remotes/origin/{branch}");
    let log = ["reflog", "show", "--date=unix", "--format=%gd%x09%gs", "-n", "20", &remote];
    git_out(dir, &log, until).is_some_and(|out| pushes_after(&out, since))
}

/// Whether `git reflog show --date=unix --format=%gd%x09%gs`'s output has a push after `since`.
fn pushes_after(reflog: &str, since: i64) -> bool {
    reflog.lines().any(|l| {
        let Some((at, what)) = l.split_once('\t') else { return false };
        let when = at.rsplit_once("@{").and_then(|(_, t)| t.strip_suffix('}'));
        what.starts_with("update by push")
            && when.and_then(|t| t.parse::<i64>().ok()).is_some_and(|t| t > since)
    })
}

/// The commits a turn made and the files they changed.
#[derive(Debug, Default, PartialEq)]
struct Made {
    /// Newest first.
    commits: Vec<String>,
    /// Relative to the session's folder, as the edit tools' are (`events_log::edited`).
    files: Vec<String>,
}

/// What's on `dir`'s HEAD since `from` that the repository's default branch doesn't have: a turn
/// that rebases or merges onto main brings main's commits into `from..HEAD`, and they aren't the
/// turn's. The default branch is the remote's (`origin/HEAD`), else a local main or master; on that
/// branch itself, nothing is left out, or a session committing there would show nothing.
fn made_since(dir: &Path, from: &str, until: Instant) -> Option<Made> {
    let run = |args: &[&str]| git_out(dir, args, until);
    // One call for where the session is, its branch and the default branch.
    let format = "--format=%(HEAD)%09%(refname)%09%(symref)";
    let refs = run(&["for-each-ref", format, "refs/remotes/origin/HEAD", "refs/heads/"])?;
    let prefix = run(&["rev-parse", "--show-prefix"])?;
    let range = format!("{from}..HEAD");
    let log = |not: Option<&str>| {
        let mut args = vec!["log", "--format=%x00%H", "--name-only", "-z", &range];
        args.extend(not);
        args.push("--");
        git_call(dir, &args, until)
    };
    let not_base = base_ref(&refs).map(|b| format!("^{b}"));
    let out = commits_log(log(not_base.as_deref()), || match &not_base {
        Some(_) => log(None),
        None => Git::Failed,
    })?;
    Some(parse_made(&out, prefix.trim_end_matches('\n')))
}

/// The turn's `git log`, from how the one leaving the default branch out came out (`first`). A
/// default branch that's gone (an `origin/HEAD` left pointing nowhere) makes git say no, so it asks
/// again leaving nothing out. A log with no answer says nothing of the sort: in a big repository
/// asking again would count what a rebase brought in as the turn's, so that's no commits at all.
fn commits_log(first: Git, leaving_nothing_out: impl FnOnce() -> Git) -> Option<String> {
    let answer = match first {
        Git::Failed => leaving_nothing_out(),
        other => other,
    };
    match answer {
        Git::Out(out) => Some(out),
        Git::Failed | Git::NoAnswer => None,
    }
}

/// The default branch to leave out, from `for-each-ref`'s `%(HEAD)\t%(refname)\t%(symref)` lines
/// for `origin/HEAD` and the local branches; None when there's none, or it's the one checked out.
fn base_ref(refs: &str) -> Option<String> {
    let lines: Vec<(&str, &str, &str)> = refs
        .lines()
        .map(|l| {
            let mut f = l.split('\t');
            (f.next().unwrap_or(""), f.next().unwrap_or(""), f.next().unwrap_or(""))
        })
        .collect();
    let current = lines.iter().find(|l| l.0 == "*").map(|l| l.1);
    let local = |name: &str| lines.iter().any(|l| l.1 == format!("refs/heads/{name}"));
    let (base, name) = match lines.iter().find(|l| l.1 == "refs/remotes/origin/HEAD") {
        Some(&(_, _, target)) => {
            (target.to_string(), target.strip_prefix("refs/remotes/origin/")?.to_string())
        }
        None => {
            let name = ["main", "master"].into_iter().find(|n| local(n))?;
            (format!("refs/heads/{name}"), name.to_string())
        }
    };
    let checked_out = format!("refs/heads/{name}");
    (current != Some(checked_out.as_str())).then_some(base)
}

/// `git log --format=%x00%H --name-only -z`'s output: a NUL, a commit, a NUL, then its files, each
/// ending in a NUL, the first after a newline. Files are from the repository's top; `prefix` is
/// the session's folder in it (`rev-parse --show-prefix`).
fn parse_made(out: &str, prefix: &str) -> Made {
    let mut made = Made::default();
    let mut seen = std::collections::HashSet::new();
    let mut next_is_commit = false;
    for token in out.split('\0') {
        let token = token.trim_start_matches('\n');
        if token.is_empty() {
            next_is_commit = true;
        } else if std::mem::take(&mut next_is_commit) {
            made.commits.push(token.to_string());
        } else {
            let f = relative_to(prefix, token);
            if seen.insert(f.clone()) {
                made.files.push(f);
            }
        }
    }
    made
}

/// `path`, from the repository's top, as seen from its folder `prefix` (`git rev-parse
/// --show-prefix`: empty at the top, else ending in `/`).
fn relative_to(prefix: &str, path: &str) -> String {
    if let Some(inside) = path.strip_prefix(prefix) {
        return inside.to_string();
    }
    let here: Vec<&str> = prefix.split('/').filter(|s| !s.is_empty()).collect();
    let there: Vec<&str> = path.split('/').collect();
    let same = here.iter().zip(&there).take_while(|(a, b)| a == b).count();
    let up = "../".repeat(here.len() - same);
    format!("{up}{}", there[same..].join("/"))
}

/// What the status file says after a hook event.
#[derive(Debug, PartialEq)]
struct Entry {
    state: HookState,
    /// Why a "needs" session waits on the user (see `Reason`).
    reason: Option<Reason>,
    message: String,
    /// What a permission prompt asks to use and run (`Record::tool`, `Record::input`).
    tool: Option<String>,
    input: Option<String>,
}

/// The longest input a record keeps, in characters: enough to read what a command does.
const INPUT_KEPT: usize = 400;

/// What a tool call's input asks for, in a line: a command (Claude's string, Codex's words), else
/// a path, URL or pattern, else the JSON itself. None when there is no input.
fn input_text(tool_input: &Value) -> Option<String> {
    let text = match tool_input {
        Value::Null => return None,
        Value::Object(o) => {
            let field = |k: &str| match o.get(k)? {
                Value::String(s) => Some(s.clone()),
                Value::Array(a) => {
                    Some(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                }
                _ => None,
            };
            ["command", "file_path", "path", "url", "pattern", "query"]
                .iter()
                .find_map(|k| field(k))
                .unwrap_or_else(|| tool_input.to_string())
        }
        other => other.to_string(),
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then(|| text.chars().take(INPUT_KEPT).collect())
}

/// Tools that wait on the user's answers, so they count as "needs you", not "working". Claude's
/// questions can take several screens, and Codex's `request_user_input` up to three questions.
const QUESTION_TOOLS: [&str; 3] = ["AskUserQuestion", "ExitPlanMode", "request_user_input"];

/// The status a hook event writes, given the file as it stands (`prev`), or None to leave it.
fn entry_for(state_arg: HookState, v: &Value, prev: &Record) -> Option<Entry> {
    let tool = v["tool_name"].as_str().unwrap_or("");
    let entry = |state, reason, message: &str| Entry {
        state,
        reason,
        message: message.into(),
        tool: None,
        input: None,
    };
    let (needs, asking) = (HookState::Needs, Some(Reason::Question));
    Some(match v["hook_event_name"].as_str().unwrap_or("") {
        "PreToolUse" if QUESTION_TOOLS.contains(&tool) => entry(needs, asking, &question(tool)),
        // Claude's prompts (it fires this for its questions and plans too) and Codex's approvals,
        // at the moment the prompt appears.
        "PermissionRequest" if QUESTION_TOOLS.contains(&tool) => {
            entry(needs, asking, &question(tool))
        }
        "PermissionRequest" => {
            let message = match v["tool_input"]["description"].as_str() {
                Some(d) => d.to_string(),
                None if tool.is_empty() => "Waiting for your approval".into(),
                None => format!("Waiting for your approval to use {tool}"),
            };
            Entry {
                tool: Some(tool).filter(|t| !t.is_empty()).map(str::to_string),
                input: input_text(&v["tool_input"]),
                ..entry(needs, Some(Reason::Permission), &message)
            }
        }
        "Notification" => {
            let message = v["message"].as_str().unwrap_or("");
            match v["notification_type"].as_str() {
                Some("auth_success") => return None,
                Some("idle_prompt") => entry(HookState::Done, None, message),
                _ if message.contains("waiting for your input") => {
                    entry(HookState::Done, None, message)
                }
                // Claude sends this 6 s after any prompt it still waits on, questions and plans
                // included, and versions without PermissionRequest send only this. So it keeps what
                // an earlier hook said the session waits for.
                Some("permission_prompt") if prev.state == HookState::Needs => Entry {
                    state: needs,
                    reason: prev.reason.or(asking),
                    message: Some(prev.message.as_str())
                        .filter(|m| !m.is_empty())
                        .unwrap_or(message)
                        .into(),
                    tool: prev.tool.clone(),
                    input: prev.input.clone(),
                },
                Some("permission_prompt") => entry(needs, Some(Reason::Permission), message),
                _ => entry(needs, asking, message),
            }
        }
        // Claude starts its session again after compacting, also in the middle of a turn: keep
        // the state rather than calling a working session idle.
        "SessionStart" if v["source"] == "compact" && prev.state != HookState::Unknown => Entry {
            state: prev.state,
            reason: prev.reason,
            message: prev.message.clone(),
            tool: prev.tool.clone(),
            input: prev.input.clone(),
        },
        _ => entry(state_arg, None, ""),
    })
}

fn question(tool: &str) -> String {
    let cli = if tool == "request_user_input" { "Codex" } else { "Claude" };
    format!("{cli} is asking you a question")
}

/// The status file's record (see `status_file`). `name` is the session's `--name`; `run` adds
/// `reason` to a "needs" record and `launch`, the start of the session the hook belongs to.
pub fn status_record(
    state: HookState,
    session_id: Option<&str>,
    name: Option<&str>,
    message: &str,
    ts: i64,
) -> Record {
    Record {
        state,
        session_id: session_id.map(str::to_string),
        name: name.map(str::to_string),
        message: message.into(),
        ts,
        ..Record::default()
    }
}

fn touch(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, crate::util::now().to_string());
}

/// Tracker tools: any MCP tool whose name mentions Jira, Atlassian or Linear, or a GitHub one
/// about issues, or a Jira CLI or `gh issue`.
fn touches_jira(tool: &str, command: &str) -> bool {
    let t = tool.to_lowercase();
    t.contains("jira")
        || t.contains("atlassian")
        || t.contains("linear")
        || (t.contains("github") && t.contains("issue"))
        || (tool == "Bash"
            && (command.contains("acli ")
                || command.contains("gh issue")
                || command.split_whitespace().next() == Some("jira")))
}

/// GitHub MCP tools, pushes, and gh commands that read or change PRs and CI.
fn touches_github(tool: &str, command: &str) -> bool {
    tool.to_lowercase().contains("github")
        || (tool == "Bash"
            && ["git push", "gh pr", "gh run", "gh workflow", "gh api", "gh issue"]
                .iter()
                .any(|c| command.contains(c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `git args` in `dir`, as a user who commits.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=Tester", "-c", "user.email=tester@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A hook event as `run` gives it to a Claude session: its message, then its turn.
    fn hook_in(status: &Path, v: &Value, dir: Option<&Path>) {
        log_message(status, v);
        log_turn(status, v, dir);
    }

    /// The turns in the log beside the status file `status`, newest first.
    fn turns(status: &Path) -> Vec<events_log::Turn> {
        entries(status)
            .into_iter()
            .filter_map(|e| match e {
                events_log::Entry::Turn(t) => Some(t),
                events_log::Entry::Message(_) => None,
            })
            .collect()
    }

    /// The messages in that log, newest first.
    fn messages(status: &Path) -> Vec<events_log::Message> {
        entries(status)
            .into_iter()
            .filter_map(|e| match e {
                events_log::Entry::Message(m) => Some(m),
                events_log::Entry::Turn(_) => None,
            })
            .collect()
    }

    fn entries(status: &Path) -> Vec<events_log::Entry> {
        events_log::read_entries(&events_log::log_path(status))
    }

    /// A hook's input recorded from a real CLI and trimmed to mock names.
    fn recorded(file: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks").join(file);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// A status file for a test named `name`, in a scratch folder with no log yet.
    fn scratch_status(name: &str) -> PathBuf {
        let dir = crate::store::scratch::dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("status/acme/ENG-52.json")
    }

    #[test]
    fn a_send_that_reached_a_session_writes_one_line() {
        let status = scratch_status("hook-send");
        hook_in(&status, &recorded("claude-send.json"), None);
        let logged = messages(&status);
        assert_eq!(logged.len(), 1, "{logged:?}");
        let m = &logged[0];
        assert_eq!(m.dir, events_log::Direction::Out);
        assert_eq!(m.peer.as_deref(), Some("ENG-53"));
        assert_eq!(m.text, "Rebase on main when ENG-52 merges.");
        assert!(!m.during);
    }

    #[test]
    fn what_never_reached_a_session_writes_nothing() {
        for file in [
            // Before the tool ran: it may yet fail.
            "claude-send-before.json",
            "claude-send-failed.json",
            "claude-idle-subscription.json",
            "claude-send-to-subagent.json",
            "claude-idle-notice.json",
            "claude-user-prompt.json",
            "codex-user-prompt.json",
            "codex-bash.json",
        ] {
            let status = scratch_status("hook-no-send");
            hook_in(&status, &recorded(file), None);
            assert!(entries(&status).is_empty(), "{file}");
        }
    }

    #[test]
    fn a_subagents_send_counts_as_its_sessions() {
        let status = scratch_status("hook-subagent-send");
        let v = recorded("claude-send-from-subagent.json");
        assert!(v["agent_id"].is_string(), "the recording is a subagent's");
        hook_in(&status, &v, None);
        assert_eq!(messages(&status)[0].text, "The build passes on main.");
    }

    #[test]
    fn a_message_from_another_session_writes_one_line() {
        let status = scratch_status("hook-received");
        hook_in(&status, &recorded("claude-received.json"), None);
        let logged = messages(&status);
        assert_eq!(logged.len(), 1);
        let m = &logged[0];
        assert_eq!(m.dir, events_log::Direction::In);
        assert_eq!(m.peer.as_deref(), Some("lead"));
        assert_eq!(m.text, "Fix the lint failure.");
        assert!(!m.during, "no turn was under way");
    }

    #[test]
    fn a_message_during_a_turn_says_so_and_doesnt_start_one() {
        let dir = crate::store::scratch::dir().join("hook-during");
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("acme");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q"]);
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
        let status = dir.join("status/acme/ENG-52.json");
        let hook = |v: Value| hook_in(&status, &v, Some(&repo));
        let start = events_log::read_pending(&events_log::pending_path(&status));
        assert_eq!(start.head, None);

        hook(recorded("claude-user-prompt.json"));
        let noted = events_log::read_pending(&events_log::pending_path(&status));
        hook(recorded("claude-received.json"));
        assert!(messages(&status)[0].during, "a turn was under way");
        // It's the same turn: the commit it started from stays.
        let after = events_log::read_pending(&events_log::pending_path(&status));
        assert_eq!((after.head.clone(), after.since), (noted.head, noted.since));

        // After its end, a message comes to a session with nothing under way.
        hook(json!({ "hook_event_name": "Stop", "last_assistant_message": "Done." }));
        hook(recorded("claude-received.json"));
        assert!(!messages(&status)[0].during);
        // ... and an interrupted turn fires no Stop, so the next one reads as during. Known, and
        // said in docs/design.md: Esc fires no hook at all.
        hook(recorded("claude-user-prompt.json"));
        hook(recorded("claude-received.json"));
        assert!(messages(&status)[0].during);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reply_by_address_names_who_it_answers_and_never_the_address() {
        let status = scratch_status("hook-reply");
        let log = events_log::log_path(&status);
        // An address it has heard nothing from.
        hook_in(&status, &recorded("claude-send-by-address.json"), None);
        assert_eq!(messages(&status)[0].peer, None);
        assert_eq!(messages(&status)[0].label(), "a session");
        // Once a message came from there, a reply takes the name it gave.
        hook_in(&status, &recorded("claude-received.json"), None);
        hook_in(&status, &recorded("claude-send-by-address.json"), None);
        assert_eq!(messages(&status)[0].peer.as_deref(), Some("lead"));
        // The address is kept only where a message came from, for this lookup.
        let text = std::fs::read_to_string(&log).unwrap();
        let out: Vec<&str> = text.lines().filter(|l| l.contains(r#""dir":"out""#)).collect();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|l| !l.contains("uds:")), "{out:?}");
    }

    /// `repo`, a clone of a bare origin on branch `eng-52` with a commit to push.
    fn cloned(dir: &Path) -> PathBuf {
        let _ = std::fs::remove_dir_all(dir);
        let origin = dir.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git_in(&origin, &["init", "-q", "--bare"]);
        let repo = dir.join("acme");
        git_in(dir, &["clone", "-q", origin.to_str().unwrap(), repo.to_str().unwrap()]);
        git_in(&repo, &["checkout", "-q", "-b", "eng-52"]);
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
        repo
    }

    /// A turn in `repo` that began a few seconds ago, for what `run` does in it to count as after
    /// its start, as a second is all the reflog tells apart. Says whether it came out on origin.
    fn turn_on_origin(status: &Path, repo: &Path, run: &dyn Fn()) -> bool {
        let pending = events_log::pending_path(status);
        let start = crate::util::now() - 5;
        events_log::note_start(&pending, &head(repo).unwrap(), start).unwrap();
        run();
        log_turn(status, &json!({ "hook_event_name": "Stop" }), Some(repo));
        turns(status)[0].on_origin
    }

    #[test]
    fn a_turn_says_its_branch_reached_origin_from_what_git_recorded() {
        let dir = crate::store::scratch::dir().join("hook-on-origin");
        let repo = cloned(&dir);
        let status = dir.join("status/acme/ENG-52.json");
        let turn = |repo: &Path, run: &dyn Fn()| turn_on_origin(&status, repo, run);
        // Nothing, a dry run, and only the words.
        assert!(!turn(&repo, &|| {}));
        assert!(!turn(&repo, &|| {
            git_in(&repo, &["push", "-q", "--dry-run", "-u", "origin", "eng-52"]);
        }));
        assert!(!turn(&repo, &|| {
            git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "git push origin eng-52"]);
        }));
        // A real one, with no upstream set.
        assert!(turn(&repo, &|| {
            git_in(&repo, &["push", "-q", "origin", "eng-52"]);
        }));

        // A fetch moves the same ref, in a clone that has pushed nothing, and isn't a push.
        let origin = dir.join("origin.git");
        let fetcher = dir.join("fetcher");
        git_in(&dir, &["clone", "-q", origin.to_str().unwrap(), fetcher.to_str().unwrap()]);
        git_in(&fetcher, &["checkout", "-q", "eng-52"]);
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "more"]);
        git_in(&repo, &["push", "-q", "origin", "eng-52"]);
        assert!(!turn(&fetcher, &|| {
            git_in(&fetcher, &["fetch", "-q", "origin"]);
        }));
        let ref_log = ["reflog", "show", "--format=%gs", "-n", "1", "refs/remotes/origin/eng-52"];
        let moved = git_in(&fetcher, &ref_log);
        assert!(moved.starts_with("fetch"), "{moved}");

        // A push to another branch, which this one's reflog doesn't show, is missed, not claimed.
        assert!(!turn(&fetcher, &|| {
            git_in(&fetcher, &["checkout", "-q", "-b", "side"]);
            git_in(&fetcher, &["push", "-q", "origin", "side:other"]);
        }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Why the row says "on origin", not "pushed": git records that the branch got there, and
    /// worktrees share that record, so a push nobody in this session made counts.
    #[test]
    fn a_push_from_another_checkout_counts_too() {
        let dir = crate::store::scratch::dir().join("hook-other-checkout");
        let main = cloned(&dir);
        let worktree = dir.join("acme-ENG-53");
        let wt = worktree.to_str().unwrap();
        git_in(&main, &["worktree", "add", "-q", "-b", "feat/eng-53", wt]);
        git_in(&worktree, &["commit", "-q", "--allow-empty", "-m", "work"]);
        let status = dir.join("status/acme/ENG-53.json");
        // The session in the worktree pushes nothing; the main checkout pushes its branch.
        assert!(turn_on_origin(&status, &worktree, &|| {
            git_in(&main, &["push", "-q", "origin", "feat/eng-53"]);
        }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An interrupt fires no hook, so the turn after one counts from the interrupted one's start.
    #[test]
    fn a_turn_after_an_interrupt_counts_from_the_interrupted_ones_start() {
        let dir = crate::store::scratch::dir().join("hook-interrupted");
        let repo = cloned(&dir);
        let status = dir.join("status/acme/ENG-52.json");
        let pending = events_log::pending_path(&status);
        let long_ago = crate::util::now() - 3600;
        events_log::note_start(&pending, &head(&repo).unwrap(), long_ago).unwrap();
        // Interrupted; then a push; then the next prompt, which keeps the older start.
        git_in(&repo, &["push", "-q", "origin", "eng-52"]);
        log_turn(&status, &json!({ "hook_event_name": "UserPromptSubmit" }), Some(&repo));
        assert_eq!(events_log::read_pending(&pending).since, Some(long_ago));
        log_turn(&status, &json!({ "hook_event_name": "Stop" }), Some(&repo));
        assert!(turns(&status)[0].on_origin);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pushes_are_read_from_the_reflog() {
        let log = "origin/eng-52@{200}\tupdate by push\n\
                   origin/eng-52@{150}\tfetch origin: forced-update\n\
                   origin/eng-52@{100}\tupdate by push\n";
        assert!(pushes_after(log, 199));
        assert!(pushes_after(log, 101));
        assert!(!pushes_after(log, 200), "in the second it started: neither turn's, not both");
        assert!(!pushes_after(log, 201), "before the turn");
        let fetched = "origin/eng-52@{200}\tfetch origin: fast-forward\n";
        assert!(!pushes_after(fetched, 0));
        assert!(!pushes_after("", 0));
        assert!(!pushes_after("junk\nno tab here\na@{x}\tupdate by push\n", 0));
    }

    #[test]
    fn a_turns_end_stops_asking_git_at_its_limit() {
        let dir = crate::store::scratch::dir().join("hook-limit");
        let repo = cloned(&dir);
        let from = head(&repo).unwrap();
        let later = Instant::now() + STOP_WAIT;
        git_in(&repo, &["push", "-q", "origin", "eng-52"]);
        assert!(reached_origin_since(&repo, 0, later));
        assert!(made_since(&repo, &from, later).is_some());
        // No time left: no call, nothing claimed, and the line goes in without it.
        let gone = Instant::now();
        assert!(git_out(&repo, &["status"], gone).is_none());
        assert!(!reached_origin_since(&repo, 0, gone));
        assert!(made_since(&repo, &from, gone).is_none());
        assert!(head_by(&repo, gone).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_log_that_said_no_is_asked_again() {
        let out = || Git::Out("all".to_string());
        let never = || -> Git { panic!("asked again") };
        assert_eq!(commits_log(Git::Out("own".into()), never).as_deref(), Some("own"));
        assert_eq!(commits_log(Git::Failed, out).as_deref(), Some("all"), "the base is gone");
        assert_eq!(commits_log(Git::Failed, || Git::Failed), None);
        assert_eq!(commits_log(Git::Failed, || Git::NoAnswer), None);
        // Too slow to answer: nothing says the base is gone, so nothing counts its commits.
        assert_eq!(commits_log(Git::NoAnswer, never), None);
    }

    #[test]
    fn a_cli_with_no_messages_logs_none() {
        assert!(logs_messages(Some("Claude")));
        assert!(!logs_messages(Some("Codex")));
        assert!(logs_messages(None), "a session an older taskman started");
    }

    #[test]
    fn a_turn_outside_a_branch_or_a_remote_reaches_no_origin() {
        let later = || Instant::now() + STOP_WAIT;
        let dir = crate::store::scratch::dir().join("hook-no-remote");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!reached_origin_since(&dir, 0, later()), "not a repository");
        git_in(&dir, &["init", "-q"]);
        git_in(&dir, &["commit", "-q", "--allow-empty", "-m", "start"]);
        assert!(!reached_origin_since(&dir, 0, later()), "no remote");
        git_in(&dir, &["checkout", "-q", "--detach"]);
        assert!(!reached_origin_since(&dir, 0, later()), "no branch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logs_each_turn_with_its_edits_and_commits() {
        let dir = crate::store::scratch::dir().join("hook-turns");
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("acme");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q"]);
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
        let status = dir.join("status/acme/ENG-52.json");
        let cwd = repo.display().to_string();
        let hook = |v: Value| log_turn(&status, &v, v["cwd"].as_str().map(Path::new));
        let edit = |file: &str| {
            json!({ "hook_event_name": "PostToolUse", "cwd": cwd, "tool_name": "Edit",
                "tool_input": { "file_path": repo.join(file).display().to_string() } })
        };

        hook(json!({ "hook_event_name": "UserPromptSubmit", "cwd": cwd, "prompt": "fix it" }));
        hook(edit("src/main.rs"));
        hook(json!({ "hook_event_name": "PostToolUse", "cwd": cwd, "tool_name": "Bash",
            "tool_input": { "command": "cargo test" } }));
        hook(edit("src/main.rs"));
        // The edited file, committed, and one a formatter wrote, which no edit tool named.
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(repo.join("src/fmt.rs"), "\n").unwrap();
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-q", "-m", "one"]);
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let newest = git_in(&repo, &["rev-parse", "HEAD"]);
        hook(json!({ "hook_event_name": "Stop", "cwd": cwd, "session_id": "5f0c",
            "last_assistant_message": "Fixed the lint failure." }));
        let logged = turns(&status);
        assert_eq!(logged.len(), 1);
        let t = &logged[0];
        assert_eq!(
            (t.message.as_str(), t.files.as_slice()),
            ("Fixed the lint failure.", &["src/main.rs".to_string(), "src/fmt.rs".into()][..])
        );
        assert_eq!((t.commits.len(), t.commits[0].as_str()), (2, newest.as_str()));
        assert_eq!(t.session_id.as_deref(), Some("5f0c"));
        let left = events_log::read_pending(&events_log::pending_path(&status));
        assert_eq!((left.head, left.after_stop), (Some(newest), true));

        // A Stop hook of the user's own kept Claude going, with no prompt: what it commits then is
        // the next Stop's.
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "more"]);
        hook(json!({ "hook_event_name": "Stop", "cwd": cwd, "stop_hook_active": true,
            "last_assistant_message": "Also did more." }));
        let t = &turns(&status)[0];
        assert_eq!((t.message.as_str(), t.commits.len()), ("Also did more.", 1));
        // ... while a prompt starts from where it is: a commit of the user's between turns isn't
        // the session's.
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "by hand"]);
        hook(json!({ "hook_event_name": "UserPromptSubmit", "cwd": cwd }));
        hook(json!({ "hook_event_name": "Stop", "cwd": cwd, "last_assistant_message": "" }));
        assert_eq!(turns(&status)[0].commits.len(), 0);

        // An interrupted turn fires no Stop: its edits go into the next turn's line, which
        // counts commits from where the interrupted one started.
        hook(json!({ "hook_event_name": "UserPromptSubmit", "cwd": cwd }));
        hook(edit("a.rs"));
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "three"]);
        hook(json!({ "hook_event_name": "UserPromptSubmit", "cwd": cwd }));
        hook(edit("b.rs"));
        hook(json!({ "hook_event_name": "Stop", "cwd": cwd, "last_assistant_message": "" }));
        let t = &turns(&status)[0];
        assert_eq!(
            (t.files.clone(), t.commits.len()),
            (vec!["a.rs".to_string(), "b.rs".into()], 1)
        );

        // A turn outside a repository.
        let elsewhere = dir.join("plain");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let cwd = elsewhere.display().to_string();
        hook(json!({ "hook_event_name": "UserPromptSubmit", "cwd": cwd }));
        hook(json!({ "hook_event_name": "Stop", "cwd": cwd, "last_assistant_message": "Done." }));
        let t = &turns(&status)[0];
        assert_eq!((t.message.as_str(), t.commits.len(), t.file_count()), ("Done.", 0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rebase_or_merge_onto_main_credits_only_the_turns_own_commits() {
        let dir = crate::store::scratch::dir().join("hook-rebase");
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("acme");
        std::fs::create_dir_all(&repo).unwrap();
        let status = dir.join("status/acme/ENG-52.json");
        let commit = |file: &str| {
            std::fs::write(repo.join(file), file).unwrap();
            git_in(&repo, &["add", file]);
            git_in(&repo, &["commit", "-q", "-m", file]);
        };
        git_in(&repo, &["init", "-q", "-b", "main"]);
        commit("start.rs");
        git_in(&repo, &["checkout", "-q", "-b", "eng-52"]);
        commit("feature.rs");
        git_in(&repo, &["checkout", "-q", "main"]);
        commit("upstream-1.rs");
        commit("upstream-2.rs");
        git_in(&repo, &["checkout", "-q", "eng-52"]);
        // The session works in its worktree; its shell's `cd` elsewhere doesn't move the range.
        let elsewhere = dir.join("elsewhere").display().to_string();
        let hook = |v: Value| log_turn(&status, &v, Some(&repo));
        let turn = |run: &dyn Fn()| {
            hook(json!({ "hook_event_name": "UserPromptSubmit", "cwd": elsewhere }));
            run();
            hook(json!({ "hook_event_name": "Stop", "cwd": elsewhere }));
            let t = turns(&status).remove(0);
            (t.commit_count(), t.files)
        };

        // On a local main, no remote: the rebased commit is the turn's, main's aren't.
        let rebase = || {
            git_in(&repo, &["rebase", "-q", "main"]);
        };
        assert_eq!(turn(&rebase), (1, vec!["feature.rs".to_string()]));

        // With a remote, its default branch: a merge brings in only the merge commit.
        let main = git_in(&repo, &["rev-parse", "main"]);
        git_in(&repo, &["checkout", "-q", "main"]);
        commit("upstream-3.rs");
        let advanced = git_in(&repo, &["rev-parse", "main"]);
        git_in(&repo, &["update-ref", "refs/remotes/origin/main", &advanced]);
        git_in(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
        git_in(&repo, &["checkout", "-q", "eng-52"]);
        // The local main is behind; only the remote's counts.
        git_in(&repo, &["branch", "-q", "-f", "main", &main]);
        let merge = || {
            git_in(&repo, &["merge", "-q", "--no-edit", "origin/main"]);
        };
        assert_eq!(turn(&merge), (1, vec![]));

        // On the default branch itself, everything since the prompt is the turn's.
        git_in(&repo, &["checkout", "-q", "main"]);
        let work = || {
            commit("on-main-1.rs");
            commit("on-main-2.rs");
        };
        let (count, files) = turn(&work);
        assert_eq!((count, files.len()), (2, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_branch_left_out() {
        let refs = |lines: &[&str]| lines.join("\n");
        let origin = " \trefs/remotes/origin/HEAD\trefs/remotes/origin/trunk";
        assert_eq!(
            base_ref(&refs(&[origin, "*\trefs/heads/eng-52\t", " \trefs/heads/main\t"])),
            Some("refs/remotes/origin/trunk".into())
        );
        assert_eq!(base_ref(&refs(&[origin, "*\trefs/heads/trunk\t"])), None, "on it");
        assert_eq!(
            base_ref(&refs(&[" \trefs/heads/master\t", "*\trefs/heads/eng-52\t"])),
            Some("refs/heads/master".into())
        );
        assert_eq!(base_ref(&refs(&["*\trefs/heads/main\t"])), None);
        assert_eq!(base_ref(&refs(&["*\trefs/heads/eng-52\t"])), None, "no default branch");
        assert_eq!(base_ref(""), None);
    }

    #[test]
    fn a_turns_commits_and_their_files() {
        let out = "\0aaa\0\nsrc/a.rs\0b c.rs\0\0bbb\0\0ccc\0\nsrc/a.rs\0";
        assert_eq!(
            parse_made(out, ""),
            Made {
                commits: vec!["aaa".into(), "bbb".into(), "ccc".into()],
                files: vec!["src/a.rs".into(), "b c.rs".into()],
            }
        );
        assert_eq!(parse_made(out, "src/").files, ["a.rs", "../b c.rs"]);
        assert_eq!(parse_made("", ""), Made::default());
    }

    #[test]
    fn the_sessions_folder_is_where_taskman_started_it() {
        let v = json!({ "cwd": "/src/acme-other" });
        let started = Some("/src/acme-ENG-52".into());
        assert_eq!(session_dir(started, &v), Some("/src/acme-ENG-52".into()));
        assert_eq!(session_dir(Some("".into()), &v), Some("/src/acme-other".into()));
        assert_eq!(session_dir(None, &v), Some("/src/acme-other".into()));
    }

    #[test]
    fn committed_files_are_relative_to_the_sessions_folder() {
        assert_eq!(relative_to("", "src/a.rs"), "src/a.rs");
        assert_eq!(relative_to("crates/app/", "crates/app/src/a.rs"), "src/a.rs");
        assert_eq!(relative_to("crates/app/", "crates/lib/b.rs"), "../lib/b.rs");
        assert_eq!(relative_to("crates/app/", "README.md"), "../../README.md");
        assert_eq!(relative_to("app/", "application.rs"), "../application.rs");
    }

    #[test]
    fn detects_tools_that_change_panels() {
        assert!(touches_jira("mcp__plugin_atlassian_atlassian__addOrEditJiraIssueComment", ""));
        assert!(touches_jira("mcp__jira__jira_add_comment", ""));
        assert!(touches_jira("mcp__claude_ai_Linear__create_comment", ""));
        assert!(!touches_jira("Bash", "git status"));
        // GitHub Issues as the tracker.
        assert!(touches_jira("Bash", "gh issue comment 12 --body done"));
        assert!(touches_jira("mcp__github__add_issue_comment", ""));
        assert!(!touches_jira("mcp__github__create_pull_request", ""));
        assert!(touches_github("Bash", "git push -u origin HEAD"));
        assert!(touches_github("Bash", "cd x && gh pr create --fill"));
        assert!(touches_github("mcp__github__create_pull_request", ""));
        assert!(!touches_github("Read", ""));
    }

    /// The entry a hook event writes over the record `prev`, as (state, reason, message) the
    /// way the file spells them.
    fn after(
        state_arg: &str,
        event: &Value,
        prev: &Value,
    ) -> Option<(String, Option<&'static str>, String)> {
        let prev = status_file::parse(&prev.to_string()).unwrap_or_default();
        let e = entry_for(HookState::from_word(state_arg), event, &prev)?;
        let state = serde_json::to_value(e.state).unwrap().as_str().unwrap().to_string();
        let reason = e.reason.map(|r| match r {
            Reason::Permission => "permission",
            Reason::Question => "question",
        });
        Some((state, reason, e.message))
    }

    fn written(state_arg: &str, event: &Value, prev: &Value) -> Value {
        let (state, reason, message) = after(state_arg, event, prev).unwrap();
        let mut v = json!({ "state": state, "message": message });
        if let Some(r) = reason {
            v["reason"] = json!(r);
        }
        v
    }

    #[test]
    fn questions_wait_on_the_user_until_answered() {
        // Claude 2.1.296: a three-question AskUserQuestion fires PreToolUse, PermissionRequest at
        // once and Notification(permission_prompt) 6 s later; nothing per answer; PostToolUse
        // on submit.
        let ask = |event: &str| json!({ "hook_event_name": event, "tool_name": "AskUserQuestion" });
        let s = written("working", &ask("PreToolUse"), &Value::Null);
        assert_eq!((s["state"].as_str(), s["reason"].as_str()), (Some("needs"), Some("question")));
        let s = written("needs", &ask("PermissionRequest"), &s);
        assert_eq!(s["reason"], "question");
        let note = json!({ "hook_event_name": "Notification", "notification_type": "permission_prompt",
            "message": "Claude needs your permission" });
        let s = written("needs", &note, &s);
        assert_eq!(
            (s["reason"].as_str(), s["message"].as_str()),
            (Some("question"), Some("Claude is asking you a question"))
        );
        assert_eq!(
            after("working", &ask("PostToolUse"), &s),
            Some(("working".into(), None, String::new()))
        );

        // ExitPlanMode the same, its notification naming the plan.
        let plan = |event: &str| json!({ "hook_event_name": event, "tool_name": "ExitPlanMode" });
        let s = written("working", &plan("PreToolUse"), &Value::Null);
        let s = written("needs", &plan("PermissionRequest"), &s);
        let s = written("needs", &note, &s);
        assert_eq!(s["reason"], "question");

        // Codex's request_user_input fires PreToolUse with its own name, PostToolUse after the
        // last answer.
        let codex = json!({ "hook_event_name": "PreToolUse", "tool_name": "request_user_input" });
        assert_eq!(
            after("working", &codex, &Value::Null),
            Some(("needs".into(), Some("question"), "Codex is asking you a question".into()))
        );
    }

    #[test]
    fn permission_prompts_carry_the_command() {
        let prev = Record::default();
        let asked = |input: Value| {
            let v = json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash",
                "tool_input": input });
            entry_for(HookState::Working, &v, &prev).unwrap()
        };
        let e = asked(json!({ "command": "rm   -rf\n target", "description": "Clean" }));
        assert_eq!((e.tool.as_deref(), e.input.as_deref()), (Some("Bash"), Some("rm -rf target")));
        // Codex gives the command as words.
        let e = asked(json!({ "command": ["git", "push"] }));
        assert_eq!(e.input.as_deref(), Some("git push"));
        let e = asked(json!({ "file_path": "/etc/hosts" }));
        assert_eq!(e.input.as_deref(), Some("/etc/hosts"));
        let e = asked(json!({ "odd": 1 }));
        assert_eq!(e.input.as_deref(), Some(r#"{"odd":1}"#));
        assert_eq!(asked(Value::Null).input, None);
        let long = asked(json!({ "command": "x".repeat(2 * INPUT_KEPT) }));
        assert_eq!(long.input.unwrap().chars().count(), INPUT_KEPT);
        // The reminder notification keeps what the prompt said.
        let prev = Record {
            state: HookState::Needs,
            reason: Some(Reason::Permission),
            tool: Some("Bash".into()),
            input: Some("make".into()),
            ..Record::default()
        };
        let note = json!({ "hook_event_name": "Notification", "notification_type": "permission_prompt",
            "message": "Claude needs your permission" });
        let e = entry_for(HookState::Needs, &note, &prev).unwrap();
        assert_eq!(e.input.as_deref(), Some("make"));
    }

    #[test]
    fn permission_prompts_say_so() {
        let bash = json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash",
            "tool_input": { "command": "./build.sh", "description": "Run the build" } });
        let s = written("needs", &bash, &json!({ "state": "working" }));
        assert_eq!(
            (s["reason"].as_str(), s["message"].as_str()),
            (Some("permission"), Some("Run the build"))
        );
        // The notification that follows a prompt still showing after 6 s keeps it.
        let note = json!({ "hook_event_name": "Notification", "notification_type": "permission_prompt",
            "message": "Claude needs your permission" });
        let again = written("needs", &note, &s);
        assert_eq!(
            (again["reason"].as_str(), again["message"].as_str()),
            (Some("permission"), Some("Run the build"))
        );
        // A Claude without PermissionRequest: the notification alone.
        assert_eq!(
            after("needs", &note, &json!({ "state": "working" })),
            Some(("needs".into(), Some("permission"), "Claude needs your permission".into()))
        );
        // Codex's approval, and one without a description.
        let codex = json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": { "command": "make" } });
        assert_eq!(
            after("needs", &codex, &Value::Null).unwrap().2,
            "Waiting for your approval to use Bash"
        );
        // A prompt of a kind taskman doesn't know waits until a hook says otherwise.
        let other = json!({ "hook_event_name": "Notification", "notification_type": "elicitation_dialog", "message": "Fill in" });
        assert_eq!(after("needs", &other, &Value::Null).unwrap().1, Some("question"));
    }

    #[test]
    fn notifications_that_are_not_prompts() {
        let idle = json!({ "hook_event_name": "Notification", "notification_type": "idle_prompt",
            "message": "Claude is waiting for your input" });
        assert_eq!(after("needs", &idle, &Value::Null).unwrap().0, "done");
        let auth =
            json!({ "hook_event_name": "Notification", "notification_type": "auth_success" });
        assert_eq!(after("needs", &auth, &Value::Null), None);
    }

    #[test]
    fn compacting_keeps_the_state() {
        let compact = json!({ "hook_event_name": "SessionStart", "source": "compact" });
        let working = json!({ "state": "working", "message": "" });
        assert_eq!(
            after("idle", &compact, &working),
            Some(("working".into(), None, String::new()))
        );
        let waiting =
            json!({ "state": "needs", "reason": "permission", "message": "Run the build" });
        assert_eq!(after("idle", &compact, &waiting).unwrap().1, Some("permission"));
        assert_eq!(after("idle", &compact, &Value::Null).unwrap().0, "idle");
        let startup = json!({ "hook_event_name": "SessionStart", "source": "startup" });
        assert_eq!(after("idle", &startup, &working).unwrap().0, "idle");
    }

    #[test]
    fn lets_sessions_run_taskman_status() {
        let v: Value =
            serde_json::from_str(&settings_json(Path::new("/usr/local/bin/taskman"))).unwrap();
        assert_eq!(
            v["permissions"]["allow"],
            json!(["Bash(taskman status:*)", "Bash(taskman add:*)", "Bash(taskman move:*)"])
        );
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            "'/usr/local/bin/taskman' hook done"
        );
        assert_eq!(
            v["hooks"]["PermissionRequest"][0]["hooks"][0]["command"],
            "'/usr/local/bin/taskman' hook needs"
        );
    }

    #[test]
    fn quotes_the_binary_path_for_the_shell() {
        let v: Value =
            serde_json::from_str(&settings_json(Path::new("/home/o'neil/bin/taskman"))).unwrap();
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            r"'/home/o'\''neil/bin/taskman' hook done"
        );
        assert!(
            codex_config(Path::new("/a b/taskman"))[0]
                .contains(r#"command="'/a b/taskman' hook idle""#)
        );
    }

    #[test]
    fn codex_trust_hash_matches_codex() {
        // Computed with Codex's formula, which reproduces the hashes Codex 0.161 stored on trust.
        assert_eq!(
            codex_hook_hash("stop", "'/usr/local/bin/taskman' hook done"),
            "sha256:2a1ea95d0889e9ae2c4b8a730a664ca38e62e54d157d2793d44062ff7e62d03d"
        );
    }

    #[test]
    fn codex_config_trusts_every_hook() {
        let args = codex_config(Path::new("/usr/local/bin/taskman"));
        assert_eq!(args.len(), CODEX_HOOKS.len() + 1);
        assert_eq!(
            args[5],
            r#"hooks.Stop=[{hooks=[{type="command",command="'/usr/local/bin/taskman' hook done",timeout=3}]}]"#
        );
        let state = args.last().unwrap();
        assert!(state.starts_with("hooks.state={"));
        assert!(state.contains(
            r#""/<session-flags>/config.toml:stop:0:0"={trusted_hash="sha256:2a1ea95d0889e9ae2c4b8a730a664ca38e62e54d157d2793d44062ff7e62d03d"}"#
        ));
        assert_eq!(state.matches("trusted_hash").count(), CODEX_HOOKS.len());
    }
}
