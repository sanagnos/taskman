//! Links in a session and in the tracker and GitHub panels. vt100 (our fork, see Cargo.toml)
//! keeps the OSC 8 link each cell was printed with; drawing a session writes it back out around
//! those cells, so the user's terminal opens them, and `prefix l` lists them. The panels' links
//! (the issue, the PR, checks, reviews, URLs in their text) are written out around their cells the
//! same way. See "Links in sessions" and "Links in panels" in docs/design.md.

use crate::github::{CheckState, Pr};
use crate::tracker::Issue;
use ratatui::buffer::{Buffer, Cell, CellDiffOption, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::io::Write;
use std::num::NonZeroU16;

/// The longest URL taskman passes on or lists, as Codex does: a linked cell repeats it.
pub const MAX_URL: usize = 8 * 1024;
/// The most links `prefix l` lists.
const LISTED: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub url: String,
    /// The text the link showed, when it isn't the URL itself.
    pub label: Option<String>,
}

impl Link {
    /// A label that is the URL, or its start (with or without the scheme), says nothing more. It
    /// may be broken over rows, so it's compared without spaces.
    fn new(url: &str, label: &str) -> Self {
        let words: Vec<&str> = label.split_whitespace().collect();
        let joined = words.concat();
        let bare = url.split_once("://").map_or(url, |(_, rest)| rest);
        let says_more =
            !words.is_empty() && !url.starts_with(&joined) && !bare.starts_with(&joined);
        Self { url: url.to_string(), label: says_more.then(|| words.join(" ")) }
    }
}

/// Only http(s) links are opened, or passed on to the terminal: a session's output decides the
/// URL, and other schemes (`file:`, editor and custom ones) can start programs. As in Codex, the
/// URL must also name a host: `http://`, `https:///x` and `https://:80` don't.
pub fn openable(url: &str) -> bool {
    if url.len() > MAX_URL || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.strip_prefix('[') {
        // An IPv6 address, then perhaps a port.
        Some(v6) => match v6.split_once(']') {
            Some((addr, port)) if (port.is_empty() || is_port(port)) => {
                return !addr.is_empty()
                    && addr.chars().all(|c| c.is_ascii_hexdigit() || ":.".contains(c));
            }
            _ => return false,
        },
        None => match host_port.rsplit_once(':') {
            Some((host, port)) if is_port(port) => host,
            Some(_) => return false,
            None => host_port,
        },
    };
    !host.is_empty() && host.chars().all(|c| c.is_alphanumeric() || "-._~%".contains(c))
}

/// `:` and a port number, or a bare `:`.
fn is_port(port: &str) -> bool {
    let port = port.strip_prefix(':').unwrap_or(port);
    port.chars().all(|c| c.is_ascii_digit()) && port.len() <= 5
}

/// Wraps each cell of `area` that shows an http(s) link of `screen` (drawn there by tui-term) in
/// OSC 8, as Codex does: the terminal opens it on a click, while ratatui still counts the cell's
/// width rather than the escapes'. `Runs` then sends each link once per run of cells.
///
/// Anything drawn over the pane afterwards must render `Clear` first: ratatui's `set_symbol`
/// keeps a cell's `diff_option`, so a cell drawn over would keep the link and its forced width.
pub fn mark(buf: &mut Buffer, area: Rect, screen: &vt100::Screen) {
    for row in 0..area.height {
        if !screen.row_has_hyperlinks(row) {
            continue;
        }
        // The last link met on the row and its opening escape, if it's openable: a link's cells
        // usually come in runs, so the check and the escape are done once a run. The screen keeps
        // each link once, so the same link is the same reference.
        let mut last: Option<(&vt100::Hyperlink, Option<String>)> = None;
        for col in 0..area.width {
            let Some(link) = screen.cell_hyperlink(row, col) else { continue };
            if !last.as_ref().is_some_and(|(l, _)| std::ptr::eq(*l, link)) {
                last =
                    Some((link, openable(link.uri()).then(|| opening(&params(link), link.uri()))));
            }
            let Some((_, Some(open))) = &last else { continue };
            // The right half of a wide character is drawn with its left.
            if screen.cell(row, col).is_some_and(vt100::Cell::is_wide_continuation) {
                continue;
            }
            wrap(&mut buf[(area.x + col, area.y + row)], open);
        }
    }
}

const CLOSE: &str = "\x1b]8;;\x07";

fn opening(params: &str, url: &str) -> String {
    format!("\x1b]8;{params};{url}\x07")
}

/// The cell's symbol inside OSC 8 (`open` is the opening escape), its width forced to the
/// symbol's rather than the escapes'.
fn wrap(cell: &mut Cell, open: &str) {
    if cell.diff_option == CellDiffOption::Skip {
        return;
    }
    let width = NonZeroU16::new(cell.cell_width()).unwrap_or(NonZeroU16::MIN);
    let mut symbol = String::with_capacity(open.len() + cell.symbol().len() + CLOSE.len());
    symbol.push_str(open);
    symbol.push_str(cell.symbol());
    symbol.push_str(CLOSE);
    cell.set_symbol(&symbol).set_diff_option(CellDiffOption::ForcedWidth(width));
}

/// The text a cell shows: its symbol without the OSC 8 escapes `wrap` put around it.
pub fn shown(symbol: &str) -> &str {
    let text = match symbol.strip_prefix("\x1b]8;") {
        Some(rest) => rest.split_once('\x07').map_or(rest, |(_, text)| text),
        None => symbol,
    };
    text.strip_suffix(CLOSE).unwrap_or(text)
}

/// The terminal's writer, sending each link once per run of cells rather than once per cell:
/// `mark` wraps every cell on its own, since ratatui counts a cell's width only that way, so a
/// close followed by an opening of the same link, with only cursor moves and colours between them,
/// is left out (`join_runs`), as tmux and tcell send OSC 8 only when the link changes. Output is
/// kept until `flush`, which ratatui calls after each frame, so a run is joined across writes, and
/// every link sent in a frame is closed by its end.
pub struct Runs<W: Write> {
    inner: W,
    frame: Vec<u8>,
    out: Vec<u8>,
}

impl<W: Write> Runs<W> {
    pub fn new(inner: W) -> Self {
        Self { inner, frame: Vec::new(), out: Vec::new() }
    }
}

