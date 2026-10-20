//! `taskman uninstall`: removes taskman in one step, as `rustup self uninstall` does. By default
//! only the program goes, when it's a copy the install script or `taskman update` put there, and
//! the user's data stays for a reinstall to pick up, as `apt remove` leaves it; `--purge` (or yes
//! to the second question) removes that too, as `apt purge` does: the config and state folders and
//! the tokens taskman saved, in the keychain or its own token folder. Data and tokens go together,
//! so no account is left pointing at a token that's gone. It lists what it will remove and asks
//! first. Repositories, worktrees, branches, shell files and the CLIs' own folders are the user's
//! and stay; it lists the worktrees taskman made that are still there. The guide's "Uninstall"
//! section lists the same things, for removing taskman by hand.

use crate::cli::{Arg, Lexer, out};
use crate::keychain::{self, Keychain};
use crate::status::real;
use crate::store::{self, Config, Saved};
use crate::update::{self, Installs};
use crate::{add, keys, resume, token_file};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

const USAGE: &str = "Usage: taskman uninstall [--yes] [--purge]

Removes this program, and asks whether to remove your data too: your projects, accounts and
settings, saved state, and the tokens taskman saved in your keychain or in its token folder. Kept,
a reinstall picks them up again. Lists what it will remove and asks first. Your repositories,
worktrees and branches stay, and so do Claude Code's and Codex's own folders. A copy installed
with cargo or Homebrew, or built from source, stays too: remove it the way it was installed.

  --purge  remove your data and tokens too
  --yes    don't ask, as where there's no terminal to ask in: the program only, or with --purge
           your data and tokens too";

/// The data's question: the second, after the program's, or the only one when the program stays.
const PURGE: &str = "Also remove your projects, accounts, saved state and tokens? They'd be used again if you reinstall.";
const PURGE_ONLY: &str = "Remove your projects, accounts, saved state and tokens? They'd be used again if you reinstall.";

