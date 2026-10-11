//! Small helpers: timestamps, relative ages, opening URLs.

use std::process::{Command, Stdio};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().cast_signed())
        .unwrap_or(0)
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

/// Unix seconds → `YYYY-MM-DDTHH:MM:SSZ`, the format `parse_ts` reads.
pub fn iso_from_unix(ts: i64) -> String {
    let (days, secs) = (ts.div_euclid(86400), ts.rem_euclid(86400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// "now", "5m", "3h", "2d", "6w".
pub fn ago(ts: i64) -> String {
    let s = (now() - ts).max(0);
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
        for ts in [0, 1709294400, 951782400, 4102444799] {
            assert_eq!(parse_ts(&iso_from_unix(ts)), Some(ts));
        }
    }
}
