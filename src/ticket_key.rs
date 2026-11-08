//! Ticket keys, written once: a prefix of letters, digits or `_` starting with a letter, a dash and
//! a number (`ENG-52`); what reads one out of text someone typed or pasted (`parse`), for the Add
//! form, `taskman add` and `status`; and the strict reading of the commands that act on a ticket
//! (`exact`, for `close` and `title`), which never looks inside a phrase. A tracker's own ways of
//! writing one (GitHub's `#12`) stay with the tracker (`Tracker::key_from_input`).

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

/// The key `input` is when read whole, as the commands that act on a ticket read it (`taskman
/// close`, `title`): a key on its own (`ENG-52`, in any case), or a link that names it where Jira
/// and Linear do, right after `/browse/`, `/issue/` or `/issues/` in its path or as
/// `selectedIssue=` in its query. Nothing is read out of a phrase, another word or any other link,
/// unlike `parse`: closing removes worktrees, so a command never guesses which ticket text means.
pub fn exact(input: &str) -> Option<String> {
    let s = input.trim();
    if well_formed(s) {
        return Some(s.to_uppercase());
    }
    let rest = link(s)?;
    let rest = rest.split_once('#').map_or(rest, |(r, _)| r);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let parts: Vec<&str> = path.split('/').collect();
    let after = parts
        .windows(2)
        .find(|w| matches!(w[0], "browse" | "issue" | "issues") && well_formed(w[1]))
        .map(|w| w[1]);
    let param = || {
        query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .find_map(|(name, v)| (name == "selectedIssue" && well_formed(v)).then_some(v))
    };
    after.or_else(param).map(str::to_uppercase)
}

/// Whether `s` is a web link alone, as `exact` reads one (`link`).
pub fn is_link(s: &str) -> bool {
    link(s).is_some()
}

/// Whether what's before the first `://` in `s`, if it has one, is `http` or `https`, in any case.
/// `see:https://…` and `ftp://…` aren't.
pub fn web_scheme_or_none(s: &str) -> bool {
    s.split_once("://")
        .is_none_or(|(scheme, _)| ["http", "https"].iter().any(|w| scheme.eq_ignore_ascii_case(w)))
}

/// What follows the host of `s` when it's a web link alone: `http://` or `https://` and a host
/// (`jira`, `localhost:8080`), or with no scheme a host with a dot in it and a `/`
/// (`linear.app/acme/issue/ENG-52`). None for anything else, a path like `./browse/ENG-52` too.
fn link(s: &str) -> Option<&str> {
    if s.is_empty() || s.contains(char::is_whitespace) || !web_scheme_or_none(s) {
        return None;
    }
    let (rest, scheme) = s.split_once("://").map_or((s, false), |(_, rest)| (rest, true));
    let (host, after) = rest.split_once('/')?;
    let (name, port) = host.split_once(':').unwrap_or((host, "0"));
    let label = |l: &str| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let labels = name.split('.').collect::<Vec<_>>();
    (labels.iter().all(|l| label(l)) && (scheme || labels.len() > 1) && is_number(port))
        .then_some(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reads_a_key_only_from_the_whole_input() {
        let jira = "https://example.atlassian.net";
        for input in [
            "ENG-52",
            "eng-52",
            "\tENG-52\n",
            &format!("{jira}/browse/ENG-52"),
            &format!("{jira}/browse/ENG-52?focusedCommentId=10#comment-10"),
            &format!("{jira}/browse/ENG-52#ENG-7"),
            &format!("{jira}/jira/software/projects/ENG/boards/1?selectedIssue=ENG-52"),
            &format!("{jira}/jira/software/c/projects/ENG/issues/ENG-52?filter=all"),
            "https://linear.app/acme/issue/ENG-52/phase-2",
            "HTTP://linear.app/team-1/issue/eng-52",
            // The path names it before the query does.
            "https://linear.app/acme/issue/ENG-52/x?selectedIssue=ENG-7#y",
            "linear.app/acme/issue/ENG-52",
            "jira.example.com:8080/browse/ENG-52",
            // With a scheme, a host needs no dot: a self-hosted Jira's often has none.
            "http://jira:8080/browse/ENG-52",
            "http://localhost/browse/ENG-52",
        ] {
            assert_eq!(exact(input).as_deref(), Some("ENG-52"), "{input}");
        }
        for input in [
            "",
            "Fix ENG-52 now",
            "ENG-52 ENG-53",
            "close ENG-52",
            "see https://example.atlassian.net/browse/ENG-52",
            "https://example.atlassian.net/browse/ENG-52 now",
            "fix/ENG-52",
            "ENG-52,",
            "(ENG-52)",
            "ENG-52.",
            "#ENG-52",
            "52",
            "https://example.com/t/ENG-52/split-2",
            "https://example.com/view?id=ENG-52",
            "https://example.com/#ENG-52",
            "https://example.com/#/browse/ENG-52",
            "https://example.atlassian.net/browse/ENG-52x",
            "ftp://example.com/browse/ENG-52",
            "file:///browse/ENG-52",
            "browse/ENG-52",
            "./browse/ENG-52",
            "../issues/ENG-52",
            "jira/browse/ENG-52",
            ".example.com/browse/ENG-52",
            "example..com/browse/ENG-52",
            "example.com./browse/ENG-52",
            "https:///browse/ENG-52",
            "https://jira:/browse/ENG-52",
            "see:https://example.atlassian.net/browse/ENG-52",
            "src/browse/ENG-52",
            "https://github.com/acme/ENG-52/issues/88",
            "https://eng-1.example.com/ENG-52",
        ] {
            assert_eq!(exact(input), None, "{input}");
        }
    }

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

        /// Keys alone and in Jira's and Linear's links read whole; with a word before or after,
        /// never.
        #[test]
        fn only_a_whole_key_or_link_reads_exactly(
            (key, upper) in key(),
            words in prop::collection::vec("[A-Za-z0-9#/:.-]{1,10}", 1..3),
            space in "[ \t\n]{1,2}",
            workspace in "[a-z][a-z0-9-]{0,10}",
            words_first in any::<bool>(),
        ) {
            let jira = "https://example.atlassian.net";
            for whole in [
                key.clone(),
                format!("{jira}/browse/{key}"),
                format!("{jira}/jira/software/projects/ENG/boards/1?selectedIssue={key}"),
                format!("https://linear.app/{workspace}/issue/{key}/fix-login"),
            ] {
                prop_assert_eq!(exact(&whole), Some(upper.clone()), "{}", whole);
                let words = words.join(&space);
                let phrase = if words_first {
                    format!("{words}{space}{whole}")
                } else {
                    format!("{whole}{space}{words}")
                };
                prop_assert_eq!(exact(&phrase), None, "{:?}", phrase);
            }
        }

        #[test]
        fn what_exact_reads_is_a_key_in_the_input(
            input in "(\\PC|[-/=?&# ]|[A-Za-z]{1,3}-[0-9]{1,3}|https?://|browse/|issue/){0,12}",
        ) {
            if let Some(k) = exact(&input) {
                prop_assert!(well_formed(&k), "{k}");
                prop_assert_eq!(k.to_uppercase(), k.clone());
                prop_assert!(input.to_uppercase().contains(&k), "{input:?} {k}");
                prop_assert!(!input.trim().contains(char::is_whitespace), "{input:?}");
                prop_assert_eq!(exact(&k), Some(k.clone()));
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
