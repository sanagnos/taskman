//! Selecting a session's text with the mouse, against the binary in a pty with a stub session and
//! stub clipboard commands: a selection stays after the button is released and is copied; ctrl+c
//! copies it again without interrupting the session, and with nothing selected interrupts it;
//! streaming output moves neither the highlight nor what's copied; scrolling, with the wheel or by
//! dragging past the pane's edge, keeps the selection on its text. Under NO_COLOR it still shows,
//! over a background too, with nothing coloured. `prefix v` opens the session's history in `$EDITOR`.

mod common;

use common::{Tui, clipboard, find, highlighted, project, scratch_dir, script, stub_file, stubbed};
use std::path::Path;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// taskman on one ticket, ENG-1, whose stub session is ready. Returns it and the session's first
/// column on screen.
fn start(name: &'static str, dir: &Path) -> (Tui, u16) {
    project(dir, &["ENG-1"]);
    let tui = Tui::start(name, dir, stubbed(dir), LIMIT);
    let (col, _) = tui.wait_text("stub ready for ENG-1");
    (tui, col)
}

/// Drags over `len` cells of `row` from `col`, and lets go.
fn select(tui: &mut Tui, col: u16, row: u16, len: u16) {
    tui.press(col, row);
    tui.drag(col + len - 1, row);
    tui.release(col + len - 1, row);
}

/// Waits for a copy that puts `text` on the clipboard.
fn wait_copied(tui: &Tui, text: &str) {
    tui.wait_until(
        &format!("{text:?} wasn't copied (the clipboard has {:?})", clipboard(&tui.dir)),
        || clipboard(&tui.dir).as_deref() == Some(text),
    );
}

fn forget_clipboard(dir: &Path) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if e.file_name().to_string_lossy().starts_with("clipboard.") {
            std::fs::remove_file(e.path()).unwrap();
        }
    }
}

#[test]
fn a_selection_stays_and_ctrl_c_copies_it_without_interrupting() {
    let dir = scratch_dir("select-copy");
    let (mut tui, _) = start("a_selection_stays_and_ctrl_c_copies_it_without_interrupting", &dir);
    tui.send(b"lines 50\r");
    tui.wait_text("printed 50");

    let (col, row) = find(&tui.with(|t| t.screen().clone()), "line 030").unwrap();
    select(&mut tui, col, row, 8);
    wait_copied(&tui, "line 030");
    tui.wait_text("Copied 8 characters");
    // Still highlighted once the button is up.
    assert_eq!(tui.with(|t| highlighted(t.screen())), "line 030");

    // ctrl+c copies it again and clears it; the session sees nothing.
    forget_clipboard(&dir);
    tui.send(b"\x03");
    wait_copied(&tui, "line 030");
    tui.wait_for("the selection cleared", |s| highlighted(s).is_empty().then_some(()));
    tui.send(b"after\r");
    tui.wait_text("got: after");
    assert_eq!(stub_file(&dir, "ENG-1", "signals"), "");

    // With nothing selected, ctrl+c interrupts the session as usual.
    tui.send(b"\x03");
    tui.wait_until("ctrl+c didn't interrupt the session", || {
        stub_file(&dir, "ENG-1", "signals") == "INT\n"
    });
    tui.quit();
}

#[test]
fn streaming_output_moves_neither_the_highlight_nor_the_copy() {
    let dir = scratch_dir("select-stream");
    let (mut tui, col) = start("streaming_output_moves_neither_the_highlight_nor_the_copy", &dir);
    let ticks = |dir: &Path| stub_file(dir, "ENG-1", "ticks").trim().parse::<u32>().unwrap_or(0);
    tui.send(b"stream\r");
    // A screenful, so each new tick scrolls the session.
    tui.wait_until("the session didn't stream", || ticks(&dir) > 60);

    let row = 10;
    select(&mut tui, col, row, 9);
    let selected = tui.wait_for("a tick highlighted", |s| {
        let h = highlighted(s);
        h.starts_with("tick ").then_some(h)
    });
    wait_copied(&tui, &selected);
    let at = ticks(&dir);
    tui.wait_until("the session stopped streaming", || ticks(&dir) > at + 50);
    tui.with(|t| {
        assert_eq!(highlighted(t.screen()), selected, "the highlight moved");
        assert_eq!(find(t.screen(), &selected), Some((col, row)), "the text under it moved");
    });
    assert_eq!(clipboard(&dir).as_deref(), Some(selected.as_str()));

    // Esc clears it, and the pane shows the session as it is now.
    tui.send(b"\x1b");
    tui.wait_for("the live session after Esc", |s| {
        if !highlighted(s).is_empty() {
            return None;
        }
        let shown = s.contents();
        let last = shown
            .lines()
            .filter_map(|l| l.split("tick ").nth(1)?.get(..4)?.parse::<u32>().ok())
            .max()?;
        (last > at + 50).then_some(())
    });
    tui.quit();
}

