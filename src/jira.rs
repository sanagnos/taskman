//! Jira Cloud REST client: one issue with its comments, rendered to plain text.

use crate::net;
use crate::tracker::{
    self, CHOICE_PAGES, COMMENTS, Choice, Comment, Created, FetchError, Issue, Listed,
};
use crate::util::parse_ts;
use base64::Engine;
use serde_json::{Value, json};

#[derive(Clone)]
pub struct JiraConfig {
    pub base: String,
    auth: String,
    agent: ureq::Agent,
}

impl JiraConfig {
    pub fn new(base: &str, email: &str, token: &str) -> Self {
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("{email}:{token}"));
        let agent = net::config(net::TRACKER_TIMEOUT).build().into();
        Self { base: base.trim_end_matches('/').to_string(), auth, agent }
    }

    fn get(&self, path: &str) -> Result<Value, ureq::Error> {
        self.get_with(path, &[])
    }

    /// `get` with query parameters, encoded.
    fn get_with(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, ureq::Error> {
        net::retry_dropped(|| {
            self.agent
                .get(&format!("{}{path}", self.base))
                .query_pairs(query.iter().copied())
                .header("Authorization", &format!("Basic {}", self.auth))
                .header("Accept", "application/json")
                .call()?
                .body_mut()
                .read_json()
        })
    }

    /// Whether a Jira project with this key exists and is visible to the account. Errors other
    /// than "not found" (offline, say) come back as Err so callers can ignore them.
    pub fn project_exists(&self, key: &str) -> Result<bool, String> {
        match self.get(&format!("/rest/api/3/project/{key}")) {
            Ok(_) => Ok(true),
            Err(ureq::Error::StatusCode(404)) => Ok(false),
            Err(e) => Err(describe(e, "")),
        }
    }

    /// The projects the account can see, a page at a time, up to `CHOICE_PAGES` pages.
    pub fn projects(&self) -> Result<Vec<Choice>, String> {
        let mut projects = Vec::new();
        for _ in 0..CHOICE_PAGES {
            let at = projects.len().to_string();
            let page = self
                .get_with(
                    "/rest/api/3/project/search",
                    &[("startAt", &at), ("maxResults", "100"), ("orderBy", "key")],
                )
                .map_err(|e| describe(e, ""))?;
            let (got, last) = parse_projects(&page);
            let none = got.is_empty();
            projects.extend(got);
            if last || none {
                break;
            }
        }
        Ok(projects)
    }

    /// The open issues of the project whose key is `prefix` (status not in the Done category),
    /// the 100 most recently updated; `mine` when assigned to the account.
    pub fn open_issues(&self, prefix: &str) -> Result<Vec<Listed>, String> {
        let project: String =
            prefix.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let project = project.to_uppercase();
        let me = self.get("/rest/api/3/myself").map_err(|e| describe(e, ""))?;
        let jql =
            format!("project = \"{project}\" AND statusCategory != Done ORDER BY updated DESC");
        let v = self
            .get_with(
                "/rest/api/3/search/jql",
                &[("jql", &jql), ("fields", "summary,assignee"), ("maxResults", "100")],
            )
            .map_err(|e| match e {
                ureq::Error::StatusCode(400) => {
                    format!("Jira has no project {project}, or this account can't see it")
                }
                e => describe(e, ""),
            })?;
        Ok(parse_open(&v, me["accountId"].as_str().unwrap_or("")))
    }

    /// `create`'s POST, answered whatever its status, as Jira says in the body why it refused.
    /// Never asked again: a request that dropped may have made the ticket.
    fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), ureq::Error> {
        let mut resp = self
            .agent
            .post(&format!("{}{path}", self.base))
            .config()
            .http_status_as_error(false)
            .build()
            .header("Authorization", &format!("Basic {}", self.auth))
            .header("Accept", "application/json")
            .send_json(body)?;
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().read_json().unwrap_or(Value::Null)))
    }

    /// Creates a ticket in the project whose key is `prefix`: a Task, or the project's first
    /// standard issue type when it has no Task. Refused, with Jira's reasons, when the project
    /// requires fields taskman doesn't fill.
    pub fn create(
        &self,
        prefix: &str,
        title: &str,
        description: &str,
    ) -> Result<Created, FetchError> {
        let project = prefix.to_uppercase();
        let v = self.get(&format!("/rest/api/3/project/{project}")).map_err(|e| match e {
            ureq::Error::StatusCode(404) => FetchError::Other(format!(
                "Jira has no project {project}, or this account can't see it"
            )),
            e => failure(e, ""),
        })?;
        let Some((type_id, type_name)) = issue_type(&v) else {
            return Err(format!("Jira project {project} has no issue type a ticket can be").into());
        };
        let mut fields = json!({
            "project": { "key": project },
            "issuetype": { "id": type_id },
            "summary": title,
        });
        if !description.trim().is_empty() {
            fields["description"] = text_to_adf(description);
        }
        let place = format!("project {project} in Jira");
        let (status, v) =
            self.post("/rest/api/3/issue", &json!({ "fields": fields })).map_err(|e| {
                let sent = !tracker::unsent(&e);
                let e = failure(e, "");
                if sent { e.map(|e| tracker::maybe_created(&e, &place)) } else { e }
            })?;
        match status {
            200..=299 => Ok(parse_created(&v, &self.base, &place)?),
            401 | 403 => Err(failure(ureq::Error::StatusCode(status), "")),
            400..=499 => Err(refusal(&v, &project, &type_name, status).into()),
            _ => {
                Err(tracker::maybe_created(&format!("Jira answered HTTP {status}"), &place).into())
            }
        }
    }

    /// Checks the credentials; returns the account's display name.
    pub fn whoami(&self) -> Result<String, String> {
        let v = self.get("/rest/api/3/myself").map_err(|e| describe(e, ""))?;
        Ok(v["displayName"].as_str().unwrap_or("?").to_string())
    }
}

