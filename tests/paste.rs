//! A long paste into a session whose CLI reads slowly, and asks the terminal where its cursor is
//! each time it does, holds nothing up: taskman goes on taking keys while the paste is written,
//! and quits as usual. The UI thread used to write the paste itself, and the session's reader,
//! answering the queries, waited on it, so taskman froze until the CLI had read it all.

mod common;

use common::{Tui, project, scratch_dir, script, taskman};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(90);

/// A CLI that turns on bracketed paste, then reads its input 256 bytes at a time, 20 times a
/// second, into `<KEY>.input`, asking for the cursor's position (`ESC[6n`) after each read.
fn slow_cli(dir: &Path) -> PathBuf {
    let text = r#"#!/bin/sh
f="DIR/$TASKMAN_TICKET"
echo $$ > "$f.pid"
stty raw -echo
printf '\033[?2004hslow reader for %s\r\n' "$TASKMAN_TICKET"
while :; do
  dd bs=256 count=1 2>/dev/null >> "$f.input"
  printf '\033[6n'
  sleep 0.05
done
"#;
    script(dir, "claude", &text.replace("DIR", &dir.display().to_string()))
}

fn read(dir: &Path, key: &str) -> Vec<u8> {
    std::fs::read(dir.join(format!("{key}.input"))).unwrap_or_default()
}

#[test]
fn a_long_paste_to_a_slow_cli_holds_nothing_up() {
    let dir = scratch_dir("paste");
    project(&dir, &["ENG-1", "ENG-2"]);
    let mut cmd = taskman(&dir);
    cmd.env("TASKMAN_CLAUDE", slow_cli(&dir));
    let mut tui = Tui::start("a_long_paste_to_a_slow_cli_holds_nothing_up", &dir, cmd, LIMIT);
    tui.wait_text("slow reader for ENG-1");

    // 100 KB takes the CLI 20 seconds to read.
    tui.step("pasting");
    let line = format!("{}\n", "x".repeat(99));
    let paste = format!("\x1b[200~{}\x1b[201~", line.repeat(1000));
    tui.send(paste.as_bytes());
    tui.wait_until("the paste didn't start reaching the CLI", || {
        common::contains(&read(&dir, "ENG-1"), b"\x1b[200~")
    });

    tui.step("switching to ENG-2");
    let start = Instant::now();
    tui.send(b"\x012");
    tui.wait_text("slow reader for ENG-2");
    assert!(start.elapsed() < Duration::from_secs(5), "ENG-2 took {:?}", start.elapsed());
    assert!(
        !common::contains(&read(&dir, "ENG-1"), b"\x1b[201~"),
        "the CLI read the whole paste first: it doesn't read slowly enough to show anything"
    );

    tui.step("quitting");
    tui.quit();
}
