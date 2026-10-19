//! GitHub Issues as a tracker: one issue with its comments, in the same shape as the other
//! trackers, and the open issues the Add form lists. Everything goes through `gh api graphql`, on
//! gh's own login, against the repository the project saved (`Tracker::site`), never one a
//! worktree's remotes would pick.

use crate::github;
use crate::tracker::{self, COMMENTS, Comment, Created, FetchError, Issue, LinkedPr, Listed};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A repository on a GitHub host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    pub host: String,
    pub owner: String,
    pub name: String,
}

const GITHUB: &str = "github.com";

impl Repo {
    /// From a tracker's site: `acme/web` on github.com, or `git.acme.example/acme/web`.
    pub fn parse(site: &str) -> Option<Repo> {
        let parts: Vec<&str> = site.trim().trim_matches('/').split('/').collect();
        let (host, owner, name) = match parts[..] {
            [owner, name] => (GITHUB, owner, name),
            [host, owner, name] => (host, owner, name),
            _ => return None,
        };
        let part = |s: &str| !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c == '#');
        (part(host) && part(owner) && part(name)).then(|| Repo {
            host: host.to_lowercase(),
            owner: owner.into(),
            name: name.into(),
        })
    }

    /// The site a tracker saves for `owner/name` on `host`.
    pub fn site(host: &str, name: &str) -> String {
        if host.eq_ignore_ascii_case(GITHUB) { name.to_string() } else { format!("{host}/{name}") }
    }

    /// `acme/web`, with its host when it isn't github.com; what gh's `--repo` takes.
    pub fn spec(&self) -> String {
        Repo::site(&self.host, &format!("{}/{}", self.owner, self.name))
    }

    pub fn issue_url(&self, number: u64) -> String {
        format!("https://{}/{}/{}/issues/{number}", self.host, self.owner, self.name)
    }

    fn same(&self, host: &str, owner: &str, name: &str) -> bool {
        self.host.eq_ignore_ascii_case(host)
            && self.owner.eq_ignore_ascii_case(owner)
            && self.name.eq_ignore_ascii_case(name)
    }
}

/// The issue number in a key: 12 in `WEB-12`.
pub fn number(key: &str) -> Option<u64> {
    let (prefix, n) = key.rsplit_once('-')?;
    (!prefix.is_empty() && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
}

/// The key that `input` names when it's written the GitHub way: `#12`, `acme/web#12`, or an
/// issue's link. A link or reference to another repository, or to a pull request, is refused
/// with the reason. None for anything else (`12`, `WEB-12`), which reads as any tracker's key.
pub fn key_from_input(input: &str, prefix: &str, repo: &Repo) -> Option<Result<String, String>> {
    let s = input.trim();
    let key = |n: &str| match n.parse::<u64>() {
        Ok(n) if !prefix.is_empty() => Ok(format!("{}-{n}", prefix.to_uppercase())),
        Ok(_) => Err(format!("{s}: the project has no ticket prefix; set one in its settings")),
        Err(_) => Err(format!("{s} isn't an issue number")),
    };
    let digits = |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit());
    if let Some(n) = s.strip_prefix('#') {
        return digits(n).then(|| key(n));
    }
    // `owner/name#12`, as GitHub writes references to another repository.
    if !s.contains("://")
        && let Some((path, n)) = s.split_once('#')
        && let Some((owner, name)) = path.split_once('/')
        && digits(n)
        && !name.contains('/')
    {
        if repo.same(&repo.host, owner, name) {
            return Some(key(n));
        }
        return Some(Err(format!(
            "{s} is an issue of {path}; this project's issues are {}",
            repo.spec()
        )));
    }
    // A link: `https://github.com/acme/web/issues/12`, with or without the scheme.
    let rest = s.split_once("://").map_or(s, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    let [host, owner, name, kind, n, ..] = parts[..] else { return None };
    if !digits(n) || !host.contains('.') {
        return None;
    }
    let other = !repo.same(host, owner, name);
    let theirs = Repo::site(host, &format!("{owner}/{name}"));
    match kind {
        "pull" | "pulls" => {
            Some(Err(format!("{theirs}#{n} is a pull request; add the issue it closes instead")))
        }
        "issues" if other => Some(Err(format!(
            "that's an issue of {theirs}; this project's issues are {}",
            repo.spec()
        ))),
        "issues" => Some(key(n)),
        _ => None,
    }
}

/// How gh is run: the real one in the project folder, or, in tests, recorded output.
#[derive(Clone)]
enum Runner {
    Gh(PathBuf),
    #[cfg(test)]
    Replay(std::sync::Arc<std::sync::Mutex<tests::Recording>>),
}

#[derive(Clone)]
pub struct GithubConfig {
    pub repo: Repo,
    runner: Runner,
}

const ISSUE: &str = "query($owner: String!, $name: String!, $number: Int!, $comments: Int!) {
  repository(owner: $owner, name: $name) {
    issueOrPullRequest(number: $number) {
      __typename
      ... on Issue {
        title body state stateReason
        labels(first: 20) { nodes { name } }
        assignees(first: 10) { nodes { login } }
        comments(last: $comments) { totalCount nodes { id author { login } body createdAt } }
        closedByPullRequestsReferences(first: 10, includeClosedPrs: true) {
          nodes { number state url }
        }
      }
    }
  }
}";

