//! The hooks' status files when a SessionEnd hook fires late: a CLI runs it on the hangup that
//! stops it, after taskman has removed the file. Closed and added again, the ticket doesn't take
//! the old session's end for its own, or resume the old conversation; restarted (`prefix R`), the
//! new session doesn't show as ended.

mod common;

use common::{Tui, scratch_dir, script, taskman};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A CLI that reports a conversation of its own through the real hooks, as Claude does: a
/// SessionStart as it starts, with the id it was told to resume or a new one, and a SessionEnd on
/// the hangup. The SessionEnd hook is a process of its own, as Claude's is, so it outlives the
/// kill that follows the hangup a moment later when taskman drops a session; it writes once the
/// test lets it (`<KEY>.release`), or gives up. The CLI notes its arguments, then its pid, in
/// `dir`.
fn hooked_cli(dir: &Path) -> PathBuf {
    let text = r#"#!/bin/sh
d='DIR'; t="$TASKMAN_TICKET"
sid="sid-$$"; prev=
for a in "$@"; do [ "$prev" = --resume ] && sid=$a; prev=$a; done
perl -e 'print map { "$_\0" } @ARGV' -- "$@" > "$d/$t.part" && mv "$d/$t.part" "$d/$t.args"
echo $$ > "$d/$t.pid"
printf '{"hook_event_name":"SessionStart","source":"startup","session_id":"%s"}' "$sid" | taskman hook idle
ended() {
  # Ignored before the fork, so the hook inherits that: the shell exits at once, and the hangup
  # that sends to its process group would end a child that hadn't set it yet.
  trap '' HUP
  (
    i=0; while [ ! -e "$d/$t.release" ] && [ $i -lt 300 ]; do sleep 0.05; i=$((i + 1)); done
    [ -e "$d/$t.release" ] || exit 0
    printf '{"hook_event_name":"SessionEnd","reason":"other","session_id":"%s"}' "$sid" | taskman hook ended
  ) &
  exit 0
}
trap ended HUP
echo "stub ready for $t"
while :; do sleep 0.1; done
"#;
    script(dir, "claude", &text.replace("DIR", &dir.display().to_string()))
}

/// Project acme with tickets ENG-1 and ENG-2 in `dir`, ENG-1 selected, run by `hooked_cli`.
/// Returns the repo.
fn start(name: &'static str, dir: &Path) -> (PathBuf, Tui) {
    let repo = common::project(dir, &["ENG-1", "ENG-2"]);
    let mut cmd = taskman(dir);
    cmd.env("TASKMAN_CLAUDE", hooked_cli(dir)).env("CLAUDE_CONFIG_DIR", dir.join("claude-config"));
    let tui = Tui::start(name, dir, cmd, Duration::from_secs(90));
    (repo, tui)
}

/// The pid of the CLI last started for `key`.
fn pid(dir: &Path, key: &str) -> Option<i32> {
    std::fs::read_to_string(dir.join(format!("{key}.pid"))).ok()?.trim().parse().ok()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// What the CLI last started for `key` was started with.
fn args(dir: &Path, key: &str) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(format!("{key}.args"))).unwrap_or_default();
    text.split_terminator('\0').map(str::to_string).collect()
}

/// The conversation tickets.json records for `key`.
fn saved_sid(dir: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("state/taskman/tickets.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let t = v["tickets"].as_array()?.iter().find(|t| t["key"] == key)?;
    t["session_id"].as_str().map(str::to_string)
}

/// `key`'s status file as the hooks last wrote it.
fn status(dir: &Path, key: &str) -> String {
    let folders = std::fs::read_dir(dir.join("state/taskman/status")).into_iter().flatten();
    folders
        .flatten()
        .find_map(|d| std::fs::read_to_string(d.path().join(format!("{key}.json"))).ok())
        .unwrap_or_default()
}

/// Leaves a transcript for conversation `sid`, so the CLI would resume it.
fn transcript(dir: &Path, sid: &str) {
    let folder = dir.join("claude-config/projects/acme");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(format!("{sid}.jsonl")), "{}\n").unwrap();
}

/// `key`'s row of the sidebar, which is 30 columns wide on the test terminal.
fn sidebar_row(tui: &Tui, key: &str) -> String {
    let text = tui.text();
    let mut rows = text.lines().map(|l| l.chars().take(30).collect::<String>());
    rows.find(|r| r.contains(key)).unwrap_or_default()
}

/// Lets the SessionEnd hook of `key`'s old CLI, `old`, write, and gives the TUI time to have read
/// it: it reads status files every 300 ms, and what it mustn't do with this one shows nowhere.
fn let_end(tui: &Tui, dir: &Path, key: &str, old: i32) {
    tui.wait_until("the old CLI didn't end", || !alive(old));
    std::fs::write(dir.join(format!("{key}.release")), "").unwrap();
    tui.wait_until("the old session's end wasn't written", || {
        status(dir, key).contains("\"ended\"")
    });
    std::thread::sleep(Duration::from_millis(1000));
}