pub fn run(args: &[String]) -> i32 {
    crate::cli::run("uninstall", USAGE, args, |args| uninstall(parse_args(args)?))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Args {
    yes: bool,
    purge: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut lex = Lexer::new(args, USAGE);
    while let Some(arg) = lex.next()? {
        match arg {
            Arg::Option("--yes" | "-y") => out.yes = true,
            Arg::Option("--purge") => out.purge = true,
            Arg::Option(_) => return Err(lex.unknown()),
            Arg::Word(w) => return Err(format!("unknown argument {w}\n\n{USAGE}")),
        }
    }
    Ok(out)
}

fn uninstall(args: Args) -> Result<i32, String> {
    // Its sessions would carry on, and its next save would write the state folder back.
    if add::running_elsewhere() {
        return Err(format!(
            "taskman is running: quit it ({} q, then Enter) so its sessions stop, then run taskman uninstall again",
            keys::prefix()
        ));
    }
    // Held until the state folder is gone, so no taskman starts meanwhile.
    let state = own(&store::state_dir());
    let _lock = if state == Ok(true) { Some(store::lock()?) } else { None };
    let plan = plan();
    if args.purge
        && let Some(e) = &plan.refused
    {
        return Err(e.clone());
    }
    let keychain = Keychain::os();
    print(&plan, &keychain, args.purge);
    let (program, data) = (plan.program.is_some(), plan.has_data());
    if !program && !data {
        out!("\nNothing of taskman's is left to remove.");
        return Ok(0);
    }
    let purge = if args.yes {
        args.purge && data
    } else {
        if !std::io::stdin().is_terminal() {
            return Err(
                "there's no terminal to ask in: pass --yes to remove the program, or --yes --purge to remove your data and tokens too"
                    .into(),
            );
        }
        let first = match (program, args.purge && data) {
            (true, false) => Some("Remove taskman (the program)?"),
            (true, true) => {
                Some("Remove taskman, with your projects, accounts, saved state and tokens?")
            }
            // The program stays, so only the data's question is left.
            (false, _) => None,
        };
        if let Some(question) = first
            && !confirm(question)?
        {
            out!("Nothing was removed.");
            return Ok(1);
        }
        match first {
            // The first question named the data too.
            Some(_) if args.purge => data,
            Some(_) => data && confirm(PURGE)?,
            None => confirm(PURGE_ONLY)?,
        }
    };
    if !program && !purge {
        out!("Nothing was removed.");
        return Ok(if args.yes { 0 } else { 1 });
    }
    Ok(if remove(&plan, &keychain, purge) { 0 } else { 1 })
}

/// What `taskman uninstall` removes, and what it leaves.
#[derive(Debug, Default)]
struct Plan {
    /// taskman's own files and folders, each with what it holds, removed whole. A link inside one
    /// is removed, never followed.
    paths: Vec<(PathBuf, &'static str)>,
    /// Keychain items, by name.
    items: Vec<String>,
    /// The token folder beside a `TASKMAN_CONFIG` file, which may hold other files: removed once
    /// its token files are, if nothing else is left in it.
    tokens_dir: Option<PathBuf>,
    /// The running program, removed last.
    program: Option<PathBuf>,
    /// Why the program stays, when it does.
    program_stays: Option<String>,
    /// Worktrees taskman made that are still there: (repository, worktree).
    worktrees: Vec<(PathBuf, PathBuf)>,
    /// Why tickets.json couldn't be read, which leaves the worktrees unlisted.
    unread: Option<String>,
    /// Why the data can't be removed, if it can't: then the plan holds none, and it all stays.
    refused: Option<String>,
}

impl Plan {
    /// Whether there's data of the user's: what `--purge` removes.
    fn has_data(&self) -> bool {
        !self.paths.is_empty() || !self.items.is_empty()
    }
}

/// Whether there's something at `path`, refusing a link: taskman removes only its own files and
/// folders, and reads none through a link either, so it never reaches what a link leads to. Only
/// `path` itself is taskman's: a link above it (macOS's /var, a home folder reached through one)
/// is where the user keeps it, and is used as it is.
fn own(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            let to = std::fs::read_link(path)
                .map(|t| format!(" to {}", t.display()))
                .unwrap_or_default();
            Err(format!(
                "{} is a link{to}, and taskman removes only its own files and folders, not what a link leads to. Remove the link yourself (and what it leads to, if that's taskman's), then run taskman uninstall again.",
                tilde(path)
            ))
        }
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("couldn't look at {}: {e}", tilde(path))),
    }
}

fn plan() -> Plan {
    let mut plan = Plan::default();
    if let Err(e) = data(&mut plan) {
        plan = Plan { refused: Some(e), ..Plan::default() };
    }
    match program() {
        Some(Ok(exe)) => plan.program = Some(exe),
        Some(Err(why)) => plan.program_stays = Some(why),
        None => {}
    }
    plan
}

/// Adds the data and tokens to `plan`, and the worktrees it lists; refuses a link where one of
/// taskman's own paths should be, and a config.json it can't read, which would leave tokens behind.
fn data(plan: &mut Plan) -> Result<(), String> {
    let own_dir = store::own_config_dir();
    let config_dir = match &own_dir {
        Some(dir) => own(dir)?,
        None => false,
    };
    let config_path = store::config_path();
    let config: Config =
        if own(&config_path)? { store::read_json(&config_path)? } else { Config::default() };
    let tokens = token_file::dir();
    let tokens_there = own(&tokens)?;
    match own_dir {
        Some(dir) if config_dir => {
            let what = if tokens_there {
                "your accounts and settings, and the tokens saved in files"
            } else {
                "your accounts and settings"
            };
            plan.paths.push((dir, what));
        }
        Some(_) => {}
        // `TASKMAN_CONFIG` names a file in a folder that may hold other things: the file goes,
        // and in the token folder beside it, only the files its accounts read.
        None => {
            if config_path.exists() {
                plan.paths.push((config_path, "your accounts and settings"));
            }
            for a in &config.accounts {
                // In this token folder, however its path was spelled when the token was saved.
                if let Some(file) = token_file::file_of(&a.token)
                    && file.parent().is_some_and(|d| real(d) == real(&tokens))
                    && own(&file)?
                    && !plan.paths.iter().any(|(p, _)| *p == file)
                {
                    plan.paths.push((file, "a token saved in a file"));
                }
            }
            plan.tokens_dir = tokens_there.then_some(tokens);
        }
    }
    let state = store::state_dir();
    if own(&state)? {
        let saved =
            if own(&state.join("tickets.json"))? { store::load() } else { Ok(Saved::default()) };
        match saved {
            Ok(saved) => plan.worktrees = worktrees(&saved),
            Err(e) => plan.unread = Some(e),
        }
        plan.paths.push((state, "your projects, tickets and session state"));
    }
    plan.paths = outermost(std::mem::take(&mut plan.paths));
    for a in &config.accounts {
        if let Some(name) = keychain::name_of(&a.token)
            && !plan.items.iter().any(|i| i == name)
        {
            plan.items.push(name.to_string());
        }
    }
    Ok(())
}

