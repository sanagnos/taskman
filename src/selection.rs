//! Selecting text with the mouse: what a selection holds, what keys do while one is shown, reading
//! the selected text back, and putting it on the clipboard.
//!
//! A selection stays until the next click, key or Esc, as in zellij, WezTerm or kitty. It is held
//! in rows of the pane's content rather than screen cells, so scrolling, with the wheel or by
//! dragging past the pane's edge, keeps it on the same text. While it is shown, the Claude pane
//! draws a `Snapshot` of the session, so streaming output can't move what's highlighted or change
//! what gets copied.

use crate::session::{Parser, lock};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget, Wrap};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// The most rows one auto-scroll step moves, however far past the edge the pointer is.
const MAX_EDGE_STEP: i32 = 5;

/// A cell of a pane's content: `row` counts from the session's oldest history row, or from a
/// panel's first wrapped row; `col` from the pane's left edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: usize,
    pub col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Claude,
    Jira,
    Gh,
}

/// What a double or triple click selects, once the pane's cells are at hand (the next frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    Word,
    Line,
}

/// A selection made by dragging over one pane, or by a double or triple click.
pub struct Selection {
    pub pane: Pane,
    /// The pane's content rectangle.
    pub rect: Rect,
    pub anchor: Point,
    pub head: Point,
    /// Where the pointer was last dragged to, maybe past the pane's edge: scrolling while the
    /// button is held moves the head to what comes under it.
    pub pointer: Position,
    /// The button was released: copy on the next frame, once the pane has been drawn.
    pub released: bool,
    pub copied: bool,
    /// Set by a double or triple click: the word or line under `anchor` is picked on the next
    /// frame, which sets `picked`.
    pub grab: Option<Grab>,
    pub picked: bool,
    /// The Claude pane's content while the selection is shown.
    pub snapshot: Option<Snapshot>,
    /// When the pane last scrolled because the pointer was held past its edge.
    pub scrolled_at: Option<Instant>,
}

impl Selection {
    pub fn new(
        pane: Pane,
        rect: Rect,
        at: Point,
        pointer: Position,
        snapshot: Option<Snapshot>,
    ) -> Self {
        Self {
            pane,
            rect,
            anchor: at,
            head: at,
            pointer,
            released: false,
            copied: false,
            grab: None,
            picked: false,
            snapshot,
            scrolled_at: None,
        }
    }

    /// Start and end in reading order.
    pub fn span(&self) -> (Point, Point) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    /// More than the cell under a click: worth a highlight, a badge and a copy.
    pub fn dragged(&self) -> bool {
        self.anchor != self.head || self.picked
    }
}

/// A character that ends a word, besides whitespace: quotes, brackets, parentheses and a few
/// separators, as terminals count them when copying.
fn is_delimiter(symbol: &str) -> bool {
    symbol.chars().all(|c| c.is_whitespace() || "\"'`()[]{}<>,;|".contains(c))
}

/// Sentence punctuation, left off a word's end.
fn is_trailing(symbol: &str) -> bool {
    matches!(symbol, "." | "," | ":" | ";" | "!" | "?")
}

/// The columns, both included, of the word at `col` in a row's cells: a run with no whitespace
/// or delimiter, without the punctuation that ends a sentence. `None` on a blank, a delimiter
/// or that punctuation. A cell is its symbol; "" is the cell a wide character covers, which goes
/// with the character before it.
pub fn word_at(cells: &[String], col: u16) -> Option<(u16, u16)> {
    let in_word = |i: usize| cells[i].is_empty() || !is_delimiter(&cells[i]);
    let mut at = usize::from(col);
    if at >= cells.len() {
        return None;
    }
    while at > 0 && cells[at].is_empty() {
        at -= 1;
    }
    if !in_word(at) {
        return None;
    }
    let (mut start, mut end) = (at, at);
    while start > 0 && in_word(start - 1) {
        start -= 1;
    }
    while end + 1 < cells.len() && in_word(end + 1) {
        end += 1;
    }
    while end > start && is_trailing(&cells[end]) {
        end -= 1;
    }
    if end < at || is_trailing(&cells[end]) {
        return None;
    }
    Some((u16::try_from(start).ok()?, u16::try_from(end).ok()?))
}

/// The columns of a row's text, from its first to its last visible character, or `None` when the
/// row is blank.
pub fn line_at(cells: &[String]) -> Option<(u16, u16)> {
    let visible = |i: &usize| !cells[*i].trim().is_empty();
    let start = (0..cells.len()).find(visible)?;
    let mut end = (0..cells.len()).rfind(visible)?;
    while cells.get(end + 1).is_some_and(String::is_empty) {
        end += 1; // the cells a wide character covers
    }
    Some((u16::try_from(start).ok()?, u16::try_from(end).ok()?))
}

/// The cells of the row `grab` is made on, widened to the word or line `col` is in.
pub fn grab_span(cells: &[String], grab: Grab, col: u16) -> Option<(u16, u16)> {
    match grab {
        Grab::Word => word_at(cells, col),
        Grab::Line => line_at(cells).filter(|_| !cells.is_empty()),
    }
}

/// The content cell under `pos`, kept inside `rect`, while the pane shows content row `top` at
/// its top.
pub fn point_at(rect: Rect, top: usize, pos: Position) -> Point {
    let x = pos.x.clamp(rect.left(), rect.right().saturating_sub(1));
    let y = pos.y.clamp(rect.top(), rect.bottom().saturating_sub(1));
    Point { row: top + usize::from(y - rect.y), col: x - rect.x }
}

/// How many rows to scroll by on each auto-scroll step while the button is held at `pointer`:
/// negative above the pane, positive below it, more the farther away, none inside.
pub fn edge_step(rect: Rect, pointer: Position) -> i32 {
    let past = if pointer.y < rect.top() {
        -i32::from(rect.top() - pointer.y)
    } else if pointer.y >= rect.bottom() {
        i32::from(pointer.y - rect.bottom()) + 1
    } else {
        0
    };
    past.clamp(-MAX_EDGE_STEP, MAX_EDGE_STEP)
}

