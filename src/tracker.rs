//! Everything that depends on the tracker: the issue Jira, Linear, Local files and GitHub Issues
//! all produce, the client that fetches one, the TUI's tokens and clients, and what each provider
//! does differently.

use crate::github_issues::{self, GithubConfig, Repo};
use crate::jira::{self, JiraConfig};
use crate::linear::{self, LinearConfig};
use crate::local;
use crate::store::{self, Account, Config, Project, Provider, Tracker};
use crate::ticket_key;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

#[derive(Clone, Debug, Default)]
pub struct Issue {
    pub summary: String,
    pub status: String,
    pub status_category: String,
    pub issue_type: String,
    pub priority: String,
    pub assignee: String,
    pub description: String,
    /// Newest first: all of them, or the newest `COMMENTS`.
    pub comments: Vec<Comment>,
    /// Older comments the tracker has that weren't fetched.
    pub comments_left_out: usize,
    /// The tracker's name for the ticket's git branch (Linear's `branchName`, a Local files
    /// ticket's file name); empty for Jira.
    pub branch: String,
    /// The pull requests that close it (GitHub's closing references); empty for other trackers.
    pub prs: Vec<LinkedPr>,
}

#[derive(Clone, Debug)]
pub struct LinkedPr {
    pub number: u64,
    pub state: crate::github::PrState,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub created: String,
    pub body: String,
}

/// Comments fetched per issue, the newest ones; Jira and Linear say how many older ones they
/// left out.
pub const COMMENTS: usize = 100;

/// How an issue's description and comments come back: cleaned up to plain text for a panel, or as
/// the tracker wrote them (Linear's and Local files' Markdown, links and code included) for
/// `taskman status`. Jira's rich text is plain text either way.
#[derive(Clone, Copy)]
pub enum Text {
    Plain,
    Markdown,
}

impl Text {
    /// What Markdown goes through.
    pub fn render(self) -> fn(&str) -> String {
        match self {
            Text::Plain => linear::markdown_to_text,
            Text::Markdown => |md| md.to_string(),
        }
    }
}

/// Why a request to a tracker failed. The kind is what a command acts on (`taskman add` refuses
/// a key that's `NotFound` and adds one unchecked otherwise); the text is what it says, often the
/// tracker's own words, so it's printed through `untrusted`.
#[derive(Clone, Debug, PartialEq)]
pub enum FetchError {
    /// The tracker answered that the issue doesn't exist, or the account can't see it.
    NotFound(String),
    /// No answer came: the network, or a sandbox, cut the request off.
    Unreachable(String),
    /// The tracker refused the credentials, or there were none to send (a token that couldn't be
    /// read, no account picked).
    Auth(String),
    Other(String),
}

impl FetchError {
    pub fn message(&self) -> &str {
        match self {
            FetchError::NotFound(m)
            | FetchError::Unreachable(m)
            | FetchError::Auth(m)
            | FetchError::Other(m) => m,
        }
    }

    /// The same kind of error, saying `f` of what it said.
    pub fn map(self, f: impl FnOnce(String) -> String) -> FetchError {
        match self {
            FetchError::NotFound(m) => FetchError::NotFound(f(m)),
            FetchError::Unreachable(m) => FetchError::Unreachable(f(m)),
            FetchError::Auth(m) => FetchError::Auth(f(m)),
            FetchError::Other(m) => FetchError::Other(f(m)),
        }
    }

    /// From a request that failed with `e`, saying `message`.
    pub(crate) fn request(e: &ureq::Error, message: String) -> FetchError {
        if cut_off(e) { FetchError::Unreachable(message) } else { FetchError::Other(message) }
    }

    /// The message, for a fetch the TUI repeats by itself: one that got no answer says so.
    pub fn repeated(self) -> String {
        match self {
            FetchError::Unreachable(m) => format!("{m}; taskman tries again on the next refresh"),
            e => e.into(),
        }
    }
}

/// Why a request to `tracker` (`Jira`) at `url` failed with `e`, in words that say where the
/// problem is: no network, a name that doesn't resolve, a tracker that didn't answer or failed on
/// its side. Anything else keeps ureq's words.
pub(crate) fn request_failed(e: &ureq::Error, tracker: &str, url: &str) -> FetchError {
    use std::io::ErrorKind::*;
    use ureq::Timeout;
    let host = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = host.split('/').next().unwrap_or(host);
    let unreachable = |why: &str| FetchError::Unreachable(format!("can't reach {host}: {why}"));
    let no_name = "its name doesn't resolve (no network, or a DNS or VPN problem)";
    let no_route = "no connection (no network, or a VPN, proxy or firewall in the way)";
    let io = |kinds: &[std::io::ErrorKind]| match e {
        ureq::Error::Io(io) => kinds.contains(&io.kind()),
        _ => false,
    };
    match e {
        ureq::Error::HostNotFound | ureq::Error::Timeout(Timeout::Resolve) => unreachable(no_name),
        // getaddrinfo's failures come as an error of no particular kind, std's words first.
        ureq::Error::Io(io) if io.to_string().starts_with("failed to lookup address") => {
            unreachable(no_name)
        }
        ureq::Error::ConnectionFailed | ureq::Error::Timeout(Timeout::Connect) => {
            unreachable(no_route)
        }
        _ if io(&[
            ConnectionRefused,
            PermissionDenied,
            NetworkUnreachable,
            HostUnreachable,
            AddrNotAvailable,
            NetworkDown,
        ]) =>
        {
            unreachable(no_route)
        }
        ureq::Error::Timeout(_) => FetchError::Unreachable(format!(
            "{host} didn't answer within {}s (a slow network, or {tracker} is having trouble)",
            crate::net::TRACKER_TIMEOUT.as_secs()
        )),
        _ if io(&[TimedOut]) => FetchError::Unreachable(format!(
            "{host} didn't answer (a slow network, or {tracker} is having trouble)"
        )),
        _ if io(&[ConnectionReset, ConnectionAborted, UnexpectedEof, BrokenPipe]) => {
            FetchError::Unreachable(format!("the connection to {host} broke"))
        }
        ureq::Error::StatusCode(status @ 500..=599) => server_failed(tracker, *status).into(),
        other => FetchError::request(other, format!("{tracker} request failed: {other}")),
    }
}

