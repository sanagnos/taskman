//! `taskman close KEY...` and `taskman close --merged`: closes tickets, or an agent, in the running
//! taskman from any shell or session. Like `taskman add` it only leaves a request in the inbox: the
//! TUI holds the tickets and their sessions, closes each as the close dialog does and removes a
//! worktree only when the removal check finds nothing it would lose (`App::close_requested`), and
//! the command prints its reply.

use crate::add::{self, Close, Item, Line, Request};
use crate::app::parse_key;
use crate::cli::{self, Arg, Lexer};
use crate::resume::Hold;
use crate::status::{self, Here, Scope};
use crate::store::{self, Cli, Saved, Ticket};
use crate::{untrusted, worktree};
use std::path::{Path, PathBuf};
use std::time::Duration;

const USAGE: &str = "Usage: taskman close KEY... [options]
       taskman close --merged [KEY...] [options]
       taskman close --agent NAME [--project NAME]

Closes tickets in taskman as closing one there does: its session stops, and the worktree taskman
created for it is removed, with its branch once merged, only if nothing would be lost: no
uncommitted, untracked or ignored files (such as .env; caches tagged with CACHEDIR.TAG, like
Cargo's target/, and .DS_Store-style OS files don't count), no commits that are on no remote and
not in the merged PR.
Otherwise the worktree stays and the reply says why. Nothing is forced. If taskman isn't running,
the tickets close when it starts.

  KEY             a ticket number (52), key (ENG-52) or link; several at once
  --merged        only tickets whose PR has merged; without KEY, every such ticket in the
                  project (in every project, run outside one)
  --keep-worktree close without removing any worktree
  --agent NAME    close a project agent instead
  --project NAME  the project, when the folder or session doesn't say which";

/// How long the command waits for the reply: the TUI looks the PRs up first, and removing
/// worktrees runs git for each.
const WAIT: Duration = Duration::from_secs(30);

pub fn run(args: &[String]) -> i32 {
    cli::run("close", USAGE, args, |args| close(args).map(|ok| i32::from(!ok)))
}

/// Prints a line per ticket; returns whether each was closed, or had nothing to do.
fn close(args: &[String]) -> Result<bool, String> {
    let a = parse_args(args)?;
    let saved = store::load()?;
    let (session, session_repo) = store::session_env();
    let me = session
        .as_deref()
        .and_then(|s| store::session_ticket(&saved.tickets, s, session_repo.as_deref()));
    let plan = plan(&a, &saved, me, Here::current().as_ref())?;
    let codex = std::env::var(status::CODEX_NO_NETWORK).is_ok_and(|v| v == "1");
    let mut all_ok = true;
    let mut show = |line: Line| {
        all_ok &= line.ok;
        let text = untrusted::clean(&line.text);
        if line.ok {
            println!("{text}");
        } else {
            eprintln!("taskman close: {text}");
        }
    };
    for line in plan.lines {
        show(line);
    }
    for (p, items) in plan.requests {
        let req = Request {
            repo: saved.projects[p].repo.clone(),
            from: session.as_deref().map(|s| add::from(s, me)),
            items,
            cli: Cli::Claude,
            worktree: None,
            base: None,
            reply_by: 0,
            close: Some(Close { merged: a.merged, keep_worktree: a.keep_worktree }),
        };
        for line in add::submit(req, WAIT, codex)? {
            show(line);
        }
    }
    Ok(all_ok)
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    /// Ticket numbers, keys or links, as typed.
    inputs: Vec<String>,
    agent: Option<String>,
    project: Option<String>,
    merged: bool,
    keep_worktree: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut lex = Lexer::new(args, USAGE);
    while let Some(arg) = lex.next()? {
        match arg {
            Arg::Option("--merged") => a.merged = true,
            Arg::Option("--keep-worktree") => a.keep_worktree = true,
            Arg::Option("--agent") => a.agent = Some(lex.value()?.to_string()),
            Arg::Option("--project") => a.project = Some(lex.value()?.to_string()),
            Arg::Option(_) => return Err(lex.unknown()),
            Arg::Word(w) => a.inputs.push(w.to_string()),
        }
    }
    if a.agent.is_some() {
        if !a.inputs.is_empty() {
            return Err("--agent closes one agent; close tickets with another taskman close".into());
        }
        if a.merged || a.keep_worktree {
            return Err(
                "agents have no PR or worktree of their own; drop --merged and --keep-worktree"
                    .into(),
            );
        }
    }
    if a.agent.is_none() && a.inputs.is_empty() && !a.merged {
        return Err(format!(
            "name a ticket to close, like taskman close ENG-52, or pass --merged for those whose PR merged\n\n{USAGE}"
        ));
    }
    Ok(a)
}

