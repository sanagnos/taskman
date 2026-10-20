//! Small helpers: timestamps, relative ages, opening URLs.

use std::process::{Command, Stdio};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

/// Like `truncate`, but in display columns: a wide character counts two and is never split.
/// A sidebar card's name is cut with it so the state badge beside it never is.
pub fn fit_label(label: &str, room: usize) -> String {
    if label.width() <= room {
        return label.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in label.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > room {
            break;
        }
        out.push(c);
        used += cw;
    }
    if room > 0 {
        out.push('…');
    }
    out
}

/// `s` broken into lines at most `width` columns wide, between words; a word longer than that is
/// broken where it reaches the edge. For text whose rows must be counted before it's drawn, or
/// that wraps under an indent of its own (Help).
pub fn wrap_words(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let gap = usize::from(!line.is_empty());
        if line.width() + gap + word.width() <= width {
            if gap == 1 {
                line.push(' ');
            }
            line.push_str(word);
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        for c in word.chars() {
            if line.width() + c.width().unwrap_or(0) > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            line.push(c);
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

/// Like `truncate`, but keeps the end: `…` and the last `max - 1` characters.
pub fn truncate_start(s: &str, max: usize) -> String {
    let n = s.chars().count();
    match max {
        _ if n <= max => s.to_string(),
        0 => String::new(),
        _ => format!("…{}", s.chars().skip(n + 1 - max).collect::<String>()),
    }
}

/// Like `truncate`, but keeps both ends and puts the `…` in the middle.
pub fn truncate_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max == 0 {
        return truncate(s, max);
    }
    let head = max / 2;
    let tail = max - 1 - head;
    format!(
        "{}…{}",
        s.chars().take(head).collect::<String>(),
        s.chars().skip(n - tail).collect::<String>()
    )
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

    #[test]
    fn fits_a_label_in_display_columns() {
        assert_eq!(fit_label("✦ tester", 23), "✦ tester");
        assert_eq!(fit_label("✦ open-source-planner", 21), "✦ open-source-planner");
        assert_eq!(fit_label("✦ open-source-planner", 13), "✦ open-sourc…");
        assert_eq!(fit_label("ENG-52", 4), "ENG…");
        // A wide character that doesn't fit whole is left out, never split.
        assert_eq!(fit_label("✦ 日本語チーム", 7), "✦ 日本…");
        assert_eq!(fit_label("✦ 日本語チーム", 8), "✦ 日本…");
        for room in 0..16 {
            assert!(fit_label("✦ 日本語チーム-planner", room).width() <= room, "room {room}");
        }
        assert_eq!(fit_label("abc", 1), "…");
        assert_eq!(fit_label("abc", 0), "");
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
        fn wrapped_lines_fit_and_keep_the_text(s in "[a-z日 ]{0,60}", width in 2usize..30) {
            let lines = wrap_words(&s, width);
            for l in &lines {
                prop_assert!(l.width() <= width, "{l:?} wider than {width}");
            }
            let joined: String = lines.concat().split_whitespace().collect();
            let words: String = s.split_whitespace().collect();
            prop_assert_eq!(joined, words);
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