/// `paths` without those inside another of them, or listed before: the state folder may hold the
/// config file, or be the config folder, each maybe spelled through a link.
fn outermost<T>(paths: Vec<(PathBuf, T)>) -> Vec<(PathBuf, T)> {
    let all: Vec<PathBuf> = paths.iter().map(|(p, _)| resolved(p)).collect();
    let mut kept: Vec<(PathBuf, T)> = Vec::new();
    for (p, what) in paths {
        let r = resolved(&p);
        let inside = all.iter().any(|q| *q != r && r.starts_with(q));
        if !inside && !kept.iter().any(|(k, _)| resolved(k) == r) {
            kept.push((p, what));
        }
    }
    kept
}

/// `p` with the links above it resolved, but not `p` itself if it's one: the same file or folder
/// whichever way its parents are spelled, never what a link leads to.
fn resolved(p: &Path) -> PathBuf {
    match (p.parent(), p.file_name()) {
        (Some(dir), Some(name)) => real(dir).join(name),
        _ => p.to_path_buf(),
    }
}

/// `p` as `~/...` when it's under the home folder, whichever way either is spelled: the
/// program's path has its links resolved (`current_exe`), and $HOME may be reached through a link
/// (macOS's /var is /private/var).
fn tilde(p: &Path) -> String {
    let home = store::home();
    if p.starts_with(&home) {
        return store::tilde(p);
    }
    match resolved(p).strip_prefix(real(&home)) {
        Ok(rest) => store::tilde(&home.join(rest)),
        Err(_) => store::tilde(p),
    }
}

/// The running program, links resolved: Ok when it's taskman's to remove, a copy the install
/// script or `taskman update` put there (one `taskman update` would replace), else why it stays.
/// A copy installed another way is removed that way, so it's left rather than guessed at. None
/// when it can't be found, as when an update replaced it.
fn program() -> Option<Result<PathBuf, String>> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let installs = update::installs(&exe, &update::cargo_bin(), update::writable(exe.parent()?));
    Some(stays(installs, &exe).map_or(Ok(exe), Err))
}

/// Why the program at `exe` stays, installed as `installs` says, and how to remove it.
fn stays(installs: Installs, exe: &Path) -> Option<String> {
    let shown = tilde(exe);
    Some(match installs {
        Installs::Yes => return None,
        Installs::DevBuild => {
            format!(
                "the program, {shown}: a development build, which `cargo clean` in its checkout removes"
            )
        }
        Installs::Cargo => {
            "the program, installed with cargo: remove it with `cargo uninstall taskman-app`".into()
        }
        Installs::Homebrew => {
            "the program, installed with Homebrew: remove it with `brew uninstall taskman`".into()
        }
        Installs::ReadOnly => format!(
            "the program, {shown}: taskman can't write to its folder, so remove it the way it was installed (a package manager, or sudo rm)"
        ),
    })
}

/// The worktrees taskman made for the tickets it has that are still there, with their
/// repositories.
fn worktrees(saved: &Saved) -> Vec<(PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    for t in &saved.tickets {
        let w = (t.repo.clone(), t.cwd.clone());
        if t.created_worktree
            && real(&t.cwd) != real(&t.repo)
            && t.cwd.is_dir()
            && !out.contains(&w)
        {
            out.push(w);
        }
    }
    out
}

