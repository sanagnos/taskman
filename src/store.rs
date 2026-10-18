//! Persisted state (projects, tickets, UI preferences), the accounts config, and status file paths.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What each provider does differently is in `tracker.rs`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Jira,
    Linear,
    /// Markdown files in a folder of the project (`local.rs`); needs no account.
    Local,
    /// The issues of the project's GitHub repository, through `gh` (`github_issues.rs`); needs no
    /// account.
    Github,
}

/// Where a project's tickets live.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(from = "SavedTracker", into = "SavedTracker")]
pub struct Tracker {
    pub provider: Provider,
    /// Jira: the site's base URL (`https://example.atlassian.net`). Linear: the workspace slug.
    /// GitHub Issues: the repository, `acme/web` (`host/acme/web` off github.com), read from the
    /// project's remotes when it's saved. Empty for Local files.
    pub site: String,
    /// Local files: the board's folder in the project, like `workplan`.
    pub folder: Option<String>,
    /// Ticket prefix (Jira project key or Linear team key), used only to complete bare ticket
    /// numbers: with `ENG`, typing `52` adds `ENG-52`.
    pub key: String,
    /// The account this project uses (`Account::id`). None picks one by site, or the provider's
    /// only account.
    pub account: Option<String>,
    pub extra: Extra,
}

/// A tracker as tickets.json holds it. Builds before `folder` kept a Local files board's folder in
/// `site`, and still read it from there: so it is read from `site` when `folder` is missing,
/// and written to both.
#[derive(Serialize, Deserialize)]
struct SavedTracker {
    provider: Provider,
    site: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    #[serde(default)]
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    #[serde(flatten)]
    extra: Extra,
}

impl From<SavedTracker> for Tracker {
    fn from(s: SavedTracker) -> Self {
        let (site, folder) = match s.provider {
            Provider::Local => (String::new(), s.folder.or(Some(s.site))),
            _ => (s.site, s.folder),
        };
        let (key, account, extra) = (s.key, s.account, s.extra);
        Tracker { provider: s.provider, site, folder, key, account, extra }
    }
}

impl From<Tracker> for SavedTracker {
    fn from(t: Tracker) -> Self {
        // A board without a folder is in the default one (`Tracker::board`); saved as "" it would
        // load as the project folder itself.
        let folder = match t.provider {
            Provider::Local => t.folder.or_else(|| Some(crate::local::FOLDER.into())),
            _ => t.folder,
        };
        let site = match (t.provider, &folder) {
            (Provider::Local, Some(folder)) => folder.clone(),
            _ => t.site,
        };
        let (key, account, extra) = (t.key, t.account, t.extra);
        SavedTracker { provider: t.provider, site, folder, key, account, extra }
    }
}

/// One repository and its tracker. Tickets belong to the project whose repo they share.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Project {
    pub name: String,
    /// Main checkout.
    pub repo: PathBuf,
    #[serde(default)]
    pub tracker: Option<Tracker>,
    #[serde(default)]
    pub collapsed: bool,
    /// Agents closed in this project, with their conversations, so adding the name again resumes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_agents: Vec<ClosedAgent>,
    #[serde(default, skip_serializing_if = "WorktreePrefs::is_default")]
    pub worktrees: WorktreePrefs,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Project {
    /// Why a ticket can't be added to a project without a tracker.
    pub fn agents_only(&self) -> String {
        format!(
            "{} has no tracker, so it has agents only; connect a tracker (Jira, Linear, Local files or GitHub Issues) in its settings in taskman ({} ,) to add tickets",
            self.name,
            crate::keys::prefix()
        )
    }
}

/// How tickets added to a project get their worktrees. Every field defaults, so projects saved
/// before these settings existed behave the same.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct WorktreePrefs {
    /// Whether the Add form creates one by default. None: yes. A project whose workflow makes its
    /// own worktrees saves `Some(false)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<bool>,
    /// Branch name format (see `worktree::branch_name`); None is `{tracker}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Inside the main checkout, in `.claude/worktrees/`, like `claude --worktree`; else beside it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inside: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

impl WorktreePrefs {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the Add form creates a worktree for new tickets by default.
    pub fn creates(&self) -> bool {
        self.create.unwrap_or(true)
    }

    pub fn branch_format(&self) -> &str {
        self.branch.as_deref().unwrap_or(crate::worktree::TRACKER)
    }

    pub fn folder_format(&self) -> &str {
        if self.inside { crate::worktree::INSIDE } else { crate::worktree::BESIDE }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClosedAgent {
    pub name: String,
    /// Empty when it closed before its session reported one; kept anyway for its role.
    #[serde(default)]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Cli::is_claude")]
    pub cli: Cli,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The coding CLI a session runs.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Cli {
    #[default]
    Claude,
    Codex,
}

impl Cli {
    pub const ALL: [Cli; 2] = [Cli::Claude, Cli::Codex];

    pub fn label(self) -> &'static str {
        match self {
            Cli::Claude => "Claude",
            Cli::Codex => "Codex",
        }
    }

    pub fn from_label(s: &str) -> Cli {
        Cli::ALL.into_iter().find(|c| c.label() == s).unwrap_or_default()
    }

    pub fn is_claude(&self) -> bool {
        *self == Cli::Claude
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Ticket {
    pub key: String,
    /// Main checkout of the repository the ticket's work happens in; names its project.
    pub repo: PathBuf,
    /// Where the session runs: the ticket's worktree if one existed when the
    /// ticket was added, otherwise the main checkout.
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Cli::is_claude")]
    pub cli: Cli,
    /// Branch the ticket's PR is opened from, once known.
    #[serde(default)]
    pub branch: Option<String>,
    /// Last session id reported by the hooks, used to resume after a restart.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Jira comment ids already seen. None until the first fetch, which marks everything as seen.
    #[serde(default)]
    pub seen_jira: Option<Vec<String>>,
    /// PR comment and review ids already seen, prefixed `c:` and `r:`.
    #[serde(default)]
    pub seen_gh: Option<Vec<String>>,
    /// Set for a project agent: a session with no ticket, named this. Its `key` is then the
    /// session's name, `<project>-<agent>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// An agent's standing instruction, appended to its brief at every session start (not sent
    /// as a message): what it's for, like "test PRs against their tickets".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// taskman created the worktree in `cwd`, so closing the ticket may offer to remove it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub created_worktree: bool,
    /// The ticket this one stacks on: its worktree started from that ticket's branch, and its PR
    /// targets it until the base merges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Base>,
    /// A worktree asked for and not made yet. The session waits for it, across restarts, until
    /// it's made or the user chooses the main checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_worktree: Option<PendingWorktree>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A ticket's worktree still to make: the base it starts from, and why the last try failed.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PendingWorktree {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Base>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A stacked ticket's base. The branch is kept as well as the key: the base ticket is often
/// closed once it merges, and the brief and the merge hint still name its branch.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Base {
    pub key: String,
    pub branch: String,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Ticket {
    /// What the sidebar and headers call it: the agent's name or the ticket key.
    pub fn display(&self) -> &str {
        self.agent.as_deref().unwrap_or(&self.key)
    }

    /// Whether this is `key` in the project whose main checkout is `repo`. A key is unique only
    /// within its project: every Local files board can have WP-1, and Jira and Linear keys repeat
    /// across sites and workspaces.
    pub fn is(&self, repo: &Path, key: &str) -> bool {
        self.repo == repo && self.key == key
    }
}

/// The name a session gets (`--name`), which other sessions message it by: its key, or
/// `<project>/<key>` while another project has a ticket or agent with the same key, or
/// `<project>-<id>/<key>` while the other project's name slugs the same too ("alpha web" and
/// "alpha-web"); the id is the start of the folder's hash in `project_id`, so it never changes.
/// Decided as the session starts, so one started before the other keeps the plain key until it
/// restarts. `running` holds the names of the other sessions running now: one of them can have a
/// name this would give (its project was renamed since), and two sessions never share a name.
pub fn session_name<'a>(
    t: &Ticket,
    projects: &[Project],
    all: impl IntoIterator<Item = &'a Ticket>,
    running: &[&str],
) -> String {
    let others: Vec<&Path> = all
        .into_iter()
        .filter(|o| o.repo != t.repo && o.key.eq_ignore_ascii_case(&t.key))
        .map(|o| o.repo.as_path())
        .collect();
    // The project's name (its folder's once it's gone), with `id` digits of the hash.
    let label = |repo: &Path, id: usize| {
        let name = projects
            .iter()
            .find(|p| p.repo == repo)
            .map(|p| slug(&p.name))
            .filter(|s| !s.is_empty());
        match name.unwrap_or_else(|| slug(&project_name(repo))) {
            name if name.is_empty() => repo_hash(repo),
            name if id == 0 => name,
            name => format!("{name}-{}", &repo_hash(repo)[..id]),
        }
    };
    let qualified = [0, 4, 8].map(|id| {
        let mine = label(&t.repo, id);
        let unique = id == 8 || !others.iter().any(|o| label(o, id).eq_ignore_ascii_case(&mine));
        unique.then(|| format!("{mine}/{}", t.key))
    });
    let free = |name: &String| !running.iter().any(|r| r.eq_ignore_ascii_case(name));
    std::iter::once(others.is_empty().then(|| t.key.clone()))
        .chain(qualified)
        .flatten()
        .find(free)
        .unwrap_or_else(|| format!("{}/{}", label(&t.repo, 8), t.key))
}

/// The ticket or agent of the session a command runs in, from its `TASKMAN_TICKET` (`key`) and
/// `TASKMAN_REPO` (`repo`). Sessions an older taskman started have only the key, which then names
/// a ticket only when one project has it.
pub fn session_ticket<'a>(
    tickets: &'a [Ticket],
    key: &str,
    repo: Option<&Path>,
) -> Option<&'a Ticket> {
    if let Some(repo) = repo {
        return tickets.iter().find(|t| t.is(repo, key));
    }
    let mut found = tickets.iter().filter(|t| t.key == key);
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The session a command runs in, when taskman started it: `TASKMAN_TICKET` and `TASKMAN_REPO`.
pub fn session_env() -> (Option<String>, Option<PathBuf>) {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    (
        var("TASKMAN_TICKET").and_then(|v| v.into_string().ok()),
        var("TASKMAN_REPO").map(PathBuf::from),
    )
}

/// Lowercase letters, digits and dashes, for session names.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Missing fields take their defaults, so a file saved before a field existed still loads.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct UiPrefs {
    pub sidebar_collapsed: bool,
    pub show_jira: bool,
    pub show_comments: bool,
    pub show_gh: bool,
    /// The Add form has explained what Codex sessions can't do; shown until one is added.
    pub codex_explained: bool,
    /// The tip that tmux or screen uses the same prefix has been shown, once.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub prefix_tip_shown: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            sidebar_collapsed: false,
            show_jira: true,
            show_comments: true,
            show_gh: true,
            codex_explained: false,
            prefix_tip_shown: false,
            extra: Extra::new(),
        }
    }
}

