//! Reading a session's whole history, as `prefix l` and `prefix v` do, leaves taskman's memory as it
//! was, against the binary in a pty with a stub session. The history is read a screenful at a time
//! by scrolling, and vt100 grew the rows in view to the pane's width and kept them so: 10,000 short
//! rows took 20 MB more for good. Linux only, where the resident memory is in /proc.
#![cfg(target_os = "linux")]

mod common;

use common::{Tui, find, project, scratch_dir, script, stubbed};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// What reading the history may add, in kB: under 0.5 MB in practice.
const SLACK: u64 = 4 * 1024;

/// taskman's resident memory, in kB.
fn resident(tui: &Tui) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{}/status", tui.child.id())).unwrap();
    let line = status.lines().find(|l| l.starts_with("VmRSS:")).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
fn reading_the_history_leaves_the_memory_as_it_was() {
    let dir = scratch_dir("history-memory");
    project(&dir, &["ENG-1"]);
    let edited = dir.join("edited.txt");
    let editor = script(&dir, "editor", &format!("#!/bin/sh\ncp \"$1\" '{}'\n", edited.display()));
    let mut cmd = stubbed(&dir);
    cmd.env("EDITOR", editor).env_remove("VISUAL");
    let mut tui = Tui::start("reading_the_history_leaves_the_memory_as_it_was", &dir, cmd, LIMIT);
    tui.wait_text("stub ready for ENG-1");
    tui.send(b"lines 10000\r");
    tui.wait_text("printed 10000");
    let before = resident(&tui);

    // Listing the links, which reads every row.
    tui.send(b"\x01l");
    let listed = tui.wait_for("the links listed", |s| {
        find(s, "No links in this session").map(|_| false).or(find(s, "╭ Links").map(|_| true))
    });
    if listed {
        tui.send(b"\x1b");
        tui.wait_for("the list closed", |s| find(s, "╭ Links").is_none().then_some(()));
    }
    let after_links = resident(&tui);
    assert!(
        after_links < before + SLACK,
        "listing the links took {} kB more ({before} kB before)",
        after_links.saturating_sub(before)
    );

    // Opening the history in the editor, which reads every row too.
    tui.send(b"\x01v");
    tui.wait_until("the editor didn't run", || edited.exists());
    tui.wait_until("taskman didn't draw again after the editor", || {
        tui.with(|t| {
            let entered = String::from_utf8_lossy(&t.raw).matches("\x1b[?1049h").count();
            entered == 2 && find(t.screen(), "printed 10000").is_some()
        })
    });
    let text = std::fs::read_to_string(&edited).unwrap();
    // The rows scrolled out of view as well as the screen.
    assert!(text.contains("line 100\n") && text.contains("line 10000\nprinted"), "{text:.200}");
    let after_editor = resident(&tui);
    assert!(
        after_editor < before + SLACK,
        "opening the history took {} kB more ({before} kB before)",
        after_editor.saturating_sub(before)
    );
    tui.quit();
}
