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

/// The stub's `warnings` rows: `row NNN`, eight warning signs with U+FE0F, `end`.
const WARNINGS: usize = 8;

/// What's wrong with the stub's `warnings` rows on `screen`, if anything: each as printed, the
/// rest of it blank up to the pane's edge, that edge in one column, and `count` of them. A cell is
/// read without U+FE0F, so a terminal that draws the warning sign one column wide and leaves the
/// next blank reads the same as one that draws it two wide.
fn stale(screen: &vt100::Screen, count: usize) -> Option<String> {
    let mut edges = Vec::new();
    for (y, row) in rows(screen).iter().enumerate() {
        let row: Vec<String> = row
            .iter()
            .map(|c| match c.replace('\u{fe0f}', "") {
                c if c.is_empty() => " ".to_string(),
                c => c,
            })
            .collect();
        let text = row.concat();
        let Some(at) = (0..row.len()).find(|&i| row[i..].concat().starts_with("row ")) else {
            continue;
        };
        let n = &text[text.find("row ").unwrap() + 4..][..3];
        let printed = format!("row {n} {}end", "⚠  ".repeat(WARNINGS));
        let end = at + printed.chars().count();
        if row.get(at..end).map(<[String]>::concat) != Some(printed) {
            return Some(format!("row {y} isn't as printed: {text:?}"));
        }
        let Some(edge) = (end..row.len()).find(|&i| row[i] == "│") else {
            return Some(format!("row {y}: no pane edge after end: {text:?}"));
        };
        if !row[end..edge].concat().trim().is_empty() {
            return Some(format!("row {y}: stale text after end: {text:?}"));
        }
        edges.push(edge);
    }
    if edges.len() != count {
        return Some(format!("{} rows of warning signs, not {count}", edges.len()));
    }
    edges.windows(2).any(|w| w[0] != w[1]).then(|| format!("the pane's edge moves: {edges:?}"))
}

/// Waits for the `warnings` rows to be whole both in the test's terminal and in one that widens
/// the warning sign as tmux 3.4 does; fails with what's wrong in each if they aren't.
fn wait_whole(tui: &Tui, what: &str, count: usize) {
    tui.step(what);
    let until = std::time::Instant::now() + common::WAIT;
    loop {
        let (wide, tmux) = tui.with(|t| (stale(t.screen(), count), stale(t.tmux.screen(), count)));
        if wide.is_none() && tmux.is_none() {
            return;
        }
        assert!(
            std::time::Instant::now() < until,
            "{what}: {wide:?} in the terminal:\n{}\n{tmux:?} as tmux 3.4 shows it:\n{}",
            tui.text(),
            tui.with(|t| t.tmux.screen().contents())
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn warning_signs_drawn_over_a_box_or_under_a_closed_overlay_leave_nothing_stale() {
    let dir = scratch_dir("emoji-overlay");
    project(&dir, &["ENG-1"]);
    let mut tui = Tui::start(
        "warning_signs_drawn_over_a_box_or_under_a_closed_overlay_leave_nothing_stale",
        &dir,
        stubbed(&dir),
        LIMIT,
    );
    tui.wait_text("stub ready for ENG-1");
    // The session redraws a box's edges as warning signs: each sign lands where `─` was drawn.
    tui.send(b"box 25\r");
    tui.wait_for("the box", |s| {
        let edges = rows(s).iter().filter(|r| r.concat().contains(&"─".repeat(60))).count();
        (edges == 25).then_some(())
    });
    tui.send(b"warnings 25\r");
    tui.wait_text("printed 25");
    wait_whole(&tui, "the warning signs drawn over the box", 25);

    // Overlays drawn over them, their borders and text on the cells beside each sign, then closed.
    for (open, shown) in
        [(&b"\x01?"[..], "Every key goes to the session"), (b"\x01q", "Quit taskman?")]
    {
        tui.send(open);
        tui.wait_text(shown);
        let from = tui.with(|t| t.raw.len());
        tui.send(b"\x1b");
        tui.wait_for("the overlay closed", |s| common::find(s, shown).is_none().then_some(()));
        wait_whole(&tui, &format!("the warning signs after {shown:?} closed"), 25);
        // It was over the warning signs: closing it drew them again.
        let redrawn = tui.with(|t| common::contains(&t.raw[from..], "\u{26a0}\u{fe0f}".as_bytes()));
        assert!(redrawn, "closing {shown:?} drew no warning sign");
    }
    tui.quit();
}
