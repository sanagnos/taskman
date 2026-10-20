//! A session that starts waiting on you while the terminal says taskman isn't focused (a focus
//! report, CSI O) shows a desktop notification through the platform's command (a stub here), and
//! through the terminal (OSC 9) when no command shows it. While taskman has the focus, none. The
//! notification says the ticket and why it waits, never the CLI's words.

mod common;

use common::{NOTIFIERS, Tui, notified, scratch_dir, script, taskman};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A CLI whose prompts are typed: `ask N` reports a permission prompt through the real hook, its
/// command holding an escape of its own, `work N` a new turn; each then prints `done: <line>`.
fn asking_cli(dir: &Path) -> PathBuf {
    let text = r#"#!/bin/sh
echo "stub ready for $TASKMAN_TICKET"
while IFS= read -r line; do
  case "$line" in
    ask*) printf '{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"echo \\u001b]9;from the CLI\\u0007"}}' | taskman hook needs;;
    work*) printf '{"hook_event_name":"UserPromptSubmit"}' | taskman hook working;;
  esac
  echo "done: $line"
done
"#;
    script(dir, "claude", text)
}

fn start(name: &'static str, dir: &Path) -> Tui {
    common::project(dir, &["ENG-1"]);
    let mut cmd = taskman(dir);
    cmd.env("TASKMAN_CLAUDE", asking_cli(dir))
        .env("CLAUDE_CONFIG_DIR", dir.join("claude-config"))
        // So Linux without a desktop still has notify-send to try.
        .env("DISPLAY", ":99");
    let tui = Tui::start(name, dir, cmd, Duration::from_secs(60));
    tui.wait_text("stub ready for ENG-1");
    tui
}

/// Types `line` into the session and waits for the CLI to have done it.
fn type_line(tui: &mut Tui, line: &str) {
    tui.send(format!("{line}\r").as_bytes());
    tui.wait_text(&format!("done: {line}"));
}

/// Waits until the status poll has read the session's state, as the sidebar's card shows it.
fn wait_state(tui: &Tui, word: &str) {
    tui.wait_text(word);
}

const SAYS: &str = "ENG-1 needs you: a permission prompt";

#[test]
fn a_prompt_while_you_are_away_notifies_through_the_desktop() {
    let dir = scratch_dir("notify-desktop");
    let mut tui = start("a_prompt_while_you_are_away_notifies_through_the_desktop", &dir);

    // Focused: the screen shows it, no notification.
    tui.send(b"\x1b[I");
    type_line(&mut tui, "ask 1");
    wait_state(&tui, "needs you");
    type_line(&mut tui, "work 1");
    wait_state(&tui, "working");
    assert_eq!(notified(&dir), "");

    // Away: one notification, saying the ticket and why.
    tui.send(b"\x1b[O");
    type_line(&mut tui, "ask 2");
    tui.wait_until("the notification", || !notified(&dir).is_empty());
    let shown = notified(&dir);
    assert_eq!(shown.lines().count(), 1, "{shown}");
    assert!(shown.trim_end().ends_with(SAYS), "{shown}");
    assert!(!shown.contains("from the CLI"), "{shown}");
    // A desktop command showed it: nothing for the terminal.
    assert!(!tui.with(|t| common::contains(&t.raw, b"\x1b]9;")));
    tui.quit();
}

#[test]
fn without_a_desktop_command_the_terminal_shows_it() {
    let dir = scratch_dir("notify-terminal");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in NOTIFIERS {
        script(&bin, name, "#!/bin/sh\nexit 1\n");
    }
    let mut tui = start("without_a_desktop_command_the_terminal_shows_it", &dir);

    tui.send(b"\x1b[O");
    type_line(&mut tui, "ask 1");
    let osc = format!("\x1b]9;taskman · {SAYS}\x07");
    tui.wait_until("the terminal's notification", || {
        tui.with(|t| common::contains(&t.raw, osc.as_bytes()))
    });
    tui.quit();
}

/// A Codex whose approvals are typed: `review N` fires the PermissionRequest hook and draws
/// Codex's reviewer's status line for 3 s, as its reviewer deciding does; `ran N` fires
/// PostToolUse, as the approved command ending does; `ask N` fires the hook and draws Codex's
/// prompt. Each then prints `done: <line>`.
fn reviewing_codex(dir: &Path) -> PathBuf {
    let text = r#"#!/bin/sh
echo "stub ready for $TASKMAN_TICKET"
hook() { printf '{"hook_event_name":"%s","tool_name":"Bash","tool_input":{"command":"cargo test"}}' "$1" | taskman hook "$2"; }
while IFS= read -r line; do
  case "$line" in
    review*)
      printf '• Reviewing approval request (0s • esc to interrupt)'
      hook PermissionRequest needs
      i=1; while [ $i -le 15 ]; do
        sleep 0.2; printf '\r• Reviewing approval request (%ds • esc to interrupt)' $((i / 5)); i=$((i + 1))
      done
      printf '\r\033[K• Working (3s • esc to interrupt)\n';;
    ran*) hook PostToolUse working;;
    ask*) hook PermissionRequest needs; printf '  Press enter to confirm or esc to cancel\n';;
  esac
  echo "done: $line"
done
"#;
    script(dir, "codex", text)
}

#[test]
fn an_approval_codexs_reviewer_decides_never_notifies() {
    let dir = scratch_dir("notify-reviewer");
    common::project(&dir, &["ENG-1"]);
    let saved = dir.join("state/taskman/tickets.json");
    let text = std::fs::read_to_string(&saved).unwrap();
    assert!(text.contains(r#""key":"ENG-1""#), "{text}");
    std::fs::write(&saved, text.replace(r#""key":"ENG-1""#, r#""key":"ENG-1","cli":"codex""#))
        .unwrap();
    let mut cmd = taskman(&dir);
    cmd.env("TASKMAN_CODEX", reviewing_codex(&dir)).env("DISPLAY", ":99");
    let mut tui = Tui::start(
        "an_approval_codexs_reviewer_decides_never_notifies",
        &dir,
        cmd,
        Duration::from_secs(60),
    );
    tui.wait_text("stub ready for ENG-1");
    tui.send(b"\x1b[O");

    // Away while the reviewer decides: working all along, and no notification.
    tui.send(b"review 1\r");
    tui.wait_text("Reviewing approval request");
    tui.wait_until("the review's end", || {
        let screen = tui.text();
        assert!(!screen.contains("needs you"), "{screen}");
        assert_eq!(notified(&dir), "");
        screen.contains("done: review 1")
    });
    assert!(tui.text().contains("working"), "{}", tui.text());
    type_line(&mut tui, "ran 1");
    assert_eq!(notified(&dir), "");

    // Codex's prompt for the user: needs you, and the notification.
    type_line(&mut tui, "ask 1");
    wait_state(&tui, "needs you");
    tui.wait_until("the notification", || !notified(&dir).is_empty());
    let shown = notified(&dir);
    assert_eq!(shown.lines().count(), 1, "{shown}");
    assert!(shown.trim_end().ends_with(SAYS), "{shown}");
    tui.quit();
}
