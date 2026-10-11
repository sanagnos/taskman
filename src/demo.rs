//! Demo mode (`TASKMAN_DEMO=<dir>`): tracker issues and pull requests come from JSON fixtures in
//! that folder instead of Jira, Linear and GitHub, so demos and screenshots show made-up data and
//! need no accounts. Claude sessions are still real. `demo/fixtures` has examples.
//!
//! `<KEY>.issue.json` and `<KEY>.pr.json` per ticket; a missing PR file means "no PR yet". Times are
//! given as ages (`"12m"`, `"3h"`, `"2d"`) so the fixtures never go stale.

use crate::github::{Check, CheckState, Gh, Lookup, NO_REMOTE, Pr, PrComment, Review};
use crate::tracker::{Comment, Issue};
use crate::util::{iso_from_unix, now};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub fn dir() -> Option<PathBuf> {
    std::env::var_os("TASKMAN_DEMO").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

/// `"12m"` → the unix time 12 minutes ago. Also `s`, `h`, `d`; anything else means now.
fn ago(s: &str) -> i64 {
    let (n, unit) = s.trim().split_at(s.trim().len().saturating_sub(1));
    let n: i64 = n.parse().unwrap_or(0);
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => 0,
    };
    now() - secs
}

#[derive(Deserialize)]
struct IssueFile {
    summary: String,
    #[serde(default)]
    status: String,
    /// "new", "indeterminate" (in progress) or "done": picks the status colour.
    #[serde(default)]
    status_category: String,
    #[serde(default)]
    issue_type: String,
    #[serde(default)]
    priority: String,
    #[serde(default)]
    assignee: String,
    #[serde(default)]
    description: String,
    /// The tracker's branch name (Linear's `branchName`).
    #[serde(default)]
    branch: String,
    /// Newest first.
    #[serde(default)]
    comments: Vec<CommentFile>,
}

#[derive(Deserialize)]
struct CommentFile {
    id: String,
    author: String,
    ago: String,
    body: String,
}

#[derive(Deserialize)]
struct PrFile {
    number: u64,
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default = "open")]
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    review_decision: String,
    #[serde(default)]
    mergeable: String,
    #[serde(default)]
    head: String,
    /// The branch it merges into; a stacked ticket's PR names its base's branch.
    #[serde(default = "default_base")]
    base: String,
    #[serde(default)]
    checks: Vec<CheckFile>,
    #[serde(default)]
    reviews: Vec<ReviewFile>,
    #[serde(default)]
    comments: Vec<CommentFile>,
}

fn open() -> String {
    "OPEN".into()
}

fn default_base() -> String {
    "main".into()
}

#[derive(Deserialize)]
struct CheckFile {
    name: String,
    /// "pass", "fail", "pending" or "skipped".
    state: String,
    #[serde(default)]
    ago: Option<String>,
}

#[derive(Deserialize)]
struct ReviewFile {
    id: String,
    author: String,
    state: String,
    ago: String,
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("demo: can't read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("demo: {} isn't valid: {e}", path.display()))
}

pub fn issue(dir: &Path, key: &str) -> Result<Issue, String> {
    let f: IssueFile = read(&dir.join(format!("{key}.issue.json")))?;
    Ok(Issue {
        summary: f.summary,
        status: f.status,
        status_category: f.status_category,
        issue_type: f.issue_type,
        priority: f.priority,
        assignee: if f.assignee.is_empty() { "Unassigned".into() } else { f.assignee },
        description: f.description,
        branch: f.branch,
        comments: f
            .comments
            .into_iter()
            .map(|c| Comment {
                id: c.id,
                author: c.author,
                created: iso_from_unix(ago(&c.ago)),
                body: c.body,
            })
            .collect(),
        comments_left_out: 0,
        prs: Vec::new(),
    })
}

/// Whether `folder` shows PRs, without asking gh: any git remote will do, since they come from the
/// fixtures. A demo project without a remote shows how taskman looks with no GitHub.
pub fn probe(folder: &Path) -> Gh {
    match std::process::Command::new("git").arg("-C").arg(folder).arg("remote").output() {
        Ok(out) if out.status.success() && !out.stdout.trim_ascii().is_empty() => Gh::Ready,
        Ok(out) if out.status.success() => Gh::NoRemote,
        _ => Gh::NotGit,
    }
}

pub fn pr(dir: &Path, repo: &Path, key: &str) -> Result<Lookup, String> {
    if probe(repo) != Gh::Ready {
        return Err(NO_REMOTE.into());
    }
    let path = dir.join(format!("{key}.pr.json"));
    if !path.exists() {
        return Ok(Lookup { pr: None, closed: None, branch_gone: false });
    }
    let f: PrFile = read(&path)?;
    let mut checks: Vec<Check> = f
        .checks
        .into_iter()
        .map(|c| Check {
            name: c.name,
            state: match c.state.as_str() {
                "fail" => CheckState::Fail,
                "pending" => CheckState::Pending,
                "skipped" => CheckState::Skipped,
                _ => CheckState::Pass,
            },
            when: c.ago.as_deref().map(ago),
            url: String::new(),
        })
        .collect();
    checks.sort_by(|a, b| a.state.cmp(&b.state).then(a.name.cmp(&b.name)));
    let pr = Pr {
        number: f.number,
        title: f.title,
        url: f.url,
        state: f.state,
        is_draft: f.draft,
        review_decision: f.review_decision,
        mergeable: f.mergeable,
        head: f.head,
        head_oid: String::new(),
        base: f.base,
        checks,
        reviews: f
            .reviews
            .into_iter()
            .map(|r| Review {
                id: r.id,
                author: r.author,
                state: r.state,
                when: iso_from_unix(ago(&r.ago)),
                ..Default::default()
            })
            .collect(),
        comments: f
            .comments
            .into_iter()
            .map(|c| PrComment {
                id: c.id,
                author: c.author,
                when: iso_from_unix(ago(&c.ago)),
                body: c.body,
                url: String::new(),
            })
            .collect(),
        threads: Ok(Default::default()),
    };
    Ok(Lookup { pr: Some(pr), closed: None, branch_gone: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_example_fixtures() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("demo/fixtures");
        let i = issue(&dir, "ENG-52").unwrap();
        assert!(!i.summary.is_empty() && !i.comments.is_empty());
        let p = pr(&dir, &dir, "ENG-52").unwrap().pr.unwrap();
        assert!(p.count(CheckState::Fail) > 0);
        assert!(pr(&dir, &dir, "NO-1").unwrap().pr.is_none());
        assert_eq!(p.base, "main");
        assert_eq!(
            pr(&dir, &dir, "ENG-54").unwrap().pr.unwrap().base,
            "feat/ENG-53-webhook-retries"
        );
    }

    #[test]
    fn ages_count_back_from_now() {
        assert_eq!(now() - ago("12m"), 720);
        assert_eq!(now() - ago("2d"), 172800);
    }
}
