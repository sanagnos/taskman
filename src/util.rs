//! Small helpers: timestamps, relative ages, opening URLs, measuring and cutting text.

use ratatui::buffer::CellWidth as _;
use std::process::{Command, Stdio};
use unicode_segmentation::UnicodeSegmentation as _;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().cast_signed())
        .unwrap_or(0)
}

/// Runs `f`, catching a panic (a bug: in a parser, say), which the panic hook has written to
/// panics.log; the error then says where. For threads whose message something waits for: one that
/// ended without a word would leave it waiting for good.
pub fn caught<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .map_err(|_| format!("taskman hit a bug; details in {}", crate::store::panics_log()))
}

/// The panic unit tests make to stand for a bug: `caught` catches it, and the panic hook
/// `quiet_test_panics` sets keeps it out of the test's output.
#[cfg(test)]
pub struct TestPanic;

/// Leaves `TestPanic` out of the output; other panics, such as a failed assertion's, still print.
#[cfg(test)]
pub fn quiet_test_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !info.payload().is::<TestPanic>() {
                hook(info);
            }
        }));
    });
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM|±HHMM]` (GitHub and Jira formats) to unix seconds.
pub fn parse_ts(s: &str) -> Option<i64> {
    if s.len() < 19 || !s.is_char_boundary(19) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, se) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let mut rest = &s[19..];
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let digits: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
            let oh: i64 = digits.get(0..2)?.parse().ok()?;
            let om: i64 = digits.get(2..4).and_then(|m| m.parse().ok()).unwrap_or(0);
            let secs = oh * 3600 + om * 60;
            if sign == '-' { -secs } else { secs }
        }
        _ => 0,
    };
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - offset)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// "now", "5m", "3h", "2d", "6w".
pub fn ago(ts: i64) -> String {
    short_age(now() - ts)
}

/// "just now", "5m ago", "3h ago": `secs` seconds ago, in words, in [`ago`]'s buckets.
pub fn ago_words(secs: i64) -> String {
    match short_age(secs).as_str() {
        "now" => "just now".into(),
        a => format!("{a} ago"),
    }
}

fn short_age(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        86400..1209600 => format!("{}d", s / 86400),
        _ => format!("{}w", s / 604800),
    }
}

pub fn ago_str(s: &str) -> String {
    parse_ts(s).map(ago).unwrap_or_default()
}

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version")
        .map(|v| v.to_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

/// Opens a URL in the user's real browser. Tries $BROWSER, then WSL, Linux and macOS openers.
pub fn open_url(url: &str) -> bool {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(b) = std::env::var("BROWSER") {
        candidates.push(b);
    }
    candidates.push("wslview".into());
    if is_wsl() {
        candidates.push("explorer.exe".into());
    }
    candidates.push("xdg-open".into());
    candidates.push("open".into());
    candidates.into_iter().any(|c| {
        Command::new(&c)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    })
}

/// `s` broken into lines at most `room` columns wide, between words; a word longer than that is
/// broken where it reaches the edge, a wide character or an emoji with its selector kept whole.
/// For text whose rows must be counted before it's drawn, or that wraps under an indent of its own
/// (Help).
pub fn wrap_words(s: &str, room: usize) -> Vec<String> {
    let room = room.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let gap = usize::from(!line.is_empty());
        if width(&line) + gap + width(word) <= room {
            if gap == 1 {
                line.push(' ');
            }
            line.push_str(word);
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        for g in word.graphemes(true) {
            if width(&line) + grapheme_width(g) > room && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            line.push_str(g);
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// The columns `s` takes on screen, measured as ratatui draws it: grapheme by grapheme, so an
/// emoji with U+FE0F (⚠️) counts two, a combining mark nothing, and a grapheme with a control
/// character in it nothing, as ratatui leaves those out. Cut and pad with this, never
/// `chars().count()`, or a CJK or emoji title overflows the room it was cut for.
pub fn width(s: &str) -> usize {
    s.graphemes(true).map(grapheme_width).sum()
}

/// `width` of a line of spans.
pub fn line_width(line: &ratatui::text::Line) -> usize {
    line.spans.iter().map(|s| width(&s.content)).sum()
}

fn grapheme_width(g: &str) -> usize {
    if g.contains(char::is_control) { 0 } else { usize::from(g.cell_width()) }
}

/// `s` and spaces after it to make `cols` columns (`width`): `format!("{s:<cols$}")` counts
/// characters, so a CJK or emoji `s` would come out too wide.
pub fn pad(s: &str, cols: usize) -> String {
    format!("{s}{}", " ".repeat(cols.saturating_sub(width(s))))
}

/// The longest start of `s` at most `room` columns wide, in whole graphemes.
fn head(s: &str, room: usize) -> &str {
    let mut used = 0;
    let mut end = 0;
    for (i, g) in s.grapheme_indices(true) {
        used += grapheme_width(g);
        if used > room {
            break;
        }
        end = i + g.len();
    }
    &s[..end]
}

/// The longest end of `s` at most `room` columns wide, in whole graphemes.
fn tail(s: &str, room: usize) -> &str {
    let mut used = 0;
    let mut start = s.len();
    for (i, g) in s.grapheme_indices(true).rev() {
        used += grapheme_width(g);
        if used > room {
            break;
        }
        start = i;
    }
    &s[start..]
}

/// `s` in at most `max` columns (`width`): as it is if it fits, else cut and ended with `…`. A
/// wide character or an emoji with its selector that doesn't fit whole is left out, never split,
/// so the cut may be a column short.
pub fn truncate(s: &str, max: usize) -> String {
    match max {
        _ if width(s) <= max => s.to_string(),
        0 => String::new(),
        _ => format!("{}…", head(s, max - 1)),
    }
}

/// Like `truncate`, but keeps the end: `…` and the last `max - 1` columns.
pub fn truncate_start(s: &str, max: usize) -> String {
    match max {
        _ if width(s) <= max => s.to_string(),
        0 => String::new(),
        _ => format!("…{}", tail(s, max - 1)),
    }
}

/// Like `truncate`, but keeps both ends and puts the `…` in the middle.
pub fn truncate_middle(s: &str, max: usize) -> String {
    if width(s) <= max || max == 0 {
        return truncate(s, max);
    }
    let start = head(s, max / 2);
    format!("{start}…{}", tail(s, max - 1 - width(start)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_panic_caught_says_where_it_was_written() {
        quiet_test_panics();
        assert_eq!(caught(|| 1), Ok(1));
        let panicked = caught(|| -> u8 { std::panic::panic_any(TestPanic) });
        let bug = "taskman hit a bug; details in ~/.local/state/taskman/panics.log";
        assert_eq!(panicked, Err(bug.to_string()));
    }

    /// The columns ratatui moves on by drawing `s`.
    fn drawn(s: &str) -> usize {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 400, 1));
        usize::from(buf.set_stringn(0, 0, s, 400, ratatui::style::Style::new()).0)
    }

    /// Text that measures differently by characters, by `char` widths and by graphemes: CJK, the
    /// warning sign with and without U+FE0F, an emoji, a ZWJ sequence, combining marks, and
    /// control characters.
    fn tricky() -> impl Strategy<Value = String> {
        let parts = [
            "a", "b", " ", "-", "日", "語", "⚠", "\u{fe0f}", "\u{301}", "\u{200d}", "👍", "é",
            "\u{7}", "\t", "\u{1b}", "\u{ff9e}",
        ];
        prop::collection::vec(prop::sample::select(parts.to_vec()), 0..30).prop_map(|v| v.concat())
    }

    /// Where `cut` may end: a grapheme boundary of `s`.
    fn boundary(s: &str, at: usize) -> bool {
        at == s.len() || s.grapheme_indices(true).any(|(i, _)| i == at)
    }

    #[test]
    fn measures_text_as_ratatui_draws_it() {
        assert_eq!(width("ENG-52"), 6);
        assert_eq!(width("日本語"), 6);
        // U+FE0F makes the warning sign an emoji, two columns; without it, it's one.
        assert_eq!(width("⚠"), 1);
        assert_eq!(width("⚠\u{fe0f}"), 2);
        assert_eq!(width("⚠\u{fe0f} failed"), 9);
        // A combining mark adds nothing; a control character is left out.
        assert_eq!(width("cafe\u{301}"), 4);
        assert_eq!(width("a\u{7}b\tc\u{1b}"), 3);
        for s in ["日本語", "⚠\u{fe0f}", "cafe\u{301}", "a\u{7}b", "👍\u{200d}👍"] {
            assert_eq!(width(s), drawn(s), "{s:?}");
        }
        assert_eq!(pad("日本", 6), "日本  ");
        assert_eq!(pad("⚠\u{fe0f}", 3), "⚠\u{fe0f} ");
        assert_eq!(pad("abcd", 2), "abcd");
    }

    #[test]
    fn cuts_by_display_columns() {
        assert_eq!(truncate("✦ tester", 23), "✦ tester");
        assert_eq!(truncate("✦ open-source-planner", 21), "✦ open-source-planner");
        assert_eq!(truncate("✦ open-source-planner", 13), "✦ open-sourc…");
        assert_eq!(truncate("ENG-52", 4), "ENG…");
        // A wide character that doesn't fit whole is left out, never split.
        assert_eq!(truncate("✦ 日本語チーム", 7), "✦ 日本…");
        assert_eq!(truncate("✦ 日本語チーム", 8), "✦ 日本…");
        assert_eq!(truncate_start("日本語チーム", 6), "…ーム");
        assert_eq!(truncate_middle("日本語チーム", 7), "日…ーム");
        // An emoji keeps its selector: both go, or neither.
        assert_eq!(truncate("a⚠\u{fe0f}b", 3), "a…");
        assert_eq!(truncate("a⚠b", 3), "a⚠b");
        assert_eq!(truncate_start("a⚠\u{fe0f}", 2), "…");
        assert_eq!(truncate_start("ab⚠\u{fe0f}", 3), "…⚠\u{fe0f}");
        // A combining mark stays with its letter.
        assert_eq!(truncate("cafe\u{301} au lait", 5), "cafe\u{301}…");
        assert_eq!(truncate_start("e\u{301}abc", 3), "…bc");
        // Control characters take no room.
        assert_eq!(truncate("a\u{7}bc", 3), "a\u{7}bc");
    }

    #[test]
    fn wraps_between_words() {
        assert_eq!(wrap_words("move the card; Enter ends", 14), ["move the card;", "Enter ends"]);
        assert_eq!(wrap_words("a  b", 10), ["a b"]);
        assert_eq!(wrap_words("", 10), [""]);
        // A word longer than the line is broken at the edge, a wide character kept whole.
        assert_eq!(wrap_words("see ~/.local/state", 8), ["see", "~/.local", "/state"]);
        assert_eq!(wrap_words("日本語", 5), ["日本", "語"]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Every line fits, and the words come back in order.
        #[test]
        fn wrapped_lines_fit_and_keep_the_text(
            s in prop_oneof!["[a-z日 ]{0,60}".boxed(), tricky().boxed()],
            room in 2usize..30,
        ) {
            let lines = wrap_words(&s, room);
            for l in &lines {
                // Only a character wider than the room on its own (日 and ﾞ are one, three wide).
                let one = l.graphemes(true).count() == 1;
                prop_assert!(width(l) <= room || one, "{l:?} wider than {room}");
            }
            let joined: String = lines.concat().split_whitespace().collect();
            let words: String = s.split_whitespace().collect();
            prop_assert_eq!(joined, words);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn measures_what_ratatui_draws(s in tricky()) {
            prop_assert_eq!(width(&s), drawn(&s));
        }

        /// A cut fits, wastes less than a grapheme's columns (one that didn't fit whole), and
        /// keeps whole graphemes of `s`.
        #[test]
        fn cuts_fit_and_keep_whole_graphemes(s in tricky(), max in 0usize..24) {
            let whole = width(&s) <= max;
            let widest = s.graphemes(true).map(width).max().unwrap_or(1);
            let cut = truncate(&s, max);
            let start = truncate_start(&s, max);
            let middle = truncate_middle(&s, max);
            for out in [&cut, &start, &middle] {
                prop_assert!(width(out) <= max, "{out:?} wider than {max}");
                if whole {
                    prop_assert_eq!(out, &s);
                } else if max > 0 {
                    prop_assert!(width(out) + widest > max, "{out:?} short of {max}");
                }
            }
            if whole || max == 0 {
                return Ok(());
            }
            let kept = cut.strip_suffix('…').unwrap();
            prop_assert!(s.starts_with(kept) && boundary(&s, kept.len()));
            let kept = start.strip_prefix('…').unwrap();
            prop_assert!(s.ends_with(kept) && boundary(&s, s.len() - kept.len()));
            let (head, tail) = middle.split_once('…').unwrap();
            prop_assert!(s.starts_with(head) && boundary(&s, head.len()));
            prop_assert!(s.ends_with(tail) && boundary(&s, s.len() - tail.len()));
        }
    }

    #[test]
    fn cuts_at_either_end_or_the_middle() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate_start("abcdef", 4), "…def");
        assert_eq!(truncate_middle("abcdef", 4), "ab…f");
        assert_eq!(truncate_middle("abcdefg", 5), "ab…fg");
        for cut in [truncate, truncate_start, truncate_middle] {
            assert_eq!(cut("abc", 3), "abc");
            assert_eq!(cut("abc", 0), "");
            assert_eq!(cut("abc", 1), "…");
        }
    }

    #[test]
    fn parses_github_and_jira_timestamps() {
        assert_eq!(parse_ts("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_ts("2024-03-01T12:00:00Z"), Some(1709294400));
        assert_eq!(parse_ts("2024-03-01T13:00:00.123+0100"), Some(1709294400));
        assert_eq!(parse_ts("2024-03-01T07:00:00-05:00"), Some(1709294400));
        assert_eq!(parse_ts("garbage"), None);
    }

    #[test]
    fn says_how_long_ago_in_buckets() {
        let (m, h, d, w) = (60, 3600, 86400, 604800);
        let cases = [
            (-5, "just now"),
            (0, "just now"),
            (m - 1, "just now"),
            (m, "1m ago"),
            (h - 1, "59m ago"),
            (h, "1h ago"),
            (d - 1, "23h ago"),
            (d, "1d ago"),
            (2 * w - 1, "13d ago"),
            (2 * w, "2w ago"),
        ];
        for (secs, words) in cases {
            assert_eq!(ago_words(secs), words, "{secs}s");
        }
        assert_eq!(ago_words(i64::MAX), format!("{}w ago", i64::MAX / w));
        assert_eq!(ago(now() - h), "1h");
        assert_eq!(ago(now()), "now");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn reading_a_timestamp_never_panics(
            s in prop_oneof![
                "\\PC{0,30}",
                concat!(
                    "[0-9+\\-]{4}-[0-9+\\-]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}",
                    "(\\.[0-9]{0,6})?(Z|[+\\-][0-9:]{0,6}|\\PC{0,3})?",
                ),
                "[0-9TZ:.+\\- é日]{0,30}",
            ],
        ) {
            parse_ts(&s);
        }

        #[test]
        fn a_timestamp_reads_the_same_in_each_form(
            (y, mo, d) in (1970..2100i64, 1..=12i64, 1..=28i64),
            (h, mi, se) in (0..24i64, 0..60i64, 0..60i64),
            frac in "(\\.[0-9]{1,6})?",
            (sign, oh, om) in (prop::sample::select(vec!['+', '-']), 0..=14i64, 0..60i64),
        ) {
            let at = format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{se:02}");
            let utc = parse_ts(&format!("{at}Z")).unwrap();
            prop_assert_eq!(parse_ts(&at), Some(utc));
            prop_assert_eq!(parse_ts(&format!("{at}{frac}Z")), Some(utc));
            let offset = (oh * 3600 + om * 60) * if sign == '-' { -1 } else { 1 };
            for zone in [format!("{sign}{oh:02}:{om:02}"), format!("{sign}{oh:02}{om:02}")] {
                prop_assert_eq!(parse_ts(&format!("{at}{frac}{zone}")), Some(utc - offset));
            }
            let next = format!("{y:04}-{mo:02}-{:02}T{h:02}:{mi:02}:{se:02}Z", d + 1);
            prop_assert_eq!(parse_ts(&next), Some(utc + 86400));
        }
    }
}