/// What to ask taskman, per project, and what the command can answer itself.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    /// Items to close by project index, in the order asked.
    requests: Vec<(usize, Vec<Item>)>,
    lines: Vec<Line>,
}

impl Plan {
    fn push(&mut self, p: usize, item: Item) {
        match self.requests.iter_mut().find(|(q, _)| *q == p) {
            Some((_, items)) if !items.contains(&item) => items.push(item),
            Some(_) => {}
            None => self.requests.push((p, vec![item])),
        }
    }
}

/// Finds each ticket's project as `taskman status` does (`status::locate`), so a key open in two
/// projects is never closed in the wrong one; keys that aren't open are answered here.
fn plan(a: &Args, saved: &Saved, me: Option<&Ticket>, here: Option<&Here>) -> Result<Plan, String> {
    let named = a.project.as_deref().map(|n| cli::project_named(&saved.projects, n)).transpose()?;
    let mut plan = Plan::default();
    if let Some(name) = &a.agent {
        let p = cli::pick_project(a.project.as_deref(), &[], me, saved, here)?;
        let project = &saved.projects[p];
        let name = store::slug(name);
        if store::find_agent(&saved.tickets, &project.repo, &name).is_some() {
            plan.push(p, Item::Agent { name, role: None });
        } else {
            plan.lines.push(Line::ok(format!("✦ {name} isn't open in {}", project.name)));
        }
        return Ok(plan);
    }
    if a.inputs.is_empty() {
        // --merged alone: taskman picks the merged tickets from those it has open.
        let projects: Vec<usize> = match named.or_else(|| cli::context(me, saved, here)) {
            Some(p) => vec![p],
            None => (0..saved.projects.len()).collect(),
        };
        let open = |p: usize| {
            saved.tickets.iter().any(|t| t.repo == saved.projects[p].repo && t.agent.is_none())
        };
        for &p in &projects {
            if open(p) {
                plan.requests.push((p, Vec::new()));
            }
        }
        if plan.requests.is_empty() {
            let place = match projects[..] {
                [p] => saved.projects[p].name.clone(),
                _ => "taskman".into(),
            };
            plan.lines.push(Line::ok(format!("no tickets are open in {place}")));
        }
        return Ok(plan);
    }
    for input in &a.inputs {
        let located = if !input.is_empty() && input.chars().all(|c| c.is_ascii_digit()) {
            cli::pick_project(a.project.as_deref(), &[], me, saved, here)
                .and_then(|p| cli::resolve_key(input, p, &saved.projects).map(|k| (k, p)))
                .and_then(|(key, p)| {
                    status::locate(&key, saved, Scope::Named(&saved.projects[p].repo))
                        .map(|l| (key, l))
                })
        } else {
            parse_key(input)
                .ok_or_else(|| format!("{input} isn't a ticket number, key or link"))
                .and_then(|key| {
                    status::locate(&key, saved, status::scope(named, None, me, saved, here))
                        .map(|l| (key, l))
                })
        };
        match located {
            Err(e) => plan.lines.push(Line::err(e)),
            Ok((key, (None, project))) => {
                plan.lines.push(Line::ok(format!("{key} isn't open in {}", project.name)));
            }
            Ok((_, (Some(t), project))) => {
                let p =
                    saved.projects.iter().position(|q| q.repo == project.repo).unwrap_or_default();
                plan.push(p, Item::Ticket { key: t.key.clone(), title: String::new() });
            }
        }
    }
    Ok(plan)
}

// ---- the TUI's side ---------------------------------------------------------------------

/// What closing did with one ticket, for the reply.
#[derive(Debug, PartialEq, Clone)]
pub enum Outcome {
    NotOpen,
    /// With `--merged`: its PR, if it has one, hasn't merged.
    NotMerged(Option<u64>),
    /// With `--merged`: its PR couldn't be looked up, so it stays open.
    Unchecked(String),
    /// Its session runs in another terminal (`taskman resume`), which holds its worktree.
    Elsewhere,
    /// It stays open, as the state couldn't be saved without it: why.
    Unsaved(String),
    Closed(Worktree),
}

