//! What the CLI commands share: how they run and read their arguments, the project a command
//! means, and the ticket key an input names in it (`taskman add`, `move`, `close`, `status`,
//! `resume` and `update`).

use crate::app::parse_key;
use crate::status::{self, Here, real};
use crate::store::{self, Project, Saved, Ticket};
use crate::untrusted;
use std::path::Path;

/// Like println!, but a reader that stops early (`taskman --help | head`) isn't a panic: the line
/// is dropped and the command carries on, so it still finishes what it was doing and releases what
/// it holds.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, $($arg)*);
    }};
}
pub(crate) use out;

/// Runs `taskman NAME`: prints `usage` for `-h` or `--help` anywhere in `args`, before anything is
/// read or fetched, else returns the exit code `command` gives, or prints its error and returns 1.
pub(crate) fn run(
    name: &str,
    usage: &str,
    args: &[String],
    command: impl FnOnce(&[String]) -> Result<i32, String>,
) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        out!("{usage}");
        return 0;
    }
    match command(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("taskman {name}: {}", untrusted::clean(&e));
            1
        }
    }
}

/// One of a command's arguments, as `Lexer` reads it.
#[derive(Debug, PartialEq)]
pub(crate) enum Arg<'a> {
    /// `--name`, or `--name` of `--name=value` with the value kept for `Lexer::value`, or `-x`.
    Option(&'a str),
    /// Anything else, as typed.
    Word(&'a str),
}

/// Reads a command's arguments one at a time, so every command takes `--x`, `--x VALUE` and
/// `--x=VALUE` and refuses an empty value, a value given to an option that takes none and an
/// unknown option alike. Each command matches only the options it knows.
pub(crate) struct Lexer<'a> {
    args: std::slice::Iter<'a, String>,
    usage: &'a str,
    /// The option last read.
    option: &'a str,
    /// The value it came with after `=`, until `value` takes it.
    inline: Option<&'a str>,
}

impl<'a> Lexer<'a> {
    /// `usage` goes with the error for an unknown option.
    pub fn new(args: &'a [String], usage: &'a str) -> Self {
        Self { args: args.iter(), usage, option: "", inline: None }
    }

    pub fn next(&mut self) -> Result<Option<Arg<'a>>, String> {
        // The command matched the option without asking for its value.
        if self.inline.take().is_some() {
            return Err(format!("{} takes no value", self.option));
        }
        let Some(arg) = self.args.next() else { return Ok(None) };
        if !arg.starts_with('-') {
            return Ok(Some(Arg::Word(arg)));
        }
        (self.option, self.inline) = match arg.split_once('=') {
            Some((option, value)) if option.starts_with("--") => (option, Some(value)),
            _ => (arg.as_str(), None),
        };
        Ok(Some(Arg::Option(self.option)))
    }

    /// The value of the option just read: after its `=`, else the next argument, whatever it is.
    pub fn value(&mut self) -> Result<&'a str, String> {
        self.inline
            .take()
            .or_else(|| self.args.next().map(String::as_str))
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{} needs a value", self.option))
    }

    /// The error for the option just read, which the command doesn't know.
    pub fn unknown(&self) -> String {
        format!("unknown option {}\n\n{}", self.option, self.usage)
    }
}

/// The project a command means: the one named, else the session's (`me`), else the folder's, else
/// the one whose ticket prefix the first key has.
pub(crate) fn pick_project(
    named: Option<&str>,
    inputs: &[String],
    me: Option<&Ticket>,
    saved: &Saved,
    here: Option<&Here>,
) -> Result<usize, String> {
    let projects = &saved.projects;
    if projects.is_empty() {
        return Err(format!(
            "taskman has no projects yet; add one in taskman ({} P)",
            crate::keys::prefix()
        ));
    }
    if let Some(name) = named {
        return project_named(projects, name);
    }
    if let Some(p) = context(me, saved, here) {
        return Ok(p);
    }
    let Some(prefix) = inputs.iter().find_map(|i| parse_key(i)).map(|k| prefix_of(&k).to_string())
    else {
        return if projects.len() == 1 {
            Ok(0)
        } else {
            Err("this folder isn't in a project: pass --project NAME, or a whole key like ENG-52"
                .into())
        };
    };
    let matching: Vec<usize> =
        (0..projects.len()).filter(|&p| has_prefix(&projects[p], &prefix)).collect();
    match matching[..] {
        [p] => Ok(p),
        [] => Err(format!("no project has the ticket prefix {prefix}; pass --project NAME")),
        _ => {
            let names = matching
                .iter()
                .map(|&p| projects[p].name.as_str())
                .collect::<Vec<_>>()
                .join(" and ");
            Err(format!("projects {names} both use {prefix}; pass --project NAME"))
        }
    }
}

