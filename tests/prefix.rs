//! Another prefix key, set in the config: every hint names it. The footer, the prefix menu and
//! Help, scrolled to its end, say ctrl+b and never the default ctrl+a.

mod common;

use common::{Tui, project, scratch_dir, stubbed};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// The default prefix, as a hint names it in full and in short.
fn no_default_prefix(tui: &Tui, where_: &str) {
    let text = tui.text();
    assert!(!text.contains("ctrl+a") && !text.contains("^a"), "{where_} names ctrl+a:\n{text}");
}

#[test]
fn hints_name_the_prefix_set_in_the_config() {
    let dir = scratch_dir("prefix");
    project(&dir, &["ENG-1"]);
    std::fs::write(dir.join("config.json"), r#"{"accounts":[],"prefix":"ctrl+b"}"#).unwrap();
    let mut tui = Tui::start("hints_name_the_prefix_set_in_the_config", &dir, stubbed(&dir), LIMIT);

    tui.step("waiting for the footer");
    tui.wait_text("^b menu");
    no_default_prefix(&tui, "the footer");

    tui.step("opening the prefix menu");
    tui.send(b"\x02");
    tui.wait_text("press a command key");
    no_default_prefix(&tui, "the prefix menu");

    tui.step("opening Help");
    tui.send(b"?");
    tui.wait_text("except after the ctrl+b prefix");
    no_default_prefix(&tui, "Help");
    tui.send(b"\x1b[F");
    tui.wait_text("Enter resumes it here");
    tui.wait_text("send ctrl+b itself to the session");
    no_default_prefix(&tui, "Help's end");

    tui.step("quitting");
    // Any key closes Help; not Esc, which with the next key would arrive as Alt+key.
    tui.send(b"x");
    tui.wait_for("Help closed", |s| common::find(s, "Enter resumes it").is_none().then_some(()));
    tui.send(b"\x02q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}
