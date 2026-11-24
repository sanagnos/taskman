//! `taskman wake KEY`: asks the running taskman to start a ticket's sleeping, or not yet started,
//! session again in its own pane, as selecting it does, so an agent can then message it. Like
//! `taskman add` it only leaves a request in the inbox; the TUI decides and answers once the
//! session is up (`app::wake`). A session reaches its own project's tickets only (`cli::confine`).

use crate::add::{self, Item, Line, Request};
use crate::cli::{self, Arg, Lexer};
use crate::status::{self, Here};
use crate::store::{self, Cli, Saved, Ticket};
use crate::untrusted;
use std::path::PathBuf;
use std::time::Duration;

const USAGE: &str = "Usage: taskman wake KEY [--project NAME]

Starts a ticket's session again in its own pane in taskman, as selecting it does, without
switching to it: one taskman put to sleep, or one that hasn't started since taskman did. It
answers once the session is up, so you can message it, or says it's still starting. Needs
taskman running. A session handed to another terminal, one that ended, and a Codex session,
which can't be messaged, aren't started.

  KEY             a ticket number (52), key (ENG-52) or link
  --project NAME  the project, when the folder or session doesn't say which. In a session
                  taskman started, only the session's own";

/// How long the command waits for the reply: the TUI answers once the session's CLI reports its
/// start, or says it's still starting just before this runs out (`app::wake`).
const WAIT: Duration = Duration::from_secs(20);

pub fn run(args: &[String]) -> i32 {
    cli::run("wake", USAGE, args, |args| wake(args).map(|ok| i32::from(!ok)))
}

/// Prints the TUI's reply; returns whether the session is up or starting.
fn wake(args: &[String]) -> Result<bool, String> {
    let a = parse_args(args)?;
    let saved = store::load()?;
    let (session, session_repo) = store::session_env();
    let me = session
        .as_deref()
        .and_then(|s| store::session_ticket(&saved.tickets, s, session_repo.as_deref()));
    let own = cli::session_project(&saved, session_repo.as_deref())?;
    let (repo, key) = target(&a, &saved, me, own, Here::current().as_ref())?;
    // Queued, it would wake a session at taskman's next start that nobody is waiting for.
    if !add::tui_running() {
        return Err("taskman isn't running, so no session is either; start taskman first".into());
    }
    let codex = std::env::var(status::CODEX_NO_NETWORK).is_ok_and(|v| v == "1");
    let req = Request {
        repo,
        from: session.as_deref().map(|s| add::from(s, me)),
        items: vec![Item::Wake { key }],
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

/// The project's main checkout and the key of the ticket to wake: one open in taskman, in the
/// session's own project when run in one (`own`).
fn target(
    a: &Args,
    saved: &Saved,
    me: Option<&Ticket>,
    own: Option<usize>,
    here: Option<&Here>,
) -> Result<(PathBuf, String), String> {
    let named = a.project.as_deref().map(|n| cli::own_named(saved, n, own)).transpose()?;
    let (key, (t, project)) = cli::locate_ticket(&a.input, named, me, saved, here, own)?;
    let p = saved
        .projects
        .iter()
        .position(|q| q.repo == project.repo)
        .ok_or_else(|| format!("{key}'s project is no longer in taskman"))?;
    cli::confine(p, own, saved)?;
    let t = t.ok_or_else(|| format!("{key} isn't open in {}", untrusted::line(&project.name)))?;
    if t.agent.is_some() {
        return Err(format!(
            "{} is an agent, and agents don't sleep; wake takes a ticket",
            t.display()
        ));
    }
    Ok((project.repo.clone(), t.key.clone()))
}

fn show(line: &Line) {
    let text = untrusted::clean(&line.text);
    if line.ok {
        cli::out!("{text}");
    } else {
        eprintln!("taskman wake: {text}");
    }
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    /// The ticket's number, key or link, as typed.
    input: String,
    project: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut words = Vec::new();
    let mut lex = Lexer::new(args, USAGE);
    while let Some(arg) = lex.next()? {
        match arg {
            Arg::Option("--project") => a.project = Some(lex.value()?.to_string()),
            Arg::Option(_) => return Err(lex.unknown()),
            Arg::Word(w) => words.push(w.to_string()),
        }
    }
    match <[String; 1]>::try_from(words) {
        Ok([input]) => a.input = input,
        Err(words) if words.is_empty() => {
            return Err(format!("name the ticket, like taskman wake ENG-52\n\n{USAGE}"));
        }
        Err(_) => return Err("wake one ticket at a time, like taskman wake ENG-52".into()),
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::saved;

    fn args(v: &[&str]) -> Result<Args, String> {
        parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn takes_one_ticket_and_the_project() {
        assert_eq!(args(&["ENG-52"]), Ok(Args { input: "ENG-52".into(), project: None }));
        assert_eq!(
            args(&["52", "--project", "acme"]),
            Ok(Args { input: "52".into(), project: Some("acme".into()) })
        );
        assert!(args(&[]).unwrap_err().starts_with("name the ticket"));
        assert!(args(&["ENG-52", "ENG-53"]).unwrap_err().contains("one ticket at a time"));
        assert!(args(&["--all-projects", "ENG-52"]).is_err());
    }

    /// In a session only its own project's tickets, and no answer names another project; from a
    /// shell, any project's.
    #[test]
    fn a_session_wakes_only_its_own_projects_tickets() {
        let s = saved();
        let a = |input: &str, project: Option<&str>| Args {
            input: input.into(),
            project: project.map(String::from),
        };
        let web = Some(0);
        let found = target(&a("ENG-52", None), &s, None, web, None).unwrap();
        assert_eq!(found, (PathBuf::from("/src/acme-web"), "ENG-52".into()));
        assert_eq!(target(&a("52", None), &s, None, web, None).unwrap(), found);
        // From acme-api's session: its own tracker answers for any prefix, and --project for
        // another project is refused without naming it.
        let api = Some(1);
        let e = target(&a("ENG-52", None), &s, None, api, None).unwrap_err();
        assert!(!e.contains("acme-web"), "{e}");
        let e = target(&a("ENG-52", Some("acme-web")), &s, None, api, None).unwrap_err();
        assert!(e.contains("keeps to its own project") && !e.contains("acme-web"), "{e}");
        // From a shell, --project picks any.
        assert_eq!(target(&a("52", Some("acme-web")), &s, None, None, None).unwrap(), found);
        let e = target(&a("ENG-53", Some("acme-web")), &s, None, None, None).unwrap_err();
        assert!(e.contains("ENG-53 isn't open in acme-web"), "{e}");
    }
}
