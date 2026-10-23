//! Pull request status through the `gh` CLI, and ticket → worktree/branch discovery through git.

use crate::proc;
use crate::untrusted;
use crate::util::parse_ts;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

/// git here reads the local repository, except `ls-remote`; this only stops one stuck on a lock
/// or an unreachable remote.
const GIT_LIMIT: Duration = Duration::from_secs(30);
/// A gh call that takes longer gives up; the ticket's next refresh tries again.
const GH_LIMIT: Duration = Duration::from_secs(30);

fn git(dir: &Path, args: &[&str]) -> std::io::Result<Output> {
    crate::git::run(dir, args, GIT_LIMIT)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    Fail,
    Pending,
    Pass,
    Skipped,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    pub when: Option<i64>,
    /// Its run's page (`detailsUrl`, or a status's `targetUrl`); may be empty.
    pub url: String,
}

/// Declares an enum of GitHub's names for a state, with `parse` and `name` between them. `Other`
/// keeps a name it doesn't know, as GitHub adds states; it shows as GitHub's word.
macro_rules! states {
    ($(#[$doc:meta])* $ty:ident { $($v:ident = $name:literal,)* }) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum $ty {
            $($v,)*
            Other(String),
        }

        impl $ty {
            pub fn parse(s: &str) -> $ty {
                match s {
                    $($name => $ty::$v,)*
                    _ => $ty::Other(s.to_string()),
                }
            }

            /// GitHub's name for it, as `--json` prints it.
            pub fn name(&self) -> &str {
                match self {
                    $($ty::$v => $name,)*
                    $ty::Other(s) => s,
                }
            }
        }

        /// GitHub's name in words: `changes requested`.
        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(&untrusted::line(&self.name().to_lowercase().replace('_', " ")))
            }
        }
    };
}

states! {
    /// A PR's `state`. Whether it's a draft is a flag of its own (`Pr::is_draft`).
    PrState { Open = "OPEN", Closed = "CLOSED", Merged = "MERGED", }
}

states! {
    /// A review's `state`.
    ReviewState {
        Approved = "APPROVED",
        ChangesRequested = "CHANGES_REQUESTED",
        Commented = "COMMENTED",
        Dismissed = "DISMISSED",
        Pending = "PENDING",
    }
}

states! {
    /// A PR's `reviewDecision`: what its reviews add up to under the branch's rules. `Unset` when
    /// GitHub sends none, as when no review is required.
    Decision {
        Unset = "",
        Approved = "APPROVED",
        ChangesRequested = "CHANGES_REQUESTED",
        ReviewRequired = "REVIEW_REQUIRED",
    }
}

states! {
    /// A PR's `mergeable`: `Unknown` while GitHub works it out.
    Mergeable { Yes = "MERGEABLE", Conflicting = "CONFLICTING", Unknown = "UNKNOWN", }
}

impl ReviewState {
    /// Whether it's a decision that stands for its author: a comment doesn't replace one.
    fn decides(&self) -> bool {
        *self != ReviewState::Commented
    }

    pub fn glyph(&self) -> &'static str {
        match self {
            ReviewState::Approved => "✔",
            ReviewState::ChangesRequested => "✎",
            ReviewState::Dismissed => "–",
            ReviewState::Commented | ReviewState::Pending => "💬",
            ReviewState::Other(_) => "?",
        }
    }

    /// What its author did, after their name: `ana requested changes`.
    pub fn verb(&self) -> String {
        match self {
            ReviewState::Approved => "approved".into(),
            ReviewState::ChangesRequested => "requested changes".into(),
            ReviewState::Dismissed => "dismissed".into(),
            ReviewState::Commented | ReviewState::Pending => "commented".into(),
            ReviewState::Other(_) => self.to_string(),
        }
    }
}

impl Decision {
    /// None when there's no decision to show.
    pub fn glyph(&self) -> Option<&'static str> {
        match self {
            Decision::Unset => None,
            Decision::Approved => Some("✔"),
            Decision::ChangesRequested => Some("✎"),
            Decision::ReviewRequired => Some("◌"),
            Decision::Other(_) => Some("?"),
        }
    }

    /// The word on a sidebar row, which shows only a decision taken.
    pub fn short(&self) -> Option<&'static str> {
        match self {
            Decision::Approved => Some("appr"),
            Decision::ChangesRequested => Some("chg"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Review {
    pub id: String,
    pub author: String,
    pub state: ReviewState,
    pub when: String,
    pub body: String,
    pub url: String,
}

#[derive(Clone, Debug, Default)]
pub struct PrComment {
    pub id: String,
    pub author: String,
    pub when: String,
    pub body: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub is_draft: bool,
    pub review_decision: Decision,
    pub mergeable: Mergeable,
    pub head: String,
    /// Whether the head branch is in another repository, a fork: then whoever opened the PR
    /// named it, so a ticket doesn't take it as its own branch. True when GitHub doesn't say.
    pub cross_repository: bool,
    /// The head branch's last commit, which a merged PR vouches for when its worktree goes.
    pub head_oid: String,
    /// The branch it merges into: the default branch, or a stacked ticket's base.
    pub base: String,
    pub checks: Vec<Check>,
    /// Every review, oldest first.
    pub reviews: Vec<Review>,
    /// Every comment, newest first.
    pub comments: Vec<PrComment>,
    /// The inline review threads; Err when GitHub sent none.
    pub threads: Result<Threads, String>,
    /// What the PR list reads of it, as these details were fetched (`Wanted::shown`).
    pub stamp: Stamp,
}

/// What the PR list query reads of a PR to tell whether it changed since its details were
/// fetched: GitHub moves `updatedAt` for a comment, a review or a new state, the head commit shows
/// a push, and its check state the checks, which move neither. Mergeability moves when the base
/// does (a conflict after another PR merged), and the draft flag and review decision are read too,
/// as the sidebar shows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub updated: String,
    pub head: String,
    pub checks: String,
    pub mergeable: String,
    pub draft: bool,
    pub review: String,
}

impl Stamp {
    fn read(p: &Value) -> Stamp {
        let s = |x: &Value| x.as_str().unwrap_or("").to_string();
        Stamp {
            updated: s(&p["updatedAt"]),
            head: s(&p["headRefOid"]),
            checks: s(&p["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"]),
            mergeable: s(&p["mergeable"]),
            draft: p["isDraft"].as_bool().unwrap_or(false),
            review: s(&p["reviewDecision"]),
        }
    }
}

impl Pr {
    pub fn count(&self, s: CheckState) -> usize {
        self.checks.iter().filter(|c| c.state == s).count()
    }

    /// Each reviewer's standing review, in the order they first reviewed: their latest approval,
    /// change request or dismissal, else their first comment. A plain comment after an approval
    /// doesn't replace the approval, and the ids stay put while only comments arrive, so they
    /// don't count as new again.
    pub fn latest_reviews(&self) -> Vec<&Review> {
        let mut out: Vec<&Review> = Vec::new();
        for r in &self.reviews {
            match out.iter().position(|x| x.author == r.author) {
                Some(_) if !r.state.decides() => {}
                Some(i) => out[i] = r,
                None => out.push(r),
            }
        }
        out
    }

    /// Reviews with text that still stand, oldest first: none of their author's approvals,
    /// change requests or dismissals came after them.
    pub fn review_text(&self) -> Vec<&Review> {
        let decided = |author: &str, after: &str| {
            self.reviews
                .iter()
                .any(|r| r.author == author && r.state.decides() && r.when.as_str() > after)
        };
        self.reviews
            .iter()
            .filter(|r| !r.body.trim().is_empty() && !decided(&r.author, &r.when))
            .collect()
    }
}

/// An inline review thread: comments on a line of a file.
#[derive(Clone, Debug)]
pub struct Thread {
    pub path: String,
    /// The line in the PR's latest diff; for an outdated thread, the line it was left on.
    pub line: Option<u64>,
    pub resolved: bool,
    /// The code it's on has changed since.
    pub outdated: bool,
    /// The first comment and the newest replies, oldest first.
    pub comments: Vec<PrComment>,
    /// Replies between the first comment and the newest ones, not fetched.
    pub left_out: usize,
}

/// A PR's newest review threads.
#[derive(Clone, Debug, Default)]
pub struct Threads {
    /// Oldest first.
    pub list: Vec<Thread>,
    /// How many the PR has, fetched or not.
    pub total: usize,
}

impl Threads {
    /// Older threads that weren't fetched.
    pub fn left_out(&self) -> usize {
        self.total.saturating_sub(self.list.len())
    }

    pub fn unresolved(&self) -> impl Iterator<Item = &Thread> {
        self.list.iter().filter(|t| !t.resolved)
    }
}

/// The error for a repository that isn't on GitHub, which panels show as a note, not a failure.
pub const NO_REMOTE: &str = "This repo has no GitHub remote";

/// Whether a project's folder can show PRs: a git repository with a GitHub remote, and `gh`
/// installed and logged in to it. Saved as the last known answer (`store::UiPrefs::github`),
/// except `Unanswered`, which says nothing about the setup and so never replaces an answer
/// (`App::gh_checked`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gh {
    Ready,
    NotGit,
    NoRemote,
    Missing,
    LoggedOut,
    /// `gh auth token` didn't answer within `GH_LIMIT`: a locked keyring, a stalled proxy.
    Unanswered,
}

impl Gh {
    /// Why there's no PR panel, for Settings and toasts.
    pub fn why(self) -> &'static str {
        match self {
            Gh::Ready => "",
            Gh::NotGit => "the folder isn't a git repository",
            Gh::NoRemote => "the repository has no GitHub remote",
            Gh::Missing => "gh isn't installed; get it from https://cli.github.com",
            Gh::LoggedOut => "gh isn't logged in; run gh auth login",
            Gh::Unanswered => "gh didn't answer within 30s; check gh auth status",
        }
    }

    /// Whether this is worth saving as the last known answer: what gh said, not that it didn't.
    pub fn saved(self) -> bool {
        self != Gh::Unanswered
    }

    /// What a failed PR fetch says about the setup, when it's about the setup.
    pub fn from_error(e: &str) -> Option<Gh> {
        match e {
            NO_REMOTE => Some(Gh::NoRemote),
            e if e == Gh::NotGit.why() => Some(Gh::NotGit),
            e if e == Gh::Missing.why() => Some(Gh::Missing),
            e if e == Gh::LoggedOut.why() => Some(Gh::LoggedOut),
            _ => None,
        }
    }
}

/// Whether a failed PR fetch only means the folder isn't on GitHub, a note rather than a failure.
pub fn is_note(e: &str) -> bool {
    matches!(Gh::from_error(e), Some(Gh::NoRemote | Gh::NotGit))
}