fn describe(e: ureq::Error, key: &str) -> String {
    failure(e, key).into()
}

/// What a request about `key` (empty for none) that failed with `e` means.
fn failure(e: ureq::Error, key: &str) -> FetchError {
    match e {
        ureq::Error::StatusCode(401 | 403) => FetchError::Auth(
            "Jira rejected the credentials (check the email and API token)".to_string(),
        ),
        ureq::Error::StatusCode(404) if !key.is_empty() => {
            FetchError::NotFound(format!("{key} not found, or not visible to this account"))
        }
        other => FetchError::request(&other, format!("Jira request failed: {other}")),
    }
}

pub fn fetch(cfg: &JiraConfig, key: &str) -> Result<Issue, FetchError> {
    let path = format!(
        "/rest/api/3/issue/{key}?fields=summary,status,issuetype,priority,assignee,description,comment"
    );
    let v = cfg.get(&path).map_err(|e| failure(e, key))?;
    let mut issue = parse_issue(&v);
    // The issue carries its oldest comments only, up to a limit; past it, ask for the newest.
    if issue.comments_left_out > 0 {
        let path =
            format!("/rest/api/3/issue/{key}/comment?orderBy=-created&maxResults={COMMENTS}");
        let page = cfg.get(&path).map_err(|e| failure(e, key))?;
        (issue.comments, issue.comments_left_out) = parse_comments(&page);
    }
    Ok(issue)
}

/// A page of comments (`fields.comment` in an issue, or the comment endpoint's answer), newest
/// first, and how many the issue has beyond them.
fn parse_comments(page: &Value) -> (Vec<Comment>, usize) {
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();
    let mut comments: Vec<Comment> = page["comments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| Comment {
            id: s(&c["id"]),
            author: s(&c["author"]["displayName"]),
            created: s(&c["created"]),
            body: adf_to_text(&c["body"]),
        })
        .collect();
    comments.sort_by_key(|c| std::cmp::Reverse(parse_ts(&c.created)));
    let total =
        page["total"].as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(comments.len());
    let left_out = total.saturating_sub(comments.len());
    (comments, left_out)
}

fn parse_issue(v: &Value) -> Issue {
    let f = &v["fields"];
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();
    let (comments, comments_left_out) = parse_comments(&f["comment"]);
    Issue {
        summary: s(&f["summary"]),
        status: s(&f["status"]["name"]),
        status_category: s(&f["status"]["statusCategory"]["key"]),
        issue_type: s(&f["issuetype"]["name"]),
        priority: s(&f["priority"]["name"]),
        assignee: f["assignee"]["displayName"].as_str().unwrap_or("Unassigned").to_string(),
        description: adf_to_text(&f["description"]),
        comments,
        comments_left_out,
        branch: String::new(),
        prs: Vec::new(),
    }
}

