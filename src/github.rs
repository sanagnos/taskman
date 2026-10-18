//! Pull request status through the `gh` CLI, and ticket → worktree/branch discovery through git.

use crate::proc;
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
    proc::run(Command::new("git").arg("-C").arg(dir).args(args), GIT_LIMIT)
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

#[derive(Clone, Debug, Default)]
pub struct Review {
    pub id: String,
    pub author: String,
    pub state: String,
    pub when: String,
    pub body: String,
    /// Its page, from the review threads' query (`gh pr view` has none); empty when not fetched.
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
    pub state: String,
    pub is_draft: bool,
    pub review_decision: String,
    pub mergeable: String,
    pub head: String,
    /// The head branch's last commit, which a merged PR vouches for when its worktree goes.
    pub head_oid: String,
    /// The branch it merges into: the default branch, or a stacked ticket's base.
    pub base: String,
    pub checks: Vec<Check>,
    /// Every review, oldest first: gh fetches them all.
    pub reviews: Vec<Review>,
    /// Newest first; gh fetches them all.
    pub comments: Vec<PrComment>,
    /// The inline review threads, fetched separately: failing to get them still shows the PR.
    pub threads: Result<Threads, String>,
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
                Some(_) if r.state == "COMMENTED" => {}
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
                .any(|r| r.author == author && r.state != "COMMENTED" && r.when.as_str() > after)
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
    /// The newest reviews' pages, by id: fetched with the threads, as `gh pr view` has none.
    pub review_urls: Vec<(String, String)>,
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
/// installed and logged in to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gh {
    Ready,
    NotGit,
    NoRemote,
    Missing,
    LoggedOut,
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
        }
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

/// Checks `folder` without the network: its remotes' hosts, then `gh auth token` for them, which
/// reads gh's own login.
pub fn probe(folder: &Path) -> Gh {
    let Ok(out) = git(folder, &["remote", "-v"]) else { return Gh::NotGit };
    if !out.status.success() {
        return Gh::NotGit;
    }
    let hosts = remote_hosts(&String::from_utf8_lossy(&out.stdout));
    let token = |host: &str| {
        proc::run(Command::new("gh").args(["auth", "token", "--hostname", host]), GH_LIMIT)
            .map(|o| o.status)
    };
    let github = hosts.iter().any(|h| h.contains("github"));
    for h in &hosts {
        match token(h) {
            Ok(s) if s.success() => return Gh::Ready,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return if github { Gh::Missing } else { Gh::NoRemote };
            }
            _ => {}
        }
    }
    // An SSH alias like github.com-work is resolved by gh to github.com, which may be logged in.
    match github {
        true if token("github.com").is_ok_and(|s| s.success()) => Gh::Ready,
        true => Gh::LoggedOut,
        false => Gh::NoRemote,
    }
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

const FIELDS: &str = "number,title,url,state,isDraft,reviewDecision,mergeable,headRefName,headRefOid,baseRefName,statusCheckRollup,reviews,comments";

pub(crate) fn gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out =
        proc::run(Command::new("gh").args(args).current_dir(dir).env("NO_COLOR", "1"), GH_LIMIT)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Gh::Missing.why().to_string(),
                std::io::ErrorKind::TimedOut => {
                    format!("gh {}: {e}; the next refresh tries again", args[..2].join(" "))
                }
                _ => format!("could not run gh: {e}"),
            })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if err.contains("no git remotes") || err.contains("none of the git remotes") {
            return Err(NO_REMOTE.into());
        }
        if err.contains("gh auth login") {
            return Err(Gh::LoggedOut.why().into());
        }
        Err(err)
    }
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
    state: String,
    updated: String,
    /// On the ticket's own branch, rather than found by searching for its key.
    exact: bool,
}

const LIST: &str = "number,title,headRefName,state,updatedAt";