/// Lists what goes and what stays; with `purge`, the data goes.
fn print(plan: &Plan, keychain: &Keychain, purge: bool) {
    if let Some(exe) = &plan.program {
        out!("taskman uninstall removes the program: {}\n", tilde(exe));
    }
    if let Some(e) = &plan.refused {
        out!("Your data and tokens stay: {e}\n");
    }
    if plan.has_data() {
        if purge {
            out!("With --purge, it also removes your data and tokens:");
        } else {
            out!(
                "Your data and tokens, kept for a reinstall unless you remove them too (--purge):"
            );
        }
        for (path, what) in &plan.paths {
            out!("  - {what}: {}", tilde(path));
        }
        if !plan.items.is_empty() {
            let items: Vec<String> =
                plan.items.iter().map(|i| format!("{} {i}", keychain::SERVICE)).collect();
            out!("  - the tokens saved in your {}: {}", keychain.label(), items.join(", "));
        }
        out!("");
    }
    out!("It leaves what's yours:");
    if let Some(why) = &plan.program_stays {
        out!("  - {why}");
    }
    out!("  - your repositories and branches");
    out!(
        "  - Claude Code's and Codex's own folders (~/.claude, ~/.codex), with the sessions' conversations"
    );
    if !plan.worktrees.is_empty() {
        out!(
            "  - the worktrees taskman made that are still there; once you've kept what you need from one, remove it from its repository:"
        );
        for (repo, worktree) in &plan.worktrees {
            out!(
                "      git -C {} worktree remove {}",
                resume::quote(&repo.display().to_string()),
                resume::quote(&worktree.display().to_string())
            );
        }
    }
    if let Some(e) = &plan.unread {
        out!("  - the worktrees taskman made, which it can't list: {e}");
    }
    let path = plan
        .program
        .as_ref()
        .and_then(|exe| exe.parent())
        .map(|dir| format!(", and the PATH line for {}", tilde(dir)))
        .unwrap_or_default();
    out!(
        "  - lines you added to your shell's startup file: an `export …_TOKEN=` or `export …_API_KEY=` line{path}"
    );
}

