//! `taskman title TEXT`: sets the title a ticket's card shows under its key (`Ticket::label`), the
//! field `prefix e` sets, so whatever a session has at hand (a plugin, a script, its own reading of
//! the ticket) can fill in a card when taskman can't reach the tracker. Like `taskman add` it only
//! leaves a request in the inbox. taskman never starts this by itself, and the text is a snapshot
//! that counts as untrusted: the TUI cleans it, and it stays out of briefs and `taskman status`.

use crate::add::{self, Item, Line, Request};
use crate::cli::{self, Arg, Lexer};
use crate::status::{self, Here};
use crate::store::{self, Cli, Saved, Ticket};
use crate::untrusted;
use std::time::Duration;

const USAGE: &str = "Usage: taskman title TEXT
       taskman title KEY TEXT [--project NAME]
       taskman title --clear [KEY] [--project NAME]

Sets what a ticket's card in taskman shows under its key, in place of the tracker's title: the
same title you can set in taskman, and the last one set wins. It stays in taskman: the tracker's
title doesn't change, and this one doesn't follow it. If taskman isn't running, it's set when it
starts.

  TEXT            the title, in quotes if it has spaces; on one line, cleaned of control
                  characters, and kept to 200 columns
  KEY             a ticket number (52), key (ENG-52) or link. In a session taskman started, only
                  the session's own ticket, which is what TEXT alone means there
  --clear         drop the title, so the card shows the tracker's again
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
    let own = cli::session_project(&saved, session_repo.as_deref())?;
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
            Arg::Option("--clear") => clear = true,
            Arg::Option("--project") => a.project = Some(lex.value()?.to_string()),
            Arg::Option(_) => return Err(lex.unknown()),
            Arg::Word(w) => words.push(w.to_string()),
        }
    }
    let mut words = words.into_iter();
    match (clear, words.len()) {
        (true, 0) => {}
        (true, 1) => a.input = words.next(),
        (true, _) => return Err("--clear takes at most a ticket, not a title".into()),
        (false, 0) => {
            return Err(format!(
                "give the title, like taskman title \"Fix the login redirect\"\n\n{USAGE}"
            ));
        }
        (false, 1) => a.text = words.next().unwrap_or_default(),
        (false, 2) => (a.input, a.text) = (words.next(), words.next().unwrap_or_default()),
        (false, _) => return Err("put the title in quotes, as one argument".into()),
    }
    // `"$TITLE"` with nothing in it isn't asking to lose the title.
    if !clear && a.text.trim().is_empty() {
        return Err("the title is empty; to drop the one it has, pass --clear".into());
    }
    Ok(a)
}

/// The project and key of the ticket to title. A session asks the user before running this (it's
/// not on `hook::settings_json`'s allow-list), but a user can allow it for good, and text the
/// session read shouldn't then be able to relabel any card: it reaches its own ticket only, and
/// only a command that leaves its environment goes wider.
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
        None => me.ok_or_else(|| {
            "name the ticket, like taskman title ENG-52 \"Fix the login redirect\"".to_string()
        })?,
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
    cli::confine(p, own, saved)?;
    if own.is_some() && !me.is_some_and(|m| m.is(&t.repo, &t.key)) {
        return Err(format!(
            "that's {}, and a session sets only its own ticket's title. To go ahead, {}.",
            t.key,
            cli::WIDER
        ));
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
        assert_eq!(args(&["--clear"]), Ok(Args::default()));
        assert_eq!(
            args(&["--clear", "52", "--project=acme"]),
            Ok(Args {
                input: Some("52".into()),
                project: Some("acme".into()),
                ..Default::default()
            })
        );
        assert!(args(&[]).unwrap_err().starts_with("give the title"));
        assert!(args(&["a", "b", "c"]).unwrap_err().contains("in quotes"));
        assert!(args(&["--clear", "52", "Fix"]).unwrap_err().contains("--clear takes at most"));
        // An empty variable doesn't clear what's there.
        for empty in [&[""][..], &["  "], &["ENG-52", ""]] {
            assert!(args(empty).unwrap_err().contains("pass --clear"), "{empty:?}");
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
        for a in
            [&["Fix"][..], &["ENG-52", "Fix"], &["eng-52", "Fix"], &["52", "Fix"], &["--clear"]]
        {
            assert_eq!(of(a), Ok((0, "ENG-52".into())), "{a:?}");
        }
        // Another ticket of its project: how to go ahead, which asks the user.
        let e = of(&["ENG-53", "Fix"]).unwrap_err();
        assert!(
            e.contains("a session sets only its own") && e.contains("env -u TASKMAN_REPO"),
            "{e}"
        );
        assert!(of(&["53", "Fix"]).unwrap_err().contains("only its own"));
        // Another project's, by its prefix or by name.
        assert!(of(&["API-7", "Fix"]).is_err());
        let e = of(&["API-7", "Fix", "--project", "acme-api"]).unwrap_err();
        assert!(e.contains("that's project acme-api"), "{e}");
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