/// Checks `folders` without the network: their remotes' hosts, then `gh auth token` for them,
/// which reads gh's own login. gh can take a second to answer, so each host is asked once however
/// many folders share it, and the hosts all at once.
pub fn probe(folders: &[PathBuf]) -> Vec<(PathBuf, Gh)> {
    let hosts: Vec<Option<Vec<String>>> = std::thread::scope(|s| {
        let handles: Vec<_> = folders.iter().map(|f| s.spawn(move || folder_hosts(f))).collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    let ready = readiness(&hosts, |asked| {
        std::thread::scope(|s| {
            let handles: Vec<_> = asked.iter().map(|h| s.spawn(move || token(h))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or(Token::No)).collect()
        })
    });
    folders.iter().cloned().zip(ready).collect()
}

/// What `gh auth token` said for a host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Yes,
    No,
    /// gh isn't installed.
    NoGh,
    /// gh didn't answer in time.
    TimedOut,
}

fn token(host: &str) -> Token {
    match proc::run(Command::new("gh").args(["auth", "token", "--hostname", host]), GH_LIMIT) {
        Ok(o) if o.status.success() => Token::Yes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Token::NoGh,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Token::TimedOut,
        _ => Token::No,
    }
}

/// The hosts of `folder`'s remotes; None if it isn't a git repository.
fn folder_hosts(folder: &Path) -> Option<Vec<String>> {
    let out = git(folder, &["remote", "-v"]).ok().filter(|o| o.status.success())?;
    Some(remote_hosts(&String::from_utf8_lossy(&out.stdout)))
}

/// Each folder's readiness from its remotes' hosts (None: not a git repository), with `ask` called
/// once, on every host any folder needs, each once, for their tokens in that order.
fn readiness(
    folders: &[Option<Vec<String>>],
    ask: impl FnOnce(&[String]) -> Vec<Token>,
) -> Vec<Gh> {
    let github = |hosts: &[String]| hosts.iter().any(|h| h.contains("github"));
    let mut asked: Vec<String> = Vec::new();
    for hosts in folders.iter().flatten() {
        let alias = github(hosts).then(|| "github.com".to_string());
        for h in hosts.iter().cloned().chain(alias) {
            if !asked.contains(&h) {
                asked.push(h);
            }
        }
    }
    let tokens = ask(&asked);
    let token = |h: &str| {
        let i = asked.iter().position(|a| a == h);
        i.and_then(|i| tokens.get(i).copied()).unwrap_or(Token::No)
    };
    let one = |hosts: &[String]| {
        let github = github(hosts);
        let mut timed_out = false;
        for h in hosts {
            match token(h) {
                Token::Yes => return Gh::Ready,
                Token::NoGh => return if github { Gh::Missing } else { Gh::NoRemote },
                Token::TimedOut => timed_out = true,
                Token::No => {}
            }
        }
        // An SSH alias like github.com-work is resolved by gh to github.com, which may be logged in.
        match (github, token("github.com")) {
            (true, Token::Yes) => Gh::Ready,
            (true, Token::TimedOut) => Gh::Unanswered,
            // Any host's: a GitHub Enterprise host looks like any other.
            _ if timed_out => Gh::Unanswered,
            (true, _) => Gh::LoggedOut,
            (false, _) => Gh::NoRemote,
        }
    };
    folders.iter().map(|f| f.as_deref().map_or(Gh::NotGit, one)).collect()
}

/// The hosts of `git remote -v`'s URLs, each once.
fn remote_hosts(text: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for h in text.lines().filter_map(|l| remote_host(l.split_whitespace().nth(1)?)) {
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
    hosts
}

/// `github.com` from `https://github.com/acme/web.git`, `git@github.com:acme/web.git` or
/// `ssh://git@github.com:22/acme/web`; None for a local path.
fn remote_host(url: &str) -> Option<String> {
    let authority = match url.split_once("://") {
        Some((_, rest)) => rest.split('/').next()?,
        None => url.split_once(':')?.0,
    };
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty() && !host.contains('/')).then(|| host.to_lowercase())
}

/// `acme/web` from the same URLs as `remote_host`; None unless the path is exactly owner/name,
/// as gh requires.
fn remote_repo(url: &str) -> Option<String> {
    remote_host(url)?;
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.split_once(':')?.1,
    };
    let path = path.trim_matches('/');
    let (owner, name) = path.strip_suffix(".git").unwrap_or(path).split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("{owner}/{name}"))
}

/// A git remote whose URL names a repository.
#[derive(Debug, PartialEq)]
struct Remote {
    name: String,
    host: String,
    repo: String,
}

/// The remotes in `git remote -v` that name a repository, each once, in git's order. Like gh, the
/// fetch URL counts, else the push URL.
fn remotes(text: &str) -> Vec<Remote> {
    let mut out: Vec<Remote> = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(url)) = (parts.next(), parts.next()) else { continue };
        if out.iter().any(|r| r.name == name) {
            continue;
        }
        if let (Some(host), Some(repo)) = (remote_host(url), remote_repo(url)) {
            out.push(Remote { name: name.into(), host, repo });
        }
    }
    out
}

/// The repository gh reads PRs from in a folder.
#[derive(Debug, PartialEq)]
pub struct DefaultRepo {
    /// `acme/web`.
    pub name: String,
    /// Its remote's host as gh resolves it: `github.com` for an SSH alias like `github.com-work`.
    pub host: String,
    /// Whether the GitHub remotes name more than one repository, so `gh repo set-default` matters.
    pub several: bool,
}

/// Which repository gh uses in `folder`, read from git alone: the remotes and the default
/// `gh repo set-default` keeps in git config. None when no remote names a repository.
pub fn default_repo(folder: &Path) -> Option<DefaultRepo> {
    let git = |args: &[&str]| git(folder, args).ok();
    let out = git(&["remote", "-v"]).filter(|o| o.status.success())?;
    // Exits 1 when nothing is set.
    let set = git(&["config", "--get-regexp", r"^remote\..*\.gh-resolved$"])
        .map(|o| o.stdout)
        .unwrap_or_default();
    pick_repo(&remotes(&String::from_utf8_lossy(&out.stdout)), &String::from_utf8_lossy(&set))
}

/// gh's choice among `remotes`, given `git config --get-regexp` lines like
/// `remote.origin.gh-resolved base` (`base` for that remote's repository, or an `owner/name`).
/// Without a default, gh run without a terminal (taskman's calls, the sessions') takes the first
/// remote named upstream, then github, then origin, then any other in git's order.
fn pick_repo(remotes: &[Remote], resolved: &str) -> Option<DefaultRepo> {
    // GitHub remotes only, when the hosts say; otherwise (GitHub Enterprise) all of them.
    let github: Vec<&Remote> = remotes.iter().filter(|r| r.host.contains("github")).collect();
    let mut pool = if github.is_empty() { remotes.iter().collect() } else { github };
    let score = |name: &str| match name.to_lowercase().as_str() {
        "upstream" => 0,
        "github" => 1,
        "origin" => 2,
        _ => 3,
    };
    pool.sort_by_key(|r| score(&r.name));
    let mut repos: Vec<&str> = pool.iter().map(|r| r.repo.as_str()).collect();
    repos.sort();
    repos.dedup();
    let several = repos.len() > 1;
    let set = |r: &Remote| {
        resolved.lines().find_map(|l| {
            let (key, value) = l.split_once(char::is_whitespace)?;
            let name = key.strip_prefix("remote.")?.strip_suffix(".gh-resolved")?;
            (name == r.name && !value.trim().is_empty()).then(|| value.trim().to_string())
        })
    };
    let (remote, name) = match pool.iter().find_map(|r| Some((r, set(r)?))) {
        Some((r, v)) if v == "base" => (r, r.repo.clone()),
        Some((r, v)) => (r, v),
        None => (pool.first()?, pool.first()?.repo.clone()),
    };
    let host = match remote.host.as_str() {
        h if h.starts_with("github.com") => "github.com".to_string(),
        h => h.to_string(),
    };
    Some(DefaultRepo { name, host, several })
}

pub(crate) fn gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out =
        proc::run(Command::new("gh").args(args).current_dir(dir).env("NO_COLOR", "1"), GH_LIMIT)
            .map_err(|e| match e.kind() {
                // Its folder, moved or deleted, isn't found either: not a reason to say gh is
                // missing, which sticks until taskman restarts.
                std::io::ErrorKind::NotFound if !dir.is_dir() => {
                    format!("{} is gone", crate::store::tilde(dir))
                }
                std::io::ErrorKind::NotFound => Gh::Missing.why().to_string(),
                std::io::ErrorKind::TimedOut => {
                    format!("gh {}: {e}; the next refresh tries again", args[..2].join(" "))
                }
                _ => format!("could not run gh: {e}"),
            })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(gh_error(String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// What gh's error output says: the setup's problem in taskman's words when it's about the setup,
/// else as gh put it.
fn gh_error(err: &str) -> String {
    if err.contains("no git remotes") || err.contains("none of the git remotes") {
        return NO_REMOTE.into();
    }
    if err.contains("gh auth login") {
        return Gh::LoggedOut.why().into();
    }
    err.to_string()
}

/// What the PR lookup found for a ticket.
pub struct Lookup {
    /// The PR to show: open if there is one, else merged.
    pub pr: Option<Pr>,
    /// When there's nothing to show, the newest PR closed without merging, for a note.
    pub closed: Option<u64>,
    /// The remembered branch exists neither locally nor on the remote and has no open PR.
    pub branch_gone: bool,
}

struct Candidate {
    number: u64,
    url: String,
    state: PrState,
    stamp: Stamp,
    /// On the ticket's own branch, rather than found by searching for its key.
    exact: bool,
}

/// A ticket whose PR to find.
#[derive(Clone, Debug, Default)]
pub struct Wanted {
    pub key: String,
    /// Its worktree's branch, else the one remembered.
    pub branch: Option<String>,
    pub closes: Option<Closes>,
    /// The PR the ticket shows: kept, rather than fetched again, when the list finds it unchanged
    /// (`unchanged`). None fetches the details of whichever PR is found.
    pub shown: Option<Pr>,
}

/// What the PR list query found for one ticket: the PRs on its branch, and those that mention its
/// key.
struct Listed {
    branch: Vec<Candidate>,
    search: Vec<Candidate>,
}

/// Tickets asked about in one query. GitHub runs each search on its own, and a query that takes
/// over 10 s fails as a whole.
const BATCH: usize = 20;

/// What each PR found carries; `candidates` reads it, and `Stamp::read` what tells whether it
/// changed.
const LISTED: &str = "fragment pr on PullRequest { number title url headRefName state updatedAt \
    headRefOid mergeable isDraft reviewDecision \
    commits(last: 1) { nodes { commit { statusCheckRollup { state } } } } }";

/// The PR list query for `asked` and its gh arguments: per ticket, the PRs on its branch (`bN`,
/// as `gh pr list --head`) and the search for its key (`qN`, as `gh pr list --search`). The
/// repository is gh's choice, as for `gh pr view`: gh fills in `{owner}` and `{repo}` in `-F`
/// values. Branches go as `-f`, plain text: `-F` would read a file for one starting with `@`, so
/// the search starts with its qualifiers, not the key. `ISSUE` searches work on GitHub Enterprise
/// Server too, and for one word find what gh's `ISSUE_ADVANCED` does.
fn list_args(host: Option<&str>, asked: &[&Wanted]) -> Vec<String> {
    let mut args: Vec<String> = vec!["api".into(), "graphql".into()];
    if let Some(h) = host {
        args.extend(["--hostname".into(), h.into()]);
    }
    let (mut vars, mut branches, mut searches) = (Vec::new(), String::new(), String::new());
    let branched = asked.iter().any(|w| w.branch.is_some());
    if branched {
        vars.extend(["$owner: String!".to_string(), "$name: String!".into()]);
        args.extend(["-F", "owner={owner}", "-F", "name={repo}"].map(String::from));
    }
    for (i, w) in asked.iter().enumerate() {
        if let Some(b) = &w.branch {
            vars.push(format!("$b{i}: String!"));
            args.extend(["-f".into(), format!("b{i}={b}")]);
            branches.push_str(&format!(
                "    b{i}: pullRequests(headRefName: $b{i}, states: [OPEN, CLOSED, MERGED], \
                 first: 10, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ nodes {{ ...pr }} }}\n"
            ));
        }
        vars.push(format!("$q{i}: String!"));
        args.extend(["-F".into(), format!("q{i}=repo:{{owner}}/{{repo}} type:pr {}", w.key)]);
        searches.push_str(&format!(
            "  q{i}: search(query: $q{i}, type: ISSUE, first: 20) {{ nodes {{ ...pr }} }}\n"
        ));
    }
    let repository = match branched {
        true => format!("  repository(owner: $owner, name: $name) {{\n{branches}  }}\n"),
        false => String::new(),
    };
    let query = format!("query({}) {{\n{repository}{searches}}}\n{LISTED}", vars.join(", "));
    args.extend(["-f".into(), format!("query={query}")]);
    args
}

/// Reads the PR list query's reply to `list_args(_, asked)`, a `Listed` per ticket in order.
fn parse_listed(json: &str, asked: &[&Wanted]) -> Result<Vec<Listed>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let data = &v["data"];
    if !data.is_object() {
        return Err("GitHub returned no PRs".into());
    }
    Ok((0..asked.len())
        .map(|i| Listed {
            branch: candidates(&data["repository"][format!("b{i}")]["nodes"], None),
            search: candidates(&data[format!("q{i}")]["nodes"], Some(&asked[i].key)),
        })
        .collect())
}

/// Reads a list of PRs; with a key, keeps only PRs whose branch or title mentions it as a whole
/// (see `mentions`), so `ENG-5` doesn't take `ENG-52`'s PR. Without one, they're the branch's own.
fn candidates(nodes: &Value, key: Option<&str>) -> Vec<Candidate> {
    let s = |p: &Value, k: &str| p[k].as_str().unwrap_or("").to_string();
    nodes
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| {
            key.is_none_or(|k| mentions(&s(p, "headRefName"), k) || mentions(&s(p, "title"), k))
        })
        .filter_map(|p| candidate(p, key.is_none()))
        .collect()
}