impl<W: Write> Write for Runs<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.frame.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.frame.is_empty() {
            self.out.clear();
            join_runs(&self.frame, &mut self.out);
            self.frame.clear();
            self.inner.write_all(&self.out)?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Drop for Runs<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// `input` with each close of an OSC 8 link left out when the same link is opened again before
/// anything is printed: only cursor moves and colours (`quiet`) come between, which leave the link
/// open. The opening escapes are `mark`'s, ended by BEL.
fn join_runs(input: &[u8], out: &mut Vec<u8>) {
    // The opening escape of the link the terminal has open.
    let mut open: Option<&[u8]> = None;
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        if let Some(link) = open
            && rest.starts_with(CLOSE.as_bytes())
        {
            let mut j = i + CLOSE.len();
            while let Some(n) = quiet(&input[j..]) {
                j += n;
            }
            if input[j..].starts_with(link) {
                out.extend_from_slice(&input[i + CLOSE.len()..j]);
                i = j + link.len();
            } else {
                out.extend_from_slice(CLOSE.as_bytes());
                open = None;
                i += CLOSE.len();
            }
        } else if rest.starts_with(b"\x1b]8;")
            && let Some(end) = rest.iter().position(|&b| b == 0x07)
        {
            let escape = &rest[..=end];
            open = (escape != CLOSE.as_bytes()).then_some(escape);
            out.extend_from_slice(escape);
            i += escape.len();
        } else {
            // Up to the next escape.
            let n = rest[1..].iter().position(|&b| b == 0x1b).map_or(rest.len(), |p| p + 1);
            out.extend_from_slice(&rest[..n]);
            i += n;
        }
    }
}

/// The length of the escape `bytes` starts with, if it's one ratatui writes between cells that
/// prints nothing: a cursor move (CUP) or colours and attributes (SGR).
fn quiet(bytes: &[u8]) -> Option<usize> {
    let params = bytes.strip_prefix(b"\x1b[")?;
    let n = params.iter().position(|b| !(b.is_ascii_digit() || *b == b';' || *b == b':'))?;
    matches!(params[n], b'm' | b'H').then_some(n + 3)
}

/// A panel's lines, and the spans in them that link somewhere. The panel draws `lines` as any
/// other; drawn again from `coded`, the same text shows each link's number where wrapping and
/// scrolling put it, and `mark_coded` wraps those cells in OSC 8. Only http(s) URLs are linked.
#[derive(Default)]
pub struct Linked {
    pub lines: Vec<Line<'static>>,
    /// The line, the span in it, and the URL's index in `urls`.
    spans: Vec<(usize, usize, usize)>,
    urls: Vec<String>,
}

impl Linked {
    pub fn push(&mut self, line: impl Into<Line<'static>>) {
        self.lines.push(line.into());
    }

    /// Links span `span` of the last line pushed to `url`.
    pub fn link(&mut self, span: usize, url: &str) {
        if self.lines.is_empty() || !openable(url) {
            return;
        }
        let i = self.urls.iter().position(|u| u == url).unwrap_or_else(|| {
            self.urls.push(url.to_string());
            self.urls.len() - 1
        });
        self.spans.push((self.lines.len() - 1, span, i));
    }

    /// A line of a description, comment or review, after `pad`: cleaned of control characters,
    /// with the http(s) URLs in it underlined and linked.
    pub fn text(&mut self, pad: &str, text: &str) {
        let text = crate::untrusted::clean(text);
        let mut spans = vec![Span::raw(pad.to_string())];
        let mut urls = Vec::new();
        let mut at = 0;
        for r in find_urls(&text) {
            spans.push(Span::raw(text[at..r.start].to_string()));
            urls.push((spans.len(), text[r.clone()].to_string()));
            spans.push(Span::styled(
                text[r.clone()].to_string(),
                Style::new().add_modifier(Modifier::UNDERLINED),
            ));
            at = r.end;
        }
        spans.push(Span::raw(text[at..].to_string()));
        self.push(spans);
        for (span, url) in urls {
            self.link(span, &url);
        }
    }

    /// Adds `other`'s lines and links after these.
    pub fn append(&mut self, other: Linked) {
        let lines = self.lines.len();
        for (line, span, url) in other.spans {
            let i = self.urls.iter().position(|u| *u == other.urls[url]).unwrap_or_else(|| {
                self.urls.push(other.urls[url].clone());
                self.urls.len() - 1
            });
            self.spans.push((lines + line, span, i));
        }
        self.lines.extend(other.lines);
    }

    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    /// The same text, unstyled but for each linked span, coloured by its URL's number (its index
    /// plus `base` plus one), so where it was drawn can be read back from the cells.
    pub fn coded(&self, base: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = self
            .lines
            .iter()
            .map(|l| {
                let spans =
                    l.spans.iter().map(|s| Span::raw(s.content.clone())).collect::<Vec<_>>();
                Line { spans, alignment: l.alignment, ..Default::default() }
            })
            .collect();
        for &(line, span, url) in &self.spans {
            if let Some(s) = lines[line].spans.get_mut(span) {
                s.style = Style::new().fg(code(base + url + 1));
            }
        }
        lines
    }
}

fn code(n: usize) -> Color {
    let [_, r, g, b] = (u32::try_from(n).unwrap_or(0) & 0xff_ffff).to_be_bytes();
    Color::Rgb(r, g, b)
}

/// Wraps each cell of `area` in `buf` in OSC 8 for the URL whose number (see `Linked::coded`) the
/// same cell of `coded` shows: the same widgets drawn from the coded lines. Like `mark`, anything
/// drawn over these cells afterwards must render `Clear` first.
pub fn mark_coded(buf: &mut Buffer, coded: &Buffer, area: Rect, urls: &[&str]) {
    let opens: Vec<String> = urls.iter().map(|url| opening(&id(url), url)).collect();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let Some(Color::Rgb(r, g, b)) = coded.cell((x, y)).map(|c| c.fg) else { continue };
            let n = (usize::from(r) << 16) | (usize::from(g) << 8) | usize::from(b);
            let Some(open) = n.checked_sub(1).and_then(|i| opens.get(i)) else { continue };
            if let Some(cell) = buf.cell_mut((x, y)) {
                wrap(cell, open);
            }
        }
    }
}

/// An id made from the URL, so the terminal treats a link's cells, each wrapped on its own, as one.
fn id(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    url.hash(&mut h);
    format!("id=taskman-{:x}", h.finish())
}

