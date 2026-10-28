//! A CLI's message as plain text, for the turns overlay (`prefix d`), where a terminal would show
//! `**` and backticks as they are: `headline` picks a line to show for a turn, `plain` takes the
//! marks out of the whole message. Not a Markdown parser: the marks the CLIs write (headings,
//! emphasis, inline code, code fences, links, rules) come out where they're plainly marks, and
//! everything else stays as written. Neither makes the text safe to print: the overlay still draws
//! it through `untrusted`.

/// The line a turn shows collapsed: the first real sentence of `message`. A heading or a lone
/// bold line is one, as Claude's `## Fixed the login race` sums its turn up, unless it's a label
/// that says nothing about the turn (Codex's `**Summary**`, `## What changed`: `generic`), which is
/// skipped with blank lines, rules and code; when a list comes first, it's the first item's. A
/// first sentence of a word or two (`Done.`) takes the next one along. One line, Markdown taken
/// out; empty when there's no text.
pub fn headline(message: &str) -> String {
    let mut lines = message.lines().map(str::trim);
    let mut code = false;
    let first = loop {
        let Some(l) = lines.next() else { return String::new() };
        if fence(l) {
            code = !code;
        } else if code || l.is_empty() || rule(l) {
        } else if let Some(title) = heading(l).or_else(|| lone_bold(l)) {
            if !generic(title) {
                return first_sentence(title);
            }
        } else {
            break l;
        }
    };
    let mut para = list_item(first).map_or(first, |(_, text)| text).to_string();
    for l in lines {
        if l.is_empty() || fence(l) || heading(l).is_some() || rule(l) || list_item(l).is_some() {
            break;
        }
        para.push(' ');
        para.push_str(l);
    }
    first_sentence(&para)
}

/// The first sentence of `para`, its Markdown taken out and on one line, with the next along when
/// it's under three words.
fn first_sentence(para: &str) -> String {
    let para = inline(para);
    let para = para.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = sentence_end(&para, 0);
    while para[..end].split_whitespace().count() < 3 && end < para.len() {
        end = sentence_end(&para, end);
    }
    para[..end].trim().to_string()
}

/// Where the sentence in `s` starting at byte `from` ends: after a `.`, `!` or `?` followed by a
/// space, or at the end. `e.g.` and the like don't end one.
fn sentence_end(s: &str, from: usize) -> usize {
    let mut chars = s[from..].char_indices().map(|(i, c)| (from + i, c)).peekable();
    while let Some((i, c)) = chars.next() {
        let next_space = chars.peek().is_none_or(|&(_, n)| n == ' ');
        if matches!(c, '.' | '!' | '?') && next_space {
            let word = s[..i].rsplit(' ').next().unwrap_or("").to_lowercase();
            if !(c == '.' && ["e.g", "i.e", "vs", "cf"].contains(&word.as_str())) {
                return i + c.len_utf8();
            }
        }
    }
    s.len()
}

/// `message` without its Markdown, for reading whole: headings, emphasis, inline code's backticks,
/// links' targets, code fences and rules come out; list markers (`*` and `+` as `-`), indents and
/// line breaks stay, and a code block's lines stay as written, indented two more.
pub fn plain(message: &str) -> String {
    let mut out = Vec::new();
    let mut code = false;
    for line in message.lines() {
        let t = line.trim_start();
        let indent = &line[..line.len() - t.len()];
        if fence(t) {
            code = !code;
        } else if code {
            out.push(format!("  {line}"));
        } else if rule(t) {
            out.push(String::new());
        } else if let Some(h) = heading(t) {
            out.push(format!("{indent}{}", inline(h)));
        } else if let Some((marker, text)) = list_item(t) {
            let marker = if matches!(marker, "*" | "+") { "-" } else { marker };
            out.push(format!("{indent}{marker} {}", inline(text)));
        } else {
            out.push(format!("{indent}{}", inline(t)));
        }
    }
    out.join("\n")
}

/// `line`, its indent left out, as a list item: its marker (`-`, `*`, `+`, `1.`, `1)`) and text.
pub fn list_item(line: &str) -> Option<(&str, &str)> {
    let (marker, text) = line.trim_start().split_once([' ', '\t'])?;
    let number = marker
        .strip_suffix(['.', ')'])
        .is_some_and(|n| (1..=9).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()));
    (matches!(marker, "-" | "*" | "+") || number).then_some((marker, text.trim_start()))
}

/// A heading's text, when `line` (its indent left out) is one.
fn heading(line: &str) -> Option<&str> {
    let text = line.trim_start_matches('#');
    let level = line.len() - text.len();
    ((1..=6).contains(&level) && (text.is_empty() || text.starts_with([' ', '\t'])))
        .then(|| text.trim())
}