/// The format of tickets.json and config.json this build writes, and the newest it reads: a file
/// of a newer format is refused (`read_json`) rather than misread, and one with none is format 1.
/// A new field doesn't raise it, since builds that don't know a field keep it (`Extra`); a change
/// older builds would misread does, and is a breaking release.
pub const FORMAT: u64 = 1;

/// A saved file's `format`, written as this build's `FORMAT` whatever the file said: `read_json`
/// has refused a newer one, and an older one was read into this build's shape.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Format;

impl Serialize for Format {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(FORMAT)
    }
}

impl<'de> Deserialize<'de> for Format {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        u64::deserialize(d).map(|_| Format)
    }
}

/// The fields of a saved struct this build doesn't know, kept as they were read and written back,
/// so a field a newer taskman added survives a save by this one. Every struct in tickets.json and
/// config.json has one; code that rebuilds a saved struct carries it over.
pub type Extra = serde_json::Map<String, serde_json::Value>;

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Saved {
    #[serde(default)]
    pub format: Format,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub tickets: Vec<Ticket>,
    #[serde(default)]
    pub selected: usize,
    #[serde(default)]
    pub ui: UiPrefs,
    #[serde(flatten)]
    pub extra: Extra,
}

pub fn state_dir() -> PathBuf {
    #[cfg(test)]
    let base = scratch::dir();
    #[cfg(not(test))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"));
    base.join("taskman")
}

/// The state folder and config file of unit tests: a folder for each test, removed when it ends, so
/// no test reads or writes the user's, and tests running at once don't share owner locks or saved
/// state. A test runs on a thread of its own; a thread it spawns gets a folder of its own too.
#[cfg(test)]
pub mod scratch {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    thread_local! {
        static DIR: Dir = {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            Dir(std::env::temp_dir().join(format!("taskman-unit-{}-{n}", std::process::id())))
        };
    }

    pub fn dir() -> PathBuf {
        DIR.with(|d| d.0.clone())
    }
}

/// The user's home folder. Unit tests get a made-up one, so what they draw (`tilde`) doesn't depend
/// on where they run, and they never read the user's `~/.claude` or `~/.codex`.
pub fn home() -> PathBuf {
    if cfg!(test) {
        return PathBuf::from("/home/tester");
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// A session's status file, which its hooks write and the TUI polls; the refresh signal files sit
/// beside it. In a folder per project, since a key is unique only within its project.
pub fn status_file(repo: &Path, key: &str) -> PathBuf {
    status_file_in(&status_dir(), repo, key)
}

/// Where `ctrl+a v` writes a session's history for `$EDITOR`, overwritten each time.
pub fn history_file(repo: &Path, key: &str) -> PathBuf {
    state_dir().join("history").join(project_id(repo)).join(format!("{}.txt", file_safe(key)))
}

pub fn status_dir() -> PathBuf {
    state_dir().join("status")
}

pub fn status_file_in(dir: &Path, repo: &Path, key: &str) -> PathBuf {
    dir.join(project_id(repo)).join(format!("{}.json", file_safe(key)))
}

/// Where older versions kept a session's status file, before a folder per project: by key alone.
/// Their sessions keep writing there after an update installs this binary, until the TUI restarts,
/// so readers fall back to it for a key that only one project has.
pub fn legacy_status_file(key: &str) -> PathBuf {
    legacy_status_file_in(&status_dir(), key)
}

pub fn legacy_status_file_in(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{}.json", file_safe(key)))
}

fn file_safe(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// The status folder's name for the project in `repo`: the folder's name, for whoever looks, and a
/// hash of its whole path, since two projects' folders can share a name. From the path rather than
/// the project's name, which can change while its sessions run.
pub fn project_id(repo: &Path) -> String {
    match slug(&project_name(repo)) {
        name if name.is_empty() => repo_hash(repo),
        name => format!("{name}-{}", repo_hash(repo)),
    }
}

/// Eight hex digits of a hash of the folder's whole path: stable, and different for two folders
/// with the same name.
fn repo_hash(repo: &Path) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(repo.as_os_str().as_encoded_bytes());
    hash[..4].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn load() -> Result<Saved, String> {
    read_json(&state_dir().join("tickets.json"))
}

/// Reads a saved file, or the default when there is none yet. Any other failure is an error and
/// the file is left as it is: the first save would replace whatever couldn't be read, which may
/// be every project and account, or state a newer taskman wrote. A file of a newer `FORMAT` is
/// refused before anything else in it is read.
pub fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(format!("couldn't read {}: {e}", tilde(path))),
    };
    let left = "It was left as it is. If a newer taskman wrote it, run that one (taskman update installs the latest)";
    let format = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("format")?.as_u64());
    if let Some(n) = format
        && n > FORMAT
    {
        return Err(format!(
            "couldn't read {}: it's in format {n}, and this taskman reads format {FORMAT} and older\n{left}.",
            tilde(path)
        ));
    }
    serde_json::from_str(&text).map_err(|e| {
        format!(
            "couldn't read {}: {e}\n{left}; otherwise fix the file, or move it aside to start without it.",
            tilde(path)
        )
    })
}

/// Held while the TUI runs, so a second one on the same state folder is refused: each rewrites
/// tickets.json from memory, so the last to save would undo the other's changes. An advisory
/// lock rather than a pid file: the system drops it when the process ends, however it ends.
pub struct Lock {
    _file: std::fs::File,
}

pub fn lock() -> Result<Lock, String> {
    lock_in(&state_dir())
}

