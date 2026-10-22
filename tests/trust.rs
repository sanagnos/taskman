//! A session waiting on its CLI's question about trusting its folder, which comes before any hook,
//! shows as needing you; answered, the hooks report it again.

mod common;

use common::{Tui, scratch_dir, script, taskman};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A CLI that asks to trust its folder in Claude Code's words, and once a line is typed reports a
/// SessionStart through the real hook, as Claude does after the question.
fn asking_cli(dir: &Path) -> PathBuf {
    let text = r#"#!/bin/sh
printf ' Quick safety check: Is this a project you created or one you trust?\n\n'
printf ' > No, exit\n   Yes, I trust this folder\n\n Enter to confirm\n'
IFS= read -r line
printf '\033[2J\033[H'
printf '{"hook_event_name":"SessionStart","source":"startup","session_id":"sid-%s"}' $$ | taskman hook idle
echo "stub ready for $TASKMAN_TICKET"
while :; do sleep 0.1; done
"#;
    script(dir, "claude", text)
}

/// `key`'s row of the sidebar, which is 30 columns wide on the test terminal.
fn sidebar_row(tui: &Tui, key: &str) -> String {
    let text = tui.text();
    let mut rows = text.lines().map(|l| l.chars().take(30).collect::<String>());
    rows.find(|r| r.contains(key)).unwrap_or_default()
}

#[test]
fn a_session_asking_to_trust_its_folder_needs_you_until_answered() {
    let dir = scratch_dir("trust");
    common::project(&dir, &["ENG-1"]);
    let mut cmd = taskman(&dir);
    cmd.env("TASKMAN_CLAUDE", asking_cli(&dir)).env("CLAUDE_CONFIG_DIR", dir.join("claude-config"));
    let mut tui = Tui::start("trust", &dir, cmd, Duration::from_secs(60));
    tui.wait_text("Yes, I trust this folder");
    tui.wait_until("the question didn't show as needing you", || {
        sidebar_row(&tui, "ENG-1").contains('▲')
    });
    tui.send(b"\x01w");
    tui.wait_text("asks to trust its folder");
    tui.send(b"\r");
    tui.wait_until("the queue didn't close", || !tui.text().contains("Waiting on you"));

    // Answered: the CLI goes on, and its hook reports it idle.
    tui.send(b"\r");
    tui.wait_text("stub ready for ENG-1");
    tui.wait_until("the session didn't go back to idle", || {
        let row = sidebar_row(&tui, "ENG-1");
        row.contains('○') && !row.contains('▲')
    });
    tui.quit();
}
