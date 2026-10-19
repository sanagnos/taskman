//! Linear GraphQL client: one issue with its comments, in the same shape as the other trackers.

use crate::tracker::{
    self, CHOICE_PAGES, COMMENTS, Choice, Comment, Created, FetchError, Issue, Listed,
};
use serde_json::{Value, json};
use std::time::Duration;

const ENDPOINT: &str = "https://api.linear.app/graphql";

#[derive(Clone)]
pub struct LinearConfig {
    endpoint: String,
    key: String,
    agent: ureq::Agent,
}

impl LinearConfig {
    /// `key` is a personal API key, which belongs to one workspace.
    pub fn new(key: &str) -> Self {
        Self::at(ENDPOINT, key)
    }

    /// A client for the API at `endpoint`: Linear's, or a test's recorded responses.
    fn at(endpoint: &str, key: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .build()
            .into();
        Self { endpoint: endpoint.to_string(), key: key.to_string(), agent }
    }

    fn query(&self, query: &str, variables: &Value) -> Result<Value, FetchError> {
        let (status, v) = self.send(query, variables).map_err(|e| failed(&e))?;
        answer(status, &v)
    }

    /// Posts a query; its status and body.
    fn send(&self, query: &str, variables: &Value) -> Result<(u16, Value), ureq::Error> {
        // A safety net: unit tests reach only `replay`'s server, even through an App's background
        // fetch.
        if cfg!(test) && !self.endpoint.starts_with("http://127.0.0.1:") {
            return Err(ureq::Error::ConnectionFailed);
        }
        let mut resp = self
            .agent
            .post(&self.endpoint)
            // Personal API keys go in as-is; only OAuth tokens take "Bearer".
            .header("Authorization", &self.key)
            .send_json(json!({ "query": query, "variables": variables }))?;
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().read_json().unwrap_or(Value::Null)))
    }

    /// Checks the key; returns the user's name and the workspace it belongs to (its URL key).
    pub fn whoami(&self) -> Result<(String, String), String> {
        let d = self
            .query("query { viewer { name displayName } organization { urlKey } }", &json!({}))?;
        let name = [&d["viewer"]["displayName"], &d["viewer"]["name"]]
            .into_iter()
            .find_map(|n| n.as_str().filter(|n| !n.is_empty()))
            .unwrap_or("?")
            .to_string();
        Ok((name, d["organization"]["urlKey"].as_str().unwrap_or("").to_string()))
    }

    /// The workspace's teams the key can see, a page at a time, up to `CHOICE_PAGES` pages.
    pub fn teams(&self) -> Result<Vec<Choice>, String> {
        let mut teams = Vec::new();
        let mut after = Value::Null;
        for _ in 0..CHOICE_PAGES {
            let d = self.query(TEAMS, &json!({ "after": after }))?;
            let (got, next) = parse_teams(&d);
            teams.extend(got);
            match next {
                Some(cursor) => after = Value::String(cursor),
                None => break,
            }
        }
        Ok(teams)
    }

    /// The open issues of the team whose key is `prefix` (not in a completed or canceled state),
    /// the 100 most recently updated; `mine` when assigned to the key's user.
    pub fn open_issues(&self, prefix: &str) -> Result<Vec<Listed>, String> {
        let team = prefix.trim().to_uppercase();
        let d = self.query(OPEN, &json!({ "team": team }))?;
        Ok(parse_open(&d))
    }

    /// Creates an issue in the team whose key is `prefix`, in the state the team starts issues in.
    pub fn create(
        &self,
        prefix: &str,
        title: &str,
        description: &str,
    ) -> Result<Created, FetchError> {
        let team = prefix.trim().to_uppercase();
        let d = self.query(TEAM, &json!({ "team": team }))?;
        let Some(id) = d["teams"]["nodes"][0]["id"].as_str() else {
            return Err(format!("Linear has no team {team}, or this API key can't see it").into());
        };
        let mut input = json!({ "teamId": id, "title": title });
        if !description.trim().is_empty() {
            input["description"] = description.into();
        }
        let place = format!("team {team} in Linear");
        let maybe = |e: &str| tracker::maybe_created(e, &place);
        let (status, v) = self.send(CREATE, &json!({ "input": input })).map_err(|e| {
            let e2 = failed(&e);
            if tracker::unsent(&e) { e2 } else { e2.map(|e| maybe(&e)) }
        })?;
        // A server failing is the one answer that doesn't say the issue wasn't made.
        if status >= 500 {
            return Err(maybe(&format!("Linear answered HTTP {status}")).into());
        }
        let d = answer(status, &v)?;
        Ok(parse_created(&d, &place)?)
    }
}

