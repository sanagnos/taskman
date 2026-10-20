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
