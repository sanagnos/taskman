//! What a session's CLI is started with, as the real binary starts it: in the pane, through
//! `taskman resume` after a hand-off, and back in the pane. taskman runs as if inside Claude Code
//! and tmux, whose variables must not reach the CLI, with a stub `tmux` that records what it's
//! asked so no tmux server is involved.

mod common;

use common::{Tui, scratch_dir, script, stub_tools, stubbed};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

const SID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

/// Set in the shell taskman runs in when that shell is a Claude Code session (its identity
/// included) in Codex's sandbox, in tmux.
const NESTED: [(&str, &str); 14] = [
    ("CLAUDECODE", "1"),
    ("CLAUDE_CODE_ENTRYPOINT", "cli"),
    ("CLAUDE_CODE_SSE_PORT", "41234"),
    ("CLAUDE_CODE_CHILD_SESSION", "1"),
    ("CLAUDE_CODE_SESSION_ID", "11111111-2222-4333-8444-555555555555"),
    ("CLAUDE_CODE_MESSAGING_SOCKET", "/nonexistent/cc-socks/1.sock"),
    ("CLAUDE_CODE_MESSAGING_TOKEN", "not-a-real-token"),
    ("CLAUDE_PID", "4242"),
    ("CLAUDE_CODE_EXECPATH", "/nonexistent/claude"),
    ("CLAUDE_CODE_SESSION_ATTENDED", "1"),
    ("CODEX_SANDBOX", "seatbelt"),
    ("CODEX_SANDBOX_NETWORK_DISABLED", "1"),
    ("TMUX", "/nonexistent/tmux-test,1,0"),
    ("TMUX_PANE", "%1"),
];

/// How many of `NESTED` are a Claude Code or Codex session's; the rest are tmux's, which only the
/// pane takes out.
const SESSION_VARS: usize = 12;

/// What configures Claude Code, in the same shell: named like the session's variables, and must
/// reach the CLI.
const CONFIGURED: [(&str, &str); 2] =
    [("CLAUDE_CODE_USE_BEDROCK", "0"), ("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "8192")];

/// Project acme with ticket ENG-1 in `dir`, whose conversation `SID` has a transcript. Returns
/// the repo.
fn fixture(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    std::fs::write(repo.join("workplan/todo/ENG-1-test.md"), "# test\n").unwrap();
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let tickets = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": [{"key": "ENG-1", "repo": repo, "cwd": repo, "session_id": SID}],
    });
    std::fs::write(state.join("tickets.json"), tickets.to_string()).unwrap();
    let transcripts = dir.join("claude-config/projects/acme");
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join(format!("{SID}.jsonl")), "{}\n").unwrap();
    repo
}

/// A CLI that records each start in `dir`: `launch<N>.args` and `launch<N>.env`, NUL-separated,
/// and `launch<N>.pid`; then it waits until it's stopped.
fn recording_cli(dir: &Path) -> PathBuf {
    let text = r#"#!/bin/sh
d='DIR'
n=$(ls "$d" | grep -c '^launch[0-9]*\.args$')
perl -e 'print map { "$_\0" } sort map { "$_=$ENV{$_}" } keys %ENV' > "$d/launch$n.env"
perl -e 'print map { "$_\0" } @ARGV' -- "$@" > "$d/launch$n.part"
echo $$ > "$d/launch$n.pid"
mv "$d/launch$n.part" "$d/launch$n.args"
echo "stub ready"
exec sleep 300
"#;
    script(dir, "claude", &text.replace("DIR", &dir.display().to_string()))
}

/// A `tmux` that answers what taskman asks it and records the rest in `dir`: the buffer it's
/// given (`tmux.buffer`) and a new window's arguments (`tmux.window`, NUL-separated).
fn stub_tmux(bin: &Path, dir: &Path) {
    let text = r#"#!/bin/sh
d='DIR'
case "$1" in
  load-buffer) cat > "$d/tmux.part" && mv "$d/tmux.part" "$d/tmux.buffer";;
  show|show-options) case "$*" in *prefix*) echo C-b;; *) echo on;; esac;;
  new-window) perl -e 'print map { "$_\0" } @ARGV' -- "$@" > "$d/tmux.part" && mv "$d/tmux.part" "$d/tmux.window";;
  *) exit 1;;