/// A request that got no status back.
fn failed(e: &ureq::Error) -> FetchError {
    FetchError::request(e, format!("Linear request failed: {e}"))
}

/// What Linear's answer to a query holds, or why it failed. Linear answers a missing entity with
/// an error saying "Entity not found", an `INVALID_INPUT` like any other bad variable.
fn answer(status: u16, v: &Value) -> Result<Value, FetchError> {
    if status == 401 || status == 403 || errors_mention(v, "authentication") {
        return Err(FetchError::Auth("Linear rejected the API key".into()));
    }
    if let Some(msg) = v["errors"][0]["message"].as_str() {
        if msg.starts_with("Entity not found") {
            return Err(FetchError::NotFound(msg.to_string()));
        }
        return Err(FetchError::Other(msg.to_string()));
    }
    if status >= 400 {
        return Err(FetchError::Other(format!("Linear request failed: HTTP {status}")));
    }
    Ok(v["data"].clone())
}

/// Linear orders by `updatedAt` newest first.
const OPEN: &str = r#"query Open($team: String!) {
  viewer { id }
  issues(
    first: 100
    orderBy: updatedAt
    filter: { team: { key: { eq: $team } }, state: { type: { nin: ["completed", "canceled"] } } }
  ) {
    nodes { identifier title assignee { id } }
  }
}"#;

const TEAMS: &str = r#"query Teams($after: String) {
  teams(first: 100, after: $after) {
    nodes { key name }
    pageInfo { hasNextPage endCursor }
  }
}"#;

const TEAM: &str = r#"query Team($team: String!) {
  teams(filter: { key: { eq: $team } }) { nodes { id } }
}"#;

const CREATE: &str = r#"mutation Create($input: IssueCreateInput!) {
  issueCreate(input: $input) { success issue { identifier url } }
}"#;

/// The issue `CREATE` made, from its answer; `place` is where it was made.
fn parse_created(d: &Value, place: &str) -> Result<Created, String> {
    let issue = &d["issueCreate"]["issue"];
    if d["issueCreate"]["success"] == false && issue.is_null() {
        return Err("Linear didn't create the issue".into());
    }
    Created::read(issue["identifier"].as_str(), issue["url"].as_str().unwrap_or(""), place)
}

