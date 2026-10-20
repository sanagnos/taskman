//! Ticket keys, written once: a prefix of letters, digits or `_` starting with a letter, a dash and
//! a number (`ENG-52`), and what reads one out of text someone typed or pasted (`parse`), for the
//! Add form and the commands alike. A tracker's own ways of writing one (GitHub's `#12`) stay
//! with the tracker (`Tracker::key_from_input`).

use std::fmt::Display;

/// Whether `s` reads as a ticket prefix: letters, digits or `_`, starting with a letter.
pub fn is_prefix(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `s` reads as the number a key ends in: one digit or more.
pub fn is_number(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Whether `s` reads as a key, in any case: a prefix, a dash, a number.
pub fn well_formed(s: &str) -> bool {
    s.split_once('-').is_some_and(|(p, n)| is_prefix(p) && is_number(n))
}

/// The key of `number` under `prefix`: `ENG-52`.
pub fn join(prefix: &str, number: impl Display) -> String {
    format!("{}-{number}", prefix.to_uppercase())
}

/// The key a bare number names in a project whose prefix is `prefix`: `52` is `ENG-52`. None for
/// anything else, or when the project has no prefix.
pub fn complete(input: &str, prefix: &str) -> Option<String> {
    (!prefix.is_empty() && is_number(input)).then(|| join(prefix, input))
}

/// A key's prefix: `ENG` in `ENG-52`, and the whole of what has no dash. Split at the last dash,
/// as `number` is, so the two read any key the same way.
pub fn prefix_of(key: &str) -> &str {
    key.rsplit_once('-').map_or(key, |(p, _)| p)
}

/// The number in a key: 12 in `WEB-12`. Read after the last dash, whatever the prefix before it,
/// so a key written some other way (`A-B-1`) still gives its number.
pub fn number(key: &str) -> Option<u64> {
    let (prefix, n) = key.rsplit_once('-')?;
    (!prefix.is_empty() && is_number(n)).then(|| n.parse().ok()).flatten()
}

/// Accepts `ENG-52`, `eng-52`, or a link to one: Jira's `/browse/ENG-52` or `selectedIssue=ENG-52`,
/// Linear's `/issue/ENG-52/slug`. Any other link gives the first part of its path that reads as a
/// key, then of its query, then of its fragment: a slug ending in `phase-2` after the key, or a
/// fragment `#comment-10`, is never the ticket.
pub fn parse(input: &str) -> Option<String> {
    let s = input.trim();
    let (s, fragment) = s.split_once('#').unwrap_or((s, ""));
    let (path, query) = s.split_once('?').unwrap_or((s, ""));
    // The host can read as a key (`eng-52.example.com`), so the path starts after it.
    let path = path
        .split_once("://")
        .map_or(path, |(_, rest)| rest.split_once('/').map_or("", |(_, p)| p));
    let parts: Vec<&str> = path.split(['/', ' ']).collect();
    let after =
        |marker: &str| parts.windows(2).find(|w| w[0] == marker && well_formed(w[1])).map(|w| w[1]);
    let params = || query.split('&').filter_map(|p| p.split_once('='));
    let key = after("browse")
        .or_else(|| after("issue"))
        .or_else(|| {
            params().find(|&(name, v)| name == "selectedIssue" && well_formed(v)).map(|(_, v)| v)
        })
        .or_else(|| parts.iter().copied().find(|p| well_formed(p)))
        .or_else(|| params().map(|(_, v)| v).find(|v| well_formed(v)))
        .or_else(|| fragment.split(['/', '=', '?', '&', ' ']).find(|p| well_formed(p)))?;
    Some(key.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reads_prefixes_numbers_and_keys() {
        for good in ["ENG", "eng", "A_1", "x"] {
            assert!(is_prefix(good), "{good}");
        }
        for bad in ["", "1ENG", "_A", "EN-G", "EN G"] {
            assert!(!is_prefix(bad), "{bad}");
        }
        for good in ["ENG-12", "A_1-7", "web-3"] {
            assert!(well_formed(good), "{good}");
        }
        for bad in ["ENG-", "-12", "1ENG-2", "ENG-1x", "ENG 1-2", "ENG-1; rm", "#12", "A-B-1", ""] {
            assert!(!well_formed(bad), "{bad}");
        }
    }

    #[test]
    fn completes_bare_numbers() {
        assert_eq!(complete("52", "eng").as_deref(), Some("ENG-52"));
        assert_eq!(complete("52", ""), None);
        assert_eq!(complete("", "ENG"), None);
        assert_eq!(complete("#52", "ENG"), None);
        assert_eq!(complete("ENG-52", "ENG"), None);
        assert_eq!(join("wp", 3), "WP-3");
    }

    #[test]
    fn reads_numbers_and_prefixes_from_keys() {
        assert_eq!(number("WEB-12"), Some(12));
        assert_eq!(number("A_1-7"), Some(7));
        assert_eq!(number("WEB-"), None);
        assert_eq!(number("-12"), None);
        assert_eq!(number("12"), None);
        assert_eq!(number("WEB-1x"), None);
        assert_eq!(prefix_of("WEB-12"), "WEB");
        assert_eq!(prefix_of("WEB"), "WEB");
        // A key that isn't well formed is still split at its last dash, by both.
        assert_eq!(number("A-B-1"), Some(1));
        assert_eq!(prefix_of("A-B-1"), "A-B");
    }

    #[test]
    fn parses_keys_and_links() {
        assert_eq!(parse("eng-52").as_deref(), Some("ENG-52"));
        assert_eq!(parse("https://example.atlassian.net/browse/ENG-52").as_deref(), Some("ENG-52"));
        let board = "https://example.atlassian.net/jira/software/projects/ENG/boards/1";
        assert_eq!(parse(&format!("{board}?selectedIssue=ENG-7")).as_deref(), Some("ENG-7"));
        // What follows the key in a link isn't the ticket, even when it reads as a key.
        assert_eq!(
            parse("https://example.atlassian.net/browse/ENG-52?focusedCommentId=10#comment-10")
                .as_deref(),
            Some("ENG-52")
        );
        assert_eq!(
            parse("https://linear.app/acme/issue/ENG-52/phase-2").as_deref(),
            Some("ENG-52")
        );
        assert_eq!(parse("linear.app/acme/issue/eng-52/phase-2").as_deref(), Some("ENG-52"));
        // A workspace or host that reads as a key doesn't count either.
        assert_eq!(parse("https://linear.app/team-1/issue/ENG-52").as_deref(), Some("ENG-52"));
        assert_eq!(parse("https://eng-1.example.com/tickets/ENG-52").as_deref(), Some("ENG-52"));
        // Other links: the first key in the path, then in the query.
        assert_eq!(parse("https://example.com/t/ENG-52/split-2").as_deref(), Some("ENG-52"));
        assert_eq!(parse("https://example.com/view?id=ENG-52&page=2").as_deref(), Some("ENG-52"));
        // The fragment only when the path and query have no key.
        assert_eq!(
            parse("https://example.atlassian.net/browse/ENG-52#comment-10").as_deref(),
            Some("ENG-52")
        );
        assert_eq!(parse("https://example.com/#ENG-52").as_deref(), Some("ENG-52"));
        assert_eq!(parse("https://example.com/#/view?id=ENG-52").as_deref(), Some("ENG-52"));
        assert_eq!(parse("hello"), None);
        assert_eq!(parse("ENG-"), None);
    }

    /// A ticket key in any case, and its canonical form.
    fn key() -> impl Strategy<Value = (String, String)> {
        "[A-Za-z][A-Za-z0-9_]{0,6}-[0-9]{1,5}".prop_map(|k| (k.clone(), k.to_uppercase()))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn keys_parse_alone_and_in_links(
            (key, upper) in key(),
            (other, _) in key(),
            space in "[ \t]{0,2}",
            // Slugs and workspaces can end in a number, so they can read as keys too.
            words in "[a-z]{1,8}(-[a-z0-9]{1,8}){0,3}",
            workspace in "[a-z][a-z0-9-]{0,10}",
            comment in "[0-9]{1,6}",
            dir in "[a-z]{1,8}",
        ) {
            let jira = "https://example.atlassian.net";
            for input in [
                format!("{space}{key}{space}"),
                format!("{jira}/browse/{key}"),
                format!("{jira}/jira/software/projects/ENG/boards/1?selectedIssue={key}"),
                format!("{jira}/browse/{key}?focusedCommentId={comment}"),
                format!("{jira}/browse/{key}?focusedCommentId={comment}#comment-{comment}"),
                format!("{jira}/browse/{key}#{other}"),
                format!("https://linear.app/{workspace}/issue/{key}/{words}"),
                format!("https://linear.app/{workspace}/issue/{key}/{words}#comment-{comment}"),
                format!("linear.app/{workspace}/issue/{key}"),
                // Unknown links: the first key in the path.
                format!("https://example.com/{dir}/{key}/{other}?id={other}#{other}"),
                // A key in the fragment only when the path and query have none.
                format!("https://example.com/{dir}?page={comment}#{key}/{other}"),
            ] {
                prop_assert_eq!(parse(&input), Some(upper.clone()), "{}", input);
            }
        }

        #[test]
        fn what_parses_is_a_key(input in "(\\PC|[-/=?&# ]|[A-Za-z]{1,3}-[0-9]{1,3}){0,12}") {
            if let Some(k) = parse(&input) {
                prop_assert!(well_formed(&k), "{k}");
                prop_assert_eq!(k.to_uppercase(), k.clone());
                prop_assert_eq!(parse(&k), Some(k.clone()));
            }
        }

        /// A well-formed key has one dash, so reading it from either end gives the same parts,
        /// and they make the key again.
        #[test]
        fn a_key_splits_into_its_prefix_and_number((key, upper) in key()) {
            prop_assert!(well_formed(&key));
            let prefix = prefix_of(&key);
            prop_assert!(is_prefix(prefix), "{key}");
            prop_assert_eq!(Some(prefix), key.split_once('-').map(|(p, _)| p));
            let digits = &key[prefix.len() + 1..];
            prop_assert_eq!(number(&key), digits.parse().ok());
            prop_assert_eq!(join(prefix, digits), upper);
        }

        #[test]
        fn reading_takes_any_text(s in "(\\PC|[-_0-9])*", prefix in "\\PC{0,4}") {
            if well_formed(&s) {
                prop_assert!(is_prefix(prefix_of(&s)), "{s}");
                prop_assert!(is_number(&s[prefix_of(&s).len() + 1..]), "{s}");
            }
            if let Some(n) = number(&s) {
                prop_assert!(s.ends_with(&n.to_string()), "{s}");
            }
            if let Some(k) = complete(&s, &prefix) {
                prop_assert!(is_number(&s));
                prop_assert_eq!(k, join(&prefix, &s));
            }
        }
    }
}
