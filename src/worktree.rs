//! Creating a ticket's worktree: its branch and folder names from the project's formats, and the
//! git commands that make it.

use crate::resume::Hold;
use crate::store::{self, Provider};
use crate::{git, github, proc, untrusted};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

/// The tracker's own branch name: Linear's `branchName`, or `{KEY}-{slug}` for Jira.
pub const TRACKER: &str = "{tracker}";
/// Beside the main checkout, like `git worktree add ../acme-web-ENG-52`.
pub const BESIDE: &str = "../{repo}-{KEY}";
/// Where `claude --worktree` puts them.
pub const INSIDE: &str = ".claude/worktrees/{KEY}";

pub const BRANCH_PRESETS: [&str; 3] = [TRACKER, "{KEY}-{slug}", "feature/{KEY}-{slug}"];

/// Checks a branch format: known placeholders only, and the ticket in it, so the tracker and
/// taskman can link the branch.
pub fn check_format(format: &str) -> Result<(), String> {
    let known = ["{tracker}", "{KEY}", "{key}", "{slug}"];
    let mut rest = format;
    while let Some(i) = rest.find('{') {
        let end = rest[i..].find('}').map(|j| i + j + 1).unwrap_or(rest.len());
        let p = &rest[i..end];
        if !known.contains(&p) {
            return Err(format!("{p} isn't a placeholder here; use {}", known.join(" ")));
        }
        rest = &rest[end..];
    }
    if [TRACKER, "{KEY}", "{key}"].iter().any(|k| format.contains(k)) {
        Ok(())
    } else {
        Err("Put {KEY} or {tracker} in the branch, so the tracker can link it".into())
    }
}

/// A ticket's branch. `title` is the issue's title and `tracker_branch` Linear's name for the
/// branch; either is empty when unknown (Jira has no branch name, or the issue couldn't
/// be fetched). A tracker's name a brief couldn't carry (`untrusted::branch`) isn't used.
pub fn branch_name(
    format: &str,
    provider: Option<Provider>,
    key: &str,
    title: &str,
    tracker_branch: &str,
) -> String {
    let tracker = match provider {
        // Linear's `branchName`, say, or a Local files ticket's file name.
        Some(p) if p.branch_source().is_some() && untrusted::branch(tracker_branch).is_some() => {
            tracker_branch.to_string()
        }
        // Jira links branches by the key in capitals; lowercase keys don't link.
        _ => "{KEY}-{slug}".to_string(),
    };
    let name = format
        .replace(TRACKER, &tracker)
        .replace("{KEY}", &key.to_uppercase())
        .replace("{key}", &key.to_lowercase())
        .replace("{slug}", &slug(title));
    // An unknown title leaves `ENG-52-`; keep `ENG-52`.
    name.trim_end_matches(['-', '_', '/', '.']).to_string()
}

/// A title as a branch word: lowercase letters and digits joined by dashes, apostrophes dropped
/// (`tracker's` → `trackers`, as Linear does), cut at a word to stay readable.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().filter(|c| !matches!(c, '\'' | '’')) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.len() <= 50 {
        return out.to_string();
    }
    match out[..=50].rfind('-') {
        Some(i) => out[..i].to_string(),
        None => out[..50].to_string(),
    }
}

/// The ticket's folder: `BESIDE` or `INSIDE` filled in for the main checkout `repo`.
pub fn folder_path(format: &str, repo: &Path, key: &str) -> PathBuf {
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let path = repo.join(format.replace("{repo}", &name).replace("{KEY}", &key.to_uppercase()));
    // `~/src/acme-web/../acme-web-ENG-52` reads as `~/src/acme-web-ENG-52`.
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => _ = out.pop(),
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// What `create` made.
#[derive(Debug)]
pub struct Created {
    pub path: PathBuf,
    pub branch: String,
    /// Something worth saying, like starting from the local HEAD because the fetch failed.
    pub note: Option<String>,
    /// It started from the base branch `create` was given.
    pub stacked: bool,
    /// taskman made it: git did now, or a try that a quit cut short did (`create`'s `ours`).
    /// False when the folder already was the branch's worktree otherwise: someone else's, which
    /// closing the ticket must never remove.
    pub made: bool,
}

/// Creates the worktree at `folder` on `branch`: the branch if it exists locally or on origin,
/// else a new one after a fetch, from `base` (another ticket's branch, see `start_point`) or
/// origin's default branch (the local HEAD without a remote or when the fetch fails). A base
/// that's gone everywhere, most likely merged and deleted, falls back to the default branch. A
/// folder that exists is used only if it is already that branch's worktree, and counts as made
/// only when `ours`: taskman started making it there before, and quit before it heard back.
/// Slow: it fetches.
pub fn create(
    repo: &Path,
    folder: &Path,
    branch: &str,
    base: Option<&str>,
    ours: bool,
) -> Result<Created, String> {
    let done = |note, stacked, made| {
        Ok(Created { path: folder.to_path_buf(), branch: branch.to_string(), note, stacked, made })
    };
    let list = github::worktrees(repo).unwrap_or_default();
    let listed = list.iter().find(|w| same_path(&w.path, folder));
    if !folder.exists()
        && let Some(w) = listed
    {
        // Removed by hand: git still lists it, and adds none in its place until it's let go.
        // There's nothing in it to lose.
        let f = folder.to_string_lossy();
        let out = git(repo, &["worktree", "remove", &f]);
        if !ok(&out) {
            return Err(match (&w.locked, out) {
                (Some(_), _) => format!(
                    "git keeps {} as a locked worktree; unlock it with git -C {} worktree unlock {f}",
                    store::tilde(folder),
                    store::tilde(repo)
                ),
                (None, Ok(o)) => git_error(&o),
                (None, Err(e)) => format!("could not run git: {e}"),
            });
        }
    } else if folder.exists() {
        match listed {
            // Left by a `git worktree add` that was killed or stopped at the time limit.
            Some(w) if w.unfinished() => remove_unfinished(repo, folder)?,
            Some(w) if w.branch.as_deref() == Some(branch) => return done(None, false, ours),
            _ => {
                return Err(format!(
                    "{} already exists; move it, or change Folder in Project settings",
                    store::tilde(folder)
                ));
            }
        }
    }
    if !ok(&git(repo, &["check-ref-format", "--branch", branch])) {
        return Err(format!(
            "{branch} isn't a valid branch name; change Branch in Project settings"
        ));
    }
    exclude_if_inside(repo, folder);
    let has_origin = ok(&git(repo, &["remote", "get-url", "origin"]));
    let fetched = has_origin && fetch(repo);
    let f = folder.to_string_lossy();
    let local =
        ok(&git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]));
    let remote = has_origin
        && ok(&git(
            repo,
            &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/origin/{branch}")],
        ));
    let mut note = None;
    let mut stacked = false;
    let out = if local {
        add(repo, &[&f, branch])
    } else if remote {
        add(repo, &["--track", "-b", branch, &f, &format!("origin/{branch}")])
    } else {
        let start = base.and_then(|b| {
            let start = start_point(repo, b, has_origin);
            if start.is_none() {
                note = Some(format!("{b} is gone (merged?), so it starts from the default branch"));
            }
            start
        });
        stacked = start.is_some();
        let start = match start.or_else(|| fetched.then(|| default_branch(repo)).flatten()) {
            Some(b) => b,
            None => {
                if has_origin {
                    note = Some("couldn't fetch origin, so it starts from your local HEAD".into());
                }
                "HEAD".into()
            }
        };
        add(repo, &["--no-track", "-b", branch, &f, &start])
    };
    let err = match out {
        Ok(o) if o.status.success() => return done(note, stacked, true),
        Ok(o) => git_error(&o),
        Err(e) => format!("could not run git: {e}"),
    };
    // git cleans up after a failure of its own, but one stopped at the time limit leaves the
    // worktree half-made. The branch stays: the next try checks it out.
    let list = github::worktrees(repo).unwrap_or_default();
    if list.iter().any(|w| same_path(&w.path, folder) && w.unfinished()) {
        let _ = remove_unfinished(repo, folder);
    }
    Err(err)
}

/// `git worktree add` with `args`. In English, as git locks the worktree with "initializing" in
/// the user's language until its files are checked out, and that's how `Worktree::unfinished`
/// knows one that was stopped.
fn add(repo: &Path, args: &[&str]) -> std::io::Result<Output> {
    proc::run(git::command(repo).args(["worktree", "add"]).args(args).env("LC_ALL", "C"), GIT_LIMIT)
}