#[test]
fn the_wheel_keeps_the_selection_on_its_text() {
    let dir = scratch_dir("select-wheel");
    let (mut tui, _) = start("the_wheel_keeps_the_selection_on_its_text", &dir);
    tui.send(b"lines 100\r");
    tui.wait_text("printed 100");

    let (col, row) = tui.with(|t| find(t.screen(), "line 090")).unwrap();
    select(&mut tui, col, row, 8);
    wait_copied(&tui, "line 090");
    for _ in 0..3 {
        tui.wheel(true, col, row);
    }
    // Three rows a step: the text, and its highlight, are nine rows further down.
    tui.wait_for("line 090 nine rows down", |s| {
        (find(s, "line 090") == Some((col, row + 9))).then_some(())
    });
    assert_eq!(tui.with(|t| highlighted(t.screen())), "line 090");
    tui.quit();
}

#[test]
fn dragging_past_the_top_scrolls_and_selects_beyond_the_view() {
    let dir = scratch_dir("select-edge");
    let (mut tui, _) = start("dragging_past_the_top_scrolls_and_selects_beyond_the_view", &dir);
    tui.send(b"lines 100\r");
    tui.wait_text("printed 100");
    assert!(!tui.text().contains("line 030"), "line 030 should be out of view to begin with");

    // From the end of line 080 up past the pane's top edge, the button held.
    let (col, row) = tui.with(|t| find(t.screen(), "line 080")).unwrap();
    tui.press(col + 7, row);
    tui.drag(col, 0);
    tui.wait_text("line 030");
    tui.release(col, 0);

    tui.wait_until("nothing was copied", || clipboard(&dir).is_some());
    let copied = clipboard(&dir).unwrap();
    let numbers: Vec<u32> =
        copied.lines().map(|l| l.strip_prefix("line ").unwrap().parse().unwrap()).collect();
    assert_eq!(numbers.last(), Some(&80), "{copied}");
    assert!(numbers[0] <= 30, "{copied}");
    assert!(numbers.windows(2).all(|w| w[1] == w[0] + 1), "{copied}");
    tui.quit();
}