/// A tracker that answered `status`, 500 or more: its own trouble, not the setup's.
pub(crate) fn server_failed(tracker: &str, status: u16) -> String {
    format!("{tracker} failed on its side (HTTP {status}); try again later")
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl From<String> for FetchError {
    fn from(message: String) -> Self {
        FetchError::Other(message)
    }
}

impl From<FetchError> for String {
    fn from(e: FetchError) -> Self {
        match e {
            FetchError::NotFound(m)
            | FetchError::Unreachable(m)
            | FetchError::Auth(m)
            | FetchError::Other(m) => m,
        }
    }
}

/// What fetches a project's issues.
#[derive(Clone)]
pub enum Client {
    Jira(JiraConfig),
    Linear(LinearConfig),
    /// A Local files board, in the project's own folder.
    Local(PathBuf),
    /// A GitHub repository's issues, through gh.
    Github(GithubConfig),
}

impl Client {
    /// The client for `tracker` in the project folder `repo`, with the account and token it uses;
    /// None when it needs an account (`Provider::needs_account`) and has none.
    pub fn new(
        tracker: &Tracker,
        repo: &Path,
        account: Option<(&Account, &str)>,
    ) -> Option<Client> {
        match (tracker.provider, account) {
            (Provider::Local, _) => tracker.board(repo).map(Client::Local),
            (Provider::Github, _) => GithubConfig::new(&tracker.site, repo).map(Client::Github),
            (Provider::Jira, Some((a, token))) => Some(Client::Jira(JiraConfig::new(
                &store::jira_base(&tracker.site),
                &a.email,
                token,
            ))),
            (Provider::Linear, Some((_, token))) => Some(Client::Linear(LinearConfig::new(token))),
            (Provider::Jira | Provider::Linear, None) => None,
        }
    }

    /// The project's client for the CLI commands, from the same account choice and token sources
    /// as the TUI, reading the token now; None for a project without a tracker. A token that
    /// can't be read is `Auth`, whatever its command printed.
    pub fn for_project(config: &Config, project: &Project) -> Result<Option<Client>, FetchError> {
        let Some(t) = &project.tracker else { return Ok(None) };
        if !t.provider.needs_account() {
            return Ok(Client::new(t, &project.repo, None));
        }
        let Some(i) = config.account_for(t) else {
            return Err(FetchError::Auth(format!(
                "no {} account picked for project {}; pick or add one in its settings in taskman ({} ,)",
                t.provider.label(),
                project.name,
                crate::keys::prefix()
            )));
        };
        let a = &config.accounts[i];
        let token = a.resolve_token().map_err(|e| {
            FetchError::Auth(format!("{e} (the token of {} account {})", t.provider.label(), a.id))
        })?;
        Ok(Client::new(t, &project.repo, Some((a, &token))))
    }

    pub fn fetch(&self, key: &str, text: Text) -> Result<Issue, FetchError> {
        let text = text.render();
        match self {
            Client::Jira(c) => jira::fetch(c, key),
            Client::Linear(c) => linear::fetch(c, key, text),
            Client::Local(board) => local::fetch(board, key, text).map(|(i, _)| i),
            Client::Github(c) => c.fetch(key, text),
        }
    }

    /// A Local files client's board.
    pub fn board(&self) -> Option<&Path> {
        match self {
            Client::Local(board) => Some(board),
            Client::Jira(_) | Client::Linear(_) | Client::Github(_) => None,
        }
    }

    /// Whether issues come over the network, fetched on a timer; a board is read again on every
    /// status poll instead, when its files change.
    pub fn remote(&self) -> bool {
        self.board().is_none()
    }

    /// What the account can pick the project's ticket prefix from (`Provider::lists_choices`):
    /// Jira's projects or Linear's teams it can see, sorted by key. None for the others, which
    /// have nothing to look up.
    pub fn choices(&self) -> Option<Result<Vec<Choice>, String>> {
        let choices = match self {
            Client::Jira(c) => c.projects(),
            Client::Linear(c) => c.teams(),
            Client::Local(_) | Client::Github(_) => return None,
        };
        Some(choices.map(tidy_choices))
    }

    /// Whether the project `prefix` names exists there (`Provider::checks_prefix`); Err when that
    /// couldn't be told (offline, say).
    pub fn project_exists(&self, prefix: &str) -> Result<bool, String> {
        match self {
            Client::Jira(c) => c.project_exists(prefix),
            Client::Linear(_) | Client::Local(_) | Client::Github(_) => Ok(true),
        }
    }
}

/// What a project's ticket prefix can be: a Jira project or Linear team the account can see, with
/// its key (the prefix) and its name, as the project form lists them.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub key: String,
    pub name: String,
}

/// Pages a listing of choices may take before taskman stops asking: a thousand projects or more,
/// and the key can still be typed.
pub const CHOICE_PAGES: usize = 10;

/// The choices `input` picks, for the project form: those whose key starts with it, or whose name
/// has every word typed, ignoring case. Empty input lists them all; the order is kept.
pub fn choosing<'a>(choices: &'a [Choice], input: &str) -> Vec<&'a Choice> {
    let s = input.trim().to_lowercase();
    let words: Vec<&str> = s.split_whitespace().collect();
    choices
        .iter()
        .filter(|c| {
            let name = c.name.to_lowercase();
            c.key.to_lowercase().starts_with(&s) || words.iter().all(|w| name.contains(w))
        })
        .collect()
}

/// Keys that read as a ticket prefix, each once, sorted: one the tracker wrote some other way
/// can't be a prefix, and is left out.
pub(crate) fn tidy_choices(mut choices: Vec<Choice>) -> Vec<Choice> {
    choices.retain(|c| ticket_key::is_prefix(&c.key));
    choices.sort_by(|a, b| a.key.cmp(&b.key));
    choices.dedup_by(|a, b| a.key == b.key);
    choices
}

