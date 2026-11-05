//! Jira Cloud REST client: one issue with its comments, rendered to plain text.

use crate::net;
use crate::store;
use crate::tracker::{
    self, CHOICE_PAGES, COMMENTS, Choice, Comment, Created, FetchError, Issue, Listed,
};
use crate::util::parse_ts;
use base64::Engine;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use ureq::Body;
use ureq::http::Response;

/// How long each question to `serverInfo` may take: it's asked after a request has already failed.
const SERVER_INFO_TIMEOUT: Duration = Duration::from_secs(5);

/// How long after asking `serverInfo` and getting no clear answer it may be asked again: a failure
/// repeats with every refresh, and each asking can send credentials.
const ASK_AGAIN_AFTER: Duration = Duration::from_secs(600);

/// What a client knows about which Jira it's talking to. It lasts as long as the client: a TUI's is
/// kept across refreshes until its account is saved or its token changes (`tracker::Clients`), and
/// each command has its own, so each run of one may send credentials while it finds out.
#[derive(Default)]
struct Deployment {
    /// Whether it's Data Center or Server, once it has said.
    self_hosted: Option<bool>,
    /// When `serverInfo` was last asked.
    asked: Option<Instant>,
    /// Jira wants a CAPTCHA solved before it takes another login, so this client sends no
    /// credentials: another failed login would only count towards locking the account.
    captcha: bool,
}

#[derive(Clone)]
pub struct JiraConfig {
    pub base: String,
    auth: String,
    token: String,
    agent: net::TrackerAgent,
    deployment: Arc<Mutex<Deployment>>,
}

impl JiraConfig {
    pub fn new(base: &str, email: &str, token: &str) -> Self {
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("{email}:{token}"));
        let agent = net::TrackerAgent::new(net::TRACKER_TIMEOUT);
        Self {
            base: base.trim_end_matches('/').to_string(),
            auth,
            token: token.to_string(),
            agent,
            deployment: Arc::default(),
        }
    }

    fn get(&self, path: &str) -> Result<Value, ureq::Error> {
        self.get_with(path, &[])
    }

    /// `get` with query parameters, encoded.
    fn get_with(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, ureq::Error> {
        self.unless_refused()?;
        net::retry_dropped(|| {
            let mut response = self
                .agent
                .ready()
                .get(&format!("{}{path}", self.base))
                .query_pairs(query.iter().copied())
                .header("Authorization", &format!("Basic {}", self.auth))
                .header("Accept", "application/json")
                .call()?;
            self.status_of(&mut response)?;
            response.body_mut().read_json()
        })
    }

    /// Refuses a request before it sends credentials that can't work, to a Jira known to be Data
    /// Center or Server or waiting on a CAPTCHA: a failure repeats with every refresh, and failed
    /// logins can lock an account. The 403 only stands for that, as `failure` says what it means.
    fn unless_refused(&self) -> Result<(), ureq::Error> {
        if self.refusing() { Err(ureq::Error::StatusCode(403)) } else { Ok(()) }
    }

    fn refusing(&self) -> bool {
        let known = self.deployment();
        known.self_hosted == Some(true) || known.captcha
    }

    /// Notes a response saying Jira wants a CAPTCHA solved.
    fn note_captcha(&self, response: &Response<Body>) {
        let denied = response.headers().get(DENIED_REASON).and_then(|v| v.to_str().ok());
        if asks_for_captcha(denied) {
            self.deployment().captcha = true;
        }
    }

    fn deployment(&self) -> MutexGuard<'_, Deployment> {
        self.deployment.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An error status as ureq's error, keeping what its body and headers say about this Jira.
    fn status_of(&self, response: &mut Response<Body>) -> Result<(), ureq::Error> {
        let status = response.status().as_u16();
        if status < 400 {
            return Ok(());
        }
        self.note_captcha(response);
        if status == 403
            && let Ok(body) = response.body_mut().read_to_string()
            && says_self_hosted(&self.base, &body)
        {
            self.deployment().self_hosted = Some(true);
        }
        Err(ureq::Error::StatusCode(status))
    }

    /// Whether a Jira project with this key exists and is visible to the account. Errors other
    /// than "not found" (offline, say) come back as Err so callers can ignore them.
    pub fn project_exists(&self, key: &str) -> Result<bool, String> {
        match self.get(&format!("/rest/api/3/project/{key}")) {
            Ok(_) => Ok(true),
            Err(e @ ureq::Error::StatusCode(404)) if !self.self_hosted_after(&e) => Ok(false),
            Err(e) => Err(self.describe(e, "")),
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
                .map_err(|e| self.describe(e, ""))?;
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
        let me = self.get("/rest/api/3/myself").map_err(|e| self.describe(e, ""))?;
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
                e => self.describe(e, ""),
            })?;
        Ok(parse_open(&v, me["accountId"].as_str().unwrap_or("")))
    }

    /// `create`'s POST, answered whatever its status, as Jira says in the body why it refused.
    /// Never asked again: a request that dropped may have made the ticket.
    fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), ureq::Error> {
        self.unless_refused()?;
        let mut resp = self
            .agent
            .ready()
            .post(&format!("{}{path}", self.base))
            .header("Authorization", &format!("Basic {}", self.auth))
            .header("Accept", "application/json")
            .send_json(body)?;
        self.note_captcha(&resp);
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
            ureq::Error::StatusCode(404) if !self.self_hosted_after(&e) => FetchError::Other(
                format!("Jira has no project {project}, or this account can't see it"),
            ),
            e => self.failure(e, ""),
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
                let sent = !tracker::unsent(&e) && !self.refusing();
                let e = self.failure(e, "");
                if sent { e.map(|e| tracker::maybe_created(&e, &place)) } else { e }
            })?;
        match status {
            200..=299 => Ok(parse_created(&v, &self.base, &place)?),
            401 | 403 => Err(self.failure(ureq::Error::StatusCode(status), "")),
            400..=499 => Err(refusal(&v, &project, &type_name, status).into()),
            _ => {
                Err(tracker::maybe_created(&format!("Jira answered HTTP {status}"), &place).into())
            }
        }
    }

    /// Checks the credentials; returns the account's display name.
    pub fn whoami(&self) -> Result<String, String> {
        let v = self.get("/rest/api/3/myself").map_err(|e| self.describe(e, ""))?;
        Ok(v["displayName"].as_str().unwrap_or("?").to_string())
    }
}