/// Removes a worktree whose `git worktree add` never finished, so it can be made again. Its
/// checkout never finished, so it holds no work; git keeps it locked, so it takes two `--force`s.
fn remove_unfinished(repo: &Path, folder: &Path) -> Result<(), String> {
    let f = folder.to_string_lossy();
    if ok(&git(repo, &["worktree", "remove", "--force", "--force", &f])) {
        return Ok(());
    }
    Err(format!(
        "{} is a worktree git never finished making; remove it with \
         git worktree remove --force --force {f}",
        store::tilde(folder)
    ))
}

/// Where a branch stacked on `base` starts: the newer of the local branch and origin's copy, as
/// the base ticket's session may not have pushed its latest commits, or someone else may have
/// pushed more. When the two have diverged, origin's, which the stacked PR targets. None when the
/// branch is in neither place.
pub fn start_point(repo: &Path, base: &str, has_origin: bool) -> Option<String> {
    let exists = |r: &str| ok(&git(repo, &["rev-parse", "--verify", "--quiet", r]));
    let local = format!("refs/heads/{base}");
    let remote = format!("refs/remotes/origin/{base}");
    match (exists(&local), has_origin && exists(&remote)) {
        (true, true) if ok(&git(repo, &["merge-base", "--is-ancestor", &remote, &local])) => {
            Some(local)
        }
        (_, true) => Some(remote),
        (true, false) => Some(local),
        (false, false) => None,
    }
}

/// Whether a ticket's worktree can be removed without losing work, checked when the ticket closes.
#[derive(Debug, PartialEq)]
pub struct RemoveCheck {
    pub path: PathBuf,
    /// What removing it would lose, like "2 uncommitted files". Removal is offered only when empty.
    pub problems: Vec<String>,
    /// The local branch to delete with it: merged, so its work is on the default branch or in
    /// the merged PR.
    pub branch: Option<String>,
    /// The worktree's HEAD, the commit the check found nothing missing from.
    pub head: String,
    /// Ignored files and folders removing it would delete, like `.env` or `data.db`, as git
    /// reports them (a folder holding only ignored files as `name/`); rebuildable caches left out
    /// (`tagged_cache`). Removal is offered only when empty, unless the user chooses to delete
    /// them.
    pub ignored: Vec<String>,
}

impl RemoveCheck {
    /// Nothing removing it would lose.
    pub fn safe(&self) -> bool {
        self.problems.is_empty() && self.ignored.is_empty()
    }

    /// Only ignored files stand in the way: removal is offered as deleting them.
    pub fn safe_but_ignored(&self) -> bool {
        self.problems.is_empty() && !self.ignored.is_empty()
    }

    /// Why it stays, like "it has 1 untracked file, ignored files: .env and data.db".
    pub fn kept_because(&self) -> String {
        let mut why = self.problems.clone();
        if !self.ignored.is_empty() {
            why.push(format!("ignored files: {}", some_of(&self.ignored)));
        }
        format!("it has {}", why.join(", "))
    }
}