/// The cells of `start..=end` a pane showing content row `top` at its top draws: none when the
/// selection is all above or below the view, else its ends cut to the view.
pub fn on_screen(rect: Rect, top: usize, start: Point, end: Point) -> Option<(Position, Position)> {
    let rows = usize::from(rect.height);
    if rect.is_empty() || end.row < top || start.row >= top + rows {
        return None;
    }
    let y = |row: usize| rect.y + u16::try_from(row - top).unwrap_or(0);
    let x = |col: u16| rect.x + col.min(rect.width - 1);
    let from = if start.row < top {
        Position::new(rect.x, rect.y)
    } else {
        Position::new(x(start.col), y(start.row))
    };
    let to = if end.row >= top + rows {
        Position::new(rect.right() - 1, rect.bottom() - 1)
    } else {
        Position::new(x(end.col), y(end.row))
    };
    Some((from, to))
}

/// What a key does while a selection is shown.
#[derive(Debug, PartialEq)]
pub enum KeyAction {
    /// Leave the selection alone and handle the key as usual (key releases, a lone modifier).
    Keep,
    /// Copy the selection and clear it; the session never sees the key. The next ctrl+c, with
    /// nothing selected, interrupts as usual (Windows Terminal's rule).
    Copy,
    /// Clear the selection; the session never sees the key.
    Clear,
    /// Clear the selection and handle the key as usual.
    Forward,
}

pub fn key_action(key: &KeyEvent) -> KeyAction {
    if key.kind == KeyEventKind::Release || matches!(key.code, KeyCode::Modifier(_)) {
        return KeyAction::Keep;
    }
    let m = key.modifiers;
    match key.code {
        KeyCode::Char('c' | 'C')
            if m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
        {
            KeyAction::Copy
        }
        KeyCode::Esc => KeyAction::Clear,
        _ => KeyAction::Forward,
    }
}

// ---- the session's content ---------------------------------------------------------------

/// What the Claude pane shows while a selection is held: the session's history above a copy of its
/// screen as it was when the selection began. Rows in the history don't change once they've
/// scrolled off the screen, so they're read from the live session when needed rather than copied
/// (up to `SCROLLBACK` rows on every click); the screen's rows do change, so they're copied.
///
/// Content row `r` below `history` is the session's history row `r - dropped`; from `history` on,
/// it is row `r - history` of `screen`.
pub struct Snapshot {
    parser: Arc<Mutex<Parser>>,
    /// The session's screen when the selection began, without its history.
    screen: vt100::Screen,
    history: usize,
    /// History rows dropped since the selection began: a full history drops its oldest row for
    /// each one it takes.
    dropped: usize,
    /// The content row at the top of the pane.
    top: usize,
    /// The session was on its alternate screen, which keeps no history.
    alternate: bool,
    /// The live screen's scroll position and history length when last looked at (see `track`).
    pin: (usize, usize),
}

impl Snapshot {
    /// Leaves the live session scrolled back by a row (see `track`) until it is dropped.
    pub fn new(parser: Arc<Mutex<Parser>>) -> Self {
        let mut p = lock(&parser);
        let live = p.screen_mut();
        let back = live.scrollback();
        let history = history_len(live);
        live.set_scrollback(0);
        let screen = freeze(live);
        let alternate = live.alternate_screen();
        let pin = pin(live, history);
        drop(p);
        Self {
            parser,
            screen,
            history,
            dropped: 0,
            top: history.saturating_sub(back),
            alternate,
            pin,
        }
    }

    pub fn top(&self) -> usize {
        self.top
    }

    /// The oldest content row still in the session's history.
    fn oldest(&self) -> usize {
        self.dropped.min(self.history)
    }

    /// Counts the rows the session's history dropped since the last call; call it often enough
    /// that fewer than `SCROLLBACK` rows arrive in between. False once the session has switched
    /// between its normal and alternate screens, whose rows these aren't.
    pub fn track(&mut self) -> bool {
        let mut p = lock(&self.parser);
        let live = p.screen_mut();
        if live.alternate_screen() != self.alternate {
            return false;
        }
        let (dropped, pin) = dropped_since(live, self.pin);
        self.pin = pin;
        drop(p);
        self.dropped += dropped;
        self.top = self.top.max(self.oldest());
        true
    }

    /// Moves the view `delta` rows, positive toward the newest; false when it can't go further.
    pub fn scroll(&mut self, delta: i32) -> bool {
        let top = self
            .top
            .saturating_add_signed(isize::try_from(delta).unwrap_or(0))
            .clamp(self.oldest(), self.history);
        std::mem::replace(&mut self.top, top) != top
    }

    /// The rows the pane shows.
    pub fn view(&self) -> View {
        let (rows, cols) = self.screen.size();
        let from_history = self.history.saturating_sub(self.top).min(usize::from(rows));
        let mut view = Vec::with_capacity(usize::from(rows));
        if from_history > 0 {
            let mut p = lock(&self.parser);
            let live = p.screen_mut();
            let back = live.scrollback();
            let len = history_len(live);
            live.set_scrollback(len.saturating_sub(self.top.saturating_sub(self.dropped)));
            view.extend(
                (0..from_history).map(|r| row_cells(live, u16::try_from(r).unwrap_or(0), cols)),
            );
            live.set_scrollback(back);
        }
        view.extend(
            (0..rows - u16::try_from(from_history).unwrap_or(0))
                .map(|r| row_cells(&self.screen, r, cols)),
        );
        View { rows: view }
    }

    /// The symbols of the cells of content row `row`, if the pane shows it.
    pub fn row_symbols(&self, row: usize) -> Option<Vec<String>> {
        let view = self.view();
        let cells = view.rows.get(row.checked_sub(self.top)?)?;
        Some(
            cells
                .iter()
                .map(|c| {
                    if c.is_wide_continuation() {
                        String::new()
                    } else if c.has_contents() {
                        c.contents().to_string()
                    } else {
                        " ".to_string()
                    }
                })
                .collect(),
        )
    }