impl JiraConfig {
    /// Whether `other` signs in the same way to the same site, so what this one knows holds for it.
    pub(crate) fn same_login(&self, other: &JiraConfig) -> bool {
        (&self.base, &self.auth, &self.token) == (&other.base, &other.auth, &other.token)
    }

    /// Whether `other` is this client or a copy of it, knowing what it knows.
    #[cfg(test)]
    pub(crate) fn same_client(&self, other: &JiraConfig) -> bool {
        Arc::ptr_eq(&self.deployment, &other.deployment)
    }

    fn describe(&self, e: ureq::Error, key: &str) -> String {
        self.failure(e, key).into()
    }

    /// What a request about `key` (empty for none) that failed with `e` means.
    fn failure(&self, e: ureq::Error, key: &str) -> FetchError {
        if self.self_hosted_after(&e) {
            return FetchError::Other(format!(
                "{} is Jira Data Center or Server, which taskman doesn't support yet: it works with Jira Cloud only",
                store::site_host(&self.base)
            ));
        }
        if matches!(e, ureq::Error::StatusCode(401 | 403)) && self.deployment().captcha {
            return FetchError::Auth(format!(
                "Jira wants a CAPTCHA solved before it takes another login: log in to {} in a browser, then save the account in Settings or restart taskman",
                store::site_host(&self.base)
            ));
        }
        match e {
            ureq::Error::StatusCode(401 | 403) => FetchError::Auth(
                "Jira rejected the credentials (check the email and API token)".to_string(),
            ),
            ureq::Error::StatusCode(404) if !key.is_empty() => {
                FetchError::NotFound(format!("{key} not found, or not visible to this account"))
            }
            other => tracker::request_failed(&other, "Jira", &self.base),
        }
    }