/// Up to three names, `a, b and c`, or the first two and how many more: `a, b and 2 more`.
/// Cleaned onto one line each, as a file's name can hold anything.
pub fn some_of(names: &[String]) -> String {
    let names: Vec<String> = names.iter().map(|n| untrusted::line(n)).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [a, b, _, _, ..] => format!("{a}, {b} and {} more", names.len() - 2),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Checks the worktree at `path` for anything removing it would lose: uncommitted, untracked or
/// ignored files (`git status` leaves ignored ones out, and `git worktree remove` deletes them
/// without asking), and commits that are on no remote and not in `merged`, the head commit of the
/// ticket's merged PR (a squash merge leaves the branch's own commits on no remote once GitHub
/// deletes the branch, but commits made after the merge are in neither). Ok(None) when there's
/// no worktree there to remove; Err when git couldn't be run or failed, with what went wrong,
/// since the folder stays then and the reply says why. Local git only, so it's quick enough to
/// run as the dialog opens.
pub fn removal_check(
    repo: &Path,
    path: &Path,
    merged: Option<&str>,
) -> Result<Option<RemoveCheck>, String> {
    let list = checked(repo, &["worktree", "list", "--porcelain"])?;
    let list = github::parse_worktrees(&String::from_utf8_lossy(&list.stdout));
    // The main checkout is never removed, whatever the ticket thinks.
    let Some(listed) = list.iter().skip(1).find(|w| same_path(&w.path, path)) else {
        return Ok(None);
    };
    let head = checked(path, &["rev-parse", "--verify", "HEAD"])?;
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    // Asked for outright, since the repository's settings can hide work from `git status`:
    // `status.showUntrackedFiles=no` (common in big monorepos) leaves new files out, and
    // `diff.ignoreSubmodules` a submodule's changes. `ls-files` and `rev-list` below read no such
    // setting.
    let status = checked(path, &STATUS)?;
    let status = String::from_utf8_lossy(&status.stdout);
    let untracked = status.lines().filter(|l| l.starts_with("??")).count();
    let changed = status.lines().count() - untracked;
    let hidden = hidden_changes(path)?;
    let ignored = ignored(path)?;
    // A PR head this clone never fetched proves nothing: count against the remotes alone.
    let pr_head = merged
        .filter(|oid| !oid.is_empty() && oid.chars().all(|c| c.is_ascii_hexdigit()))
        .and_then(|oid| rev(repo, &format!("{oid}^{{commit}}")));
    let mut args = vec!["rev-list", "--count", head.as_str(), "--not", "--remotes"];
    args.extend(pr_head.as_deref());
    let o = checked(path, &args)?;
    let unpushed: usize = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(1);
    let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let mut problems = Vec::new();
    if changed > 0 {
        problems.push(plural(changed, "uncommitted file"));
    }
    if untracked > 0 {
        problems.push(plural(untracked, "untracked file"));
    }
    if !hidden.is_empty() {
        let mut kinds: Vec<&str> = hidden.iter().map(|(_, kind)| *kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        let what = plural(hidden.len(), "changed file");
        problems.push(format!("{what} hidden from git status ({})", kinds.join(", ")));
    }
    if unpushed > 0 {
        problems.push(plural(unpushed, "unpushed commit"));
    }
    let within = |tip: &str| ok(&git(repo, &["merge-base", "--is-ancestor", &head, tip]));
    let merged = pr_head.as_deref().is_some_and(within)
        || local_default_branch(repo).is_some_and(|b| within(&b));
    let branch = listed.branch.clone().filter(|_| merged);
    Ok(Some(RemoveCheck { path: path.to_path_buf(), problems, branch, head, ignored }))
}

/// Runs git in `dir` for the removal check. Err says what went wrong, as the reply shows it:
/// git couldn't be run, or refused (`git_error`).
fn checked(dir: &Path, args: &[&str]) -> Result<Output, String> {
    match git(dir, args) {
        Ok(o) if o.status.success() => Ok(o),
        Ok(o) => Err(format!("git {} failed: {}", args[0], git_error(&o))),
        Err(e) => Err(format!("could not run git: {e}")),
    }
}

/// `git status` as the removal check reads it, whatever the repository's settings hide.
const STATUS: [&str; 4] =
    ["status", "--porcelain", "--untracked-files=normal", "--ignore-submodules=none"];

/// Tracked files in the worktree at `path` whose edits `git status` doesn't show, with what hides
/// them: marked assume-unchanged (`git update-index --assume-unchanged`, or every file under
/// `core.ignoreStat`) or skip-worktree (by hand, or by a sparse checkout), as people keep local
/// edits to config files. git never looks at them, but `git worktree remove` deletes them like
/// any other, so each one on disk is read and hashed against the index; one that isn't on disk
/// (a sparse checkout leaves most skip-worktree files out) holds nothing to lose. Err when git
/// fails, with why.
fn hidden_changes(path: &Path) -> Result<Vec<(PathBuf, &'static str)>, String> {
    use std::os::unix::ffi::OsStrExt;
    let o = checked(path, &["ls-files", "-z", "-v", "-s"])?;
    // `h 100644 <blob> 0\t<file>`: a lowercase tag is assume-unchanged, `S` skip-worktree.
    let mut flagged = Vec::new();
    for entry in o.stdout.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else { continue };
        let head = String::from_utf8_lossy(&entry[..tab]);
        let mut words = head.split(' ');
        let (Some(tag), Some(mode), Some(blob)) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        let kind = match tag {
            "S" => "skip-worktree",
            t if t.chars().all(|c| c.is_ascii_lowercase()) => "assume-unchanged",
            _ => continue,
        };
        // A submodule's changes are `git status`'s to report (`--ignore-submodules=none`).
        if mode == "160000" {
            continue;
        }
        let file = PathBuf::from(std::ffi::OsStr::from_bytes(&entry[tab + 1..]));
        flagged.push((file, mode == "120000", blob.to_string(), kind));
    }
    let mut changed = Vec::new();
    let mut files = Vec::new();
    for (file, link, blob, kind) in flagged {
        let Ok(meta) = path.join(&file).symlink_metadata() else { continue };
        match (link, meta.file_type().is_symlink()) {
            // A link's blob is its target; one that can't be read counts as changed.
            (true, true) => {
                let Ok(target) = std::fs::read_link(path.join(&file)) else {
                    changed.push((file, kind));
                    continue;
                };
                if hash_object(path, target.as_os_str().as_bytes())? != blob {
                    changed.push((file, kind));
                }
            }
            // `hash-object` takes paths a line each: one it can't be given counts as changed.
            (false, false) if !file.as_os_str().as_bytes().contains(&b'\n') => {
                files.push((file, blob, kind));
            }
            _ => changed.push((file, kind)),
        }
    }
    if !files.is_empty() {
        // One git for all of them, applying each path's clean filters as `git add` does.
        let mut input = Vec::new();
        for (file, _, _) in &files {
            input.extend_from_slice(file.as_os_str().as_bytes());
            input.push(b'\n');
        }
        let o = hashed(git::command(path).args(["hash-object", "--stdin-paths"]), input)?;
        let hashes = String::from_utf8_lossy(&o.stdout);
        let hashes: Vec<&str> = hashes.lines().collect();
        if hashes.len() != files.len() {
            return Err(format!(
                "git hash-object answered for {} of {} files",
                hashes.len(),
                files.len()
            ));
        }
        for ((file, blob, kind), now) in files.into_iter().zip(hashes) {
            if now != blob {
                changed.push((file, kind));
            }
        }
    }
    Ok(changed)
}

/// The id git gives `content` as a blob, in the repository's own hash.
fn hash_object(repo: &Path, content: &[u8]) -> Result<String, String> {
    let o = hashed(git::command(repo).args(["hash-object", "--stdin"]), content.to_vec())?;
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Runs a `git hash-object` given `input`, as `checked` runs git.
fn hashed(cmd: &mut Command, input: Vec<u8>) -> Result<Output, String> {
    match proc::run_with_input(cmd, input, GIT_LIMIT) {
        Ok(o) if o.status.success() => Ok(o),
        Ok(o) => Err(format!("git hash-object failed: {}", git_error(&o))),
        Err(e) => Err(format!("could not run git: {e}")),
    }
}

/// The ignored files and folders in the worktree at `path`, as `git worktree remove` would delete
/// them, less tagged caches, the OS's folder metadata (`OS_METADATA`) and symlinks: a link holds
/// no content, and git deletes the link, never what it points to, so the notes maintainers link
/// into every worktree don't keep it. A folder holding nothing but ignored files is one `name/`,
/// so a cache reports at its top, where its tag is. Err when git fails, with why.
fn ignored(path: &Path) -> Result<Vec<String>, String> {
    let args = [
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
        "--no-empty-directory",
    ];
    let o = checked(path, &args)?;
    let list = o
        .stdout
        .split(|&b| b == 0)
        .filter(|e| !e.is_empty())
        .map(|e| String::from_utf8_lossy(e).into_owned())
        .filter(|e| match e.strip_suffix('/') {
            Some(_) => !tagged_cache(&path.join(e)),
            // git lists a link without a slash, even one to a folder; `symlink_metadata` sees
            // the link itself, not what it points to.
            None => {
                !OS_METADATA.contains(&e.rsplit('/').next().unwrap_or(e))
                    && !path.join(e).symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())
            }
        })
        .collect();
    Ok(list)
}

/// Files the OS writes into folders you open (Finder's, Explorer's), which never hold work. A
/// global gitignore usually ignores them, so without this exception nearly every worktree opened
/// on macOS would stay. Files only, by exact name in any folder: a folder git reports whole still
/// counts, and this is no list of build folders (`node_modules/`, `.venv/` and `.idea/` count).
const OS_METADATA: [&str; 3] = [".DS_Store", "Thumbs.db", "desktop.ini"];

/// The Cache Directory Tagging spec's signature, which a cache's `CACHEDIR.TAG` starts with.
const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Whether `dir` is a cache its tool rebuilds, tagged as the spec says (Cargo's `target/`,
/// pytest's, mypy's and ruff's caches): deleting it loses nothing. The signature, not just the
/// file's name, so a stray `CACHEDIR.TAG` doesn't make a folder of real work deletable. No list of
/// names like `node_modules`: what isn't tagged counts as work.
fn tagged_cache(dir: &Path) -> bool {
    let mut head = [0; CACHEDIR_SIGNATURE.len()];
    std::fs::File::open(dir.join("CACHEDIR.TAG"))
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| head == CACHEDIR_SIGNATURE)
}

/// Removes a worktree `removal_check` found safe, and its merged branch. `check.ignored` are the
/// ignored files the user chose to delete with it, empty unless they did (`safe_but_ignored`). It
/// checks again first, as the session may have committed or written files while the dialog was
/// open, and keeps everything if anything changed, including ignored files that weren't there. git
/// refuses a worktree with changes, and the branch goes only while it still points at the commit
/// the check verified, so nothing is forced. Err says why nothing was removed. The git that
/// deletes the folder shares `hold`, the ticket's owner lock, which the caller must have taken:
/// it keeps the ticket from coming back into the folder, and if taskman ends meanwhile, until git
/// is done.
pub fn remove(
    repo: &Path,
    check: &RemoveCheck,
    merged: Option<&str>,
    hold: &Hold,
) -> Result<String, String> {
    let place = store::tilde(&check.path);
    let now = removal_check(repo, &check.path, merged)?
        .ok_or_else(|| format!("{place} isn't a worktree any more"))?;
    if !now.problems.is_empty() {
        return Err(format!("it has {} now", now.problems.join(", ")));
    }
    let new: Vec<String> = now.ignored.into_iter().filter(|f| !check.ignored.contains(f)).collect();
    if !new.is_empty() {
        return Err(format!("it has new ignored files: {}", some_of(&new)));
    }
    if now.head != check.head || now.branch != check.branch {
        return Err("its branch moved after the check".into());
    }
    let f = check.path.to_string_lossy();
    // git's own refusal runs `git status` with the repository's settings, so it's told to look for
    // untracked files too (see `STATUS`); the check above already did, this is the last guard.
    let mut cmd = git::command(repo);
    cmd.args(["-c", "status.showUntrackedFiles=normal", "worktree", "remove", &f]);
    hold.share(&mut cmd);
    match proc::run(&mut cmd, GIT_LIMIT) {
        Ok(o) if o.status.success() => {}
        Ok(o) => return Err(git_error(&o)),
        Err(e) => return Err(format!("could not run git: {e}")),
    }
    let Some(b) = &check.branch else { return Ok(format!("Removed {place}")) };
    match delete_branch(repo, b, &check.head) {
        Ok(()) => Ok(format!("Removed {place} and branch {b}")),
        Err(e) => Ok(format!("Removed {place}; kept branch {b}: {e}")),
    }
}

/// Deletes branch `b` only if it still points at `head`, in one step: a commit made after the
/// check moves it, and then it stays. Not `branch -d`: a squash-merged branch isn't an ancestor of
/// anything, but the check found its work merged.
fn delete_branch(repo: &Path, b: &str, head: &str) -> Result<(), String> {
    match git(repo, &["update-ref", "-d", &format!("refs/heads/{b}"), head]) {
        Ok(o) if o.status.success() => {}
        Ok(_) if rev(repo, &format!("refs/heads/{b}")).is_some_and(|now| now != head) => {
            return Err("it has new commits".into());
        }
        Ok(o) => return Err(git_error(&o)),
        Err(e) => return Err(e.to_string()),
    }
    // What `branch -D` also drops: its upstream and other settings.
    let _ = git(repo, &["config", "--remove-section", &format!("branch.{b}")]);
    Ok(())
}

/// Whether two paths are the same folder. git lists worktrees by their real paths, which differ
/// from the ones taskman keeps behind a symlink (macOS's /var, a linked home or projects folder).
pub fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
        // One that's gone can't be resolved, but its folder can: git lists a worktree removed by
        // hand by its real path.
        || a.file_name().is_some()
            && a.file_name() == b.file_name()
            && matches!((a.parent(), b.parent()), (Some(x), Some(y)) if same_path(x, y))
}