/// A page of teams, and the cursor of the next page if there is one.
fn parse_teams(d: &Value) -> (Vec<Choice>, Option<String>) {
    let teams = d["teams"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(Choice {
                key: t["key"].as_str()?.to_string(),
                name: t["name"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect();
    let page = &d["teams"]["pageInfo"];
    let next = page["endCursor"].as_str().filter(|_| page["hasNextPage"] == true).map(String::from);
    (teams, next)
}

fn parse_open(d: &Value) -> Vec<Listed> {
    let me = d["viewer"]["id"].as_str().unwrap_or("");
    d["issues"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            Some(Listed {
                key: i["identifier"].as_str()?.to_string(),
                title: i["title"].as_str().unwrap_or("").to_string(),
                mine: !me.is_empty() && i["assignee"]["id"] == me,
            })
        })
        .collect()
}

fn errors_mention(v: &Value, word: &str) -> bool {
    v["errors"].as_array().is_some_and(|es| {
        es.iter().any(|e| {
            e["message"].as_str().is_some_and(|m| m.to_lowercase().contains(word))
                || e["extensions"]["code"].as_str().is_some_and(|c| c.to_lowercase().contains(word))
        })
    })
}

const ISSUE: &str = r#"query Issue($id: String!, $comments: Int!) {
  issue(id: $id) {
    identifier title description priorityLabel branchName
    state { name type }
    assignee { name displayName }
    labels { nodes { name } }
    comments(first: $comments) {
      nodes { id body createdAt user { name displayName } }
      pageInfo { hasNextPage endCursor }
    }
  }
}"#;

/// Counts the comments after `after`, ids only, to say how many weren't fetched.
const OLDER: &str = r#"query Older($id: String!, $after: String!) {
  issue(id: $id) { comments(first: 250, after: $after) { nodes { id } pageInfo { hasNextPage endCursor } } }
}"#;

/// Pages counted past the fetched comments; more than that many and the count is a lower bound.
const MAX_PAGES: usize = 40;

/// Fetches an issue by its identifier, like `ENG-123`, its description and comments' Markdown
/// passed through `text` (`markdown_to_text` for a panel).
pub fn fetch(cfg: &LinearConfig, key: &str, text: fn(&str) -> String) -> Result<Issue, FetchError> {
    let not_found = || FetchError::NotFound(format!("{key} not found, or not visible to this key"));
    let d = cfg.query(ISSUE, &json!({ "id": key, "comments": COMMENTS })).map_err(|e| match e {
        FetchError::NotFound(_) => not_found(),
        e => e,
    })?;
    if d["issue"].is_null() {
        return Err(not_found());
    }
    let mut issue = parse_issue(&d["issue"], text);
    // Linear lists comments newest first and has no count, so count the older pages.
    let mut page = d["issue"]["comments"]["pageInfo"].clone();
    for _ in 0..MAX_PAGES {
        let Some(after) = page["endCursor"].as_str().filter(|_| page["hasNextPage"] == true) else {
            break;
        };
        let d = cfg.query(OLDER, &json!({ "id": key, "after": after }))?;
        let older = &d["issue"]["comments"];
        issue.comments_left_out += older["nodes"].as_array().map_or(0, Vec::len);
        page = older["pageInfo"].clone();
    }
    Ok(issue)
}

fn parse_issue(i: &Value, text: fn(&str) -> String) -> Issue {
    let s = |x: &Value| x.as_str().unwrap_or("").trim().to_string();
    let person = |u: &Value| {
        let d = s(&u["displayName"]);
        if d.is_empty() { s(&u["name"]) } else { d }
    };
    let mut comments: Vec<Comment> = i["comments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| Comment {
            id: s(&c["id"]),
            author: person(&c["user"]),
            created: s(&c["createdAt"]),
            body: text(&s(&c["body"])),
        })
        .collect();
    // Newest first, like the Jira panel.
    comments.sort_by(|a, b| b.created.cmp(&a.created));
    let category = match i["state"]["type"].as_str().unwrap_or("") {
        "completed" | "canceled" => "done",
        "started" => "indeterminate",
        _ => "new",
    };
    let labels: Vec<String> =
        i["labels"]["nodes"].as_array().into_iter().flatten().map(|l| s(&l["name"])).collect();
    let priority = s(&i["priorityLabel"]);
    let assignee = person(&i["assignee"]);
    Issue {
        summary: s(&i["title"]),
        status: s(&i["state"]["name"]),
        status_category: category.into(),
        issue_type: labels.join(", "),
        priority: if priority == "No priority" { String::new() } else { priority },
        assignee: if assignee.is_empty() { "Unassigned".into() } else { assignee },
        description: text(&s(&i["description"])),
        comments,
        comments_left_out: 0,
        branch: s(&i["branchName"]),
        prs: Vec::new(),
    }
}