/// An open issue, as the Add form lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    /// Its ticket key: `ENG-12`, or `WEB-12` for GitHub's issue #12 (`Provider::written_key`).
    pub key: String,
    pub title: String,
    /// Assigned to you: the account, or gh's login, the tracker is reached with.
    pub mine: bool,
}

impl Listed {
    /// The number in its key: 12 in `ENG-12`.
    pub fn number(&self) -> Option<u64> {
        ticket_key::number(&self.key)
    }
}

/// Puts the issues assigned to you first, each group keeping the tracker's order.
pub fn mine_first(issues: &mut [Listed]) {
    issues.sort_by_key(|l| !l.mine);
}

/// The open issues `input` picks, for the Add form: those whose number starts with the digits
/// typed (after an optional `#`), or whose title has every word typed, ignoring case. Empty input
/// lists them all.
pub fn matching<'a>(issues: &'a [Listed], input: &str) -> Vec<&'a Listed> {
    let s = input.trim();
    let digits = s.strip_prefix('#').unwrap_or(s);
    if ticket_key::is_number(digits) {
        return issues
            .iter()
            .filter(|i| i.number().is_some_and(|n| n.to_string().starts_with(digits)))
            .collect();
    }
    let words: Vec<String> = s.split_whitespace().map(str::to_lowercase).collect();
    issues
        .iter()
        .filter(|i| {
            let title = i.title.to_lowercase();
            words.iter().all(|w| title.contains(w.as_str()))
        })
        .collect()
}

impl Client {
    /// The open issues the Add form lists (`Tracker::lists_open`), those assigned to you first:
    /// Jira's project and Linear's team whose key is `prefix`, the GitHub repository's, or the
    /// board's tickets not done. Up to 100 from a tracker, the most recently updated.
    pub fn open_issues(&self, prefix: &str) -> Result<Vec<Listed>, String> {
        let mut issues = match self {
            Client::Jira(c) => c.open_issues(prefix)?,
            Client::Linear(c) => c.open_issues(prefix)?,
            Client::Local(board) => local::open_issues(board),
            Client::Github(c) => c.open_issues(prefix)?,
        };
        // A key names a worktree and a branch: one the tracker wrote some other way is left out.
        issues.retain(|l| ticket_key::well_formed(&l.key));
        mine_first(&mut issues);
        Ok(issues)
    }

    /// Creates a ticket for `taskman add --new`: in Linear's team and Jira's project whose key is
    /// `prefix`, the GitHub repository, or the board's todo column. `description` is Markdown,
    /// empty for none. A failed create is never tried again, and one that may have reached the
    /// tracker says so: a command run again after it could otherwise make the ticket twice.
    pub fn create(
        &self,
        prefix: &str,
        title: &str,
        description: &str,
    ) -> Result<Created, local::WriteError> {
        let title = title.trim();
        if title.is_empty() {
            return Err("give the new ticket a title".into());
        }
        if !ticket_key::is_prefix(prefix) {
            return Err(format!(
                "the project has no ticket prefix; set one in its settings in taskman ({} ,)",
                crate::keys::prefix()
            )
            .into());
        }
        let created = match self {
            Client::Jira(c) => c.create(prefix, title, description),
            Client::Linear(c) => c.create(prefix, title, description),
            Client::Github(c) => c.create(prefix, title, description),
            Client::Local(board) => {
                return local::create(board, prefix, title, description)
                    .map(|f| Created { key: f.key, at: store::tilde(&f.path) });
            }
        };
        created.map_err(local::WriteError::from)
    }
}

/// A ticket `Client::create` made: its key, and where it is (its link, or its file on a board).
#[derive(Clone, Debug, PartialEq)]
pub struct Created {
    pub key: String,
    pub at: String,
}

impl Created {
    /// From the key and link a tracker answered with, which must read as a key. `place` names
    /// where the ticket was made, for when they don't.
    pub(crate) fn read(key: Option<&str>, at: &str, place: &str) -> Result<Created, String> {
        match key {
            Some(key) if ticket_key::well_formed(key) => {
                Ok(Created { key: key.into(), at: at.into() })
            }
            _ => Err(maybe_created(
                &format!("the answer gave no ticket key taskman can read ({key:?})"),
                place,
            )),
        }
    }
}

/// Whether a request that failed with `e` never reached the server, so it can't have done
/// anything there: the name didn't resolve, or the connection wasn't made. A connection that
/// broke later may have carried it.
pub(crate) fn unsent(e: &ureq::Error) -> bool {
    use std::io::ErrorKind;
    use ureq::Timeout;
    if let ureq::Error::Io(io) = e {
        return matches!(
            io.kind(),
            ErrorKind::ConnectionRefused
                | ErrorKind::PermissionDenied
                | ErrorKind::NetworkUnreachable
                | ErrorKind::HostUnreachable
                | ErrorKind::AddrNotAvailable
                | ErrorKind::NetworkDown
        );
    }
    matches!(
        e,
        ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
            | ureq::Error::BadUri(_)
            | ureq::Error::Http(_)
            | ureq::Error::InvalidProxyUrl
            | ureq::Error::ConnectProxyFailed(_)
            | ureq::Error::RequireHttpsOnly(_)
            | ureq::Error::Timeout(Timeout::Resolve | Timeout::Connect)
    )
}