    /// Whether a request that failed with `e` failed because this is Data Center or Server, which
    /// taskman's Cloud API isn't on. The failure only says it's worth asking: Cloud answers a
    /// missing ticket with a 404 and a wrong token with a 401 too, and Data Center answers
    /// `/rest/api/3` with a 403, a 404 or a login page depending on its settings and the
    /// credentials.
    fn self_hosted_after(&self, e: &ureq::Error) -> bool {
        use ureq::Error::{Json, StatusCode};
        let status = match e {
            StatusCode(status @ (401 | 403 | 404)) => Some(*status),
            Json(_) => None,
            _ => return false,
        };
        if cloud_site(&self.base) {
            return false;
        }
        let credentials = {
            let mut known = self.deployment();
            if let Some(self_hosted) = known.self_hosted {
                return self_hosted;
            }
            let now = Instant::now();
            if !ask_due(known.asked, now) {
                return false;
            }
            known.asked = Some(now);
            credentials_to_try(status, known.captcha)
        };
        // Not kept unless it told: a gateway's page or a timeout says nothing about the Jira.
        let told = credentials.into_iter().find_map(|credential| self.server_info(credential));
        // Another request may have learned more meanwhile, and a clearer answer stands.
        let mut known = self.deployment();
        known.self_hosted = known.self_hosted.max(told);
        known.self_hosted == Some(true)
    }

    /// What `serverInfo` says when asked with `credential`. All of them go to the site the token is
    /// for. None when it doesn't tell, or credentials shouldn't go any more: another request may
    /// have just found the site self-hosted, or Jira may have asked for a CAPTCHA.
    fn server_info(&self, credential: Credential) -> Option<bool> {
        let auth = match credential {
            Credential::Anonymous => None,
            _ if self.refusing() => return None,
            Credential::Basic => Some(format!("Basic {}", self.auth)),
            Credential::Bearer => Some(format!("Bearer {}", self.token)),
        };
        let mut request = self
            .agent
            .ready()
            .get(&format!("{}/rest/api/2/serverInfo", self.base))
            .config()
            .timeout_global(Some(SERVER_INFO_TIMEOUT))
            .build()
            .header("Accept", "application/json");
        if let Some(auth) = auth {
            request = request.header("Authorization", &auth);
        }
        let mut response = request.call().ok()?;
        self.note_captcha(&response);
        let status = response.status().as_u16();
        self_hosted_by(status, &response.body_mut().read_to_string().ok()?)
    }
}

/// The header Jira says why it refused a login in.
const DENIED_REASON: &str = "X-Authentication-Denied-Reason";

fn asks_for_captcha(denied: Option<&str>) -> bool {
    denied.is_some_and(|reason| reason.contains("CAPTCHA"))
}

/// A way to ask `serverInfo`, as which of them answers depends on how that Jira is set up.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Credential {
    Anonymous,
    /// The account's, as they're sent to Cloud.
    Basic,
    /// The token as a Data Center personal access token.
    Bearer,
}

/// How to ask after a request failed with `status` (None for an answer that wasn't JSON). A 401 has
/// just refused the Basic login, so it isn't sent again, as failed logins add up to a CAPTCHA or a
/// lock. Once Jira wants a CAPTCHA, no credentials go at all.
fn credentials_to_try(status: Option<u16>, captcha: bool) -> Vec<Credential> {
    use Credential::*;
    match (captcha, status) {
        (true, _) => vec![Anonymous],
        (false, Some(401)) => vec![Anonymous, Bearer],
        (false, _) => vec![Anonymous, Basic, Bearer],
    }
}

/// Whether `serverInfo` may be asked again, having last been asked at `asked`.
fn ask_due(asked: Option<Instant>, now: Instant) -> bool {
    asked.is_none_or(|asked| now.duration_since(asked) >= ASK_AGAIN_AFTER)
}

/// Whether a 403's `body` from the Jira at `base` says it's Data Center or Server. Atlassian's own
/// domains are Cloud, whatever a page says.
fn says_self_hosted(base: &str, body: &str) -> bool {
    !cloud_site(base) && self_hosted_by(403, body) == Some(true)
}

/// A site under one of Atlassian's own domains is Cloud, so a failure there needs no asking.
fn cloud_site(base: &str) -> bool {
    let host = store::site_host(base).to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or_default();
    [".atlassian.net", ".jira.com"].iter().any(|domain| host.ends_with(domain))
}