/// The commit `rev` names, as a full id.
fn rev(dir: &Path, rev: &str) -> Option<String> {
    let o =
        git(dir, &["rev-parse", "--verify", "--quiet", rev]).ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// The default branch's name, like `main`, as git last saw `origin/HEAD`; `main` when unknown.
/// Quick: no network.
pub fn default_branch_name(repo: &Path) -> String {
    default_branch_of(repo).name
}

/// The branch a ticket's worktree starts from when it stacks on no other (see `create`).
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultBranch {
    /// Its name, like `main`: origin's default as git last saw `origin/HEAD`, else `main`, or
    /// without a remote the main checkout's own branch. `main` too where git can't tell.
    pub name: String,
    /// Where a new worktree starts, for the Add form to say: `origin/main`, or the local HEAD.
    pub start: String,
}

/// As `DefaultBranch` says. Quick: no network.
pub fn default_branch_of(repo: &Path) -> DefaultBranch {
    if let Some(b) = local_default_branch(repo) {
        let name = b.strip_prefix("origin/").unwrap_or(&b).to_string();
        return DefaultBranch { name, start: b };
    }
    let unknown = || DefaultBranch { name: "main".into(), start: "origin/main".into() };
    if !ok(&git(repo, &["rev-parse", "--git-dir"])) {
        return unknown();
    }
    if ok(&git(repo, &["remote", "get-url", "origin"])) {
        // `create` asks the remote, which the form can't wait for.
        return DefaultBranch { start: "origin's default branch".into(), ..unknown() };
    }
    let head = git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty());
    match head {
        Some(b) => DefaultBranch { start: format!("HEAD ({b}): no remote"), name: b },
        None => DefaultBranch { start: "HEAD: no remote".into(), ..unknown() },
    }
}

