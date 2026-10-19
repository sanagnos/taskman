//! A vt100 screen as tui-term's `PseudoTerminal` draws it. tui-term's own implementation (its
//! `vt100` feature) ties vt100 to the version tui-term asks for; implementing its `Screen` and
//! `Cell` traits here lets vt100 move on its own. It draws what tui-term's did.

use ratatui::buffer;
use ratatui::style::{Color, Modifier, Style};

/// A session's screen, scrolled back or not, for `PseudoTerminal::new`.
pub struct Screen<'a>(pub &'a vt100::Screen);

impl tui_term::widget::Screen for Screen<'_> {
    type C = Cell;

    fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.0.cell(row, col).map(Cell::wrap)
    }

    fn hide_cursor(&self) -> bool {
        self.0.hide_cursor()
    }

    /// Counted from the top of the view, as `cell` is: scrolled back, the screen (and the cursor
    /// on it) starts below the history rows shown, or past the bottom of the view.
    fn cursor_position(&self) -> (u16, u16) {
        let (row, col) = self.0.cursor_position();
        let back = u16::try_from(self.0.scrollback()).unwrap_or(u16::MAX);
        (row.saturating_add(back), col)
    }
}

/// A vt100 cell, drawn with its text, colours and attributes.
#[repr(transparent)]
pub struct Cell(vt100::Cell);

impl Cell {
    /// tui-term borrows each cell from the screen, so the wrapper is the screen's own cell, not
    /// a copy.
    pub fn wrap(cell: &vt100::Cell) -> &Self {
        // SAFETY: `Cell` is `repr(transparent)` over `vt100::Cell`, so the two have the same
        // layout, and the reference keeps the borrow's lifetime.
        unsafe { &*std::ptr::from_ref(cell).cast::<Self>() }
    }
}

impl tui_term::widget::Cell for Cell {
    fn has_contents(&self) -> bool {
        self.0.has_contents()
    }

    fn apply(&self, out: &mut buffer::Cell) {
        let cell = &self.0;
        if cell.has_contents() {
            out.set_symbol(cell.contents());
        }
        let mut modifier = Modifier::empty();
        for (on, m) in [
            (cell.bold(), Modifier::BOLD),
            (cell.italic(), Modifier::ITALIC),
            (cell.underline(), Modifier::UNDERLINED),
            (cell.inverse(), Modifier::REVERSED),
            (cell.dim(), Modifier::DIM),
        ] {
            if on {
                modifier |= m;
            }
        }
        out.set_style(
            Style::reset()
                .fg(colour(cell.fgcolor()))
                .bg(colour(cell.bgcolor()))
                .add_modifier(modifier),
        );
    }
}

fn colour(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;
    use tui_term::widget::{Cursor, PseudoTerminal, Screen as _};

    fn draw(parser: &vt100::Parser, cursor: bool) -> Buffer {
        let (rows, cols) = parser.screen().size();
        let area = Rect::new(0, 0, cols, rows);
        let mut buf = Buffer::empty(area);
        let screen = Screen(parser.screen());
        PseudoTerminal::new(&screen)
            .cursor(Cursor::default().visibility(cursor))
            .render(area, &mut buf);
        buf
    }

    #[test]
    fn draws_text_colours_and_attributes() {
        let mut p = vt100::Parser::new(1, 12, 0);
        p.process(b"a\x1b[3;4;7;2;31;48;2;1;2;3mb\x1b[0;1;38;5;200mc\x1b[0m\xe6\x97\xa5");
        let buf = draw(&p, false);
        let a = &buf[(0, 0)];
        assert_eq!(
            (a.symbol(), a.fg, a.bg, a.modifier),
            ("a", Color::Reset, Color::Reset, Modifier::empty())
        );
        let b = &buf[(1, 0)];
        assert_eq!((b.symbol(), b.fg, b.bg), ("b", Color::Indexed(1), Color::Rgb(1, 2, 3)));
        let all = Modifier::ITALIC | Modifier::UNDERLINED | Modifier::REVERSED | Modifier::DIM;
        assert_eq!(b.modifier, all);
        // Bold apart: vt100 keeps bold and dim as one intensity, the last set.
        let c = &buf[(2, 0)];
        assert_eq!((c.symbol(), c.fg, c.modifier), ("c", Color::Indexed(200), Modifier::BOLD));
        assert_eq!(buf[(3, 0)].symbol(), "日");
        // A blank cell keeps the buffer's symbol.
        assert_eq!(buf[(11, 0)].symbol(), " ");
    }

    #[test]
    fn an_emoji_takes_the_columns_the_terminal_gives_it() {
        use unicode_width::UnicodeWidthStr as _;
        // The warning sign with U+FE0F is two columns, as unicode-width and terminals measure it,
        // and without it one; ✅ and ❌ are two either way.
        let mut p = vt100::Parser::new(1, 14, 0);
        p.process("a\u{26a0}\u{fe0f}b✅c❌d\u{26a0}e".as_bytes());
        let buf = draw(&p, false);
        let at = |x| buf[(x, 0)].symbol().to_string();
        assert_eq!(
            (0..12).map(at).collect::<Vec<_>>(),
            ["a", "\u{26a0}\u{fe0f}", " ", "b", "✅", " ", "c", "❌", " ", "d", "\u{26a0}", "e"]
        );
        // Each symbol's width is the columns it holds, so ratatui skips what the emoji covers.
        assert_eq!(at(1).width(), 2);
        assert_eq!(at(10).width(), 1);
        let blank = Buffer::empty(buf.area);
        let written: Vec<u16> = blank.diff(&buf).iter().map(|(x, _, _)| *x).collect();
        assert_eq!(written, [0, 1, 3, 4, 6, 7, 9, 10, 11]);
    }

    #[test]
    fn the_cursor_moves_down_by_the_history_shown() {
        let mut p = vt100::Parser::new(4, 20, 20);
        for i in 0..9 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        // On the screen's top row, past its text.
        p.process(b"\x1b[1;9H");
        assert_eq!(Screen(p.screen()).cursor_position(), (0, 8));
        p.screen_mut().set_scrollback(2);
        assert_eq!(Screen(p.screen()).cursor_position(), (2, 8));
        // Drawn where the screen's row shows, or not at all once scrolled past the view.
        assert_eq!(draw(&p, true)[(8, 2)].symbol(), "█");
        p.screen_mut().set_scrollback(4);
        let buf = draw(&p, true);
        assert!(buf.content.iter().all(|c| c.symbol() != "█"));
    }
}