const OPEN: &str = "query($owner: String!, $name: String!) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    issues(first: 100, states: OPEN, orderBy: { field: UPDATED_AT, direction: DESC }) {
      nodes { number title assignees(first: 10) { nodes { login } } }
    }
  }
}";

impl GithubConfig {
    /// The client for the repository `site` names, running gh in the project folder `dir`.
    pub fn new(site: &str, dir: &Path) -> Option<Self> {
        Some(Self { repo: Repo::parse(site)?, runner: Runner::Gh(dir.to_path_buf()) })
    }

    fn query(&self, query: &str, vars: &[(&str, String)]) -> Result<Value, FetchError> {
        let mut args: Vec<String> = vec!["api".into(), "graphql".into()];
        args.extend(["--hostname".into(), self.repo.host.clone()]);
        // -f passes text as it is; -F reads numbers (and would read a file for a value starting
        // with @, which a number never does).
        args.extend(["-f".into(), format!("owner={}", self.repo.owner)]);
        args.extend(["-f".into(), format!("name={}", self.repo.name)]);
        for (k, v) in vars {
            args.extend(["-F".into(), format!("{k}={v}")]);
        }
        args.extend(["-f".into(), format!("query={query}")]);
        let out = self.gh(&args).map_err(|e| self.explain(&e))?;
        let v: Value =
            serde_json::from_str(&out).map_err(|e| FetchError::Other(format!("gh api: {e}")))?;
        if let Some(msg) = v["errors"][0]["message"].as_str() {
            return Err(self.explain(msg));
        }
        Ok(v["data"].clone())
    }