    /// The text from `start` to `end` (inclusive). Rows the terminal wrapped are joined, wide
    /// characters come out once, and trailing blanks are dropped.
    pub fn text(&self, start: Point, end: Point) -> String {
        let start = start.max(Point { row: self.oldest(), col: 0 });
        if start > end {
            return String::new();
        }
        let mut text = String::new();
        let mut wrapped = false;
        if start.row < self.history {
            let last = end.row.min(self.history - 1);
            let to = if last == end.row { end.col } else { u16::MAX };
            let mut p = lock(&self.parser);
            (text, wrapped) = live_text(
                p.screen_mut(),
                start.row - self.dropped,
                last - self.dropped,
                start.col,
                to,
            );
        }
        if end.row >= self.history {
            let (rows, _) = self.screen.size();
            let first = start.row.max(self.history) - self.history;
            let last = (end.row - self.history).min(usize::from(rows) - 1);
            let from = if start.row >= self.history { start.col } else { 0 };
            if start.row < self.history && !wrapped {
                text.push('\n');
            }
            let to_u16 = |r: usize| u16::try_from(r).unwrap_or(u16::MAX);
            text.push_str(
                &visible_text(&self.screen, to_u16(first), to_u16(last), from, end.col).0,
            );
        }
        tidy(&text)
    }
}

impl Drop for Snapshot {
    /// Leaves the session scrolled to where the view is, as a terminal does when its selection is
    /// cleared: on the same text if it's in the history, else at the bottom.
    fn drop(&mut self) {
        let mut p = lock(&self.parser);
        let live = p.screen_mut();
        // The other screen's scroll position can't be reached; a key puts it back to the bottom.
        if live.alternate_screen() != self.alternate {
            return;
        }
        let (dropped, (_, len)) = dropped_since(live, self.pin);
        let top = self.top.max((self.dropped + dropped).min(self.history));
        let back =
            if top < self.history { len.saturating_sub(top - (self.dropped + dropped)) } else { 0 };
        live.set_scrollback(back);
    }
}

/// How many rows the history has: vt100 caps the scroll position at it.
pub fn history_len(live: &mut vt100::Screen) -> usize {
    let back = live.scrollback();
    live.set_scrollback(usize::MAX);
    let len = live.scrollback();
    live.set_scrollback(back);
    len
}

/// Scrolls the live screen back a row (when it has any history), so vt100 counts the rows the
/// history takes in its scroll position: while scrolled back, each row added moves the position
/// along so the view stays on the same rows. Returns the position and the history's length.
fn pin(live: &mut vt100::Screen, len: usize) -> (usize, usize) {
    live.set_scrollback(1);
    (live.scrollback(), len)
}

/// The rows dropped off the top of the history since `pin` was taken (rows added minus how much
/// the history grew), and a new pin.
fn dropped_since(
    live: &mut vt100::Screen,
    (was_back, was_len): (usize, usize),
) -> (usize, (usize, usize)) {
    let (back, len) = (live.scrollback(), history_len(live));
    let dropped = if was_back > 0 && back >= was_back {
        (back - was_back).saturating_sub(len.saturating_sub(was_len))
    } else {
        0
    };
    (dropped, pin(live, len))
}

fn row_cells(screen: &vt100::Screen, row: u16, cols: u16) -> Vec<vt100::Cell> {
    (0..cols).map_while(|col| screen.cell(row, col).cloned()).collect()
}

/// A `Snapshot`'s rows, drawn with `PseudoTerminal`.
pub struct View {
    rows: Vec<Vec<vt100::Cell>>,
}

impl tui_term::widget::Screen for View {
    type C = vt100::Cell;

    fn cell(&self, row: u16, col: u16) -> Option<&vt100::Cell> {
        self.rows.get(usize::from(row))?.get(usize::from(col))
    }

    fn hide_cursor(&self) -> bool {
        true
    }

    fn cursor_position(&self) -> (u16, u16) {
        (u16::MAX, u16::MAX)
    }
}

/// A copy of what `screen` shows (scrolled back or not), without its scrollback: cloning the live
/// screen would copy up to `SCROLLBACK` rows on every click.
pub fn freeze(screen: &vt100::Screen) -> vt100::Screen {
    let (rows, cols) = screen.size();
    let mut p = vt100::Parser::new(rows, cols, 0);
    p.process(&screen.contents_formatted());
    p.screen().clone()
}

/// Everything the session shows and has scrolled off its screen, oldest first, for `$EDITOR`.
pub fn session_text(live: &mut vt100::Screen) -> String {
    let rows = usize::from(live.size().0);
    let len = history_len(live);
    tidy(&live_text(live, 0, len + rows - 1, 0, u16::MAX).0)
}

/// The text of the session's rows `first..=last`, counted from its oldest history row (history,
/// then screen), from column `from` of the first to `to` (inclusive) of the last; and whether
/// `last` wraps onto the next row. Reads a screenful at a time by scrolling, and scrolls back to
/// where it was.
fn live_text(
    live: &mut vt100::Screen,
    first: usize,
    last: usize,
    from: u16,
    to: u16,
) -> (String, bool) {
    let back = live.scrollback();
    let len = history_len(live);
    let rows = usize::from(live.size().0);
    let last = last.min(len + rows - 1);
    let mut text = String::new();
    let mut wrapped = false;
    let mut row = first;
    while row <= last {
        // `row` at the top of the view, or where it is on the screen.
        live.set_scrollback(len.saturating_sub(row));
        let at = row.saturating_sub(len);
        let n = (rows - at).min(last - row + 1);
        if row > first && !wrapped {
            text.push('\n');
        }
        let to_u16 = |r: usize| u16::try_from(r).unwrap_or(u16::MAX);
        let (piece, w) = visible_text(
            live,
            to_u16(at),
            to_u16(at + n - 1),
            if row == first { from } else { 0 },
            if row + n - 1 == last { to } else { u16::MAX },
        );
        text.push_str(&piece);
        wrapped = w;
        row += n;
    }
    live.set_scrollback(back);
    (text, wrapped)
}

