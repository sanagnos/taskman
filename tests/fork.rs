//! Forking an agent with `prefix f`, as the real binary does it: the CLI is asked its version, the
//! fork starts with a copy of the source's conversation and a first message saying who it is now,
//! takes the id its hooks report, and a restart resumes that id, never the source's. For Claude
//! Code, which takes the fork's id up front, and Codex, which makes its own.

mod common;

use common::{Tui, scratch_dir, script, stubbed};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

const SOURCE: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
/// The id a Codex fork makes for itself, which its hooks report.
const CODEX_FORK: &str = "ffffffff-1111-4222-8333-444444444444";

/// Project acme in `dir` with agent `lead` on `cli`, whose conversation `SOURCE` has a transcript
/// where that CLI keeps them. Returns the repo.
fn fixture(dir: &Path, cli: &str) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [{"key": "acme-lead", "repo": repo, "cwd": repo, "agent": "lead", "cli": cli,
                     "session_id": SOURCE}],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    transcript(dir, cli, SOURCE);
    repo
}

/// Leaves a transcript for conversation `sid` where `cli` keeps them.
fn transcript(dir: &Path, cli: &str, sid: &str) {
    let path = match cli {
        "claude" => dir.join(format!("cli-home/projects/acme/{sid}.jsonl")),
        _ => dir
            .join(format!("cli-home/sessions/2026/10/30/rollout-2026-10-30T10-00-00-{sid}.jsonl")),
    };
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "{}\n").unwrap();
}

/// A CLI that answers `--version` with `version` and `--help` with Claude's option to turn off
/// the system prompt's record, and otherwise records each start in `dir` (`launch<N>.args` and
/// `launch<N>.env`, NUL-separated) and waits until it's stopped.
fn recording_cli(dir: &Path, name: &str, version: &str) -> PathBuf {
    let text = r#"#!/bin/sh
d='DIR'
case "$1" in
  --version) echo 'VERSION'; exit 0;;
  --help) echo '  --system-prompt-snapshot <on|off>   Record the system prompt'; exit 0;;
esac
n=$(ls "$d" | grep -c '^launch[0-9]*\.args$')
perl -e 'print map { "$_\0" } sort map { "$_=$ENV{$_}" } keys %ENV' > "$d/launch$n.env"
perl -e 'print map { "$_\0" } @ARGV' -- "$@" > "$d/launch$n.part"
mv "$d/launch$n.part" "$d/launch$n.args"
echo "stub ready"
exec sleep 300
"#;
    let text = text.replace("DIR", &dir.display().to_string()).replace("VERSION", version);
    script(dir, name, &text)
}

fn fields(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap();
    text.split_terminator('\0').map(str::to_string).collect()
}

/// Launch `n`'s arguments, once the CLI has written them.
fn args(tui: &Tui, n: usize) -> Vec<String> {
    let path = tui.dir.join(format!("launch{n}.args"));
    tui.wait_until(&format!("launch {n} not recorded"), || path.exists());
    fields(&path)
}

/// The argument after `flag`.
fn after<'a>(args: &'a [String], flag: &str) -> &'a str {
    let i = args.iter().position(|a| a == flag).unwrap_or_else(|| panic!("no {flag}: {args:?}"));
    &args[i + 1]
}

/// Runs the hook launch `n`'s session runs when a turn ends, reporting conversation `sid`.
fn turn_ends(dir: &Path, n: usize, sid: &str) {
    let mut hook = common::taskman(dir);
    hook.args(["hook", "done"]).stdin(Stdio::piped()).stdout(Stdio::null());
    for var in fields(&dir.join(format!("launch{n}.env"))) {
        if let Some((k, v)) = var.split_once('=')
            && k.starts_with("TASKMAN_")
        {
            hook.env(k, v);
        }
    }
    let mut child = hook.spawn().unwrap();
    let event = serde_json::json!({ "hook_event_name": "Stop", "session_id": sid });
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), event.to_string().as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

/// The agent `key` as tickets.json has it.
fn saved(dir: &Path, key: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join("state/taskman/tickets.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    saved["tickets"].as_array().unwrap().iter().find(|t| t["key"] == key).unwrap().clone()
}

fn forks(cli: &str) {
    let dir = scratch_dir(&format!("fork-{cli}"));
    let repo = fixture(&dir, cli);
    let (program, var, home, version) = match cli {
        "claude" => ("claude", "TASKMAN_CLAUDE", "CLAUDE_CONFIG_DIR", "2.1.314 (Claude Code)"),
        _ => ("codex", "TASKMAN_CODEX", "CODEX_HOME", "codex-cli 0.166.0"),
    };
    let mut cmd = stubbed(&dir);
    cmd.env(var, recording_cli(&dir, program, version)).env(home, dir.join("cli-home"));
    let mut tui = Tui::start("fork", &dir, cmd, Duration::from_secs(90));
    // The lead resumes its own conversation.
    let lead = args(&tui, 0);
    assert!(lead.iter().any(|a| a == SOURCE), "{lead:?}");
    tui.wait_text("stub ready");

    tui.send(b"\x01f");
    tui.wait_text("Fork of acme-lead, as of now");
    tui.send(b"deep-dive\r");
    let fork = args(&tui, 1);
    let message = fork.last().unwrap();
    assert!(
        message.starts_with("You're now acme-deep-dive, a new agent forked from acme-lead"),
        "{message}"
    );
    let id = match cli {
        "claude" => {
            assert_eq!(fork[..3], ["--resume", SOURCE, "--fork-session"]);
            assert_eq!(after(&fork, "--system-prompt-snapshot"), "off");
            assert_eq!(after(&fork, "--name"), "acme-deep-dive");
            assert!(message.contains("Your session name for messages is acme-deep-dive"));
            let id = after(&fork, "--session-id").to_string();
            assert!(id.len() == 36 && id != SOURCE, "{id}");
            id
        }
        _ => {
            let n = fork.len();
            assert_eq!(fork[n - 5..n - 1], ["fork", SOURCE, "-C", repo.to_str().unwrap()]);
            CODEX_FORK.to_string()
        }
    };
    tui.wait_text("Forked acme-lead into ✦ deep-dive");

    // Its start may report the source's conversation first: not the fork's to take.
    turn_ends(&dir, 1, SOURCE);
    turn_ends(&dir, 1, &id);
    transcript(&dir, cli, &id);
    tui.wait_until("the fork's id not saved", || {
        let t = saved(&dir, "acme-deep-dive");
        t["session_id"] == id.as_str() && t.get("fork").is_none()
    });
    assert!(!tui.text().contains("switched"), "{}", tui.text());

    // A restart resumes the fork's own conversation, as any agent's.
    tui.send(b"\x01R");
    let restarted = args(&tui, 2);
    assert!(restarted.contains(&id), "{restarted:?}");
    assert!(!restarted.iter().any(|a| a == SOURCE || a == "--fork-session" || a == "fork"));
    assert!(!restarted.last().unwrap().starts_with("You're now"), "{restarted:?}");
    // The source keeps its own.
    assert_eq!(saved(&dir, "acme-lead")["session_id"], SOURCE);

    tui.quit();
}

#[test]
fn a_claude_agent_forks_and_the_fork_keeps_its_own_conversation() {
    forks("claude");
}

#[test]
fn a_codex_agent_forks_and_the_fork_keeps_its_own_conversation() {
    forks("codex");
}