    fn gh(&self, args: &[String]) -> Result<String, String> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match &self.runner {
            // A safety net: unit tests never run gh, even through an App's background fetch.
            Runner::Gh(_) if cfg!(test) => Err("unit tests don't run gh".into()),
            Runner::Gh(dir) => github::gh(dir, &args),
            #[cfg(test)]
            Runner::Replay(r) => r.lock().unwrap().reply(&args),
        }
    }

    /// Opens an issue in the repository, keyed with `prefix`. The REST call, as GraphQL's needs
    /// the repository's id first.
    pub fn create(
        &self,
        prefix: &str,
        title: &str,
        description: &str,
    ) -> Result<Created, FetchError> {
        let mut args: Vec<String> = vec!["api".into(), "--hostname".into(), self.repo.host.clone()];
        args.extend(["-X".into(), "POST".into()]);
        args.push(format!("repos/{}/{}/issues", self.repo.owner, self.repo.name));
        // -f passes text as it is, a leading @ included.
        args.extend(["-f".into(), format!("title={title}")]);
        if !description.trim().is_empty() {
            args.extend(["-f".into(), format!("body={description}")]);
        }
        let place = format!("{}'s issues", self.repo.spec());
        let out = self.gh(&args).map_err(|e| {
            // gh says `HTTP 4xx` when GitHub refused it, and `error connecting` when it never got
            // there; anything else (a timeout, say) may have opened the issue.
            let lower = e.to_lowercase();
            let refused = lower.contains("http 4") || lower.contains("error connecting");
            let unrun = e == github::Gh::Missing.why()
                || e == github::Gh::LoggedOut.why()
                || e.starts_with("could not run gh");
            if refused || unrun {
                self.explain(&e)
            } else {
                self.explain(&e).map(|e| tracker::maybe_created(&e, &place))
            }
        })?;
        Ok(parse_created(&out, prefix, &place)?)
    }

    /// What a failed call means and what to do about it. gh's active account is the one used, and
    /// people often have two (work and personal), so access errors name the repository and host.
    /// gh's error text is all it says, so the kind is read from it.
    fn explain(&self, e: &str) -> FetchError {
        let (repo, host) = (self.repo.spec(), &self.repo.host);
        let lower = e.to_lowercase();
        // gh's errors can run over lines ("error connecting to …", then advice); a panel shows
        // one line.
        let one_line =
            || e.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("; ");
        if lower.contains("could not resolve to an issue") {
            FetchError::NotFound(one_line())
        } else if lower.contains("could not resolve to a repository") || lower.contains("http 404")
        {
            FetchError::Other(format!(
                "gh can't see {repo} on {host}: it doesn't exist, or gh's active account can't reach it. Run gh auth switch -h {host} for another account, or gh auth refresh -h {host} -s repo for a private repository"
            ))
        } else if lower.contains("http 401") || lower.contains("bad credentials") {
            FetchError::Auth(format!(
                "{host} rejected gh's login; run gh auth login -h {host}, or gh auth switch"
            ))
        } else if lower.contains("scope") {
            FetchError::Auth(format!(
                "gh's token can't read {repo}'s issues; run gh auth refresh -h {host} -s repo"
            ))
        } else if lower.contains("saml") {
            FetchError::Auth(format!(
                "{repo}'s organization needs gh's token authorized for its single sign-on; run gh auth refresh -h {host}"
            ))
        } else if lower.contains("http 403") || lower.contains("resource not accessible") {
            FetchError::Auth(format!(
                "gh's active account can't read {repo}'s issues; run gh auth switch -h {host} for another account"
            ))
        } else if e == github::Gh::LoggedOut.why() {
            FetchError::Auth(e.to_string())
        } else if lower.contains("error connecting to") {
            FetchError::Unreachable(one_line())
        } else {
            FetchError::Other(one_line())
        }
    }

    /// Fetches the issue `key` names (its number), its body and comments' Markdown passed through
    /// `text` (`markdown_to_text` for a panel).
    pub fn fetch(&self, key: &str, text: fn(&str) -> String) -> Result<Issue, FetchError> {
        let repo = self.repo.spec();
        let n = number(key).ok_or_else(|| format!("{key} has no issue number"))?;
        let not_found =
            || FetchError::NotFound(format!("{key} not found: {repo} has no issue #{n}"));
        let d = self
            .query(ISSUE, &[("number", n.to_string()), ("comments", COMMENTS.to_string())])
            .map_err(|e| match e {
                FetchError::NotFound(_) => not_found(),
                e => e,
            })?;
        let node = &d["repository"]["issueOrPullRequest"];
        match node["__typename"].as_str() {
            Some("Issue") => Ok(parse_issue(node, text)),
            Some("PullRequest") => Err(FetchError::NotFound(format!(
                "{key} not found: #{n} in {repo} is a pull request, not an issue"
            ))),
            _ => Err(not_found()),
        }
    }

    /// The open issues, most recently updated first, keyed with `prefix`; `mine` when assigned
    /// to gh's account.
    pub fn open_issues(&self, prefix: &str) -> Result<Vec<Listed>, String> {
        parse_open(&self.query(OPEN, &[])?, prefix)
    }
}