/// Whether a request that failed with `e` got no answer at all, as when Codex's sandbox cuts the
/// network off: the name didn't resolve, the connection failed or broke, or the time ran out.
pub(crate) fn cut_off(e: &ureq::Error) -> bool {
    use ureq::Timeout;
    matches!(
        e,
        ureq::Error::Io(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
            | ureq::Error::Timeout(Timeout::Resolve | Timeout::Connect | Timeout::Global)
    )
}

/// A create that failed after it may have reached the tracker (it timed out, the connection
/// dropped, the tracker failed or answered with something unreadable): it may have made the
/// ticket, so it must not simply be run again. `place` is where to look, like `ENG in Jira`.
pub(crate) fn maybe_created(e: &str, place: &str) -> String {
    format!(
        "{e}. The ticket may have been created anyway: look in {place} before running this again, and add it with taskman add KEY if it's there"
    )
}

/// Checks an account's `token`: who it belongs to, and for Linear its workspace. Jira tests it
/// against `site`.
pub fn whoami(account: &Account, site: &str, token: &str) -> Result<(String, String), String> {
    match account.provider {
        Provider::Jira => JiraConfig::new(&store::jira_base(site), &account.email, token)
            .whoami()
            .map(|name| (name, String::new())),
        Provider::Linear => LinearConfig::new(token).whoami(),
        Provider::Local | Provider::Github => {
            Err(format!("{} need no account", account.provider.label()))
        }
    }
}

// ---- the TUI's clients -----------------------------------------------------------------------

/// How a ticket's tracker panel can be filled.
pub enum Access {
    Client(Client),
    NoTracker,
    /// The project's tracker needs an account and has none: its provider and site.
    NoAccount(Provider, String),
    /// The account's token is still being read.
    Reading,
    Failed(String),
}

impl Access {
    /// The Local files board the ticket is on.
    pub fn board(&self) -> Option<&Path> {
        match self {
            Access::Client(c) => c.board(),
            _ => None,
        }
    }
}

/// The TUI's tokens, its clients built from them, and the Local files boards' listings.
#[derive(Default)]
pub struct Clients {
    /// Tokens by source (an account's `env:` or `cmd:`): the token or why there isn't one, None
    /// while it's being read. A source is read once, again when it failed, or when its account is
    /// saved; a password manager isn't asked on every change to another account or a project.
    tokens: HashMap<String, Option<Result<String, String>>>,
    /// Threads waiting for a token being read, by source.
    waiters: HashMap<String, Vec<mpsc::Sender<Result<String, String>>>>,
    /// Clients by (account index, site host), for every project whose tracker uses an account.
    clients: HashMap<(usize, String), Client>,
    /// Local files boards' listings, shared by the status poll and the Add form.
    boards: RefCell<HashMap<PathBuf, local::Cache>>,
}

impl Clients {
    /// Has `read` read, on a thread each, the tokens of accounts not read yet or whose read
    /// failed (it passes them back to `token_read`), then builds the clients projects need. Runs
    /// at startup and whenever accounts or projects change.
    pub fn rebuild<F>(&mut self, config: &Config, projects: &[Project], read: F)
    where
        F: Fn(Account) + Clone + Send + 'static,
    {
        let sources: HashSet<&String> = config.accounts.iter().map(|a| &a.token).collect();
        self.tokens.retain(|s, _| sources.contains(s));
        for a in &config.accounts {
            if matches!(self.tokens.get(&a.token), Some(None | Some(Ok(_)))) {
                continue;
            }
            self.tokens.insert(a.token.clone(), None);
            let (a, read) = (a.clone(), read.clone());
            std::thread::spawn(move || read(a));
        }
        self.build(config, projects);
    }

    /// A token from `source` was read: hands it to the threads waiting for it and builds the
    /// clients that use it.
    pub fn token_read(
        &mut self,
        source: &str,
        token: Result<String, String>,
        config: &Config,
        projects: &[Project],
    ) {
        for w in self.waiters.remove(source).unwrap_or_default() {
            let _ = w.send(token.clone());
        }
        self.tokens.insert(source.to_string(), Some(token));
        self.build(config, projects);
    }

    /// The token read from `source`, if it was read and worked.
    pub fn token(&self, source: &str) -> Option<&str> {
        match self.tokens.get(source) {
            Some(Some(Ok(token))) => Some(token),
            _ => None,
        }
    }

    /// Forgets the token from `source`, so the next rebuild reads it again.
    pub fn forget(&mut self, source: &str) {
        self.tokens.remove(source);
    }

    /// The clients for the tokens read so far.
    fn build(&mut self, config: &Config, projects: &[Project]) {
        self.clients.clear();
        for p in projects {
            let Some(t) = &p.tracker else { continue };
            let Some(i) = config.account_for(t) else { continue };
            let a = &config.accounts[i];
            if let Some(token) = self.token(&a.token)
                && let Some(client) = Client::new(t, &p.repo, Some((a, token)))
            {
                self.clients.insert((i, store::site_host(&t.site)), client);
            }
        }
    }

    /// How the tracker panel of a ticket in `project` can be filled.
    pub fn access(&self, config: &Config, project: Option<&Project>) -> Access {
        let Some(p) = project else { return Access::NoTracker };
        let Some(tracker) = &p.tracker else { return Access::NoTracker };
        if !tracker.provider.needs_account() {
            return Client::new(tracker, &p.repo, None).map_or(Access::NoTracker, Access::Client);
        }
        let Some(i) = config.account_for(tracker) else {
            return Access::NoAccount(tracker.provider, tracker.site.clone());
        };
        let token = match self.tokens.get(&config.accounts[i].token) {
            Some(Some(token)) => token,
            _ => return Access::Reading,
        };
        match (token, self.clients.get(&(i, store::site_host(&tracker.site)))) {
            (Err(e), _) => Access::Failed(e.clone()),
            (Ok(_), Some(c)) => Access::Client(c.clone()),
            (Ok(_), None) => {
                Access::Failed(format!("the {} client isn't ready yet", tracker.provider.label()))
            }
        }
    }

    /// For a project whose account's token is still being read: waits for it, then gives the
    /// access. For a worktree's branch name, which needs the title; it runs once.
    pub fn access_later(
        &mut self,
        config: &Config,
        project: &Project,
    ) -> Option<impl FnOnce() -> Access + Send + 'static> {
        let tracker = project.tracker.clone()?;
        let a = config.accounts.get(config.account_for(&tracker)?)?.clone();
        // Only a read under way sends the token.
        if !matches!(self.tokens.get(&a.token), Some(None)) {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        self.waiters.entry(a.token.clone()).or_default().push(tx);
        let repo = project.repo.clone();
        Some(move || match rx.recv() {
            Ok(Ok(token)) => Client::new(&tracker, &repo, Some((&a, &token)))
                .map_or(Access::NoTracker, Access::Client),
            Ok(Err(e)) => Access::Failed(e),
            Err(_) => Access::Reading,
        })
    }

    /// Has `project`'s tracker answer through `client`: a test's, which `replay` serves.
    #[cfg(test)]
    pub(crate) fn serve(&mut self, config: &Config, project: &Project, client: Client) {
        let t = project.tracker.as_ref().expect("a project with a tracker");
        let i = config.account_for(t).expect("an account for the tracker");
        self.clients.insert((i, store::site_host(&t.site)), client);
    }

    /// The board's files, from the cache: listed again only when one of its folders changed, so
    /// the Add form can ask on every keystroke and the status poll on every poll.
    pub fn board_files(&self, board: &Path) -> Vec<local::File> {
        self.boards.borrow_mut().entry(board.to_path_buf()).or_default().files(board).to_vec()
    }

    /// `board_files` without the copy.
    pub fn board_listing(&mut self, board: &Path) -> &[local::File] {
        self.boards.get_mut().entry(board.to_path_buf()).or_default().files(board)
    }
}

// ---- what each provider does ---------------------------------------------------------------
// `Provider` and `Tracker` are saved state (`store.rs`); every difference between providers is a
// method here, so a new provider is a variant there and one arm per method here.

impl Provider {
    pub const ALL: [Provider; 4] =
        [Provider::Jira, Provider::Linear, Provider::Github, Provider::Local];

    pub fn label(self) -> &'static str {
        match self {
            Provider::Jira => "Jira",
            Provider::Linear => "Linear",
            Provider::Local => "Local files",
            Provider::Github => "GitHub Issues",
        }
    }

    /// What a comment on one of its tickets is called beside the PR's: GitHub's are on the issue,
    /// as the PR's are on GitHub too. Local files have no comments, so theirs is never shown.
    pub fn comment_noun(self) -> &'static str {
        match self {
            Provider::Jira => "Jira comment",
            Provider::Linear => "Linear comment",
            Provider::Github => "issue comment",
            Provider::Local => "comment",
        }
    }

    /// Whether it's reached with an account's credentials; Local files are read from the project,
    /// and GitHub Issues through gh's own login.
    pub fn needs_account(self) -> bool {
        match self {
            Provider::Jira | Provider::Linear => true,
            Provider::Local | Provider::Github => false,
        }
    }

    /// Whether one account works on any site: a Jira token belongs to a person, a Linear key to
    /// one workspace.
    pub fn account_spans_sites(self) -> bool {
        match self {
            Provider::Jira => true,
            Provider::Linear | Provider::Local | Provider::Github => false,
        }
    }

    /// Whether its tickets are files on a board in the project folder, which the tracker names by
    /// its folder (`Tracker::folder`) rather than a site.
    pub fn has_board(self) -> bool {
        match self {
            Provider::Local => true,
            Provider::Jira | Provider::Linear | Provider::Github => false,
        }
    }

    /// Whether its site is the repository of the project's GitHub remote, read when the project is
    /// saved rather than typed: gh decides which repository a folder is, as for the PR panel.
    pub fn site_from_remote(self) -> bool {
        match self {
            Provider::Github => true,
            Provider::Jira | Provider::Linear | Provider::Local => false,
        }
    }

    /// A ticket key as the tracker writes it: GitHub's `#12` for `WEB-12`.
    pub fn written_key(self, key: &str) -> String {
        match (self, ticket_key::number(key)) {
            (Provider::Github, Some(n)) => format!("#{n}"),
            _ => key.to_string(),
        }
    }

    /// The project form's field for the ticket prefix: the Jira project or Linear team it's the
    /// key of, picked from those the account can see (`lists_choices`), or typed for the others.
    pub fn prefix_field(self) -> &'static str {
        match self {
            Provider::Jira => "Project",
            Provider::Linear => "Team",
            Provider::Local | Provider::Github => "Ticket prefix",
        }
    }

    /// Whether the project form lists what the account can see to pick the prefix from
    /// (`Client::choices`), once it can sign in.
    pub fn lists_choices(self) -> bool {
        match self {
            Provider::Jira | Provider::Linear => true,
            Provider::Local | Provider::Github => false,
        }
    }

    /// The project form's field for the site, if it's typed: Jira's site, Linear's workspace.
    pub fn site_field(self) -> Option<&'static str> {
        match self {
            Provider::Jira => Some("Site"),
            Provider::Linear => Some("Workspace"),
            Provider::Local | Provider::Github => None,
        }
    }

    /// What to type in the site field when it's empty.
    pub fn site_missing(self) -> &'static str {
        match self {
            Provider::Jira => "Enter the Jira site, like your-site.atlassian.net",
            Provider::Linear => "Enter the Linear workspace (the part after linear.app/)",
            Provider::Local | Provider::Github => "",
        }
    }

    /// Whether an account signs in with an email as well as its token: Jira's API tokens do.
    pub fn signs_in_with_email(self) -> bool {
        match self {
            Provider::Jira => true,
            Provider::Linear | Provider::Local | Provider::Github => false,
        }
    }

    /// The account form's hint under Email, where it's shown (`signs_in_with_email`).
    pub fn email_hint(self) -> &'static str {
        match self {
            Provider::Jira => "the email you sign in to Atlassian with",
            Provider::Linear | Provider::Local | Provider::Github => "",
        }
    }

    /// The environment variable a new account's email is suggested from, where it signs in with
    /// one.
    pub fn email_var(self) -> Option<&'static str> {
        match self {
            Provider::Jira => Some("JIRA_EMAIL"),
            Provider::Linear | Provider::Local | Provider::Github => None,
        }
    }

    /// Whether the project form checks that the ticket prefix names a project there.
    pub fn checks_prefix(self) -> bool {
        match self {
            Provider::Jira => true,
            Provider::Linear | Provider::Local | Provider::Github => false,
        }
    }

    /// What the `{tracker}` branch format takes a ticket's branch name from (`Issue::branch`);
    /// None when the tracker names no branch, and `{KEY}-{slug}` stands in.
    pub fn branch_source(self) -> Option<&'static str> {
        match self {
            Provider::Jira | Provider::Github => None,
            Provider::Linear => Some("Linear's branch name"),
            Provider::Local => Some("the ticket file's name"),
        }
    }

    /// A branch name the tracker might give `key`, for the branch format's example.
    pub fn example_branch(self, key: &str) -> String {
        match self {
            Provider::Local | Provider::Github => format!("{key}-fix-login"),
            Provider::Jira | Provider::Linear => format!("you/{}-fix-login", key.to_lowercase()),
        }
    }

    /// The project form's hint under Tracker once this one is picked.
    pub fn hint(self) -> &'static str {
        match self {
            Provider::Local => "markdown files in the project, no account · ←/→ to change",
            Provider::Github => "the GitHub remote's issues, through gh · ←/→ to change",
            Provider::Jira | Provider::Linear => "←/→ to change; None keeps the project to agents",
        }
    }

    /// The project form's hint under Site once this one is picked.
    pub fn site_hint(self) -> &'static str {
        match self {
            Provider::Jira => "your Jira site, like your-site.atlassian.net",
            Provider::Linear => "the part after linear.app/ in its address",
            Provider::Local | Provider::Github => "",
        }
    }

    /// The account form's hint under Site, where the account's site is what its token is tested
    /// against (Jira) or the one workspace its key belongs to (Linear).
    pub fn account_site_hint(self) -> &'static str {
        match self {
            Provider::Jira => "where to test the token, like your-site.atlassian.net",
            Provider::Linear => "workspace: the part after linear.app/",
            Provider::Local | Provider::Github => "",
        }
    }

    /// The environment variable the account form suggests for its token.
    pub fn token_var(self) -> &'static str {
        match self {
            Provider::Jira => "JIRA_API_TOKEN",
            Provider::Linear => "LINEAR_API_KEY",
            Provider::Local | Provider::Github => "",
        }
    }

    /// The page that makes a token. Linear's is in the workspace, `site` as typed: until there's
    /// one, `<workspace>` stands in for it.
    pub fn token_page(self, site: &str) -> Option<String> {
        match self {
            Provider::Jira => {
                Some("https://id.atlassian.com/manage-profile/security/api-tokens".into())
            }
            Provider::Linear => {
                // Typed by the user, but kept to a slug's characters all the same.
                let ws: String = site
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                    .collect();
                let ws = if ws.is_empty() { "<workspace>" } else { &ws };
                Some(format!("https://linear.app/{ws}/settings/account/security"))
            }
            Provider::Local | Provider::Github => None,
        }
    }

    /// The project form's hint under Ticket prefix.
    pub fn prefix_hint(self) -> &'static str {
        match self {
            Provider::Local => "WP for WP-3; new tickets take the next number",
            Provider::Github => "WEB for WEB-12, issue #12; left empty, from the repo's name",
            Provider::Jira | Provider::Linear => "ENG for ENG-12; then typing 12 adds it",
        }
    }

    /// A site as typed into a form, as it's saved: a Jira site becomes `https://host`.
    pub fn site_from_input(self, site: String) -> String {
        match self {
            Provider::Jira if !site.is_empty() => store::jira_base(&site),
            _ => site,
        }
    }

    /// A saved site as a form shows it: a Jira site by its host.
    pub fn site_for_input(self, site: &str) -> String {
        match self {
            Provider::Jira => store::site_host(site),
            Provider::Linear | Provider::Local | Provider::Github => site.to_string(),
        }
    }
}