fn lock_in(dir: &Path) -> Result<Lock, String> {
    use std::io::{Read, Seek, Write};
    use std::os::fd::AsRawFd;
    let path = dir.join("lock");
    let open = || {
        private_dir(dir)?;
        std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)
    };
    let mut file = open().map_err(|e| format!("couldn't open {}: {e}", tilde(&path)))?;
    // SAFETY: flock on a descriptor this function owns; it reads or writes no memory.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let mut pid = String::new();
        let _ = file.read_to_string(&mut pid);
        let pid = pid.trim();
        let pid = if pid.is_empty() { String::new() } else { format!(" (pid {pid})") };
        return Err(already_running(dir, &pid));
    }
    // The pid only names the other taskman in that message; the lock is what counts.
    let _ = file
        .set_len(0)
        .and_then(|()| file.rewind())
        .and_then(|()| write!(file, "{}", std::process::id()));
    Ok(Lock { _file: file })
}

/// Why a second taskman won't start on the state folder `dir`; `pid` is ` (pid N)` when known.
pub fn already_running(dir: &Path, pid: &str) -> String {
    format!(
        "another taskman is already running{pid} with {}. Two would overwrite each other's saves: use that one, or quit it ({} q) and start again.",
        tilde(dir),
        crate::keys::prefix()
    )
}

/// Creates folder `dir` if need be and makes it the user's alone (0700). The state folder holds
/// the inbox, which adds tickets and agents, and the sessions' status files; the config folder
/// names accounts and token commands.
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

pub fn save(saved: &Saved) -> anyhow::Result<()> {
    write_atomic(&state_dir().join("tickets.json"), &serde_json::to_vec_pretty(saved)?)
}

/// Writes `bytes` to `path` whole or not at all: to a file beside it, synced to disk, then renamed
/// over it; a crash leaves the old file or the new one, never an empty or a half-written one. The
/// folder is synced after, where the system lets it be, so the rename itself lasts too.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    write_renamed(path, bytes, true)
}

/// `write_atomic` without the syncs, for a file rewritten all the time whose loss costs nothing:
/// the inbox heartbeat, of which only the time is read. Syncing it would commit the disk's
/// journal three times a second for as long as taskman runs.
pub fn write_unsynced(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    write_renamed(path, bytes, false)
}

fn write_renamed(path: &Path, bytes: &[u8], sync: bool) -> anyhow::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    // A system that can't sync a file (some network and virtual filesystems) still keeps it.
    if sync
        && let Err(e) = f.sync_all()
        && e.kind() != std::io::ErrorKind::Unsupported
        && e.raw_os_error() != Some(libc::EINVAL)
    {
        return Err(e.into());
    }
    drop(f);
    std::fs::rename(&tmp, path)?;
    if sync
        && let Some(dir) = path.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Shortens a path under $HOME to `~/...` for display.