fn parse_issue(i: &Value, text: fn(&str) -> String) -> Issue {
    let s = |x: &Value| x.as_str().unwrap_or("").trim().to_string();
    let names = |x: &Value, field: &str| -> Vec<String> {
        x["nodes"].as_array().into_iter().flatten().map(|n| s(&n[field])).collect()
    };
    let total = i["comments"]["totalCount"].as_u64().unwrap_or(0);
    // The newest COMMENTS, oldest first; the panel shows newest first.
    let mut comments: Vec<Comment> = i["comments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| Comment {
            id: s(&c["id"]),
            // A deleted account's comments have no author; GitHub shows them as ghost's.
            author: Some(s(&c["author"]["login"]))
                .filter(|a| !a.is_empty())
                .unwrap_or("ghost".into()),
            created: s(&c["createdAt"]),
            body: text(&s(&c["body"])),
        })
        .collect();
    comments.reverse();
    let left_out = usize::try_from(total).unwrap_or(usize::MAX).saturating_sub(comments.len());
    let (status, category) = match (s(&i["state"]).as_str(), s(&i["stateReason"]).as_str()) {
        ("CLOSED", "NOT_PLANNED") => ("Closed as not planned", "done"),
        ("CLOSED", "DUPLICATE") => ("Closed as duplicate", "done"),
        ("CLOSED", _) => ("Closed", "done"),
        _ => ("Open", "new"),
    };
    let assignees = names(&i["assignees"], "login");
    let prs = i["closedByPullRequestsReferences"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(LinkedPr {
                number: p["number"].as_u64()?,
                state: s(&p["state"]),
                url: s(&p["url"]),
            })
        })
        .collect();
    Issue {
        summary: s(&i["title"]),
        status: status.into(),
        status_category: category.into(),
        // Labels, as the Linear panel shows them.
        issue_type: names(&i["labels"], "name").join(", "),
        priority: String::new(),
        assignee: if assignees.is_empty() { "Unassigned".into() } else { assignees.join(", ") },
        description: text(&s(&i["body"])),
        comments,
        comments_left_out: left_out,
        branch: String::new(),
        prs,
    }
}

/// The issue `create` opened, from GitHub's answer; `place` is where it was opened.
fn parse_created(out: &str, prefix: &str, place: &str) -> Result<Created, String> {
    let v: Value = serde_json::from_str(out).unwrap_or(Value::Null);
    let key = v["number"].as_u64().map(|n| format!("{}-{n}", prefix.to_uppercase()));
    Created::read(key.as_deref(), v["html_url"].as_str().unwrap_or(""), place)
}