esac
"#;
    script(bin, "tmux", &text.replace("DIR", &dir.display().to_string()));
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

fn env(dir: &Path, n: usize) -> BTreeMap<String, String> {
    let lines = fields(&dir.join(format!("launch{n}.env")));
    lines.iter().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect()
}

fn pid(dir: &Path, n: usize) -> i32 {
    let text = std::fs::read_to_string(dir.join(format!("launch{n}.pid"))).unwrap();
    text.trim().parse().unwrap()
}

#[test]
fn a_session_starts_the_same_in_the_pane_and_outside_and_nothing_nested_reaches_it() {
    let dir = scratch_dir("launch");
    let repo = fixture(&dir);
    let mut cmd = stubbed(&dir);
    stub_tmux(&dir.join("bin"), &dir);
    cmd.env("TASKMAN_CLAUDE", recording_cli(&dir))
        .env("CLAUDE_CONFIG_DIR", dir.join("claude-config"))
        .env("COLORTERM", "truecolor")
        .env("ACME_KEPT", "1")
        .envs(NESTED)
        .envs(CONFIGURED);
    let mut tui = Tui::start("launch", &dir, cmd, Duration::from_secs(90));

    // In the pane: resumed, with the hooks, the name and the brief.
    let pane = args(&tui, 0);
    assert_eq!(pane[..2], ["--resume", SID]);
    let settings: serde_json::Value = serde_json::from_str(&pane[3]).unwrap();
    assert_eq!(pane[2], "--settings");
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_taskman")).unwrap();
    let hook = settings["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
    assert_eq!(hook, format!("'{}' hook done", exe.display()));
    assert_eq!(pane[4..6], ["--name", "ENG-1"]);
    assert_eq!(pane[6], "--append-system-prompt");
    assert!(pane[7].starts_with("This session is dedicated to ticket ENG-1."), "{}", pane[7]);
    let vars = env(&dir, 0);
    for (k, _) in NESTED {
        assert_eq!(vars.get(k), None, "{k} reached the session");
    }
    let state = dir.join("state/taskman");
    let get = |k: &str| vars.get(k).map(String::as_str);
    assert_eq!(get("TASKMAN_TICKET"), Some("ENG-1"));
    assert_eq!(get("TASKMAN_REPO"), Some(repo.to_str().unwrap()));
    assert_eq!(get("TASKMAN_SESSION_NAME"), Some("ENG-1"));
    assert!(get("TASKMAN_STATUS_FILE").unwrap().starts_with(state.to_str().unwrap()));
    assert!(get("TASKMAN_STATUS_FILE").unwrap().ends_with("/ENG-1.json"));
    let bin = state.join("bin");
    assert!(get("PATH").unwrap().starts_with(&format!("{}:", bin.display())));
    assert_eq!(std::fs::canonicalize(bin.join("taskman")).unwrap(), exe);
    assert_eq!((get("TERM"), get("COLORTERM")), (Some("xterm-256color"), Some("truecolor")));
    assert_eq!(get("ACME_KEPT"), Some("1"), "the rest of the environment is passed on");
    for (k, v) in CONFIGURED {
        assert_eq!(get(k), Some(v), "{k} configures the CLI and must reach it");
    }
    assert_eq!(get("CLAUDE_CONFIG_DIR"), dir.join("claude-config").to_str());

    // Handed off: the CLI is asked to quit, the command that continues it copied, and a tmux
    // window offered, which runs that command with this binary and this state.
    tui.send(b"\x01t");
    tui.wait_text("Continue ENG-1 in a new tmux window?");
    let buffer = dir.join("tmux.buffer");
    tui.wait_until("nothing copied", || buffer.exists());
    assert_eq!(std::fs::read_to_string(&buffer).unwrap(), "taskman resume ENG-1");
    tui.send(b"\r");
    let window = dir.join("tmux.window");
    tui.wait_until("no tmux window opened", || window.exists());
    let window = fields(&window);
    assert_eq!(window[..5], ["new-window", "-n", "ENG-1", "-c", repo.to_str().unwrap()]);
    let line = format!("{} resume ENG-1", bin.join("taskman").display());
    assert_eq!(window.last(), Some(&line));
    for var in [
        format!("XDG_STATE_HOME={}", dir.join("state").display()),
        format!("TASKMAN_CONFIG={}", dir.join("config.json").display()),
        format!("CLAUDE_CONFIG_DIR={}", dir.join("claude-config").display()),
        "TASKMAN_NO_AUTOUPDATE=1".to_string(),
    ] {
        let at = window.iter().position(|a| *a == var);
        assert!(at.is_some_and(|i| window[i - 1] == "-e"), "{var} not passed: {window:?}");
    }
    tui.wait_text("ENG-1 is handed off");

    // `taskman resume` starts the same, from another shell in Claude Code.
    let mut resume = common::taskman(&dir);
    resume
        .args(["resume", "ENG-1"])
        .current_dir(&repo)
        .env("TASKMAN_CLAUDE", dir.join("claude"))
        .env("CLAUDE_CONFIG_DIR", dir.join("claude-config"))
        .envs(NESTED[..SESSION_VARS].iter().copied())
        .envs(CONFIGURED)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let resume = resume.spawn().unwrap();
    assert_eq!(args(&tui, 1), pane, "resumed outside with other arguments");
    let outside = env(&dir, 1);
    for k in ["TASKMAN_TICKET", "TASKMAN_REPO", "TASKMAN_SESSION_NAME", "TASKMAN_STATUS_FILE"] {
        assert_eq!(outside.get(k), vars.get(k), "{k}");
    }
    assert!(outside["PATH"].starts_with(&format!("{}:", bin.display())));
    for (k, _) in &NESTED[..SESSION_VARS] {
        assert_eq!(outside.get(*k), None, "{k} reached the session outside");
    }
    for (k, v) in CONFIGURED {
        assert_eq!(outside.get(k).map(String::as_str), Some(v), "{k} didn't reach it outside");
    }
    tui.wait_text("ENG-1 is running in another terminal");

    // It ends there: back in the pane with Enter, as it started.
    // SAFETY: sends a signal; no memory is involved.
    unsafe { libc::kill(pid(&dir, 1), libc::SIGTERM) };
    let out = resume.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("ENG-1 is back. Select it in taskman and press Enter"), "{said}");
    tui.wait_text("ENG-1 is back");
    tui.send(b"\r");
    assert_eq!(args(&tui, 2), pane, "brought back with other arguments");

    // Quit as a user would, so a coverage run counts what this binary ran.
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

/// A CLI that isn't there doesn't pass for a session that ended: the pane says it didn't start
/// and what to do, and once it's installed Enter starts it.
#[test]
fn a_cli_that_cannot_start_says_so() {
    let dir = scratch_dir("launch-missing");
    fixture(&dir);
    let mut cmd = stubbed(&dir);
    // Looked for on PATH, which starts with the stub tools' folder.
    cmd.env("TASKMAN_CLAUDE", "acme-cli");
    let mut tui = Tui::start("launch-missing", &dir, cmd, Duration::from_secs(60));
    tui.wait_text("The Claude session for ENG-1 didn't start.");
    tui.wait_text("acme-cli isn't installed");
    assert!(!tui.text().contains("has ended"), "{}", tui.text());

    std::fs::copy(dir.join("claude"), stub_tools(&dir).join("acme-cli")).unwrap();
    tui.send(b"\r");
    tui.wait_text("stub ready for ENG-1");
    tui.quit();
}