/// What an answer from `/rest/api/2/serverInfo` says about the Jira: `deploymentType` is `Cloud` on
/// Cloud and `Server` on Data Center. Turning Basic authentication off, Data Center's default from
/// Jira 11, answers every request with a 403 saying so, and is something only Data Center and
/// Server can do. None for an answer that doesn't tell, a type it doesn't know included: telling a
/// Cloud user they're self-hosted would send them the wrong way, and the failure stands as it was.
fn self_hosted_by(status: u16, body: &str) -> Option<bool> {
    let v: Value = serde_json::from_str(body).ok()?;
    match status {
        200 => match v["deploymentType"].as_str()?.to_ascii_lowercase().as_str() {
            "cloud" => Some(false),
            "server" | "datacenter" => Some(true),
            _ => None,
        },
        403 => v["message"]
            .as_str()
            .is_some_and(|m| m.contains("Basic Authentication has been disabled"))
            .then_some(true),
        _ => None,
    }
}

pub fn fetch(cfg: &JiraConfig, key: &str) -> Result<Issue, FetchError> {
    let path = format!(
        "/rest/api/3/issue/{key}?fields=summary,status,issuetype,priority,assignee,description,comment"
    );
    let v = cfg.get(&path).map_err(|e| cfg.failure(e, key))?;
    let mut issue = parse_issue(&v);
    // The issue carries its oldest comments only, up to a limit; past it, ask for the newest.
    if issue.comments_left_out > 0 {
        let path =
            format!("/rest/api/3/issue/{key}/comment?orderBy=-created&maxResults={COMMENTS}");
        let page = cfg.get(&path).map_err(|e| cfg.failure(e, key))?;
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
        assert_eq!(fetch(500), Other("Jira failed on its side (HTTP 500); try again later".into()));
        // The comments request fails after the issue's.
        let server = replay::serve(vec![
            (200, include_str!("../tests/fixtures/jira/issue-many-comments.json")),
            (503, ""),
        ]);
        assert_eq!(
            client(&server).fetch("ENG-53", Text::Plain).unwrap_err(),
            Other("Jira failed on its side (HTTP 503); try again later".into())
        );
        // Not JSON, from a proxy say.
        let server = replay::serve(vec![(200, "<html>sign in</html>")]);
        assert!(matches!(
            client(&server).fetch("ENG-52", Text::Plain).unwrap_err(),
            Other(e) if e.starts_with("Jira request failed: ")
        ));
        // Nothing listens.
        let closed = replay::closed();
        let down = Client::Jira(JiraConfig::new(&closed, "ana@example.com", "token"));
        let host = closed.trim_start_matches("http://");
        let refused = format!(
            "can't reach {host}: no connection (no network, or a VPN, proxy or firewall in the way)"
        );
        assert_eq!(down.fetch("ENG-52", Text::Plain).unwrap_err(), Unreachable(refused));
        // The connection drops, and drops again when asked once more.
        let server = replay::serve(vec![(replay::DROP, ""), (replay::DROP, "")]);
        let host = server.url.trim_start_matches("http://");
        assert_eq!(
            client(&server).fetch("ENG-52", Text::Plain).unwrap_err(),
            Unreachable(format!("the connection to {host} broke"))
        );
    }

    #[test]
    fn says_when_jira_doesnt_answer_in_time() {
        let server = replay::serve(vec![(replay::HANG, "")]);
        let quick = net::TrackerAgent::new(std::time::Duration::from_millis(300));
        let cfg =
            JiraConfig { agent: quick, ..JiraConfig::new(&server.url, "ana@example.com", "t") };
        let host = server.url.trim_start_matches("http://");
        let e = super::fetch(&cfg, "ENG-52").unwrap_err();
        let said =
            format!("{host} didn't answer within 20s (a slow network, or Jira is having trouble)");
        assert_eq!(e, Unreachable(said));
        // The TUI tries again by itself, and says so.
        assert!(e.repeated().ends_with("; taskman tries again on the next refresh"));
    }

    const SELF_HOSTED: &str = "is Jira Data Center or Server, which taskman doesn't support yet: it works with Jira Cloud only";

    /// How Jira 11 answers by default: Basic is off, for every path. The 403 says so itself, so
    /// nothing else needs asking, and no more logins are sent once it has.
    #[test]
    fn says_so_when_basic_authentication_is_off_on_data_center() {
        let off = r#"{"message":"Basic Authentication has been disabled on this instance."}"#;
        let server = replay::serve(vec![(403, off)]);
        let c = client(&server);
        for _ in 0..3 {
            let e = c.fetch("ENG-52", Text::Plain).unwrap_err();
            assert!(String::from(e).ends_with(SELF_HOSTED));
        }
        assert_eq!(server.requests().len(), 1);
    }

    /// Once known, neither reading nor creating sends the login again.
    #[test]
    fn known_self_hosted_sends_no_more_credentials() {
        let info = r#"{"deploymentType":"Server"}"#;
        let server = replay::serve(vec![(404, "<html></html>"), (401, "{}"), (200, info)]);
        let cfg = JiraConfig::new(&server.url, "ana@example.com", "t");
        for _ in 0..2 {
            let e = cfg.whoami().unwrap_err();
            assert!(e.ends_with(SELF_HOSTED), "{e}");
        }
        let e = cfg.create("ENG", "Fix login", "").unwrap_err();
        let said = String::from(e);
        assert!(said.ends_with(SELF_HOSTED), "{said}");
        assert!(!said.contains("may have been created"), "{said}");
        assert_eq!(server.requests().len(), 3);
        assert!(matches!(
            cfg.post("/rest/api/3/issue", &json!({})),
            Err(ureq::Error::StatusCode(403))
        ));
        assert_eq!(server.requests().len(), 0);
    }

    /// With Basic on and credentials that work, /rest/api/3 is a 404 page, which mustn't read as a
    /// ticket or project that isn't there.
    #[test]
    fn a_404_from_data_center_isnt_a_missing_ticket() {
        let page = "<!DOCTYPE html><html></html>";
        let info = r#"{"version":"11.3.13","deploymentType":"Server"}"#;
        let server = replay::serve(vec![(404, page), (200, info)]);
        let e = client(&server).fetch("ENG-52", Text::Plain).unwrap_err();
        assert!(String::from(e).ends_with(SELF_HOSTED));

        let server = replay::serve(vec![(404, page), (200, info)]);
        let cfg = JiraConfig::new(&server.url, "ana@example.com", "t");
        let e = cfg.create("ENG", "Fix login", "").unwrap_err();
        assert!(String::from(e).ends_with(SELF_HOSTED));

        let server = replay::serve(vec![(404, page), (200, info)]);
        let cfg = JiraConfig::new(&server.url, "ana@example.com", "t");
        let e = cfg.project_exists("ENG").unwrap_err();
        assert!(e.ends_with(SELF_HOSTED), "{e}");
    }

    /// A personal access token answers as Bearer only. The 401 has just refused the login, so it
    /// isn't sent again.
    #[test]
    fn asks_with_a_personal_access_token_last() {
        let denied = r#"{"errorMessages":[],"errors":{}}"#;
        let server = replay::serve(vec![
            (401, denied),
            (401, denied),
            (200, r#"{"deploymentType":"Server"}"#),
        ]);
        let e = client(&server).fetch("ENG-52", Text::Plain).unwrap_err();
        assert!(String::from(e).ends_with(SELF_HOSTED));
        let asked = server.requests();
        assert_eq!(asked.len(), 3);
        assert_eq!(asked[1].header("authorization"), None);
        assert_eq!(asked[2].header("authorization"), Some("Bearer token-1"));
    }

    #[test]
    fn asks_without_credentials_jira_just_refused() {
        use Credential::*;
        assert_eq!(credentials_to_try(Some(401), false), [Anonymous, Bearer]);
        for status in [Some(403), Some(404), None] {
            assert_eq!(credentials_to_try(status, false), [Anonymous, Basic, Bearer]);
            assert_eq!(credentials_to_try(status, true), [Anonymous]);
        }
        assert_eq!(credentials_to_try(Some(401), true), [Anonymous]);
    }

    const CAPTCHA_FIRST: &str = "Jira wants a CAPTCHA solved before it takes another login";

    /// Each failed login counts towards locking the account, so once Jira wants a CAPTCHA neither
    /// a refresh nor a create sends another, and the message says what to do.
    #[test]
    fn a_captcha_stops_ordinary_requests() {
        let denied = r#"{"errorMessages":[],"errors":{}}"#;
        let server = replay::serve(vec![(replay::CAPTCHA, denied), (401, denied)]);
        let cfg = JiraConfig::new(&server.url, "ana@example.com", "t");
        for _ in 0..2 {
            let e = cfg.whoami().unwrap_err();
            assert!(e.starts_with(CAPTCHA_FIRST) && e.ends_with("or restart taskman"), "{e}");
        }
        let asked = server.requests();
        assert_eq!(asked.len(), 2);
        assert_eq!(asked[1].header("authorization"), None);

        let e = String::from(cfg.create("ENG", "Fix login", "").unwrap_err());
        assert!(e.starts_with(CAPTCHA_FIRST), "{e}");
        assert!(!e.contains("may have been created"), "{e}");
        assert!(matches!(
            cfg.post("/rest/api/3/issue", &json!({})),
            Err(ureq::Error::StatusCode(403))
        ));
        assert_eq!(server.requests().len(), 0);
    }

    /// Another request finding the site self-hosted while this one waits on serverInfo: what's
    /// confirmed stands, and this one sends no credentials after it.
    #[test]
    fn asking_doesnt_undo_or_ignore_what_another_request_found() {
        use std::io::{Read, Write};
        use std::sync::mpsc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let cfg = JiraConfig::new(&url, "ana@example.com", "t");
        let (asked_tx, asked) = mpsc::channel();
        let (found_tx, found) = mpsc::channel::<()>();
        let held = listener.try_clone().unwrap();
        // Holds the first question, without credentials, until the other request has answered.
        let server = std::thread::spawn(move || {
            let (mut stream, _) = held.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).unwrap();
            asked_tx.send(()).unwrap();
            found.recv().unwrap();
            let body = r#"{"errorMessages":[],"errors":{}}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 401 Recorded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        let first = cfg.clone();
        let asking =
            std::thread::spawn(move || first.self_hosted_after(&ureq::Error::StatusCode(403)));
        asked.recv().unwrap();
        cfg.deployment().self_hosted = Some(true);
        found_tx.send(()).unwrap();
        assert!(asking.join().unwrap());
        server.join().unwrap();
        assert_eq!(cfg.deployment().self_hosted, Some(true));
        // Had it gone on to ask with the login, that request would be waiting here.
        listener.set_nonblocking(true).unwrap();
        assert!(listener.accept().is_err());
    }

    /// A CAPTCHA asked for while asking serverInfo stops the rest of the asking.
    #[test]
    fn a_captcha_while_asking_sends_no_more_credentials() {
        let page = "<html>Forbidden</html>";
        let denied = r#"{"errorMessages":[],"errors":{}}"#;
        // The last is for a Bearer request that mustn't be sent, so one that is gets recorded.
        let server = replay::serve(vec![
            (403, page),
            (401, denied),
            (replay::CAPTCHA, denied),
            (200, r#"{"deploymentType":"Server"}"#),
        ]);
        let cfg = JiraConfig::new(&server.url, "ana@example.com", "t");
        let e = cfg.whoami().unwrap_err();
        assert!(e.starts_with(CAPTCHA_FIRST), "{e}");
        let asked = server.requests();
        assert_eq!(asked.len(), 3);
        assert!(asked[2].header("authorization").unwrap().starts_with("Basic "));
    }

    /// A create that's answered with a CAPTCHA stops the next one.
    #[test]
    fn a_captcha_on_a_create_stops_the_next_request() {
        let server = replay::serve(vec![(replay::CAPTCHA, "{}")]);
        let cfg = JiraConfig::new(&server.url, "ana@example.com", "t");
        assert_eq!(cfg.post("/rest/api/3/issue", &json!({})).unwrap().0, 401);
        assert!(matches!(cfg.get("/rest/api/3/myself"), Err(ureq::Error::StatusCode(403))));
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn atlassians_own_domains_arent_told_self_hosted_by_a_page() {
        let off = r#"{"message":"Basic Authentication has been disabled on this instance."}"#;
        assert!(says_self_hosted("https://jira.example.com", off));
        assert!(!says_self_hosted("https://example.atlassian.net", off));
        assert!(!says_self_hosted("https://jira.example.com", "<html>Forbidden</html>"));
    }

    #[test]
    fn knows_when_jira_wants_a_captcha() {
        let header = "CAPTCHA_CHALLENGE; login-url=https://jira.example.com/login.jsp";
        assert!(asks_for_captcha(Some(header)));
        assert!(!asks_for_captcha(Some("AUTHENTICATION_DENIED")));
        assert!(!asks_for_captcha(None));
    }

    /// A gateway's page with the VPN off says nothing about the Jira behind it, so it isn't kept:
    /// asked again once it's due, Jira answers. Until then, nothing more is sent.
    #[test]
    fn an_answer_that_doesnt_tell_isnt_kept() {
        let page = "<html>Forbidden</html>";
        let info = r#"{"deploymentType":"Server"}"#;
        let server = replay::serve(vec![
            (403, page),
            (403, page),
            (403, page),
            (403, page),
            (403, page),
            (404, page),
            (200, info),
        ]);
        let c = client(&server);
        let Client::Jira(jira) = &c else { unreachable!() };
        let rejected = "Jira rejected the credentials (check the email and API token)";
        for _ in 0..2 {
            let e = c.fetch("ENG-52", Text::Plain).unwrap_err();
            assert_eq!(e, Auth(rejected.into()));
        }
        assert_eq!(jira.deployment().self_hosted, None);
        assert_eq!(server.requests().len(), 5);
        jira.deployment().asked = None;
        let e = c.fetch("ENG-52", Text::Plain).unwrap_err();
        assert!(String::from(e).ends_with(SELF_HOSTED));
        assert_eq!(server.requests().len(), 2);
    }

    #[test]
    fn asks_again_only_once_its_due() {
        let now = Instant::now();
        assert!(ask_due(None, now));
        assert!(!ask_due(Some(now), now));
        let just_before = ASK_AGAIN_AFTER.saturating_sub(Duration::from_secs(1));
        assert!(!ask_due(Some(now), now + just_before));
        assert!(ask_due(Some(now), now + ASK_AGAIN_AFTER));
    }

    /// A Cloud site on a domain of its own keeps Cloud's words, and asks once.
    #[test]
    fn a_failure_on_cloud_stays_what_it_was() {
        let cloud = r#"{"deploymentType":"Cloud"}"#;
        let server = replay::serve(vec![(404, "{}"), (200, cloud), (404, "{}")]);
        let c = client(&server);
        for _ in 0..2 {
            assert_eq!(
                c.fetch("ENG-52", Text::Plain).unwrap_err(),
                NotFound("ENG-52 not found, or not visible to this account".into())
            );
        }
        assert_eq!(server.requests().len(), 3);
    }

    #[test]
    fn atlassians_own_domains_arent_asked() {
        assert!(cloud_site("https://example.atlassian.net"));
        assert!(cloud_site("https://EXAMPLE.atlassian.net:443"));
        assert!(cloud_site("https://example.jira.com"));
        assert!(!cloud_site("https://jira.example.com"));
        assert!(!cloud_site("https://atlassian.net.example.com"));
    }

    #[test]
    fn server_info_tells_cloud_from_self_hosted() {
        assert_eq!(self_hosted_by(200, r#"{"deploymentType":"Server"}"#), Some(true));
        assert_eq!(self_hosted_by(200, r#"{"deploymentType":"DataCenter"}"#), Some(true));
        assert_eq!(self_hosted_by(200, r#"{"deploymentType":"Cloud"}"#), Some(false));
        assert_eq!(self_hosted_by(200, r#"{"deploymentType":"Something new"}"#), None);
        let off = r#"{"message":"Basic Authentication has been disabled on this instance."}"#;
        assert_eq!(self_hosted_by(403, off), Some(true));
        for (status, body) in [
            (200, "{}"),
            (200, r#"{"deploymentType":""}"#),
            (200, "<html></html>"),
            (401, r#"{"deploymentType":"Server"}"#),
            (403, r#"{"message":"Forbidden"}"#),
            (403, "<html>Forbidden (403)</html>"),
            (404, "{}"),
        ] {
            assert_eq!(self_hosted_by(status, body), None, "{status} {body}");
        }
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
            (200, r#"{"deploymentType":"Cloud"}"#),
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
                "GET /rest/api/2/serverInfo HTTP/1.1",
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

        /// Only a 200 with a type, or the 403 saying Basic is off, tells anything.
        #[test]
        fn server_info_tells_only_when_it_says(status in 100u16..600, body in ".{0,40}") {
            let told = self_hosted_by(status, &body);
            if status != 200 && status != 403 {
                prop_assert_eq!(told, None);
            }
        }

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
