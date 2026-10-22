//! The programs taskman runs for itself (gh here; git and token commands the same way) can't reach
//! the terminal taskman draws on: one that asks there, as ssh does for a key's passphrase, fails at
//! once rather than writing over the screen or waiting on keys the TUI reads.

mod common;

use common::{Tui, project, scratch_dir, script, sh, stub_tools, stubbed};
use std::path::Path;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// A gh that asks for a passphrase on the terminal, as ssh does, and notes in `dir/tty` whether it
/// could open it.
fn asking_gh(dir: &Path) {
    let text = r#"#!/bin/sh
if [ "$1 $2" = "auth token" ]; then
  if { printf 'Enter passphrase for key: ' > /dev/tty; } 2>/dev/null; then
    echo reachable > 'DIR/tty.part'
  else
    echo unreachable > 'DIR/tty.part'
  fi
  mv 'DIR/tty.part' 'DIR/tty'
fi
echo 'gh stub: no answer' >&2
exit 1
"#;
    script(&stub_tools(dir), "gh", &text.replace("DIR", &dir.display().to_string()));
}

#[test]
fn a_helper_cannot_reach_the_terminal() {
    let dir = scratch_dir("helpers");
    let repo = project(&dir, &["ENG-52"]);
    sh(&repo, "git init -q && git remote add origin https://github.com/acme/web.git");
    asking_gh(&dir);
    let mut cmd = stubbed(&dir);
    cmd.env("GIT_ALLOW_PROTOCOL", "file");
    let mut tui = Tui::start("a_helper_cannot_reach_the_terminal", &dir, cmd, LIMIT);
    tui.wait_text("stub ready for ENG-52");

    tui.step("gh asks on the terminal");
    let tty = dir.join("tty");
    tui.wait_until("gh asked", || tty.exists());
    assert_eq!(std::fs::read_to_string(&tty).unwrap(), "unreachable\n");
    assert!(
        !tui.text().contains("Enter passphrase"),
        "the prompt reached the screen:\n{}",
        tui.text()
    );
    tui.quit();
}