/// Parses `gh pr list` output; with a key, keeps only PRs whose branch or title mentions it as a
/// whole (see `mentions`), so `ENG-5` doesn't take `ENG-52`'s PR. Without one, they're the
/// branch's own.
fn candidates(json: &str, key: Option<&str>) -> Result<Vec<Candidate>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let s = |p: &Value, k: &str| p[k].as_str().unwrap_or("").to_string();
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .filter(|p| {
            key.is_none_or(|k| mentions(&s(p, "headRefName"), k) || mentions(&s(p, "title"), k))
        })
        .filter_map(|p| {
            Some(Candidate {
                number: p["number"].as_u64()?,
                state: s(p, "state"),
                updated: s(p, "updatedAt"),
                exact: key.is_none(),
            })
        })
        .collect())
}

/// The PR to show (newest open, else newest merged; the branch's own before those that only
/// mention the key) and the newest closed-unmerged one.
fn pick(c: &[Candidate]) -> (Option<u64>, Option<u64>) {
    let newest = |state: &str| {
        c.iter()
            .filter(|x| x.state == state)
            .max_by(|a, b| (a.exact, &a.updated).cmp(&(b.exact, &b.updated)))
            .map(|x| x.number)
    };
    (newest("OPEN").or_else(|| newest("MERGED")), newest("CLOSED"))
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
        nodes { number state updatedAt repository { nameWithOwner } }
      }
    }
  }
}";

/// The PRs GitHub links to the issue as closing it, in the issue's own repository: a PR from
/// another one can't be viewed by its number there.
fn closing(repo: &Path, c: &Closes) -> Result<Vec<Candidate>, String> {
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
        format!("query={CLOSING_QUERY}"),
    ];
    let json = gh(repo, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(parse_closing(&json, &format!("{}/{}", c.repo.owner, c.repo.name)))
}

fn parse_closing(json: &str, name: &str) -> Vec<Candidate> {
    let v: Value = serde_json::from_str(json).unwrap_or_default();
    let s = |p: &Value, k: &str| p[k].as_str().unwrap_or("").to_string();
    v["data"]["repository"]["issue"]["closedByPullRequestsReferences"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| s(&p["repository"], "nameWithOwner").eq_ignore_ascii_case(name))
        .filter_map(|p| {
            Some(Candidate {
                number: p["number"].as_u64()?,
                state: s(p, "state"),
                updated: s(p, "updatedAt"),
                exact: true,
            })
        })
        .collect()
}

/// Finds the ticket's PR: one GitHub links as closing its issue (`closes`) when there's an open
/// or merged one, else among those on its branch and those mentioning its key. A PR closed
/// without merging isn't shown, so abandoned work doesn't look live.
pub fn fetch(
    repo: &Path,
    branch: Option<&str>,
    key: &str,
    closes: Option<&Closes>,
) -> Result<Lookup, String> {
    // A failed query only means falling back to the search: the calls after it say what's wrong.
    if let Some(c) = closes
        && let Ok(linked) = closing(repo, c)
        && let (Some(n), _) = pick(&linked)
    {
        let spec = c.repo.spec();
        let pr = view(repo, n, Some(&spec))?;
        return Ok(Lookup { pr: Some(pr), closed: None, branch_gone: false });
    }
    let mut found = Vec::new();
    if let Some(b) = branch {
        found = candidates(
            &gh(
                repo,
                &["pr", "list", "--state", "all", "--head", b, "--json", LIST, "--limit", "10"],
            )?,
            None,
        )?;
    }
    let open_on_branch = found.iter().any(|c| c.state == "OPEN");
    if !open_on_branch {
        let by_key = gh(
            repo,
            &["pr", "list", "--state", "all", "--search", key, "--json", LIST, "--limit", "20"],
        )?;
        for c in candidates(&by_key, Some(key))? {
            if !found.iter().any(|x| x.number == c.number) {
                found.push(c);
            }
        }
    }
    let (best, closed) = pick(&found);
    let branch_gone = match branch {
        Some(b) if !open_on_branch => !branch_exists(repo, b),
        _ => false,
    };
    let pr = match best {
        Some(n) => Some(view(repo, n, None)?),
        None => None,
    };
    Ok(Lookup { pr, closed: if best.is_none() { closed } else { None }, branch_gone })
}