fn parse_open(d: &Value, prefix: &str) -> Result<Vec<Listed>, String> {
    let issues =
        d["repository"]["issues"]["nodes"].as_array().ok_or("GitHub returned no issues")?;
    let me = d["viewer"]["login"].as_str().unwrap_or("");
    let prefix = prefix.to_uppercase();
    Ok(issues
        .iter()
        .filter_map(|i| {
            Some(Listed {
                key: format!("{prefix}-{}", i["number"].as_u64()?),
                title: i["title"].as_str().unwrap_or("").to_string(),
                mine: !me.is_empty()
                    && i["assignees"]["nodes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|a| a["login"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(me))),
            })
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tracker::FetchError::{Auth, NotFound, Other, Unreachable};
    use crate::tracker::{Client, Text};
    use proptest::prelude::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// gh's recorded replies, served in order, and the arguments each call was given.
    pub struct Recording {
        replies: VecDeque<Result<String, String>>,
        pub calls: Vec<Vec<String>>,
    }

    impl Recording {
        pub(super) fn reply(&mut self, args: &[&str]) -> Result<String, String> {
            self.calls.push(args.iter().map(|a| a.to_string()).collect());
            self.replies.pop_front().unwrap_or_else(|| Err("no more recorded replies".into()))
        }
    }

    /// A client for `site` whose gh answers with `replies`; the recording keeps its calls.
    pub fn replay(
        site: &str,
        replies: Vec<Result<&str, &str>>,
    ) -> (GithubConfig, Arc<Mutex<Recording>>) {
        let rec = Arc::new(Mutex::new(Recording {
            replies: replies
                .into_iter()
                .map(|r| r.map(String::from).map_err(String::from))
                .collect(),
            calls: Vec::new(),
        }));
        let cfg =
            GithubConfig { repo: Repo::parse(site).unwrap(), runner: Runner::Replay(rec.clone()) };
        (cfg, rec)
    }

    const ISSUE_JSON: &str = include_str!("../tests/fixtures/github/issue.json");

    fn arg<'a>(call: &'a [String], name: &str) -> Option<&'a str> {
        call.iter().find_map(|a| a.strip_prefix(&format!("{name}=")))
    }

    #[test]
    fn reads_repositories() {
        let r = Repo::parse("acme/web").unwrap();
        assert_eq!(
            (r.host.as_str(), r.owner.as_str(), r.name.as_str()),
            ("github.com", "acme", "web")
        );
        assert_eq!(r.spec(), "acme/web");
        assert_eq!(r.issue_url(12), "https://github.com/acme/web/issues/12");
        let ghe = Repo::parse("git.acme.example/acme/web").unwrap();
        assert_eq!(ghe.spec(), "git.acme.example/acme/web");
        assert_eq!(ghe.issue_url(3), "https://git.acme.example/acme/web/issues/3");
        for bad in ["", "acme", "a/b/c/d", "acme/", "/web", "acme/we b"] {
            assert_eq!(Repo::parse(bad), None, "{bad}");
        }
        assert_eq!(Repo::site("github.com", "acme/web"), "acme/web");
        assert_eq!(Repo::site("git.acme.example", "acme/web"), "git.acme.example/acme/web");
    }

    #[test]
    fn reads_keys_written_the_github_way() {
        let repo = Repo::parse("acme/web").unwrap();
        let k = |s: &str| key_from_input(s, "web", &repo);
        assert_eq!(k("#12"), Some(Ok("WEB-12".into())));
        assert_eq!(k(" #12 "), Some(Ok("WEB-12".into())));
        assert_eq!(k("acme/web#12"), Some(Ok("WEB-12".into())));
        assert_eq!(k("Acme/Web#12"), Some(Ok("WEB-12".into())));
        assert_eq!(k("https://github.com/acme/web/issues/12"), Some(Ok("WEB-12".into())));
        assert_eq!(
            k("https://github.com/acme/web/issues/12#issuecomment-99"),
            Some(Ok("WEB-12".into()))
        );
        assert_eq!(k("github.com/acme/web/issues/12/"), Some(Ok("WEB-12".into())));
        // Another repository, or a pull request, is refused with the reason.
        assert_eq!(
            k("https://github.com/acme/api/issues/12"),
            Some(Err("that's an issue of acme/api; this project's issues are acme/web".into()))
        );
        assert_eq!(
            k("acme/api#12"),
            Some(Err(
                "acme/api#12 is an issue of acme/api; this project's issues are acme/web".into()
            ))
        );
        assert_eq!(
            k("https://github.com/acme/web/pull/45"),
            Some(Err("acme/web#45 is a pull request; add the issue it closes instead".into()))
        );
        assert_eq!(
            k("https://github.com/acme/web/pull/45/files"),
            Some(Err("acme/web#45 is a pull request; add the issue it closes instead".into()))
        );
        // The same repository on another host is another repository.
        assert!(matches!(k("https://git.acme.example/acme/web/issues/12"), Some(Err(_))));
        // Anything else reads as any tracker's key.
        for s in ["12", "WEB-12", "web-12", "#", "#x", "fix login", "https://github.com/acme/web"] {
            assert_eq!(k(s), None, "{s}");
        }
        assert!(matches!(key_from_input("#12", "", &repo), Some(Err(_))));
    }

    #[test]
    fn reads_numbers_from_keys() {
        assert_eq!(number("WEB-12"), Some(12));
        assert_eq!(number("A_1-7"), Some(7));
        assert_eq!(number("WEB-"), None);
        assert_eq!(number("12"), None);
        assert_eq!(number("WEB-1x"), None);
    }

    #[test]
    fn fetches_an_issue() {
        let (cfg, rec) = replay("acme/web", vec![Ok(ISSUE_JSON)]);
        let issue = Client::Github(cfg).fetch("WEB-12", Text::Plain).unwrap();
        assert_eq!(issue.summary, "Login fails with SSO");
        assert_eq!((issue.status.as_str(), issue.status_category.as_str()), ("Open", "new"));
        assert_eq!(issue.issue_type, "bug, auth");
        assert_eq!(issue.assignee, "ana-example, ben-example");
        assert_eq!(issue.description, "▍Steps\n• Sign in with SSO\n• See the redirect loop");
        assert_eq!(
            issue.comments.iter().map(|c| (c.id.as_str(), c.author.as_str())).collect::<Vec<_>>(),
            [("IC_2", "ghost"), ("IC_1", "ben-example")]
        );
        assert_eq!(issue.comments_left_out, 3);
        assert_eq!(issue.branch, "");
        assert_eq!(
            issue.prs.iter().map(|p| (p.number, p.state.as_str())).collect::<Vec<_>>(),
            [(45, "OPEN")]
        );
        // The saved repository, on its host, whatever the folder's remotes say.
        let calls = &rec.lock().unwrap().calls;
        assert_eq!(calls.len(), 1);
        assert_eq!(&calls[0][..4], ["api", "graphql", "--hostname", "github.com"]);
        assert_eq!(arg(&calls[0], "owner"), Some("acme"));
        assert_eq!(arg(&calls[0], "name"), Some("web"));
        assert_eq!(arg(&calls[0], "number"), Some("12"));
        assert_eq!(arg(&calls[0], "comments"), Some(COMMENTS.to_string().as_str()));
    }

    #[test]
    fn keeps_markdown_for_status() {
        let (cfg, _) = replay("acme/web", vec![Ok(ISSUE_JSON)]);
        let issue = Client::Github(cfg).fetch("WEB-12", Text::Markdown).unwrap();
        assert!(
            issue.description.starts_with("## Steps\n- Sign in with **SSO**"),
            "{}",
            issue.description
        );
    }

    #[test]
    fn says_why_an_issue_is_missing() {
        let fetch = |reply: Result<&str, &str>| {
            let (cfg, _) = replay("acme/web", vec![reply]);
            cfg.fetch("WEB-45", |s| s.to_string()).unwrap_err()
        };
        assert_eq!(
            fetch(Ok(include_str!("../tests/fixtures/github/pull-request.json"))),
            NotFound("WEB-45 not found: #45 in acme/web is a pull request, not an issue".into())
        );
        let no_issue = NotFound("WEB-45 not found: acme/web has no issue #45".into());
        assert_eq!(fetch(Ok(include_str!("../tests/fixtures/github/no-issue.json"))), no_issue);
        // What gh prints when the answer has errors.
        assert_eq!(
            fetch(Err(
                "GraphQL: Could not resolve to an issue or pull request with the number of 45. (repository.issueOrPullRequest)"
            )),
            no_issue
        );
        assert_eq!(
            fetch(Ok(include_str!("../tests/fixtures/github/no-repository.json"))),
            Other("gh can't see acme/web on github.com: it doesn't exist, or gh's active account can't reach it. Run gh auth switch -h github.com for another account, or gh auth refresh -h github.com -s repo for a private repository".into())
        );
        assert_eq!(
            fetch(Err("HTTP 401: Bad credentials (https://api.github.com/graphql)")),
            Auth("github.com rejected gh's login; run gh auth login -h github.com, or gh auth switch".into())
        );
        assert_eq!(
            fetch(Err(
                "GraphQL: Your token has not been granted the required scopes to execute this query."
            )),
            Auth("gh's token can't read acme/web's issues; run gh auth refresh -h github.com -s repo".into())
        );
        assert_eq!(
            fetch(Err("error connecting to github.com\ncheck your internet connection")),
            Unreachable("error connecting to github.com; check your internet connection".into())
        );
        // Being logged out, or gh missing, is said as the PR panel says it.
        assert_eq!(
            fetch(Err(github::Gh::LoggedOut.why())),
            Auth(github::Gh::LoggedOut.why().into())
        );
        assert_eq!(fetch(Err(github::Gh::Missing.why())), Other(github::Gh::Missing.why().into()));
        let (cfg, _) = replay("acme/web", vec![]);
        assert_eq!(
            cfg.fetch("WEB", |s| s.to_string()).unwrap_err(),
            Other("WEB has no issue number".into())
        );
    }

    #[test]
    fn opens_an_issue_in_the_saved_repository() {
        let (cfg, rec) = replay(
            "git.acme.example/acme/web",
            vec![Ok(include_str!("../tests/fixtures/github/created.json"))],
        );
        let made = Client::Github(cfg).create("web", "@Fix the signup form", "Steps").unwrap();
        assert_eq!(
            (made.key.as_str(), made.at.as_str()),
            ("WEB-16", "https://github.com/acme/web/issues/16")
        );
        let calls = &rec.lock().unwrap().calls;
        assert_eq!(
            calls[0],
            [
                "api",
                "--hostname",
                "git.acme.example",
                "-X",
                "POST",
                "repos/acme/web/issues",
                "-f",
                "title=@Fix the signup form",
                "-f",
                "body=Steps"
            ]
        );
    }

    #[test]
    fn says_why_it_couldnt_open_an_issue_and_whether_it_may_exist() {
        let create = |reply: Result<&str, &str>| {
            let (cfg, rec) = replay("acme/web", vec![reply, reply]);
            let e = Client::Github(cfg).create("WEB", "Fix it", "").unwrap_err().message;
            assert_eq!(rec.lock().unwrap().calls.len(), 1, "asked again");
            e
        };
        let maybe = "may have been created anyway: look in acme/web's issues";
        assert_eq!(
            create(Err("Issues are disabled for this repo (HTTP 410)")),
            "Issues are disabled for this repo (HTTP 410)"
        );
        assert_eq!(
            create(Err("HTTP 401: Bad credentials (https://api.github.com/repos/acme/web/issues)")),
            "github.com rejected gh's login; run gh auth login -h github.com, or gh auth switch"
        );
        assert!(!create(Err("error connecting to github.com")).contains(maybe));
        assert_eq!(create(Err(github::Gh::Missing.why())), github::Gh::Missing.why());
        for e in [
            create(Err("gh api --hostname: timed out after 30s; the next refresh tries again")),
            create(Err("HTTP 502: Bad Gateway")),
            create(Ok("{}")),
            create(Ok("not json")),
        ] {
            assert!(e.contains(maybe), "{e}");
        }
    }

    #[test]
    fn lists_open_issues_mine_first() {
        let (cfg, rec) = replay(
            "git.acme.example/acme/web",
            vec![Ok(include_str!("../tests/fixtures/github/open-issues.json"))],
        );
        let open = Client::Github(cfg).open_issues("web").unwrap();
        assert_eq!(
            open.iter().map(|l| (l.key.as_str(), l.mine)).collect::<Vec<_>>(),
            [("WEB-9", true), ("WEB-14", true), ("WEB-15", false), ("WEB-3", false)]
        );
        assert_eq!(open[0].title, "Crash on start");
        assert_eq!(&rec.lock().unwrap().calls[0][2..4], ["--hostname", "git.acme.example"]);
    }

    /// A worktree's or the folder's origin can be a fork: every call names the repository the
    /// project saved, and the PR lookup views the PRs that close an issue there too.
    #[test]
    fn asks_the_saved_repository_whatever_the_folders_remotes_say() {
        let dir = crate::store::state_dir().join("fork");
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git").args(args).current_dir(&dir).status();
            assert!(ok.unwrap().success(), "{args:?}");
        };
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@github.com:ana-example/web.git"]);
        let t: crate::store::Tracker = serde_json::from_value(serde_json::json!({
            "provider": "github", "site": "acme/web", "key": "WEB",
        }))
        .unwrap();
        let Some(Client::Github(c)) = Client::new(&t, &dir, None) else { panic!("no client") };
        assert_eq!(c.repo.spec(), "acme/web");
        let closes = t.closes("WEB-12").unwrap();
        assert_eq!((closes.repo.spec(), closes.number), ("acme/web".to_string(), 12));
        assert_eq!(t.issue_url("WEB-12").as_deref(), Some("https://github.com/acme/web/issues/12"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn input_parsing_takes_any_text(s in "(\\PC|[#/:.0-9])*") {
            let repo = Repo::parse("acme/web").unwrap();
            if let Some(Ok(key)) = key_from_input(&s, "WEB", &repo) {
                prop_assert!(key.starts_with("WEB-"));
                prop_assert!(number(&key).is_some());
            }
        }

        #[test]
        fn only_a_missing_issue_is_not_found(
            lead in "\\PC{0,20}",
            said in prop::sample::select(vec!["", " not found", "Could not resolve to an issue", "error connecting to github.com"]),
            tail in "\\PC{0,20}",
        ) {
            let (cfg, _) = replay("acme/web", vec![]);
            let gh = format!("{lead}{said}{tail}");
            let lower = gh.to_lowercase();
            match cfg.explain(&gh) {
                NotFound(_) => prop_assert!(lower.contains("could not resolve to an issue"), "{gh:?}"),
                Unreachable(_) => prop_assert!(lower.contains("error connecting to"), "{gh:?}"),
                Auth(m) | Other(m) => prop_assert!(!m.is_empty() || gh.trim().is_empty(), "{gh:?}"),
            }
        }

        #[test]
        fn issue_links_give_their_number(n in 1u64..1_000_000, slash in proptest::bool::ANY) {
            let repo = Repo::parse("acme/web").unwrap();
            let url = format!("https://github.com/acme/web/issues/{n}{}", if slash { "/" } else { "" });
            prop_assert_eq!(key_from_input(&url, "web", &repo), Some(Ok(format!("WEB-{n}"))));
            prop_assert_eq!(key_from_input(&format!("#{n}"), "web", &repo), Some(Ok(format!("WEB-{n}"))));
        }

        #[test]
        fn parsing_takes_any_json(v in prop::collection::btree_map(
            prop::sample::select(vec!["title", "body", "state", "stateReason", "labels", "assignees", "comments", "closedByPullRequestsReferences"]),
            prop_oneof![
                Just(serde_json::Value::Null),
                any::<String>().prop_map(serde_json::Value::from),
                any::<i64>().prop_map(serde_json::Value::from),
                Just(serde_json::json!({ "nodes": [{ "name": 1, "login": null, "number": "x" }], "totalCount": -3 })),
            ],
            0..8,
        )) {
            let v = serde_json::to_value(v).unwrap();
            let issue = parse_issue(&v, |s| s.to_string());
            prop_assert!(!issue.status.is_empty());
            let _ = parse_open(&serde_json::json!({ "repository": { "issues": { "nodes": [v.clone()] } } }), "WEB");
            if let Ok(made) = parse_created(&v.to_string(), "web", "acme/web") {
                prop_assert!(made.key.starts_with("WEB-"));
            }
        }

        #[test]
        fn a_created_issue_is_keyed_by_its_number(n in 1u64..1_000_000, url in "\\PC{0,30}") {
            let out = serde_json::json!({ "number": n, "html_url": url }).to_string();
            let made = parse_created(&out, "web", "acme/web").unwrap();
            prop_assert_eq!((made.key, made.at), (format!("WEB-{n}"), url));
        }
    }
}
