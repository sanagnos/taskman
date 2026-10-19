//! The terminal taskman draws to: crossterm's backend, writing a blank into the column an emoji
//! with a selector (⚠️) covers before the emoji, rather than after it as ratatui does, where the
//! blank lands a column too far right in a terminal that drew the emoji two wide. See "Emoji that
//! terminals measure differently" in docs/design.md.
//!
//! It works around a ratatui bug, seen in ratatui 0.30.2 with ratatui-crossterm 0.1.2:
//! ratatui-core's diff yields the cell after a two-column cell with U+FE0F (x+1), and
//! ratatui-crossterm's `draw` moves the cursor only when a cell doesn't follow the last one
//! written, taking every cell as one column, so that write goes out with no move and lands on
//! x+2. Once a ratatui release moves the cursor right after a two-column cell, this can go, with
//! `tests/emoji.rs` as the check. Until then it's harmless if ratatui changes either half: the x+1
//! cell is covered by the wide character, so dropping it, or never getting it, loses nothing, and
//! every write here gets its own cursor move.

use ratatui::backend::{ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Cell, CellWidth as _};
use ratatui::layout::{Position, Size};
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar as _;

pub struct Backend<W: Write>(CrosstermBackend<W>);

impl<W: Write> Backend<W> {
    pub fn new(writer: W) -> Self {
        Self(CrosstermBackend::new(writer))
    }
}

/// A cell to write: one of the frame's, or a blank written ahead of a character's covered cells.
enum Update<'a> {
    Frame(&'a Cell),
    Blank(Cell),
}

/// `content` with a blank written ahead of each cell an unsettled character covers, and ratatui's
/// own writes of those cells after it left out.
fn reorder<'a>(content: impl Iterator<Item = (u16, u16, &'a Cell)>) -> Vec<(u16, u16, Update<'a>)> {
    let mut out = Vec::new();
    // The row and columns the last unsettled character covers.
    let mut covered: Option<(u16, std::ops::Range<u16>)> = None;
    for (x, y, cell) in content {
        if let Some((row, cols)) = &covered
            && *row == y
            && cols.contains(&x)
        {
            continue;
        }
        covered = None;
        let width = cell.cell_width();
        if unsettled(crate::links::shown(cell.symbol()), width) {
            let cols = x.saturating_add(1)..x.saturating_add(width);
            for col in cols.clone() {
                let mut blank = cell.clone();
                blank.set_symbol(" ");
                out.push((col, y, Update::Blank(blank)));
            }
            covered = Some((y, cols));
        }
        out.push((x, y, Update::Frame(cell)));
    }
    out
}

/// Whether a symbol of `width` columns is wider than its first character on its own: an emoji
/// selector or a joined sequence decides it, which terminals measure differently. A character
/// that is wide on its own (CJK, ✅) every terminal draws wide, clearing the cell it covers.
fn unsettled(text: &str, width: u16) -> bool {
    width > 1 && text.chars().next().and_then(char::width).unwrap_or(0) < usize::from(width)
}

impl<W: Write> ratatui::backend::Backend for Backend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let updates = reorder(content);
        // crossterm's backend moves the cursor before a cell unless it follows the last one written
        // in the row, so the blanks ahead of a character, and the character after them, each get
        // their own move.
        self.0.draw(updates.iter().map(|(x, y, u)| match u {
            Update::Frame(cell) => (*x, *y, *cell),
            Update::Blank(cell) => (*x, *y, cell),
        }))
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.0.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.0.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.0.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.0.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.0.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.0.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.0.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.0.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.0.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        ratatui::backend::Backend::flush(&mut self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::Backend as _;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    /// What the backend writes to go from `before` to `after`, one row of text each, without the
    /// colours.
    fn written(before: &str, after: &str) -> String {
        let area = Rect::new(0, 0, 12, 1);
        let (mut prev, mut next) = (Buffer::empty(area), Buffer::empty(area));
        prev.set_string(0, 0, before, ratatui::style::Style::default());
        next.set_string(0, 0, after, ratatui::style::Style::default());
        drawn(prev.diff_iter(&next))
    }

    /// What the backend writes for `updates`, without the colours.
    fn drawn<'a>(updates: impl Iterator<Item = (u16, u16, &'a Cell)>) -> String {
        let mut out = Vec::new();
        {
            let mut backend = Backend::new(&mut out);
            backend.draw(updates).unwrap();
            ratatui::backend::Backend::flush(&mut backend).unwrap();
        }
        let out = String::from_utf8(out).unwrap();
        let mut text = String::new();
        let mut rest = out.as_str();
        while let Some(at) = rest.find("\x1b[") {
            text.push_str(&rest[..at]);
            let escape = &rest[at..];
            let end = escape.find(|c: char| c.is_ascii_alphabetic()).unwrap() + 1;
            if !escape[..end].ends_with('m') {
                text.push_str(&escape[..end]);
            }
            rest = &escape[end..];
        }
        text + rest
    }

    #[test]
    fn clears_the_cell_a_warning_sign_covers_before_drawing_it() {
        // A box's edge was where the emoji now is: the cell it covers gets a blank first, then the
        // emoji goes where it belongs, and the text after it gets a move of its own.
        assert_eq!(
            written("──────────", "a\u{26a0}\u{fe0f} bc"),
            "\x1b[1;1Ha\x1b[1;3H \x1b[1;2H\u{26a0}\u{fe0f}\x1b[1;4H bc    "
        );
    }

    #[test]
    fn writes_the_same_whether_or_not_ratatui_yields_the_covered_cell() {
        // ratatui 0.30's diff yields the cell after the emoji (x+1) when it changed; a ratatui
        // that stops doing so, or that skips it as for any wide character, must draw the same.
        let (a, warning, blank, b) =
            (Cell::new("a"), Cell::new("\u{26a0}\u{fe0f}"), Cell::new(" "), Cell::new("b"));
        let with = [(0, 0, &a), (1, 0, &warning), (2, 0, &blank), (3, 0, &b)];
        let without = [(0, 0, &a), (1, 0, &warning), (3, 0, &b)];
        let expected = "\x1b[1;1Ha\x1b[1;3H \x1b[1;2H\u{26a0}\u{fe0f}\x1b[1;4Hb";
        assert_eq!(drawn(with.into_iter()), expected);
        assert_eq!(drawn(without.into_iter()), expected);
    }

    #[test]
    fn leaves_characters_wide_on_their_own_to_the_terminal() {
        assert_eq!(written("──────", "a✅b日"), "\x1b[1;1Ha✅\x1b[1;4Hb日");
    }

    #[test]
    fn tells_which_widths_terminals_disagree_on() {
        assert!(unsettled("\u{26a0}\u{fe0f}", 2));
        assert!(unsettled("🇺🇸", 2));
        assert!(!unsettled("\u{26a0}", 1));
        assert!(!unsettled("✅", 2));
        assert!(!unsettled("日", 2));
        assert!(!unsettled("a", 1));
    }
}