fn quit(tui: &mut Tui) {
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

#[test]
fn a_closed_tickets_late_session_end_is_not_the_reopened_tickets() {
    let dir = scratch_dir("hooks-close");
    let (repo, mut tui) = start("hooks-close", &dir);
    tui.wait_text("stub ready for ENG-1");
    let old = pid(&dir, "ENG-1").unwrap();
    let old_sid = format!("sid-{old}");
    tui.wait_until("ENG-1's conversation wasn't saved", || {
        saved_sid(&dir, "ENG-1").as_deref() == Some(old_sid.as_str())
    });
    transcript(&dir, &old_sid);
    // ENG-2 selected, so ENG-1 isn't when it comes back.
    tui.send(b"\x01]");
    tui.wait_text("stub ready for ENG-2");

    // Closed and added again from a shell, as an agent would do it, before the old CLI's
    // SessionEnd hook runs.
    let run = |args: &[&str]| {
        let out = taskman(&dir).args(args).current_dir(&repo).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    run(&["close", "ENG-1"]);
    run(&["add", "ENG-1"]);
    tui.wait_until("ENG-1 wasn't added again", || sidebar_row(&tui, "ENG-1").contains("new"));
    let_end(&tui, &dir, "ENG-1", old);
    let row = sidebar_row(&tui, "ENG-1");
    assert!(row.contains("new") && !row.contains('■'), "{row}");
    assert_eq!(saved_sid(&dir, "ENG-1"), None, "the old conversation became the new ticket's");

    // Selected, it starts a conversation of its own.
    tui.send(b"\x01]");
    tui.wait_until("ENG-1 didn't start", || pid(&dir, "ENG-1").is_some_and(|p| p != old));
    let new = pid(&dir, "ENG-1").unwrap();
    assert!(!args(&dir, "ENG-1").iter().any(|a| a == "--resume"), "{:?}", args(&dir, "ENG-1"));
    tui.wait_until("the new conversation wasn't saved", || {
        saved_sid(&dir, "ENG-1").as_deref() == Some(format!("sid-{new}").as_str())
    });
    quit(&mut tui);
}

#[test]
fn a_restarted_sessions_late_end_does_not_show_the_new_one_as_ended() {
    let dir = scratch_dir("hooks-restart");
    let (_repo, mut tui) = start("hooks-restart", &dir);
    tui.wait_text("stub ready for ENG-1");
    let old = pid(&dir, "ENG-1").unwrap();
    let sid = format!("sid-{old}");
    tui.wait_until("ENG-1's conversation wasn't saved", || {
        saved_sid(&dir, "ENG-1").as_deref() == Some(sid.as_str())
    });
    transcript(&dir, &sid);

    // Restarted: the new CLI resumes the same conversation, so the old one's end names it too.
    tui.send(b"\x01R");
    tui.wait_until("ENG-1 didn't restart", || pid(&dir, "ENG-1").is_some_and(|p| p != old));
    assert_eq!(args(&dir, "ENG-1")[..2], ["--resume".to_string(), sid.clone()]);
    tui.wait_text("stub ready for ENG-1");
    let second_start = format!("\"launch\":\"{}-2\"", tui.child.id());
    tui.wait_until("the new start didn't report", || status(&dir, "ENG-1").contains(&second_start));
    let_end(&tui, &dir, "ENG-1", old);
    let row = sidebar_row(&tui, "ENG-1");
    assert!(row.contains('○') && !row.contains('■'), "the new session shows as ended: {row}");
    assert!(tui.text().contains("stub ready for ENG-1"), "{}", tui.text());
    quit(&mut tui);
}

/// The real command, given a hook's input as a CLI gives it: a message from another session and a
/// send each write a line beside the status file, and a Codex prompt writes none.
#[test]
fn the_hook_logs_messages_from_recorded_input() {
    use std::io::Write as _;
    use std::process::Stdio;
    let dir = scratch_dir("hook-log-messages");
    let status = dir.join("status/acme/ENG-52.json");
    let log = status.with_extension("events.jsonl");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks");
    let feed = |file: &str, state: &str| {
        let input = std::fs::read(fixtures.join(file)).unwrap();
        let mut child = taskman(&dir)
            .args(["hook", state])
            .env("TASKMAN_STATUS_FILE", &status)
            .env("TASKMAN_CWD", &dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&input).unwrap();
        assert!(child.wait().unwrap().success());
    };
    feed("codex-user-prompt.json", "working");
    feed("codex-bash.json", "working");
    assert!(!log.exists(), "nothing to log yet");
    feed("claude-received.json", "working");
    feed("claude-send.json", "working");
    feed("claude-send-failed.json", "working");
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains(r#""dir":"in""#) && lines[0].contains("Fix the lint failure."));
    assert!(lines[1].contains(r#""dir":"out""#) && lines[1].contains("Rebase on main"));
    assert!(!text.contains("Then push") && !text.contains("The base is about to go"), "{text}");
}