/// PR `n` in full, with its review threads: in the repository gh picks in `repo`, or in `spec`'s
/// (`[host/]owner/name`).
fn view(repo: &Path, n: u64, spec: Option<&str>) -> Result<Pr, String> {
    let n = n.to_string();
    let mut args = vec!["pr", "view", n.as_str(), "--json", FIELDS];
    if let Some(spec) = spec {
        args.extend(["--repo", spec]);
    }
    let mut pr = parse(&gh(repo, &args)?)?;
    pr.threads = threads(repo, &pr);
    if let Ok(t) = &pr.threads {
        for r in &mut pr.reviews {
            if let Some((_, url)) = t.review_urls.iter().find(|(id, _)| *id == r.id) {
                r.url.clone_from(url);
            }
        }
    }
    Ok(pr)
}

/// Review threads fetched: the newest ones, and in each the first comment and the newest replies.
/// The newest `THREADS` reviews' pages come with them.
const THREADS: usize = 100;
const REPLIES: usize = 20;

const THREADS_QUERY: &str =
    "query($owner: String!, $name: String!, $number: Int!, $threads: Int!, $replies: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviews(last: $threads) { nodes { id url } }
      reviewThreads(last: $threads) {
        totalCount
        nodes {
          path line originalLine isResolved isOutdated
          first: comments(first: 1) { totalCount nodes { id author { login } body createdAt url } }
          newest: comments(last: $replies) { nodes { id author { login } body createdAt url } }
        }
      }
    }
  }
}";

/// The PR's review threads, and its reviews' pages, through gh's GraphQL API: `gh pr view` shows
/// neither. The host and repository come from the PR's URL, so they're the ones `gh pr view` used.
fn threads(repo: &Path, pr: &Pr) -> Result<Threads, String> {
    let (host, owner, name) =
        pr_repo(&pr.url).ok_or_else(|| format!("can't tell the repository from {}", pr.url))?;
    // -f passes text as it is; -F reads numbers (and would read a file for a value starting
    // with @).
    let args = [
        "api".to_string(),
        "graphql".into(),
        "--hostname".into(),
        host,
        "-f".into(),
        format!("owner={owner}"),
        "-f".into(),
        format!("name={name}"),
        "-F".into(),
        format!("number={}", pr.number),
        "-F".into(),
        format!("threads={THREADS}"),
        "-F".into(),
        format!("replies={REPLIES}"),
        "-f".into(),
        format!("query={THREADS_QUERY}"),
    ];
    parse_threads(&gh(repo, &args.iter().map(String::as_str).collect::<Vec<_>>())?)
}

/// `("github.com", "acme", "web")` from `https://github.com/acme/web/pull/412`.
fn pr_repo(url: &str) -> Option<(String, String, String)> {
    let mut parts = url.strip_prefix("https://")?.split('/');
    let (host, owner, name) = (parts.next()?, parts.next()?, parts.next()?);
    (parts.next() == Some("pull") && ![host, owner, name].contains(&""))
        .then(|| (host.to_string(), owner.to_string(), name.to_string()))
}