/// The links in a ticket's tracker and GitHub panels, for `prefix l`: the issue and the PRs that
/// close it, the PR, failing
/// then running checks, reviews, unresolved threads, PR comments, the URLs in their text, and
/// passing checks last. Each URL once, http(s) only.
pub fn panel(
    key: &str,
    issue_url: Option<&str>,
    issue: Option<&Issue>,
    pr: Option<&Pr>,
) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    let mut add = |url: &str, label: Option<String>| {
        if openable(url) && out.len() < LISTED && !out.iter().any(|l| l.url == url) {
            out.push(Link { url: url.to_string(), label });
        }
    };
    let mut texts: Vec<&str> = Vec::new();
    if let Some(url) = issue_url {
        add(url, Some(issue.map_or_else(|| key.to_string(), |i| format!("{key} · {}", i.summary))));
    }
    if let Some(i) = issue {
        for p in &i.prs {
            add(
                &p.url,
                Some(format!("PR #{} ({}), closing {key}", p.number, p.state.to_lowercase())),
            );
        }
        texts.push(&i.description);
        texts.extend(i.comments.iter().map(|c| c.body.as_str()));
    }
    if let Some(pr) = pr {
        add(&pr.url, Some(format!("PR #{} · {}", pr.number, pr.title)));
        for (state, word) in
            [(CheckState::Fail, "Failing check"), (CheckState::Pending, "Running check")]
        {
            for c in pr.checks.iter().filter(|c| c.state == state) {
                add(&c.url, Some(format!("{word}: {}", c.name)));
            }
        }
        for r in pr.latest_reviews() {
            add(
                &r.url,
                Some(format!(
                    "Review by {} ({})",
                    r.author,
                    r.state.to_lowercase().replace('_', " ")
                )),
            );
        }
        texts.extend(pr.review_text().iter().map(|r| r.body.as_str()));
        if let Ok(t) = &pr.threads {
            for th in t.unresolved() {
                let place = th.line.map_or(th.path.clone(), |l| format!("{}:{l}", th.path));
                if let Some(c) = th.comments.first() {
                    add(&c.url, Some(format!("Thread on {place}")));
                }
                texts.extend(th.comments.iter().map(|c| c.body.as_str()));
            }
        }
        for c in pr.comments.iter().take(SHOWN_COMMENTS) {
            add(&c.url, Some(format!("Comment by {}", c.author)));
        }
        texts.extend(pr.comments.iter().take(SHOWN_COMMENTS).map(|c| c.body.as_str()));
    }
    for text in texts {
        for r in find_urls(text) {
            add(&text[r], None);
        }
    }
    if let Some(pr) = pr {
        for c in pr.checks.iter().filter(|c| c.state == CheckState::Pass) {
            add(&c.url, Some(format!("Check: {}", c.name)));
        }
    }
    out
}

/// The PR comments the GitHub panel shows, newest first, when none are new.
pub const SHOWN_COMMENTS: usize = 5;

/// The link's id passed on, so the terminal treats its cells, each wrapped on its own, as one link
/// (underlined together on hover); left out when it holds what the parameters can't.
fn params(link: &vt100::Hyperlink) -> String {
    match link.id() {
        Some(id) if id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) => {
            format!("id={id}")
        }
        _ => String::new(),
    }
}

/// The session's links, newest first, once each: those on its screen and in its history (the
/// alternate screen keeps none). A link's text is joined across the rows it spans. Plain http(s)
/// URLs in the text are found too, in lines joined as `Lines` does.
pub fn list(screen: &mut vt100::Screen) -> Vec<Link> {
    let mut spans: Vec<Link> = Vec::new();
    let mut lines = Lines::default();
    // The link being read, its text so far, and whether a gap (a space or a row's end) came since.
    let mut open: Option<(vt100::Hyperlink, String, bool)> = None;
    let end = |open: &mut Option<(vt100::Hyperlink, String, bool)>, spans: &mut Vec<Link>| {
        if let Some((link, text, _)) = open.take() {
            spans.push(Link::new(link.uri(), &text));
        }
    };
    // Each row is read once, for its links and its text.
    let mut row_text = String::new();
    each_row(screen, |screen, row| {
        let cols = crate::term::size(screen).1;
        let linked = screen.row_has_hyperlinks(row);
        // The columns up to the last that isn't blank.
        let mut used = 0;
        row_text.clear();
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else { break };
            let text = cell.contents();
            if !cell.is_wide_continuation() {
                match text {
                    "" => row_text.push(' '),
                    s => {
                        row_text.push_str(s);
                        if s != " " {
                            used = col + if cell.is_wide() { 2 } else { 1 };
                        }
                    }
                }
            }
            match linked.then(|| screen.cell_hyperlink(row, col)).flatten() {
                Some(link) if link.uri().len() <= MAX_URL => match &mut open {
                    Some((l, label, gap)) if l == link => {
                        if std::mem::take(gap) {
                            label.push(' ');
                        }
                        label.push_str(text);
                    }
                    _ => {
                        end(&mut open, &mut spans);
                        open = Some((link.clone(), text.to_string(), false));
                    }
                },
                // Blank cells between parts of a link (an indent, the space Claude moves over)
                // don't end it; other text does.
                _ if text.trim().is_empty() => {
                    if let Some((_, _, gap)) = &mut open {
                        *gap = true;
                    }
                }
                _ => end(&mut open, &mut spans),
            }
        }
        if let Some((_, _, gap)) = &mut open {
            *gap = true;
        }
        if let Some(line) = lines.add(&row_text, used, cols, screen.row_wrapped(row)) {
            spans.extend(plain_urls(&line));
        }
    });
    end(&mut open, &mut spans);
    if let Some(line) = lines.finish() {
        spans.extend(plain_urls(&line));
    }

    let mut links: Vec<Link> = Vec::new();
    for span in spans.into_iter().rev() {
        match links.iter().position(|l| l.url == span.url) {
            // Codex shows "text (url)", both linked: keep the text.
            Some(i) => {
                if links[i].label.is_none() {
                    links[i].label = span.label;
                }
            }
            None if links.len() < LISTED => links.push(span),
            None => {}
        }
    }
    links
}

fn plain_urls(line: &str) -> impl Iterator<Item = Link> + '_ {
    find_urls(line).into_iter().map(|r| Link::new(&line[r], ""))
}

