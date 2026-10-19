//! Scrolling a session back over emoji, against the binary in a pty with a stub session. The
//! terminal in the test draws a warning sign with U+FE0F two columns wide, as Windows Terminal
//! does, so a width taskman and the terminal disagree on shifts the rest of the row and leaves
//! stale text behind.

mod common;

use common::{Tui, project, scratch_dir, stubbed};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// The screen's rows, a cell to an entry (a wide character's second cell is empty).
fn rows(screen: &vt100::Screen) -> Vec<Vec<String>> {
    let (rows, cols) = common::size(screen);
    (0..rows)
        .map(|row| {
            (0..cols)
                .map(|col| {
                    screen.cell(row, col).map_or(String::new(), |c| c.contents().to_string())
                })
                .collect()
        })
        .collect()
}

/// What's wrong with the session's `row NNN ... end` rows on the screen, if anything: `end` a fixed
/// number of columns from `row` (the emoji's width), nothing after it but the pane's edge, and that
/// edge in one column.
fn problem(screen: &vt100::Screen) -> Option<String> {
    let mut edges = Vec::new();
    for (y, row) in rows(screen).iter().enumerate() {
        let text: String = row.iter().map(|c| if c.is_empty() { " " } else { c }).collect();
        // The scrolled-back note takes the pane's top row's right end.
        if text.contains("lines back") {
            continue;
        }
        let Some(at) =
            row.iter().position(|c| c == "r").filter(|&i| row[i..].concat().starts_with("row "))
        else {
            continue;
        };
        // With U+FE0F the warning sign is two columns, without it one.
        let end = at + if text.contains('\u{fe0f}') { 17 } else { 16 };
        if row[end..].concat().get(..3) != Some("end") {
            return Some(format!("row {y}: no end at column {end}: {text:?}"));
        }
        // Then blanks and the pane's edge; what's past that edge is the panels'.
        let Some(edge) = (end + 3..row.len()).find(|&i| row[i] == "│") else {
            return Some(format!("row {y}: no pane edge after end: {text:?}"));
        };
        if !row[end + 3..edge].concat().trim().is_empty() {
            return Some(format!("row {y}: stale text after end: {text:?}"));
        }
        edges.push(edge);
    }
    edges.windows(2).any(|w| w[0] != w[1]).then(|| format!("the pane's edge moves: {edges:?}"))
}

#[test]
fn scrolling_back_over_emoji_leaves_nothing_stale() {
    let dir = scratch_dir("emoji-scroll");
    project(&dir, &["ENG-1"]);
    let mut tui =
        Tui::start("scrolling_back_over_emoji_leaves_nothing_stale", &dir, stubbed(&dir), LIMIT);
    let (col, row) = tui.wait_text("stub ready for ENG-1");
    tui.send(b"emoji 120\r");
    tui.wait_text("printed 120");
    tui.wait_for("the rows drawn evenly", |s| problem(s).is_none().then_some(()));

    // Back through the history and forth again, a step at a time: each position has to be whole
    // before the next, as a frame that leaves a cell behind keeps it from then on.
    for _ in 0..8 {
        tui.wheel(true, col, row);
        tui.wait_for("the rows drawn evenly, scrolled back", |s| {
            problem(s).is_none().then_some(())
        });
    }
    tui.wait_for("older rows", |s| (!common::find(s, "row 120").is_some()).then_some(()));
    for _ in 0..8 {
        tui.wheel(false, col, row);
        tui.wait_for("the rows drawn evenly, scrolled forward", |s| {
            problem(s).is_none().then_some(())
        });
    }
    tui.wait_text("row 120");
    tui.quit();
}