/// What happened to a closed ticket's worktree.
#[derive(Debug, PartialEq, Clone)]
pub enum Worktree {
    /// It had none: an agent, or a ticket in the main checkout.
    None,
    /// Made outside taskman, so taskman leaves it.
    NotOurs(PathBuf),
    /// `--keep-worktree`.
    Asked(PathBuf),
    /// Kept, and why: the removal check found work in it, or it changed meanwhile.
    Kept(PathBuf, String),
    /// Removed, as `worktree::remove` says (with its branch, or why the branch stayed).
    Removed(String),
}

/// Removes a closed ticket's worktree at `path` if the removal check, run now, finds nothing
/// removing it would lose (the close dialog's `r`). `merged` is the head of the ticket's merged PR;
/// `hold` its owner lock, as `remove` takes it.
pub fn remove_worktree(repo: &Path, path: &Path, merged: Option<&str>, hold: &Hold) -> Worktree {
    match worktree::removal_check(repo, path, merged) {
        // git did run: a folder it doesn't list isn't taskman's to delete.
        Ok(None) if path.exists() => {
            Worktree::Kept(path.into(), "git doesn't list it as a worktree".into())
        }
        // Nothing could be checked, so nothing goes; the reply says what happened.
        Err(e) if path.exists() => Worktree::Kept(path.into(), e),
        Ok(None) | Err(_) => Worktree::None,
        // Ignored files too: deleting them is the close dialog's to offer, never a command's.
        Ok(Some(c)) if !c.safe() => Worktree::Kept(path.into(), c.kept_because()),
        Ok(Some(c)) => match remove(repo, &c, merged, hold) {
            Ok(done) => Worktree::Removed(done),
            Err(e) => Worktree::Kept(path.into(), e),
        },
    }
}

/// `worktree::remove`, unless a ticket open in taskman runs in the folder again. The owner lock
/// the caller holds, `hold`, keeps the closed ticket from coming back meanwhile (`App::close_at`);
/// git gets a copy of it, so that lasts while git deletes even if taskman ends. This is the last
/// look before deleting, in case another ticket took the folder.
pub fn remove(
    repo: &Path,
    check: &worktree::RemoveCheck,
    merged: Option<&str>,
    hold: &Hold,
) -> Result<String, String> {
    if let Some(e) = in_use(&store::load(), &check.path) {
        return Err(e);
    }
    worktree::remove(repo, check, merged, hold)
}

/// Why a closed ticket's worktree stays when its session's owner lock couldn't be taken (`e`, from
/// `resume::take`: the state folder can't be written, say). The lock is what keeps a new session
/// out of the folder while git deletes it, so without it nothing is removed.
pub fn unreserved(e: &std::io::Error) -> String {
    format!(
        "couldn't claim its session to keep the folder free while git removes it ({e}); \
         check that {} can be written",
        store::tilde(&store::status_dir())
    )
}

/// Why the folder at `path` stays: a saved ticket runs in it, or the saved state can't be read
/// to tell.
fn in_use(saved: &Result<Saved, String>, path: &Path) -> Option<String> {
    let saved = match saved {
        Ok(saved) => saved,
        Err(_) => {
            return Some("couldn't read taskman's saved tickets to check nothing uses it".into());
        }
    };
    let same = |t: &Ticket| {
        t.cwd == path
            || t.cwd.canonicalize().ok().is_some_and(|c| path.canonicalize().ok() == Some(c))
    };
    saved.tickets.iter().find(|t| same(t)).map(|t| format!("{} is open in it again", t.display()))
}

/// Why ticket `name` stays open: the state can't be saved (`e`, from `App::try_save`).
pub fn unsaved(name: &str, e: &str) -> String {
    format!("{name} stays open: {e}")
}

/// The reply's line for ticket `name` (a key, or `✦ agent`) in project `project`.
pub fn line(name: &str, project: &str, o: &Outcome) -> Line {
    let kept = |path: &Path, why: &str| format!("Closed {name}. Kept {}{why}", store::tilde(path));
    match o {
        Outcome::NotOpen => Line::ok(format!("{name} isn't open in {project}")),
        Outcome::NotMerged(Some(n)) => {
            Line::ok(format!("{name}'s PR #{n} isn't merged: not closed"))
        }
        Outcome::NotMerged(None) => Line::ok(format!("{name} has no PR: not closed")),
        Outcome::Unchecked(e) => {
            Line::err(format!("couldn't look up {name}'s PR, so it stays open: {e}"))
        }
        Outcome::Elsewhere => Line::err(elsewhere(name)),
        Outcome::Unsaved(e) => Line::err(unsaved(name, e)),
        Outcome::Closed(w) => Line::ok(match w {
            Worktree::None => format!("Closed {name}"),
            Worktree::NotOurs(p) => kept(p, ": taskman didn't create it"),
            Worktree::Asked(p) => kept(p, " (--keep-worktree)"),
            Worktree::Kept(p, why) => kept(p, &format!(": {why}")),
            Worktree::Removed(done) => format!("Closed {name}. {done}"),
        }),
    }
}