/// `origin/HEAD` as git last saw it, like `origin/main`, without asking the remote.
fn local_default_branch(repo: &Path) -> Option<String> {
    let o = git(repo, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Origin's default branch, like `origin/main`: from `origin/HEAD`, or by asking the remote when a
/// remote added by hand never set it.
fn default_branch(repo: &Path) -> Option<String> {
    if let Some(b) = local_default_branch(repo) {
        return Some(b);
    }
    let o = git::run(repo, &["ls-remote", "--symref", "origin", "HEAD"], FETCH_LIMIT)
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&o.stdout);
    let head =
        text.lines().find_map(|l| l.strip_prefix("ref: refs/heads/")?.split_whitespace().next())?;
    let b = format!("origin/{head}");
    ok(&git(repo, &["rev-parse", "--verify", "--quiet", &b])).then_some(b)
}

/// A worktree inside the main checkout would show up there as untracked: ignore its folder in
/// `.git/info/exclude`, which stays local, rather than touching `.gitignore`.
fn exclude_if_inside(repo: &Path, folder: &Path) {
    let Ok(rel) = folder.strip_prefix(repo) else { return };
    let dir = rel.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(rel);
    let line = format!("/{}/", dir.display());
    let Some(common) = git(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .ok()
        .filter(|o| o.status.success())
    else {
        return;
    };
    let path = PathBuf::from(String::from_utf8_lossy(&common.stdout).trim()).join("info/exclude");
    // Bytes, as the file needn't be UTF-8, and appended to: a read that failed must never empty
    // the user's own rules.
    let text = match std::fs::read(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => return,
    };
    if text.split(|&b| b == b'\n').any(|l| l.trim_ascii() == line.as_bytes()) {
        return;
    }
    let sep = if text.is_empty() || text.ends_with(b"\n") { "" } else { "\n" };
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    let file = std::fs::OpenOptions::new().append(true).create(true).open(&path);
    if let Ok(mut file) = file {
        let _ = file.write_all(format!("{sep}# taskman worktrees\n{line}\n").as_bytes());
    }
}

/// The session waits on the network calls, so an unreachable remote gives up after a while
/// rather than holding it for minutes.
const FETCH_LIMIT: Duration = Duration::from_secs(30);
/// Long enough for a big checkout and the hooks it runs; it only stops a git that hangs.
const GIT_LIMIT: Duration = Duration::from_secs(300);

fn fetch(repo: &Path) -> bool {
    ok(&git::run(repo, &["fetch", "--quiet", "origin"], FETCH_LIMIT))
}

fn git(repo: &Path, args: &[&str]) -> std::io::Result<Output> {
    git::run(repo, args, GIT_LIMIT)
}

fn ok(out: &std::io::Result<Output>) -> bool {
    out.as_ref().is_ok_and(|o| o.status.success())
}

/// git's own complaint, without its `fatal:` prefix.
fn git_error(o: &Output) -> String {
    let err = String::from_utf8_lossy(&o.stderr);
    let line = err
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("git worktree add failed")
        .trim();
    line.strip_prefix("fatal: ")
        .or_else(|| line.strip_prefix("error: "))
        .unwrap_or(line)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn names_branches_per_tracker() {
        let linear = Some(Provider::Linear);
        let jira = Some(Provider::Jira);
        assert_eq!(
            branch_name(TRACKER, linear, "ENG-52", "Fix login", "you/eng-52-fix-login"),
            "you/eng-52-fix-login"
        );
        // A Local files ticket's branch is made from its file's name (`local::File::branch`).
        assert_eq!(
            branch_name(TRACKER, Some(Provider::Local), "WP-3", "Fix login", "WP-3-fix-login"),
            "WP-3-fix-login"
        );
        assert_eq!(
            branch_name(TRACKER, jira, "eng-52", "Fix the login page", ""),
            "ENG-52-fix-the-login-page"
        );
        // A tracker's name with characters a brief can't carry gives way to the key and title.
        assert_eq!(
            branch_name(TRACKER, linear, "ENG-52", "Fix login", "you/eng-52 $(x)\u{E0041}"),
            "ENG-52-fix-login"
        );
        // The issue couldn't be fetched: just the key.
        assert_eq!(branch_name(TRACKER, linear, "ENG-52", "", ""), "ENG-52");
        assert_eq!(branch_name(TRACKER, None, "ENG-52", "", ""), "ENG-52");
        assert_eq!(
            branch_name(
                "feature/{KEY}-{slug}",
                linear,
                "ENG-52",
                "Fix login",
                "you/eng-52-fix-login"
            ),
            "feature/ENG-52-fix-login"
        );
        assert_eq!(branch_name("{key}", jira, "ENG-52", "Fix login", ""), "eng-52");
    }

    #[test]
    fn slugs_titles() {
        assert_eq!(
            slug("Create a worktree, with the tracker's branch name"),
            "create-a-worktree-with-the-trackers-branch-name"
        );
        assert_eq!(slug("  Fix: login (SSO)  "), "fix-login-sso");
        let long =
            slug("Create a worktree for each ticket with the tracker's branch name and more words");
        assert!(long.len() <= 50 && !long.ends_with('-'), "{long}");
        assert_eq!(long, "create-a-worktree-for-each-ticket-with-the");
    }

    #[test]
    fn places_folders() {
        let repo = Path::new("/src/acme-web");
        assert_eq!(folder_path(BESIDE, repo, "eng-52"), Path::new("/src/acme-web-ENG-52"));
        assert_eq!(
            folder_path(INSIDE, repo, "ENG-52"),
            Path::new("/src/acme-web/.claude/worktrees/ENG-52")
        );
    }

    /// A clone of a bare `origin` whose main moves on after the clone, so a worktree from the
    /// latest main has a commit the local checkout doesn't.
    fn scratch_repo(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("taskman-wt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // git reports worktrees by their real path, and macOS's temp folder is behind a symlink
        // (/var -> /private/var).
        let root = std::fs::canonicalize(&root).unwrap();
        let sh = |dir: &Path, cmd: &str| {
            let o = Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap();
            assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
        };
        let id = "-c user.name=t -c user.email=t@example.com";
        sh(
            &root,
            "git init -q --bare -b main origin.git && git clone -q origin.git acme-web 2>/dev/null",
        );
        let repo = root.join("acme-web");
        sh(
            &repo,
            &format!(
                "git {id} commit -q --allow-empty -m one && git push -q origin main && git branch eng-7-old && git push -q origin eng-7-old:eng-8-remote && git remote set-head origin main >/dev/null"
            ),
        );
        sh(
            &root,
            &format!(
                "git clone -q origin.git other && cd other && git {id} commit -q --allow-empty -m two && git push -q origin main"
            ),
        );
        (root, repo)
    }

    /// Commits a file to a new branch `eng-52-slow` whose checkout, while `<root>/slow` exists,
    /// writes its process group to `<root>/pgid` and waits, as a big or Git LFS checkout might.
    fn slow_branch(root: &Path, repo: &Path) {
        let r = root.display();
        let smudge = format!(
            "if [ -e '{r}/slow' ]; then ps -o pgid= -p $$ > '{r}/pgid.new' && \
             mv '{r}/pgid.new' '{r}/pgid'; sleep 30; fi; cat"
        );
        let id = "-c user.name=t -c user.email=t@example.com";
        let script = format!(
            "git checkout -q -b eng-52-slow && echo '* filter=slow' > .gitattributes && \
             echo hello > a.txt && git add . && git {id} commit -q -m slow && git checkout -q main"
        );
        let o = Command::new("sh").arg("-c").arg(script).current_dir(repo).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(ok(&git(repo, &["config", "filter.slow.smudge", &smudge])));
        std::fs::write(root.join("slow"), "").unwrap();
    }

    /// Waits for the slow checkout to start, then stops its whole process group, as the time
    /// limit does.
    fn stop_slow_checkout(root: &Path) {
        let pgid = root.join("pgid");
        let start = std::time::Instant::now();
        while !pgid.exists() {
            assert!(start.elapsed() < Duration::from_secs(30), "the checkout never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let pgid = std::fs::read_to_string(&pgid).unwrap();
        let o = Command::new("kill").args(["-KILL", "--", &format!("-{}", pgid.trim())]).output();
        assert!(o.unwrap().status.success());
        std::fs::remove_file(root.join("pgid")).unwrap();
    }

    /// The owner lock removal must hold, on a made-up ticket's file in the scratch folder.
    fn hold(root: &Path) -> Hold {
        crate::resume::take(&root.join("ENG.owner")).unwrap().unwrap()
    }

    #[test]
    fn never_leaves_or_adopts_a_half_made_worktree() {
        let (root, repo) = scratch_repo("stopped");
        slow_branch(&root, &repo);
        let folder = folder_path(BESIDE, &repo, "ENG-52");
        let listed = || {
            let list = github::worktrees(&repo).unwrap();
            list.into_iter().find(|w| same_path(&w.path, &folder))
        };

        // Its own `git worktree add` stopped mid-checkout: it says so and takes the folder away.
        let creating = {
            let (repo, folder) = (repo.clone(), folder.clone());
            std::thread::spawn(move || create(&repo, &folder, "eng-52-slow", None, false))
        };
        stop_slow_checkout(&root);
        assert!(creating.join().unwrap().is_err());
        assert!(!folder.exists());
        assert_eq!(listed(), None);

        // One stopped before taskman last quit, say, locked and with every file staged for
        // deletion: made again, not adopted.
        let mut add = Command::new("git");
        add.arg("-C").arg(&repo).args(["worktree", "add"]).arg(&folder).arg("eng-52-slow");
        let mut adding = add
            .env("LC_ALL", "C")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        stop_slow_checkout(&root);
        let _ = adding.wait();
        assert!(listed().is_some_and(|w| w.unfinished()));
        assert_eq!(github::find_worktree(&repo, "ENG-52"), None);
        std::fs::remove_file(root.join("slow")).unwrap();
        let c = create(&repo, &folder, "eng-52-slow", None, false).unwrap();
        assert!(c.made);
        assert_eq!(std::fs::read_to_string(folder.join("a.txt")).unwrap(), "hello\n");
        let status = git(&folder, &["status", "--porcelain"]).unwrap();
        assert_eq!(String::from_utf8_lossy(&status.stdout), "");
        assert_eq!(listed().and_then(|w| w.locked), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn adds_to_an_exclude_file_that_isnt_utf8() {
        let (root, repo) = scratch_repo("exclude");
        let path = repo.join(".git/info/exclude");
        let theirs = b"/secrets/\n# caf\xe9\n/build".to_vec();
        std::fs::write(&path, &theirs).unwrap();
        create(&repo, &folder_path(INSIDE, &repo, "ENG-53"), "ENG-53", None, false).unwrap();
        let mut want = theirs;
        want.extend_from_slice(b"\n# taskman worktrees\n/.claude/worktrees/\n");
        assert_eq!(std::fs::read(&path).unwrap(), want);
        create(&repo, &folder_path(INSIDE, &repo, "ENG-54"), "ENG-54", None, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), want);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Origin's default as git last saw it; without a remote, the branch the main checkout is on,
    /// which is where `create` starts a worktree then.
    #[test]
    fn names_where_a_new_worktree_starts() {
        let (root, repo) = scratch_repo("default");
        let main = DefaultBranch { name: "main".into(), start: "origin/main".into() };
        assert_eq!(default_branch_of(&repo), main);
        let o = Command::new("sh")
            .arg("-c")
            .arg("git init -q -b master local && git -C local remote add upstream ../origin.git")
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let local =
            DefaultBranch { name: "master".into(), start: "HEAD (master): no remote".into() };
        assert_eq!(default_branch_of(&root.join("local")), local);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn creates_worktrees() {
        let (root, repo) = scratch_repo("create");
        let head = |dir: &Path, rev: &str| {
            String::from_utf8(git(dir, &["rev-parse", rev]).unwrap().stdout).unwrap()
        };

        // A new branch starts from the fetched main, not the stale local one.
        let folder = folder_path(BESIDE, &repo, "ENG-52");
        let c = create(&repo, &folder, "ENG-52-fix-login", None, false).unwrap();
        assert_eq!((c.path.as_path(), c.note, c.made), (folder.as_path(), None, true));
        assert_eq!(head(&folder, "HEAD"), head(&repo, "origin/main"));
        assert_ne!(head(&folder, "HEAD"), head(&repo, "main"));

        // An existing branch, local or only on origin, is checked out rather than created.
        assert!(
            create(&repo, &folder_path(BESIDE, &repo, "ENG-7"), "eng-7-old", None, false).is_ok()
        );
        let remote = folder_path(BESIDE, &repo, "ENG-8");
        create(&repo, &remote, "eng-8-remote", None, false).unwrap();
        assert_eq!(head(&remote, "HEAD"), head(&repo, "origin/eng-8-remote"));

        // Its own folder again is fine, though git didn't make it now: made only when taskman
        // started making it there before. Someone else's folder isn't touched.
        assert!(!create(&repo, &folder, "ENG-52-fix-login", None, false).unwrap().made);
        assert!(create(&repo, &folder, "ENG-52-fix-login", None, true).unwrap().made);
        // Found by its real path when it's named through a symlink, as on macOS.
        let linked = root.with_extension("link");
        let _ = std::fs::remove_file(&linked);
        std::os::unix::fs::symlink(&root, &linked).unwrap();
        let via_link = linked.join(folder.file_name().unwrap());
        assert!(!create(&repo, &via_link, "ENG-52-fix-login", None, false).unwrap().made);
        assert!(create(&repo, &via_link, "ENG-52-fix-login", None, true).unwrap().made);
        std::fs::remove_file(&linked).unwrap();
        std::fs::create_dir_all(root.join("acme-web-ENG-9")).unwrap();
        assert!(
            create(&repo, &folder_path(BESIDE, &repo, "ENG-9"), "ENG-9", None, false)
                .unwrap_err()
                .contains("already exists")
        );
        assert!(
            create(&repo, &folder_path(BESIDE, &repo, "ENG-10"), "bad..name", None, false)
                .unwrap_err()
                .contains("valid branch")
        );

        // Inside the checkout, the folder is ignored locally so it doesn't show as untracked.
        create(&repo, &folder_path(INSIDE, &repo, "ENG-53"), "ENG-53", None, false).unwrap();
        let status =
            String::from_utf8(git(&repo, &["status", "--porcelain"]).unwrap().stdout).unwrap();
        assert_eq!(status, "");
        let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches("/.claude/worktrees/").count(), 1);
        create(&repo, &folder_path(INSIDE, &repo, "ENG-54"), "ENG-54", None, false).unwrap();
        assert_eq!(std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap(), exclude);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn makes_a_worktree_removed_by_hand_again() {
        let (root, repo) = scratch_repo("again");
        let head = |dir: &Path| String::from_utf8(git(dir, &["rev-parse", "HEAD"]).unwrap().stdout);
        let folder = folder_path(BESIDE, &repo, "ENG-52");
        create(&repo, &folder, "ENG-52-fix-login", None, false).unwrap();
        let id = ["-c", "user.name=t", "-c", "user.email=t@example.com"];
        assert!(ok(&git(
            &folder,
            &[&id[..], &["commit", "-q", "--allow-empty", "-m", "work"]].concat()
        )));
        let work = head(&folder).unwrap();
        // git still lists it, and would add none in its place.
        std::fs::remove_dir_all(&folder).unwrap();
        let c = create(&repo, &folder, "ENG-52-fix-login", None, false).unwrap();
        assert!(c.made);
        assert_eq!(head(&folder).unwrap(), work, "made again on its branch, with its commits");

        // A locked one is the user's to let go of.
        assert!(ok(&git(&repo, &["worktree", "lock", &folder.to_string_lossy()])));
        std::fs::remove_dir_all(&folder).unwrap();
        let e = create(&repo, &folder, "ENG-52-fix-login", None, false).unwrap_err();
        assert!(e.contains("locked worktree; unlock it with git -C"), "{e}");
        assert!(!folder.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn removes_only_what_loses_nothing() {
        let (root, repo) = scratch_repo("remove");
        let lock = hold(&root);
        let sh = |dir: &Path, cmd: &str| {
            assert!(
                Command::new("sh").arg("-c").arg(cmd).current_dir(dir).status().unwrap().success(),
                "{cmd}"
            );
        };
        let id = "-c user.name=t -c user.email=t@example.com";
        let add = |key: &str| {
            create(&repo, &folder_path(BESIDE, &repo, key), key, None, false).unwrap().path
        };

        // Fresh from main: nothing to lose, and the branch has nothing main lacks.
        let clean = add("ENG-1");
        let check = removal_check(&repo, &clean, None).unwrap().unwrap();
        assert_eq!((check.safe(), check.branch.as_deref()), (true, Some("ENG-1")));

        let dirty = add("ENG-2");
        sh(&dirty, "echo x >> new.txt && git add new.txt && echo y > loose.txt");
        assert_eq!(
            removal_check(&repo, &dirty, None).unwrap().unwrap().problems,
            ["1 uncommitted file", "1 untracked file"]
        );

        // A commit only here would be lost; unmerged, the branch stays.
        let ahead = add("ENG-3");
        sh(&ahead, &format!("git {id} commit -q --allow-empty -m work"));
        let check = removal_check(&repo, &ahead, None).unwrap().unwrap();
        assert_eq!(
            (check.problems.as_slice(), check.branch.as_deref()),
            (&["1 unpushed commit".to_string()][..], None)
        );
        sh(&ahead, "git push -q origin ENG-3");
        let pushed = removal_check(&repo, &ahead, None).unwrap().unwrap();
        assert_eq!((pushed.safe(), pushed.branch.as_deref()), (true, None));
        let pr_head = rev(&ahead, "HEAD").unwrap();
        assert_eq!(
            removal_check(&repo, &ahead, Some(&pr_head)).unwrap().unwrap().branch.as_deref(),
            Some("ENG-3")
        );

        // Removing takes the folder, and the branch only when merged.
        assert!(remove(&repo, &pushed, None, &lock).unwrap().starts_with("Removed"));
        assert!(!ahead.exists());
        assert!(ok(&git(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/ENG-3"])));
        remove(&repo, &removal_check(&repo, &clean, None).unwrap().unwrap(), None, &lock).unwrap();
        assert!(!ok(&git(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/ENG-1"])));

        // Never the main checkout, nor a folder that isn't a worktree any more.
        assert_eq!(removal_check(&repo, &repo, None), Ok(None));
        assert_eq!(removal_check(&repo, &clean, None), Ok(None));
        let _ = std::fs::remove_dir_all(root);
    }

    /// `status.showUntrackedFiles=no` hides new files from `git status`, and from the check `git
    /// worktree remove` runs itself before deleting: the removal check asks for them anyway, so a
    /// worktree with a new file stays.
    #[test]
    fn sees_untracked_files_the_settings_hide() {
        let (root, repo) = scratch_repo("hidden");
        let lock = hold(&root);
        let wt =
            create(&repo, &folder_path(BESIDE, &repo, "ENG-4"), "ENG-4", None, false).unwrap().path;
        let clean = removal_check(&repo, &wt, None).unwrap().unwrap();
        assert!(clean.safe());
        // Set in the repository, which its worktrees share.
        assert!(ok(&git(&repo, &["config", "status.showUntrackedFiles", "no"])));
        std::fs::write(wt.join("notes.txt"), "work\n").unwrap();
        assert_eq!(
            removal_check(&repo, &wt, None).unwrap().unwrap().problems,
            ["1 untracked file"]
        );
        // Removing with the check from before the file finds it now and keeps the folder.
        assert!(remove(&repo, &clean, None, &lock).unwrap_err().contains("1 untracked file"));
        assert!(wt.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// An ignored symlink holds no content, and `git worktree remove` deletes the link, never its
    /// target, so it doesn't keep the worktree (maintainers link their notes into every one);
    /// an ignored file beside it still does.
    #[test]
    fn ignored_symlinks_do_not_count() {
        let (root, repo) = scratch_repo("links");
        let lock = hold(&root);
        let sh = |dir: &Path, cmd: &str| {
            assert!(
                Command::new("sh").arg("-c").arg(cmd).current_dir(dir).status().unwrap().success(),
                "{cmd}"
            );
        };
        let id = "-c user.name=t -c user.email=t@example.com";
        std::fs::write(root.join("notes.md"), "notes\n").unwrap();
        std::fs::create_dir(root.join("shared")).unwrap();
        std::fs::write(root.join("shared/a"), "a\n").unwrap();
        let wt =
            create(&repo, &folder_path(BESIDE, &repo, "ENG-6"), "ENG-6", None, false).unwrap().path;
        sh(
            &wt,
            &format!(
                "printf 'notes.md\\nlocal\\n.env\\n' > .gitignore && git add .gitignore \
                 && git {id} commit -q -m ignore && git push -q origin ENG-6 \
                 && ln -s ../notes.md notes.md && ln -s ../shared local"
            ),
        );
        let check = removal_check(&repo, &wt, None).unwrap().unwrap();
        assert!(check.safe(), "{:?}", check.ignored);
        std::fs::write(wt.join(".env"), "SECRET=1\n").unwrap();
        assert_eq!(removal_check(&repo, &wt, None).unwrap().unwrap().ignored, [".env"]);
        std::fs::remove_file(wt.join(".env")).unwrap();
        remove(&repo, &check, None, &lock).unwrap();
        assert!(!wt.exists());
        assert_eq!(std::fs::read_to_string(root.join("notes.md")).unwrap(), "notes\n");
        assert_eq!(std::fs::read_to_string(root.join("shared/a")).unwrap(), "a\n");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Edits to files marked assume-unchanged or skip-worktree don't show in `git status`, and
    /// `git worktree remove` deletes them: the check reads the files itself. One off the disk, as
    /// a sparse checkout leaves skip-worktree files, is nothing to lose.
    #[test]
    fn keeps_edits_hidden_from_git_status() {
        let (root, repo) = scratch_repo("hidden-edits");
        let lock = hold(&root);
        let sh = |dir: &Path, cmd: &str| {
            assert!(
                Command::new("sh").arg("-c").arg(cmd).current_dir(dir).status().unwrap().success(),
                "{cmd}"
            );
        };
        let id = "-c user.name=t -c user.email=t@example.com";
        let wt =
            create(&repo, &folder_path(BESIDE, &repo, "ENG-5"), "ENG-5", None, false).unwrap().path;
        sh(
            &wt,
            &format!(
                "printf 'a=1\\n' > config.ini && printf 'b=1\\n' > local.ini \
                 && ln -s config.ini link && git add -A && git {id} commit -q -m config \
                 && git push -q origin ENG-5 \
                 && git update-index --assume-unchanged config.ini link \
                 && git update-index --skip-worktree local.ini"
            ),
        );
        let clean = removal_check(&repo, &wt, None).unwrap().unwrap();
        assert!(clean.safe(), "{:?}", clean.problems);
        sh(
            &wt,
            "printf 'a=2\\n' > config.ini && printf 'b=2\\n' > local.ini && ln -sfn local.ini link",
        );
        let status = git(&wt, &STATUS).unwrap().stdout;
        assert!(
            status.is_empty(),
            "git status shows the edits: {}",
            String::from_utf8_lossy(&status)
        );
        let problems = |wt: &Path| removal_check(&repo, wt, None).unwrap().unwrap().problems;
        assert_eq!(
            problems(&wt),
            ["3 changed files hidden from git status (assume-unchanged, skip-worktree)"]
        );
        std::fs::remove_file(wt.join("local.ini")).unwrap();
        assert_eq!(problems(&wt), ["2 changed files hidden from git status (assume-unchanged)"]);
        assert!(remove(&repo, &clean, None, &lock).unwrap_err().contains("hidden from git status"));
        assert_eq!(std::fs::read_to_string(wt.join("config.ini")).unwrap(), "a=2\n");
        let _ = std::fs::remove_dir_all(root);
    }

    /// `git status` leaves ignored files out and `git worktree remove` deletes them: a clean,
    /// pushed worktree with an ignored `.env` must stay unless the user chooses to delete it.
    #[test]
    fn keeps_ignored_files_but_not_tagged_caches() {
        let (root, repo) = scratch_repo("ignored");
        let lock = hold(&root);
        let sh = |dir: &Path, cmd: &str| {
            assert!(
                Command::new("sh").arg("-c").arg(cmd).current_dir(dir).status().unwrap().success(),
                "{cmd}"
            );
        };
        let id = "-c user.name=t -c user.email=t@example.com";
        let tag = "printf 'Signature: 8a477f597d28d172789f06886806bc55\\n# a cache\\n'";
        // Pushed, with a .gitignore of its own: nothing but ignored files to lose.
        let add = |key: &str| {
            let wt =
                create(&repo, &folder_path(BESIDE, &repo, key), key, None, false).unwrap().path;
            sh(
                &wt,
                &format!(
                    "printf '.env\\n*.db\\ntarget/\\nbuild/\\n*.DS_Store\\ndesktop.ini\\n' > .gitignore && git add .gitignore && git {id} commit -q -m ignore && git push -q origin {key}"
                ),
            );
            wt
        };

        // Cargo's target/, tagged, and pytest's cache, which ignores itself: rebuildable.
        let caches = add("ENG-1");
        sh(
            &caches,
            &format!(
                "mkdir -p target/debug .pytest_cache/v && echo x > target/debug/app && {tag} > target/CACHEDIR.TAG \\
                 && echo '*' > .pytest_cache/.gitignore && echo y > .pytest_cache/v/c && {tag} > .pytest_cache/CACHEDIR.TAG"
            ),
        );
        // The OS's folder metadata, wherever it is, isn't work either.
        sh(
            &caches,
            "mkdir -p sub && touch sub/kept && git add sub && git -c user.name=t -c user.email=t@example.com commit -q -m sub && git push -q origin ENG-1 \\
             && touch .DS_Store sub/.DS_Store sub/Thumbs.db desktop.ini",
        );
        let check = removal_check(&repo, &caches, None).unwrap().unwrap();
        assert!(check.safe(), "{check:?}");
        remove(&repo, &check, None, &lock).unwrap();
        assert!(!caches.exists());

        // An ignored .env beside the cache: kept, and named.
        let env = add("ENG-2");
        sh(&env, &format!("echo SECRET=1 > .env && mkdir target && {tag} > target/CACHEDIR.TAG"));
        let check = removal_check(&repo, &env, None).unwrap().unwrap();
        assert_eq!(check.ignored, [".env"]);
        assert!(!check.safe() && check.safe_but_ignored());
        assert_eq!(check.kept_because(), "it has ignored files: .env");

        // A CACHEDIR.TAG without the signature, or a folder git reports whole that holds a cache
        // among other files, isn't a cache.
        let fake = add("ENG-3");
        sh(
            &fake,
            "mkdir -p build/out && echo work > build/out/a && echo nope > build/CACHEDIR.TAG",
        );
        sh(
            &fake,
            &format!(
                "mkdir -p notes/target && echo n > notes/n.db && {tag} > notes/target/CACHEDIR.TAG"
            ),
        );
        // Nor is a folder git reports whole for holding only OS metadata, or a file merely named
        // like it.
        sh(&fake, "mkdir meta && touch meta/.DS_Store x.DS_Store");
        let check = removal_check(&repo, &fake, None).unwrap().unwrap();
        assert_eq!(check.ignored, ["build/", "meta/", "notes/", "x.DS_Store"]);
        assert!(!check.safe());

        // An ignored file written after a clean check: kept, and the file with it.
        let late = add("ENG-4");
        let check = removal_check(&repo, &late, None).unwrap().unwrap();
        assert!(check.safe());
        sh(&late, "echo rows > data.db");
        assert_eq!(
            remove(&repo, &check, None, &lock),
            Err("it has new ignored files: data.db".into())
        );
        assert!(late.join("data.db").exists());

        // Removing anyway deletes what the check listed, but not files that came after it.
        let check = removal_check(&repo, &env, None).unwrap().unwrap();
        sh(&env, "echo more > local.db");
        assert_eq!(
            remove(&repo, &check, None, &lock),
            Err("it has new ignored files: local.db".into())
        );
        assert!(env.join(".env").exists());
        let check = removal_check(&repo, &env, None).unwrap().unwrap();
        assert_eq!(check.ignored, [".env", "local.db"]);
        assert!(remove(&repo, &check, None, &lock).unwrap().starts_with("Removed"));
        assert!(!env.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn names_some_files() {
        let names = |n: &[&str]| some_of(&n.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(names(&[]), "");
        assert_eq!(names(&[".env"]), ".env");
        assert_eq!(names(&[".env", "data.db"]), ".env and data.db");
        assert_eq!(names(&[".env", "data.db", "notes/"]), ".env, data.db and notes/");
        assert_eq!(names(&[".env", "data.db", "a", "b"]), ".env, data.db and 2 more");
        // A file's name can hold a terminal's escape sequence.
        assert_eq!(names(&["a\x1b]52;c;eA==\x07b"]), "a ]52;c;eA== b");
    }

    /// A squash-merged PR whose branch GitHub deleted: the commits it merged are on no remote, and
    /// only the PR's head commit vouches for them.
    #[test]
    fn keeps_commits_made_after_the_merge() {
        let (root, repo) = scratch_repo("after-merge");
        let lock = hold(&root);
        let sh = |dir: &Path, cmd: &str| {
            assert!(
                Command::new("sh").arg("-c").arg(cmd).current_dir(dir).status().unwrap().success(),
                "{cmd}"
            );
        };
        let id = "-c user.name=t -c user.email=t@example.com";
        let branch_at = |b: &str| rev(&repo, &format!("refs/heads/{b}"));
        let wt =
            create(&repo, &folder_path(BESIDE, &repo, "ENG-5"), "ENG-5", None, false).unwrap().path;
        sh(&wt, &format!("git {id} commit -q --allow-empty -m work && git push -q origin ENG-5"));
        let pr_head = rev(&wt, "HEAD").unwrap();
        sh(&wt, "git push -q origin --delete ENG-5 && git fetch -q --prune");
        assert_eq!(
            removal_check(&repo, &wt, None).unwrap().unwrap().problems,
            ["1 unpushed commit"]
        );
        let merged = removal_check(&repo, &wt, Some(&pr_head)).unwrap().unwrap();
        assert_eq!((merged.safe(), merged.branch.as_deref()), (true, Some("ENG-5")));

        // A commit after the merge is in no PR: reported, and the branch kept.
        sh(&wt, &format!("git {id} commit -q --allow-empty -m later"));
        let later = removal_check(&repo, &wt, Some(&pr_head)).unwrap().unwrap();
        assert_eq!(
            (later.problems.as_slice(), later.branch.as_deref()),
            (&["1 unpushed commit".to_string()][..], None)
        );

        // Another ticket's merged PR (ENG-52's, say) vouches for nothing here.
        let other = create(&repo, &folder_path(BESIDE, &repo, "ENG-52"), "ENG-52", None, false)
            .unwrap()
            .path;
        sh(&other, &format!("git {id} commit -q --allow-empty -m theirs"));
        let theirs = rev(&other, "HEAD").unwrap();
        assert_eq!(
            removal_check(&repo, &wt, Some(&theirs)).unwrap().unwrap().problems,
            ["2 unpushed commits"]
        );
        // Nor does a PR head this clone never fetched, or something that isn't a commit id.
        assert!(
            !removal_check(&repo, &wt, Some("0123456789abcdef0123456789abcdef01234567"))
                .unwrap()
                .unwrap()
                .safe()
        );
        assert!(!removal_check(&repo, &wt, Some("--all")).unwrap().unwrap().safe());

        // The dialog's race: its check passed, then the session committed before removal.
        sh(&wt, "git reset -q --hard HEAD~1");
        let check = removal_check(&repo, &wt, Some(&pr_head)).unwrap().unwrap();
        assert!(check.safe());
        sh(&wt, &format!("git {id} commit -q --allow-empty -m during"));
        let during = branch_at("ENG-5").unwrap();
        assert!(
            remove(&repo, &check, Some(&pr_head), &lock).unwrap_err().contains("1 unpushed commit")
        );
        assert!(wt.exists());
        assert_eq!(branch_at("ENG-5").as_deref(), Some(during.as_str()));

        // Moved and pushed: nothing would be lost, but it isn't what was checked, so it stays.
        sh(&wt, "git push -q origin ENG-5");
        assert!(remove(&repo, &check, Some(&pr_head), &lock).unwrap_err().contains("moved"));
        assert!(wt.exists());

        // The branch moved between the worktree going and the branch going: kept.
        assert_eq!(delete_branch(&repo, "ENG-5", &pr_head), Err("it has new commits".into()));
        assert_eq!(branch_at("ENG-5").as_deref(), Some(during.as_str()));
        assert_eq!(delete_branch(&repo, "ENG-5", &during), Ok(()));
        assert_eq!(branch_at("ENG-5"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// git lists worktrees by real path; a project reached through a symlink must still find
    /// its own.
    #[test]
    fn finds_worktrees_through_a_symlink() {
        let (root, _) = scratch_repo("symlink");
        let lock = hold(&root);
        let link = root.join("projects");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let repo = link.join("acme-web");
        let folder = folder_path(BESIDE, &repo, "ENG-52");
        assert!(folder.starts_with(&link));
        create(&repo, &folder, "ENG-52", None, false).unwrap();
        assert!(create(&repo, &folder, "ENG-52", None, false).is_ok());
        assert!(
            create(&repo, &folder, "ENG-53", None, false).unwrap_err().contains("already exists")
        );
        let check = removal_check(&repo, &folder, None).unwrap().unwrap();
        assert_eq!((check.safe(), check.branch.as_deref()), (true, Some("ENG-52")));
        remove(&repo, &check, None, &lock).unwrap();
        assert!(!folder.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stacks_on_another_tickets_branch() {
        let (root, repo) = scratch_repo("stack");
        let sh = |dir: &Path, cmd: &str| {
            assert!(
                Command::new("sh").arg("-c").arg(cmd).current_dir(dir).status().unwrap().success(),
                "{cmd}"
            );
        };
        let id = "-c user.name=t -c user.email=t@example.com";
        let head =
            |rev: &str| String::from_utf8(git(&repo, &["rev-parse", rev]).unwrap().stdout).unwrap();
        let start = |b: &str| start_point(&repo, b, true);
        let base = create(&repo, &folder_path(BESIDE, &repo, "ENG-51"), "eng-51", None, false)
            .unwrap()
            .path;
        sh(
            &base,
            &format!("git {id} commit -q --allow-empty -m base && git push -q origin eng-51"),
        );

        // Pushed and current: either will do, the local branch is taken.
        assert_eq!(start("eng-51").as_deref(), Some("refs/heads/eng-51"));
        // Commits not pushed yet: the local branch, which has them.
        sh(&base, &format!("git {id} commit -q --allow-empty -m local"));
        assert_eq!(start("eng-51").as_deref(), Some("refs/heads/eng-51"));
        let stacked =
            create(&repo, &folder_path(BESIDE, &repo, "ENG-52"), "eng-52", Some("eng-51"), false)
                .unwrap();
        assert!(stacked.stacked);
        assert_eq!(head("eng-52"), head("eng-51"));
        // Diverged from what's on origin: origin's, which the PR targets.
        sh(
            &root,
            &format!(
                "cd other && git fetch -q && git checkout -q eng-51 && git {id} commit -q --allow-empty -m theirs && git push -q origin eng-51"
            ),
        );
        sh(&repo, "git fetch -q");
        assert_eq!(start("eng-51").as_deref(), Some("refs/remotes/origin/eng-51"));
        // Only on origin, or only local.
        assert_eq!(start("eng-8-remote").as_deref(), Some("refs/remotes/origin/eng-8-remote"));
        assert_eq!(start("eng-7-old").as_deref(), Some("refs/heads/eng-7-old"));
        assert_eq!(start_point(&repo, "eng-8-remote", false), None);

        // A base that's gone everywhere: from the default branch, and not stacked.
        let gone =
            create(&repo, &folder_path(BESIDE, &repo, "ENG-53"), "eng-53", Some("eng-50"), false)
                .unwrap();
        assert!(!gone.stacked && gone.note.unwrap().contains("eng-50 is gone"));
        assert_eq!(head("eng-53"), head("origin/main"));
        // The ticket's own branch exists already: it's checked out, and the base doesn't apply.
        let own =
            create(&repo, &folder_path(BESIDE, &repo, "ENG-7"), "eng-7-old", Some("eng-51"), false)
                .unwrap();
        assert!(!own.stacked);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn starts_from_local_head_when_the_fetch_fails() {
        let (root, repo) = scratch_repo("offline");
        git(&repo, &["remote", "set-url", "origin", "/nonexistent/origin.git"]).unwrap();
        let c =
            create(&repo, &folder_path(BESIDE, &repo, "ENG-52"), "ENG-52", None, false).unwrap();
        assert!(c.note.unwrap().contains("local HEAD"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn checks_formats() {
        for f in BRANCH_PRESETS {
            assert!(check_format(f).is_ok(), "{f}");
        }
        assert!(check_format("feature/{slug}").is_err());
        assert!(check_format("{KEY}-{title}").unwrap_err().contains("{title}"));
        assert!(check_format("{repo}-{KEY}").unwrap_err().contains("{repo}"));
    }

    /// A ticket key, as `ticket_key::parse` accepts it, in any case.
    fn key() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9_]{0,6}-[0-9]{1,5}"
    }

    proptest! {
        // Each case runs git.
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn branch_names_are_valid_for_git(
            format in prop::sample::select(vec![BRANCH_PRESETS[0], BRANCH_PRESETS[1], BRANCH_PRESETS[2], "{key}", "fix/{key}-{slug}"]),
            provider in prop::sample::select(vec![None, Some(Provider::Jira), Some(Provider::Linear), Some(Provider::Local)]),
            key in key(),
            title in "(\\PC|[ ./:~^?*\\[\\\\@{}-]){0,80}",
        ) {
            // Without the tracker's own branch name, which git checks when the worktree is made.
            let name = branch_name(format, provider, &key, &title, "");
            let ok = Command::new("git").args(["check-ref-format", "--branch", &name]).output().unwrap().status.success();
            prop_assert!(ok, "{name:?} from {title:?}");
            prop_assert!(name.contains(&key.to_uppercase()) || name.contains(&key.to_lowercase()), "{name:?}");
        }

        #[test]
        fn slugs_are_short_lowercase_words(title in "\\PC{0,120}") {
            let s = slug(&title);
            prop_assert!(s.len() <= 50, "{s:?}");
            prop_assert!(s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "{s:?}");
            prop_assert!(!s.starts_with('-') && !s.ends_with('-') && !s.contains("--"), "{s:?}");
        }

        #[test]
        fn folders_stay_where_their_format_says(repo in "[a-z][a-z0-9._-]{0,15}", key in key()) {
            let repo = Path::new("/src").join(repo);
            let key_upper = key.to_uppercase();
            let beside = folder_path(BESIDE, &repo, &key);
            prop_assert_eq!(beside.parent(), Some(Path::new("/src")));
            prop_assert_eq!(beside.file_name().unwrap().to_string_lossy(), format!("{}-{key_upper}", repo.file_name().unwrap().to_string_lossy()));
            prop_assert_eq!(folder_path(INSIDE, &repo, &key), repo.join(".claude/worktrees").join(&key_upper));
        }
    }
}