/// The project `--project NAME` names: the one with that name, else the one with it ignoring case,
/// else the one whose name's slug (as in session names) is its slug. A looser match only counts
/// when one project has it, so another project's exact name is never shadowed by a slug.
pub(crate) fn project_named(projects: &[Project], name: &str) -> Result<usize, String> {
    let names = |ps: &[usize]| ps.iter().map(|&p| projects[p].name.as_str()).collect::<Vec<_>>();
    let slug = store::slug(name);
    let matches = |p: &Project, tier| match tier {
        0 => p.name == name,
        1 => p.name.eq_ignore_ascii_case(name),
        _ => store::slug(&p.name) == slug,
    };
    for tier in 0..3 {
        let found: Vec<usize> =
            (0..projects.len()).filter(|&p| matches(&projects[p], tier)).collect();
        match found[..] {
            [] => continue,
            [p] => return Ok(p),
            _ => {
                return Err(format!(
                    "{name} could mean projects {}; pass --project with the exact name",
                    names(&found).join(" or ")
                ));
            }
        }
    }
    let all: Vec<usize> = (0..projects.len()).collect();
    Err(format!("no project is named {name}; taskman has {}", names(&all).join(", ")))
}

/// The project of the session the command runs in, else of the repository the folder is in.
pub(crate) fn context(me: Option<&Ticket>, saved: &Saved, here: Option<&Here>) -> Option<usize> {
    let of = |repo: &Path| saved.projects.iter().position(|p| real(&p.repo) == repo);
    if let Some(t) = me
        && let Some(p) = of(&real(&t.repo))
    {
        return Some(p);
    }
    status::project_here(&saved.projects, here?)
}

fn prefix_of(key: &str) -> &str {
    key.rsplit_once('-').map_or(key, |(p, _)| p)
}

fn has_prefix(p: &Project, prefix: &str) -> bool {
    p.tracker.as_ref().is_some_and(|t| !t.key.is_empty() && t.key.eq_ignore_ascii_case(prefix))
}