pub fn tilde(p: &Path) -> String {
    match p.strip_prefix(home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

// ---- accounts ----------------------------------------------------------------------------

/// How long a `cmd:` token command may take: long enough to unlock a password manager in its own
/// window. taskman reads tokens in the background, so a slow one only delays its tracker panels.
const TOKEN_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Credentials a project uses for its tracker. Holds where the token comes from, never the
/// token itself. A Jira token belongs to a person and works on every site they can reach, so one
/// Jira account can serve many projects; a Linear key belongs to one workspace.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Account {
    /// Stable id that projects refer to, like `jira-1`.
    #[serde(default)]
    pub id: String,
    pub provider: Provider,
    /// Linear: the workspace. Jira: a site to test the connection against (any works).
    #[serde(default)]
    pub site: String,
    /// Jira only: the Atlassian account email.
    #[serde(default)]
    pub email: String,
    /// `env:NAME` reads an environment variable; `cmd:COMMAND` runs a command (a password
    /// manager, say) and uses its output; `keychain:NAME` reads taskman's item in the OS keychain.
    pub token: String,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Account {
    pub fn matches(&self, t: &Tracker) -> bool {
        self.provider == t.provider && same_site(&self.site, &t.site)
    }

    /// Who the account is: the email for Jira, the workspace for Linear.
    pub fn who(&self) -> String {
        match self.provider {
            Provider::Jira if !self.email.is_empty() => self.email.clone(),
            Provider::Linear if !self.site.is_empty() => self.site.clone(),
            p => p.label().to_string(),
        }
    }

    /// How the account appears in pickers: who it is and a short source. The full source is on
    /// the account's own Settings page; `account_labels` tells apart accounts this would merge.
    pub fn label(&self) -> String {
        format!("{} · {}", self.who(), short_source(&self.token))
    }

    pub fn resolve_token(&self) -> Result<String, String> {
        resolve_token(&self.token, TOKEN_LIMIT)
    }
}

/// Whether an account's `token` says where to read the token from, as it must, rather than being
/// the token. The account form refuses anything else, but an older version saved it as typed.
pub fn is_token_source(token: &str) -> bool {
    token.starts_with("env:")
        || token.starts_with("cmd:")
        || crate::keychain::name_of(token).is_some()
}

/// `$NAME` or `${NAME}`, as people used to the shell type it, as `env:NAME`; anything else as it
/// is. Only a plain variable name converts: `$(cmd)` or `$FOO bar` stays as typed, to be refused.
pub fn env_source_from_shell(token: &str) -> String {
    let name = token
        .strip_prefix('$')
        .map(|n| n.strip_prefix('{').map_or(Some(n), |b| b.strip_suffix('}')));
    match name {
        Some(Some(name)) if is_env_name(name) => format!("env:{name}"),
        _ => token.to_string(),
    }
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The token `source` (`env:NAME`, `cmd:COMMAND` or `keychain:NAME`) gives, or why it gives none.
fn resolve_token(source: &str, limit: std::time::Duration) -> Result<String, String> {
    if let Some(var) = source.strip_prefix("env:") {
        let var = var.trim();
        return std::env::var(var).ok().filter(|v| !v.trim().is_empty()).ok_or_else(|| {
            let shell = std::env::var("SHELL").unwrap_or_default();
            unset(var, &shell, cfg!(target_os = "macos"))
        });
    }
    if let Some(name) = crate::keychain::name_of(source) {
        return crate::keychain::Keychain::os().read(name);
    }
    if let Some(cmd) = source.strip_prefix("cmd:") {
        let out = crate::proc::run(std::process::Command::new("sh").args(["-c", cmd]), limit).map_err(|e| {
            match e.kind() {
                std::io::ErrorKind::TimedOut => format!(
                    "the token command didn't finish within {}s; it runs without a terminal, so it can't ask for a password there",
                    limit.as_secs()
                ),
                _ => format!("could not run the token command: {e}"),
            }
        })?;
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return match (out.status.success() && !token.is_empty(), why.is_empty()) {
            (true, _) => Ok(token),
            (false, true) => Err(format!("the token command printed no token; {SEE_GUIDE}")),
            (false, false) => Err(format!("the token command failed: {why}; {SEE_GUIDE}")),
        };
    }
    // Saved by an older version, whose account form took the token itself. Never repeat it.
    Err(format!(
        "the token field holds the token itself; change it to env:VAR or cmd:COMMAND in taskman's Settings ({} ,) to keep the token out of config.json",
        crate::keys::prefix()
    ))
}

/// Where a failed token source sends the user.
const SEE_GUIDE: &str = "see Storing a token in taskman's guide";

/// Why the variable `var` gave no token, and what to do: set it where `shell` reads it at start,
/// then start taskman from a new terminal, which has it. On macOS (`mac`), Terminal starts bash as
/// a login shell, which reads ~/.bash_profile and not ~/.bashrc.
fn unset(var: &str, shell: &str, mac: bool) -> String {
    let bash = if mac { "~/.bash_profile" } else { "~/.bashrc" };
    let file = match shell.rsplit('/').next() {
        Some("zsh") => "~/.zshrc".to_string(),
        Some("bash") => bash.to_string(),
        _ => format!("your shell's startup file (~/.zshrc for zsh, {bash} for bash)"),
    };
    format!(
        "{var} isn't set in the shell taskman started from: add export {var}=… to {file}, then open a new terminal and start taskman there"
    )
}

/// What pickers show for a token saved as typed by an older version: never the token.
const RAW_TOKEN: &str = "token saved in config (fix in Settings)";

/// A token source short enough for a picker: `env:NAME` whole, `cmd:` and the command's first
/// word (its file name, when that's a path), `keychain` for the keychain.
pub fn short_source(token: &str) -> String {
    let token = token.trim();
    if !is_token_source(token) {
        return RAW_TOKEN.into();
    }
    if crate::keychain::name_of(token).is_some() {
        return "keychain".into();
    }
    let Some(cmd) = token.strip_prefix("cmd:") else { return token.to_string() };
    let first = cmd.split_whitespace().next().unwrap_or_default();
    format!("cmd:{}", first.rsplit('/').find(|s| !s.is_empty()).unwrap_or(first))
}

/// The whole source, for pickers where the short one wouldn't tell accounts apart.
fn full_source(token: &str) -> String {
    let token = token.trim();
    if is_token_source(token) { token.to_string() } else { RAW_TOKEN.into() }
}

/// Picker labels for `accounts`, one each and all different. Where short labels would be the
/// same, those accounts show their full source, which the form cuts in the middle to fit (commands
/// usually differ at their ends); identical sources add the id.
pub fn account_labels(accounts: &[&Account]) -> Vec<String> {
    let same = |a: &Account, f: fn(&Account) -> String| {
        accounts.iter().filter(|b| f(b) == f(a)).count() > 1
    };
    let full = |a: &Account| format!("{} · {}", a.who(), full_source(&a.token));
    accounts
        .iter()
        .map(|a| match () {
            _ if !same(a, Account::label) => a.label(),
            _ if !same(a, full) => full(a),
            _ => format!("{} ({})", full(a), a.id),
        })
        .collect()
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    #[serde(default)]
    pub format: Format,
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// Install new releases in the background; off only says one is out.
    #[serde(default = "yes")]
    pub auto_update: bool,
    /// The key before every taskman command.
    #[serde(default)]
    pub prefix: crate::keys::Prefix,
    /// Seconds a session goes unselected before it's put to sleep (stopped, to free its memory);
    /// None never. Missing means `SLEEP_AFTER`.
    #[serde(default = "sleep_after")]
    pub sleep_after: Option<u64>,
    /// The most sessions to keep awake: past it, the least recently selected idle ones sleep
    /// whatever the time. None for no limit.
    #[serde(default)]
    pub max_awake: Option<usize>,
    /// Light or dark, or `auto` to ask the terminal.
    #[serde(default)]
    pub theme: crate::theme::Setting,
    #[serde(flatten)]
    pub extra: Extra,
}

/// How long a session goes unselected before it sleeps, unless the config says otherwise: long
/// enough that a ticket put down for a meeting or a review is still running when you're back,
/// short enough that an afternoon's tickets don't all stay in memory the next day.
pub const SLEEP_AFTER: u64 = 3600;

fn sleep_after() -> Option<u64> {
    Some(SLEEP_AFTER)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            format: Format,
            accounts: Vec::new(),
            auto_update: true,
            prefix: crate::keys::Prefix::default(),
            sleep_after: sleep_after(),
            max_awake: None,
            theme: crate::theme::Setting::Auto,
            extra: Extra::new(),
        }
    }
}

fn yes() -> bool {
    true
}

impl Config {
    /// The account a project's tracker uses: the one it names, else one on the same site, else
    /// (Jira only, whose tokens work on every site) the only Jira account.
    pub fn account_for(&self, t: &Tracker) -> Option<usize> {
        if !t.provider.needs_account() {
            return None;
        }
        if let Some(id) = &t.account {
            return self.accounts.iter().position(|a| &a.id == id);
        }
        if let Some(i) = self.accounts.iter().position(|a| a.matches(t)) {
            return Some(i);
        }
        if !t.provider.account_spans_sites() {
            return None; // a Linear key belongs to one workspace
        }
        let any: Vec<usize> =
            (0..self.accounts.len()).filter(|&i| self.accounts[i].provider == t.provider).collect();
        (any.len() == 1).then(|| any[0])
    }

    pub fn next_id(&self, provider: Provider) -> String {
        let prefix = provider.label().to_lowercase();
        let n = (1..)
            .find(|n| !self.accounts.iter().any(|a| a.id == format!("{prefix}-{n}")))
            .unwrap_or(1);
        format!("{prefix}-{n}")
    }
}

pub fn config_path() -> PathBuf {
    if cfg!(test) {
        return state_dir().join("config.json");
    }
    if let Some(p) = std::env::var_os("TASKMAN_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("taskman").join("config.json")
}

pub fn load_config() -> Result<Config, String> {
    read_json(&config_path())
}

pub fn save_config(c: &Config) -> anyhow::Result<()> {
    if let Some(dir) = own_config_dir() {
        private_dir(&dir)?;
    }
    write_atomic(&config_path(), &serde_json::to_vec_pretty(c)?)
}

/// taskman's own config folder; None when `TASKMAN_CONFIG` puts the file in a folder that may hold
/// other things, whose permissions are the user's business.
pub fn own_config_dir() -> Option<PathBuf> {
    std::env::var_os("TASKMAN_CONFIG")
        .is_none()
        .then(|| config_path().parent().map(Path::to_path_buf))
        .flatten()
}

/// Compares Jira sites by host, so `https://example.atlassian.net/` and
/// `example.atlassian.net` agree.
pub fn same_site(a: &str, b: &str) -> bool {
    site_host(a).eq_ignore_ascii_case(&site_host(b))
}

pub fn site_host(s: &str) -> String {
    let s = s.trim();
    let s = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    s.split('/').next().unwrap_or("").to_string()
}

/// Normalises a Jira site to `https://host`.
pub fn jira_base(site: &str) -> String {
    format!("https://{}", site_host(site))
}

pub fn project_name(repo: &Path) -> String {
    repo.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("taskman-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Reads `json` as a saved file; returns the result and whether the file is still as written.
    fn read<T: serde::de::DeserializeOwned + Default>(
        name: &str,
        json: &str,
    ) -> (Result<T, String>, bool) {
        let path = scratch(name).join("tickets.json");
        std::fs::write(&path, json).unwrap();
        let res = read_json(&path);
        (res, std::fs::read_to_string(&path).is_ok_and(|s| s == json))
    }

    #[test]
    fn a_missing_file_is_empty_state() {
        let saved: Saved = read_json(&scratch("missing").join("tickets.json")).unwrap();
        assert!(saved.projects.is_empty() && saved.tickets.is_empty());
        let config: Config = read_json(&scratch("missing-config").join("config.json")).unwrap();
        assert!(config.auto_update);
    }

    #[test]
    fn keeps_state_written_by_a_newer_taskman() {
        // A provider this build doesn't know, as `local` was to builds before it.
        let newer = r#"{"projects":[{"name":"acme","repo":"/src/acme","tracker":{"provider":"youtrack","site":"acme"}}]}"#;
        let (res, kept) = read::<Saved>("newer", newer);
        let e = res.unwrap_err();
        assert!(e.contains("unknown variant `youtrack`") && e.contains("newer taskman"), "{e}");
        assert!(kept);
        let config = r#"{"accounts":[{"id":"x-1","provider":"youtrack","token":"env:X"}]}"#;
        let (res, kept) = read::<Config>("newer-config", config);
        assert!(res.is_err() && kept);
    }

    #[test]
    fn refuses_a_newer_format_and_leaves_the_file() {
        // Read first, whatever else in the file this build can't read either.
        let newer = r#"{"format": 2, "projects": [{"name": "acme", "repo": "/src/acme", "tracker": {"provider": "youtrack", "site": "acme"}}]}"#;
        let (res, kept) = read::<Saved>("format", newer);
        let e = res.unwrap_err();
        assert!(
            e.contains("format 2") && e.contains("newer taskman wrote it, run that one"),
            "{e}"
        );
        assert!(kept);
        let (res, kept) = read::<Config>("format-config", r#"{"accounts": [], "format": 3}"#);
        assert!(res.unwrap_err().contains("format 3") && kept);
        let (res, kept) = read::<Saved>("format-text", r#"{"format": "2"}"#);
        assert!(res.unwrap_err().contains("invalid type") && kept);
    }

    #[test]
    fn a_file_without_a_format_is_format_1() {
        for (name, json) in [("format-none", "{}"), ("format-1", r#"{"format": 1}"#)] {
            let (saved, _) = read::<Saved>(name, json);
            let saved = saved.unwrap();
            assert!(saved.extra.is_empty(), "{:?}", saved.extra);
            assert_eq!(serde_json::to_value(&saved).unwrap()["format"], 1);
            let (config, _) = read::<Config>(&format!("{name}-config"), json);
            assert_eq!(serde_json::to_value(config.unwrap()).unwrap()["format"], 1);
        }
    }

    /// tickets.json and config.json with a field this build doesn't know in each struct, as a
    /// newer patch release might write them.
    const UNKNOWN_FIELDS: &str = r#"{
      "format": 1,
      "layout": {"sidebar": 30},
      "projects": [{
        "name": "acme", "repo": "/src/acme", "colour": "teal",
        "tracker": {"provider": "linear", "site": "acme", "key": "ENG", "team_id": "t-1"},
        "closed_agents": [{"name": "lead", "session_id": "sid-1", "pinned": true}],
        "worktrees": {"inside": true, "sparse": ["src"]}
      }],
      "tickets": [{
        "key": "ENG-52", "repo": "/src/acme", "cwd": "/src/acme-ENG-52", "priority": 2,
        "base": {"key": "ENG-51", "branch": "eng-51", "merged_at": null},
        "pending_worktree": {"error": "fetch failed", "attempts": 3}
      }],
      "ui": {"show_gh": false, "theme": "light"}
    }"#;
    const UNKNOWN_CONFIG: &str = r#"{
      "format": 1,
      "accounts": [{"id": "linear-1", "provider": "linear", "site": "acme", "token": "env:ACME_LINEAR_TOKEN", "scopes": ["read"]}],
      "sleep_after": null,
      "bell": "off"
    }"#;

    #[test]
    fn keeps_fields_it_doesnt_know_through_a_load_and_a_save() {
        std::fs::create_dir_all(state_dir()).unwrap();
        std::fs::write(state_dir().join("tickets.json"), UNKNOWN_FIELDS).unwrap();
        std::fs::write(config_path(), UNKNOWN_CONFIG).unwrap();
        save(&load().unwrap()).unwrap();
        save_config(&load_config().unwrap()).unwrap();
        for (file, before) in [("tickets.json", UNKNOWN_FIELDS), ("config.json", UNKNOWN_CONFIG)] {
            let after = std::fs::read_to_string(state_dir().join(file)).unwrap();
            let after: Value = serde_json::from_str(&after).unwrap();
            assert_kept(&serde_json::from_str(before).unwrap(), &after, file);
        }
        // Still read for what they are: the ones this build knows aren't taken as unknown.
        let saved = load().unwrap();
        assert_eq!(saved.projects[0].tracker.as_ref().unwrap().key, "ENG");
        assert!(saved.projects[0].worktrees.inside && !saved.ui.show_gh);
        assert_eq!(saved.tickets[0].base.as_ref().unwrap().branch, "eng-51");
        assert_eq!(load_config().unwrap().sleep_after, None);
    }

    #[test]
    fn reads_a_local_board_folder_saved_in_site() {
        // As builds before `folder` wrote it, which they still read: the folder in `site`.
        let old = r#"{"projects":[{"name":"notes","repo":"/src/notes","tracker":{"provider":"local","site":"plans","key":"WP"}}]}"#;
        let (res, _) = read::<Saved>("local-site", old);
        let t = res.unwrap().projects[0].tracker.clone().unwrap();
        assert_eq!(
            (t.site.as_str(), t.folder.as_deref(), t.key.as_str()),
            ("", Some("plans"), "WP")
        );
        assert_eq!(t.board(Path::new("/src/notes")), Some(PathBuf::from("/src/notes/plans")));
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "provider": "local", "site": "plans", "folder": "plans", "key": "WP" })
        );
        assert_eq!(serde_json::from_value::<Tracker>(json).unwrap(), t);
        let unset = Tracker { folder: None, ..t.clone() };
        let reloaded: Tracker =
            serde_json::from_value(serde_json::to_value(&unset).unwrap()).unwrap();
        assert_eq!(reloaded.board(Path::new("/src/notes")), unset.board(Path::new("/src/notes")));
        // Other trackers keep their site, and write no folder.
        let jira = r#"{"provider":"jira","site":"https://example.atlassian.net","key":"ENG","account":"jira-1"}"#;
        let t: Tracker = serde_json::from_str(jira).unwrap();
        assert_eq!((t.site.as_str(), t.folder.as_deref()), ("https://example.atlassian.net", None));
        assert_eq!(serde_json::to_string(&t).unwrap(), jira);
    }

    #[test]
    fn keeps_a_file_that_isnt_json() {
        let (res, kept) = read::<Saved>("invalid", r#"{"projects": [}"#);
        assert!(res.unwrap_err().contains("line 1 column"));
        assert!(kept);
        let (res, kept) = read::<Config>("invalid-config", "{\"accounts\": [],}\n");
        assert!(res.is_err() && kept);
    }

    #[test]
    fn fills_in_missing_ui_prefs() {
        let (res, _) =
            read::<Saved>("ui", r#"{"projects":[{"name":"acme","repo":"/src/acme"}],"ui":{}}"#);
        let saved = res.unwrap();
        assert_eq!(saved.projects[0].name, "acme");
        let defaults = UiPrefs::default();
        assert_eq!(
            (
                saved.ui.sidebar_collapsed,
                saved.ui.show_jira,
                saved.ui.show_comments,
                saved.ui.show_gh
            ),
            (
                defaults.sidebar_collapsed,
                defaults.show_jira,
                defaults.show_comments,
                defaults.show_gh
            )
        );
        let some: UiPrefs = serde_json::from_str(r#"{"show_gh":false}"#).unwrap();
        assert!(!some.show_gh && some.show_jira);
    }

    /// tickets.json and config.json as the first private build wrote them: its own serializer's
    /// output for mock state.
    #[test]
    fn loads_state_saved_by_the_first_build() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/state-first-private-build");
        let saved: Saved = read_json(&dir.join("tickets.json")).unwrap();
        let config: Config = read_json(&dir.join("config.json")).unwrap();

        let names: Vec<_> = saved.projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["acme", "web", "notes"]);
        assert!(saved.projects[1].collapsed && saved.projects[2].tracker.is_none());
        assert!(
            saved
                .projects
                .iter()
                .all(|p| p.closed_agents.is_empty() && p.worktrees == WorktreePrefs::default())
        );
        let tickets: Vec<_> = saved
            .tickets
            .iter()
            .map(|t| (t.key.as_str(), t.session_id.as_deref(), t.agent.as_deref()))
            .collect();
        assert_eq!(
            tickets,
            [
                ("ENG-52", Some("sid-52"), None),
                ("acme-lead", Some("sid-lead"), Some("lead")),
                ("WEB-7", None, None)
            ]
        );
        let eng = &saved.tickets[0];
        assert_eq!(eng.branch.as_deref(), Some("eng-52-login"));
        assert_eq!(eng.seen_gh.as_deref(), Some(&["c:7".to_string(), "r:9".to_string()][..]));
        assert!(saved.tickets.iter().all(|t| t.cli == Cli::Claude && t.base.is_none()));
        // Every ticket already had its project; nothing needs making up at startup.
        assert!(saved.tickets.iter().all(|t| saved.projects.iter().any(|p| p.repo == t.repo)));
        assert_eq!(saved.selected, 1);
        assert!(!saved.ui.show_comments && saved.ui.show_gh && !saved.ui.codex_explained);

        // Accounts already had ids: the Jira project names its account, the Linear one matches by
        // workspace.
        let ids: Vec<_> = config.accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["jira-1", "linear-1"]);
        let tracker = |p: usize| saved.projects[p].tracker.as_ref().unwrap();
        assert_eq!(
            (config.account_for(tracker(0)), config.account_for(tracker(1))),
            (Some(0), Some(1))
        );
        assert!(config.auto_update);
        assert_eq!(config.prefix, crate::keys::Prefix::default());

        // Saved again, everything the first build wrote is kept.
        for (file, now) in [
            ("tickets.json", serde_json::to_value(&saved)),
            ("config.json", serde_json::to_value(&config)),
        ] {
            let before: Value =
                serde_json::from_str(&std::fs::read_to_string(dir.join(file)).unwrap()).unwrap();
            assert_kept(&before, &now.unwrap(), file);
        }
    }

    /// Every field in `before` has the same value in `after`, which may add fields.
    fn assert_kept(before: &Value, after: &Value, at: &str) {
        match (before, after) {
            (Value::Object(b), Value::Object(a)) => {
                for (k, v) in b {
                    assert_kept(v, a.get(k).unwrap_or(&Value::Null), &format!("{at}.{k}"));
                }
            }
            (Value::Array(b), Value::Array(a)) if b.len() == a.len() => {
                for (i, (b, a)) in b.iter().zip(a).enumerate() {
                    assert_kept(b, a, &format!("{at}[{i}]"));
                }
            }
            _ => assert_eq!(before, after, "{at}"),
        }
    }

    #[test]
    fn refuses_a_second_instance() {
        let dir = scratch("lock");
        let first = lock_in(&dir).unwrap();
        let e = lock_in(&dir).err().unwrap();
        assert!(e.contains(&format!("already running (pid {})", std::process::id())), "{e}");
        drop(first);
        // A test forking a command meanwhile (git, in the worktree tests) shares the lock until
        // its child execs, which closes it there; so the lock can outlive `first` for a moment.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while lock_in(&dir).is_err() {
            assert!(std::time::Instant::now() < until, "the lock stayed held after it was dropped");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn shell_variables_become_env_sources() {
        assert_eq!(env_source_from_shell("$JIRA_API_TOKEN"), "env:JIRA_API_TOKEN");
        assert_eq!(env_source_from_shell("${JIRA_API_TOKEN}"), "env:JIRA_API_TOKEN");
        assert_eq!(env_source_from_shell("$_x9"), "env:_x9");
        for kept in [
            "$(op read op://acme/jira/token)",
            "$FOO bar",
            "${FOO",
            "$FOO}",
            "${FOO}x",
            "${}",
            "$",
            "$9FOO",
            "$FOO-BAR",
            "${FOO:-bar}",
            "$$FOO",
            "env:$FOO",
            "jira-raw-secret",
            "env:JIRA_API_TOKEN",
            "cmd:echo $FOO",
            "",
        ] {
            assert_eq!(env_source_from_shell(kept), kept);
        }
    }

    /// Unknown fields, whatever their names and values, survive a load and a save in every struct
    /// of tickets.json and config.json.
    mod unknown_fields {
        use super::{Config, Extra, Saved, assert_kept};
        use proptest::prelude::*;
        use serde_json::{Value, json};

        fn value() -> impl Strategy<Value = Value> {
            let leaf = prop_oneof![
                Just(Value::Null),
                any::<bool>().prop_map(Value::from),
                any::<i64>().prop_map(Value::from),
                any::<u64>().prop_map(Value::from),
                any::<f64>()
                    .prop_filter("JSON has no NaN", |f| f.is_finite())
                    .prop_map(Value::from),
                ".{0,12}".prop_map(Value::from),
            ];
            leaf.prop_recursive(3, 16, 4, |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..4).prop_map(Value::from),
                    prop::collection::btree_map(".{0,8}", inner, 0..4)
                        .prop_map(|m| Value::Object(m.into_iter().collect())),
                ]
            })
        }

        /// Fields no build has, so none is read as a known one.
        fn fields() -> impl Strategy<Value = Extra> {
            prop::collection::btree_map("new_.{0,10}", value(), 0..3)
                .prop_map(|m| m.into_iter().collect())
        }

        fn add(to: &mut Value, fields: Extra) {
            to.as_object_mut().unwrap().extend(fields);
        }

        /// Loads `before` as the TUI does, saves it again and checks nothing in it was lost.
        fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(before: &Value) {
            let loaded: T = serde_json::from_str(&before.to_string()).unwrap();
            let after: Value =
                serde_json::from_slice(&serde_json::to_vec_pretty(&loaded).unwrap()).unwrap();
            assert_kept(before, &after, "");
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn in_tickets_json(
                top in fields(), project in fields(), tracker in fields(), closed in fields(),
                worktrees in fields(), ticket in fields(), base in fields(), pending in fields(),
                ui in fields(),
            ) {
                let mut saved = json!({
                    "projects": [{
                        "name": "acme", "repo": "/src/acme",
                        "tracker": {"provider": "jira", "site": "https://example.atlassian.net", "key": "ENG"},
                        "closed_agents": [{"name": "lead", "session_id": "sid-1"}],
                        "worktrees": {"branch": "{tracker}"},
                    }],
                    "tickets": [{
                        "key": "ENG-52", "repo": "/src/acme", "cwd": "/src/acme",
                        "base": {"key": "ENG-51", "branch": "eng-51"},
                        "pending_worktree": {"error": "fetch failed"},
                    }],
                    "ui": {},
                });
                add(&mut saved, top);
                let p = &mut saved["projects"][0];
                add(p, project);
                add(&mut p["tracker"], tracker);
                add(&mut p["closed_agents"][0], closed);
                add(&mut p["worktrees"], worktrees);
                let t = &mut saved["tickets"][0];
                add(t, ticket);
                add(&mut t["base"], base);
                add(&mut t["pending_worktree"], pending);
                add(&mut saved["ui"], ui);
                round_trip::<Saved>(&saved);
            }

            #[test]
            fn in_config_json(top in fields(), account in fields()) {
                let mut config = json!({
                    "accounts": [{"id": "jira-1", "provider": "jira", "site": "https://example.atlassian.net",
                                  "email": "dev@example.com", "token": "env:JIRA_API_TOKEN"}],
                    "sleep_after": null,
                });
                add(&mut config, top);
                add(&mut config["accounts"][0], account);
                round_trip::<Config>(&config);
            }
        }
    }

    mod shell_variables {
        use super::env_source_from_shell;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn a_name_becomes_env(name in "[A-Za-z_][A-Za-z0-9_]{0,30}") {
                prop_assert_eq!(env_source_from_shell(&format!("${name}")), format!("env:{name}"));
                prop_assert_eq!(env_source_from_shell(&format!("${{{name}}}")), format!("env:{name}"));
            }

            #[test]
            fn anything_else_is_kept(token in ".{0,30}") {
                let name = token.strip_prefix("${").and_then(|t| t.strip_suffix('}'))
                    .or_else(|| token.strip_prefix('$'));
                let is_var = name.is_some_and(|n| {
                    !n.is_empty()
                        && !n.starts_with(|c: char| c.is_ascii_digit())
                        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                });
                if !is_var {
                    prop_assert_eq!(env_source_from_shell(&token), token);
                }
            }

            /// Strings near the shape of a variable, which a fully random one rarely is.
            #[test]
            fn near_misses_are_kept(token in r"\$\{?[A-Za-z0-9_ ${}()=:-]{0,12}\}?") {
                let out = env_source_from_shell(&token);
                prop_assert!(out == token || out.starts_with("env:"), "{}", out);
                if let Some(name) = out.strip_prefix("env:") {
                    let (bare, braced) = (format!("${name}"), format!("${{{name}}}"));
                    prop_assert!(token == bare || token == braced, "{}", token);
                    prop_assert!(!name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()));
                    prop_assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
                }
            }
        }
    }

    #[test]
    fn token_commands() {
        let secs = std::time::Duration::from_secs;
        assert_eq!(resolve_token("cmd:echo ' tok '", secs(10)), Ok("tok".into()));
        assert_eq!(
            resolve_token("cmd:echo locked >&2; exit 1", secs(10)),
            Err("the token command failed: locked; see Storing a token in taskman's guide".into())
        );
        assert_eq!(
            resolve_token("cmd:true", secs(10)),
            Err("the token command printed no token; see Storing a token in taskman's guide".into())
        );
        let slow = resolve_token("cmd:sleep 10; echo tok", secs(1)).unwrap_err();
        assert!(slow.starts_with("the token command didn't finish within 1s"), "{slow}");
        let raw = resolve_token("lin_api_raw", secs(1)).unwrap_err();
        assert!(raw.contains("env:VAR or cmd:COMMAND") && !raw.contains("lin_api_raw"), "{raw}");
    }

    #[test]
    fn an_unset_variable_says_where_to_set_it() {
        let e = unset("JIRA_API_TOKEN", "/bin/zsh", false);
        assert_eq!(
            e,
            "JIRA_API_TOKEN isn't set in the shell taskman started from: add export JIRA_API_TOKEN=… to ~/.zshrc, then open a new terminal and start taskman there"
        );
        assert_eq!(unset("JIRA_API_TOKEN", "/bin/zsh", true), e);
        // Terminal on macOS starts bash as a login shell, which skips ~/.bashrc.
        assert!(unset("X", "/bin/bash", true).contains("export X=… to ~/.bash_profile,"));
        assert!(unset("X", "/usr/bin/bash", false).contains("export X=… to ~/.bashrc,"));
        let other = "to your shell's startup file (~/.zshrc for zsh, ~/.bash_profile for bash),";
        assert!(unset("X", "/usr/local/bin/fish", true).contains(other));
        let other = "to your shell's startup file (~/.zshrc for zsh, ~/.bashrc for bash),";
        assert!(unset("X", "", false).contains(other));
        let read = resolve_token("env:TASKMAN_TEST_UNSET_TOKEN", std::time::Duration::from_secs(1));
        assert!(read.unwrap_err().starts_with("TASKMAN_TEST_UNSET_TOKEN isn't set"));
    }

    #[test]
    fn keychain_sources() {
        assert!(is_token_source("keychain:jira-1"));
        assert!(!is_token_source("keychain:"));
        assert!(!is_token_source("keychain:two words"));
        assert_eq!(short_source("keychain:jira-1"), "keychain");
        // No stub yet: none is found, and no real keychain is asked.
        let none = resolve_token("keychain:jira-1", std::time::Duration::from_secs(1)).unwrap_err();
        assert!(none.ends_with("isn't installed"), "{none}");
        crate::keychain::stub();
        crate::keychain::Keychain::os().store("jira-1", "ATATT-mock").unwrap();
        let a = Account {
            id: "jira-1".into(),
            provider: Provider::Jira,
            site: String::new(),
            email: "dana@example.com".into(),
            token: "keychain:jira-1".into(),
            extra: Default::default(),
        };
        assert_eq!(a.resolve_token(), Ok("ATATT-mock".into()));
        assert_eq!(a.label(), "dana@example.com · keychain");
    }

    #[test]
    fn keeps_dont_create_across_a_reload() {
        let project = |worktrees| Project {
            name: "acme".into(),
            repo: "/src/acme".into(),
            tracker: None,
            collapsed: false,
            closed_agents: Vec::new(),
            worktrees,
            extra: Default::default(),
        };
        let reload = |p: Project| {
            let saved = Saved { projects: vec![p], ..Default::default() };
            let back: Saved =
                serde_json::from_slice(&serde_json::to_vec_pretty(&saved).unwrap()).unwrap();
            back.projects[0].worktrees.clone()
        };
        let off = WorktreePrefs { create: Some(false), ..Default::default() };
        assert!(!reload(project(off.clone())).creates());
        assert_eq!(reload(project(off)).create, Some(false));
        // The default isn't written, and a project saved without it creates.
        assert!(reload(project(WorktreePrefs::default())).creates());
        let old: Saved =
            serde_json::from_str(r#"{"projects":[{"name":"acme","repo":"/src/acme"}]}"#).unwrap();
        assert!(old.projects[0].worktrees.creates());
    }

    #[test]
    fn picks_accounts_by_id_then_site_then_sole() {
        let acc = |id: &str, provider, site: &str| Account {
            id: id.into(),
            provider,
            site: site.into(),
            email: String::new(),
            token: "env:X".into(),
            extra: Default::default(),
        };
        let mut c = Config {
            accounts: vec![
                acc("jira-1", Provider::Jira, "https://a.atlassian.net"),
                acc("linear-1", Provider::Linear, "acme"),
            ],
            ..Default::default()
        };
        let t = |provider, site: &str, account: Option<&str>| Tracker {
            provider,
            site: site.into(),
            key: String::new(),
            folder: None,
            account: account.map(Into::into),
            extra: Default::default(),
        };
        // sole Jira account, any site
        assert_eq!(c.account_for(&t(Provider::Jira, "b.atlassian.net", None)), Some(0));
        assert_eq!(c.account_for(&t(Provider::Linear, "acme", None)), Some(1));
        // another workspace's key won't do
        assert_eq!(c.account_for(&t(Provider::Linear, "other", None)), None);
        c.accounts.push(acc("linear-2", Provider::Linear, "other"));
        assert_eq!(c.account_for(&t(Provider::Linear, "third", Some("linear-2"))), Some(2));
        assert_eq!(c.next_id(Provider::Linear), "linear-3");
    }

    #[test]
    fn updates_install_unless_turned_off() {
        let old: Config = serde_json::from_str(r#"{"accounts":[]}"#).unwrap();
        assert!(old.auto_update);
        assert!(Config::default().auto_update);
        let off: Config = serde_json::from_str(r#"{"accounts":[],"auto_update":false}"#).unwrap();
        assert!(!off.auto_update);
    }

    #[test]
    fn sleep_settings_default_to_an_hour_and_no_limit() {
        let old: Config = serde_json::from_str(r#"{"accounts":[]}"#).unwrap();
        assert_eq!((old.sleep_after, old.max_awake), (Some(SLEEP_AFTER), None));
        let set: Config =
            serde_json::from_str(r#"{"accounts":[],"sleep_after":null,"max_awake":4}"#).unwrap();
        assert_eq!((set.sleep_after, set.max_awake), (None, Some(4)));
        let saved = serde_json::to_string(&Config::default()).unwrap();
        assert!(saved.contains(r#""sleep_after":3600"#), "{saved}");
    }

    #[test]
    fn prefix_defaults_to_ctrl_a() {
        use crate::keys::Prefix;
        let old: Config = serde_json::from_str(r#"{"accounts":[]}"#).unwrap();
        assert_eq!(old.prefix, Prefix::CtrlA);
        let b: Config = serde_json::from_str(r#"{"accounts":[],"prefix":"ctrl+b"}"#).unwrap();
        assert_eq!(b.prefix, Prefix::CtrlB);
        assert!(serde_json::to_string(&b).unwrap().contains(r#""prefix":"ctrl+b""#));
        // A typo doesn't stop taskman starting.
        let typo: Config = serde_json::from_str(r#"{"accounts":[],"prefix":"ctlr+b"}"#).unwrap();
        assert_eq!(typo.prefix, Prefix::CtrlA);
    }

    #[test]
    fn tickets_default_to_claude() {
        let old: Ticket =
            serde_json::from_str(r#"{"key":"ENG-52","repo":"/r","cwd":"/r"}"#).unwrap();
        assert_eq!(old.cli, Cli::Claude);
        assert!(!serde_json::to_string(&old).unwrap().contains("cli"));
        let codex = Ticket { cli: Cli::Codex, ..old };
        let back: Ticket = serde_json::from_str(&serde_json::to_string(&codex).unwrap()).unwrap();
        assert_eq!(back.cli, Cli::Codex);
        assert_eq!(Cli::from_label("Codex"), Cli::Codex);
        assert_eq!(back.base, None);
        assert!(!serde_json::to_string(&back).unwrap().contains("base"));
        let stacked = Ticket {
            base: Some(Base {
                key: "ENG-51".into(),
                branch: "eng-51-login".into(),
                extra: Default::default(),
            }),
            ..back
        };
        let back: Ticket = serde_json::from_str(&serde_json::to_string(&stacked).unwrap()).unwrap();
        assert_eq!(back.base, stacked.base);
        let prefs: UiPrefs = serde_json::from_str(
            r#"{"sidebar_collapsed":false,"show_jira":true,"show_comments":true,"show_gh":true}"#,
        )
        .unwrap();
        assert!(!prefs.codex_explained);
    }

    #[test]
    fn agents_keep_an_optional_role() {
        let old: Ticket = serde_json::from_str(
            r#"{"key":"acme-tester","repo":"/r","cwd":"/r","agent":"tester"}"#,
        )
        .unwrap();
        assert_eq!(old.role, None);
        assert!(!serde_json::to_string(&old).unwrap().contains("role"));
        let tester = Ticket { role: Some("Test PRs against their tickets".into()), ..old };
        let back: Ticket = serde_json::from_str(&serde_json::to_string(&tester).unwrap()).unwrap();
        assert_eq!(back.role.as_deref(), Some("Test PRs against their tickets"));
        // Closed agents saved before roles, and one closed before its session had an id.
        let closed: ClosedAgent =
            serde_json::from_str(r#"{"name":"tester","session_id":"abc"}"#).unwrap();
        assert_eq!((closed.role, closed.cli), (None, Cli::Claude));
        let early: ClosedAgent =
            serde_json::from_str(r#"{"name":"tester","role":"Test PRs"}"#).unwrap();
        assert_eq!((early.session_id.as_str(), early.role.as_deref()), ("", Some("Test PRs")));
    }

    #[test]
    fn slugs_session_names() {
        assert_eq!(slug("My Project"), "my-project");
        assert_eq!(slug("  spike: auth!! "), "spike-auth");
        assert_eq!(slug("--"), "");
    }

    fn ticket(key: &str, repo: &str) -> Ticket {
        serde_json::from_value(serde_json::json!({ "key": key, "repo": repo, "cwd": repo }))
            .unwrap()
    }

    fn project(name: &str, repo: &str) -> Project {
        serde_json::from_value(serde_json::json!({ "name": name, "repo": repo })).unwrap()
    }

    #[test]
    fn names_a_session_by_its_key_unless_another_project_has_it() {
        let alpha = ticket("WP-1", "/src/alpha");
        let mut projects = vec![project("Alpha Web", "/src/alpha"), project("beta", "/src/beta")];
        let mut all =
            vec![alpha.clone(), ticket("WP-2", "/src/beta"), ticket("ENG-52", "/src/alpha")];
        assert_eq!(session_name(&alpha, &projects, &all, &[]), "WP-1");
        all.push(ticket("wp-1", "/src/beta"));
        assert_eq!(session_name(&alpha, &projects, &all, &[]), "alpha-web/WP-1");
        assert_eq!(session_name(&all[3], &projects, &all, &[]), "beta/wp-1");
        assert_eq!(session_name(&all[2], &projects, &all, &[]), "ENG-52");
        // A project that's gone: its folder's name.
        projects.remove(0);
        assert_eq!(session_name(&alpha, &projects, &all, &[]), "alpha/WP-1");
    }

    #[test]
    fn qualifies_session_names_by_folder_when_project_names_slug_alike() {
        let projects = [
            project("alpha web", "/src/one"),
            project("alpha-web", "/src/two"),
            project("beta", "/src/b"),
        ];
        let all =
            [ticket("WP-1", "/src/one"), ticket("WP-1", "/src/two"), ticket("WP-1", "/src/b")];
        let names: Vec<String> =
            all.iter().map(|t| session_name(t, &projects, &all, &[])).collect();
        let id = |repo: &str| repo_hash(Path::new(repo))[..4].to_string();
        assert_eq!(
            names,
            [
                format!("alpha-web-{}/WP-1", id("/src/one")),
                format!("alpha-web-{}/WP-1", id("/src/two")),
                "beta/WP-1".into()
            ]
        );
        assert_ne!(names[0], names[1]);
        // Stable: the same folder gets the same name at every start.
        assert_eq!(session_name(&all[0], &projects, &all, &[]), names[0]);

        // A running session already has the name (its project was "beta" when it started, and has
        // been renamed since): the next one up.
        let renamed = [project("gamma", "/src/one"), project("beta", "/src/b")];
        let pair = [ticket("WP-1", "/src/one"), ticket("WP-1", "/src/b")];
        assert_eq!(
            session_name(&pair[1], &renamed, &pair, &["beta/WP-1"]),
            format!("beta-{}/WP-1", id("/src/b"))
        );
        assert_eq!(
            session_name(&pair[1], &renamed, &pair, &["BETA/wp-1"]),
            format!("beta-{}/WP-1", id("/src/b"))
        );
        // Even the plain key, when a running session has it.
        let alone = [ticket("ENG-52", "/src/b")];
        assert_eq!(session_name(&alone[0], &renamed, &alone, &["ENG-52"]), "beta/ENG-52");
    }

    #[test]
    fn finds_the_sessions_ticket() {
        let all = vec![
            ticket("WP-1", "/src/alpha"),
            ticket("WP-1", "/src/beta"),
            ticket("ENG-52", "/src/alpha"),
        ];
        let find = |key: &str, repo: Option<&str>| {
            session_ticket(&all, key, repo.map(Path::new)).map(|t| &t.repo)
        };
        assert_eq!(find("WP-1", Some("/src/beta")), Some(&PathBuf::from("/src/beta")));
        assert_eq!(find("WP-1", Some("/src/gamma")), None);
        // A session an older taskman started says only its key: enough while it's unique.
        assert_eq!(find("ENG-52", None), Some(&PathBuf::from("/src/alpha")));
        assert_eq!(find("WP-1", None), None);
    }

    #[test]
    fn keeps_status_files_per_project() {
        let dir = Path::new("/state/status");
        let alpha = status_file_in(dir, Path::new("/src/alpha"), "WP-1");
        let other = status_file_in(dir, Path::new("/home/you/alpha"), "WP-1");
        assert!(alpha.to_string_lossy().starts_with("/state/status/alpha-"), "{}", alpha.display());
        assert_eq!(alpha.file_name().unwrap(), "WP-1.json");
        // Folders with the same name in different places are different projects.
        assert_ne!(alpha.parent(), other.parent());
        assert_eq!(alpha, status_file_in(dir, Path::new("/src/alpha"), "WP-1"));
        // Where older versions wrote them.
        assert_eq!(legacy_status_file_in(dir, "WP-1"), Path::new("/state/status/WP-1.json"));
        assert_eq!(
            legacy_status_file_in(dir, "acme lead"),
            Path::new("/state/status/acme_lead.json")
        );
    }

    #[test]
    fn compares_sites_by_host() {
        assert!(same_site("https://example.atlassian.net/", "example.atlassian.net"));
        assert!(!same_site("https://example.atlassian.net", "https://y.atlassian.net"));
    }

    fn jira(id: &str, token: &str) -> Account {
        Account {
            id: id.into(),
            provider: Provider::Jira,
            site: "example.atlassian.net".into(),
            email: "you@example.com".into(),
            token: token.into(),
            extra: Default::default(),
        }
    }

    #[test]
    fn shortens_token_sources() {
        assert_eq!(short_source("env:JIRA_API_TOKEN"), "env:JIRA_API_TOKEN");
        assert_eq!(
            short_source("cmd:security find-generic-password -a you -s taskman-jira -w"),
            "cmd:security"
        );
        assert_eq!(short_source("cmd:  /usr/local/bin/op read op://acme/jira/token"), "cmd:op");
        assert_eq!(short_source("cmd:"), "cmd:");
        assert_eq!(jira("jira-1", "cmd:pass show acme/jira").label(), "you@example.com · cmd:pass");
        // Saved as typed by an older version: the token itself, which no picker may show.
        assert_eq!(
            jira("jira-1", "ATATT3xFfGF0-raw-secret").label(),
            "you@example.com · token saved in config (fix in Settings)"
        );
        let linear = Account {
            id: "linear-1".into(),
            provider: Provider::Linear,
            site: "acme".into(),
            email: String::new(),
            token: "env:LINEAR_API_KEY_ACME".into(),
            extra: Default::default(),
        };
        assert_eq!(linear.label(), "acme · env:LINEAR_API_KEY_ACME");
    }

    #[test]
    fn tells_apart_accounts_with_the_same_short_label() {
        let a = jira("jira-1", "cmd:security find-generic-password -a you -s taskman-jira -w");
        let b = jira("jira-2", "cmd:security find-generic-password -a you -s taskman-jira2 -w");
        let c = jira("jira-3", "env:JIRA_API_TOKEN");
        let d = jira("jira-4", "env:JIRA_API_TOKEN");
        assert_eq!(
            account_labels(&[&a, &b, &c, &d]),
            [
                "you@example.com · cmd:security find-generic-password -a you -s taskman-jira -w",
                "you@example.com · cmd:security find-generic-password -a you -s taskman-jira2 -w",
                "you@example.com · env:JIRA_API_TOKEN (jira-3)",
                "you@example.com · env:JIRA_API_TOKEN (jira-4)",
            ]
        );
        // Without a clash, short labels.
        assert_eq!(
            account_labels(&[&a, &c]),
            ["you@example.com · cmd:security", "you@example.com · env:JIRA_API_TOKEN"]
        );
        // Two tokens saved as typed are told apart by id, never by what they hold.
        let (e, f) = (jira("jira-5", "raw-secret-one"), jira("jira-6", "raw-secret-two"));
        let labels = account_labels(&[&e, &f]);
        assert_eq!(
            labels,
            [
                "you@example.com · token saved in config (fix in Settings) (jira-5)",
                "you@example.com · token saved in config (fix in Settings) (jira-6)",
            ]
        );
        assert!(labels.iter().all(|l| !l.contains("secret")));
    }
}