fn candidate(p: &Value, exact: bool) -> Option<Candidate> {
    let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
    Some(Candidate {
        number: p["number"].as_u64()?,
        url: s("url"),
        state: PrState::parse(&s("state")),
        stamp: Stamp::read(p),
        exact,
    })
}

/// The PR to show (newest open, else newest merged; the branch's own before those that only
/// mention the key) and the newest closed-unmerged one.
fn pick<'a>(c: impl IntoIterator<Item = &'a Candidate>) -> (Option<&'a Candidate>, Option<u64>) {
    let c: Vec<&Candidate> = c.into_iter().collect();
    let newest = |state: PrState| {
        c.iter()
            .filter(|x| x.state == state)
            .max_by(|a, b| (a.exact, &a.stamp.updated).cmp(&(b.exact, &b.stamp.updated)))
            .copied()
    };
    let shown = newest(PrState::Open).or_else(|| newest(PrState::Merged));
    (shown, newest(PrState::Closed).map(|x| x.number))
}

/// The issue a ticket's PR closes, when the project's tracker is its repository's issues
/// (`Tracker::closes`).
#[derive(Clone, Debug)]
pub struct Closes {
    pub repo: crate::github_issues::Repo,
    pub number: u64,
}

const CLOSING_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      closedByPullRequestsReferences(first: 20, includeClosedPrs: true) {
        nodes { ...pr repository { nameWithOwner } }
      }
    }
  }
}";

/// The PRs GitHub links to the issue as closing it, in the issue's own repository: a PR from
/// another one can't be viewed by its number there.
fn closing(
    gh: &impl Fn(&[&str]) -> Result<String, String>,
    c: &Closes,
) -> Result<Vec<Candidate>, String> {
    let args = [
        "api".to_string(),
        "graphql".into(),
        "--hostname".into(),
        c.repo.host.clone(),
        "-f".into(),
        format!("owner={}", c.repo.owner),
        "-f".into(),
        format!("name={}", c.repo.name),
        "-F".into(),
        format!("number={}", c.number),
        "-f".into(),
        format!("query={CLOSING_QUERY}\n{LISTED}"),
    ];
    let json = gh(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(parse_closing(&json, &format!("{}/{}", c.repo.owner, c.repo.name)))
}

fn parse_closing(json: &str, name: &str) -> Vec<Candidate> {
    let v: Value = serde_json::from_str(json).unwrap_or_default();
    v["data"]["repository"]["issue"]["closedByPullRequestsReferences"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| {
            let repo = p["repository"]["nameWithOwner"].as_str().unwrap_or("");
            repo.eq_ignore_ascii_case(name)
        })
        .filter_map(|p| candidate(p, true))
        .collect()
}

/// Finds the ticket's PR (see `fetch_all`).
pub fn fetch(
    repo: &Path,
    branch: Option<&str>,
    key: &str,
    closes: Option<&Closes>,
    gh: impl Fn(&[&str]) -> Result<String, String> + Sync,
) -> Result<Lookup, String> {
    let wanted = Wanted {
        key: key.into(),
        branch: branch.map(String::from),
        closes: closes.cloned(),
        shown: None,
    };
    let mut found = fetch_all(repo, std::slice::from_ref(&wanted), gh);
    found.pop().unwrap_or_else(|| Err("no lookup ran".into()))
}

/// Finds each ticket's PR, in order: one GitHub links as closing its issue (`closes`) when
/// there's an open or merged one, else among those on its branch and those mentioning its key,
/// listed for all of them in one query (`BATCH` at a time). A PR closed without merging isn't
/// shown, so abandoned work doesn't look live. The PR found is fetched in full, with its review
/// threads, unless it's the one the ticket shows and hasn't changed (`Wanted::shown`). `gh` runs
/// gh with the given arguments in `repo`
/// (`|args| github::gh(repo, args)`); git reads `repo` itself.
pub fn fetch_all(
    repo: &Path,
    wanted: &[Wanted],
    gh: impl Fn(&[&str]) -> Result<String, String> + Sync,
) -> Vec<Result<Lookup, String>> {
    let mut out: Vec<Option<Result<Lookup, String>>> = wanted.iter().map(|_| None).collect();
    let closes: Vec<usize> = (0..wanted.len()).filter(|&i| wanted[i].closes.is_some()).collect();
    let found =
        each(&closes, |&i| wanted[i].closes.as_ref().and_then(|c| linked(&gh, &wanted[i], c)));
    for (&i, found) in closes.iter().zip(found) {
        out[i] = found;
    }
    let rest: Vec<usize> = (0..wanted.len()).filter(|&i| out[i].is_none()).collect();
    // gh picks the repository, but `gh api` takes its host from `--hostname`, not the remote.
    let host = match rest.is_empty() {
        true => None,
        false => default_repo(repo).map(|d| d.host),
    };
    for chunk in rest.chunks(BATCH) {
        let asked: Vec<&Wanted> = chunk.iter().map(|&i| &wanted[i]).collect();
        let args = list_args(host.as_deref(), &asked);
        let listed = gh(&args.iter().map(String::as_str).collect::<Vec<_>>())
            .and_then(|json| parse_listed(&json, &asked));
        let tickets: Vec<(usize, usize)> = chunk.iter().copied().enumerate().collect();
        let found = each(&tickets, |&(n, i)| {
            Some(match &listed {
                Ok(l) => settle(repo, &gh, &wanted[i], &l[n]),
                Err(e) => Err(e.clone()),
            })
        });
        for (&(_, i), found) in tickets.iter().zip(found) {
            out[i] = found;
        }
    }
    out.into_iter().map(|r| r.unwrap_or_else(|| Err("the PR lookup crashed".into()))).collect()
}

/// `f` on each item, on threads of their own: each fetching a PR takes gh a second or two. None
/// for one whose thread panicked.
fn each<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> Option<R> + Sync) -> Vec<Option<R>> {
    std::thread::scope(|s| {
        let handles: Vec<_> = items.iter().map(|x| s.spawn(|| f(x))).collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    })
}

/// The open or merged PR GitHub links as closing the issue, viewed in the issue's repository.
/// None when there's none, or the query failed: that only means falling back to the list, whose
/// call says what's wrong.
fn linked(
    gh: &(impl Fn(&[&str]) -> Result<String, String> + Sync),
    w: &Wanted,
    c: &Closes,
) -> Option<Result<Lookup, String>> {
    let found = closing(gh, c).ok()?;
    let pr = details(gh, pick(&found).0?, w.shown.as_ref());
    Some(pr.map(|pr| Lookup { pr: Some(pr), closed: None, branch_gone: false }))
}

/// The ticket's PR among those listed for it: the PRs on its branch, and when none of them is
/// open, those mentioning its key too.
fn settle(
    repo: &Path,
    gh: &impl Fn(&[&str]) -> Result<String, String>,
    w: &Wanted,
    listed: &Listed,
) -> Result<Lookup, String> {
    let mut found: Vec<&Candidate> = listed.branch.iter().collect();
    let open_on_branch = found.iter().any(|c| c.state == PrState::Open);
    if !open_on_branch {
        for c in &listed.search {
            if !found.iter().any(|x| x.number == c.number) {
                found.push(c);
            }
        }
    }
    let (best, closed) = pick(found);
    let branch_gone = match &w.branch {
        Some(b) if !open_on_branch => !branch_exists(repo, b),
        _ => false,
    };
    let pr = match best {
        Some(c) => Some(details(gh, c, w.shown.as_ref())?),
        None => None,
    };
    Ok(Lookup { pr, closed: if best.is_none() { closed } else { None }, branch_gone })
}

/// The PR found, in full: the one shown when it hasn't changed, else fetched.
fn details(
    gh: &impl Fn(&[&str]) -> Result<String, String>,
    c: &Candidate,
    shown: Option<&Pr>,
) -> Result<Pr, String> {
    match shown {
        Some(pr) if unchanged(pr, c) => Ok(pr.clone()),
        _ => view(gh, c.number, &c.url),
    }
}

/// Whether the PR found is the one shown, as it was when its details were fetched. One whose
/// threads failed is fetched again, and so is one GitHub didn't stamp, which can't be compared.
fn unchanged(shown: &Pr, c: &Candidate) -> bool {
    shown.url == c.url
        && !c.stamp.updated.is_empty()
        && shown.stamp == c.stamp
        && shown.threads.is_ok()
}

/// Review threads fetched: the newest ones, and in each the first comment and the newest replies.
const THREADS: usize = 100;
const REPLIES: usize = 20;
/// Reviews and comments come a page at a time, newest first, until there are no more.
const PAGE: usize = 100;
/// Pages of reviews or comments fetched at most past the first, in case a cursor never ends.
const PAGES: usize = 50;

const REVIEW: &str = "id author { login } state submittedAt body url";
const COMMENT: &str = "id author { login } body createdAt url";

/// The PR in full, with its review threads: what `gh pr view --json` gave, and the threads, which
/// it doesn't have, in one query. A status's `createdAt` is its `startedAt`, as gh calls it.
fn details_query() -> String {
    format!(
        "query($owner: String!, $name: String!, $number: Int!, $page: Int!, $threads: Int!, \
         $replies: Int!) {{
  repository(owner: $owner, name: $name) {{
    pullRequest(number: $number) {{
      number title url state isDraft reviewDecision mergeable updatedAt
      headRefName headRefOid baseRefName isCrossRepository
      commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{
        state
        contexts(first: 100) {{ nodes {{
          __typename
          ... on CheckRun {{
            name status conclusion startedAt completedAt detailsUrl
            checkSuite {{ workflowRun {{ workflow {{ name }} }} }}
          }}
          ... on StatusContext {{ context state targetUrl startedAt: createdAt }}
        }} }}
      }} }} }} }}
      reviews(last: $page) {{ pageInfo {{ hasPreviousPage startCursor }} nodes {{ {REVIEW} }} }}
      comments(last: $page) {{ pageInfo {{ hasPreviousPage startCursor }} nodes {{ {COMMENT} }} }}
      reviewThreads(last: $threads) {{
        totalCount
        nodes {{
          path line originalLine isResolved isOutdated
          first: comments(first: 1) {{ totalCount nodes {{ {COMMENT} }} }}
          newest: comments(last: $replies) {{ nodes {{ {COMMENT} }} }}
        }}
      }}
    }}
  }}
}}"
    )
}

/// The page of reviews or comments (`what`, with `fields`) before the cursor `$before`.
fn older_query(what: &str, fields: &str) -> String {
    format!(
        "query($owner: String!, $name: String!, $number: Int!, $page: Int!, $before: String!) {{
  repository(owner: $owner, name: $name) {{
    pullRequest(number: $number) {{
      {what}(last: $page, before: $before) {{
        pageInfo {{ hasPreviousPage startCursor }} nodes {{ {fields} }}
      }}
    }}
  }}
}}"
    )
}

