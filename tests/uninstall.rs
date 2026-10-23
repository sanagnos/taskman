//! `taskman uninstall` against a scratch home, state and config, with a stub keychain tool first on
//! PATH. By default it removes only the program (an install script's copy), and leaves the data
//! and tokens for a reinstall; with --purge, or yes to the second question, it removes taskman's
//! own files, folders and tokens too, but nothing a link leads to, nor a token file it didn't
//! write. It refuses while taskman runs, without a terminal to ask in unless --yes, and --purge
//! when one of its own paths is a link.

mod common;

use common::{Tui, scratch_dir, script, taskman};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// The keychain tool this OS's taskman runs.
const KEYCHAIN: &str = if cfg!(target_os = "macos") { "security" } else { "secret-tool" };

fn run(mut cmd: Command, args: &[&str]) -> Output {
    assert_ne!(
        Path::new(cmd.get_program()),
        Path::new(env!("CARGO_BIN_EXE_taskman")),
        "uninstall runs on a copy, never the binary the other tests run"
    );
    cmd.arg("uninstall").args(args).stdin(Stdio::null()).output().unwrap()
}

/// Copies the binary under test to `program`.
fn copy_to(program: &Path) {
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    // cp, not std::fs::copy: a process another test starts meanwhile inherits a file this one has
    // open to write, and running the copy then fails ("Text file busy").
    let copied =
        Command::new("cp").arg(env!("CARGO_BIN_EXE_taskman")).arg(program).status().unwrap();
    assert!(copied.success());
}

/// `program` run as `taskman(dir)` runs the binary under test.
fn run_as(program: &Path, dir: &Path) -> Command {
    let mut cmd = Command::new(program);
    for (k, v) in taskman(dir).get_envs() {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    cmd
}

/// A development build, as `taskman(dir)` runs it, but a copy of the binary under test in a
/// folder not named `target`: uninstall must leave it, and if it didn't, only the copy would go.
fn dev_build(dir: &Path) -> Command {
    let program = dir.join("build/debug/taskman");
    if !program.exists() {
        copy_to(&program);
    }
    run_as(&program, dir)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn gone(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err()
}

/// A stub keychain in `dir/bin` holding item `taskman <name>` for each of `names`.
fn keychain(dir: &Path, names: &[&str]) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(bin.join("items")).unwrap();
    script(&bin, KEYCHAIN, include_str!("fixtures/keychain-stub.sh"));
    for name in names {
        std::fs::write(bin.join(format!("items/taskman-{name}")), "ATATT-mock").unwrap();
    }
    bin
}

/// config.json at `path` with an account for each token source.
fn config(path: &Path, tokens: &[&str]) {
    let accounts: Vec<_> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            serde_json::json!({"id": format!("jira-{}", i + 1), "provider": "jira", "token": t})
        })
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::json!({ "accounts": accounts }).to_string()).unwrap();
}

/// The state folder with project acme in `dir/repo` and ticket ENG-1 in a worktree taskman made,
/// `dir/repo-ENG-1`. Returns the state folder.
fn seed_state(dir: &Path) -> PathBuf {
    let (repo, worktree) = (dir.join("repo"), dir.join("repo-ENG-1"));
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(state.join("status")).unwrap();
    let saved = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo}],
        "tickets": [{"key": "ENG-1", "repo": repo, "cwd": worktree, "created_worktree": true}],
    });
    std::fs::write(state.join("tickets.json"), saved.to_string()).unwrap();
    std::fs::write(state.join("panics.log"), "").unwrap();
    state
}

/// A token file as the account form writes it: in a 0700 folder.
fn token_file(dir: &Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, "lin_api_mock\n").unwrap();
    path
}

fn cat(path: &Path) -> String {
    format!("cmd:cat '{}'", path.display())
}

/// taskman installed by the install script in a scratch home, with its data in the default
/// places, an account's token in the keychain and another in its token folder.
struct Installed {
    dir: PathBuf,
    program: PathBuf,
    config_dir: PathBuf,
    state: PathBuf,
    own_token: PathBuf,
    /// A file of the user's that an account's command reads.
    theirs: PathBuf,
    /// What links in the state folder lead to.
    outside: PathBuf,
    /// The stub keychain's folder.
    bin: PathBuf,
}