/// Whether `line` (trimmed) opens or closes a code block.
fn fence(line: &str) -> bool {
    line.starts_with("```") || line.starts_with("~~~")
}

/// Whether `line` (trimmed) is a rule: three or more of `-`, `*` or `_`, alone.
fn rule(line: &str) -> bool {
    let marks: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    marks.len() >= 3 && matches!(marks[0], '-' | '*' | '_') && marks.iter().all(|&c| c == marks[0])
}

/// The text of `line` (trimmed) when it's bold and nothing else, as a label: `**Summary**`,
/// `**Done:**`.
fn lone_bold(line: &str) -> Option<&str> {
    let line = line.strip_suffix(':').unwrap_or(line);
    ["**", "__"].iter().find_map(|m| {
        line.strip_prefix(m)
            .and_then(|l| l.strip_suffix(m))
            .filter(|inner| !inner.trim().is_empty() && !inner.contains(m))
    })
}

/// Whether a heading's or a bold line's text is a label that could head any turn, ignoring case,
/// marks and a trailing colon: `Summary`, `What changed`, `Done`.
fn generic(title: &str) -> bool {
    const LABELS: &[&str] = &[
        "summary",
        "summary of changes",
        "changes",
        "changes made",
        "what changed",
        "what i changed",
        "what i did",
        "what was done",
        "overview",
        "result",
        "results",
        "outcome",
        "done",
        "notes",
        "note",
        "details",
        "update",
        "updates",
        "status",
        "recap",
        "tl;dr",
        "tldr",
        "highlights",
        "next",
        "next steps",
        "files",
        "files changed",
        "testing",
        "tests",
        "verification",
        "conclusion",
    ];
    let title = inline(title).to_lowercase();
    let title = title.trim().trim_end_matches(':').trim();
    LABELS.contains(&title)
}