fn parse_threads(json: &str) -> Result<Threads, String> {
    let count = |x: &Value| x.as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let pr = &v["data"]["repository"]["pullRequest"];
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
    let review_urls = pr["reviews"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| (s(&r["id"]), s(&r["url"])))
        .filter(|(id, url)| !id.is_empty() && !url.is_empty())
        .collect();
    Ok(Threads { list, total, review_urls })
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

fn parse(json: &str) -> Result<Pr, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();

    let mut checks: Vec<Check> = v["statusCheckRollup"]
        .as_array()
        .map(|items| items.iter().map(parse_check).collect())
        .unwrap_or_default();
    checks.sort_by(|a, b| a.state.cmp(&b.state).then(a.name.cmp(&b.name)));

    let reviews: Vec<Review> = v["reviews"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Review {
            id: s(&r["id"]),
            author: s(&r["author"]["login"]),
            state: s(&r["state"]),
            when: s(&r["submittedAt"]),
            body: s(&r["body"]),
            url: String::new(),
        })
        .collect();

    let mut comments: Vec<PrComment> = v["comments"]
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
        state: s(&v["state"]),
        is_draft: v["isDraft"].as_bool().unwrap_or(false),
        review_decision: s(&v["reviewDecision"]),
        mergeable: s(&v["mergeable"]),
        head: s(&v["headRefName"]),
        head_oid: s(&v["headRefOid"]),
        base: s(&v["baseRefName"]),
        checks,
        reviews,
        comments,
        // `fetch` fills them in.
        threads: Err("not fetched".into()),
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
    let name = match (s("workflowName"), s("name")) {
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
    use serde_json::json;

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

    #[test]
    fn prefers_open_then_merged_and_sets_closed_aside() {
        let c = |number, state: &str, updated: &str| Candidate {
            number,
            state: state.into(),
            updated: updated.into(),
            exact: false,
        };
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
        let json = r#"[{"number":7,"title":"ENG-52 thing","headRefName":"x","state":"OPEN","updatedAt":"t"},
                       {"number":8,"title":"other","headRefName":"y","state":"OPEN","updatedAt":"t"}]"#;
        assert_eq!(candidates(json, Some("eng-52")).unwrap().len(), 1);
    }

    #[test]
    fn takes_the_prs_that_close_an_issue_in_its_repository() {
        let json = include_str!("../tests/fixtures/github/closing-prs.json");
        let linked = parse_closing(json, "Acme/Web");
        // acme/api's PR can't be viewed by its number in acme/web.
        assert_eq!(linked.iter().map(|c| c.number).collect::<Vec<_>>(), [40, 45]);
        assert!(linked.iter().all(|c| c.exact));
        assert_eq!(pick(&linked), (Some(45), Some(40)));
        assert!(parse_closing("{}", "acme/web").is_empty());
        assert!(parse_closing("not json", "acme/web").is_empty());
    }

    #[test]
    fn matches_whole_keys_only() {
        let json = r#"[{"number":1,"title":"ENG-52: login","headRefName":"x","state":"MERGED","updatedAt":"t"},
                       {"number":2,"title":"Login","headRefName":"you/eng-52-login","state":"MERGED","updatedAt":"t"},
                       {"number":3,"title":"Fix [eng-5] login","headRefName":"y","state":"OPEN","updatedAt":"t"},
                       {"number":4,"title":"Fix","headRefName":"ENG-5-fix","state":"OPEN","updatedAt":"t"},
                       {"number":5,"title":"Fix","headRefName":"ENG-5","state":"OPEN","updatedAt":"t"},
                       {"number":6,"title":"XENG-5 fix","headRefName":"z","state":"OPEN","updatedAt":"t"}]"#;
        let numbers =
            |key| candidates(json, Some(key)).unwrap().iter().map(|c| c.number).collect::<Vec<_>>();
        assert_eq!(numbers("ENG-5"), [3, 4, 5]);
        assert_eq!(numbers("eng-52"), [1, 2]);
        assert!(mentions("ENG-5", "eng-5") && mentions("feature/ENG-5.1", "ENG-5"));
        assert!(!mentions("ENG-52", "ENG-5") && !mentions("ENG-5", ""));
    }

    #[test]
    fn parses_pr_view() {
        let json = r#"{"number":412,"title":"t","url":"u","state":"OPEN","isDraft":false,
            "reviewDecision":"CHANGES_REQUESTED","mergeable":"MERGEABLE","headRefName":"0008-bulk/ENG-52","baseRefName":"main",
            "statusCheckRollup":[
              {"__typename":"CheckRun","name":"unit","workflowName":"CI","status":"COMPLETED","conclusion":"SUCCESS","completedAt":"2024-01-01T00:00:00Z"},
              {"__typename":"CheckRun","name":"e2e","workflowName":"CI","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://github.com/acme/web/actions/runs/1/job/2"},
              {"__typename":"CheckRun","name":"lint","workflowName":"","status":"IN_PROGRESS","conclusion":""},
              {"__typename":"StatusContext","context":"vercel","state":"SUCCESS","targetUrl":"https://vercel.example/acme/web/1"}],
            "reviews":[{"id":"r1","author":{"login":"a"},"state":"APPROVED","submittedAt":"x"},
                       {"author":{"login":"a"},"state":"COMMENTED","submittedAt":"y"},
                       {"author":{"login":"b"},"state":"CHANGES_REQUESTED","submittedAt":"z"}],
            "comments":[]}"#;
        let pr = parse(json).unwrap();
        assert_eq!(pr.base, "main");
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
            latest.iter().map(|r| (r.author.as_str(), r.state.as_str())).collect::<Vec<_>>(),
            [("a", "APPROVED"), ("b", "CHANGES_REQUESTED")]
        );
        assert_eq!(latest[0].id, "r1");
        assert_eq!(pr.reviews.len(), 3);
        assert!(pr.threads.is_err(), "not fetched yet, so not empty");
    }

    #[test]
    fn keeps_review_text_that_still_stands() {
        let json = r#"{"number":1,"reviews":[
            {"id":"1","author":{"login":"a"},"state":"CHANGES_REQUESTED","submittedAt":"2024-01-01T00:00:00Z","body":"Rename it"},
            {"id":"2","author":{"login":"a"},"state":"APPROVED","submittedAt":"2024-01-02T00:00:00Z","body":"Thanks"},
            {"id":"3","author":{"login":"a"},"state":"COMMENTED","submittedAt":"2024-01-03T00:00:00Z","body":"One more nit"},
            {"id":"4","author":{"login":"b"},"state":"COMMENTED","submittedAt":"2024-01-01T00:00:00Z","body":"Why?"},
            {"id":"5","author":{"login":"b"},"state":"COMMENTED","submittedAt":"2024-01-02T00:00:00Z","body":""},
            {"id":"6","author":{"login":"b"},"state":"CHANGES_REQUESTED","submittedAt":"2024-01-03T00:00:00Z","body":"Add a test"}]}"#;
        let pr = parse(json).unwrap();
        let ids = |rs: Vec<&Review>| rs.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        // a's change request was answered by the approval; b's question by the change request.
        assert_eq!(ids(pr.review_text()), ["2", "3", "6"]);
        assert_eq!(ids(pr.latest_reviews()), ["2", "6"]);
    }

    #[test]
    fn parses_review_threads() {
        let c = |id: &str, body: &str| {
            let url = format!("https://github.com/acme/web/pull/1#discussion_r{id}");
            json!({ "id": id, "author": { "login": "bo" }, "body": body, "createdAt": "t", "url": url })
        };
        let json = json!({ "data": { "repository": { "pullRequest": {
          "reviews": { "nodes": [{ "id": "PRR_1", "url": "https://github.com/acme/web/pull/1#pullrequestreview-1" }] },
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
        } } } } });
        let t = parse_threads(&json.to_string()).unwrap();
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
        assert_eq!(
            t.review_urls,
            [("PRR_1".into(), "https://github.com/acme/web/pull/1#pullrequestreview-1".into())]
        );
        assert!(parse_threads(r#"{"data":{"repository":{"pullRequest":null}}}"#).is_err());
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