/// The issue type a new ticket gets from a project's `issueTypes`: Task, else the first standard
/// type (not a sub-task, nor above a story like an epic). Its id and name.
fn issue_type(project: &Value) -> Option<(String, String)> {
    let types: Vec<&Value> = project["issueTypes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["subtask"] != true && t["id"].is_string())
        .collect();
    let standard = |t: &&&Value| t["hierarchyLevel"].as_i64().unwrap_or(0) == 0;
    let task = |t: &&&Value| t["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case("task"));
    let t = types.iter().find(task).or_else(|| types.iter().find(standard))?;
    Some((t["id"].as_str()?.to_string(), t["name"].as_str().unwrap_or("ticket").to_string()))
}

/// The ticket `create` made, from Jira's answer: its key, linked on the site at `base`.
fn parse_created(v: &Value, base: &str, place: &str) -> Result<Created, String> {
    let key = v["key"].as_str();
    Created::read(key, &format!("{base}/browse/{}", key.unwrap_or("")), place)
}

/// Why Jira refused a new ticket, from its `errorMessages` and per-field `errors`.
fn refusal(v: &Value, project: &str, type_name: &str, status: u16) -> String {
    let mut reasons: Vec<String> = v["errorMessages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str())
        .map(String::from)
        .collect();
    if let Some(errors) = v["errors"].as_object() {
        reasons.extend(errors.iter().map(|(field, m)| match m.as_str() {
            Some(m) => m.to_string(),
            None => format!("{field}: {m}"),
        }));
    }
    let reasons = reasons.iter().map(|r| r.trim().trim_end_matches('.')).collect::<Vec<_>>();
    let reasons = if reasons.is_empty() { format!("HTTP {status}") } else { reasons.join("; ") };
    if reasons.to_lowercase().contains("required") {
        format!(
            "Jira project {project} requires fields taskman doesn't fill to create a {type_name} ({reasons}); create the ticket in Jira and add it with taskman add KEY"
        )
    } else {
        format!("Jira refused the new {type_name}: {reasons}")
    }
}

/// Text, Markdown as it's written, in Atlassian Document Format: a paragraph per block of lines,
/// its lines kept apart.
fn text_to_adf(text: &str) -> Value {
    let mut paragraphs = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().chain([""]) {
        if !line.trim().is_empty() {
            lines.push(line.trim_end());
            continue;
        }
        if lines.is_empty() {
            continue;
        }
        let mut content = Vec::new();
        for (i, l) in lines.drain(..).enumerate() {
            if i > 0 {
                content.push(json!({ "type": "hardBreak" }));
            }
            content.push(json!({ "type": "text", "text": l }));
        }
        paragraphs.push(json!({ "type": "paragraph", "content": content }));
    }
    json!({ "type": "doc", "version": 1, "content": paragraphs })
}

/// A page of `project/search`: its projects, and whether it's the last page.
fn parse_projects(page: &Value) -> (Vec<Choice>, bool) {
    let projects: Vec<Choice> = page["values"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(Choice {
                key: p["key"].as_str()?.to_string(),
                name: p["name"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect();
    // An older Jira says nothing of the last page: a page short of full is it.
    let last = page["isLast"].as_bool().unwrap_or_else(|| {
        let total = page["total"].as_u64().unwrap_or(0);
        let at = page["startAt"].as_u64().unwrap_or(0);
        at + projects.len() as u64 >= total
    });
    (projects, last)
}

/// A search's issues as the Add form lists them; `me` is the account's id.
fn parse_open(v: &Value, me: &str) -> Vec<Listed> {
    v["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            Some(Listed {
                key: i["key"].as_str()?.to_string(),
                title: i["fields"]["summary"].as_str().unwrap_or("").to_string(),
                mine: !me.is_empty() && i["fields"]["assignee"]["accountId"] == me,
            })
        })
        .collect()
}

/// Renders Atlassian Document Format to readable plain text.
pub fn adf_to_text(node: &Value) -> String {
    if let Some(s) = node.as_str() {
        return s.to_string(); // older API versions return wiki text
    }
    let mut out = String::new();
    walk(node, &mut out, 0);
    // Collapse runs of blank lines.
    let mut cleaned = String::new();
    let mut blank = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        cleaned.push_str(line.trim_end());
        cleaned.push('\n');
    }
    cleaned.trim().to_string()
}

fn children(node: &Value, out: &mut String, depth: usize) {
    if let Some(cs) = node["content"].as_array() {
        for c in cs {
            walk(c, out, depth);
        }
    }
}

fn walk(node: &Value, out: &mut String, depth: usize) {
    let attr = |k: &str| node["attrs"][k].as_str().unwrap_or("").to_string();
    match node["type"].as_str().unwrap_or("") {
        "text" => out.push_str(node["text"].as_str().unwrap_or("")),
        "hardBreak" => out.push('\n'),
        "paragraph" => {
            children(node, out, depth);
            out.push_str("\n\n");
        }
        "heading" => {
            out.push('▍');
            children(node, out, depth);
            out.push_str("\n\n");
        }
        "bulletList" | "orderedList" => {
            let ordered = node["type"] == "orderedList";
            if let Some(items) = node["content"].as_array() {
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&"  ".repeat(depth));
                    out.push_str(&if ordered {
                        format!("{}. ", i + 1)
                    } else {
                        "• ".to_string()
                    });
                    let mut inner = String::new();
                    children(item, &mut inner, depth + 1);
                    out.push_str(inner.trim_matches('\n'));
                    out.push('\n');
                }
            }
            out.push('\n');
        }
        "codeBlock" => {
            let mut inner = String::new();
            children(node, &mut inner, depth);
            for l in inner.lines() {
                out.push_str("  │ ");
                out.push_str(l);
                out.push('\n');
            }
            out.push('\n');
        }
        "blockquote" => {
            let mut inner = String::new();
            children(node, &mut inner, depth);
            for l in inner.trim().lines() {
                out.push_str("> ");
                out.push_str(l);
                out.push('\n');
            }
            out.push('\n');
        }
        "mention" => out.push_str(&attr("text")),
        "emoji" => {
            let t = attr("text");
            out.push_str(if t.is_empty() { attr("shortName") } else { t }.as_str());
        }
        "inlineCard" | "blockCard" | "embedCard" => out.push_str(&attr("url")),
        "status" => out.push_str(&format!("[{}]", attr("text"))),
        "date" => out.push_str(&attr("timestamp")),
        "rule" => out.push_str("────────\n\n"),
        "mediaSingle" | "mediaGroup" => out.push_str("[attachment]\n\n"),
        "table" => {
            if let Some(rows) = node["content"].as_array() {
                for row in rows {
                    let cells: Vec<String> = row["content"]
                        .as_array()
                        .map(|cs| {
                            cs.iter()
                                .map(|c| {
                                    let mut t = String::new();
                                    children(c, &mut t, depth);
                                    t.split_whitespace().collect::<Vec<_>>().join(" ")
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    out.push_str(&cells.join(" │ "));
                    out.push('\n');
                }
            }
            out.push('\n');
        }
        _ => children(node, out, depth),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay;
    use crate::tracker::FetchError::{Auth, NotFound, Other, Unreachable};
    use crate::tracker::{Client, Text};
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn renders_adf() {
        let doc = json!({"type":"doc","content":[
            {"type":"heading","content":[{"type":"text","text":"Goal"}]},
            {"type":"paragraph","content":[{"type":"text","text":"Hi "},{"type":"mention","attrs":{"text":"@Ana"}}]},
            {"type":"bulletList","content":[
                {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"one"}]}]},
                {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"two"}]}]}
            ]}
        ]});
        assert_eq!(adf_to_text(&doc), "▍Goal\n\nHi @Ana\n\n• one\n• two");
    }

    #[test]
    fn says_how_many_comments_it_left_out() {
        let c = |id: &str, created: &str| json!({ "id": id, "author": { "displayName": "Ana" }, "created": created, "body": "hi" });
        // The issue's own field, oldest first and short of the total.
        let field = json!({ "total": 250, "comments": [c("1", "2024-01-01T10:00:00.000+0000"), c("2", "2024-01-01T09:30:00.000-0100")] });
        let (comments, left_out) = parse_comments(&field);
        assert_eq!(comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["2", "1"]);
        assert_eq!(left_out, 248);
        let issue = parse_issue(
            &json!({ "fields": { "comment": { "total": 1, "comments": [c("1", "2024-01-01T10:00:00.000+0000")] } } }),
        );
        assert_eq!((issue.comments.len(), issue.comments_left_out), (1, 0));
    }

    // Recorded responses (tests/fixtures/jira), served locally through the whole fetch.

    fn client(server: &replay::Server) -> Client {
        Client::Jira(JiraConfig::new(&format!("{}/", server.url), "ana@example.com", "token-1"))
    }

    #[test]
    fn fetches_an_issue() {
        let server = replay::serve(vec![(200, include_str!("../tests/fixtures/jira/issue.json"))]);
        let issue = client(&server).fetch("ENG-52", Text::Plain).unwrap();
        assert_eq!(
            (issue.summary.as_str(), issue.status.as_str(), issue.status_category.as_str()),
            ("Fix the login page", "In Progress", "indeterminate")
        );
        assert_eq!((issue.issue_type.as_str(), issue.priority.as_str()), ("Task", "Medium"));
        assert_eq!(issue.assignee, "Ana Example");
        assert_eq!(
            issue.description,
            "The login page loops after SSO.\n\n• Sign in with SSO\n• See it redirect again\n\n  │ GET /login 302"
        );
        assert_eq!(
            issue.comments.iter().map(|c| c.author.as_str()).collect::<Vec<_>>(),
            ["Ben Example", "Ana Example"]
        );
        assert_eq!(
            (issue.comments[0].body.as_str(), issue.comments_left_out),
            ("Fixed by clearing the cookie.", 0)
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].line.starts_with("GET /rest/api/3/issue/ENG-52?fields=summary,"),
            "{}",
            requests[0].line
        );
        let auth = base64::engine::general_purpose::STANDARD.encode("ana@example.com:token-1");
        assert_eq!(requests[0].header("authorization"), Some(format!("Basic {auth}").as_str()));
    }

    #[test]
    fn fetches_the_newest_comments_past_the_limit() {
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/jira/issue-many-comments.json")),
            (200, include_str!("../tests/fixtures/jira/comments-newest.json")),
        ]);
        let issue = client(&server).fetch("ENG-53", Text::Plain).unwrap();
        assert_eq!(
            issue.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["150", "149", "148"]
        );
        assert_eq!(issue.comments[2].body, "third newest");
        assert_eq!(issue.comments_left_out, 147);
        // Nulls where a field has no value.
        assert_eq!(
            (issue.priority.as_str(), issue.assignee.as_str(), issue.description.as_str()),
            ("", "Unassigned", "")
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1].line.starts_with(&format!(
                "GET /rest/api/3/issue/ENG-53/comment?orderBy=-created&maxResults={COMMENTS} "
            )),
            "{}",
            requests[1].line
        );
    }

    #[test]
    fn says_what_went_wrong() {
        let not_found = include_str!("../tests/fixtures/jira/not-found.json");
        let fetch = |status| {
            let server = replay::serve(vec![(status, not_found)]);
            client(&server).fetch("ENG-52", Text::Plain).unwrap_err()
        };
        assert_eq!(fetch(404), NotFound("ENG-52 not found, or not visible to this account".into()));
        let rejected = Auth("Jira rejected the credentials (check the email and API token)".into());
        assert_eq!(fetch(401), rejected);
        assert_eq!(fetch(403), rejected);
        assert!(matches!(fetch(500), Other(e) if e.starts_with("Jira request failed: ")));
        // The comments request fails after the issue's.
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/jira/issue-many-comments.json")),
            (503, ""),
        ]);
        assert!(matches!(
            client(&server).fetch("ENG-53", Text::Plain).unwrap_err(),
            Other(e) if e.starts_with("Jira request failed: ")
        ));
        // Not JSON, from a proxy say.
        let server = replay::serve(vec![(200, "<html>sign in</html>")]);
        assert!(matches!(
            client(&server).fetch("ENG-52", Text::Plain).unwrap_err(),
            Other(e) if e.starts_with("Jira request failed: ")
        ));
        // Nothing listens.
        let down = Client::Jira(JiraConfig::new(&replay::closed(), "ana@example.com", "token"));
        let e = down.fetch("ENG-52", Text::Plain).unwrap_err();
        assert!(matches!(&e, Unreachable(m) if m.starts_with("Jira request failed: ")), "{e:?}");
    }

    #[test]
    fn creates_a_task_in_the_prefixs_project() {
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/jira/project.json")),
            (201, include_str!("../tests/fixtures/jira/created.json")),
        ]);
        let made =
            client(&server).create("eng", "Fix the signup form", "Steps:\none\n\ntwo").unwrap();
        assert_eq!(made.key, "ENG-53");
        assert_eq!(made.at, format!("{}/browse/ENG-53", server.url));
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0].line.starts_with("GET /rest/api/3/project/ENG "),
            "{}",
            requests[0].line
        );
        assert!(requests[1].line.starts_with("POST /rest/api/3/issue "), "{}", requests[1].line);
        let fields = &requests[1].json()["fields"];
        assert_eq!(fields["project"], json!({ "key": "ENG" }));
        // Task, though the project lists an epic and a bug before it.
        assert_eq!(fields["issuetype"], json!({ "id": "10001" }));
        assert_eq!(fields["summary"], "Fix the signup form");
        assert_eq!(adf_to_text(&fields["description"]), "Steps:\none\n\ntwo");
        // No description, no field.
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/jira/project.json")),
            (201, include_str!("../tests/fixtures/jira/created.json")),
        ]);
        client(&server).create("ENG", "Fix it", " ").unwrap();
        assert!(server.requests()[1].json()["fields"].get("description").is_none());
    }

    #[test]
    fn picks_a_standard_issue_type_when_there_is_no_task() {
        let types = |ts: Value| issue_type(&json!({ "issueTypes": ts }));
        let story = json!({ "id": "7", "name": "Story", "subtask": false, "hierarchyLevel": 0 });
        let epic = json!({ "id": "8", "name": "Epic", "subtask": false, "hierarchyLevel": 1 });
        let sub = json!({ "id": "9", "name": "Sub-task", "subtask": true, "hierarchyLevel": -1 });
        assert_eq!(types(json!([epic, sub, story])), Some(("7".into(), "Story".into())));
        assert_eq!(
            types(json!([epic, { "id": "3", "name": "task" }])),
            Some(("3".into(), "task".into()))
        );
        assert_eq!(types(json!([sub])), None);
        assert_eq!(types(json!([])), None);
    }

    #[test]
    fn says_why_it_couldnt_create_and_whether_the_ticket_may_exist() {
        let project = include_str!("../tests/fixtures/jira/project.json");
        let create = |replies: Vec<(u16, &'static str)>| {
            let server = replay::serve(replies);
            client(&server).create("ENG", "Fix the signup form", "").unwrap_err().message
        };
        let e = create(vec![
            (200, project),
            (400, include_str!("../tests/fixtures/jira/create-needs-fields.json")),
        ]);
        assert_eq!(
            e,
            "Jira project ENG requires fields taskman doesn't fill to create a Task (Team is required; Sprint is required); create the ticket in Jira and add it with taskman add KEY"
        );
        let e = create(vec![
            (200, project),
            (400, r#"{"errorMessages":["Summary is too long."],"errors":{}}"#),
        ]);
        assert_eq!(e, "Jira refused the new Task: Summary is too long");
        assert_eq!(
            create(vec![(404, include_str!("../tests/fixtures/jira/not-found.json"))]),
            "Jira has no project ENG, or this account can't see it"
        );
        assert_eq!(
            create(vec![(200, project), (401, "")]),
            "Jira rejected the credentials (check the email and API token)"
        );
        // The tracker failing, or an answer without a key, may still have made it.
        for reply in [(502, ""), (201, "{}"), (201, "<html>")] {
            let e = create(vec![(200, project), reply]);
            assert!(e.contains("may have been created anyway: look in project ENG in Jira"), "{e}");
        }
        // Never asked twice.
        let server = replay::serve(vec![
            (200, project),
            (502, ""),
            (201, include_str!("../tests/fixtures/jira/created.json")),
        ]);
        assert!(client(&server).create("ENG", "Fix it", "").is_err());
        assert_eq!(server.requests().len(), 2);
    }

    #[test]
    fn asks_again_when_a_read_drops_but_never_a_create() {
        let project = include_str!("../tests/fixtures/jira/project.json");
        let created = include_str!("../tests/fixtures/jira/created.json");
        let server = replay::serve(vec![
            (replay::DROP, ""),
            (200, include_str!("../tests/fixtures/jira/issue.json")),
        ]);
        assert_eq!(client(&server).fetch("ENG-52", Text::Plain).unwrap().assignee, "Ana Example");
        assert_eq!(server.requests().len(), 2);
        let server = replay::serve(vec![(replay::DROP, ""), (200, project), (201, created)]);
        assert_eq!(client(&server).create("ENG", "Fix it", "").unwrap().key, "ENG-53");
        assert_eq!(server.requests().len(), 3);
        // The POST may have made the ticket before the connection dropped.
        let server = replay::serve(vec![(200, project), (replay::DROP, ""), (201, created)]);
        let e = client(&server).create("ENG", "Fix it", "").unwrap_err();
        assert!(e.message.contains("may have been created anyway"), "{}", e.message);
        assert_eq!(server.requests().len(), 2);
    }

    #[test]
    fn lists_the_projects_a_page_at_a_time() {
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/jira/projects-1.json")),
            (200, include_str!("../tests/fixtures/jira/projects-2.json")),
        ]);
        let choices = client(&server).choices().unwrap().unwrap();
        // Sorted by key, and a key that can't be a prefix is left out.
        assert_eq!(
            choices.iter().map(|c| (c.key.as_str(), c.name.as_str())).collect::<Vec<_>>(),
            [("ENG", "Engineering"), ("OPS", "Operations"), ("WEB", "Website")]
        );
        let lines: Vec<String> = server.requests().into_iter().map(|r| r.line).collect();
        assert_eq!(
            lines,
            [
                "GET /rest/api/3/project/search?startAt=0&maxResults=100&orderBy=key HTTP/1.1",
                "GET /rest/api/3/project/search?startAt=2&maxResults=100&orderBy=key HTTP/1.1"
            ]
        );
        // What went wrong is said as for an issue.
        let server = replay::serve(vec![(401, "")]);
        assert_eq!(
            client(&server).choices().unwrap().unwrap_err(),
            "Jira rejected the credentials (check the email and API token)"
        );
        // An account that sees no project: one empty page, and no more asked.
        let server = replay::serve(vec![(200, r#"{"values":[],"isLast":false}"#)]);
        assert_eq!(client(&server).choices(), Some(Ok(vec![])));
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn a_page_without_is_last_is_the_last_when_it_reaches_the_total() {
        let page = |at: u64, n: usize, total: u64| {
            let values: Vec<Value> =
                (0..n).map(|i| json!({ "key": format!("P{i}"), "name": "x" })).collect();
            parse_projects(&json!({ "startAt": at, "total": total, "values": values })).1
        };
        assert!(!page(0, 50, 120));
        assert!(page(100, 20, 120));
        assert!(page(0, 0, 0));
    }

    #[test]
    fn checks_projects_and_credentials() {
        let server = replay::serve(vec![
            (200, r#"{"key":"ENG","name":"Engineering"}"#),
            (404, include_str!("../tests/fixtures/jira/not-found.json")),
            (200, r#"{"accountId":"000000:0000-ana","displayName":"Ana Example"}"#),
        ]);
        let Client::Jira(jira) = client(&server) else { unreachable!() };
        assert_eq!(jira.project_exists("ENG"), Ok(true));
        assert_eq!(jira.project_exists("OPS"), Ok(false));
        assert_eq!(jira.whoami().as_deref(), Ok("Ana Example"));
        let lines: Vec<String> = server.requests().into_iter().map(|r| r.line).collect();
        assert_eq!(
            lines,
            [
                "GET /rest/api/3/project/ENG HTTP/1.1",
                "GET /rest/api/3/project/OPS HTTP/1.1",
                "GET /rest/api/3/myself HTTP/1.1"
            ]
        );
    }

    #[test]
    fn lists_a_projects_open_issues_mine_first() {
        let server = replay::serve(vec![
            (200, r#"{"accountId":"000000:0000-ana","displayName":"Ana Example"}"#),
            (200, include_str!("../tests/fixtures/jira/open-issues.json")),
        ]);
        let open = client(&server).open_issues("eng").unwrap();
        assert_eq!(
            open.iter().map(|l| (l.key.as_str(), l.mine)).collect::<Vec<_>>(),
            [("ENG-64", true), ("ENG-12", true), ("ENG-71", false), ("ENG-58", false)]
        );
        assert_eq!(open[0].title, "Fix the login page");
        let lines: Vec<String> = server.requests().into_iter().map(|r| r.line).collect();
        assert_eq!(lines[0], "GET /rest/api/3/myself HTTP/1.1");
        // The project by its key, statuses not done, newest first; encoded.
        assert_eq!(
            lines[1],
            "GET /rest/api/3/search/jql?jql=project%20%3D%20%22ENG%22%20AND%20statusCategory%20!%3D%20Done%20ORDER%20BY%20updated%20DESC&fields=summary%2Cassignee&maxResults=100 HTTP/1.1"
        );
        // A project the search doesn't know.
        let server = replay::serve(vec![
            (200, r#"{"accountId":"000000:0000-ana"}"#),
            (
                400,
                r#"{"errorMessages":["The value 'OPS' does not exist for the field 'project'."]}"#,
            ),
        ]);
        assert_eq!(
            client(&server).open_issues("OPS").unwrap_err(),
            "Jira has no project OPS, or this account can't see it"
        );
        let server = replay::serve(vec![(401, "")]);
        assert_eq!(
            client(&server).open_issues("ENG").unwrap_err(),
            "Jira rejected the credentials (check the email and API token)"
        );
    }

    // Properties of the ADF renderer, over generated documents.

    /// A word of a text node: letters and digits, so it can't read as layout.
    fn word() -> impl Strategy<Value = String> {
        "[A-Za-z0-9]{1,8}"
    }

    fn text(words: &[String]) -> Value {
        json!({ "type": "text", "text": words.join(" ") })
    }

    /// Inline content, the leaves of a document.
    fn inline() -> impl Strategy<Value = Value> {
        prop_oneof![
            4 => prop::collection::vec(word(), 1..4).prop_map(|w| text(&w)),
            1 => Just(json!({ "type": "hardBreak" })),
            1 => word().prop_map(|w| json!({ "type": "mention", "attrs": { "id": "000000:0000", "text": format!("@{w}") } })),
            1 => Just(json!({ "type": "emoji", "attrs": { "shortName": ":tada:", "text": "🎉" } })),
            1 => word().prop_map(|w| json!({ "type": "status", "attrs": { "text": w, "color": "blue" } })),
            1 => Just(json!({ "type": "inlineCard", "attrs": { "url": "https://example.com/x" } })),
        ]
    }

    fn with(kind: &str, content: Vec<Value>) -> Value {
        let mut node = json!({ "type": kind });
        node["content"] = Value::Array(content);
        node
    }

    /// Block content as Jira writes it: paragraphs, headings and code at the leaves; lists, quotes,
    /// tables and panels holding more blocks.
    fn block() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            prop::collection::vec(inline(), 0..5).prop_map(|c| with("paragraph", c)),
            prop::collection::vec(inline(), 0..3).prop_map(|c| with("heading", c)),
            prop::collection::vec(word(), 1..4).prop_map(|w| with("codeBlock", vec![text(&w)])),
            Just(json!({ "type": "rule" })),
            Just(
                json!({ "type": "mediaSingle", "content": [{ "type": "media", "attrs": { "id": "x" } }] })
            ),
        ];
        leaf.prop_recursive(4, 48, 4, |blocks| {
            let items = prop::collection::vec(prop::collection::vec(blocks.clone(), 1..3), 1..4);
            prop_oneof![
                items.clone().prop_map(|i| with(
                    "bulletList",
                    i.into_iter().map(|c| with("listItem", c)).collect()
                )),
                items.prop_map(|i| with(
                    "orderedList",
                    i.into_iter().map(|c| with("listItem", c)).collect()
                )),
                prop::collection::vec(blocks.clone(), 1..3).prop_map(|c| with("blockquote", c)),
                prop::collection::vec(blocks.clone(), 1..3).prop_map(|c| with("panel", c)),
                prop::collection::vec(prop::collection::vec(blocks, 1..3), 1..3).prop_map(
                    |cells| {
                        let row = |cells: Vec<Vec<Value>>| {
                            with(
                                "tableRow",
                                cells.into_iter().map(|c| with("tableCell", c)).collect(),
                            )
                        };
                        with("table", vec![row(cells.clone()), row(cells)])
                    }
                ),
            ]
        })
    }

    /// Every text node's words, in document order.
    fn words_in(node: &Value, out: &mut Vec<String>) {
        if node["type"] == "text" {
            out.extend(node["text"].as_str().unwrap_or("").split_whitespace().map(String::from));
        }
        for c in node["content"].as_array().into_iter().flatten() {
            words_in(c, out);
        }
    }

    /// Any JSON, with the keys and node types ADF uses turning up often.
    fn any_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<f64>().prop_filter("JSON has no NaN", |f| f.is_finite()).prop_map(Value::from),
            ".{0,12}".prop_map(Value::from),
            prop::sample::select(vec![
                "text",
                "paragraph",
                "table",
                "bulletList",
                "codeBlock",
                "listItem",
                "doc"
            ])
            .prop_map(Value::from),
        ];
        leaf.prop_recursive(5, 64, 6, |inner| {
            let key = prop::sample::select(vec![
                "type", "content", "text", "attrs", "fields", "comment", "comments", "total",
                "body", "status", "x",
            ]);
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::from),
                prop::collection::btree_map(key, inner, 0..6).prop_map(|m| Value::Object(
                    m.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
                )),
            ]
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn rendering_keeps_every_word(blocks in prop::collection::vec(block(), 0..6)) {
            let doc = with("doc", blocks);
            let out = adf_to_text(&doc);
            let mut words = Vec::new();
            words_in(&doc, &mut words);
            for w in &words {
                prop_assert!(out.contains(w.as_str()), "{w:?} missing from {out:?}");
            }
            // At most one blank line in a row, and none at either end.
            prop_assert!(!out.contains("\n\n\n"), "{out:?}");
            prop_assert_eq!(out.trim(), out.as_str());
        }

        #[test]
        fn rendering_and_parsing_take_any_json(v in any_json()) {
            adf_to_text(&v);
            parse_issue(&v);
            parse_comments(&v);
            parse_open(&v, "000000:0000-ana");
            parse_projects(&v);
            parse_projects(&json!({ "values": [v.clone(), { "key": v }], "total": 1 }));
            issue_type(&v);
            issue_type(&json!({ "issueTypes": [v.clone()] }));
            refusal(&v, "ENG", "Task", 400);
            refusal(&json!({ "errorMessages": [v.clone()], "errors": { "x": v.clone() } }), "ENG", "Task", 400);
        }

        #[test]
        fn a_created_ticket_has_a_key(v in any_json()) {
            if let Ok(made) = parse_created(&v, "https://example.atlassian.net", "ENG") {
                prop_assert!(crate::ticket_key::well_formed(&made.key));
            }
            let key = json!({ "key": "ENG-53", "id": v });
            prop_assert_eq!(parse_created(&key, "", "ENG").unwrap().key, "ENG-53");
        }

        #[test]
        fn descriptions_keep_every_word(lines in prop::collection::vec("[ a-z*#`]{0,12}", 0..8)) {
            let text = lines.join("\n");
            let out = adf_to_text(&text_to_adf(&text));
            for w in text.split_whitespace() {
                prop_assert!(out.contains(w), "{w:?} missing from {out:?}");
            }
        }
    }
}
