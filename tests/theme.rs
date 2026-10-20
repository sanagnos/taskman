//! The terminal's background, against the binary in a pty with a stub session: a terminal that
//! says it's white gets a light footer and selection, and a session asking for the background is
//! told white; one that doesn't say is taken as dark, as before.

mod common;

use common::{Tui, project, scratch_dir, stubbed};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

fn quit(tui: &mut Tui) {
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

#[test]
fn a_light_terminal_draws_light_and_tells_the_sessions() {
    let dir = scratch_dir("theme-light");
    project(&dir, &["ENG-1"]);
    let mut tui = Tui::start_light("theme-light", &dir, stubbed(&dir), LIMIT);
    tui.wait_text("stub ready for ENG-1");
    // The footer, the bottom row, on the light grey; the selected card too.
    let footer = tui.with(|t| t.screen().cell(39, 0).map(|c| c.bgcolor()));
    assert_eq!(footer, Some(vt100::Color::Idx(254)));
    let (col, row) = tui.wait_text("┃ ENG-1");
    let card = tui.with(|t| t.screen().cell(row, col + 2).map(|c| c.bgcolor()));
    assert_eq!(card, Some(vt100::Color::Idx(254)));

    tui.send(b"colour\r");
    tui.wait_text("colour ]11;rgb:ffff/ffff/ffff");
    quit(&mut tui);
}

#[test]
fn a_terminal_that_does_not_say_is_dark() {
    let dir = scratch_dir("theme-dark");
    project(&dir, &["ENG-1"]);
    let mut tui = Tui::start("theme-dark", &dir, stubbed(&dir), LIMIT);
    tui.wait_text("stub ready for ENG-1");
    let footer = tui.with(|t| t.screen().cell(39, 0).map(|c| c.bgcolor()));
    assert_eq!(footer, Some(vt100::Color::Idx(236)));

    tui.send(b"colour\r");
    tui.wait_text("colour ]11;rgb:1e1e/1e1e/1e1e");
    quit(&mut tui);
}