/// PR `n` in full, with its review threads, in the repository its `url` names: one query, and
/// one more for each hundred reviews or comments past the newest hundred.
fn view(gh: &impl Fn(&[&str]) -> Result<String, String>, n: u64, url: &str) -> Result<Pr, String> {
    let (host, owner, name) =
        pr_repo(url).ok_or_else(|| format!("can't tell the repository from {url}"))?;
    // -f passes text as it is; -F reads numbers (and would read a file for a value starting
    // with @).
    let ask = |vars: &[(&str, String)], query: &str| {
        let mut args = vec![
            "api".to_string(),
            "graphql".into(),
            "--hostname".into(),
            host.clone(),
            "-f".into(),
            format!("owner={owner}"),
            "-f".into(),
            format!("name={name}"),
            "-F".into(),
            format!("number={n}"),
            "-F".into(),
            format!("page={PAGE}"),
        ];
        for (flag, var) in vars {
            args.extend([flag.to_string(), var.clone()]);
        }
        args.extend(["-f".into(), format!("query={query}")]);
        let json = gh(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
        serde_json::from_str::<Value>(&json).map_err(|e| e.to_string())
    };
    let counts = [("-F", format!("threads={THREADS}")), ("-F", format!("replies={REPLIES}"))];
    let mut v = ask(&counts, &details_query())?;
    for (what, fields) in [("reviews", REVIEW), ("comments", COMMENT)] {
        let query = older_query(what, fields);
        for _ in 0..PAGES {
            let got = &v["data"]["repository"]["pullRequest"][what];
            let Some(before) = got["pageInfo"]["startCursor"].as_str() else { break };
            if got["pageInfo"]["hasPreviousPage"] != true {
                break;
            }
            let older = ask(&[("-f", format!("before={before}"))], &query)?;
            let page = &older["data"]["repository"]["pullRequest"][what];
            let mut nodes = page["nodes"].as_array().cloned().unwrap_or_default();
            nodes.extend(got["nodes"].as_array().cloned().unwrap_or_default());
            let got = &mut v["data"]["repository"]["pullRequest"][what];
            got["pageInfo"] = page["pageInfo"].clone();
            got["nodes"] = Value::Array(nodes);
        }
    }
    parse(&v)
}

/// `("github.com", "acme", "web")` from `https://github.com/acme/web/pull/412`.
fn pr_repo(url: &str) -> Option<(String, String, String)> {
    let mut parts = url.strip_prefix("https://")?.split('/');
    let (host, owner, name) = (parts.next()?, parts.next()?, parts.next()?);
    (parts.next() == Some("pull") && ![host, owner, name].contains(&""))
        .then(|| (host.to_string(), owner.to_string(), name.to_string()))
}

/// The PR's review threads, from its details (`details_query`).
fn parse_threads(pr: &Value) -> Result<Threads, String> {
    let count = |x: &Value| x.as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
    let t = &pr["reviewThreads"];
    if t.is_null() {
        return Err("GitHub returned no review threads".into());
    }
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();
    let list = t["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| {
            let mut comments: Vec<PrComment> = Vec::new();
            for c in n["first"]["nodes"]
                .as_array()
                .into_iter()
                .chain(n["newest"]["nodes"].as_array())
                .flatten()
            {
                // The first comment is also among the newest when the thread is short.
                if comments.iter().all(|x| x.id != s(&c["id"])) {
                    comments.push(PrComment {
                        id: s(&c["id"]),
                        author: s(&c["author"]["login"]),
                        when: s(&c["createdAt"]),
                        body: s(&c["body"]),
                        url: s(&c["url"]),
                    });
                }
            }
            let total = count(&n["first"]["totalCount"]);
            Thread {
                path: s(&n["path"]),
                line: n["line"].as_u64().or(n["originalLine"].as_u64()),
                resolved: n["isResolved"].as_bool().unwrap_or(false),
                outdated: n["isOutdated"].as_bool().unwrap_or(false),
                left_out: total.saturating_sub(comments.len()),
                comments,
            }
        })
        .collect::<Vec<_>>();
    let total = count(&t["totalCount"]).max(list.len());
    Ok(Threads { list, total })
}

/// Whether the branch exists locally or on origin. Unsure (say, offline) counts as existing.
fn branch_exists(repo: &Path, branch: &str) -> bool {
    let git = |args: &[&str]| git(repo, args);
    if git(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .is_ok_and(|o| o.status.success())
    {
        return true;
    }
    match git(&["ls-remote", "--exit-code", "--heads", "origin", branch]) {
        Ok(o) => o.status.code() != Some(2), // 2: the remote answered and has no such branch
        Err(_) => true,
    }
}

/// Reads the details query's reply (`view`).
fn parse(reply: &Value) -> Result<Pr, String> {
    let v = &reply["data"]["repository"]["pullRequest"];
    if !v.is_object() {
        return Err("GitHub returned no PR".into());
    }
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();
    let rollup = &v["commits"]["nodes"][0]["commit"]["statusCheckRollup"];

    let mut checks: Vec<Check> = rollup["contexts"]["nodes"]
        .as_array()
        .map(|items| items.iter().map(parse_check).collect())
        .unwrap_or_default();
    checks.sort_by(|a, b| a.state.cmp(&b.state).then(a.name.cmp(&b.name)));

    let reviews: Vec<Review> = v["reviews"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Review {
            id: s(&r["id"]),
            author: s(&r["author"]["login"]),
            state: ReviewState::parse(r["state"].as_str().unwrap_or("")),
            when: s(&r["submittedAt"]),
            body: s(&r["body"]),
            url: s(&r["url"]),
        })
        .collect();

    let mut comments: Vec<PrComment> = v["comments"]["nodes"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .map(|c| PrComment {
                    id: s(&c["id"]),
                    author: s(&c["author"]["login"]),
                    when: s(&c["createdAt"]),
                    body: s(&c["body"]),
                    url: s(&c["url"]),
                })
                .collect()
        })
        .unwrap_or_default();
    comments.reverse();

    Ok(Pr {
        number: v["number"].as_u64().unwrap_or(0),
        title: s(&v["title"]),
        url: s(&v["url"]),
        state: PrState::parse(v["state"].as_str().unwrap_or("")),
        is_draft: v["isDraft"].as_bool().unwrap_or(false),
        review_decision: Decision::parse(v["reviewDecision"].as_str().unwrap_or("")),
        mergeable: Mergeable::parse(v["mergeable"].as_str().unwrap_or("")),
        head: s(&v["headRefName"]),
        cross_repository: v["isCrossRepository"].as_bool() != Some(false),
        head_oid: s(&v["headRefOid"]),
        base: s(&v["baseRefName"]),
        checks,
        reviews,
        comments,
        threads: parse_threads(v),
        stamp: Stamp::read(v),
    })
}

fn parse_check(c: &Value) -> Check {
    let s = |k: &str| c[k].as_str().unwrap_or("");
    if c["__typename"] == "StatusContext" {
        let state = match s("state") {
            "SUCCESS" => CheckState::Pass,
            "PENDING" | "EXPECTED" => CheckState::Pending,
            _ => CheckState::Fail,
        };
        let (name, url) = (s("context").to_string(), s("targetUrl").to_string());
        return Check { name, state, when: parse_ts(s("startedAt")), url };
    }
    let state = if s("status") != "COMPLETED" {
        CheckState::Pending
    } else {
        match s("conclusion") {
            "SUCCESS" => CheckState::Pass,
            "SKIPPED" | "NEUTRAL" => CheckState::Skipped,
            _ => CheckState::Fail,
        }
    };
    let workflow = c["checkSuite"]["workflowRun"]["workflow"]["name"].as_str().unwrap_or("");
    let name = match (workflow, s("name")) {
        ("", n) => n.to_string(),
        (w, n) if w == n => n.to_string(),
        (w, n) => format!("{w} / {n}"),
    };
    let when = parse_ts(s("completedAt")).or_else(|| parse_ts(s("startedAt")));
    Check { name, state, when, url: s("detailsUrl").to_string() }
}

/// A repository's worktrees, the main checkout first, from any path inside it.
pub type Worktrees = Vec<Worktree>;

#[derive(Debug, Clone, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    /// None when detached.
    pub branch: Option<String>,
    /// Why it's locked (empty when no reason was given); None when it isn't.
    pub locked: Option<String>,
}

impl Worktree {
    /// A `git worktree add` that never finished, killed or timed out mid-checkout: git locks the
    /// worktree as "initializing" until its files are checked out, and one left that way has
    /// every file staged for deletion. Never a ticket's to work in.
    pub fn unfinished(&self) -> bool {
        self.locked.as_deref() == Some("initializing")
    }
}

pub fn worktrees(path: &Path) -> Option<Worktrees> {
    let out = git(path, &["worktree", "list", "--porcelain"]).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_worktrees(&String::from_utf8_lossy(&out.stdout)))
}

pub(crate) fn parse_worktrees(text: &str) -> Worktrees {
    let mut out: Worktrees = Vec::new();
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            out.push(Worktree { path: PathBuf::from(p), branch: None, locked: None });
        } else if let Some(last) = out.last_mut() {
            if let Some(b) = line.strip_prefix("branch refs/heads/") {
                last.branch = Some(b.to_string());
            } else if line == "locked" {
                last.locked = Some(String::new());
            } else if let Some(why) = line.strip_prefix("locked ") {
                last.locked = Some(why.to_string());
            }
        }
    }
    out
}

/// Resolves any path inside a repository (including a worktree) to its main checkout.
pub fn main_checkout(path: &Path) -> Option<PathBuf> {
    worktrees(path)?.into_iter().next().map(|w| w.path)
}

/// Finds the worktree whose branch belongs to the ticket: one ending in `/<KEY>`, as workflows that
/// create their own worktrees name them (`change/ENG-52`), or any branch containing the key.
/// Unfinished ones are left out: `worktree::create` makes them again.
pub fn find_worktree(repo: &Path, key: &str) -> Option<(PathBuf, String)> {
    pick_worktree(&worktrees(repo)?, key)
}

pub fn pick_worktree(list: &[Worktree], key: &str) -> Option<(PathBuf, String)> {
    let mut best = None;
    for w in list.iter().filter(|w| !w.unfinished()) {
        let Some(b) = &w.branch else { continue };
        match names_ticket(b, key) {
            Some(true) => return Some((w.path.clone(), b.clone())),
            Some(false) => _ = best.get_or_insert((w.path.clone(), b.clone())),
            None => {}
        }
    }
    best
}

/// Whether the branch names the ticket, ignoring case: `Some(true)` when it ends in `/<KEY>` or is
/// the key, `Some(false)` when it only `mentions` it.
fn names_ticket(branch: &str, key: &str) -> Option<bool> {
    if branch.rsplit('/').next().is_some_and(|last| last.eq_ignore_ascii_case(key)) {
        return Some(true);
    }
    mentions(branch, key).then_some(false)
}