fn installed(name: &str) -> Installed {
    installed_in(scratch_dir(name))
}

/// A scratch folder reached through a link, as macOS's temporary folders are (/var is
/// /private/var), so Linux sees what macOS does: the program's path comes back with its links
/// resolved, and the home folder's doesn't.
fn behind_a_link(name: &str) -> PathBuf {
    let real = scratch_dir(name);
    let link = real.with_file_name(format!("{}-link", real.file_name().unwrap().to_str().unwrap()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    link
}

fn installed_in(dir: PathBuf) -> Installed {
    let home = dir.join("home");
    let bin = keychain(&dir, &["jira-1", "jira-9"]);
    let config_dir = home.join(".config/taskman");
    let own_token = token_file(&config_dir.join("tokens"), "jira-2");
    let theirs = dir.join("my-token");
    std::fs::write(&theirs, "ATATT-mine").unwrap();
    config(
        &config_dir.join("config.json"),
        &["keychain:jira-1", &cat(&own_token), &cat(&theirs), "env:JIRA_API_TOKEN"],
    );
    let state = seed_state(&dir);
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("notes"), "kept").unwrap();
    std::os::unix::fs::symlink(&outside, state.join("linked-folder")).unwrap();
    std::os::unix::fs::symlink(outside.join("notes"), state.join("linked-file")).unwrap();
    let program = home.join(".local/bin/taskman");
    copy_to(&program);
    Installed { dir, program, config_dir, state, own_token, theirs, outside, bin }
}

impl Installed {
    /// The install script's copy, run as `taskman` runs the build, with its config in its default
    /// place, ~/.config/taskman. It acts as a release, which only the release workflow builds.
    fn cmd(&self) -> Command {
        let mut cmd = run_as(&self.program, &self.dir);
        cmd.env_remove("TASKMAN_CONFIG").env("TASKMAN_TEST_RELEASE_BUILD", "1");
        cmd
    }

    /// Whether all of the data and tokens are still there.
    fn data_kept(&self) -> bool {
        self.config_dir.join("config.json").exists()
            && self.own_token.exists()
            && self.state.join("tickets.json").exists()
            && self.bin.join("items/taskman-jira-1").exists()
    }

    /// What never goes: an item no account names, what the links led to, the user's token file,
    /// the repository and the worktree, and the folders above taskman's.
    fn assert_theirs_kept(&self) {
        assert!(self.bin.join("items/taskman-jira-9").exists());
        assert_eq!(std::fs::read_to_string(self.outside.join("notes")).unwrap(), "kept");
        assert_eq!(std::fs::read_to_string(&self.theirs).unwrap(), "ATATT-mine");
        for kept in ["repo", "repo-ENG-1", "state", "home/.config", "home/.local/bin"] {
            assert!(self.dir.join(kept).is_dir(), "{kept} is gone");
        }
    }
}

#[test]
fn removes_the_program_and_keeps_the_data() {
    let t = installed("uninstall-program");
    let out = run(t.cmd(), &["--yes"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));

    assert!(
        stdout.starts_with("taskman uninstall removes the program: ~/.local/bin/taskman\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "Your data and tokens, kept for a reinstall unless you remove them too (--purge):\n"
        ),
        "{stdout}"
    );
    assert!(stdout.contains(": taskman jira-1\n"), "{stdout}");
    assert!(stdout.contains("\nRemoved ~/.local/bin/taskman\n"), "{stdout}");
    assert!(stdout.contains("taskman is uninstalled. Your data and tokens stay"), "{stdout}");

    assert!(gone(&t.program));
    assert!(t.data_kept(), "{stdout}");
    t.assert_theirs_kept();
    // The keychain wasn't even asked.
    assert!(!t.bin.join("calls").exists());
}

#[test]
fn purge_removes_taskmans_own_data_and_nothing_else() {
    let t = installed("uninstall-purge");
    let out = run(t.cmd(), &["--yes", "--purge"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));

    // Listed first, with what stays.
    assert!(stdout.contains("With --purge, it also removes your data and tokens:\n"), "{stdout}");
    assert!(
        stdout.contains(
            "  - your accounts and settings, and the tokens saved in files: ~/.config/taskman\n"
        ),
        "{stdout}"
    );
    let worktree = format!(
        "      git -C {} worktree remove {}\n",
        t.dir.join("repo").display(),
        t.dir.join("repo-ENG-1").display()
    );
    assert!(stdout.contains(&worktree), "{stdout}");
    assert!(stdout.contains("and the PATH line for ~/.local/bin\n"), "{stdout}");
    assert!(stdout.ends_with("taskman is uninstalled.\n"), "{stdout}");

    for path in [&t.program, &t.config_dir, &t.state, &t.bin.join("items/taskman-jira-1")] {
        assert!(gone(path), "{} is still there", path.display());
    }
    t.assert_theirs_kept();
    let calls = std::fs::read_to_string(t.bin.join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 1, "{calls}");
    assert!(calls.contains("jira-1") && !calls.contains("ATATT"), "{calls}");

    // Run again, there's nothing left to remove.
    let out = run(dev_build(&t.dir), &["--purge"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).ends_with("Nothing of taskman's is left to remove.\n"));
}

/// With `TASKMAN_CONFIG`, the file is in a folder of the user's: only it goes, and in the token
/// folder beside it only the files its accounts read. A keychain that fails is reported with the
/// command that does it by hand, and the rest still goes. A development build stays, whatever
/// its folder is called.
#[test]
fn purge_removes_only_its_files_beside_a_config_file() {
    let dir = scratch_dir("uninstall-config-file");
    let bin = keychain(&dir, &["jira-1"]);
    std::fs::write(bin.join("fail"), "Cannot autolaunch D-Bus without X11 $DISPLAY").unwrap();
    let token = token_file(&dir.join("tokens"), "jira-2");
    std::fs::write(dir.join("tokens/notes"), "kept").unwrap();
    config(&dir.join("config.json"), &["keychain:jira-1", &cat(&token)]);
    std::fs::write(dir.join("other.json"), "kept").unwrap();
    let state = seed_state(&dir);

    // Without --purge, there's nothing to remove: the program stays.
    let out = run(dev_build(&dir), &["--yes"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(stdout.contains("  - the program, ") && stdout.contains("a development build"));
    assert!(stdout.ends_with("Nothing was removed.\n"), "{stdout}");
    assert!(dir.join("config.json").exists() && token.exists() && state.exists());

    let out = run(dev_build(&dir), &["--yes", "--purge"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.contains("  - a token saved in a file: "), "{stdout}");
    let by_hand = if cfg!(target_os = "macos") {
        "security delete-generic-password -s taskman -a jira-1"
    } else {
        "secret-tool clear service taskman account jira-1"
    };
    assert!(stderr.contains("D-Bus") && stderr.contains(by_hand), "{stderr}");
    assert!(stderr.contains("the rest is removed"), "{stderr}");

    for path in [&dir.join("config.json"), &token, &state] {
        assert!(gone(path), "{} is still there", path.display());
    }
    assert_eq!(std::fs::read_to_string(dir.join("tokens/notes")).unwrap(), "kept");
    assert_eq!(std::fs::read_to_string(dir.join("other.json")).unwrap(), "kept");
    assert!(bin.join("items/taskman-jira-1").exists());
    assert!(dir.join("build/debug/taskman").exists());
}

#[test]
fn refuses_while_taskman_runs() {
    let dir = scratch_dir("uninstall-running");
    let state = seed_state(&dir);
    config(&dir.join("config.json"), &[]);
    // What the running TUI's heartbeat writes every second.
    std::fs::create_dir_all(state.join("inbox")).unwrap();
    std::fs::write(state.join("inbox/alive"), r#"{"version":"0.1.22","inbox":4}"#).unwrap();
    for args in [&["--yes"][..], &["--yes", "--purge"]] {
        let out = run(dev_build(&dir), args);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            text(&out.stderr),
            "taskman uninstall: taskman is running: quit it (ctrl+a q, then Enter) so its sessions stop, then run taskman uninstall again\n"
        );
        assert!(out.stdout.is_empty());
    }
    assert!(state.join("tickets.json").exists() && dir.join("config.json").exists());
}

/// A link where taskman's own folder or file should be leads somewhere taskman can't call its
/// own: --purge refuses before it reads or removes anything, and without it the data stays and
/// says why.
#[test]
fn refuses_to_purge_through_a_link_for_one_of_its_own_paths() {
    let dir = scratch_dir("uninstall-link");
    config(&dir.join("config.json"), &[]);
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("tickets.json"), "{}").unwrap();
    std::fs::create_dir_all(dir.join("state")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join("state/taskman")).unwrap();
    let why = format!(
        "{} is a link to {}, and taskman removes only its own files and folders",
        dir.join("state/taskman").display(),
        elsewhere.display()
    );

    let out = run(dev_build(&dir), &["--yes", "--purge"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with(&format!("taskman uninstall: {why}")), "{stderr}");
    assert!(out.stdout.is_empty());

    let out = run(dev_build(&dir), &["--yes"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(stdout.contains(&format!("Your data and tokens stay: {why}")), "{stdout}");

    assert!(std::fs::symlink_metadata(dir.join("state/taskman")).unwrap().is_symlink());
    assert!(elsewhere.join("tickets.json").exists() && dir.join("config.json").exists());
}

#[test]
fn needs_a_terminal_to_ask_in() {
    let dir = scratch_dir("uninstall-no-terminal");
    let state = seed_state(&dir);
    config(&dir.join("config.json"), &[]);
    for args in [&[][..], &["--purge"]] {
        let out = run(dev_build(&dir), args);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            text(&out.stderr),
            "taskman uninstall: there's no terminal to ask in: pass --yes to remove the program, or --yes --purge to remove your data and tokens too\n"
        );
        assert!(state.join("tickets.json").exists());
    }
}

/// In a terminal, the program's question, then the data's, each no unless answered yes; no to
/// the first removes nothing.
#[test]
fn asks_about_the_program_then_the_data() {
    for (name, answers, program_kept, data_kept) in [
        ("uninstall-ask-no", &["n"][..], true, true),
        ("uninstall-ask-program", &["y", ""], false, true),
        ("uninstall-ask-all", &["y", "yes"], false, false),
    ] {
        let t = installed(name);
        let mut cmd = t.cmd();
        cmd.arg("uninstall");
        let mut tui = Tui::start("asks_about_the_program_then_the_data", &t.dir, cmd, LIMIT);
        tui.wait_text("Remove taskman (the program)? [y/N]");
        tui.send(format!("{}\r", answers[0]).as_bytes());
        if let Some(second) = answers.get(1) {
            tui.wait_text("Also remove your projects, accounts, saved state and tokens? They'd");
            tui.send(format!("{second}\r").as_bytes());
            tui.wait_text("taskman is uninstalled.");
            assert!(tui.wait_exit().success(), "{name}");
        } else {
            tui.wait_text("Nothing was removed.");
            assert_eq!(tui.wait_exit().code(), Some(1), "{name}");
        }
        assert_eq!(t.program.exists(), program_kept, "{name}");
        assert_eq!(t.data_kept(), data_kept, "{name}");
        if !data_kept {
            assert!(gone(&t.config_dir) && gone(&t.state), "{name}");
        }
        t.assert_theirs_kept();
    }
}

/// $HOME reached through a link: the program is still the install script's copy, shown under
/// `~`, and the data is still taskman's own, removed with --purge.
#[test]
fn works_with_home_behind_a_link() {
    let t = installed_in(behind_a_link("uninstall-linked-home"));
    let out = run(t.cmd(), &["--yes"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(
        stdout.starts_with("taskman uninstall removes the program: ~/.local/bin/taskman\n"),
        "{stdout}"
    );
    assert!(stdout.contains("and the PATH line for ~/.local/bin\n"), "{stdout}");
    assert!(stdout.contains("\nRemoved ~/.local/bin/taskman\n"), "{stdout}");
    assert!(gone(&t.program));
    assert!(t.data_kept(), "{stdout}");

    // Then the data, with the build, as `taskman uninstall --purge` before the program went would.
    let mut cmd = dev_build(&t.dir);
    cmd.env_remove("TASKMAN_CONFIG");
    let out = run(cmd, &["--yes", "--purge"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(
        stdout.contains(
            "  - your accounts and settings, and the tokens saved in files: ~/.config/taskman\n"
        ),
        "{stdout}"
    );
    for path in [&t.config_dir, &t.state, &t.bin.join("items/taskman-jira-1")] {
        assert!(gone(path), "{} is still there", path.display());
    }
    t.assert_theirs_kept();
}

/// A token file is taskman's when it's in its token folder, however the folder's path was spelled
/// when the token was saved; one elsewhere stays, even with a name like one.
#[test]
fn purge_finds_its_token_files_however_their_folder_was_spelled() {
    let dir = behind_a_link("uninstall-token-spelling");
    let real = dir.canonicalize().unwrap();
    let token = token_file(&dir.join("tokens"), "jira-1");
    let elsewhere = token_file(&dir.join("elsewhere"), "jira-2");
    config(
        &dir.join("config.json"),
        &[&cat(&real.join("tokens/jira-1")), &cat(&real.join("elsewhere/jira-2"))],
    );
    let out = run(dev_build(&dir), &["--yes", "--purge"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(gone(&token) && gone(&dir.join("config.json")), "{stdout}");
    assert!(elsewhere.exists(), "{stdout}");
    // The folder's empty now, so it goes too.
    assert!(gone(&dir.join("tokens")), "{stdout}");
}

/// An empty XDG variable is unset, as the XDG spec says: the folders are the defaults under the
/// home folder, never a `taskman` in the folder it's run from, such as a checkout of taskman.
#[test]
fn an_empty_xdg_state_home_never_names_a_folder_in_the_current_directory() {
    let dir = scratch_dir("uninstall-empty-xdg");
    let cwd = dir.join("work");
    let checkout = cwd.join("taskman");
    std::fs::create_dir_all(checkout.join("src")).unwrap();
    std::fs::write(checkout.join("src/main.rs"), "fn main() {}\n").unwrap();
    let state = dir.join("home/.local/state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("tickets.json"), "{}").unwrap();
    let mut cmd = dev_build(&dir);
    cmd.env("XDG_STATE_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env_remove("TASKMAN_CONFIG")
        .current_dir(&cwd);
    let out = run(cmd, &["--purge", "--yes"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(
        checkout.join("src/main.rs").exists(),
        "uninstall --purge removed ./taskman:\n{stdout}"
    );
    assert!(
        stdout.contains("  - your projects, tickets and session state: ~/.local/state/taskman\n"),
        "{stdout}"
    );
    assert!(gone(&state), "{stdout}");
}

/// A relative `TASKMAN_CONFIG` names a different file in each folder: --purge refuses it rather
/// than remove whatever file of that name is where it's run.
#[test]
fn purge_refuses_a_relative_config_file() {
    let dir = scratch_dir("uninstall-relative-config");
    let cwd = dir.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join("config.json"), r#"{"theirs": true}"#).unwrap();
    let state = seed_state(&dir);
    let cmd = || {
        let mut cmd = dev_build(&dir);
        cmd.env("TASKMAN_CONFIG", "config.json").current_dir(&cwd);
        cmd
    };
    let why =
        "config.json isn't a full path, so what it names depends on the folder you run taskman in";

    let out = run(cmd(), &["--purge", "--yes"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with(&format!("taskman uninstall: {why}")), "{stderr}");
    assert!(out.stdout.is_empty());

    let out = run(cmd(), &["--yes"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(stdout.contains(&format!("Your data and tokens stay: {why}")), "{stdout}");

    assert_eq!(std::fs::read_to_string(cwd.join("config.json")).unwrap(), r#"{"theirs": true}"#);
    assert!(state.join("tickets.json").exists());
}