/// The text of the rows `first..=last` the screen shows, from column `from` of the first to `to`
/// (inclusive) of the last, with the rows the terminal wrapped joined; and whether `last` wraps.
fn visible_text(
    screen: &vt100::Screen,
    first: u16,
    last: u16,
    from: u16,
    to: u16,
) -> (String, bool) {
    let cols = screen.size().1;
    let to = to.min(cols.saturating_sub(1));
    (screen.contents_between(first, from, last, to + 1), screen.row_wrapped(last))
}

// ---- panels --------------------------------------------------------------------------------

/// A panel's lines rendered whole at its width while it holds the selection: what the rows
/// scrolled out of view hold, and where each line wrapped.
pub struct Panel {
    pub rect: Rect,
    buf: Buffer,
    breaks: Vec<Option<String>>,
}

impl Panel {
    pub fn new(rect: Rect, lines: &[Line]) -> Self {
        let width = rect.width.max(1);
        let p = Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false });
        let height = u16::try_from(p.line_count(width)).unwrap_or(u16::MAX).max(1);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        p.render(buf.area, &mut buf);
        Self { rect, buf, breaks: wrap_breaks(lines, width) }
    }

    /// The symbols of the cells of row `row`, "" for those a wide character covers.
    pub fn row_symbols(&self, row: usize) -> Option<Vec<String>> {
        let area = self.buf.area;
        let y = u16::try_from(row).ok().filter(|&y| y < area.bottom())?;
        let mut covered = 0;
        Some(
            (0..area.width)
                .map(|x| {
                    let symbol = self.buf.cell((x, y)).map_or(" ", |c| c.symbol());
                    if covered > 0 {
                        covered -= 1;
                        return String::new();
                    }
                    covered = symbol.width().saturating_sub(1);
                    symbol.to_string()
                })
                .collect(),
        )
    }

    pub fn text(&self, start: Point, end: Point) -> String {
        let area = self.buf.area;
        let last = usize::from(area.bottom() - 1);
        if start.row > last {
            return String::new();
        }
        let pos =
            |p: Point| Position::new(p.col.min(area.width - 1), u16::try_from(p.row).unwrap_or(0));
        let end = if end.row > last {
            Position::new(area.width - 1, area.bottom() - 1)
        } else {
            pos(end)
        };
        buffer_text(&self.buf, area, pos(start), end, &self.breaks)
    }
}

/// The text from `start` to `end` (inclusive) of what was drawn in `rect`. `breaks` has an entry
/// per row of `rect`: `Some(separator)` where the row continues the one above it (a soft wrap),
/// `None` or missing for a new line.
pub fn buffer_text(
    buf: &Buffer,
    rect: Rect,
    start: Position,
    end: Position,
    breaks: &[Option<String>],
) -> String {
    let mut text = String::new();
    for y in start.y..=end.y {
        let from = if y == start.y { start.x } else { rect.left() };
        let to = if y == end.y { end.x } else { rect.right().saturating_sub(1) };
        let row = row_text(buf, y, from, to);
        let row = match breaks.get(usize::from(y - rect.y)) {
            Some(Some(sep)) if y > start.y => {
                text.push_str(sep);
                row.trim_start()
            }
            _ if y > start.y => {
                text.push('\n');
                &row
            }
            _ => &row,
        };
        text.push_str(row.trim_end());
    }
    tidy(&text)
}

pub fn highlight(buf: &mut Buffer, rect: Rect, start: Position, end: Position) {
    for y in start.y..=end.y {
        let from = if y == start.y { start.x } else { rect.left() };
        let to = if y == end.y { end.x } else { rect.right().saturating_sub(1) };
        for x in from..=to {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.modifier.insert(Modifier::REVERSED);
            }
        }
    }
}

/// Columns `from..=to` of row `y`, skipping the cells a wide character covers.
fn row_text(buf: &Buffer, y: u16, from: u16, to: u16) -> String {
    let mut row = String::new();
    let mut covered = 0;
    for x in from..=to {
        let Some(cell) = buf.cell((x, y)) else { continue };
        if covered > 0 {
            covered -= 1;
            continue;
        }
        row.push_str(cell.symbol());
        covered = cell.symbol().width().saturating_sub(1);
    }
    row
}

fn tidy(text: &str) -> String {
    text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim_end().to_string()
}

/// For each row `lines` take in a wrapping `Paragraph` `width` columns wide: `Some(separator)` if
/// the row continues the line above it, with the text the wrap dropped (a space, or nothing in a
/// word too long for the row), else `None`.
pub fn wrap_breaks(lines: &[Line], width: u16) -> Vec<Option<String>> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in lines {
        let p = Paragraph::new(line.clone()).wrap(Wrap { trim: false });
        let height = u16::try_from(p.line_count(width)).unwrap_or(u16::MAX).max(1);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        p.render(buf.area, &mut buf);
        let rows: Vec<String> = (0..height).map(|y| row_text(&buf, y, 0, width - 1)).collect();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        out.extend(soft_breaks(&text, &rows));
    }
    out
}

/// Lines up the rows one line of `text` was wrapped into with the text, to find what each soft
/// wrap dropped. A row that can't be found in the text (a tab, say) counts as a new line.
fn soft_breaks(text: &str, rows: &[String]) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(rows.len());
    let mut at = 0;
    for (i, row) in rows.iter().enumerate() {
        let row = if i == 0 { row.trim_end() } else { row.trim() };
        let rest = &text[at..];
        let start = if i == 0 { at } else { at + rest.len() - rest.trim_start().len() };
        if text[start..].starts_with(row) {
            out.push((i > 0).then(|| text[at..start].to_string()));
            at = start + row.len();
        } else {
            out.push(None);
            if let Some(found) = text[at..].find(row) {
                at += found + row.len();
            }
        }
    }
    out
}