/// Rows joined into lines of text, for finding URLs: rows the terminal wrapped as they are, and a
/// row whose text reaches the last column with the next row's indent left out, since Claude Code
/// breaks a word too long for the row itself and indents the rest. A URL either CLI breaks at a
/// `/` or `-` on a row that isn't full is still found only in part.
#[derive(Default)]
struct Lines {
    text: String,
    /// The last row went on onto the next: wrapped by the terminal (`Some(true)`), or full.
    joins: Option<bool>,
}

impl Lines {
    /// Adds a row's text, `cols` columns with the last not blank at `used`, and whether the
    /// terminal wrapped it; returns the line it ends, if it does.
    fn add(&mut self, text: &str, used: u16, cols: u16, wrapped: bool) -> Option<String> {
        let text = match self.joins {
            Some(false) => text.trim_start(),
            _ => text,
        };
        self.text.push_str(text);
        self.joins = if wrapped {
            Some(true)
        } else if cols > 0 && used >= cols {
            Some(false)
        } else {
            None
        };
        if self.joins.is_some() { None } else { self.finish() }
    }

    fn finish(&mut self) -> Option<String> {
        self.joins = None;
        let line = std::mem::take(&mut self.text);
        (!line.trim().is_empty()).then_some(line)
    }
}

/// The http(s) URLs in `text`, as byte ranges: up to a character a URL can't hold, without the
/// punctuation that ends a sentence or a closing bracket the URL didn't open, and none longer than
/// `MAX_URL`.
fn find_urls(text: &str) -> Vec<std::ops::Range<usize>> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(start) = lower[from..].find("http").map(|i| i + from) {
        let rest = &lower[start..];
        let Some(scheme) = ["https://", "http://"].into_iter().find(|s| rest.starts_with(s)) else {
            from = start + 4;
            continue;
        };
        let len = text[start..]
            .char_indices()
            .find(|&(_, c)| !url_char(c))
            .map_or(text.len() - start, |(i, _)| i);
        let end = start + trimmed(&text[start..start + len]);
        if end > start + scheme.len() && end - start <= MAX_URL {
            out.push(start..end);
        }
        from = end.max(start + scheme.len());
    }
    out
}

fn url_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || "-._~:/?#[]@!$&'()*+,;=%".contains(c)
        || (!c.is_ascii() && c.is_alphanumeric())
}

/// How much of `url` is left without what ends the sentence it's in. The brackets are counted
/// once and the counts kept as the end comes off: untrusted text can follow a URL with thousands of
/// `)`, and counting again for each was quadratic.
fn trimmed(url: &str) -> usize {
    // Opened minus closed, in what's left.
    let balance = |open, close| {
        url.chars().fold(0isize, |n, c| n + isize::from(c == open) - isize::from(c == close))
    };
    let (mut parens, mut squares) = (balance('(', ')'), balance('[', ']'));
    let mut s = url;
    while let Some(c) = s.chars().next_back() {
        let drop = match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' => true,
            ')' => parens < 0,
            ']' => squares < 0,
            _ => false,
        };
        if !drop {
            break;
        }
        match c {
            ')' => parens += 1,
            ']' => squares += 1,
            _ => {}
        }
        s = &s[..s.len() - c.len_utf8()];
    }
    s.len()
}

