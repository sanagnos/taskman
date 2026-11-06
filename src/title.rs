//! `taskman title TEXT`: sets the title a ticket's card shows under its key (`Ticket::label`), the
//! field `prefix e` sets, so whatever a session has at hand (a plugin, a script, its own reading of
//! the ticket) can fill in a card when taskman can't reach the tracker. Like `taskman add` it only
//! leaves a request in the inbox. taskman never starts this by itself, and the text is a snapshot
//! that counts as untrusted: the TUI cleans it, and it stays out of briefs and `taskman status`.

use crate::add::{self, Item, Line, Request};
use crate::cli::{self, Arg, Lexer};
use crate::status::{self, Here};
use crate::store::{self, Cli, Saved, Ticket};
use crate::{ticket_key, untrusted, util};
use std::time::Duration;

const USAGE: &str = "Usage: taskman title [--] TEXT
       taskman title [--project NAME] [--] KEY TEXT
       taskman title --clear KEY [--project NAME]

Sets what a ticket's card in taskman shows under its key, in place of the tracker's title: the
same title you can set in taskman, and the last one set wins. It stays in taskman: the tracker's
title doesn't change, and this one doesn't follow it. If taskman isn't running, it's set when it
starts.

  TEXT            the title, in quotes if it has spaces; on one line, cleaned of control
                  characters, and kept to 200 columns. Alone, it means the session's own ticket,
                  and it can't be one word that reads as a ticket's number or key; to use one
                  as the title, name the ticket first: taskman title ENG-52 sha-256
  KEY             a ticket number (52), key (ENG-52) or link. In a session taskman started, only
                  the session's own ticket
  --              what follows isn't an option: use it before text from a script, which may
                  start with a dash
  --clear KEY     drop the ticket's title, so its card shows the tracker's again
  --project NAME  the project, when the folder doesn't say which";

/// How long the command waits for the reply.
const WAIT: Duration = Duration::from_secs(5);

pub fn run(args: &[String]) -> i32 {
    cli::run("title", USAGE, args, |args| title(args).map(|ok| i32::from(!ok)))
}

/// Prints the TUI's reply; returns whether the title was set.
fn title(args: &[String]) -> Result<bool, String> {
    let a = parse_args(args)?;
    let saved = store::load()?;
    let (session, session_repo) = store::session_env();
    let me = session
        .as_deref()
        .and_then(|s| store::session_ticket(&saved.tickets, s, session_repo.as_deref()));
    let own = cli::session_project(&saved, session_repo.as_deref())
        .map_err(|_| "this session's project is no longer in taskman".to_string())?;
    let (p, key) = target(&a, &saved, me, own, Here::current().as_ref())?;
    let codex = std::env::var(status::CODEX_NO_NETWORK).is_ok_and(|v| v == "1");
    let req = Request {
        repo: saved.projects[p].repo.clone(),
        from: session.as_deref().map(|s| add::from(s, me)),
        items: vec![Item::Title { key, text: a.text }],
        cli: Cli::Claude,
        worktree: None,
        base: None,
        reply_by: 0,
        close: None,
    };
    let mut all_ok = true;
    for line in add::submit(req, WAIT, codex)? {
        all_ok &= line.ok;
        show(&line);
    }
    Ok(all_ok)
}

fn show(line: &Line) {
    // The reply quotes the title, which may be anyone's text.
    let text = untrusted::clean(&line.text);
    if line.ok {
        cli::out!("{text}");
    } else {
        eprintln!("taskman title: {text}");
    }
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    /// The ticket's number, key or link, as typed; None for the session's own.
    input: Option<String>,
    /// The title; empty with `--clear`.
    text: String,
    project: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut clear = false;
    let mut words = Vec::new();
    let mut lex = Lexer::new(args, USAGE);
    while let Some(arg) = lex.next()? {
        match arg {
            // Takes the ticket as its value, so `taskman title "$TITLE"` or `taskman title KEY
            // "$TITLE"` with `--clear` in the variable is refused for want of one, rather than
            // losing the title.
            Arg::Option("--clear") => {
                clear = true;
                let ticket = lex.value().map_err(|_| {
                    "--clear needs the ticket, like taskman title --clear ENG-52".to_string()
                })?;
                a.input = Some(ticket.to_string());
            }
            Arg::Option("--project") => a.project = Some(lex.value()?.to_string()),
            Arg::Option(_) => return Err(lex.unknown()),
            Arg::Word(w) => words.push(w.to_string()),
        }
    }
    if clear {
        return if words.is_empty() {
            Ok(a)
        } else {
            Err("--clear takes the ticket, and no title".into())
        };
    }
    let mut words = words.into_iter();
    match words.len() {
        0 => {
            return Err(format!(
                "give the title, like taskman title \"Fix the login redirect\"\n\n{USAGE}"
            ));
        }
        1 => a.text = words.next().unwrap_or_default(),
        2 => (a.input, a.text) = (words.next(), words.next().unwrap_or_default()),
        _ => return Err("put the title in quotes, as one argument".into()),
    }
    // On its own, the session's ticket is meant, whatever project is named.
    if a.input.is_none() && a.project.is_some() {
        return Err("--project goes with a ticket: taskman title KEY TEXT --project NAME".into());
    }
    // Someone naming a ticket and forgetting the title shouldn't title their own ticket with it.
    // One word only: `ticket_key::parse` finds a key inside a phrase, and "Move to sha-256" is a
    // title.
    let word = a.text.trim();
    if a.input.is_none()
        && !word.contains(char::is_whitespace)
        && (ticket_key::is_number(word) || ticket_key::parse(word).is_some())
    {
        return Err(not_a_title(word));
    }
    // `"$TITLE"` with nothing in it isn't asking to lose the title, and nor is text with nothing
    // left once it's cleaned, which the TUI would only refuse after the command said it was set.
    if util::blank(&untrusted::line(&a.text)) {
        return Err("the title is empty; to drop the one it has, pass --clear KEY".into());
    }
    Ok(a)
}