// ---- clipboard -----------------------------------------------------------------------------

/// Where a copy went, as far as can be told.
#[derive(Debug, PartialEq)]
pub enum Copied {
    /// A clipboard command took it.
    Command,
    /// tmux took it, but its `set-clipboard` is off, so it stays in a tmux buffer.
    TmuxBuffer,
    /// No command worked: it goes out as OSC 52, which the terminal may or may not honour.
    Osc52,
}

pub fn toast(copied: &Copied, text: &str) -> String {
    described(copied, &format!("{} characters", text.chars().count()))
}

/// Where `what` went, for a toast.
pub fn described(copied: &Copied, what: &str) -> String {
    match copied {
        Copied::Command => format!("Copied {what}"),
        Copied::TmuxBuffer => format!("Copied {what} to a tmux buffer (its set-clipboard is off)"),
        Copied::Osc52 => format!("Sent {what} to the terminal's clipboard (OSC 52)"),
    }
}

#[derive(Default)]
struct Env {
    tmux: bool,
    macos: bool,
    wsl: bool,
    wayland: bool,
    x11: bool,
}

impl Env {
    fn current() -> Self {
        let set = |v: &str| std::env::var_os(v).is_some_and(|s| !s.is_empty());
        Env {
            tmux: set("TMUX"),
            macos: cfg!(target_os = "macos"),
            wsl: set("WSL_DISTRO_NAME")
                || std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists(),
            wayland: set("WAYLAND_DISPLAY"),
            x11: set("DISPLAY"),
        }
    }
}

const TMUX: &[&str] = &["tmux", "load-buffer", "-w", "-"];
const CLIP_EXE: &[&str] = &["clip.exe"];

/// The clipboard commands to try, in order. Inside tmux its buffer comes first: tmux passes it on
/// to the outer terminal itself (`-w`), which works over ssh too, and its default `set-clipboard
/// external` drops the OSC 52 a program inside it sends.
fn commands(env: &Env) -> Vec<&'static [&'static str]> {
    let mut out: Vec<&'static [&'static str]> = Vec::new();
    if env.tmux {
        out.push(TMUX);
    }
    if env.macos {
        out.push(&["pbcopy"]);
    }
    if env.wsl {
        out.push(CLIP_EXE);
    }
    if env.wayland {
        out.push(&["wl-copy"]);
    }
    if env.x11 {
        out.push(&["xclip", "-selection", "clipboard"]);
        out.push(&["xsel", "--clipboard", "--input"]);
    }
    out
}

/// What a command reads: clip.exe takes UTF-8 as the console's code page and garbles anything
/// beyond ASCII, but reads UTF-16LE (without a BOM, which it would keep) correctly.
fn input(cmd: &[&str], text: &str) -> Vec<u8> {
    if cmd == CLIP_EXE {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    } else {
        text.as_bytes().to_vec()
    }
}

/// Puts `text` on the clipboard with the first command that works. Runs commands, so call it off
/// the UI thread; the caller sends OSC 52 when this returns `Copied::Osc52`.
pub fn copy(text: &str) -> Copied {
    for cmd in commands(&Env::current()) {
        if !pipe(cmd, input(cmd, text)) {
            continue;
        }
        if cmd == TMUX && tmux_clipboard_off() {
            return Copied::TmuxBuffer;
        }
        return Copied::Command;
    }
    Copied::Osc52
}

fn tmux_clipboard_off() -> bool {
    crate::proc::run(
        Command::new("tmux").args(["show-options", "-gv", "set-clipboard"]),
        Duration::from_secs(2),
    )
    .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "off")
}

