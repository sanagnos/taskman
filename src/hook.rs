//! `taskman hook <state>`: run by the Claude Code and Codex hooks in sessions taskman launched.
//! Records the session's state and id to the ticket's status file, which the TUI polls.
//! It must never block or fail loudly, so every error is swallowed.

use crate::status_file::{self, HookState, Reason, Record};
use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;

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
    // what asked for it. `taskman close` is left out on purpose: it stops sessions and removes
    // worktrees, so the session asks the user first.
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
