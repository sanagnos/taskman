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
    let mut cmd = taskman(&dir);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    cmd.env("PATH", path).env_remove("JIRA_EMAIL");
    let mut tui = Tui::start("the_account_form_saves_a_token_to_the_keychain", &dir, cmd, LIMIT);
    tui.wait_text("Welcome to taskman");

    // The keychain is looked for in the background at start: until it's found the form offers
    // only a variable or command, so open it again until it offers the keychain.
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        tui.send(b"\x01A");
        tui.wait_text("New account");
        if tui.text().contains("Keep in") {
            break;
        }
        assert!(Instant::now() < until, "no keychain offered:\n{}", tui.text());
        close_settings(&mut tui);
    }
    tui.wait_text("taskman never saves your token in its files");

    // Site (left empty), Email, Keep in, Token: pasted, as from the token page.
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

    close_settings(&mut tui);
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

/// Esc out of Settings, back to the list first if a form is being edited: one Esc at a time, as
/// two at once read as Alt+Esc.
fn close_settings(tui: &mut Tui) {
    if tui.text().contains("Esc back to list") {
        tui.send(b"\x1b");
        tui.wait_text("Esc close");
    }
    tui.send(b"\x1b");
    tui.wait_for("Settings closed", |s| common::find(s, "Esc close").is_none().then_some(()));
}