/// Runs `cmd` with `data` on its stdin and says whether it succeeded. Not `proc::run`: xclip and
/// wl-copy leave a process behind that holds the clipboard, and it would keep output pipes open.
fn pipe(cmd: &[&str], data: Vec<u8>) -> bool {
    use std::os::unix::process::CommandExt;
    let Ok(mut child) = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    else {
        return false;
    };
    // Written from its own thread, so a command that doesn't read can't hold this one past the
    // time limit; killing it ends the write.
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_c_copies_esc_clears_and_other_keys_go_on() {
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(key_action(&key(KeyCode::Char('c'), ctrl)), KeyAction::Copy);
        // ctrl+shift+c, after `keys::normalize`.
        assert_eq!(key_action(&key(KeyCode::Char('C'), ctrl)), KeyAction::Copy);
        assert_eq!(key_action(&key(KeyCode::Esc, KeyModifiers::NONE)), KeyAction::Clear);
        assert_eq!(key_action(&key(KeyCode::Char('c'), KeyModifiers::NONE)), KeyAction::Forward);
        assert_eq!(
            key_action(&key(KeyCode::Char('c'), ctrl | KeyModifiers::ALT)),
            KeyAction::Forward
        );
        assert_eq!(key_action(&key(KeyCode::Char('a'), ctrl)), KeyAction::Forward);
        assert_eq!(key_action(&key(KeyCode::Enter, KeyModifiers::NONE)), KeyAction::Forward);
    }

    #[test]
    fn releases_and_lone_modifiers_keep_the_selection() {
        let mut release = key(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_action(&release), KeyAction::Keep);
        let shift = key(
            KeyCode::Modifier(crossterm::event::ModifierKeyCode::LeftShift),
            KeyModifiers::SHIFT,
        );
        assert_eq!(key_action(&shift), KeyAction::Keep);
        // Pressing ctrl on its way to ctrl+c mustn't end the selection first.
        let ctrl = key(
            KeyCode::Modifier(crossterm::event::ModifierKeyCode::LeftControl),
            KeyModifiers::CONTROL,
        );
        assert_eq!(key_action(&ctrl), KeyAction::Keep);
    }

    #[test]
    fn span_is_in_reading_order() {
        let at = |row, col| Point { row, col };
        let rect = Rect::new(0, 0, 10, 5);
        let mut s = Selection::new(Pane::Jira, rect, at(30, 5), Position::new(5, 3), None);
        assert!(!s.dragged());
        s.head = at(28, 8);
        assert!(s.dragged());
        assert_eq!(s.span(), (at(28, 8), at(30, 5)));
    }

    /// A session's parser with `lines` lines printed: "line 0", "line 1", ...
    fn session(rows: u16, cols: u16, scrollback: usize, lines: usize) -> Arc<Mutex<Parser>> {
        let mut p = vt100::Parser::new_with_callbacks(
            rows,
            cols,
            scrollback,
            crate::session::Callbacks::default(),
        );
        for i in 0..lines {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        Arc::new(Mutex::new(p))
    }

    fn print(parser: &Arc<Mutex<Parser>>, lines: std::ops::Range<usize>) {
        for i in lines {
            lock(parser).process(format!("more {i}\r\n").as_bytes());
        }
    }

    fn view_rows(snap: &Snapshot) -> Vec<String> {
        snap.view()
            .rows
            .iter()
            .map(|r| r.iter().map(vt100::Cell::contents).collect::<String>().trim_end().into())
            .collect()
    }

    fn at(row: usize, col: u16) -> Point {
        Point { row, col }
    }

    #[test]
    fn a_selection_keeps_its_text_while_the_view_scrolls() {
        // 30 lines on a 5-row screen: "line k" is content row k; 26 rows are history.
        let parser = session(5, 20, 100, 30);
        let mut snap = Snapshot::new(parser.clone());
        assert_eq!(snap.top(), 26);
        assert_eq!(view_rows(&snap), ["line 26", "line 27", "line 28", "line 29", ""]);
        let (start, end) = (at(20, 2), at(27, 3));
        let text = "ne 20\nline 21\nline 22\nline 23\nline 24\nline 25\nline 26\nline";
        assert_eq!(snap.text(start, end), text);
        // Fifty rows up the wheel stops at the oldest row; the text is the same.
        for _ in 0..17 {
            snap.scroll(-3);
        }
        assert_eq!(snap.top(), 0);
        assert!(!snap.scroll(-3));
        assert_eq!(view_rows(&snap)[0], "line 0");
        assert_eq!(snap.text(start, end), text);
        // Part history, part screen.
        snap.scroll(24);
        assert_eq!(view_rows(&snap), ["line 24", "line 25", "line 26", "line 27", "line 28"]);
        assert!(snap.scroll(10));
        assert_eq!(snap.top(), 26);
    }

    #[test]
    fn output_while_selecting_moves_neither_the_view_nor_the_text() {
        let parser = session(5, 20, 100, 30);
        let mut snap = Snapshot::new(parser.clone());
        snap.scroll(-10);
        let before = view_rows(&snap);
        print(&parser, 0..7);
        assert!(snap.track());
        assert_eq!(view_rows(&snap), before);
        assert_eq!(snap.text(at(16, 0), at(28, 6)).lines().last(), Some("line 28"));
        assert_eq!(snap.text(at(29, 0), at(29, 19)), "line 29");
    }

    #[test]
    fn rows_dropped_off_a_full_history_keep_the_selection_on_its_text() {
        // A history of 20 rows, full: "line k" is content row k - 16.
        let parser = session(5, 20, 20, 40);
        let mut snap = Snapshot::new(parser.clone());
        snap.scroll(-15);
        let before = view_rows(&snap);
        assert_eq!(before[0], "line 21");
        let (start, end) = (at(6, 0), at(8, 6));
        assert_eq!(snap.text(start, end), "line 22\nline 23\nline 24");
        print(&parser, 0..3);
        assert!(snap.track());
        print(&parser, 3..5);
        assert!(snap.track());
        assert_eq!(snap.dropped, 5);
        assert_eq!(view_rows(&snap), before);
        assert_eq!(snap.text(start, end), "line 22\nline 23\nline 24");
        // Once its rows are gone, the view starts at the oldest row left.
        print(&parser, 5..12);
        assert!(snap.track());
        assert_eq!(snap.top(), 12);
        assert_eq!(view_rows(&snap)[0], "line 28");
        assert_eq!(snap.text(start, end), "");
    }

    #[test]
    fn clearing_a_selection_leaves_the_session_where_the_view_was() {
        let parser = session(5, 20, 100, 30);
        let mut snap = Snapshot::new(parser.clone());
        snap.scroll(-10);
        print(&parser, 0..4);
        snap.track();
        drop(snap);
        let p = lock(&parser);
        assert_eq!(p.screen().scrollback(), 14);
        assert_eq!(p.screen().contents().lines().next(), Some("line 16"));
        drop(p);
        // From the bottom, back to the bottom.
        lock(&parser).screen_mut().set_scrollback(0);
        drop(Snapshot::new(parser.clone()));
        assert_eq!(lock(&parser).screen().scrollback(), 0);
    }

    #[test]
    fn a_snapshot_starts_where_the_session_was_scrolled_to() {
        let parser = session(5, 20, 100, 30);
        lock(&parser).screen_mut().set_scrollback(4);
        let snap = Snapshot::new(parser.clone());
        assert_eq!(snap.top(), 22);
        assert_eq!(view_rows(&snap)[0], "line 22");
        print(&parser, 0..2);
        // The screen was copied as it was at the bottom, not as scrolled.
        assert_eq!(snap.text(at(29, 0), at(29, 9)), "line 29");
    }

    #[test]
    fn the_alternate_screen_has_only_its_screen() {
        let parser = session(5, 20, 100, 30);
        lock(&parser).process(b"\x1b[?1049h\x1b[Hfull screen");
        let mut snap = Snapshot::new(parser.clone());
        assert_eq!(snap.top(), 0);
        assert!(!snap.scroll(-3));
        assert_eq!(snap.text(at(0, 0), at(0, 19)), "full screen");
        // Back on the normal screen, its rows aren't these.
        lock(&parser).process(b"\x1b[?1049l");
        assert!(!snap.track());
    }

    #[test]
    fn session_text_joins_wrapped_rows_across_screenfuls() {
        let parser = session(3, 10, 100, 0);
        let long = "0123456789abcdefghijklmnopqrstuvwxyz";
        lock(&parser).process(format!("first\r\n{long}\r\n中文 x   \r\nlast").as_bytes());
        lock(&parser).screen_mut().set_scrollback(1);
        let mut p = lock(&parser);
        assert_eq!(session_text(p.screen_mut()), format!("first\n{long}\n中文 x\nlast"));
        assert_eq!(p.screen().scrollback(), 1);
    }

    #[test]
    fn points_and_edges() {
        let rect = Rect::new(10, 5, 20, 4);
        assert_eq!(point_at(rect, 100, Position::new(12, 6)), at(101, 2));
        // Outside the pane, the nearest cell in it.
        assert_eq!(point_at(rect, 100, Position::new(0, 0)), at(100, 0));
        assert_eq!(point_at(rect, 100, Position::new(50, 30)), at(103, 19));
        assert_eq!(edge_step(rect, Position::new(12, 5)), 0);
        assert_eq!(edge_step(rect, Position::new(12, 8)), 0);
        assert_eq!(edge_step(rect, Position::new(12, 4)), -1);
        assert_eq!(edge_step(rect, Position::new(12, 9)), 1);
        assert_eq!(edge_step(rect, Position::new(12, 11)), 3);
        assert_eq!(edge_step(rect, Position::new(12, 40)), MAX_EDGE_STEP);
    }

    #[test]
    fn on_screen_cuts_the_selection_to_the_view() {
        let rect = Rect::new(10, 5, 20, 4);
        // Rows 100..104 are shown.
        assert_eq!(
            on_screen(rect, 100, at(101, 3), at(102, 4)),
            Some((Position::new(13, 6), Position::new(14, 7)))
        );
        assert_eq!(
            on_screen(rect, 100, at(90, 3), at(150, 4)),
            Some((Position::new(10, 5), Position::new(29, 8)))
        );
        assert_eq!(on_screen(rect, 100, at(90, 3), at(99, 4)), None);
        assert_eq!(on_screen(rect, 100, at(104, 0), at(110, 4)), None);
    }

    #[test]
    fn a_panel_reads_rows_scrolled_out_of_view() {
        let lines: Vec<Line> =
            (0..30).map(|i| Line::from(format!("line {i:02} of the text"))).collect();
        let panel = Panel::new(Rect::new(40, 2, 12, 5), &lines);
        // Each line wraps onto two rows: "line 03 of", "the text".
        assert_eq!(panel.text(at(6, 0), at(7, 11)), "line 03 of the text");
        assert_eq!(panel.text(at(57, 0), at(500, 0)), "the text\nline 29 of the text");
        assert_eq!(panel.text(at(500, 0), at(600, 0)), "");
    }

    #[test]
    fn soft_breaks_find_what_each_wrap_dropped() {
        let rows = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            soft_breaks("the quick brown fox", &rows(&["the quick ", "brown fox"])),
            vec![None, Some(" ".into())]
        );
        // A word longer than the row breaks without a space.
        assert_eq!(
            soft_breaks(
                "see https://example.com/abcdef",
                &rows(&["see", "https://ex", "ample.com/", "abcdef"])
            ),
            vec![None, Some(" ".into()), Some("".into()), Some("".into())]
        );
        // Indentation stays with the first row.
        assert_eq!(
            soft_breaks("  - item one", &rows(&["  - item", "one"])),
            vec![None, Some(" ".into())]
        );
        // A row that isn't in the text is taken as a new line.
        assert_eq!(soft_breaks("a\tb", &rows(&["a", "zz"])), vec![None, None]);
    }

    #[test]
    fn wrap_breaks_match_how_a_paragraph_wraps() {
        let lines = vec![
            Line::from("the quick brown fox jumps"),
            Line::from(""),
            Line::from("https://example.com/a-long-path"),
            Line::from("short"),
        ];
        let breaks = wrap_breaks(&lines, 10);
        let height = Paragraph::new(lines.clone()).wrap(Wrap { trim: false }).line_count(10);
        assert_eq!(breaks.len(), height);
        // Every soft row joined with its separator gives back the lines.
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, u16::try_from(height).unwrap()));
        Paragraph::new(lines.clone()).wrap(Wrap { trim: false }).render(buf.area, &mut buf);
        let text = buffer_text(
            &buf,
            buf.area,
            Position::new(0, 0),
            Position::new(9, buf.area.height - 1),
            &breaks,
        );
        assert_eq!(text, "the quick brown fox jumps\n\nhttps://example.com/a-long-path\nshort");
    }

    #[test]
    fn buffer_text_reads_part_of_a_row_and_wide_characters_once() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 2));
        buf.set_string(0, 0, "ab 中文 cd", Style::new());
        buf.set_string(0, 1, "next  ", Style::new());
        let all = buf.area;
        assert_eq!(
            buffer_text(&buf, all, Position::new(0, 0), Position::new(11, 1), &[]),
            "ab 中文 cd\nnext"
        );
        assert_eq!(buffer_text(&buf, all, Position::new(3, 0), Position::new(6, 0), &[]), "中文");
        // From inside a row to the start of the next, joined where the row was soft-wrapped.
        let breaks = [None, Some(" ".to_string())];
        assert_eq!(
            buffer_text(&buf, all, Position::new(8, 0), Position::new(1, 1), &breaks),
            "cd ne"
        );
    }

    #[test]
    fn highlight_reverses_the_selected_cells_only() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        let all = buf.area;
        highlight(&mut buf, all, Position::new(2, 0), Position::new(0, 1));
        let reversed = |x, y| buf[(x, y)].modifier.contains(Modifier::REVERSED);
        assert!(
            !reversed(1, 0)
                && reversed(2, 0)
                && reversed(3, 0)
                && reversed(0, 1)
                && !reversed(1, 1)
        );
    }

    #[test]
    fn clipboard_commands_by_environment() {
        let names = |env: Env| commands(&env).iter().map(|c| c[0]).collect::<Vec<_>>();
        assert!(names(Env::default()).is_empty());
        assert_eq!(names(Env { tmux: true, macos: true, ..Env::default() }), ["tmux", "pbcopy"]);
        // WSLg sets both display variables too; clip.exe reaches Windows' clipboard directly.
        assert_eq!(
            names(Env { wsl: true, wayland: true, x11: true, ..Env::default() }),
            ["clip.exe", "wl-copy", "xclip", "xsel"]
        );
    }

    #[test]
    fn clip_exe_gets_utf16() {
        assert_eq!(input(CLIP_EXE, "é"), vec![0xe9, 0x00]);
        assert_eq!(input(TMUX, "é"), "é".as_bytes());
    }

    #[test]
    fn toasts_say_where_the_text_went() {
        assert_eq!(toast(&Copied::Command, "héllo"), "Copied 5 characters");
        assert_eq!(
            toast(&Copied::Osc52, "hi"),
            "Sent 2 characters to the terminal's clipboard (OSC 52)"
        );
    }

    fn cells(text: &str) -> Vec<String> {
        text.chars()
            .flat_map(|c| {
                let wide = unicode_width::UnicodeWidthChar::width(c) == Some(2);
                std::iter::once(c.to_string()).chain(wide.then(String::new))
            })
            .collect()
    }

    /// The word at the first `^` of `text`, as text.
    fn word(text: &str) -> Option<String> {
        let (before, after) = text.split_once('^')?;
        let row = cells(&format!("{before}{after}"));
        let col = u16::try_from(cells(before).len()).unwrap();
        let (from, to) = word_at(&row, col)?;
        Some(row[usize::from(from)..=usize::from(to)].concat())
    }

    #[test]
    fn a_word_is_what_terminals_copy() {
        assert_eq!(word("see E^NG-52 now").as_deref(), Some("ENG-52"));
        assert_eq!(word("at src/app/mouse.rs:2^96 in").as_deref(), Some("src/app/mouse.rs:296"));
        assert_eq!(word("cd ^~/taskman-ENG-52").as_deref(), Some("~/taskman-ENG-52"));
        assert_eq!(word("mail you@exa^mple.com").as_deref(), Some("you@example.com"));
        assert_eq!(
            word("open https://example.com/a?b=1&c^#d now").as_deref(),
            Some("https://example.com/a?b=1&c#d")
        );
        // The first and last cells of the row.
        assert_eq!(word("^abc").as_deref(), Some("abc"));
        assert_eq!(word("abc^"), None);
    }

    #[test]
    fn delimiters_and_sentence_punctuation_end_a_word() {
        assert_eq!(word("say (ENG^-52), then").as_deref(), Some("ENG-52"));
        assert_eq!(word("\"a^b\" 'c' `d`").as_deref(), Some("ab"));
        assert_eq!(word("a|^b;c <d>").as_deref(), Some("b"));
        assert_eq!(word("done in ENG^-52.").as_deref(), Some("ENG-52"));
        assert_eq!(word("rea^lly?!").as_deref(), Some("really"));
        assert_eq!(word("v1.2.^3: next").as_deref(), Some("v1.2.3"));
        assert_eq!(word("e.^g. x").as_deref(), Some("e.g"));
        // Clicking on the punctuation itself, a delimiter or a blank selects nothing.
        assert_eq!(word("done.^ ok"), None);
        assert_eq!(word("done^. ok").as_deref(), None);
        assert_eq!(word("wait ^... ok"), None);
        assert_eq!(word("^(a)"), None);
        assert_eq!(word("a ^ b"), None);
        assert_eq!(word("a|b^;c"), None);
    }

    #[test]
    fn a_wide_character_counts_as_one() {
        assert_eq!(word("日本^語 text").as_deref(), Some("日本語"));
        assert_eq!(word("x 日^本語.").as_deref(), Some("日本語"));
        let row = cells("a 日本 b");
        // The cell a wide character covers selects it.
        assert_eq!(word_at(&row, 4), Some((2, 5)));
        assert_eq!(word_at(&row, 3), Some((2, 5)));
    }

    #[test]
    fn blank_and_out_of_range_cells_select_nothing() {
        let row = cells("ab  cd");
        assert_eq!(word_at(&row, 2), None);
        assert_eq!(word_at(&row, 3), None);
        assert_eq!(word_at(&row, 40), None);
        assert_eq!(word_at(&[], 0), None);
    }

    #[test]
    fn a_line_is_its_text_trimmed() {
        assert_eq!(line_at(&cells("  > run it  ")), Some((2, 9)));
        assert_eq!(line_at(&cells("   ")), None);
        assert_eq!(line_at(&[]), None);
        assert_eq!(line_at(&cells("ab 日本")), Some((0, 6)));
        assert_eq!(grab_span(&cells("a b"), Grab::Line, 1), Some((0, 2)));
        assert_eq!(grab_span(&cells("a b"), Grab::Word, 0), Some((0, 0)));
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

        #[test]
        fn a_word_has_no_whitespace_and_holds_the_clicked_cell(
            text in "[a-zA-Z0-9 .,:;!?/_~@#()\\[\\]<>|'\"-]{0,40}日{0,2}[a-z .]{0,5}",
            col in 0u16..60,
        ) {
            let row = cells(&text);
            if let Some((from, to)) = word_at(&row, col) {
                proptest::prop_assert!(from <= to && usize::from(to) < row.len());
                let word = row[usize::from(from)..=usize::from(to)].concat();
                proptest::prop_assert!(!word.is_empty());
                proptest::prop_assert!(!word.chars().any(char::is_whitespace), "{word:?}");
                // The clicked cell, or the wide character it covers, is inside.
                proptest::prop_assert!(from <= col && col <= to, "{from} {col} {to} {text:?}");
                proptest::prop_assert!(!is_trailing(&row[usize::from(to)]));
            }
        }
    }
}