/// Calls `f` with each row of the session, oldest first: its history, a screenful at a time, then
/// its screen. Scrolls the view to read the history, and back to where it was.
fn each_row(screen: &mut vt100::Screen, mut f: impl FnMut(&vt100::Screen, u16)) {
    let back = screen.scrollback();
    let len = crate::selection::history_len(screen);
    let rows = usize::from(crate::term::size(screen).0);
    // The row to read next, counted from the oldest history row.
    let mut row = 0;
    while row < len + rows {
        screen.set_scrollback(len.saturating_sub(row));
        let top = len - screen.scrollback();
        for r in row - top..rows {
            f(screen, u16::try_from(r).unwrap_or(u16::MAX));
        }
        row = top + rows;
    }
    screen.set_scrollback(back);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn parser(rows: u16, cols: u16, scrollback: usize) -> vt100::Parser {
        vt100::Parser::new(crate::term::dim(rows), crate::term::dim(cols), scrollback)
    }

    fn link(url: &str, label: Option<&str>) -> Link {
        Link { url: url.into(), label: label.map(Into::into) }
    }

    /// As Claude Code 2.1.296 prints a Markdown link.
    const CLAUDE: &str = "Here is \x1b]8;id=azmyb0;https://example.com/artifact/abc123\x07\x1b[94mthe artifact\x1b[22G\x1b]8;;\x07\x1b[39mand more";

    #[test]
    fn lists_a_link_as_claude_prints_it() {
        let mut p = parser(5, 60, 100);
        p.process(CLAUDE.as_bytes());
        assert_eq!(
            list(p.screen_mut()),
            [link("https://example.com/artifact/abc123", Some("the artifact"))]
        );
    }

    #[test]
    fn a_label_that_is_the_url_is_left_out() {
        let url = "https://example.com/docs/a";
        assert_eq!(Link::new(url, "docs").label.as_deref(), Some("docs"));
        assert_eq!(Link::new(url, " https://example.com/do cs/a ").label, None);
        assert_eq!(Link::new(url, "example.com/docs").label, None);
        assert_eq!(Link::new(url, "  ").label, None);
    }

    #[test]
    fn lists_newest_first_including_history() {
        let mut p = parser(3, 40, 100);
        p.process(b"\x1b]8;;https://example.com/old\x07old link\x1b]8;;\x07\r\n");
        for i in 0..10 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        p.process(b"\x1b]8;;file:///etc/hosts\x07/etc/hosts\x1b]8;;\x07\r\n");
        p.screen_mut().set_scrollback(3);
        assert_eq!(
            list(p.screen_mut()),
            [link("file:///etc/hosts", None), link("https://example.com/old", Some("old link"))]
        );
        // Where the view was.
        assert_eq!(p.screen().scrollback(), 3);
    }

    #[test]
    fn a_link_claude_breaks_over_rows_is_listed_whole() {
        // Claude Code 2.1 closes the link at the row's end and opens it again on the next.
        let url = "https://example.com/a/very/long/path?query=1";
        let open = format!("\x1b]8;id=1orb7vg;{url}\x07");
        let mut p = parser(10, 30, 100);
        p.process(
            format!(
                "● See {open}https://example.com/a/ve\x1b]8;;\x07\r\x1b[2C\x1b[1B{open}ry/long/path?query=1\x1b]8;;\x07 now.\r\n"
            )
            .as_bytes(),
        );
        assert_eq!(list(p.screen_mut()), [link(url, None)]);
        let open = format!("\x1b]8;id=2;{url}\x07");
        p.process(format!("{open}the long\x1b]8;;\x07\r\n{open}one\x1b]8;;\x07\r\n").as_bytes());
        assert_eq!(list(p.screen_mut()), [link(url, Some("the long one"))]);
    }

    #[test]
    fn a_link_codex_breaks_over_rows_is_listed_whole() {
        // Codex 0.162: one link, the cursor moved to each row's indent.
        let url = "https://example.com/a/very/long/path?query=2";
        let mut p = parser(10, 30, 100);
        p.process(
            format!(
                "\x1b[1;1H• See \x1b]8;;{url}\x07https://\x1b[2;3Hexample.com/a/very/\x1b[3;3Hlong/path?query=2\x1b]8;;\x07 now."
            )
            .as_bytes(),
        );
        assert_eq!(list(p.screen_mut()), [link(url, None)]);
    }

    #[test]
    fn a_link_codex_shows_with_its_url_keeps_its_text() {
        // Codex 0.162 without a known terminal: "the artifact (https://…)", both parts linked.
        let url = "https://example.com/artifact/abc123";
        let mut p = parser(10, 80, 100);
        p.process(
            format!(
                "\x1b[?1049h\x1b[10;11H\x1b[4m\x1b]8;;{url}\x07the artifact\x1b]8;;\x07\x1b[24m (\x1b[4m\x1b]8;;{url}\x07{url}\x1b]8;;\x07\x1b[24m)"
            )
            .as_bytes(),
        );
        assert_eq!(list(p.screen_mut()), [link(url, Some("the artifact"))]);
    }

    #[test]
    fn lists_plain_urls_with_the_links() {
        let mut p = parser(5, 80, 100);
        p.process(b"See https://example.com/a_b?q=1, or (https://example.com/wiki/Foo_(bar)).\r\n");
        p.process(CLAUDE.as_bytes());
        p.process(
            b"\r\nagain https://example.com/a_b?q=1 and https://example.com/artifact/abc123.\r\n",
        );
        assert_eq!(
            list(p.screen_mut()),
            [
                link("https://example.com/artifact/abc123", Some("the artifact")),
                link("https://example.com/a_b?q=1", None),
                link("https://example.com/wiki/Foo_(bar)", None),
            ]
        );
    }

    #[test]
    fn a_plain_url_over_rows() {
        // Claude Code breaks a word too long for the row and indents the rest, with no wrap.
        let mut p = parser(5, 30, 100);
        p.process("● See https://example.com/a/ve\r\n  ry/long?q=1 now.\r\n".as_bytes());
        // And one the terminal wrapped.
        p.process(b"x https://example.com/0123456789abcdef wrapped");
        assert_eq!(
            list(p.screen_mut()),
            [
                link("https://example.com/0123456789abcdef", None),
                link("https://example.com/a/very/long?q=1", None)
            ]
        );
    }

    #[test]
    fn finds_urls_without_the_punctuation_around_them() {
        let urls = |s: &str| find_urls(s).into_iter().map(|r| s[r].to_string()).collect::<Vec<_>>();
        assert_eq!(urls("go to https://example.com."), ["https://example.com"]);
        assert_eq!(urls("(see https://example.com/x)"), ["https://example.com/x"]);
        assert_eq!(urls("[link](https://example.com/a(b)c)"), ["https://example.com/a(b)c"]);
        assert_eq!(
            urls("<https://example.com/>, 'http://example.com/q?a=1'"),
            ["https://example.com/", "http://example.com/q?a=1"]
        );
        assert_eq!(urls("HTTPS://Example.com/Path!"), ["HTTPS://Example.com/Path"]);
        assert_eq!(urls("**https://example.com/b**"), ["https://example.com/b"]);
        assert_eq!(urls("│ https://example.com/c │ next"), ["https://example.com/c"]);
        assert_eq!(urls("http:// https:// httpx://a ftp://example.com"), Vec::<String>::new());
        assert_eq!(
            urls("https://ex.com/a https://ex.com/b"),
            ["https://ex.com/a", "https://ex.com/b"]
        );
    }

    #[test]
    fn trims_a_url_followed_by_many_brackets_quickly() {
        let text = format!("https://example.com/a(b){}", ")".repeat(64 * 1024));
        let started = std::time::Instant::now();
        let found: Vec<_> = find_urls(&text).into_iter().map(|r| &text[r]).collect();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "took {:?}",
            started.elapsed()
        );
        assert_eq!(found, ["https://example.com/a(b)"]);
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL));
        let text = format!("{long} https://example.com/b");
        assert_eq!(
            find_urls(&text).into_iter().map(|r| &text[r]).collect::<Vec<_>>(),
            ["https://example.com/b"]
        );
    }

    /// The OSC 8 escapes in `s`, and its text without escapes.
    fn escapes_and_text(s: &str) -> (Vec<&str>, String) {
        let mut osc8 = Vec::new();
        let mut text = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('\x1b') {
            text.push_str(&rest[..i]);
            rest = &rest[i..];
            let end = if rest.starts_with("\x1b]") {
                rest.find('\x07').unwrap() + 1
            } else {
                rest.find(|c: char| c.is_ascii_alphabetic()).unwrap() + 1
            };
            if rest.starts_with("\x1b]8;") {
                osc8.push(&rest[..end]);
            }
            rest = &rest[end..];
        }
        text.push_str(rest);
        (osc8, text)
    }

    #[test]
    fn sends_each_link_once_a_run() {
        use ratatui::backend::Backend;
        let a = "\x1b]8;id=a;https://example.com/a\x07";
        let b = "\x1b]8;;https://example.com/b\x07";
        let mut p = parser(3, 30, 0);
        // Over two rows, coloured part-way, with a wide character; then another link, and a file:
        // one, which isn't passed on.
        p.process(
            format!("{a}one \x1b[31mtwo\x1b[39m 日 three four five six seven{CLOSE} {b}b{CLOSE} \x1b]8;;file:///x\x07f{CLOSE}")
                .as_bytes(),
        );
        let area = Rect::new(0, 0, 30, 3);
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(
            tui_term::widget::PseudoTerminal::new(&crate::term::Screen(p.screen())),
            area,
            &mut buf,
        );
        mark(&mut buf, area, p.screen());
        let mut out = Vec::new();
        {
            let mut backend = ratatui::backend::CrosstermBackend::new(Runs::new(&mut out));
            backend.draw(Buffer::empty(area).diff(&buf).into_iter()).unwrap();
            Backend::flush(&mut backend).unwrap();
        }
        let out = String::from_utf8(out).unwrap();
        let (osc8, _) = escapes_and_text(&out);
        assert_eq!(osc8, [a, CLOSE, b, CLOSE], "{out:?}");
        let run = &out[out.find(a).unwrap()..out.find(CLOSE).unwrap()];
        assert_eq!(escapes_and_text(run).1, "one two 日 three four five six seven", "{run:?}");
    }

    #[test]
    fn a_linked_cell_shows_its_symbol() {
        let mut cell = Cell::new("\u{26a0}\u{fe0f}");
        wrap(&mut cell, &opening("id=x", "https://example.com/a"));
        assert_ne!(cell.symbol(), "\u{26a0}\u{fe0f}");
        assert_eq!(shown(cell.symbol()), "\u{26a0}\u{fe0f}");
        assert_eq!(shown("a"), "a");
    }

    #[test]
    fn joins_a_link_closed_and_opened_again() {
        let join = |s: &str| {
            let mut out = Vec::new();
            join_runs(s.as_bytes(), &mut out);
            String::from_utf8(out).unwrap()
        };
        let a = "\x1b]8;id=x;https://example.com/a\x07";
        let b = "\x1b]8;;https://example.com/b\x07";
        // The same link: one run, keeping a cursor move and colours between its cells.
        assert_eq!(join(&format!("{a}x{CLOSE}{a}y{CLOSE}")), format!("{a}xy{CLOSE}"));
        assert_eq!(
            join(&format!("{a}x{CLOSE}\x1b[2;1H\x1b[1m\x1b[38;2;1;2;3m{a}y{CLOSE}{a}z{CLOSE}")),
            format!("{a}x\x1b[2;1H\x1b[1m\x1b[38;2;1;2;3myz{CLOSE}")
        );
        // Another link, text, or another escape between keeps the close.
        for s in [
            format!("{a}x{CLOSE}{b}y{CLOSE}"),
            format!("{a}x{CLOSE} {a}y{CLOSE}"),
            format!("{a}x{CLOSE}\x1b[2J{a}y{CLOSE}"),
            format!("{a}x{CLOSE}\x1b[1m"),
        ] {
            assert_eq!(join(&s), s);
        }
        // A close with no link open, other OSCs, and an unfinished escape pass through.
        let s = format!("{CLOSE}{CLOSE}\x1b]0;title\x07\x1b]8;x");
        assert_eq!(join(&s), s);
    }

    #[test]
    fn joins_runs_across_writes() {
        let a = "\x1b]8;id=x;https://example.com/a\x07";
        let input = format!("\x1b[1;1H{a}x{CLOSE}\x1b[39m{a}y{CLOSE}z");
        for at in 0..=input.len() {
            let mut out = Vec::new();
            let mut w = Runs::new(&mut out);
            w.write_all(&input.as_bytes()[..at]).unwrap();
            w.write_all(&input.as_bytes()[at..]).unwrap();
            w.flush().unwrap();
            // Nothing is left to send on drop.
            drop(w);
            assert_eq!(String::from_utf8(out).unwrap(), format!("\x1b[1;1H{a}x\x1b[39my{CLOSE}z"));
        }
    }

    #[test]
    fn marks_cells_that_show_an_http_link() {
        let mut p = parser(2, 40, 0);
        p.process(CLAUDE.as_bytes());
        p.process(
            "\r\n\x1b]8;;file:///etc/hosts\x07f\x1b]8;;\x07 \x1b]8;;https://example.com/w\x07日\x1b]8;;\x07".as_bytes(),
        );
        let area = Rect::new(2, 1, 40, 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 4));
        let screen = p.screen();
        ratatui::widgets::Widget::render(
            tui_term::widget::PseudoTerminal::new(&crate::term::Screen(screen)),
            area,
            &mut buf,
        );
        mark(&mut buf, area, screen);
        let at = |x: u16, y: u16| buf[(area.x + x, area.y + y)].clone();
        assert_eq!(at(7, 0).symbol(), " ");
        assert_eq!(
            at(8, 0).symbol(),
            "\x1b]8;id=azmyb0;https://example.com/artifact/abc123\x07t\x1b]8;;\x07"
        );
        assert_eq!(at(8, 0).diff_option, CellDiffOption::ForcedWidth(NonZeroU16::MIN));
        // Not a file: link.
        assert_eq!(at(0, 1).symbol(), "f");
        // A wide character keeps its width; its right half is left alone.
        assert_eq!(at(2, 1).symbol(), "\x1b]8;;https://example.com/w\x07日\x1b]8;;\x07");
        assert_eq!(at(2, 1).diff_option, CellDiffOption::ForcedWidth(NonZeroU16::new(2).unwrap()));
        assert_eq!(at(3, 1).diff_option, CellDiffOption::None);
    }

    #[test]
    fn passes_on_only_a_plain_id() {
        let mut p = parser(2, 20, 0);
        p.process(
            b"\x1b]8;id=a=b;https://example.com/\x07x\x1b]8;id=a:b=c;https://example.com/\x07y",
        );
        assert_eq!(params(p.screen().cell_hyperlink(0, 0).unwrap()), "");
        assert_eq!(params(p.screen().cell_hyperlink(0, 1).unwrap()), "id=a");
    }

    #[test]
    fn links_the_urls_in_text() {
        let mut l = Linked::default();
        l.text(
            "  ",
            "See https://example.com/a, file:///etc/x and\x1b]8;;https://evil.example/\x07 https://example.com/a.",
        );
        let spans: Vec<&str> = l.lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            spans,
            [
                "  ",
                "See ",
                "https://example.com/a",
                ", file:///etc/x and]8;;",
                "https://evil.example/",
                " ",
                "https://example.com/a",
                "."
            ]
        );
        // One URL, numbered once; the escape's own URL is only text now, found as any other.
        assert_eq!(l.urls(), ["https://example.com/a", "https://evil.example/"]);
        let coded = l.coded(3);
        let fg = |i: usize| coded[0].spans[i].style.fg;
        assert_eq!((fg(2), fg(4), fg(6)), (Some(code(4)), Some(code(5)), Some(code(4))));
        assert_eq!((fg(1), fg(3), fg(7)), (None, None, None));
        // The text keeps its own styles; the coded lines have none but the links'.
        assert!(l.lines[0].spans[2].style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(coded[0].spans[2].style.add_modifier, Modifier::empty());
    }

    #[test]
    fn links_only_http_and_appends_with_the_numbers_moved() {
        let mut a = Linked::default();
        a.push("x");
        a.link(0, "javascript:alert(1)");
        a.link(0, "");
        a.link(0, "https://example.com/a");
        let mut b = Linked::default();
        b.push("y");
        b.push("z");
        b.link(0, "https://example.com/b");
        b.link(0, "https://example.com/a");
        a.append(b);
        assert_eq!(a.urls(), ["https://example.com/a", "https://example.com/b"]);
        let coded = a.coded(0);
        assert_eq!(coded.len(), 3);
        assert_eq!(coded[0].spans[0].style.fg, Some(code(1)));
        assert_eq!(coded[1].spans[0].style.fg, None);
        assert_eq!(coded[2].spans[0].style.fg, Some(code(1)), "z, linked to a's URL last");
    }

    #[test]
    fn marks_the_cells_the_coded_render_shows() {
        let area = Rect::new(1, 1, 6, 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 3));
        let mut coded = Buffer::empty(area);
        buf.set_string(1, 1, "ab日cd", Style::new());
        coded.set_string(1, 1, "a", Style::new());
        coded.set_string(2, 1, "b日", Style::new().fg(code(1)));
        coded.set_string(5, 1, "c", Style::new().fg(code(9)));
        mark_coded(&mut buf, &coded, area, &["https://example.com/x"]);
        let id = id("https://example.com/x");
        assert_eq!(buf[(1, 1)].symbol(), "a");
        assert_eq!(
            buf[(2, 1)].symbol(),
            format!("\x1b]8;{id};https://example.com/x\x07b\x1b]8;;\x07")
        );
        assert_eq!(
            buf[(3, 1)].diff_option,
            CellDiffOption::ForcedWidth(NonZeroU16::new(2).unwrap())
        );
        // The right half of the wide character, and a number with no URL, are left alone.
        assert_eq!(buf[(4, 1)].diff_option, CellDiffOption::None);
        assert_eq!(buf[(5, 1)].symbol(), "c");
        assert!(
            id.starts_with("id=taskman-")
                && id[3..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        );
    }

    #[test]
    fn lists_a_tickets_panel_links_in_order() {
        use crate::github::{Check, PrComment, Review, Thread, Threads};
        let check = |name: &str, state, url: &str| Check {
            name: name.into(),
            state,
            when: None,
            url: url.into(),
        };
        let comment = |id: &str, body: &str, url: &str| PrComment {
            id: id.into(),
            author: "bo".into(),
            body: body.into(),
            url: url.into(),
            ..Default::default()
        };
        let pr = Pr {
            number: 7,
            title: "Fix login".into(),
            url: "https://github.com/acme/web/pull/7".into(),
            state: "OPEN".into(),
            is_draft: false,
            review_decision: String::new(),
            mergeable: String::new(),
            head: "eng-52".into(),
            cross_repository: false,
            head_oid: String::new(),
            base: "main".into(),
            checks: vec![
                check("lint", CheckState::Fail, "https://github.com/acme/web/actions/runs/1/job/1"),
                check("e2e", CheckState::Pending, "https://ci.example/e2e"),
                check("unit", CheckState::Pass, "https://github.com/acme/web/actions/runs/1/job/2"),
                check("odd", CheckState::Fail, ""),
            ],
            reviews: vec![Review {
                id: "r1".into(),
                author: "ana".into(),
                state: "CHANGES_REQUESTED".into(),
                body: "See https://docs.example/style".into(),
                url: "https://github.com/acme/web/pull/7#pullrequestreview-1".into(),
                ..Default::default()
            }],
            comments: vec![comment(
                "c1",
                "Logs: https://ci.example/e2e and https://logs.example/1",
                "https://github.com/acme/web/pull/7#issuecomment-1",
            )],
            threads: Ok(Threads {
                list: vec![
                    Thread {
                        path: "src/a.rs".into(),
                        line: Some(3),
                        resolved: false,
                        outdated: false,
                        comments: vec![comment(
                            "t1",
                            "Why?",
                            "https://github.com/acme/web/pull/7#discussion_r1",
                        )],
                        left_out: 0,
                    },
                    Thread {
                        path: "src/b.rs".into(),
                        line: None,
                        resolved: true,
                        outdated: false,
                        comments: vec![comment(
                            "t2",
                            "ok",
                            "https://github.com/acme/web/pull/7#discussion_r2",
                        )],
                        left_out: 0,
                    },
                ],
                ..Default::default()
            }),
            stamp: Default::default(),
        };
        let issue = Issue {
            summary: "Login fails".into(),
            description: "Seen on https://app.example/login (and javascript:x).".into(),
            ..Default::default()
        };
        let url = "https://linear.app/acme/issue/ENG-52";
        let links = panel("ENG-52", Some(url), Some(&issue), Some(&pr));
        let got: Vec<(&str, Option<&str>)> =
            links.iter().map(|l| (l.url.as_str(), l.label.as_deref())).collect();
        assert_eq!(
            got,
            [
                (url, Some("ENG-52 · Login fails")),
                ("https://github.com/acme/web/pull/7", Some("PR #7 · Fix login")),
                ("https://github.com/acme/web/actions/runs/1/job/1", Some("Failing check: lint")),
                ("https://ci.example/e2e", Some("Running check: e2e")),
                (
                    "https://github.com/acme/web/pull/7#pullrequestreview-1",
                    Some("Review by ana (changes requested)")
                ),
                ("https://github.com/acme/web/pull/7#discussion_r1", Some("Thread on src/a.rs:3")),
                ("https://github.com/acme/web/pull/7#issuecomment-1", Some("Comment by bo")),
                ("https://app.example/login", None),
                ("https://docs.example/style", None),
                ("https://logs.example/1", None),
                ("https://github.com/acme/web/actions/runs/1/job/2", Some("Check: unit")),
            ]
        );
        // Local files: no issue page, nothing loaded yet.
        assert_eq!(panel("ENG-52", None, None, None), []);
        assert_eq!(panel("ENG-52", Some(url), None, None)[0].label.as_deref(), Some("ENG-52"));
    }

    #[test]
    fn only_http_links_open() {
        assert!(openable("https://example.com/a?b=c"));
        assert!(openable("HTTP://example.com"));
        assert!(!openable("https://"));
        assert!(!openable("file:///etc/passwd"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("vscode://file/x"));
        assert!(!openable("https://example.com/a b"));
        assert!(!openable("https://example.com/\x1b]52;c;x\x07"));
        assert!(!openable(&format!("https://example.com/{}", "a".repeat(MAX_URL))));
        // A host, as Codex requires.
        assert!(openable("https://user@example.com:8443/a#b"));
        assert!(openable("http://[::1]:8080/"));
        assert!(openable("https://例え.jp/パス"));
        assert!(openable("https://example.com?q=1"));
        assert!(!openable("https:///etc/passwd"));
        assert!(!openable("https://:80/x"));
        assert!(!openable("https://example.com:http/"));
        assert!(!openable("https://[zz]/"));
        assert!(!openable("https:example.com"));
    }

    /// Text with URLs in it, as untrusted text might put them: schemes in either case, cut short,
    /// or run into punctuation, brackets and characters that aren't ASCII.
    fn text_with_urls() -> impl Strategy<Value = String> {
        let one_of = |s: &[&'static str]| prop::sample::select(s.to_vec()).prop_map(String::from);
        let piece = prop_oneof![
            2 => one_of(&["https://", "http://", "HTTPS://", "Http://", "http", "http:/"]),
            3 => one_of(&["example.com", "a", "x/y", "q?a=1", "#f", "%20", "例え"]),
            3 => one_of(&[
                ".", ",", ";", ":", "!", "?", "'", "*", "(", ")", "[", "]", " ", "\n", "│", "<",
                ">", "\"",
            ]),
            1 => "\\PC",
        ];
        prop::collection::vec(piece, 0..16).prop_map(|p| p.concat())
    }

    /// `trimmed` as it reads, counting the brackets again for each character it takes off.
    fn trimmed_slowly(url: &str) -> usize {
        let mut s = url;
        while let Some(c) = s.chars().next_back() {
            let more = |close, open| s.matches(close).count() > s.matches(open).count();
            let drop = match c {
                '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' => true,
                ')' => more(')', '('),
                ']' => more(']', '['),
                _ => false,
            };
            if !drop {
                break;
            }
            s = &s[..s.len() - c.len_utf8()];
        }
        s.len()
    }

    /// Whether `small` is `big` with some bytes left out.
    fn left_out_of(small: &[u8], big: &[u8]) -> bool {
        let mut big = big.iter();
        small.iter().all(|b| big.any(|c| c == b))
    }

    /// What ratatui's output and `mark`'s links write between and in cells: the bytes and the text
    /// among them, which has no escape in it.
    fn frame() -> impl Strategy<Value = (Vec<u8>, Vec<u8>)> {
        let text = prop::collection::vec(any::<u8>().prop_filter("not ESC", |&b| b != 0x1b), 1..6);
        let escape = prop::sample::select(vec![
            "\x1b]8;id=x;https://example.com/a\x07",
            "\x1b]8;;https://example.com/b\x07",
            CLOSE,
            CLOSE,
            "\x1b[2;1H",
            "\x1b[1m",
            "\x1b[38;2;1;2;3m",
            "\x1b[39m",
            "\x1b[2J",
            "\x1b[K",
            "\x1b]0;title\x07",
        ]);
        let piece = prop_oneof![
            2 => text.prop_map(|t| (t.clone(), t)),
            3 => escape.prop_map(|e| (e.as_bytes().to_vec(), Vec::new())),
        ];
        prop::collection::vec(piece, 0..24).prop_map(|p| {
            let (bytes, text): (Vec<_>, Vec<_>) = p.into_iter().unzip();
            (bytes.concat(), text.concat())
        })
    }

    /// `bytes` without the escapes `frame` writes: OSCs ended by BEL, and CSIs.
    fn without_escapes(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let end = match bytes[i..] {
                [0x1b, b']', ..] => bytes[i..].iter().position(|&b| b == 0x07),
                [0x1b, b'[', ..] => bytes[i..].iter().position(u8::is_ascii_alphabetic),
                _ => None,
            };
            match end {
                Some(n) => i += n + 1,
                None => {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
        }
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn found_urls_are_whole_characters_and_start_with_the_scheme(text in text_with_urls()) {
            let found = find_urls(&text);
            for pair in found.windows(2) {
                prop_assert!(pair[0].end <= pair[1].start, "{:?}", found);
            }
            for r in found {
                prop_assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
                let url = &text[r.clone()];
                let lower = url.to_ascii_lowercase();
                let scheme = ["https://", "http://"].into_iter().find(|s| lower.starts_with(s));
                prop_assert!(scheme.is_some_and(|s| url.len() > s.len()), "{:?}", url);
                prop_assert!(url.len() <= MAX_URL && url.chars().all(url_char), "{:?}", url);
                prop_assert_eq!(trimmed(url), url.len(), "{:?}", url);
            }
        }

        #[test]
        fn trimming_only_takes_off_the_end(url in "[a-z/().,;:!?'*\\[\\]日]{0,24}") {
            let n = trimmed(&url);
            prop_assert!(n <= url.len() && url.is_char_boundary(n));
            prop_assert_eq!(n, trimmed_slowly(&url));
        }

        #[test]
        fn joining_runs_keeps_the_text_and_only_leaves_escapes_out((input, text) in frame()) {
            let mut out = Vec::new();
            join_runs(&input, &mut out);
            prop_assert_eq!(without_escapes(&out), text);
            prop_assert!(left_out_of(&out, &input), "{:?} from {:?}", out, input);
            // Joined once, nothing is left to join.
            let mut again = Vec::new();
            join_runs(&out, &mut again);
            prop_assert_eq!(again, out);
        }

        #[test]
        fn joining_runs_takes_any_bytes(
            input in prop::collection::vec(
                prop_oneof![
                    prop::sample::select(vec![
                        &b"\x1b"[..], b"]8;", b";", b"\x07", CLOSE.as_bytes(), b"\x1b[", b"1;2H",
                        b"m", b"\x1b]8;;https://example.com/a\x07",
                    ]).prop_map(<[u8]>::to_vec),
                    prop::collection::vec(any::<u8>(), 1..4),
                ],
                0..24,
            ).prop_map(|p| p.concat()),
        ) {
            let mut out = Vec::new();
            join_runs(&input, &mut out);
            prop_assert!(left_out_of(&out, &input), "{:?} from {:?}", out, input);
        }
    }
}