/// Light Markdown cleanup for a plain-text panel: headings, bullets, emphasis and links,
/// in the same style as Jira text (`▍` headings, `•` bullets).
pub fn markdown_to_text(md: &str) -> String {
    md.lines()
        .map(|line| {
            let indent = line.len() - line.trim_start().len();
            let t = line.trim_start();
            let t = if let Some(h) = t.strip_prefix('#') {
                format!("▍{}", h.trim_start_matches('#').trim_start())
            } else if let Some(b) = t
                .strip_prefix("* ")
                .or_else(|| t.strip_prefix("- "))
                .or_else(|| t.strip_prefix("+ "))
            {
                format!("• {b}")
            } else {
                t.to_string()
            };
            format!("{}{}", " ".repeat(indent), inline(&t))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `[text](url)` → `text`, and drops `**`, `__` and backticks.
fn inline(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after
            .find("](")
            .and_then(|close| after[close + 2..].find(')').map(|end| (close, close + 2 + end)))
        {
            Some((close, end)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..close]);
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[..open + 1]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay;
    use crate::tracker::FetchError::{Auth, NotFound, Other, Unreachable};
    use crate::tracker::{Client, Text};
    use proptest::prelude::*;

    #[test]
    fn cleans_markdown() {
        assert_eq!(
            markdown_to_text(
                "## Goal\n* [Learn more](https://x.y/z) about **teams**\n  - nested `code`"
            ),
            "▍Goal\n• Learn more about teams\n  • nested code"
        );
        assert_eq!(markdown_to_text("a [b] c"), "a [b] c");
    }

    #[test]
    fn parses_an_issue() {
        let v = json!({
            "identifier": "ENG-7", "title": "Import CSV", "description": "Do it", "priorityLabel": "High", "branchName": "ana/eng-7-import-csv",
            "state": { "name": "In Progress", "type": "started" },
            "assignee": { "name": "ana", "displayName": "Ana" },
            "labels": { "nodes": [{ "name": "bug" }] },
            "comments": { "nodes": [
                { "id": "1", "body": "first", "createdAt": "2024-01-01T00:00:00Z", "user": { "name": "a", "displayName": "" } },
                { "id": "2", "body": "second", "createdAt": "2024-02-01T00:00:00Z", "user": null }
            ]}
        });
        let i = parse_issue(&v, markdown_to_text);
        assert_eq!(
            (i.summary.as_str(), i.status_category.as_str(), i.assignee.as_str()),
            ("Import CSV", "indeterminate", "Ana")
        );
        assert_eq!(i.comments[0].id, "2");
        assert_eq!(i.comments[1].author, "a");
        assert_eq!(i.issue_type, "bug");
        assert_eq!(i.branch, "ana/eng-7-import-csv");
        let md =
            parse_issue(&json!({ "description": "See [docs](https://x.y) and `code`" }), |m| {
                m.to_string()
            });
        assert_eq!(md.description, "See [docs](https://x.y) and `code`");
    }

    // Recorded responses (tests/fixtures/linear), served locally through the whole fetch.

    fn client(server: &replay::Server) -> Client {
        Client::Linear(LinearConfig::at(&server.url, "lin_api_mock"))
    }

    #[test]
    fn fetches_an_issue_and_counts_older_comments() {
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/linear/issue.json")),
            (200, include_str!("../tests/fixtures/linear/older-1.json")),
            (200, include_str!("../tests/fixtures/linear/older-2.json")),
        ]);
        let issue = client(&server).fetch("ENG-52", Text::Plain).unwrap();
        assert_eq!(
            (issue.summary.as_str(), issue.status_category.as_str()),
            ("Fix the login page", "indeterminate")
        );
        assert_eq!((issue.issue_type.as_str(), issue.priority.as_str()), ("Bug, Auth", "High"));
        assert_eq!(
            (issue.assignee.as_str(), issue.branch.as_str()),
            ("Ana Example", "ana/eng-52-fix-the-login-page")
        );
        assert_eq!(
            issue.description,
            "▍Steps\n• Sign in with SSO\n• See the redirect loop\n\nRuns GET /login."
        );
        assert_eq!(
            issue.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["c-2", "c-1"]
        );
        assert_eq!(issue.comments[1].body, "Seen on staging.");
        assert_eq!(issue.comments_left_out, 5);

        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|r| r.line == "POST / HTTP/1.1"));
        // A personal API key goes in as it is.
        assert_eq!(requests[0].header("authorization"), Some("lin_api_mock"));
        let vars: Vec<Value> = requests.iter().map(|r| r.json()["variables"].clone()).collect();
        assert_eq!(vars[0], json!({ "id": "ENG-52", "comments": COMMENTS }));
        assert_eq!(vars[1], json!({ "id": "ENG-52", "after": "cursor-1" }));
        assert_eq!(vars[2], json!({ "id": "ENG-52", "after": "cursor-2" }));
    }

    #[test]
    fn keeps_markdown_for_status() {
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/linear/issue.json")),
            (200, "{}"),
        ]);
        let issue = client(&server).fetch("ENG-52", Text::Markdown).unwrap();
        assert!(
            issue.description.starts_with("## Steps\n* Sign in with **SSO**\n"),
            "{}",
            issue.description
        );
        assert_eq!(issue.comments[1].body, "Seen on **staging**.");
    }

    #[test]
    fn says_what_went_wrong() {
        let fetch = |status, body| {
            let server = replay::serve(vec![(status, body)]);
            client(&server).fetch("ENG-52", Text::Plain).unwrap_err()
        };
        let not_found = NotFound("ENG-52 not found, or not visible to this key".into());
        assert_eq!(fetch(200, include_str!("../tests/fixtures/linear/not-found.json")), not_found);
        assert_eq!(fetch(400, include_str!("../tests/fixtures/linear/not-found.json")), not_found);
        assert_eq!(fetch(200, include_str!("../tests/fixtures/linear/no-issue.json")), not_found);
        let rejected = Auth("Linear rejected the API key".into());
        assert_eq!(
            fetch(400, include_str!("../tests/fixtures/linear/unauthenticated.json")),
            rejected
        );
        assert_eq!(fetch(401, ""), rejected);
        assert_eq!(
            fetch(500, "<html>down</html>"),
            Other("Linear request failed: HTTP 500".into())
        );
        assert_eq!(
            fetch(400, r#"{"errors":[{"message":"Variable \"$id\" got invalid value"}]}"#),
            Other("Variable \"$id\" got invalid value".into())
        );
        // Words in an error that isn't Linear's for a missing issue don't make it one.
        assert_eq!(
            fetch(400, r#"{"errors":[{"message":"Team not found in this query"}]}"#),
            Other("Team not found in this query".into())
        );
        // Counting the older comments fails part-way.
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/linear/issue.json")),
            (502, ""),
        ]);
        assert_eq!(
            client(&server).fetch("ENG-52", Text::Plain).unwrap_err(),
            Other("Linear request failed: HTTP 502".into())
        );
        // Nothing listens.
        let down = LinearConfig::at(&replay::closed(), "lin_api_mock");
        let e = super::fetch(&down, "ENG-52", markdown_to_text).unwrap_err();
        assert!(matches!(&e, Unreachable(m) if m.starts_with("Linear request failed: ")), "{e:?}");
    }

    #[test]
    fn creates_an_issue_in_the_prefixs_team() {
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/linear/team.json")),
            (200, include_str!("../tests/fixtures/linear/created.json")),
        ]);
        let made = client(&server).create("eng", "Fix the signup form", "See **this**").unwrap();
        assert_eq!(
            (made.key.as_str(), made.at.as_str()),
            ("ENG-53", "https://linear.app/acme/issue/ENG-53/fix-the-signup-form")
        );
        let requests = server.requests();
        assert_eq!(requests[0].json()["variables"], json!({ "team": "ENG" }));
        // No state: the team's default applies. The description goes as Markdown.
        assert_eq!(
            requests[1].json()["variables"],
            json!({ "input": {
                "teamId": "6b1f0c2e-team-eng",
                "title": "Fix the signup form",
                "description": "See **this**",
            } })
        );
        assert!(requests[1].json()["query"].as_str().unwrap().contains("issueCreate"));
    }

    #[test]
    fn says_why_it_couldnt_create_and_whether_the_issue_may_exist() {
        let team = include_str!("../tests/fixtures/linear/team.json");
        let create = |replies: Vec<(u16, &'static str)>| {
            let server = replay::serve(replies);
            let e = client(&server).create("ENG", "Fix it", "").unwrap_err().message;
            (e, server.requests().len())
        };
        assert_eq!(
            create(vec![(200, include_str!("../tests/fixtures/linear/no-team.json"))]),
            ("Linear has no team ENG, or this API key can't see it".into(), 1)
        );
        assert_eq!(
            create(vec![(200, team), (400, r#"{"errors":[{"message":"title too long"}]}"#)]),
            ("title too long".into(), 2)
        );
        assert_eq!(
            create(vec![(200, team), (200, r#"{"data":{"issueCreate":{"success":false}}}"#)]).0,
            "Linear didn't create the issue"
        );
        for reply in [(503, ""), (200, r#"{"data":{"issueCreate":{"success":true,"issue":{}}}}"#)] {
            let (e, asked) = create(vec![(200, team), reply, (200, team)]);
            assert!(e.contains("may have been created anyway: look in team ENG in Linear"), "{e}");
            assert_eq!(asked, 2);
        }
        // Nothing listens: the error says so, for Codex's sandbox to be explained.
        let down = Client::Linear(LinearConfig::at(&replay::closed(), "lin_api_mock"));
        assert!(down.create("ENG", "Fix it", "").unwrap_err().cut_off);
    }

    #[test]
    fn checks_the_key() {
        let server = replay::serve(vec![(
            200,
            r#"{"data":{"viewer":{"name":"ana","displayName":""},"organization":{"urlKey":"acme"}}}"#,
        )]);
        let Client::Linear(linear) = client(&server) else { unreachable!() };
        // An empty displayName falls back to the name, as it does for an issue's people.
        assert_eq!(linear.whoami(), Ok(("ana".into(), "acme".into())));
    }

    #[test]
    fn lists_a_teams_open_issues_mine_first() {
        let server =
            replay::serve(vec![(200, include_str!("../tests/fixtures/linear/open-issues.json"))]);
        let open = client(&server).open_issues("eng").unwrap();
        assert_eq!(
            open.iter().map(|l| (l.key.as_str(), l.mine)).collect::<Vec<_>>(),
            [("ENG-64", true), ("ENG-12", true), ("ENG-71", false), ("ENG-58", false)]
        );
        assert_eq!(open[0].title, "Fix the login page");
        // One query: the team by its key, open states only, newest first.
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let body = requests[0].json();
        assert_eq!(body["variables"], json!({ "team": "ENG" }));
        let query = body["query"].as_str().unwrap();
        assert!(query.contains(r#"nin: ["completed", "canceled"]"#), "{query}");
        assert!(query.contains("orderBy: updatedAt"), "{query}");
        // What went wrong is said as for an issue.
        let server = replay::serve(vec![(401, "")]);
        assert_eq!(client(&server).open_issues("ENG").unwrap_err(), "Linear rejected the API key");
    }

    #[test]
    fn lists_the_teams_a_page_at_a_time() {
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/linear/teams-1.json")),
            (200, include_str!("../tests/fixtures/linear/teams-2.json")),
        ]);
        let choices = client(&server).choices().unwrap().unwrap();
        assert_eq!(
            choices.iter().map(|c| (c.key.as_str(), c.name.as_str())).collect::<Vec<_>>(),
            [("ENG", "Engineering"), ("OPS", "Operations"), ("WEB", "Website")]
        );
        let vars: Vec<Value> =
            server.requests().iter().map(|r| r.json()["variables"].clone()).collect();
        assert_eq!(vars, [json!({ "after": null }), json!({ "after": "cursor-teams-1" })]);
        let server = replay::serve(vec![(401, "")]);
        assert_eq!(client(&server).choices().unwrap().unwrap_err(), "Linear rejected the API key");
    }

    // Properties of the Markdown cleanup, over generated text.

    /// A word: letters and digits, so it can't read as Markdown.
    fn word() -> impl Strategy<Value = String> {
        "[A-Za-z0-9]{1,8}"
    }

    /// A line of Markdown, and the words it shows: link targets aren't shown.
    fn markdown_line() -> impl Strategy<Value = (String, Vec<String>)> {
        let piece = prop_oneof![
            4 => word().prop_map(|w| (w.clone(), vec![w])),
            1 => word().prop_map(|w| (format!("**{w}**"), vec![w])),
            1 => word().prop_map(|w| (format!("__{w}__"), vec![w])),
            1 => word().prop_map(|w| (format!("`{w}`"), vec![w])),
            1 => (word(), word()).prop_map(|(w, u)| (format!("[{w}](https://example.com/{u})"), vec![w])),
            1 => word().prop_map(|w| (format!("[{w}]"), vec![w])),
        ];
        let lead = prop::sample::select(vec![
            "", "", "  ", "# ", "### ", "* ", "- ", "+ ", "  - ", "1. ", "> ",
        ]);
        (lead, prop::collection::vec(piece, 0..6)).prop_map(|(lead, pieces)| {
            let (text, words): (Vec<String>, Vec<Vec<String>>) = pieces.into_iter().unzip();
            (format!("{lead}{}", text.join(" ")), words.concat())
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn cleanup_keeps_every_word_and_line(lines in prop::collection::vec(markdown_line(), 0..8)) {
            let md = lines.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join("\n");
            let out = markdown_to_text(&md);
            prop_assert_eq!(out.split('\n').count(), md.lines().count().max(1));
            for (line, (_, words)) in out.split('\n').zip(&lines) {
                for w in words {
                    prop_assert!(line.contains(w.as_str()), "{w:?} missing from {line:?}");
                }
                prop_assert!(!line.contains("**") && !line.contains('`') && !line.contains("https://"), "{line:?}");
            }
        }

        #[test]
        fn cleanup_takes_any_text(s in "(\\PC|[\n\t\r \\[\\]()*_`#+-]){0,80}") {
            markdown_to_text(&s);
        }

        #[test]
        fn parsing_takes_any_json(v in prop::collection::btree_map(
            prop::sample::select(vec!["title", "description", "state", "assignee", "labels", "comments", "priorityLabel"]),
            prop_oneof![Just(Value::Null), "[a-z ]{0,8}".prop_map(Value::from), Just(json!({ "nodes": [{}, null, 3] }))],
            0..7,
        )) {
            let v = Value::Object(v.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
            parse_issue(&v, markdown_to_text);
            parse_open(&json!({ "viewer": v.clone(), "issues": { "nodes": [v.clone()] } }));
            parse_teams(&json!({ "teams": { "nodes": [v.clone()], "pageInfo": v.clone() } }));
            parse_created(&json!({ "issueCreate": { "success": v.clone(), "issue": v.clone() } }), "ENG")
                .ok();
        }

        #[test]
        fn only_linears_missing_entity_is_not_found(
            lead in prop::sample::select(vec!["", "Entity not found", "Team not found", "not found"]),
            msg in "\\PC{0,30}",
            status in prop::sample::select(vec![200u16, 400, 500]),
        ) {
            let msg = format!("{lead}{msg}");
            match answer(status, &json!({ "errors": [{ "message": msg }] })) {
                Err(NotFound(m)) => prop_assert!(m == msg && m.starts_with("Entity not found")),
                Err(Auth(_)) => prop_assert!(msg.to_lowercase().contains("authentication")),
                Err(Other(m)) => prop_assert!(m == msg && !m.starts_with("Entity not found")),
                other => prop_assert!(false, "{:?}", other),
            }
        }

        #[test]
        fn a_created_issue_has_a_key(identifier in "\\PC{0,12}", url in "\\PC{0,20}") {
            let d = json!({ "issueCreate": { "success": true, "issue": { "identifier": identifier, "url": url } } });
            match parse_created(&d, "ENG") {
                Ok(made) => prop_assert_eq!((made.key, made.at), (identifier, url)),
                Err(e) => prop_assert!(e.contains("may have been created anyway"), "{}", e),
            }
        }
    }
}