/// Why `name` doesn't close, in the reply and the TUI alike.
pub fn elsewhere(name: &str) -> String {
    format!("{name} is running in another terminal; quit it there first")
}

/// The TUI's toast for a close request: what closed, and which worktrees stayed with work in them.
/// None when nothing closed.
pub fn toast(from: Option<&str>, done: &[(String, Outcome)]) -> Option<String> {
    let closed: Vec<&str> = done
        .iter()
        .filter(|(_, o)| matches!(o, Outcome::Closed(_)))
        .map(|(n, _)| n.as_str())
        .collect();
    if closed.is_empty() {
        return None;
    }
    let who = from.map_or("Closed".to_string(), |f| format!("{f} closed"));
    let kept: Vec<String> = done
        .iter()
        .filter_map(|(n, o)| match o {
            Outcome::Closed(Worktree::Kept(_, why)) => Some(format!("{n}'s worktree ({why})")),
            _ => None,
        })
        .collect();
    let kept = if kept.is_empty() { String::new() } else { format!("; kept {}", kept.join(", ")) };
    Some(format!("{who} {}{kept}", closed.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Project, Provider, Tracker};

    fn args(s: &str) -> Result<Args, String> {
        parse_args(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    fn project(name: &str, repo: &str, prefix: &str) -> Project {
        Project {
            name: name.into(),
            repo: repo.into(),
            tracker: Some(Tracker {
                provider: Provider::Linear,
                site: "acme".into(),
                folder: None,
                key: prefix.into(),
                account: None,
                extra: Default::default(),
            }),
            collapsed: false,
            closed_agents: vec![],
            worktrees: Default::default(),
            extra: Default::default(),
        }
    }

    fn ticket(key: &str, repo: &str, agent: Option<&str>) -> Ticket {
        Ticket {
            key: key.into(),
            repo: repo.into(),
            cwd: repo.into(),
            cli: Cli::Claude,
            branch: None,
            session_id: None,
            seen_jira: None,
            seen_gh: None,
            agent: agent.map(String::from),
            role: None,
            created_worktree: false,
            base: None,
            pending_worktree: None,
            extra: Default::default(),
        }
    }

    fn saved() -> Saved {
        Saved {
            projects: vec![
                project("acme-web", "/src/acme-web", "ENG"),
                project("acme-api", "/src/acme-api", "API"),
            ],
            tickets: vec![
                ticket("ENG-52", "/src/acme-web", None),
                ticket("ENG-53", "/src/acme-web", None),
                ticket("API-7", "/src/acme-api", None),
                ticket("acme-api-planner", "/src/acme-api", Some("planner")),
            ],
            ..Default::default()
        }
    }

    fn here(repo: &str) -> Here {
        Here {
            dir: repo.into(),
            worktrees: vec![crate::github::Worktree {
                path: repo.into(),
                branch: Some("main".into()),
                locked: None,
            }],
        }
    }

    fn ticket_item(key: &str) -> Item {
        Item::Ticket { key: key.into(), title: String::new() }
    }

    /// The reply tells a folder git doesn't list from one git couldn't check at all: the
    /// worktree stays either way, but "not a worktree" would send the user looking in the wrong
    /// place when git failed.
    #[test]
    fn says_why_a_worktree_could_not_be_checked() {
        let dir = std::env::temp_dir().join(format!("taskman-unchecked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (repo, stray) = (dir.join("acme"), dir.join("acme-ENG-52"));
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let hold = crate::resume::take(&dir.join("ENG-52.owner")).unwrap().unwrap();
        // No repository to run git in: what git said, not "doesn't list it".
        let kept = remove_worktree(&dir.join("missing"), &stray, None, &hold);
        let Worktree::Kept(path, why) = &kept else { panic!("{kept:?}") };
        assert_eq!(path, &stray);
        assert!(why.starts_with("git worktree failed: "), "{why}");
        assert!(!why.contains("doesn't list"), "{why}");
        // git runs, and the folder isn't one of its worktrees.
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(init.success());
        let kept = remove_worktree(&repo, &stray, None, &hold);
        assert_eq!(kept, Worktree::Kept(stray.clone(), "git doesn't list it as a worktree".into()));
        // A folder that isn't there has nothing to keep, whatever git says.
        let gone = dir.join("gone");
        assert_eq!(remove_worktree(&dir.join("missing"), &gone, None, &hold), Worktree::None);
        assert_eq!(remove_worktree(&repo, &gone, None, &hold), Worktree::None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_a_worktree_a_ticket_runs_in_again() {
        let mut saved = saved();
        saved.tickets[1].cwd = "/src/acme-web-ENG-53".into();
        let saved = Ok(saved);
        let reason = in_use(&saved, Path::new("/src/acme-web-ENG-53"));
        assert_eq!(reason.as_deref(), Some("ENG-53 is open in it again"));
        assert_eq!(in_use(&saved, Path::new("/src/acme-web-ENG-52")), None);
        // Unreadable state can't tell: it stays.
        assert!(in_use(&Err("bad".into()), Path::new("/src/acme-web-ENG-52")).is_some());
    }

    #[test]
    fn parses_arguments() {
        assert_eq!(
            args("ENG-52 53"),
            Ok(Args { inputs: vec!["ENG-52".into(), "53".into()], ..Default::default() })
        );
        let a = args("--merged --keep-worktree --project=acme-web").unwrap();
        assert!(a.merged && a.keep_worktree && a.inputs.is_empty());
        assert_eq!(a.project.as_deref(), Some("acme-web"));
        assert_eq!(args("--agent planner").unwrap().agent.as_deref(), Some("planner"));
        assert!(args("").unwrap_err().starts_with("name a ticket to close"));
        assert!(args("--keep-worktree").unwrap_err().starts_with("name a ticket to close"));
        assert!(args("52 --force").unwrap_err().starts_with("unknown option --force"));
        assert_eq!(args("--agent"), Err("--agent needs a value".into()));
        assert_eq!(args("52 --project="), Err("--project needs a value".into()));
        // An option that takes no value refuses one, rather than reading --merged=no as --merged.
        assert_eq!(args("--merged=no"), Err("--merged takes no value".into()));
        assert!(args("--agent planner 52").unwrap_err().starts_with("--agent closes one agent"));
        assert!(args("--agent planner --merged").unwrap_err().contains("drop --merged"));
    }

    #[test]
    fn plans_each_ticket_in_its_own_project() {
        let s = saved();
        let plan_of = |line: &str, here: Option<&Here>| plan(&args(line).unwrap(), &s, None, here);
        // Keys go to the project that has them open, one request per project.
        assert_eq!(
            plan_of("ENG-52 API-7 eng-53 ENG-52", None).unwrap().requests,
            vec![
                (0, vec![ticket_item("ENG-52"), ticket_item("ENG-53")]),
                (1, vec![ticket_item("API-7")])
            ]
        );
        // A number takes the folder's project's prefix.
        assert_eq!(
            plan_of("53", Some(&here("/src/acme-web"))).unwrap().requests,
            vec![(0, vec![ticket_item("ENG-53")])]
        );
        // What isn't open is answered here, and nothing is sent for it.
        let p = plan_of("ENG-60 login", None).unwrap();
        assert!(p.requests.is_empty());
        assert_eq!(
            p.lines,
            vec![
                Line::ok("ENG-60 isn't open in acme-web"),
                Line::err("login isn't a ticket number, key or link")
            ]
        );
        // The same key open in two projects needs the project.
        let mut two = saved();
        two.projects.push(project("acme-legacy", "/src/legacy", "ENG"));
        two.tickets.push(ticket("ENG-52", "/src/legacy", None));
        let p = plan(&args("ENG-52").unwrap(), &two, None, None).unwrap();
        assert!(p.lines[0].text.contains("pass --project"), "{:?}", p.lines);
        let p = plan(&args("ENG-52 --project acme-legacy").unwrap(), &two, None, None).unwrap();
        assert_eq!(p.requests, vec![(2, vec![ticket_item("ENG-52")])]);
    }

    #[test]
    fn plans_merged_and_agents() {
        let s = saved();
        // --merged alone: the folder's project, else every project with tickets open.
        assert_eq!(
            plan(&args("--merged").unwrap(), &s, None, Some(&here("/src/acme-api")))
                .unwrap()
                .requests,
            vec![(1, vec![])]
        );
        assert_eq!(
            plan(&args("--merged").unwrap(), &s, None, None).unwrap().requests,
            vec![(0, vec![]), (1, vec![])]
        );
        let mut empty = saved();
        empty.tickets.retain(|t| t.agent.is_some());
        let p = plan(&args("--merged").unwrap(), &empty, None, None).unwrap();
        assert_eq!(p.lines, vec![Line::ok("no tickets are open in taskman")]);
        // An agent, by name in the session's project.
        let me = store::session_ticket(&s.tickets, "API-7", None);
        assert_eq!(
            plan(&args("--agent Planner").unwrap(), &s, me, None).unwrap().requests,
            vec![(1, vec![Item::Agent { name: "planner".into(), role: None }])]
        );
        assert_eq!(
            plan(&args("--agent tester").unwrap(), &s, me, None).unwrap().lines,
            vec![Line::ok("✦ tester isn't open in acme-api")]
        );
        // By name, not by a key made from the project's name, which may have changed since.
        let mut renamed = saved();
        renamed.projects[1].name = "api".into();
        assert_eq!(
            plan(&args("--agent planner").unwrap(), &renamed, me, None).unwrap().requests,
            vec![(1, vec![Item::Agent { name: "planner".into(), role: None }])]
        );
    }

    #[test]
    fn says_what_happened_per_ticket() {
        let text = |o: Outcome| line("ENG-52", "acme-web", &o);
        let wt = crate::store::home().join("taskman-ENG-52");
        assert_eq!(text(Outcome::NotOpen), Line::ok("ENG-52 isn't open in acme-web"));
        assert_eq!(
            text(Outcome::NotMerged(Some(12))),
            Line::ok("ENG-52's PR #12 isn't merged: not closed")
        );
        assert_eq!(text(Outcome::NotMerged(None)), Line::ok("ENG-52 has no PR: not closed"));
        assert!(!text(Outcome::Unchecked("gh isn't logged in".into())).ok);
        assert_eq!(
            text(Outcome::Elsewhere),
            Line::err("ENG-52 is running in another terminal; quit it there first")
        );
        assert_eq!(
            text(Outcome::Unsaved("couldn't save ~/state/tickets.json: disk full".into())),
            Line::err("ENG-52 stays open: couldn't save ~/state/tickets.json: disk full")
        );
        assert_eq!(text(Outcome::Closed(Worktree::None)), Line::ok("Closed ENG-52"));
        assert_eq!(
            text(Outcome::Closed(Worktree::Removed(
                "Removed ~/taskman-ENG-52 and branch eng-52".into()
            ))),
            Line::ok("Closed ENG-52. Removed ~/taskman-ENG-52 and branch eng-52")
        );
        assert_eq!(
            text(Outcome::Closed(Worktree::Kept(wt.clone(), "it has 2 unpushed commits".into()))),
            Line::ok("Closed ENG-52. Kept ~/taskman-ENG-52: it has 2 unpushed commits")
        );
        assert_eq!(
            text(Outcome::Closed(Worktree::Asked(wt.clone()))),
            Line::ok("Closed ENG-52. Kept ~/taskman-ENG-52 (--keep-worktree)")
        );
        assert_eq!(
            text(Outcome::Closed(Worktree::NotOurs(wt))),
            Line::ok("Closed ENG-52. Kept ~/taskman-ENG-52: taskman didn't create it")
        );
    }

    #[test]
    fn toasts_what_closed() {
        let done = vec![
            (
                "ENG-52".to_string(),
                Outcome::Closed(Worktree::Removed("Removed ~/taskman-ENG-52".into())),
            ),
            (
                "ENG-53".to_string(),
                Outcome::Closed(Worktree::Kept("/w".into(), "it has 1 untracked file".into())),
            ),
            ("ENG-54".to_string(), Outcome::NotMerged(None)),
        ];
        assert_eq!(
            toast(Some("✦ planner"), &done).as_deref(),
            Some(
                "✦ planner closed ENG-52, ENG-53; kept ENG-53's worktree (it has 1 untracked file)"
            )
        );
        assert_eq!(toast(None, &done[..1]).as_deref(), Some("Closed ENG-52"));
        assert_eq!(toast(None, &done[2..]), None);
    }
}