impl Tracker {
    /// A ticket's page on this tracker; Local files have none.
    pub fn issue_url(&self, key: &str) -> Option<String> {
        match self.provider {
            Provider::Jira => Some(format!("{}/browse/{key}", store::jira_base(&self.site))),
            Provider::Linear => Some(format!("https://linear.app/{}/issue/{key}", self.site)),
            Provider::Local => None,
            Provider::Github => Some(self.github_repo()?.issue_url(ticket_key::number(key)?)),
        }
    }

    /// Whether the Add form lists its open issues (`Client::open_issues`): Jira and Linear find
    /// them by the ticket prefix, and GitHub's issues take it for their keys, so they need one.
    pub fn lists_open(&self) -> bool {
        match self.provider {
            Provider::Jira | Provider::Linear | Provider::Github => !self.key.trim().is_empty(),
            Provider::Local => true,
        }
    }

    /// A GitHub Issues tracker's repository.
    pub fn github_repo(&self) -> Option<Repo> {
        match self.provider {
            Provider::Github => Repo::parse(&self.site),
            Provider::Jira | Provider::Linear | Provider::Local => None,
        }
    }

    /// The key `input` names when it's written this tracker's own way (GitHub's `#12`, or an
    /// issue's link), or why it can't be added; None when it isn't, and the input reads as any
    /// tracker's key or number.
    pub fn key_from_input(&self, input: &str) -> Option<Result<String, String>> {
        github_issues::key_from_input(input, &self.key, &self.github_repo()?)
    }

