//! Everything that depends on the tracker: the issue Jira, Linear, Local files and GitHub Issues
//! all produce, the client that fetches one, the TUI's tokens and clients, and what each provider
//! does differently.

use crate::github_issues::{self, GithubConfig, Repo};
use crate::jira::{self, JiraConfig};
use crate::linear::{self, LinearConfig};
use crate::local;
use crate::store::{self, Account, Config, Project, Provider, Tracker};
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
    /// `OPEN`, `MERGED` or `CLOSED`.
    pub state: String,
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

/// What fetches a project's issues.
#[derive(Clone)]
pub enum Client {
    Jira(JiraConfig),
    Linear(LinearConfig),
    /// A Local files board, in the project's own folder.
    Local(PathBuf),
    /// A GitHub repository's issues, through gh.
    Github(GithubConfig),
    /// Demo mode: issues from fixture files in this folder.
    Demo(PathBuf),
}

impl Client {
    /// The client for `tracker` in the project folder `repo`, with the account and token it uses;
    /// None when it needs an account (`Provider::needs_account`) and has none.
    pub fn new(
        tracker: &Tracker,
        repo: &Path,
        account: Option<(&Account, &str)>,
    ) -> Option<Client> {
        // A Local files board is real even in demo mode; the other trackers come from fixtures.
        if tracker.provider != Provider::Local
            && let Some(dir) = crate::demo::dir()
        {
            return Some(Client::Demo(dir));
        }
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
    /// as the TUI, reading the token now; None for a project without a tracker.
    pub fn for_project(config: &Config, project: &Project) -> Result<Option<Client>, String> {
        let Some(t) = &project.tracker else { return Ok(None) };
        if !t.provider.needs_account() || crate::demo::dir().is_some() {
            return Ok(Client::new(t, &project.repo, None));
        }
        let Some(i) = config.account_for(t) else {
            return Err(format!(
                "no {} account picked for project {}; pick or add one in its settings in taskman ({} ,)",
                t.provider.label(),
                project.name,
                crate::keys::prefix()
            ));
        };
        let a = &config.accounts[i];
        let token = a
            .resolve_token()
            .map_err(|e| format!("{e} (the token of {} account {})", t.provider.label(), a.id))?;
        Ok(Client::new(t, &project.repo, Some((a, &token))))
    }

    pub fn fetch(&self, key: &str, text: Text) -> Result<Issue, String> {
        let text = text.render();
        match self {
            Client::Jira(c) => jira::fetch(c, key),
            Client::Linear(c) => linear::fetch(c, key, text),
            Client::Local(board) => local::fetch(board, key, text).map(|(i, _)| i),
            Client::Github(c) => c.fetch(key, text),
            Client::Demo(dir) => crate::demo::issue(dir, key),
        }
    }

    /// A Local files client's board.
    pub fn board(&self) -> Option<&Path> {
        match self {
            Client::Local(board) => Some(board),
            Client::Jira(_) | Client::Linear(_) | Client::Github(_) | Client::Demo(_) => None,
        }
    }

    /// Whether issues come over the network, fetched on a timer; a board is read again on every
    /// status poll instead, when its files change.
    pub fn remote(&self) -> bool {
        self.board().is_none()
    }

    /// Whether the project `prefix` names exists there (`Provider::checks_prefix`); Err when that
    /// couldn't be told (offline, say).
    pub fn project_exists(&self, prefix: &str) -> Result<bool, String> {
        match self {
            Client::Jira(c) => c.project_exists(prefix),
            Client::Linear(_) | Client::Local(_) | Client::Github(_) | Client::Demo(_) => Ok(true),
        }
    }
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
        github_issues::number(&self.key)
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
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
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

/// Whether a key reads as one: letters, digits or `_` starting with a letter, a dash, a number.
fn well_formed(key: &str) -> bool {
    key.split_once('-').is_some_and(|(p, n)| {
        p.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
    })
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
            Client::Demo(_) => Vec::new(),
        };
        // A key names a worktree and a branch: one the tracker wrote some other way is left out.
        issues.retain(|l| well_formed(&l.key));
        mine_first(&mut issues);
        Ok(issues)
    }
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
    /// Reads, in the background, the tokens of accounts not read yet or whose read failed, each
    /// handed to `read` (which passes it back to `token_read`), then builds the clients projects
    /// need. Runs at startup and whenever accounts or projects change.
    pub fn rebuild<F>(&mut self, config: &Config, projects: &[Project], read: F)
    where
        F: Fn(String, Result<String, String>) + Clone + Send + 'static,
    {
        let sources: HashSet<&String> = config.accounts.iter().map(|a| &a.token).collect();
        self.tokens.retain(|s, _| sources.contains(s));
        for a in &config.accounts {
            if matches!(self.tokens.get(&a.token), Some(None | Some(Ok(_)))) {
                continue;
            }
            self.tokens.insert(a.token.clone(), None);
            let (a, read) = (a.clone(), read.clone());
            std::thread::spawn(move || {
                let token = a.resolve_token();
                read(a.token, token);
            });
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
        // Demo mode needs no account: Jira and Linear issues come from fixtures.
        if tracker.provider != Provider::Local && crate::demo::dir().is_some() {
            return Client::new(tracker, &p.repo, None).map_or(Access::NoTracker, Access::Client);
        }
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
        match (self, github_issues::number(key)) {
            (Provider::Github, Some(n)) => format!("#{n}"),
            _ => key.to_string(),
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
            Provider::Github => Some(self.github_repo()?.issue_url(github_issues::number(key)?)),
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
        Some(crate::github::Closes {
            repo: self.github_repo()?,
            number: github_issues::number(key)?,
        })
    }

    /// A Local files tracker's board in the project folder `repo`: never a worktree's copy.
    pub fn board(&self, repo: &Path) -> Option<PathBuf> {
        self.provider
            .has_board()
            .then(|| local::board(repo, self.folder.as_deref().unwrap_or(local::FOLDER)))
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

    fn listed(key: &str, title: &str, mine: bool) -> Listed {
        Listed { key: key.into(), title: title.into(), mine }
    }

    #[test]
    fn puts_yours_first_and_leaves_out_keys_that_dont_read_as_keys() {
        let mut issues = vec![
            listed("ENG-71", "Export", false),
            listed("ENG-64", "Login", true),
            listed("ENG-58", "Search", false),
            listed("ENG-12", "Crash", true),
        ];
        mine_first(&mut issues);
        let keys: Vec<&str> = issues.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(keys, ["ENG-64", "ENG-12", "ENG-71", "ENG-58"]);
        for good in ["ENG-12", "A_1-7", "web-3"] {
            assert!(well_formed(good), "{good}");
        }
        for bad in ["ENG-", "-12", "1ENG-2", "ENG-1x", "ENG 1-2", "ENG-1; rm", "#12", ""] {
            assert!(!well_formed(bad), "{bad}");
        }
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

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

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
