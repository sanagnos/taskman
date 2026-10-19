//! The account form saves a pasted token to the OS keychain, through the binary as a user runs it:
//! a stub `security` (macOS) or `secret-tool` (Linux) first on PATH stands in for the keychain, so
//! no real one is touched. The token reaches it on stdin, never as an argument, config.json gets
//! only where to read it (`keychain:jira-1`), and taskman then reads it back from there.

mod common;

use common::{Tui, scratch_dir, script, taskman};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(60);
const TOKEN: &str = "ATATT-mock-token-1";

#[test]
fn the_account_form_saves_a_token_to_the_keychain() {
    let dir = scratch_dir("keychain");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let program = if cfg!(target_os = "macos") { "security" } else { "secret-tool" };
    script(&bin, program, include_str!("fixtures/keychain-stub.sh"));
    // In the folder `taskman` puts first on PATH, before its own stubs.
    let mut cmd = taskman(&dir);
    cmd.env_remove("JIRA_EMAIL");
    let mut tui = Tui::start("the_account_form_saves_a_token_to_the_keychain", &dir, cmd, LIMIT);
    tui.wait_text("Welcome to taskman");

    // The keychain is looked for in the background at start: until it's found the form offers
    // only a variable or command, so open it again until it offers the keychain. Each step waits
    // for what it caused, never for something to be gone: a screen read mid-draw lacks text that
    // is about to appear, and a key sent on that belief lands in the form's Site as text, or
    // together with the Esc before it, as Alt.
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        tui.send(b"\x01A");
        // The form's last row: the rows above it, Keep in among them, are drawn by now.
        tui.wait_text("Esc back to list");
        if tui.text().contains("Keep in") {
            break;
        }
        assert!(Instant::now() < until, "no keychain offered:\n{}", tui.text());
        close_settings(&mut tui, true);
    }
    tui.wait_text("taskman never saves your token in its files");

    // Site (left empty), Email, Keep in, Token: pasted, as from the token page. A Site would be
    // looked up on the network, so check it is empty before anything is saved.
    assert!(site_is_empty(&tui.text()), "Site is not empty:\n{}", tui.text());
    tui.send(b"\tdana@example.com\t\t");
    tui.send(format!("\x1b[200~{TOKEN}\x1b[201~").as_bytes());
    tui.wait_text("••••••••••••••••••");
    assert!(!tui.text().contains(TOKEN), "the token is on screen:\n{}", tui.text());
    tui.send(b"\r");
    tui.wait_text("Saved the Jira account (add a site to test it)");

    let config = std::fs::read_to_string(dir.join("config.json")).unwrap();
    assert!(config.contains("\"token\": \"keychain:jira-1\""), "{config}");
    assert!(!config.contains(TOKEN), "the token was saved: {config}");
    let item = std::fs::read_to_string(bin.join("items/taskman-jira-1")).unwrap();
    assert_eq!(item, TOKEN);
    // Read back once to check it and once more by the account's clients, never as an argument.
    let read = if cfg!(target_os = "macos") { "find-generic-password" } else { "lookup" };
    tui.wait_until("the token read back twice", || {
        let calls = std::fs::read_to_string(bin.join("calls")).unwrap_or_default();
        calls.lines().filter(|l| l.contains(read) && l.contains("jira-1")).count() >= 2
    });
    let calls = std::fs::read_to_string(bin.join("calls")).unwrap();
    assert!(!calls.contains(TOKEN), "the token was an argument: {calls}");

    close_settings(&mut tui, false);
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

/// Esc out of Settings. A form being edited (`editing`) takes one Esc to go back to the list;
/// saving ends the editing. One Esc at a time, as two at once read as Alt+Esc, each waited for by
/// the keys the next screen offers.
fn close_settings(tui: &mut Tui, editing: bool) {
    if editing {
        tui.send(b"\x1b");
    }
    tui.wait_text("Esc close");
    tui.send(b"\x1b");
    // Only closing Settings removes its title.
    tui.wait_for("Settings closed", |s| common::find(s, " Settings ").is_none().then_some(()));
}

/// Whether the account form's Site has nothing in it.
fn site_is_empty(screen: &str) -> bool {
    let row = screen.lines().find_map(|l| l.split_once("│ Site").map(|(_, rest)| rest));
    row.is_some_and(|r| r.trim_matches(|c: char| c == '│' || c.is_whitespace()).is_empty())
}