/// The ticket key `input` names in project `p`: a bare number gets the project's prefix, a key
/// with another prefix is refused, naming the project it may belong to, and the tracker's own
/// ways (GitHub's `#12` and issue links) are read by the tracker.
pub(crate) fn resolve_key(input: &str, p: usize, projects: &[Project]) -> Result<String, String> {
    let project = &projects[p];
    if let Some(key) = project.tracker.as_ref().and_then(|t| t.key_from_input(input)) {
        return key;
    }
    let prefix = project.tracker.as_ref().map(|t| t.key.to_uppercase()).filter(|k| !k.is_empty());
    if !input.is_empty() && input.chars().all(|c| c.is_ascii_digit()) {
        return prefix.map(|pre| format!("{pre}-{input}")).ok_or_else(|| {
            format!("{input}: project {} has no ticket prefix; pass the whole key", project.name)
        });
    }
    let key =
        parse_key(input).ok_or_else(|| format!("{input} isn't a ticket number, key or link"))?;
    let Some(pre) = prefix else { return Ok(key) };
    if prefix_of(&key) == pre {
        return Ok(key);
    }
    let theirs = projects
        .iter()
        .find(|q| has_prefix(q, prefix_of(&key)))
        .map(|q| format!("; for project {0}, pass --project {0}", q.name))
        .unwrap_or_default();
    Err(format!("{key} doesn't belong to {}'s tracker ({pre}){theirs}", project.name))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::{Cli, Provider, Tracker};

    pub(crate) fn project(name: &str, repo: &str, prefix: &str) -> Project {
        let tracker = (!prefix.is_empty()).then(|| Tracker {
            provider: Provider::Linear,
            site: "acme".into(),
            key: prefix.into(),
            folder: None,
            account: None,
            extra: Default::default(),
        });
        Project {
            name: name.into(),
            repo: repo.into(),
            tracker,
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

    pub(crate) fn saved() -> Saved {
        Saved {
            projects: vec![
                project("acme-web", "/src/acme-web", "ENG"),
                project("acme-api", "/src/acme-api", "API"),
                project("notes", "/src/notes", ""),
            ],
            tickets: vec![
                ticket("ENG-52", "/src/acme-web", None),
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

    /// A command with a flag (`--json`) and an option with a value (`--project`), as the
    /// commands read theirs: options as `Err` or their value, words as typed.
    fn lexed(args: &[&str]) -> Result<Vec<String>, String> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let mut lex = Lexer::new(&args, "Usage: taskman test");
        let mut out = vec![];
        while let Some(arg) = lex.next()? {
            out.push(match arg {
                Arg::Option("--json") => "json".into(),
                Arg::Option("--project") => format!("project {}", lex.value()?),
                Arg::Option(_) => return Err(lex.unknown()),
                Arg::Word(w) => w.into(),
            });
        }
        Ok(out)
    }

    #[test]
    fn lexes_options_values_and_words() {
        let ok = |v: &[&str]| Ok(v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(lexed(&[]), ok(&[]));
        assert_eq!(
            lexed(&["ENG-52", "--json", "--project", "web", "53"]),
            ok(&["ENG-52", "json", "project web", "53"])
        );
        // A value after `=` keeps any `=` of its own, and the next argument is the value whatever
        // it looks like.
        assert_eq!(lexed(&["--project=a=b"]), ok(&["project a=b"]));
        assert_eq!(lexed(&["--project", "--json"]), ok(&["project --json"]));
        // A link with a query string is a word.
        assert_eq!(lexed(&["https://example.com/?a=b"]), ok(&["https://example.com/?a=b"]));
        // An empty value, given either way, or none.
        for args in [&["--project="][..], &["--project", ""], &["--project"], &["52", "--project"]]
        {
            assert_eq!(lexed(args), Err("--project needs a value".into()), "{args:?}");
        }
        assert_eq!(lexed(&["--json=no"]), Err("--json takes no value".into()));
        assert_eq!(lexed(&["--json=", "52"]), Err("--json takes no value".into()));
        // An unknown option is named without its value, and shown with the usage.
        assert_eq!(lexed(&["--jsn"]), Err("unknown option --jsn\n\nUsage: taskman test".into()));
        assert_eq!(
            lexed(&["--frce=yes"]),
            Err("unknown option --frce\n\nUsage: taskman test".into())
        );
        // A single dash is never split at `=`.
        assert_eq!(lexed(&["-p=web"]), Err("unknown option -p=web\n\nUsage: taskman test".into()));
        assert_eq!(lexed(&["-"]), Err("unknown option -\n\nUsage: taskman test".into()));
    }

    #[test]
    fn resolves_numbers_keys_and_links() {
        let s = saved();
        assert_eq!(resolve_key("52", 0, &s.projects), Ok("ENG-52".into()));
        assert_eq!(resolve_key("eng-52", 0, &s.projects), Ok("ENG-52".into()));
        assert_eq!(
            resolve_key("https://linear.app/acme/issue/ENG-52/fix-login", 0, &s.projects),
            Ok("ENG-52".into())
        );
        assert_eq!(
            resolve_key("https://example.atlassian.net/browse/ENG-52", 0, &s.projects),
            Ok("ENG-52".into())
        );
        assert_eq!(
            resolve_key("API-7", 0, &s.projects),
            Err("API-7 doesn't belong to acme-web's tracker (ENG); for project acme-api, pass --project acme-api"
                .into())
        );
        assert_eq!(
            resolve_key("OPS-1", 0, &s.projects),
            Err("OPS-1 doesn't belong to acme-web's tracker (ENG)".into())
        );
        assert!(
            resolve_key("login", 0, &s.projects).unwrap_err().contains("isn't a ticket number")
        );
        // A project without a prefix takes any key, but not a bare number.
        assert_eq!(resolve_key("OPS-1", 2, &s.projects), Ok("OPS-1".into()));
        assert!(resolve_key("1", 2, &s.projects).unwrap_err().contains("no ticket prefix"));
    }

    #[test]
    fn resolves_github_issues() {
        let mut projects = vec![project("web", "/src/web", "WEB")];
        let t = projects[0].tracker.as_mut().unwrap();
        (t.provider, t.site) = (Provider::Github, "acme/web".into());
        let key = |input: &str| resolve_key(input, 0, &projects);
        for input in ["12", "#12", "web-12", "acme/web#12", "https://github.com/acme/web/issues/12"]
        {
            assert_eq!(key(input), Ok("WEB-12".into()), "{input}");
        }
        assert_eq!(
            key("https://github.com/acme/web/pull/45"),
            Err("acme/web#45 is a pull request; add the issue it closes instead".into())
        );
        assert_eq!(
            key("https://github.com/acme/api/issues/3"),
            Err("that's an issue of acme/api; this project's issues are acme/web".into())
        );
    }

    #[test]
    fn picks_the_project() {
        let s = saved();
        let keys = |k: &[&str]| k.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Named, case and spacing aside.
        assert_eq!(pick_project(Some("Acme-API"), &keys(&["52"]), None, &s, None), Ok(1));
        assert!(
            pick_project(Some("blog"), &keys(&["52"]), None, &s, None)
                .unwrap_err()
                .contains("acme-web, acme-api, notes")
        );
        // The session's project wins over the key's prefix: an agent, or a ticket.
        let me = |key: &str| store::session_ticket(&s.tickets, key, None);
        assert_eq!(pick_project(None, &keys(&["ENG-60"]), me("acme-api-planner"), &s, None), Ok(1));
        assert_eq!(
            pick_project(None, &keys(&["API-1"]), me("ENG-52"), &s, Some(&here("/src/acme-api"))),
            Ok(0)
        );
        // Then the folder's repository.
        assert_eq!(
            pick_project(None, &keys(&["7"]), None, &s, Some(&here("/src/acme-api"))),
            Ok(1)
        );
        // Then the prefix.
        assert_eq!(
            pick_project(None, &keys(&["API-7"]), None, &s, Some(&here("/home/you"))),
            Ok(1)
        );
        assert!(
            pick_project(None, &keys(&["OPS-7"]), None, &s, None)
                .unwrap_err()
                .contains("no project has the ticket prefix OPS")
        );
        assert!(
            pick_project(None, &keys(&["7"]), None, &s, None).unwrap_err().contains("--project")
        );
        let mut two = saved();
        two.projects.push(project("acme-web-legacy", "/src/legacy", "ENG"));
        assert!(
            pick_project(None, &keys(&["ENG-7"]), None, &two, None)
                .unwrap_err()
                .contains("acme-web and acme-web-legacy both use ENG")
        );
        assert!(
            pick_project(None, &keys(&["7"]), None, &Saved::default(), None)
                .unwrap_err()
                .contains("no projects yet")
        );
    }

    #[test]
    fn names_a_project_exactly_before_by_slug() {
        let projects = [
            project("alpha web", "/src/a", ""),
            project("alpha-web", "/src/b", ""),
            project("Beta", "/src/c", ""),
            project("beta", "/src/d", ""),
            project("Gamma Site", "/src/e", ""),
        ];
        // The exact name wins over an earlier project's slug, and over its case.
        assert_eq!(project_named(&projects, "alpha-web"), Ok(1));
        assert_eq!(project_named(&projects, "alpha web"), Ok(0));
        assert_eq!(project_named(&projects, "beta"), Ok(3));
        assert_eq!(project_named(&projects, "Beta"), Ok(2));
        // Ignoring case, or by slug, when one project matches.
        assert_eq!(project_named(&projects, "gamma site"), Ok(4));
        assert_eq!(project_named(&projects, "gamma-site"), Ok(4));
        assert_eq!(project_named(&projects, "Gamma_Site"), Ok(4));
        // Several: refused, naming them.
        assert_eq!(
            project_named(&projects, "Alpha_Web"),
            Err("Alpha_Web could mean projects alpha web or alpha-web; pass --project with the \
                 exact name"
                .into())
        );
        assert_eq!(
            project_named(&projects, "BETA"),
            Err("BETA could mean projects Beta or beta; pass --project with the exact name".into())
        );
        assert_eq!(
            project_named(&projects, "delta"),
            Err("no project is named delta; taskman has alpha web, alpha-web, Beta, beta, \
                 Gamma Site"
                .into())
        );
    }

    mod lexer {
        use super::lexed;
        use proptest::prelude::*;

        /// Words, options with and without `=`, empty values, and dashes alone.
        fn arg() -> impl Strategy<Value = String> {
            prop_oneof![
                "[a-zA-Z0-9/:.?#=-]{0,8}",
                "--(json|project|other)(=[a-z=-]{0,4})?",
                Just("-".to_string()),
                Just("--".to_string()),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// `--project=VALUE` and `--project VALUE` mean the same, wherever they come.
            #[test]
            fn a_value_after_equals_is_the_next_argument(
                before in prop::collection::vec("[a-z0-9]{1,6}", 0..3),
                after in prop::collection::vec("[a-z0-9]{1,6}", 0..3),
                value in "\\PC{1,12}",
            ) {
                let joined = format!("--project={value}");
                let (b, a) = (before.iter().map(String::as_str), after.iter().map(String::as_str));
                let inline: Vec<&str> = b.clone().chain([joined.as_str()]).chain(a.clone()).collect();
                let apart: Vec<&str> = b.chain(["--project", &value]).chain(a).collect();
                prop_assert_eq!(lexed(&inline), lexed(&apart));
                prop_assert!(lexed(&apart).is_ok());
            }

            /// Any arguments lex to the words in order, or to one of the lexer's errors.
            #[test]
            fn arguments_lex_or_say_why_not(args in prop::collection::vec(arg(), 0..8)) {
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let words: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
                match lexed(&refs) {
                    Ok(out) => {
                        // Each argument is read once: as an option, an option's value or a word.
                        prop_assert!(out.len() <= args.len());
                        if words.len() == args.len() {
                            prop_assert_eq!(&out, &args);
                        }
                    }
                    Err(e) => prop_assert!(
                        e == "--project needs a value"
                            || e == "--json takes no value"
                            || e.starts_with("unknown option -"),
                        "{e}"
                    ),
                }
            }
        }
    }

    mod names {
        use super::{project, project_named};
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// Names that slug or fold to the same thing never hide a project's own name.
            #[test]
            fn a_projects_name_finds_it(
                names in prop::collection::hash_set("[A-Za-z]{1,3}([ _-]{1,2}[A-Za-z]{1,3}){0,2}", 1..6),
            ) {
                let projects: Vec<_> =
                    names.iter().map(|n| project(n, &format!("/src/{n}"), "")).collect();
                for (i, p) in projects.iter().enumerate() {
                    prop_assert_eq!(project_named(&projects, &p.name), Ok(i), "{}", p.name);
                }
            }
        }
    }

    mod keys {
        use super::parse_key;
        use proptest::prelude::*;

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
                for input in [
                    format!("{space}{key}{space}"),
                    format!("https://example.atlassian.net/browse/{key}"),
                    format!("https://example.atlassian.net/jira/software/projects/ENG/boards/1?selectedIssue={key}"),
                    format!("https://example.atlassian.net/browse/{key}?focusedCommentId={comment}"),
                    format!("https://example.atlassian.net/browse/{key}?focusedCommentId={comment}#comment-{comment}"),
                    format!("https://example.atlassian.net/browse/{key}#{other}"),
                    format!("https://linear.app/{workspace}/issue/{key}/{words}"),
                    format!("https://linear.app/{workspace}/issue/{key}/{words}#comment-{comment}"),
                    format!("linear.app/{workspace}/issue/{key}"),
                    // Unknown links: the first key in the path.
                    format!("https://example.com/{dir}/{key}/{other}?id={other}#{other}"),
                    // A key in the fragment only when the path and query have none.
                    format!("https://example.com/{dir}?page={comment}#{key}/{other}"),
                ] {
                    prop_assert_eq!(parse_key(&input), Some(upper.clone()), "{}", input);
                }
            }

            #[test]
            fn what_parses_is_a_key(input in "(\\PC|[-/=?&# ]|[A-Za-z]{1,3}-[0-9]{1,3}){0,12}") {
                if let Some(k) = parse_key(&input) {
                    let (p, n) = k.split_once('-').unwrap();
                    prop_assert!(p.starts_with(|c: char| c.is_ascii_uppercase()), "{k}");
                    prop_assert!(p.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'), "{k}");
                    prop_assert!(!n.is_empty() && n.chars().all(|c| c.is_ascii_digit()), "{k}");
                    prop_assert_eq!(parse_key(&k), Some(k.clone()));
                }
            }
        }
    }
}