    /// The issue that `key`'s PR closes, when the tracker is the repository's own issues: the PR
    /// lookup tries the PRs GitHub links to it first.
    pub fn closes(&self, key: &str) -> Option<crate::github::Closes> {
        Some(crate::github::Closes { repo: self.github_repo()?, number: ticket_key::number(key)? })
    }

    /// A Local files tracker's board in the project folder `repo`: never a worktree's copy.
    pub fn board(&self, repo: &Path) -> Option<PathBuf> {
        self.provider
            .has_board()
            .then(|| local::board(repo, self.folder.as_deref().unwrap_or(local::FOLDER)))
    }

    /// A ticket prefix to suggest for a project in the folder `repo` before one is typed: the one
    /// a Local files board's tickets use already, or one from the GitHub repository's name. None
    /// where there's nothing to go by, or the form lists them instead (`Provider::lists_choices`).
    pub fn suggested_prefix(&self, repo: &Path) -> Option<String> {
        match self.provider {
            Provider::Local => local::prefix_in_use(&local::files(&self.board(repo)?)),
            Provider::Github => crate::github::default_repo(repo)
                .and_then(|r| r.name.rsplit('/').next().map(local::default_prefix)),
            Provider::Jira | Provider::Linear => None,
        }
    }

    /// Whether a key names the same issue in both: the same Jira site, Linear workspace or GitHub
    /// repository. Never
    /// Local files, whose boards are each their project's own.
    pub fn shares_keys_with(&self, other: &Tracker) -> bool {
        self.provider == other.provider
            && match self.provider {
                Provider::Local => false,
                Provider::Jira => store::jira_base(&self.site)
                    .eq_ignore_ascii_case(&store::jira_base(&other.site)),
                Provider::Linear | Provider::Github => self.site.eq_ignore_ascii_case(&other.site),
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn an_error_keeps_its_kind_and_says_its_message() {
        let e = FetchError::NotFound("ENG-52 not found".into());
        assert_eq!(e.to_string(), "ENG-52 not found");
        assert_eq!(
            e.map(|m| format!("{m}, or not visible")),
            FetchError::NotFound("ENG-52 not found, or not visible".into())
        );
        assert_eq!(FetchError::from("no".to_string()), FetchError::Other("no".into()));
        let cut = local::WriteError::from(FetchError::Unreachable("offline".into()));
        assert!(cut.cut_off && !cut.denied && cut.message == "offline");
        assert!(!local::WriteError::from(FetchError::Auth("refused".into())).cut_off);
    }

    #[test]
    fn creates_only_with_a_title_and_prefix() {
        let board = Client::Local(store::state_dir().join("no-board"));
        let e = |prefix, title| board.create(prefix, title, "").unwrap_err().message;
        assert_eq!(e("ENG", "  "), "give the new ticket a title");
        assert!(e("", "Fix it").starts_with("the project has no ticket prefix"));
        assert!(e("e-1", "Fix it").starts_with("the project has no ticket prefix"));
    }

    /// Each way a request gets no answer says where the problem is, rather than ureq's words, and
    /// counts as cut off (for Codex's sandbox, and `taskman add`).
    #[test]
    fn says_plainly_why_a_tracker_cant_be_reached() {
        use std::io::{Error, ErrorKind};
        let said = |e: ureq::Error| {
            let cut = cut_off(&e);
            (request_failed(&e, "Jira", "https://example.atlassian.net/"), cut)
        };
        let unreachable = |m: &str| (FetchError::Unreachable(m.into()), true);
        let no_name = "can't reach example.atlassian.net: its name doesn't resolve (no network, or a DNS or VPN problem)";
        let lookup = "failed to lookup address information: Temporary failure in name resolution";
        assert_eq!(said(ureq::Error::Io(Error::other(lookup))), unreachable(no_name));
        assert_eq!(said(ureq::Error::HostNotFound), unreachable(no_name));
        let no_route = "can't reach example.atlassian.net: no connection (no network, or a VPN, proxy or firewall in the way)";
        assert_eq!(said(ureq::Error::ConnectionFailed), unreachable(no_route));
        let down = ureq::Error::Io(Error::from(ErrorKind::NetworkUnreachable));
        assert_eq!(said(down), unreachable(no_route));
        let slow = "example.atlassian.net didn't answer within 20s (a slow network, or Jira is having trouble)";
        assert_eq!(said(ureq::Error::Timeout(ureq::Timeout::Global)), unreachable(slow));
        let reset = ureq::Error::Io(Error::from(ErrorKind::ConnectionReset));
        assert_eq!(said(reset), unreachable("the connection to example.atlassian.net broke"));
        // An answer, though a failure on the tracker's side, and what isn't the network.
        let failed = "Jira failed on its side (HTTP 502); try again later";
        assert_eq!(said(ureq::Error::StatusCode(502)), (FetchError::Other(failed.into()), false));
        let (e, cut) = said(ureq::Error::TooManyRedirects);
        assert!(
            !cut && matches!(e, FetchError::Other(m) if m.starts_with("Jira request failed: "))
        );
        // Only the TUI, which tries again by itself, says so.
        let e = FetchError::Unreachable("can't reach example.atlassian.net".into());
        assert_eq!(
            e.repeated(),
            "can't reach example.atlassian.net; taskman tries again on the next refresh"
        );
        assert_eq!(FetchError::Auth("no".into()).repeated(), "no");
    }

    #[test]
    fn tells_requests_that_never_left_from_those_that_may_have_arrived() {
        use std::io::{Error, ErrorKind};
        for e in [
            ureq::Error::HostNotFound,
            ureq::Error::ConnectionFailed,
            ureq::Error::Timeout(ureq::Timeout::Connect),
            ureq::Error::Io(Error::from(ErrorKind::ConnectionRefused)),
            ureq::Error::Io(Error::from(ErrorKind::PermissionDenied)),
        ] {
            assert!(unsent(&e), "{e}");
        }
        for e in [
            ureq::Error::Timeout(ureq::Timeout::Global),
            ureq::Error::Timeout(ureq::Timeout::RecvResponse),
            ureq::Error::Io(Error::from(ErrorKind::ConnectionReset)),
            ureq::Error::Io(Error::from(ErrorKind::UnexpectedEof)),
            ureq::Error::StatusCode(502),
        ] {
            assert!(!unsent(&e), "{e}");
        }
        assert!(Created::read(Some("ENG-53"), "", "ENG").is_ok());
        for key in [None, Some(""), Some("ENG"), Some("ENG-5x"), Some("../ENG-5")] {
            assert!(Created::read(key, "", "x").unwrap_err().contains("may have been created"));
        }
    }

    fn listed(key: &str, title: &str, mine: bool) -> Listed {
        Listed { key: key.into(), title: title.into(), mine }
    }

    #[test]
    fn puts_yours_first() {
        let mut issues = vec![
            listed("ENG-71", "Export", false),
            listed("ENG-64", "Login", true),
            listed("ENG-58", "Search", false),
            listed("ENG-12", "Crash", true),
        ];
        mine_first(&mut issues);
        let keys: Vec<&str> = issues.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(keys, ["ENG-64", "ENG-12", "ENG-71", "ENG-58"]);
    }

    #[test]
    fn token_pages_are_full_urls_and_linears_is_in_the_workspace() {
        let jira = "https://id.atlassian.com/manage-profile/security/api-tokens";
        assert_eq!(Provider::Jira.token_page("").as_deref(), Some(jira));
        assert_eq!(Provider::Jira.token_page("acme.atlassian.net").as_deref(), Some(jira));
        let linear = |ws| Provider::Linear.token_page(ws).unwrap();
        assert_eq!(linear("acme"), "https://linear.app/acme/settings/account/security");
        assert_eq!(linear(""), "https://linear.app/<workspace>/settings/account/security");
        // Only a slug's characters reach the URL.
        assert_eq!(
            linear(" acme-2_x/?#\u{1b}"),
            "https://linear.app/acme-2_x/settings/account/security"
        );
        assert_eq!(linear("/ ?"), "https://linear.app/<workspace>/settings/account/security");
        assert_eq!(Provider::Github.token_page("acme"), None);
        assert_eq!(Provider::Local.token_page("acme"), None);
    }

    #[test]
    fn matches_numbers_and_title_words_whatever_the_trackers_keys() {
        let issues = [
            listed("ENG-9", "Crash on start", true),
            listed("ENG-15", "Fix the login form", false),
            listed("WEB-14", "Fix the search page", false),
            listed("WP-3", "Docs: explain setup", false),
        ];
        let keys = |input: &str| {
            matching(&issues, input).iter().map(|l| l.key.clone()).collect::<Vec<_>>()
        };
        assert_eq!(keys(""), ["ENG-9", "ENG-15", "WEB-14", "WP-3"]);
        // Digits, or GitHub's #, start an issue's number, whatever its prefix.
        assert_eq!(keys("1"), ["ENG-15", "WEB-14"]);
        assert_eq!(keys(" #3 "), ["WP-3"]);
        // Words, in any order and case, are in its title.
        assert_eq!(keys("FIX form"), ["ENG-15"]);
        assert_eq!(keys("fix"), ["ENG-15", "WEB-14"]);
        assert!(keys("nothing like it").is_empty());
        assert_eq!(Provider::Github.written_key("WEB-14"), "#14");
        assert_eq!(Provider::Jira.written_key("ENG-15"), "ENG-15");
        assert_eq!(Provider::Local.written_key("WP-3"), "WP-3");
    }

    #[test]
    fn lists_open_issues_with_a_prefix_to_find_them_by() {
        let t = |provider: &str, key: &str| -> Tracker {
            serde_json::from_value(
                serde_json::json!({ "provider": provider, "site": "acme", "key": key }),
            )
            .unwrap()
        };
        assert!(t("linear", "ENG").lists_open());
        assert!(t("jira", "ENG").lists_open());
        assert!(!t("linear", "").lists_open());
        assert!(!t("github", " ").lists_open());
        assert!(t("local", "").lists_open());
    }

    fn choice(key: &str, name: &str) -> Choice {
        Choice { key: key.into(), name: name.into() }
    }

    #[test]
    fn chooses_by_the_start_of_a_key_or_words_of_a_name() {
        let choices = [
            choice("ENG", "Engineering"),
            choice("MOB", "Mobile apps"),
            choice("OPS", "Operations"),
            choice("WEB", "Website and apps"),
        ];
        let keys = |input: &str| {
            choosing(&choices, input).iter().map(|c| c.key.as_str()).collect::<Vec<_>>()
        };
        assert_eq!(keys(""), ["ENG", "MOB", "OPS", "WEB"]);
        assert_eq!(keys("o"), ["MOB", "OPS"]);
        assert_eq!(keys("eng"), ["ENG"]);
        assert_eq!(keys(" Apps "), ["MOB", "WEB"]);
        assert_eq!(keys("apps web"), ["WEB"]);
        assert!(keys("nothing").is_empty());
        // Sorted by key, each key once, and only keys that are prefixes.
        let tidy = tidy_choices(vec![
            choice("WEB", "Website"),
            choice("ENG", "Engineering"),
            choice("ENG", "Engineering again"),
            choice("1ST", "Starts with a digit"),
            choice("A B", "Has a space"),
            choice("", "No key"),
        ]);
        assert_eq!(tidy, [choice("ENG", "Engineering"), choice("WEB", "Website")]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn choosing_takes_any_input_and_keeps_the_order(
            input in "\\PC{0,8}",
            names in prop::collection::vec(("[A-Z]{1,4}", "\\PC{0,12}"), 0..6),
        ) {
            let choices = tidy_choices(names.iter().map(|(k, n)| choice(k, n)).collect());
            prop_assert!(choices.windows(2).all(|w| w[0].key < w[1].key));
            prop_assert!(choices.iter().all(|c| ticket_key::is_prefix(&c.key)));
            let found = choosing(&choices, &input);
            let at: Vec<usize> =
                found.iter().map(|f| choices.iter().position(|c| c == *f).unwrap()).collect();
            prop_assert!(at.windows(2).all(|w| w[0] < w[1]));
            // A whole key typed finds it.
            for c in &choices {
                prop_assert!(choosing(&choices, &c.key.to_lowercase()).contains(&c));
            }
        }

        #[test]
        fn matching_takes_any_input_and_keeps_the_order(
            input in "(\\PC|[#0-9 ])*",
            titles in prop::collection::vec("\\PC{0,12}", 0..6),
        ) {
            let issues: Vec<Listed> = titles
                .iter()
                .enumerate()
                .map(|(n, t)| listed(&format!("ENG-{n}"), t, false))
                .collect();
            let found = matching(&issues, &input);
            let at: Vec<usize> =
                found.iter().map(|f| issues.iter().position(|i| i == *f).unwrap()).collect();
            prop_assert!(at.windows(2).all(|w| w[0] < w[1]));
        }
    }
}