fn not_a_title(word: &str) -> String {
    format!(
        "{word} looks like a ticket, not a title. To use it as the title, name the ticket first, \
         like taskman title ENG-52 {word}"
    )
}

/// The project and key of the ticket to title. A session asks the user before running this (it's
/// not on `hook::settings_json`'s allow-list), but a user can allow it for good, and text the
/// session read shouldn't then be able to relabel any card: it reaches its own ticket only.
fn target(
    a: &Args,
    saved: &Saved,
    me: Option<&Ticket>,
    own: Option<usize>,
    here: Option<&Here>,
) -> Result<(usize, String), String> {
    let named = a.project.as_deref().map(|n| cli::project_named(&saved.projects, n)).transpose()?;
    let t = match &a.input {
        Some(input) => {
            let (key, (t, project)) =
                cli::locate_ticket(input, a.project.as_deref(), named, me, saved, here)?;
            t.ok_or_else(|| format!("{key} isn't open in {}", project.name))?
        }
        None => {
            let me = me.ok_or_else(|| {
                "name the ticket, like taskman title ENG-52 \"Fix the login redirect\"".to_string()
            })?;
            // One word that only its tracker reads as a ticket, like GitHub's `#52`. A phrase is a
            // title, whatever a tracker would make of it.
            let project = saved.projects.iter().find(|p| p.repo == me.repo);
            let tracker = project.and_then(|p| p.tracker.as_ref());
            let word = a.text.trim();
            if !word.contains(char::is_whitespace)
                && tracker.and_then(|t| t.key_from_input(word)).is_some()
            {
                return Err(not_a_title(word));
            }
            me
        }
    };
    if t.agent.is_some() {
        return Err(format!(
            "{} is an agent, which goes by its name; only a ticket has a title",
            t.display()
        ));
    }
    let p = saved
        .projects
        .iter()
        .position(|p| p.repo == t.repo)
        .ok_or_else(|| format!("{}'s project is no longer in taskman", t.key))?;
    // Narrower than `cli::confine`, which keeps a session to its project, and the refusal
    // deliberately doesn't say how to go wider, as `confine`'s does: a card's title gives no reason
    // to.
    if own.is_some() && !me.is_some_and(|m| m.is(&t.repo, &t.key)) {
        return Err(format!("that's {}, and a session sets only its own ticket's title", t.key));
    }
    Ok((p, t.key.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::saved;

    fn args(v: &[&str]) -> Result<Args, String> {
        parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_arguments() {
        let text = |t: &str| Args { text: t.into(), ..Default::default() };
        assert_eq!(args(&["Fix login"]), Ok(text("Fix login")));
        assert_eq!(
            args(&["ENG-52", "Fix login"]),
            Ok(Args { input: Some("ENG-52".into()), ..text("Fix login") })
        );
        assert_eq!(
            args(&["--clear", "52", "--project=acme"]),
            Ok(Args {
                input: Some("52".into()),
                project: Some("acme".into()),
                ..Default::default()
            })
        );
        assert_eq!(args(&["--clear=52"]).unwrap().input.as_deref(), Some("52"));
        assert!(args(&[]).unwrap_err().starts_with("give the title"));
        assert!(args(&["a", "b", "c"]).unwrap_err().contains("in quotes"));
        assert!(args(&["--clear", "52", "Fix"]).unwrap_err().contains("--clear takes the ticket"));
        // A phrase is a title whatever's in it, with `--` or without.
        for phrase in ["Fix ENG-53 fallout", "Handle utf-8 names", "Move to sha-256", "Fix 52 now"]
        {
            assert_eq!(args(&[phrase]), Ok(text(phrase)));
            assert_eq!(args(&["--", phrase]), Ok(text(phrase)));
        }
        // A lone word that's a ticket isn't its title; given both, or as the title, it can be.
        for lone in ["ENG-53", "eng-53", "53", "https://linear.app/acme/issue/ENG-53/fix"] {
            let e = args(&[lone]).unwrap_err();
            assert!(e.contains("looks like a ticket, not a title"), "{e}");
            assert!(e.ends_with(&format!("like taskman title ENG-52 {lone}")), "{e}");
            assert!(args(&["--", lone]).unwrap_err().contains("looks like a ticket"));
        }
        assert_eq!(args(&["ENG-52", "ENG-53"]).unwrap().text, "ENG-53");
        // A script's text starting with a dash, after `--`; without it, it's refused.
        assert_eq!(args(&["--", "-v is broken"]), Ok(text("-v is broken")));
        assert_eq!(
            args(&["--", "ENG-52", "--flag"]),
            Ok(Args { input: Some("ENG-52".into()), ..text("--flag") })
        );
        assert!(args(&["-v is broken"]).unwrap_err().starts_with("unknown option -v is broken"));
        // `--clear` in a script's variable loses nothing, however it's passed; after `--`, it's
        // only text.
        let needs = "--clear needs the ticket, like taskman title --clear ENG-52";
        assert_eq!(args(&["--clear"]), Err(needs.into()));
        assert_eq!(args(&["ENG-52", "--clear"]), Err(needs.into()));
        assert_eq!(args(&["--clear="]), Err(needs.into()));
        assert_eq!(args(&["--", "--clear"]), Ok(text("--clear")));
        // `--project` has nothing to name without a ticket.
        assert!(args(&["Fix", "--project", "acme"]).unwrap_err().contains("goes with a ticket"));
        // Nothing left once it's cleaned: refused here, rather than said to be set.
        for hidden in
            ["\u{200b}", "\x1b", "\u{202e} \u{200b}", "\u{200e}", "\u{301}", "\u{200e} \u{301}"]
        {
            let e = args(&["ENG-52", hidden]).unwrap_err();
            assert!(e.contains("pass --clear KEY"), "{hidden:?}: {e}");
        }
        // An empty variable doesn't clear what's there.
        for empty in [&[""][..], &["  "], &["ENG-52", ""]] {
            assert!(args(empty).unwrap_err().contains("pass --clear KEY"), "{empty:?}");
        }
        assert!(args(&["--force"]).unwrap_err().starts_with("unknown option --force"));
        assert_eq!(args(&["x", "--project="]), Err("--project needs a value".into()));
    }

    /// `saved` with ENG-53 open too, and API-7 in the other project.
    fn tickets() -> Saved {
        let mut s = saved();
        let like = |key: &str, repo: &str| {
            let mut t = s.tickets[0].clone();
            (t.key, t.repo, t.cwd) = (key.into(), repo.into(), repo.into());
            t
        };
        let more = [like("ENG-53", "/src/acme-web"), like("API-7", "/src/acme-api")];
        s.tickets.extend(more);
        s
    }

    fn session<'a>(s: &'a Saved, key: &str, repo: &str) -> (Option<&'a Ticket>, Option<usize>) {
        let repo = std::path::Path::new(repo);
        (
            store::session_ticket(&s.tickets, key, Some(repo)),
            cli::session_project(s, Some(repo)).unwrap(),
        )
    }

    #[test]
    fn a_session_titles_only_its_own_ticket() {
        let s = tickets();
        let (me, own) = session(&s, "ENG-52", "/src/acme-web");
        let of = |a: &[&str]| target(&args(a).unwrap(), &s, me, own, None);
        // Its own, with the key or without, however it's written.
        for a in [
            &["Fix"][..],
            &["ENG-52", "Fix"],
            &["eng-52", "Fix"],
            &["52", "Fix"],
            &["--clear", "52"],
        ] {
            assert_eq!(of(a), Ok((0, "ENG-52".into())), "{a:?}");
        }
        // Another ticket of its project, or another project's by its prefix or by name: refused,
        // with no way around it offered.
        let refused = "and a session sets only its own ticket's title";
        assert_eq!(of(&["ENG-53", "Fix"]), Err(format!("that's ENG-53, {refused}")));
        assert_eq!(of(&["53", "Fix"]), Err(format!("that's ENG-53, {refused}")));
        assert_eq!(of(&["--clear", "53"]), Err(format!("that's ENG-53, {refused}")));
        assert_eq!(of(&["API-7", "Fix"]), Err(format!("that's API-7, {refused}")));
        assert_eq!(
            of(&["API-7", "Fix", "--project", "acme-api"]),
            Err(format!("that's API-7, {refused}"))
        );
    }

    /// A key is unique only within its project: `Ticket::is`, the repo's first rule.
    #[test]
    fn the_same_key_in_two_projects_stays_apart() {
        let mut s = tickets();
        s.projects.push(crate::cli::tests::project("legacy", "/src/legacy", "ENG"));
        let mut other = s.tickets[0].clone();
        (other.repo, other.cwd) = ("/src/legacy".into(), "/src/legacy".into());
        s.tickets.push(other);
        let legacy = s.projects.len() - 1;
        let of = |session_repo: &str, a: &[&str]| {
            let (me, own) = session(&s, "ENG-52", session_repo);
            target(&args(a).unwrap(), &s, me, own, None)
        };
        // Each session's ENG-52 is its own, with the key or without.
        for a in [&["Fix"][..], &["ENG-52", "Fix"], &["52", "Fix"]] {
            assert_eq!(of("/src/acme-web", a), Ok((0, "ENG-52".into())), "{a:?}");
            assert_eq!(of("/src/legacy", a), Ok((legacy, "ENG-52".into())), "{a:?}");
        }
        // Naming the other project's refuses, though the key's the same.
        let e = of("/src/acme-web", &["ENG-52", "Fix", "--project", "legacy"]).unwrap_err();
        assert!(e.contains("only its own"), "{e}");
    }

    /// A lone issue reference on a GitHub project is a ticket, though only its tracker reads it so.
    #[test]
    fn a_lone_issue_reference_is_not_a_title() {
        let mut s = tickets();
        let t = s.projects[0].tracker.as_mut().unwrap();
        (t.provider, t.site) = (crate::store::Provider::Github, "acme/web".into());
        let (me, own) = session(&s, "ENG-52", "/src/acme-web");
        let of = |a: &[&str]| target(&args(a).unwrap(), &s, me, own, None);
        for lone in ["#53", "acme/web#53", "https://github.com/acme/web/issues/53"] {
            let e = of(&[lone]).unwrap_err();
            assert!(e.contains("looks like a ticket, not a title"), "{e}");
        }
        for phrase in [
            "#53 broke the login",
            "Port the fix from acme/web#12",
            "Follow up https://github.com/acme/web/issues/12",
        ] {
            assert_eq!(of(&[phrase]), Ok((0, "ENG-52".into())), "{phrase}");
        }
        assert_eq!(of(&["#52", "#53"]), Ok((0, "ENG-52".into())));
        // Elsewhere `#53` means nothing, and can be a title.
        let plain = tickets();
        let (me, own) = session(&plain, "ENG-52", "/src/acme-web");
        let elsewhere = target(&args(&["#53"]).unwrap(), &plain, me, own, None);
        assert_eq!(elsewhere, Ok((0, "ENG-52".into())));
    }

    #[test]
    fn an_agent_has_no_title() {
        let s = tickets();
        let (me, own) = session(&s, "acme-api-planner", "/src/acme-api");
        let of = |a: &[&str]| target(&args(a).unwrap(), &s, me, own, None);
        assert!(of(&["Fix"]).unwrap_err().contains("is an agent"));
        // Nor does it title its project's tickets: a ticket's session does that.
        assert!(of(&["API-7", "Fix"]).unwrap_err().contains("only its own"));
    }

    #[test]
    fn a_shell_names_any_open_ticket() {
        let s = tickets();
        let of = |a: &[&str]| target(&args(a).unwrap(), &s, None, None, None);
        assert_eq!(of(&["ENG-53", "Fix"]), Ok((0, "ENG-53".into())));
        assert_eq!(of(&["API-7", "Fix"]), Ok((1, "API-7".into())));
        assert_eq!(of(&["--clear", "API-7"]), Ok((1, "API-7".into())));
        assert!(of(&["Fix"]).unwrap_err().starts_with("name the ticket"));
        assert_eq!(of(&["ENG-60", "Fix"]), Err("ENG-60 isn't open in acme-web".into()));
        assert!(of(&["login", "Fix"]).unwrap_err().contains("isn't a ticket number"));
        // A key open in two projects needs the project, and with it means that project's.
        let mut two = tickets();
        two.projects.push(crate::cli::tests::project("legacy", "/src/legacy", "ENG"));
        let mut other = two.tickets[0].clone();
        (other.repo, other.cwd) = ("/src/legacy".into(), "/src/legacy".into());
        two.tickets.push(other);
        let of = |a: &[&str]| target(&args(a).unwrap(), &two, None, None, None);
        assert!(of(&["ENG-52", "Fix"]).unwrap_err().contains("pass --project"));
        assert_eq!(of(&["ENG-52", "Fix", "--project", "legacy"]), Ok((3, "ENG-52".into())));
    }
}