/// `s` with its inline marks taken out: code spans' backticks (their text stays as written),
/// links' targets (`[text](url)` reads `text`), and emphasis (`*`, `_`, `**`, `__`, `~~`) where it
/// opens and closes like Markdown's, so `snake_case` and a lone `*` stay.
pub fn inline(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let n = c.len();
    let mut keep = vec![true; n];
    let mut code = vec![false; n];
    let run_at = |i: usize| c[i..].iter().take_while(|&&x| x == c[i]).count();
    // Code spans: a run of backticks to the next run as long. Once a run finds none, no later
    // run as long can, so it isn't looked for again: each run's search is to the end.
    let mut unclosed = std::collections::HashSet::new();
    let mut i = 0;
    while i < n {
        if c[i] != '`' {
            i += 1;
            continue;
        }
        let run = run_at(i);
        let mut j = i + run;
        let close = loop {
            if unclosed.contains(&run) {
                break None;
            }
            match (j..n).find(|&k| c[k] == '`') {
                Some(k) if run_at(k) == run => break Some(k),
                Some(k) => j = k + run_at(k),
                None => {
                    unclosed.insert(run);
                    break None;
                }
            }
        };
        match close {
            Some(j) => {
                keep[i..i + run].fill(false);
                keep[j..j + run].fill(false);
                code[i..j + run].fill(true);
                i = j + run;
            }
            None => {
                code[i..i + run].fill(true);
                i += run;
            }
        }
    }
    // Links: `[text](target)`, an image's `!` before it.
    let mut i = 0;
    while i < n {
        let link = (c[i] == '[' && !code[i])
            .then(|| (i + 1..n).find(|&k| c[k] == ']' || c[k] == '[' || code[k]))
            .flatten()
            .filter(|&k| c[k] == ']' && c.get(k + 1) == Some(&'('))
            // A target has no spaces or brackets, so the search for its end stops at one.
            .and_then(|k| {
                let end = |m: &usize| matches!(c[*m], ')' | '(' | '[') || c[*m].is_whitespace();
                (k + 2..n).find(end).map(|m| (k, m))
            })
            .filter(|&(_, m)| c[m] == ')');
        match link {
            Some((k, m)) => {
                keep[i] = false;
                keep[k..=m].fill(false);
                if i > 0 && c[i - 1] == '!' && !code[i - 1] {
                    keep[i - 1] = false;
                }
                i = m + 1;
            }
            None => i += 1,
        }
    }
    // Emphasis: a run that can open, paired with the next run of the same marks that can close.
    let mut runs = Vec::new();
    let mut i = 0;
    while i < n {
        if !matches!(c[i], '*' | '_' | '~') || code[i] || !keep[i] {
            i += 1;
            continue;
        }
        let len = (i..n).take_while(|&k| c[k] == c[i] && keep[k] && !code[k]).count();
        let before = i.checked_sub(1).map(|k| c[k]);
        let after = c.get(i + len).copied();
        let space = |x: Option<char>| x.is_none_or(char::is_whitespace);
        let word = |x: Option<char>| x.is_some_and(char::is_alphanumeric);
        let intraword = c[i] == '_' && (word(before) || word(after));
        let opens = !space(after) && !(c[i] == '_' && word(before));
        let closes = !space(before) && !(c[i] == '_' && word(after));
        if (c[i] != '~' || len == 2) && (opens || closes) && !(intraword && opens && closes) {
            runs.push((i, len, opens, closes));
        }
        i += len;
    }
    // The runs that can close, by their mark and length, in order: an opener takes the first one
    // after it not yet taken, and those before it are no use to any later opener either.
    let mut closers: std::collections::HashMap<_, std::collections::VecDeque<usize>> =
        std::collections::HashMap::new();
    for (b, &(j, len, _, closes)) in runs.iter().enumerate() {
        if closes {
            closers.entry((c[j], len)).or_default().push_back(b);
        }
    }
    let mut used = vec![false; runs.len()];
    for a in 0..runs.len() {
        let (i, len, opens, _) = runs[a];
        if used[a] || !opens {
            continue;
        }
        let Some(queue) = closers.get_mut(&(c[i], len)) else { continue };
        while queue.front().is_some_and(|&b| b <= a || used[b]) {
            queue.pop_front();
        }
        if let Some(b) = queue.pop_front() {
            used[a] = true;
            used[b] = true;
            keep[i..i + len].fill(false);
            keep[runs[b].0..runs[b].0 + len].fill(false);
        }
    }
    c.iter().zip(keep).filter(|&(_, k)| k).map(|(&x, _)| x).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_headline_is_the_first_sentence() {
        // As Claude Code tends to write: a plain opening sentence.
        let claude = "I've fixed the flaky resize test by waiting for the session to start before \
                      sending keys. The change is in `tests/resize.rs`.\n\nAll tests pass.";
        assert_eq!(
            headline(claude),
            "I've fixed the flaky resize test by waiting for the session to start before sending \
             keys."
        );
        assert_eq!(headline("Opened the PR: CI is running"), "Opened the PR: CI is running");
        assert_eq!(
            headline("\n\n  Fixed it, e.g. the lint. Then more."),
            "Fixed it, e.g. the lint."
        );
        assert_eq!(headline("Version 0.1.2 is out. Next."), "Version 0.1.2 is out.");
        // A heading that sums the turn up is its headline, as Claude Code often opens.
        let claude = "## Committed `hello, acme` and NOTES.txt\n\n- **Edit:** `src/main.rs`.";
        assert_eq!(headline(claude), "Committed hello, acme and NOTES.txt");
        assert_eq!(headline("**Fixed the login race.**\n\nDetails."), "Fixed the login race.");
        // A short one takes the next sentence along.
        assert_eq!(
            headline("Done. Pushed the fix to the branch. CI is green."),
            "Done. Pushed the fix to the branch."
        );
        // A sentence over lines is one line.
        assert_eq!(
            headline("Moved the check\ninto `poll.rs`.\nMore."),
            "Moved the check into poll.rs."
        );
    }

    #[test]
    fn a_headline_skips_headings_labels_and_lists() {
        // As Codex tends to write: a bold label or a heading, then bullets.
        let codex = "**Summary**\n- Added the `--json` flag to `taskman status`.\n- Updated tests.";
        assert_eq!(headline(codex), "Added the --json flag to taskman status.");
        let codex = "## What changed\n\n**Files:**\n\n1. `src/poll.rs`: polls *less* often.\n2. x";
        assert_eq!(headline(codex), "src/poll.rs: polls less often.");
        let codex = "# Summary:\n__Changes made__\n\n## Result\n\nThe tests pass now.";
        assert_eq!(headline(codex), "The tests pass now.");
        assert_eq!(headline("---\n```sh\ncargo test\n```\nTests pass now."), "Tests pass now.");
        assert_eq!(headline("**Fixed** the lint failure."), "Fixed the lint failure.");
        assert_eq!(
            headline("See [the PR](https://example.com/pr/1) for more."),
            "See the PR for more."
        );
        assert_eq!(headline(""), "");
        assert_eq!(headline("# Summary\n\n**Notes**"), "");
    }

    #[test]
    fn plain_takes_the_marks_out_and_keeps_the_shape() {
        let message = "## Summary\n\n**Fixed** the `lint` failure:\n\n* first, in _store.rs_\n  \
                       + nested\n3. third\n\n---\n```rust\nlet a_b = *c;\n```\n> quoted";
        assert_eq!(
            plain(message),
            "Summary\n\nFixed the lint failure:\n\n- first, in store.rs\n  - nested\n3. third\n\n\n  \
             let a_b = *c;\n> quoted"
        );
    }

    #[test]
    fn inline_marks_come_out_only_in_pairs() {
        let cases = [
            ("**bold** and *it* and __b__ and _i_", "bold and it and b and i"),
            ("***both***", "both"),
            ("~~gone~~ but ~one~ stays", "gone but ~one~ stays"),
            ("snake_case_name and __init__", "snake_case_name and init"),
            ("all *.rs files and *.toml", "all *.rs files and *.toml"),
            ("2 * 3 * 4", "2 * 3 * 4"),
            ("`*not* emphasis` and ``a ` b``", "*not* emphasis and a ` b"),
            ("an unclosed `tick", "an unclosed `tick"),
            ("![logo](a.png) [link](https://x.y/z) [1] [a b](c d)", "logo link [1] [a b](c d)"),
            ("**unclosed bold", "**unclosed bold"),
        ];
        for (s, want) in cases {
            assert_eq!(inline(s), want, "{s}");
        }
    }

    #[test]
    fn inline_takes_time_in_step_with_the_text() {
        // Marks that never pair, in each place `inline` looks for a partner: quadratic, this
        // takes minutes; in step, milliseconds.
        for mark in ["*a ", "_a ", "~~a ", "[a](b", "[a](b(", "``a`"] {
            let s = mark.repeat(200_000 / mark.len());
            let started = std::time::Instant::now();
            inline(&s);
            assert!(started.elapsed().as_secs() < 5, "{mark}: {:?}", started.elapsed());
        }
    }

    #[test]
    fn list_items() {
        assert_eq!(list_item("- a"), Some(("-", "a")));
        assert_eq!(list_item("  12. b"), Some(("12.", "b")));
        assert_eq!(list_item("3) c"), Some(("3)", "c")));
        assert_eq!(list_item("-a"), None);
        assert_eq!(list_item("2024. was a year"), Some(("2024.", "was a year")));
        assert_eq!(list_item("v1. no"), None);
    }

    /// A label as the CLIs write them, in any case.
    fn label() -> impl Strategy<Value = String> {
        (
            prop::sample::select(vec!["Summary", "what changed", "DONE", "Notes:", "Result"]),
            any::<bool>(),
        )
            .prop_map(|(l, upper)| if upper { l.to_uppercase() } else { l.to_string() })
    }

    fn words() -> impl Strategy<Value = String> {
        "[a-zA-Z]{1,8}( [a-zA-Z]{1,8}){0,6}"
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// Whatever the text, the headline is one line made of its characters, in order: marks
        /// only come out.
        #[test]
        fn a_headline_only_takes_out(s in "\\PC{0,60}(\n\\PC{0,30}){0,4}") {
            let h = headline(&s);
            prop_assert!(!h.contains('\n'));
            let mut rest = s.chars().map(|c| if c.is_whitespace() { ' ' } else { c });
            prop_assert!(h.chars().all(|x| rest.any(|c| c == x)), "{h:?} from {s:?}");
            plain(&s);
        }

        /// Whatever labels come before it, the headline is the first sentence of the first
        /// paragraph or item, its marks taken out.
        #[test]
        fn a_headline_after_labels(
            heading in prop::option::of(label()),
            label in prop::option::of(label()),
            bullet in any::<bool>(),
            // Words of three letters or more: not an abbreviation like `vs.`, which ends none.
            first in "[a-z]{3,8}( [a-z]{3,8}){0,6}",
            rest in words(),
        ) {
            let first = format!("Then we {first}.");
            let mut message = String::new();
            if let Some(h) = heading {
                message.push_str(&format!("## {h}\n\n"));
            }
            if let Some(l) = label {
                message.push_str(&format!("**{l}**\n"));
            }
            let marked = first.replacen("Then", "**Then**", 1);
            message.push_str(&format!("{}{marked} {rest}.", if bullet { "- " } else { "" }));
            prop_assert_eq!(headline(&message), first);
        }

        /// A heading that isn't a label is the headline, whatever follows it.
        #[test]
        fn a_heading_that_says_something(title in "[a-z]{3,8}( [a-z]{3,8}){1,5}", rest in words()) {
            let message = format!("## Fixed {title}\n\n- {rest}.");
            prop_assert_eq!(headline(&message), format!("Fixed {title}"));
        }

        /// Text without marks comes through `plain` as it was.
        #[test]
        fn plain_text_stays(lines in prop::collection::vec(words(), 1..5)) {
            let s = lines.join("\n");
            prop_assert_eq!(plain(&s), s);
        }
    }
}