#[test]
fn prefix_v_opens_the_history_in_the_editor() {
    let dir = scratch_dir("history");
    project(&dir, &["ENG-1"]);
    let edited = dir.join("edited.txt");
    let editor = script(&dir, "editor", &format!("#!/bin/sh\ncp \"$1\" '{}'\n", edited.display()));
    let mut cmd = stubbed(&dir);
    cmd.env("EDITOR", editor).env_remove("VISUAL");
    let mut tui = Tui::start("prefix_v_opens_the_history_in_the_editor", &dir, cmd, LIMIT);
    tui.wait_text("stub ready for ENG-1");
    tui.send(b"lines 100\r");
    tui.wait_text("printed 100");

    tui.send(b"\x01v");
    tui.wait_until("the editor didn't run", || edited.exists());
    // Back from the editor, taskman takes the terminal again (a fresh alternate screen) and draws.
    tui.wait_until("taskman didn't draw again after the editor", || {
        tui.with(|t| {
            let entered = String::from_utf8_lossy(&t.raw).matches("\x1b[?1049h").count();
            entered == 2 && find(t.screen(), "printed 100").is_some()
        })
    });
    let text = std::fs::read_to_string(&edited).unwrap();
    // The rows scrolled out of view as well as the screen.
    assert!(text.contains("stub ready for ENG-1\n") && text.contains("line 001\n"), "{text}");
    assert!(text.contains("line 100\nprinted 100"), "{text}");
    let history = std::fs::read_dir(dir.join("state/taskman/history"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let mode = std::fs::metadata(history.join("ENG-1.txt")).unwrap().permissions();
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
    tui.quit();
}

/// The colours in the SGR escapes in `raw` (30 to 38, 40 to 48, 58, 90 to 97, 100 to 107), and
/// any SGR with no parameters, a full reset: crossterm writes one for each colour under NO_COLOR.
fn colours_and_resets(raw: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    for (i, _) in raw.windows(2).enumerate().filter(|(_, w)| *w == b"\x1b[") {
        let params = &raw[i + 2..];
        let Some(n) = params.iter().position(|b| !(b.is_ascii_digit() || b";:".contains(b))) else {
            continue;
        };
        if params[n] != b'm' {
            continue;
        }
        let params = String::from_utf8_lossy(&params[..n]);
        let colour = |p: &str| {
            p.parse::<u16>()
                .is_ok_and(|p| matches!(p, 30..=38 | 40..=48 | 58 | 90..=97 | 100..=107))
        };
        if params.split([';', ':']).all(str::is_empty) || params.split(';').any(colour) {
            found.push(format!("\\e[{params}m"));
        }
    }
    found
}

/// `len` cells of `row` from `col`, each `r` if it's in reverse video, else `-`.
fn reversed(screen: &vt100::Screen, row: u16, col: u16, len: u16) -> String {
    (col..col + len)
        .map(|c| if screen.cell(row, c).is_some_and(vt100::Cell::inverse) { 'r' } else { '-' })
        .collect()
}

#[test]
fn a_selection_shows_under_no_color_with_nothing_coloured() {
    let dir = scratch_dir("select-no-color");
    project(&dir, &["ENG-1"]);
    let mut cmd = stubbed(&dir);
    cmd.env("NO_COLOR", "1");
    let mut tui =
        Tui::start("a_selection_shows_under_no_color_with_nothing_coloured", &dir, cmd, LIMIT);
    tui.wait_text("stub ready for ENG-1");
    tui.send(b"styled\r");
    let (col, row) = tui.wait_text("bold red underlined on green");
    // A background shows as reverse video instead: here `underlined on green`'s.
    let line = |s: &vt100::Screen| reversed(s, row, col, 28);
    tui.wait_for("the green background reversed", |s| {
        (line(s) == "---------rrrrrrrrrrrrrrrrrrr").then_some(())
    });

    // Selected, the plain cells reverse and the reversed ones flip back, so it shows on both.
    select(&mut tui, col, row, 19);
    wait_copied(&tui, "bold red underlined");
    tui.wait_for("the selection highlighted", |s| {
        (line(s) == "rrrrrrrrr----------rrrrrrrrr").then_some(())
    });
    tui.with(|t| {
        let screen = t.screen();
        // The session's attributes stay; its colours, and taskman's, don't.
        assert!(screen.cell(row, col).unwrap().bold());
        assert!(screen.cell(row, col + 20).unwrap().underline());
        // The selected card, marked by its background, is in reverse video.
        let card = (0..40).find(|&r| screen.contents_between(r, 0, r, 28).contains("ENG-1"));
        let card = card.unwrap();
        let key = (0..28).find(|&c| screen.cell(card, c).unwrap().contents() == "E").unwrap();
        assert_eq!(reversed(screen, card, key, 5), "rrrrr", "{}", screen.contents());
        let (rows, cols) = screen.size();
        for (r, c) in (0..rows).flat_map(|r| (0..cols).map(move |c| (r, c))) {
            let cell = screen.cell(r, c).unwrap();
            assert_eq!(
                (cell.fgcolor(), cell.bgcolor()),
                (vt100::Color::Default, vt100::Color::Default),
                "row {r}, column {c} is coloured:\n{}",
                screen.contents()
            );
        }
        assert_eq!(colours_and_resets(&t.raw), Vec::<String>::new());
    });
    tui.quit();
}