/// Whether `text` (a branch or a PR title) has the key as a whole, ignoring case: not after a
/// letter or digit, and not before a digit, so `ENG-5` matches `eng-5-fix` and `[ENG-5]` but not
/// `ENG-52`.
fn mentions(text: &str, key: &str) -> bool {
    let (t, k) = (text.to_uppercase(), key.to_uppercase());
    let whole = |i: usize| {
        let before = t[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after = t[i + k.len()..].chars().next().is_none_or(|c| !c.is_ascii_digit());
        before && after
    };
    !k.is_empty() && t.match_indices(&k).any(|(i, _)| whole(i))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn a_folder_thats_gone_isnt_gh_missing() {
        // gh never runs: the folder fails before it would start, installed or not.
        let gone = crate::store::state_dir().join("acme-moved");
        let e = gh(&gone, &["pr", "list"]).unwrap_err();
        assert!(e.ends_with("acme-moved is gone"), "{e}");
        assert_eq!(Gh::from_error(&e), None);
    }

    /// `readiness` with gh answering from `tokens`, by host; returns what it found and the hosts
    /// gh was asked about.
    fn ready_with(folders: &[Option<&[&str]>], tokens: &[(&str, Token)]) -> (Vec<Gh>, Vec<String>) {
        let folders: Vec<Option<Vec<String>>> =
            folders.iter().map(|f| f.map(|h| h.iter().map(|h| h.to_string()).collect())).collect();
        let mut asked = Vec::new();
        let ready = readiness(&folders, |hosts| {
            asked = hosts.to_vec();
            let answer = |h: &String| tokens.iter().find(|(t, _)| t == h).map(|(_, a)| *a);
            hosts.iter().map(|h| answer(h).unwrap_or(Token::No)).collect()
        });
        (ready, asked)
    }

    #[test]
    fn asks_gh_once_per_host() {
        let github: &[&str] = &["github.com"];
        let (ready, asked) = ready_with(
            &[Some(github), Some(github), Some(&["github.com", "gitlab.example.com"]), None],
            &[("github.com", Token::Yes)],
        );
        assert_eq!(ready, [Gh::Ready, Gh::Ready, Gh::Ready, Gh::NotGit]);
        assert_eq!(asked, ["github.com", "gitlab.example.com"]);
        let (_, asked) = ready_with(&[None, Some(&[])], &[]);
        assert!(asked.is_empty(), "{asked:?}");
    }

    #[test]
    fn tells_why_a_folder_has_no_pr_panel() {
        let alias: &[&str] = &["github.com-work"];
        let (ready, asked) = ready_with(&[Some(alias)], &[("github.com", Token::Yes)]);
        assert_eq!(
            (ready, asked),
            (vec![Gh::Ready], vec!["github.com-work".into(), "github.com".into()])
        );
        assert_eq!(ready_with(&[Some(alias)], &[]).0, [Gh::LoggedOut]);
        let gh = [("github.com", Token::NoGh), ("gitlab.example.com", Token::NoGh)];
        let (ready, _) = ready_with(&[Some(&["github.com"]), Some(&["gitlab.example.com"])], &gh);
        assert_eq!(ready, [Gh::Missing, Gh::NoRemote]);
        // gh logged in to a GitHub Enterprise host.
        let enterprise = [("git.example.com", Token::Yes)];
        assert_eq!(
            ready_with(&[Some(&["git.example.com"]), Some(&[])], &enterprise).0,
            [Gh::Ready, Gh::NoRemote]
        );
    }

    /// A gh that hangs (a locked keyring, a stalled proxy) is stopped at the limit, and said to
    /// have not answered rather than to be logged out, which would send the user to the wrong fix.
    #[test]
    fn a_gh_that_doesnt_answer_is_not_called_logged_out() {
        let timed_out = [("github.com", Token::TimedOut)];
        assert_eq!(ready_with(&[Some(&["github.com"])], &timed_out).0, [Gh::Unanswered]);
        // Through an SSH alias, either host may be the one that hung.
        let alias: &[&str] = &["github.com-work"];
        assert_eq!(ready_with(&[Some(alias)], &timed_out).0, [Gh::Unanswered]);
        let hung_alias = [("github.com-work", Token::TimedOut)];
        assert_eq!(ready_with(&[Some(alias)], &hung_alias).0, [Gh::Unanswered]);
        // Another host's answer still counts.
        let either = [("github.com", Token::TimedOut), ("git.example.com", Token::Yes)];
        assert_eq!(ready_with(&[Some(&["github.com", "git.example.com"])], &either).0, [Gh::Ready]);
        // A GitHub Enterprise host can't be told from any other by its name, so a host gh didn't
        // answer for may be one, and isn't saved as having no GitHub remote. One gh answered No
        // for still has none.
        let enterprise = [("git.example.com", Token::TimedOut)];
        let ready =
            ready_with(&[Some(&["git.example.com"]), Some(&["gitlab.example.com"])], &enterprise);
        assert_eq!(ready.0, [Gh::Unanswered, Gh::NoRemote]);
        assert!(!ready.0[0].saved());
        let mixed = [("git.example.com", Token::TimedOut), ("gitlab.example.com", Token::No)];
        let both: &[&str] = &["gitlab.example.com", "git.example.com"];
        assert_eq!(ready_with(&[Some(both)], &mixed).0, [Gh::Unanswered]);
        assert!(Gh::Unanswered.why().contains(&format!("within {}s", GH_LIMIT.as_secs())));
        assert!(!Gh::Unanswered.saved() && Gh::LoggedOut.saved() && Gh::Ready.saved());
    }

    #[test]
    fn reads_remote_hosts() {
        let text = "origin\thttps://github.com/acme/web.git (fetch)\norigin\thttps://github.com/acme/web.git (push)\n\
                    work\tgit@github.com-work:acme/web.git (fetch)\nmirror\tssh://git@GitLab.example.com:2222/acme/web (fetch)\n\
                    local\t/srv/git/web.git (fetch)\nfile\tfile:///srv/git/web.git (fetch)\n";
        assert_eq!(remote_hosts(text), ["github.com", "github.com-work", "gitlab.example.com"]);
        assert_eq!(
            remote_host("https://user@github.example.com/acme/web"),
            Some("github.example.com".into())
        );
        assert_eq!(remote_host("../web"), None);
    }

    #[test]
    fn reads_owner_and_name_from_remote_urls() {
        for url in [
            "https://github.com/acme/web.git",
            "https://github.com/acme/web",
            "https://user@github.com/acme/web/",
            "git@github.com:acme/web.git",
            "git@github.com-work:acme/web.git",
            "ssh://git@github.com:22/acme/web",
            "ssh://git@github.com-work/acme/web.git",
        ] {
            assert_eq!(remote_repo(url).as_deref(), Some("acme/web"), "{url}");
        }
        for url in [
            "/srv/git/web.git",
            "file:///srv/git/web.git",
            "../web",
            "https://github.com/acme",
            "https://github.com/acme/web/pulls",
        ] {
            assert_eq!(remote_repo(url), None, "{url}");
        }
    }

    #[test]
    fn picks_the_remote_gh_uses() {
        let text = "fork\tgit@github.com:me/web.git (fetch)\nfork\tgit@github.com:me/web.git (push)\n\
                    origin\tgit@github.com-work:me/web.git (fetch)\nupstream\thttps://github.com/acme/web.git (fetch)\n\
                    mirror\thttps://gitlab.example.com/acme/web-mirror.git (fetch)\nlocal\t/srv/git/web.git (fetch)\n";
        let all = remotes(text);
        assert_eq!(
            all.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["fork", "origin", "upstream", "mirror"]
        );
        let pick = |rs: &[Remote], set: &str| pick_repo(rs, set).map(|d| (d.name, d.several));
        let some = |name: &str, several| Some((name.to_string(), several));

        // upstream before origin; the GitLab mirror doesn't count.
        assert_eq!(pick(&all, ""), some("acme/web", true));
        // gh repo set-default on origin.
        assert_eq!(pick(&all, "remote.origin.gh-resolved base\n"), some("me/web", true));
        // A default may name the repository instead of `base`.
        assert_eq!(pick(&all, "remote.fork.gh-resolved acme/other\n"), some("acme/other", true));
        // A default on a remote that isn't on GitHub is ignored, as gh does.
        assert_eq!(pick(&all, "remote.mirror.gh-resolved base\n"), some("acme/web", true));

        // Without upstream: origin before other names.
        let fork_and_origin = remotes(
            "fork\tgit@github.com:acme/web.git (fetch)\norigin\thttps://github.com/acme/web (fetch)\n",
        );
        assert_eq!(pick(&fork_and_origin, ""), some("acme/web", false));
        // GitHub Enterprise: no host says github, so every remote counts.
        assert_eq!(
            pick(&remotes("origin\tgit@git.acme.example:acme/web.git (fetch)\n"), ""),
            some("acme/web", false)
        );
        assert_eq!(pick(&remotes("local\t/srv/git/web.git (fetch)\n"), ""), None);
        // The host as gh resolves it: an SSH alias is github.com.
        let host = |rs: &[Remote], set: &str| pick_repo(rs, set).map(|d| d.host);
        assert_eq!(host(&all, "remote.origin.gh-resolved base\n").as_deref(), Some("github.com"));
        assert_eq!(
            host(&remotes("origin\tgit@git.acme.example:acme/web.git (fetch)\n"), "").as_deref(),
            Some("git.acme.example")
        );
    }

    fn numbers((best, closed): (Option<&Candidate>, Option<u64>)) -> (Option<u64>, Option<u64>) {
        (best.map(|c| c.number), closed)
    }

    #[test]
    fn prefers_open_then_merged_and_sets_closed_aside() {
        let c = |number, state: &str, updated: &str| Candidate {
            number,
            url: String::new(),
            state: PrState::parse(state),
            stamp: Stamp { updated: updated.into(), ..Stamp::default() },
            exact: false,
        };
        let pick = |c: &[Candidate]| numbers(pick(c));
        let all = [
            c(1, "CLOSED", "2024-03"),
            c(2, "MERGED", "2024-01"),
            c(3, "OPEN", "2024-02"),
            c(4, "OPEN", "2024-04"),
        ];
        assert_eq!(pick(&all), (Some(4), Some(1)));
        assert_eq!(pick(&all[..2]), (Some(2), Some(1)));
        assert_eq!(pick(&all[..1]), (None, Some(1)));
        // The branch's own merged PR beats a newer one that only mentions the key.
        let own = Candidate { exact: true, ..c(5, "MERGED", "2024-01") };
        assert_eq!(pick(&[c(6, "MERGED", "2024-05"), own]), (Some(5), None));
        let json = json!([{"number":7,"title":"ENG-52 thing","headRefName":"x","state":"OPEN","updatedAt":"t"},
                          {"number":8,"title":"other","headRefName":"y","state":"OPEN","updatedAt":"t"}]);
        assert_eq!(candidates(&json, Some("eng-52")).len(), 1);
    }

    #[test]
    fn takes_the_prs_that_close_an_issue_in_its_repository() {
        let json = include_str!("../tests/fixtures/github/closing-prs.json");
        let linked = parse_closing(json, "Acme/Web");
        // acme/api's PR can't be viewed by its number in acme/web.
        assert_eq!(linked.iter().map(|c| c.number).collect::<Vec<_>>(), [40, 45]);
        assert!(linked.iter().all(|c| c.exact));
        assert_eq!(numbers(pick(&linked)), (Some(45), Some(40)));
        assert_eq!(linked[1].url, "https://github.com/acme/web/pull/45");
        assert_eq!(linked[1].stamp.checks, "FAILURE");
        assert!(parse_closing("{}", "acme/web").is_empty());
        assert!(parse_closing("not json", "acme/web").is_empty());
    }

    #[test]
    fn matches_whole_keys_only() {
        let json = json!([{"number":1,"title":"ENG-52: login","headRefName":"x","state":"MERGED","updatedAt":"t"},
                          {"number":2,"title":"Login","headRefName":"you/eng-52-login","state":"MERGED","updatedAt":"t"},
                          {"number":3,"title":"Fix [eng-5] login","headRefName":"y","state":"OPEN","updatedAt":"t"},
                          {"number":4,"title":"Fix","headRefName":"ENG-5-fix","state":"OPEN","updatedAt":"t"},
                          {"number":5,"title":"Fix","headRefName":"ENG-5","state":"OPEN","updatedAt":"t"},
                          {"number":6,"title":"XENG-5 fix","headRefName":"z","state":"OPEN","updatedAt":"t"}]);
        let numbers =
            |key| candidates(&json, Some(key)).iter().map(|c| c.number).collect::<Vec<_>>();
        assert_eq!(numbers("ENG-5"), [3, 4, 5]);
        assert_eq!(numbers("eng-52"), [1, 2]);
        assert!(mentions("ENG-5", "eng-5") && mentions("feature/ENG-5.1", "ENG-5"));
        assert!(!mentions("ENG-52", "ENG-5") && !mentions("ENG-5", ""));
    }

    /// The details query's reply for a PR with these fields.
    fn reply(pr: impl Into<Value>) -> Value {
        let pr: Value = pr.into();
        json!({ "data": { "repository": { "pullRequest": pr } } })
    }

    #[test]
    fn parses_a_prs_details() {
        let check = |name: &str, workflow: &str, status: &str, conclusion: &str| {
            json!({ "__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion,
                    "checkSuite": { "workflowRun": { "workflow": { "name": workflow } } } })
        };
        let mut e2e = check("e2e", "CI", "COMPLETED", "FAILURE");
        e2e["detailsUrl"] = json!("https://github.com/acme/web/actions/runs/1/job/2");
        let mut unit = check("unit", "CI", "COMPLETED", "SUCCESS");
        unit["completedAt"] = json!("2024-01-01T00:00:00Z");
        let status = json!({ "__typename": "StatusContext", "context": "vercel", "state": "SUCCESS",
                             "targetUrl": "https://vercel.example/acme/web/1" });
        let contexts = [unit, e2e, check("lint", "", "IN_PROGRESS", ""), status];
        let json = reply(json!({
            "number": 412, "title": "t", "url": "u", "state": "OPEN", "isDraft": false,
            "reviewDecision": "CHANGES_REQUESTED", "mergeable": "MERGEABLE",
            "headRefName": "0008-bulk/ENG-52", "baseRefName": "main", "isCrossRepository": false,
            "updatedAt": "2024-01-02T00:00:00Z", "headRefOid": "abc",
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": {
                "state": "FAILURE", "contexts": { "nodes": contexts } } } }] },
            "reviews": { "nodes": [
                { "id": "r1", "author": { "login": "a" }, "state": "APPROVED", "submittedAt": "x",
                  "url": "https://github.com/acme/web/pull/412#pullrequestreview-1" },
                { "author": { "login": "a" }, "state": "COMMENTED", "submittedAt": "y" },
                { "author": { "login": "b" }, "state": "CHANGES_REQUESTED", "submittedAt": "z" }] },
            "comments": { "nodes": [
                { "id": "c1", "body": "first", "createdAt": "x" },
                { "id": "c2", "body": "second", "createdAt": "y" }] },
            "reviewThreads": { "totalCount": 0, "nodes": [] }
        }));
        let pr = parse(&json).unwrap();
        assert_eq!(pr.base, "main");
        assert!(!pr.cross_repository);
        let stamp = Stamp {
            updated: "2024-01-02T00:00:00Z".into(),
            head: "abc".into(),
            checks: "FAILURE".into(),
            mergeable: "MERGEABLE".into(),
            draft: false,
            review: "CHANGES_REQUESTED".into(),
        };
        assert_eq!(pr.stamp, stamp);
        assert_eq!(
            (&pr.state, &pr.review_decision, &pr.mergeable),
            (&PrState::Open, &Decision::ChangesRequested, &Mergeable::Yes)
        );
        // A PR GitHub doesn't say is from this repository counts as a fork's.
        let bare =
            parse(&reply(json!({ "number": 1, "state": "OPEN", "headRefName": "x" }))).unwrap();
        assert!(bare.cross_repository);
        assert_eq!(pr.count(CheckState::Pass), 2);
        assert_eq!(pr.count(CheckState::Fail), 1);
        assert_eq!(pr.count(CheckState::Pending), 1);
        assert_eq!(pr.checks[0].name, "CI / e2e");
        assert_eq!(pr.checks[0].url, "https://github.com/acme/web/actions/runs/1/job/2");
        assert_eq!(
            pr.checks.iter().find(|c| c.name == "vercel").unwrap().url,
            "https://vercel.example/acme/web/1"
        );
        // a's comment after their approval doesn't replace it.
        let latest = pr.latest_reviews();
        assert_eq!(
            latest.iter().map(|r| (r.author.as_str(), r.state.name())).collect::<Vec<_>>(),
            [("a", "APPROVED"), ("b", "CHANGES_REQUESTED")]
        );
        assert_eq!(
            (latest[0].id.as_str(), latest[0].url.as_str()),
            ("r1", "https://github.com/acme/web/pull/412#pullrequestreview-1")
        );
        assert_eq!(pr.reviews.len(), 3);
        // Comments come oldest first and are kept newest first.
        assert_eq!(pr.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["c2", "c1"]);
        assert!(pr.threads.is_ok_and(|t| t.list.is_empty()));
        // A reply without the threads still shows the PR, and says they're missing rather than
        // that there are none; one without the PR is an error.
        assert!(bare.threads.is_err());
        assert!(parse(&reply(Value::Null)).is_err());
    }

    #[test]
    fn keeps_review_text_that_still_stands() {
        let json = r#"{"number":1,"reviews":{"nodes":[
            {"id":"1","author":{"login":"a"},"state":"CHANGES_REQUESTED","submittedAt":"2024-01-01T00:00:00Z","body":"Rename it"},
            {"id":"2","author":{"login":"a"},"state":"APPROVED","submittedAt":"2024-01-02T00:00:00Z","body":"Thanks"},
            {"id":"3","author":{"login":"a"},"state":"COMMENTED","submittedAt":"2024-01-03T00:00:00Z","body":"One more nit"},
            {"id":"4","author":{"login":"b"},"state":"COMMENTED","submittedAt":"2024-01-01T00:00:00Z","body":"Why?"},
            {"id":"5","author":{"login":"b"},"state":"COMMENTED","submittedAt":"2024-01-02T00:00:00Z","body":""},
            {"id":"6","author":{"login":"b"},"state":"CHANGES_REQUESTED","submittedAt":"2024-01-03T00:00:00Z","body":"Add a test"}]}}"#;
        let pr = parse(&reply(serde_json::from_str::<Value>(json).unwrap())).unwrap();
        let ids = |rs: Vec<&Review>| rs.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        // a's change request was answered by the approval; b's question by the change request.
        assert_eq!(ids(pr.review_text()), ["2", "3", "6"]);
        assert_eq!(ids(pr.latest_reviews()), ["2", "6"]);
    }

    #[test]
    fn reads_states_and_keeps_the_ones_it_does_not_know() {
        for name in ["OPEN", "CLOSED", "MERGED"] {
            assert!(!matches!(PrState::parse(name), PrState::Other(_)), "{name}");
        }
        for name in ["APPROVED", "CHANGES_REQUESTED", "COMMENTED", "DISMISSED", "PENDING"] {
            assert!(!matches!(ReviewState::parse(name), ReviewState::Other(_)), "{name}");
        }
        for name in ["", "APPROVED", "CHANGES_REQUESTED", "REVIEW_REQUIRED"] {
            assert!(!matches!(Decision::parse(name), Decision::Other(_)), "{name}");
        }
        for name in ["MERGEABLE", "CONFLICTING", "UNKNOWN"] {
            assert!(!matches!(Mergeable::parse(name), Mergeable::Other(_)), "{name}");
        }
        // A decision and a review word a change request differently, by grammar.
        assert_eq!(Decision::ChangesRequested.to_string(), "changes requested");
        assert_eq!(ReviewState::ChangesRequested.verb(), "requested changes");
        // A state GitHub adds later shows as its word, on one line.
        let review = ReviewState::parse("ESCALATED_TO_ADMINS");
        assert_eq!(review, ReviewState::Other("ESCALATED_TO_ADMINS".into()));
        assert_eq!((review.glyph(), review.verb()), ("?", "escalated to admins".into()));
        let decision = Decision::parse("NEEDS_TRIAGE");
        assert_eq!((decision.glyph(), decision.short()), (Some("?"), None));
        assert_eq!(decision.to_string(), "needs triage");
        assert_eq!(PrState::parse("LOCKED\n> ok").to_string(), "locked > ok");
        assert_eq!(Decision::Unset.glyph(), None);
    }

    #[test]
    fn parses_review_threads() {
        let c = |id: &str, body: &str| {
            let url = format!("https://github.com/acme/web/pull/1#discussion_r{id}");
            json!({ "id": id, "author": { "login": "bo" }, "body": body, "createdAt": "t", "url": url })
        };
        let pr = json!({
          "reviewThreads": {
            "totalCount": 7,
            "nodes": [
                { "path": "src/a.rs", "line": 12, "originalLine": 10, "isResolved": false, "isOutdated": false,
                  "first": { "totalCount": 2, "nodes": [c("1", "Why?")] },
                  "newest": { "nodes": [c("1", "Why?"), c("2", "Because")] } },
                { "path": "src/b.rs", "line": null, "originalLine": 40, "isResolved": true, "isOutdated": true,
                  "first": { "totalCount": 30, "nodes": [c("3", "Rename")] },
                  "newest": { "nodes": [c("4", "Done"), c("5", "Thanks")] } }
            ]
        } });
        let t = parse_threads(&pr).unwrap();
        assert_eq!((t.total, t.left_out(), t.unresolved().count()), (7, 5, 1));
        let ids = |th: &Thread| th.comments.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            (ids(&t.list[0]), t.list[0].left_out, t.list[0].line),
            (vec!["1".into(), "2".into()], 0, Some(12))
        );
        // An outdated thread keeps the line it was left on; replies between the first and the
        // newest are counted.
        assert_eq!(
            (ids(&t.list[1]), t.list[1].left_out, t.list[1].line),
            (vec!["3".into(), "4".into(), "5".into()], 27, Some(40))
        );
        assert_eq!(t.list[0].comments[1].url, "https://github.com/acme/web/pull/1#discussion_r2");
        assert!(parse_threads(&json!({})).is_err());
    }

    #[test]
    fn reads_the_repository_from_a_pr_url() {
        let r = |h: &str, o: &str, n: &str| Some((h.to_string(), o.to_string(), n.to_string()));
        assert_eq!(pr_repo("https://github.com/acme/web/pull/412"), r("github.com", "acme", "web"));
        assert_eq!(
            pr_repo("https://git.acme.example/acme/web/pull/7"),
            r("git.acme.example", "acme", "web")
        );
        assert_eq!(pr_repo("https://github.com/acme/web/issues/412"), None);
        assert_eq!(pr_repo(""), None);
    }

    const PR_SEARCH: &str = include_str!("../tests/fixtures/github/pr-search.json");
    const PR_SEARCH_CLOSED: &str = include_str!("../tests/fixtures/github/pr-search-closed.json");
    const PR_BRANCH: &str = include_str!("../tests/fixtures/github/pr-branch.json");
    const PR_LOOKUP: &str = include_str!("../tests/fixtures/github/pr-lookup.json");
    const PR_DETAILS: &str = include_str!("../tests/fixtures/github/pr-details.json");
    const CLOSING_JSON: &str = include_str!("../tests/fixtures/github/closing-prs.json");

    /// Which of the lookup's gh calls `args` is, by what only that call has.
    fn call(args: &[&str]) -> &'static str {
        let query = args.iter().find(|a| a.starts_with("query=")).copied().unwrap_or("");
        match () {
            _ if query.contains("search(") => "list",
            _ if query.contains("closedByPullRequestsReferences") => "closing",
            _ if query.contains("reviewThreads") => "details",
            _ if query.contains("before: $before") => "older",
            _ => "unexpected",
        }
    }

    /// Runs `fetch` in `repo` with gh answering each call from `replies`, and returns the lookup
    /// and the calls, in order.
    fn lookup(
        repo: &Path,
        branch: Option<&str>,
        closes: Option<&Closes>,
        replies: &[(&str, Result<&str, &str>)],
    ) -> (Result<Lookup, String>, Vec<String>) {
        let calls = std::sync::Mutex::new(Vec::new());
        let gh = |args: &[&str]| {
            let c = call(args);
            calls.lock().unwrap().push(format!("{c}: {}", args[..args.len().min(6)].join(" ")));
            let reply = replies.iter().find(|(name, _)| *name == c).map(|(_, r)| *r);
            reply.unwrap_or(Err("no reply")).map(String::from).map_err(String::from)
        };
        let res = fetch(repo, branch, "ENG-52", closes, gh);
        (res, calls.into_inner().unwrap())
    }

    fn names(calls: &[String]) -> Vec<&str> {
        calls.iter().map(|c| c.split(':').next().unwrap()).collect()
    }

    fn acme_web_12() -> Closes {
        Closes { repo: crate::github_issues::Repo::parse("acme/web").unwrap(), number: 12 }
    }

    #[test]
    fn takes_the_pr_github_links_as_closing_the_issue() {
        let replies =
            [("closing", Ok(CLOSING_JSON)), ("details", Ok(PR_DETAILS)), ("list", Ok(PR_SEARCH))];
        let (res, calls) = lookup(Path::new("/src/acme"), None, Some(&acme_web_12()), &replies);
        let found = res.unwrap();
        // The open one in the issue's repository, fetched there; no list.
        assert_eq!(names(&calls), ["closing", "details"]);
        assert_eq!(calls[0], "closing: api graphql --hostname github.com -f owner=acme");
        assert_eq!(calls[1], "details: api graphql --hostname github.com -f owner=acme");
        let pr = found.pr.unwrap();
        assert_eq!((pr.number, pr.head.as_str(), pr.cross_repository), (45, "eng-52-login", false));
        assert_eq!((found.closed, found.branch_gone), (None, false));
        // The threads came with it, and the review its page.
        assert_eq!(pr.threads.as_ref().unwrap().list[0].path, "src/auth/link.rs");
        assert_eq!(pr.reviews[0].url, "https://github.com/acme/web/pull/45#pullrequestreview-1");
        assert_eq!((pr.count(CheckState::Pass), pr.count(CheckState::Fail)), (1, 1));
    }

    #[test]
    fn falls_back_to_the_search_when_the_closing_query_fails() {
        let replies = [
            ("closing", Err("HTTP 502: Bad Gateway (https://api.github.com/graphql)")),
            ("list", Ok(PR_SEARCH)),
            ("details", Ok(PR_DETAILS)),
        ];
        let (res, calls) = lookup(Path::new("/src/acme"), None, Some(&acme_web_12()), &replies);
        assert_eq!(names(&calls), ["closing", "list", "details"]);
        assert_eq!(res.unwrap().pr.unwrap().number, 45);
        // As when the issue links only a PR closed without merging.
        let closed = r#"{"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[
            {"number":40,"state":"CLOSED","updatedAt":"t","repository":{"nameWithOwner":"acme/web"}}]}}}}}"#;
        let replies =
            [("closing", Ok(closed)), ("list", Ok(PR_SEARCH)), ("details", Ok(PR_DETAILS))];
        let (res, calls) = lookup(Path::new("/src/acme"), None, Some(&acme_web_12()), &replies);
        assert_eq!(names(&calls), ["closing", "list", "details"]);
        assert_eq!(res.unwrap().pr.unwrap().number, 45);
    }

    #[test]
    fn shows_the_open_pr_before_the_merged_one() {
        // #61 (ENG-520) is newer and open, but isn't ENG-52's; #38 merged; #40 closed.
        let mut threadless: Value = serde_json::from_str(PR_DETAILS).unwrap();
        threadless["data"]["repository"]["pullRequest"]["reviewThreads"] = Value::Null;
        let threadless = threadless.to_string();
        let replies = [("list", Ok(PR_SEARCH)), ("details", Ok(threadless.as_str()))];
        let (res, calls) = lookup(Path::new("/src/acme"), None, None, &replies);
        assert_eq!(names(&calls), ["list", "details"]);
        let found = res.unwrap();
        let pr = found.pr.unwrap();
        assert_eq!((pr.number, found.closed), (45, None));
        // Threads GitHub didn't send still leave the PR.
        assert!(pr.threads.is_err());
        // An open PR on the ticket's branch beats a newer open one that only mentions the key.
        let replies = [("list", Ok(PR_BRANCH)), ("details", Ok(PR_DETAILS))];
        let (res, calls) = lookup(Path::new("/src/acme"), Some("eng-52-login"), None, &replies);
        assert_eq!(names(&calls), ["list", "details"]);
        let found = res.unwrap();
        assert_eq!(found.pr.unwrap().number, 45);
        assert!(!found.branch_gone);
    }

    #[test]
    fn keeps_the_details_of_a_pr_that_has_not_changed() {
        let shown = parse(&serde_json::from_str(PR_DETAILS).unwrap()).unwrap();
        let ask = |list: &str, shown: Option<&Pr>| {
            let w = Wanted { shown: shown.cloned(), ..wanted("ENG-52", Some("eng-52-login")) };
            let replies = [("list", Ok(list)), ("details", Ok(PR_DETAILS))];
            let (mut found, calls) = lookup_all(&[w], &replies);
            let pr = found.pop().unwrap().unwrap().pr.unwrap();
            let fetched = calls.iter().filter(|c| call(&strs(c)) == "details").count();
            (pr, fetched)
        };
        // The same updatedAt, head, check state, mergeability, draft flag and review decision: the
        // details shown stay, and no query runs.
        let (pr, fetched) = ask(PR_BRANCH, Some(&shown));
        assert_eq!((pr.number, fetched), (45, 0));
        assert_eq!(pr.threads.unwrap().list[0].path, "src/auth/link.rs");
        // A change in any of them fetches the details.
        let changes = [
            ("2026-10-01T10:00:00Z", "2026-10-01T11:00:00Z"),
            (
                "0123456789abcdef0123456789abcdef01234567",
                "89abcdef0123456789abcdef0123456789abcdef",
            ),
            ("\"FAILURE\"", "\"PENDING\""),
            // Another PR merged, and this one conflicts with its base now.
            ("\"MERGEABLE\"", "\"CONFLICTING\""),
            ("\"isDraft\": false", "\"isDraft\": true"),
            ("\"CHANGES_REQUESTED\"", "\"APPROVED\""),
        ];
        for (from, to) in changes {
            let list = PR_BRANCH.replace(from, to);
            assert_ne!(list, PR_BRANCH);
            assert_eq!(ask(&list, Some(&shown)).1, 1, "{from} to {to}");
        }
        // So does nothing shown, a PR that's another one, one whose threads failed, and one
        // GitHub sent without a stamp.
        assert_eq!(ask(PR_BRANCH, None).1, 1);
        let other = Pr { url: "https://github.com/acme/api/pull/45".into(), ..shown.clone() };
        assert_eq!(ask(PR_BRANCH, Some(&other)).1, 1);
        let failed = Pr { threads: Err("x".into()), ..shown.clone() };
        assert_eq!(ask(PR_BRANCH, Some(&failed)).1, 1);
        let unstamped = Pr { stamp: Stamp::default(), ..shown.clone() };
        let list = PR_BRANCH
            .replace("\"2026-10-01T10:00:00Z\"", "null")
            .replace("\"0123456789abcdef0123456789abcdef01234567\"", "null")
            .replace("{ \"state\": \"FAILURE\" }", "null")
            .replace("\"MERGEABLE\"", "null")
            .replace("\"CHANGES_REQUESTED\"", "null");
        assert_eq!(ask(&list, Some(&unstamped)).1, 1);
    }

    #[test]
    fn fetches_reviews_and_comments_past_the_first_page() {
        let mut first: Value = serde_json::from_str(PR_DETAILS).unwrap();
        let pr = &mut first["data"]["repository"]["pullRequest"];
        pr["comments"]["pageInfo"] = json!({ "hasPreviousPage": true, "startCursor": "@c2" });
        let older = |what: &str, id: &str, more: bool, cursor: &str| {
            let node = json!({ "id": id, "author": { "login": "ana-example" }, "state": "COMMENTED",
                               "submittedAt": "2026-09-01T00:00:00Z", "createdAt": "2026-09-01T00:00:00Z" });
            let page = json!({ "pageInfo": { "hasPreviousPage": more, "startCursor": cursor },
                               "nodes": [node] });
            let mut pr = json!({});
            pr[what] = page;
            reply(pr).to_string()
        };
        let (first, c1, c0) = (
            first.to_string(),
            older("comments", "IC_2", true, "@c1"),
            older("comments", "IC_1", false, "@c0"),
        );
        let calls = std::sync::Mutex::new(Vec::new());
        let gh = |args: &[&str]| {
            calls.lock().unwrap().push(args.iter().map(|a| a.to_string()).collect::<Vec<_>>());
            let before = args.iter().find_map(|a| a.strip_prefix("before="));
            Ok::<_, String>(match before {
                None => first.clone(),
                Some("@c2") => c1.clone(),
                Some(_) => c0.clone(),
            })
        };
        let pr = view(&gh, 45, "https://github.com/acme/web/pull/45").unwrap();
        let ids: Vec<&str> = pr.comments.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["IC_10", "IC_2", "IC_1"], "newest first, every page");
        assert_eq!(pr.reviews.len(), 1, "one page of reviews");
        let calls = calls.into_inner().unwrap();
        let older: Vec<&str> = calls.iter().map(|c| call(&strs(c))).collect();
        assert_eq!(older, ["details", "older", "older"]);
        // The cursor goes as plain text, so one starting with `@` reads no file.
        assert!(calls[1].windows(2).any(|w| w[0] == "-f" && w[1] == "before=@c2"));
        assert!(calls[1].iter().any(|a| a.contains("comments(last: $page, before: $before)")));
    }

    fn wanted(key: &str, branch: Option<&str>) -> Wanted {
        Wanted { key: key.into(), branch: branch.map(String::from), ..Wanted::default() }
    }

    /// Runs `fetch_all` with gh answering each call from `replies`, and returns the lookups and
    /// every call's arguments, in order.
    fn lookup_all(
        wanted: &[Wanted],
        replies: &[(&str, Result<&str, &str>)],
    ) -> (Vec<Result<Lookup, String>>, Vec<Vec<String>>) {
        let calls = std::sync::Mutex::new(Vec::new());
        let gh = |args: &[&str]| {
            calls.lock().unwrap().push(args.iter().map(|a| a.to_string()).collect());
            let reply = replies.iter().find(|(name, _)| *name == call(args)).map(|(_, r)| *r);
            reply.unwrap_or(Err("no reply")).map(String::from).map_err(String::from)
        };
        let found = fetch_all(Path::new("/src/acme"), wanted, gh);
        (found, calls.into_inner().unwrap())
    }

    #[test]
    fn looks_up_every_tickets_pr_in_one_query() {
        let wanted = [
            wanted("ENG-52", Some("eng-52-login")),
            wanted("ENG-61", None),
            wanted("ENG-7", Some("eng-7-export")),
            wanted("ENG-8", None),
            wanted("ENG-9", Some("eng-9-search")),
        ];
        let replies = [("list", Ok(PR_LOOKUP)), ("details", Ok(PR_DETAILS))];
        let (found, calls) = lookup_all(&wanted, &replies);
        let mut viewed: Vec<&str> = calls
            .iter()
            .filter(|c| call(&strs(c)) == "details")
            .filter_map(|c| c.iter().find_map(|a| a.strip_prefix("number=")))
            .collect();
        viewed.sort();
        assert_eq!(calls.iter().filter(|c| call(&strs(c)) == "list").count(), 1);
        // ENG-52: the open PR on its branch, not the newer one that only mentions the key.
        // ENG-61: the newest open one that mentions it (ENG-610's isn't), before the merged one.
        // ENG-7: its branch's merged PR, before a newer merged one that only mentions it.
        assert_eq!(viewed, ["45", "50", "66"]);
        let found: Vec<Lookup> = found.into_iter().map(Result::unwrap).collect();
        let shown: Vec<bool> = found.iter().map(|l| l.pr.is_some()).collect();
        assert_eq!(shown, [true, true, true, false, false]);
        // ENG-8: nothing open or merged; the newest closed one is noted. ENG-9: nothing at all,
        // and a branch git can't check counts as there.
        let closed: Vec<Option<u64>> = found.iter().map(|l| l.closed).collect();
        assert_eq!(closed, [None, None, None, Some(33), None]);
        assert!(found.iter().all(|l| !l.branch_gone));
    }

    fn strs(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    #[test]
    fn lists_prs_in_the_repository_gh_picks() {
        let some = wanted("ENG-52", Some("@notes"));
        let none = wanted("ENG-61", None);
        let args = list_args(Some("git.example.com"), &[&some, &none]);
        let after =
            |flag: &str, value: &str| args.windows(2).any(|w| w[0] == flag && w[1] == value);
        assert_eq!(args[..4], ["api", "graphql", "--hostname", "git.example.com"]);
        // gh fills in the repository; a branch is plain text, so `@notes` reads no file.
        assert!(after("-F", "owner={owner}") && after("-F", "name={repo}"), "{args:?}");
        assert!(after("-f", "b0=@notes"), "{args:?}");
        assert!(after("-F", "q0=repo:{owner}/{repo} type:pr ENG-52"), "{args:?}");
        assert!(after("-F", "q1=repo:{owner}/{repo} type:pr ENG-61"), "{args:?}");
        assert!(!args.iter().any(|a| a.starts_with("b1=")), "ENG-61 has no branch: {args:?}");
        let query = args.last().unwrap();
        let vars = "$owner: String!, $name: String!, $b0: String!, $q0: String!, $q1: String!";
        assert!(query.starts_with(&format!("query=query({vars})")), "{query}");
        assert!(
            query.contains("b0: pullRequests(headRefName: $b0, states: [OPEN, CLOSED, MERGED]")
        );
        assert!(query.contains("q1: search(query: $q1, type: ISSUE, first: 20)"));
        // Without a branch there's no repository to ask, nor its variables.
        let args = list_args(None, &[&none]);
        assert_eq!(args[..2], ["api", "graphql"]);
        let query = args.last().unwrap();
        assert!(!query.contains("repository") && !query.contains("$owner"), "{query}");
        assert!(!args.iter().any(|a| a.contains("owner={owner}")));
    }

    #[test]
    fn asks_about_twenty_tickets_at_a_time_and_shares_a_failure() {
        let many: Vec<Wanted> = (1..=25).map(|n| wanted(&format!("ENG-{n}"), None)).collect();
        let (found, calls) = lookup_all(&many, &[("list", Ok(r#"{"data":{}}"#))]);
        assert_eq!(calls.len(), 2);
        assert!(calls[1].iter().any(|a| a == "q4=repo:{owner}/{repo} type:pr ENG-25"));
        assert!(found.iter().all(|l| l.as_ref().is_ok_and(|l| l.pr.is_none())));
        // A failed query fails every ticket it asked about, with what gh said.
        let (found, _) = lookup_all(&many[..3], &[("list", Err("HTTP 502: Bad Gateway"))]);
        assert!(
            found
                .iter()
                .all(|l| l.as_ref().err().map(String::as_str) == Some("HTTP 502: Bad Gateway"))
        );
        // A reply that isn't the query's.
        let (found, _) = lookup_all(&many[..1], &[("list", Ok(r#"{"errors":[]}"#))]);
        assert_eq!(found[0].as_ref().err().map(String::as_str), Some("GitHub returned no PRs"));
    }

    #[test]
    fn a_ticket_whose_issue_a_pr_closes_is_left_out_of_the_list() {
        let linked = Wanted { closes: Some(acme_web_12()), ..wanted("ENG-52", Some("x")) };
        let replies = [
            ("closing", Ok(CLOSING_JSON)),
            ("list", Ok(r#"{"data":{}}"#)),
            ("details", Ok(PR_DETAILS)),
        ];
        let (found, calls) = lookup_all(&[linked, wanted("ENG-61", None)], &replies);
        let list: Vec<&Vec<String>> = calls.iter().filter(|c| call(&strs(c)) == "list").collect();
        assert_eq!(list.len(), 1);
        assert!(list[0].iter().any(|a| a == "q0=repo:{owner}/{repo} type:pr ENG-61"));
        assert!(!list[0].iter().any(|a| a.contains("ENG-52") || a.starts_with("b0=")));
        assert_eq!(found[0].as_ref().unwrap().pr.as_ref().unwrap().number, 45);
        assert!(found[1].as_ref().unwrap().pr.is_none());
    }

    #[test]
    fn hides_a_pr_closed_without_merging() {
        let (res, calls) =
            lookup(Path::new("/src/acme"), None, None, &[("list", Ok(PR_SEARCH_CLOSED))]);
        assert_eq!(names(&calls), ["list"]);
        let found = res.unwrap();
        assert!(found.pr.is_none());
        assert_eq!((found.closed, found.branch_gone), (Some(40), false));
    }

    #[test]
    fn flags_a_branch_that_is_gone() {
        let root = std::env::temp_dir().join(format!("taskman-github-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let sh = |dir: &Path, cmd: &str| {
            let o = Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap();
            assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
        };
        sh(
            &root,
            "git init -q --bare -b main origin.git && git clone -q origin.git acme 2>/dev/null",
        );
        let repo = root.join("acme");
        sh(
            &repo,
            "git -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m one && git push -q origin main",
        );
        let none_on_branch = PR_SEARCH_CLOSED
            .replace(r#""data": {"#, r#""data": {"repository": {"b0": {"nodes": []}},"#);
        let replies = [("list", Ok(none_on_branch.as_str()))];
        let (res, calls) = lookup(&repo, Some("eng-52-login"), None, &replies);
        assert_eq!(names(&calls), ["list"]);
        let found = res.unwrap();
        assert!(found.pr.is_none() && found.branch_gone);
        // A branch that's still there, here or on origin, isn't.
        sh(&repo, "git push -q origin main:eng-52-pushed");
        for branch in ["main", "eng-52-pushed"] {
            let (res, _) = lookup(&repo, Some(branch), None, &replies);
            assert!(!res.unwrap().branch_gone, "{branch}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn says_what_gh_failing_means() {
        let logged_out = "To get started with GitHub CLI, please run:  gh auth login\n\
                          Alternatively, populate the GH_TOKEN environment variable with a GitHub API authentication token.";
        assert_eq!(gh_error(logged_out), Gh::LoggedOut.why());
        assert_eq!(Gh::from_error(&gh_error(logged_out)), Some(Gh::LoggedOut));
        let no_remote = "no git remotes found";
        assert_eq!(Gh::from_error(&gh_error(no_remote)), Some(Gh::NoRemote));
        assert_eq!(gh_error("HTTP 502: Bad Gateway"), "HTTP 502: Bad Gateway");
        // The lookup passes it on, so the panel hides until gh is logged in again.
        let replies = [("list", Err(Gh::LoggedOut.why()))];
        let (res, _) = lookup(Path::new("/src/acme"), None, None, &replies);
        assert_eq!(res.err().as_deref(), Some(Gh::LoggedOut.why()));
    }

    /// JSON of any shape, with the names and values the parsers look for among the rest.
    fn any_json() -> impl Strategy<Value = Value> {
        const NAMES: &[&str] = &[
            "number",
            "title",
            "url",
            "state",
            "isDraft",
            "headRefName",
            "isCrossRepository",
            "statusCheckRollup",
            "commits",
            "commit",
            "contexts",
            "checkSuite",
            "workflowRun",
            "workflow",
            "name",
            "updatedAt",
            "headRefOid",
            "__typename",
            "status",
            "conclusion",
            "completedAt",
            "reviews",
            "comments",
            "author",
            "login",
            "id",
            "submittedAt",
            "body",
            "createdAt",
            "data",
            "repository",
            "pullRequest",
            "reviewThreads",
            "nodes",
            "first",
            "newest",
            "totalCount",
            "line",
            "originalLine",
            "isResolved",
        ];
        let text = prop_oneof![
            prop::sample::select(
                &["StatusContext", "CheckRun", "COMPLETED", "SUCCESS", "OPEN"][..]
            )
            .prop_map(String::from),
            "\\PC{0,8}",
        ];
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<f64>().prop_map(Value::from),
            text.prop_map(Value::from),
        ];
        leaf.prop_recursive(5, 64, 6, |inner| {
            let name = prop_oneof![prop::sample::select(NAMES).prop_map(String::from), "\\PC{0,6}"];
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::from),
                prop::collection::vec((name, inner), 0..6)
                    .prop_map(|fields| Value::Object(fields.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn reads_owner_and_name_from_any_https_remote(
            host in "[a-z0-9][a-z0-9.-]{0,15}",
            owner in "[A-Za-z0-9_.-]{1,15}",
            name in "[A-Za-z0-9_.-]{1,15}",
        ) {
            let url = format!("https://{host}/{owner}/{name}.git");
            prop_assert_eq!(remote_repo(&url), Some(format!("{owner}/{name}")));
        }

        #[test]
        fn finds_a_key_between_non_alphanumeric_characters(
            key in "[A-Za-z]{1,5}-[0-9]{1,4}",
            before in "([a-z0-9 ]{0,8}[ /_(\\[:.-])?",
            after in "([ /_)\\]:.-][a-z0-9 ]{0,8})?",
            lower in any::<bool>(),
        ) {
            let text = if lower { key.to_lowercase() } else { key.to_uppercase() };
            let text = format!("{before}{text}{after}");
            prop_assert!(mentions(&text, &key), "{} in {}", key, text);
        }

        #[test]
        fn never_finds_a_key_followed_by_a_digit(
            key in "[A-Za-z]{1,5}-[0-9]{1,4}",
            before in "[a-z ]{0,8}",
            digit in 0..10u8,
            after in "[a-z0-9 ]{0,8}",
        ) {
            // Neither side has a hyphen, so the key can be nowhere else.
            let text = format!("{before}{key}{digit}{after}");
            prop_assert!(!mentions(&text, &key), "{} in {}", key, text);
        }

        #[test]
        fn a_state_keeps_any_name_github_sends(name in "(?s).{0,24}") {
            prop_assert_eq!(PrState::parse(&name).name().to_string(), name.clone());
            prop_assert_eq!(ReviewState::parse(&name).name().to_string(), name.clone());
            prop_assert_eq!(Decision::parse(&name).name().to_string(), name.clone());
            prop_assert_eq!(Mergeable::parse(&name).name().to_string(), name.clone());
        }

        #[test]
        fn a_state_shows_on_one_line(name in "(?s).{0,24}") {
            let review = ReviewState::parse(&name);
            for shown in [
                PrState::parse(&name).to_string(),
                review.to_string(),
                review.verb(),
                Decision::parse(&name).to_string(),
                Mergeable::parse(&name).to_string(),
            ] {
                prop_assert!(!shown.chars().any(char::is_control), "{:?}", shown);
            }
        }

        #[test]
        fn reading_any_json_never_panics(v in any_json()) {
            let json = v.to_string();
            let _ = parse(&v);
            let _ = parse(&reply(v.clone()));
            let _ = parse_threads(&v);
            let _ = Stamp::read(&v);
            let _ = parse_closing(&json, "acme/web");
            let _ = candidates(&v, Some("ENG-52"));
            let asked = wanted("ENG-52", Some("x"));
            let _ = parse_listed(&json, &[&asked, &asked]);
        }
    }

    #[test]
    fn reads_locked_worktrees() {
        // As git lists a `git worktree add` killed mid-checkout, one locked by hand without a
        // reason, and one locked with a reason.
        let list = parse_worktrees(
            "worktree /src/acme\nHEAD 1\nbranch refs/heads/main\n\n\
             worktree /src/acme-ENG-52\nHEAD 1\nbranch refs/heads/eng-52-login\n\
             locked initializing\n\n\
             worktree /src/acme-ENG-5\nHEAD 2\nbranch refs/heads/eng-5-signup\nlocked\n\n\
             worktree /mnt/usb/acme-ENG-6\nHEAD 3\ndetached\nlocked on a USB drive\n\
             prunable gitdir file points to non-existent location\n",
        );
        let locked: Vec<_> = list.iter().map(|w| w.locked.as_deref()).collect();
        assert_eq!(locked, [None, Some("initializing"), Some(""), Some("on a USB drive")]);
        assert_eq!(list[1].branch.as_deref(), Some("eng-52-login"));
        let unfinished: Vec<_> = list.iter().map(Worktree::unfinished).collect();
        assert_eq!(unfinished, [false, true, false, false]);
        // The half-made worktree is no ticket's; one locked for another reason still is.
        assert_eq!(pick_worktree(&list, "ENG-52"), None);
        assert_eq!(
            pick_worktree(&list, "ENG-5"),
            Some(("/src/acme-ENG-5".into(), "eng-5-signup".into()))
        );
    }

    #[test]
    fn matches_branches_to_tickets() {
        assert_eq!(names_ticket("0008-bulk/ENG-52", "eng-52"), Some(true));
        assert_eq!(names_ticket("eng-52-login", "ENG-52"), Some(false));
        assert_eq!(names_ticket("you/eng-52-fix-login", "ENG-52"), Some(false));
        assert_eq!(names_ticket("worktree-ENG-52", "ENG-52"), Some(false));
        assert_eq!(names_ticket("eng-52-login", "ENG-5"), None);
        assert_eq!(names_ticket("xeng-52", "ENG-52"), None);
        let list = parse_worktrees(
            "worktree /src/acme\nHEAD 1\nbranch refs/heads/main\n\n\
             worktree /src/acme-52\nHEAD 2\nbranch refs/heads/eng-52-login\n\n\
             worktree /src/acme-5\nHEAD 3\nbranch refs/heads/feat/ENG-5\n\n\
             worktree /src/acme-tmp\nHEAD 4\ndetached\n\n\
             worktree /src/acme-61\nHEAD 5\nbranch refs/heads/eng-61-login\n\n\
             worktree /src/acme-change\nHEAD 6\nbranch refs/heads/change/ENG-61\n",
        );
        assert_eq!(list[3].path, PathBuf::from("/src/acme-tmp"));
        assert_eq!(list[3].branch, None);
        assert_eq!(
            pick_worktree(&list, "ENG-52"),
            Some(("/src/acme-52".into(), "eng-52-login".into()))
        );
        assert_eq!(
            pick_worktree(&list, "ENG-5"),
            Some(("/src/acme-5".into(), "feat/ENG-5".into()))
        );
        assert_eq!(pick_worktree(&list, "ENG-6"), None);
        // A branch ending in /<KEY>, as a workflow that makes its own worktrees names it, wins
        // over one that only contains the key.
        assert_eq!(
            pick_worktree(&list, "ENG-61"),
            Some(("/src/acme-change".into(), "change/ENG-61".into()))
        );
    }
}