/// Asks `question` on the terminal; anything but yes is no.
fn confirm(question: &str) -> Result<bool, String> {
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\n{question} [y/N] ");
    let _ = stdout.flush();
    drop(stdout);
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| format!("couldn't read the answer: {e}"))?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Removes the program `plan` found, and with `purge` the data and tokens, the program last,
/// saying what it did. Carries on past a failure, and says what's left and how to remove it. True
/// when everything went.
fn remove(plan: &Plan, keychain: &Keychain, purge: bool) -> bool {
    out!("");
    let mut ok = true;
    let mut failed = |what: String| {
        ok = false;
        eprintln!("taskman uninstall: {what}");
    };
    let none = Plan::default();
    let data = if purge { plan } else { &none };
    // First, while config.json still names them.
    for name in &data.items {
        match keychain.delete(name) {
            Ok(()) => out!("Deleted {} {name} from your {}", keychain::SERVICE, keychain.label()),
            Err(e) => failed(format!("{e}; delete it with {}", keychain.delete_command(name))),
        }
    }
    for (path, _) in &data.paths {
        // A link isn't a folder here, so a link is removed, never followed; and remove_dir_all
        // removes the links inside a folder rather than what they lead to.
        let dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
        let removed = if dir { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        match removed {
            Ok(()) => out!("Removed {}", tilde(path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failed(format!("couldn't remove {}: {e}", tilde(path))),
        }
    }
    // Only when nothing else is in it.
    if let Some(dir) = &data.tokens_dir
        && std::fs::remove_dir(dir).is_ok()
    {
        out!("Removed {}", tilde(dir));
    }
    // Linux and macOS both let a running program's file go: the process keeps what it runs until
    // it ends, and the name is free at once.
    if let Some(exe) = &plan.program {
        match std::fs::remove_file(exe) {
            Ok(()) => out!("Removed {}", tilde(exe)),
            Err(e) => failed(format!("couldn't remove {}: {e}", tilde(exe))),
        }
    }
    let guide = format!("{}/blob/main/docs/guide.md#uninstall", env!("CARGO_PKG_REPOSITORY"));
    if !ok {
        eprintln!(
            "taskman uninstall: the rest is removed. For what's left, see Uninstall in the guide: {guide}"
        );
    } else if plan.program.is_none() {
        out!("Removed your data and tokens; the program stays, as above.");
    } else if !purge && plan.has_data() {
        out!(
            "taskman is uninstalled. Your data and tokens stay where they are, listed above; to remove them later, see Uninstall in the guide: {guide}"
        );
    } else {
        out!("taskman is uninstalled.");
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_yes_and_purge_and_refuses_the_rest() {
        let args = |a: &[&str]| parse_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let read = |yes, purge| Ok(Args { yes, purge });
        assert_eq!(args(&[]), read(false, false));
        assert_eq!(args(&["--yes"]), read(true, false));
        assert_eq!(args(&["-y"]), read(true, false));
        assert_eq!(args(&["--purge"]), read(false, true));
        assert_eq!(args(&["--purge", "--yes"]), read(true, true));
        assert!(args(&["--yes=no"]).unwrap_err().contains("--yes takes no value"));
        assert!(args(&["--force"]).unwrap_err().starts_with("unknown option --force\n\nUsage:"));
        assert!(args(&["all"]).unwrap_err().starts_with("unknown argument all\n\nUsage:"));
    }

    #[test]
    fn keeps_only_the_outermost_paths() {
        let p = |s: &str| (PathBuf::from(s), ());
        let kept =
            |v: Vec<(PathBuf, ())>| outermost(v).into_iter().map(|(p, ())| p).collect::<Vec<_>>();
        let paths = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            kept(vec![p("/c/config.json"), p("/s/taskman"), p("/s/taskman/config.json")]),
            paths(&["/c/config.json", "/s/taskman"])
        );
        // The same folder twice, and a folder that only shares a prefix with another.
        assert_eq!(
            kept(vec![p("/x/taskman"), p("/x/taskman"), p("/x/taskman-old")]),
            paths(&["/x/taskman", "/x/taskman-old"])
        );
        // Spelled through a link above it, as macOS's /var is /private/var.
        let dir = store::scratch::dir();
        let (real, link) = (dir.join("real"), dir.join("link"));
        std::fs::create_dir_all(real.join("taskman")).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (state, config) = (real.join("taskman"), link.join("taskman/config.json"));
        let (state_p, config_p) = ((state.clone(), ()), (config.clone(), ()));
        assert_eq!(kept(vec![config_p, state_p]), vec![state.clone()]);
        assert_eq!(kept(vec![(link.join("taskman"), ()), (state, ())]), vec![link.join("taskman")]);
        // A link itself is never resolved: it isn't the folder it leads to.
        assert_eq!(resolved(&link), link);
    }

    #[test]
    fn leaves_a_program_installed_another_way() {
        let exe = Path::new("/opt/taskman/bin/taskman");
        assert_eq!(stays(Installs::Yes, exe), None);
        for (installs, says) in [
            (Installs::Cargo, "cargo uninstall taskman-app"),
            (Installs::Homebrew, "brew uninstall taskman"),
            (Installs::DevBuild, "cargo clean"),
            (Installs::ReadOnly, "the way it was installed"),
        ] {
            let why = stays(installs, exe).unwrap();
            assert!(why.starts_with("the program") && why.contains(says), "{why}");
        }
    }

    #[test]
    fn lists_the_worktrees_taskman_made_that_are_there() {
        let dir = store::scratch::dir();
        let (repo, made, gone) = (dir.join("repo"), dir.join("repo-ENG-1"), dir.join("repo-ENG-2"));
        std::fs::create_dir_all(&made).unwrap();
        let ticket = |key: &str, cwd: &Path, created: bool| serde_json::json!({"key": key, "repo": repo, "cwd": cwd, "created_worktree": created});
        let saved: Saved = serde_json::from_value(serde_json::json!({"tickets": [
            ticket("ENG-1", &made, true),
            // Removed since, made by the user, and the main checkout.
            ticket("ENG-2", &gone, true),
            ticket("ENG-3", &made, false),
            ticket("ENG-4", &repo, true),
        ]}))
        .unwrap();
        assert_eq!(worktrees(&saved), vec![(repo.clone(), made.clone())]);
    }
}
