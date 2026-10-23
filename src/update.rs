//! Updates: `taskman update`, and the TUI's background updates (`Updater`).
//!
//! Both download the release's build for this platform over HTTPS from the repository's release
//! pages, falling back to `gh` (which can read a private copy), check it against the release's
//! SHA256SUMS and, when gh can, its attestation, check the new binary runs, then rename it over the
//! running binary's file. The running TUI keeps its own copy of the old binary; the new one applies
//! when taskman restarts. The TUI installs only a release with the same minor version (a new minor
//! is a breaking one) whose attestation gh verified; anything else it only announces.

use crate::cli::{Arg, Lexer};
use crate::{net, proc, store, util};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

/// The repository, from Cargo.toml's `repository`: renaming it there renames it here.
const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
/// How often a running TUI looks for a new release, in seconds. It also looks at every start.
const CHECK_EVERY: i64 = 60 * 60;
/// A check is one request: offline or stuck, give up soon and stay quiet.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// gh asks GitHub for the attestation and Sigstore for its trust root: a few requests.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(60);
/// The workflow that builds releases, in the repository: the only signer an attestation may have.
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";
/// The refs it may have run from, as a regular expression: a version tag, or main, where
/// release.sh starts it by hand for a tag. install.sh's `signer_refs` is the same.
const SIGNER_REFS: &str = r"refs/(tags/v[0-9]+\.[0-9]+\.[0-9]+|heads/main)";
/// The first release that must have an attestation: one without is refused rather than installed
/// on its checksum, as a replaced archive would have none. v0.1.0 is the first built on the public
/// repository; older ones have none and still fall back. install.sh's `require_from` is the same
/// switch.
const REQUIRE_ATTESTATION_FROM: Option<&str> = Some("v0.1.0");
/// Set to anything but 0, installs don't verify the attestation: for a network that blocks gh.
const SKIP_ATTESTATION: &str = "TASKMAN_INSECURE_SKIP_ATTESTATION";
/// A release archive is a few MB; anything this big isn't one.
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;
/// How often the TUI looks at update.json for a `taskman update` run elsewhere.
const RELOAD_EVERY: Duration = Duration::from_secs(5);

/// The C library this build links: on Linux, glibc ("gnu") or musl.
const LIBC: &str = if cfg!(target_env = "musl") {
    "musl"
} else if cfg!(target_env = "gnu") {
    "gnu"
} else {
    ""
};

/// The release build for this platform, as named in the release's archives
/// (`taskman-v1.2.3-<target>.tar.gz`). None where there's no prebuilt taskman.
fn target() -> Option<&'static str> {
    target_for(std::env::consts::OS, std::env::consts::ARCH, LIBC)
}

/// The release build for `os` and `arch` (as `std::env::consts` names them) with `libc`. A copy
/// updates to the build it is, glibc or musl: install.sh chose it for the machine, and the glibc
/// one already runs there.
fn target_for(os: &str, arch: &str, libc: &str) -> Option<&'static str> {
    Some(match (os, arch, libc) {
        ("macos", "aarch64", _) => "aarch64-apple-darwin",
        ("macos", "x86_64", _) => "x86_64-apple-darwin",
        ("linux", "x86_64", "gnu") => "x86_64-unknown-linux-gnu",
        ("linux", "x86_64", "musl") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64", "gnu") => "aarch64-unknown-linux-gnu",
        ("linux", "aarch64", "musl") => "aarch64-unknown-linux-musl",
        _ => return None,
    })
}

/// This build's version as a release tag.
pub fn running() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

const USAGE: &str = "Usage: taskman update [--force]

Installs the latest release over this taskman, after checking it against the release's SHA256SUMS
and, when gh is installed and logged in, its attestation. A running taskman keeps its version until
you restart it. A copy installed with Homebrew or cargo, or built from source, is updated the way
it was installed.

  --force  install the latest release even when it's the one running, or over a copy cargo
           installed";

pub fn run(args: &[String]) -> i32 {
    crate::cli::run("update", USAGE, args, |args| {
        update(parse_args(args)?)?;
        Ok(0)
    })
}

/// Whether to `--force` the update.
fn parse_args(args: &[String]) -> Result<bool, String> {
    let mut force = false;
    let mut lex = Lexer::new(args, USAGE);
    while let Some(arg) = lex.next()? {
        match arg {
            Arg::Option("--force") => force = true,
            Arg::Option(_) => return Err(lex.unknown()),
            Arg::Word(w) => return Err(format!("unknown argument {w}\n\n{USAGE}")),
        }
    }
    Ok(force)
}

fn update(force: bool) -> Result<(), String> {
    let current = running();
    let exe = std::env::current_exe()
        .map_err(|e| format!("can't tell where taskman is installed: {e}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    match installs(&exe, &cargo_bin(), released(), true) {
        Installs::Homebrew => {
            return Err(
                "taskman was installed with Homebrew: update it with `brew upgrade taskman`."
                    .into(),
            );
        }
        // Replacing cargo's copy behind its back leaves cargo with the wrong version on record.
        Installs::Cargo if !force => {
            return Err(format!(
                "taskman was installed with cargo: update it with `cargo install --locked --git {REPO_URL}`. To replace cargo's copy with the release anyway, run taskman update --force."
            ));
        }
        // A development build updates from its source with git, and overwriting one in a
        // checkout would confuse cargo.
        Installs::DevBuild => {
            return Err(format!(
                "this is a development build ({}). Update the source with `git pull` and rebuild, or install a release with the README's quick install.",
                exe.display()
            ));
        }
        _ => {}
    }
    let bin_dir = exe.parent().ok_or("can't tell which folder taskman is installed in")?;

    let latest = latest_release(Duration::from_secs(60))?;
    let mut state = load();
    state.latest = Some(latest.clone());
    state.checked_at = util::now();
    if !force {
        match (parse(&current), parse(&latest)) {
            (Some(c), Some(l)) if c == l => {
                save(&state);
                crate::cli::out!(
                    "taskman {current} is the latest release; nothing to update (taskman update --force reinstalls it)."
                );
                return Ok(());
            }
            (Some(c), Some(l)) if c > l => {
                save(&state);
                crate::cli::out!(
                    "taskman {current} is newer than the latest release, {latest}; nothing to update."
                );
                return Ok(());
            }
            _ => {}
        }
    }
    crate::cli::out!("Updating taskman {current} → {latest} in {}", bin_dir.display());
    if newer(&latest, &current) && !same_minor(&latest, &current) {
        crate::cli::out!(
            "{latest} has breaking changes: see its notes, {REPO_URL}/releases/tag/{latest}"
        );
    }
    install(&exe, &latest, By::Command)?;
    // A running TUI reads this and says the update applies when it restarts.
    state.installed = Some(Installed { version: latest.clone(), dir: bin_dir.to_path_buf() });
    save(&state);
    crate::cli::out!(
        "Restart taskman to use {latest}: quit with {} q, then run taskman again.",
        crate::keys::prefix()
    );
    Ok(())
}

/// `v1.2.3` → (1, 2, 3).
pub fn parse(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Whether release `a` is newer than `b`; false when either isn't a version.
fn newer(a: &str, b: &str) -> bool {
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

/// Whether releases `a` and `b` share their major and minor numbers: before 1.0 a new minor is a
/// breaking release (CHANGELOG.md), which the TUI announces rather than installs.
fn same_minor(a: &str, b: &str) -> bool {
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if (a.0, a.1) == (b.0, b.1))
}

/// `owner/name`, for gh.
fn repo() -> &'static str {
    REPO_URL.trim_start_matches("https://github.com/").trim_end_matches('/')
}

/// The release pages: GitHub's, or TASKMAN_RELEASES_URL (a local copy, for testing).
fn releases_url() -> String {
    let url = std::env::var("TASKMAN_RELEASES_URL").ok().filter(|u| !u.is_empty());
    url.unwrap_or_else(|| format!("{REPO_URL}/releases")).trim_end_matches('/').to_string()
}

/// How a release was downloaded. HTTPS needs nothing installed; gh can also read a private
/// repository.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Via {
    Https,
    Gh,
}

enum GhError {
    Missing,
    Failed(String),
}

impl GhError {
    fn text(self) -> String {
        match self {
            GhError::Missing => "the GitHub CLI (gh) isn't installed".into(),
            GhError::Failed(e) => e,
        }
    }
}

fn gh(args: &[&str], timeout: Duration) -> Result<Vec<u8>, GhError> {
    match stdout_of(proc::run(Command::new("gh").args(args), timeout)) {
        Err(_) => Err(GhError::Missing),
        Ok(out) => out.map_err(GhError::Failed),
    }
}

/// HTTPS's result, or gh's when HTTPS failed. The error is HTTPS's, with gh's when it's installed:
/// without gh, it isn't news that gh didn't work.
fn either<T>(
    https: Result<T, String>,
    gh: impl FnOnce() -> Result<T, GhError>,
) -> Result<(T, Via), String> {
    let https = match https {
        Ok(v) => return Ok((v, Via::Https)),
        Err(e) => e,
    };
    match gh() {
        Ok(v) => Ok((v, Via::Gh)),
        Err(GhError::Missing) => Err(https),
        Err(GhError::Failed(e)) => Err(format!("{https}; through gh: {e}")),
    }
}

/// An agent for one request and its redirects. It keeps no connections: GitHub redirects downloads
/// to another host anyway, and ureq would reuse one an HTTP/1.0 server is about to close.
fn agent(timeout: Duration, redirects: u32) -> ureq::Agent {
    let config = net::config(timeout).max_redirects(redirects);
    config.max_idle_connections(0).build().into()
}

fn http_error(url: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(404) => format!("{url} wasn't found"),
        ureq::Error::StatusCode(code) => format!("{url} answered HTTP {code}"),
        ureq::Error::BodyExceedsLimit(_) => format!("{url} is too big to be a release"),
        e => format!("couldn't reach {url}: {e}"),
    }
}

/// The tag in GitHub's redirect from `releases/latest` (`…/releases/tag/v1.2.3`).
fn tag_from_location(location: &str) -> Option<String> {
    let tag = location.rsplit_once("/tag/")?.1.split(['?', '#']).next()?;
    parse(tag).is_some().then(|| tag.to_string())
}

fn latest_https(timeout: Duration) -> Result<String, String> {
    let url = format!("{}/latest", releases_url());
    let resp = net::retry_dropped(|| agent(timeout, 0).get(&url).call())
        .map_err(|e| http_error(&url, e))?;
    let location = resp.headers().get("location").and_then(|l| l.to_str().ok());
    location.and_then(tag_from_location).ok_or_else(|| format!("{url} doesn't lead to a release"))
}

fn latest_release(timeout: Duration) -> Result<String, String> {
    let gh = || {
        let tag = gh(
            &["release", "view", "-R", repo(), "--json", "tagName", "--jq", ".tagName"],
            timeout,
        )?;
        Ok(String::from_utf8_lossy(&tag).trim().to_string())
    };
    let (tag, _) = either(latest_https(timeout), gh)
        .map_err(|e| format!("couldn't find the latest release: {e}"))?;
    if parse(&tag).is_none() {
        return Err(format!("the latest release, {tag}, isn't a version"));
    }
    Ok(tag)
}

fn download_https(tag: &str, name: &str) -> Result<Vec<u8>, String> {
    let url = format!("{}/download/{tag}/{name}", releases_url());
    let get = || {
        agent(INSTALL_TIMEOUT, 10)
            .get(&url)
            .call()?
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD)
            .read_to_vec()
    };
    net::retry_dropped(get).map_err(|e| http_error(&url, e))
}

fn download_gh(tag: &str, name: &str) -> Result<Vec<u8>, GhError> {
    gh(&["release", "download", tag, "-R", repo(), "-p", name, "-O", "-"], INSTALL_TIMEOUT)
}

fn download(via: Via, tag: &str, name: &str) -> Result<Vec<u8>, String> {
    match via {
        Via::Https => download_https(tag, name),
        Via::Gh => download_gh(tag, name).map_err(GhError::text),
    }
}

/// `name`'s SHA-256 in a checksum file (`sha256sum`'s format: hash, spaces, file name).
fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        let hex = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
        (file == name && hex).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// The release's checksum for `archive`, from its SHA256SUMS or, as builds before it had, from
/// `<archive>.sha256`. Without one nothing is installed: a checksum that can be skipped checks
/// nothing.
fn checksum(via: Via, tag: &str, archive: &str) -> Result<String, String> {
    let sums = download(via, tag, "SHA256SUMS").or_else(|sums| {
        download(via, tag, &format!("{archive}.sha256")).map_err(|e| {
            format!("couldn't get release {tag}'s checksums ({sums}; {e}); nothing was installed")
        })
    })?;
    expected_sum(&String::from_utf8_lossy(&sums), archive).ok_or_else(|| {
        format!("release {tag}'s checksums don't list {archive}; nothing was installed")
    })
}

/// Who installs a release.
#[derive(Clone, Copy, Debug, PartialEq)]
enum By {
    /// `taskman update`: says what it checked, and installs on the checksum alone when gh can't
    /// check the attestation.
    Command,
    /// The TUI, in the background: prints nothing (the TUI owns the terminal), and installs only
    /// what gh verified.
    Background,
}

/// Whether an install by `by` goes on after the attestation check, or why not.
fn accepts(by: By, attestation: &Attestation) -> Result<(), String> {
    match (by, attestation) {
        (_, Attestation::Verified) | (By::Command, _) => Ok(()),
        (By::Background, Attestation::Unchecked(why)) => Err(why.clone()),
        (By::Background, Attestation::Skipped) => {
            Err(format!("{SKIP_ATTESTATION} is set, so its attestation isn't verified"))
        }
    }
}

/// Why the background can't install without downloading the archive first: what `accepts` would
/// refuse anyway.
fn unverifiable() -> Option<Attestation> {
    if skip_attestation() {
        return Some(Attestation::Skipped);
    }
    let gh = proc::run(Command::new("gh").arg("--version"), CHECK_TIMEOUT);
    gh.is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        .then(|| Attestation::Unchecked(NO_GH.into()))
}

/// Installs release `tag` over the binary at `exe` (the running one, links resolved): downloads
/// this platform's build, checks it against the release's checksum and its attestation, and checks
/// the new binary runs and is that release before it replaces the old one.
fn install(exe: &Path, tag: &str, by: By) -> Result<(), String> {
    let target = target().ok_or(
        "there's no prebuilt taskman for this platform; build it from source (see the README)",
    )?;
    let archive = format!("taskman-{tag}-{target}.tar.gz");
    let say = |line: String| {
        if by == By::Command {
            crate::cli::out!("{line}");
        }
    };
    if by == By::Background
        && let Some(a) = unverifiable()
    {
        accepts(by, &a)?;
    }
    let (data, via) = either(download_https(tag, &archive), || download_gh(tag, &archive))
        .map_err(|e| format!("couldn't download {archive}: {e}"))?;
    if sha256_hex(&data) != checksum(via, tag, &archive)? {
        return Err(format!("{archive} doesn't match its checksum; nothing was installed"));
    }
    let through = if via == Via::Gh { " through gh" } else { "" };
    say(format!("Downloaded {archive}{through}; its checksum matches."));
    let attestation = if skip_attestation() {
        Attestation::Skipped
    } else {
        verify(&data, &archive, tag).map_err(|r| r.text(&archive))?
    };
    accepts(by, &attestation)?;
    match attestation {
        Attestation::Verified => {
            say(format!(
                "Verified its attestation with gh: {}'s release workflow built it.",
                repo()
            ));
        }
        Attestation::Unchecked(why) => say(format!("Checked against its checksum alone: {why}.")),
        Attestation::Skipped => say(format!(
            "Warning: {SKIP_ATTESTATION} is set, so its attestation wasn't verified: only its checksum was."
        )),
    }
    // The archive holds just the binary.
    let binary = stdout_of(proc::run_with_input(
        Command::new("tar").args(["-xzOf", "-", "taskman"]),
        data,
        CHECK_TIMEOUT,
    ))
    .map_err(|e| format!("couldn't run tar: {e}"))?
    .map_err(|e| format!("couldn't unpack {archive}: {e}"))?;
    // A running taskman may be using the old binary, and its sessions run it for their hooks.
    // Write the new one beside it and rename it over, so the switch is atomic and never edits the
    // file a process has open. Under the binary's own name, whatever that is: a copy installed as
    // `tm` would otherwise get a stray `taskman` beside it and never see the update.
    let (dir, name) = match (exe.parent(), exe.file_name()) {
        (Some(dir), Some(name)) => (dir, name.to_string_lossy()),
        _ => return Err(format!("can't tell where to install over {}", exe.display())),
    };
    let staged = dir.join(format!(".{name}.new.{}", std::process::id()));
    let _leftover = proc::Leftover::new(&staged);
    let result = stage(&staged, &binary, tag).and_then(|version| {
        std::fs::rename(&staged, exe)
            .map_err(|e| format!("couldn't replace {}: {e}", exe.display()))?;
        Ok(version)
    });
    let _ = std::fs::remove_file(&staged);
    say(format!("Installed: {} ({})", result?, exe.display()));
    Ok(())
}

/// What gh made of a release archive's attestation, when it didn't refuse it.
#[derive(Debug, PartialEq)]
enum Attestation {
    /// Signed by a workflow in the repository: GitHub built it, not only uploaded it.
    Verified,
    /// Nothing to verify with, or nothing to verify; the checksum stands alone. Why, as a clause.
    Unchecked(String),
    /// Not checked, as TASKMAN_INSECURE_SKIP_ATTESTATION asks.
    Skipped,
}

const NO_GH: &str = "the GitHub CLI (gh) isn't installed to verify its attestation";

fn skip_attestation() -> bool {
    std::env::var(SKIP_ATTESTATION).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Why an archive was refused at its attestation.
#[derive(Debug, PartialEq)]
struct Refusal {
    why: String,
    /// It may be the network, not the archive: worth naming the way around the check.
    network: bool,
}

impl Refusal {
    fn text(self, archive: &str) -> String {
        let mut text = format!(
            "gh couldn't verify {archive}'s attestation ({}); nothing was installed",
            self.why
        );
        if self.network {
            text.push_str(&format!(
                ". Try again later; if your network blocks gh, {SKIP_ATTESTATION}=1 installs on the checksum alone"
            ));
        }
        text
    }
}

/// Checks the downloaded archive's attestation with `gh attestation verify`, which needs it in a
/// file. An error is a verification that failed, and refuses the install.
fn verify(data: &[u8], archive: &str, tag: &str) -> Result<Attestation, Refusal> {
    let copy = Private::write(&std::env::temp_dir(), archive, data).map_err(|e| Refusal {
        why: format!("couldn't write {archive} to a temporary folder: {e}"),
        network: false,
    })?;
    let out = proc::run(&mut attest_command(&copy.file), VERIFY_TIMEOUT);
    drop(copy);
    attestation(out, tag, REQUIRE_ATTESTATION_FROM)
}

/// A file in a new folder only this user can open, both removed when it's dropped: another user
/// can't swap the archive between the check and gh reading it, or plant a link at a name it
/// could guess.
struct Private {
    dir: PathBuf,
    file: PathBuf,
    _leftover: proc::Leftover,
}

impl Private {
    fn write(parent: &Path, name: &str, data: &[u8]) -> std::io::Result<Self> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
        let mut tries = 0;
        let dir = loop {
            let dir = parent.join(format!(
                ".taskman-{}-{}-{tries}",
                std::process::id(),
                nanos.as_nanos()
            ));
            // Not recursive, so a folder (or link) already there is an error, never reused.
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 10 => tries += 1,
                Err(e) => return Err(e),
                Ok(()) => break dir,
            }
        };
        let copy = Self { file: dir.join(name), _leftover: proc::Leftover::new(&dir), dir };
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&copy.file)?;
        f.write_all(data)?;
        Ok(copy)
    }
}

impl Drop for Private {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn attest_command(path: &Path) -> Command {
    let mut cmd = Command::new("gh");
    cmd.args(["attestation", "verify"]).arg(path).args(["--repo", repo()]);
    cmd.arg("--cert-identity-regex").arg(signer_identity());
    cmd.arg("--deny-self-hosted-runners");
    cmd
}

/// What an attestation's certificate must name as its signer: the release workflow, run from a
/// version tag or main. `--signer-workflow`, which gh won't take with this, accepts any ref, so a
/// workflow run on any branch could sign.
fn signer_identity() -> String {
    let workflow = format!("https://github.com/{}/{RELEASE_WORKFLOW}", repo());
    let escaped: String = workflow
        .chars()
        .flat_map(|c| {
            let special = r"\.+*?()|[]{}^$".contains(c);
            special.then_some('\\').into_iter().chain([c])
        })
        .collect();
    format!("^{escaped}@{SIGNER_REFS}$")
}

/// Reads gh's answer. Without gh, logged out, with a gh too old to have the command, or for a
/// release with no attestation from before `require_from`, the checksum stands alone. Anything
/// else gh says, timing out included, is a failed verification: an unknown error never lets an
/// archive through unverified.
fn attestation(
    out: std::io::Result<Output>,
    tag: &str,
    require_from: Option<&str>,
) -> Result<Attestation, Refusal> {
    let unchecked = |why: String| Ok(Attestation::Unchecked(why));
    let out = match out {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return unchecked(NO_GH.into());
        }
        Err(e) => {
            let network = e.kind() == std::io::ErrorKind::TimedOut;
            return Err(Refusal { why: e.to_string(), network });
        }
        Ok(out) => out,
    };
    if out.status.success() {
        return Ok(Attestation::Verified);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    // gh exits with 4 when it needs a login; a token it has that GitHub refuses is the same.
    if out.status.code() == Some(4) || err.contains("HTTP 401") {
        unchecked("gh isn't logged in (gh auth login) to verify its attestation".into())
    } else if err.contains("unknown command \"attestation\"") {
        unchecked("this gh has no attestation command; update gh to verify it".into())
    // A public repository with none for this archive, and a private one (no attestations at all).
    } else if err.contains("no attestations found") || err.contains("HTTP 404") {
        match require_from {
            Some(from) if !newer(from, tag) => Err(Refusal {
                why: format!("release {tag} has none, though every release from {from} on has one"),
                network: false,
            }),
            _ => unchecked(format!("release {tag} has no attestation to verify")),
        }
    } else {
        let last = err.lines().rev().map(str::trim).find(|l| !l.is_empty());
        let why = last.unwrap_or("it failed without saying why").to_string();
        Err(Refusal { why, network: network_trouble(&err) })
    }
}

/// Whether gh's error could be the network or GitHub's limits rather than the archive. gh's
/// verdicts on the archive (its signature, signer or digest) come first: they name no way around.
fn network_trouble(err: &str) -> bool {
    const VERDICTS: &[&str] =
        &["Verification failed", "verifying with issuer", "certificate identity", "SAN", "digest"];
    const NETWORK: &[&str] = &[
        "HTTP 429",
        "rate limit",
        "dial tcp",
        "no such host",
        "connection refused",
        "connection reset",
        "i/o timeout",
        "TLS handshake",
        "x509:",
        "deadline exceeded",
        "proxyconnect",
        "network is unreachable",
    ];
    let server_error = err.match_indices("HTTP 5").any(|(i, _)| {
        let code = &err.as_bytes()[i + 6..];
        code.len() >= 2 && code[..2].iter().all(u8::is_ascii_digit)
    });
    !VERDICTS.iter().any(|v| err.contains(v))
        && (server_error || NETWORK.iter().any(|n| err.contains(n)))
}

/// Writes the new binary to `staged`, and checks it runs and says it's release `tag`.
fn stage(staged: &Path, binary: &[u8], tag: &str) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;
    let written = std::fs::write(staged, binary)
        .and_then(|()| std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755)));
    written.map_err(|e| format!("couldn't write {}: {e}", staged.display()))?;
    let broken = |e: String| {
        format!("the new binary doesn't run on this machine ({e}); nothing was installed")
    };
    let mut tries = 0;
    let out = loop {
        match stdout_of(proc::run(Command::new(staged).arg("--version"), CHECK_TIMEOUT)) {
            // Another thread starting a process at the moment the file was written can hold it
            // open for a moment.
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(broken(e.to_string())),
            Ok(out) => break out.map_err(broken)?,
        }
    };
    let version = String::from_utf8_lossy(&out).trim().to_string();
    let want = format!("taskman {}", tag.trim_start_matches('v'));
    if version != want {
        return Err(format!(
            "the new binary says it's {version:?}, not {want}; nothing was installed"
        ));
    }
    Ok(version)
}

/// `proc::run`'s result as the updater uses it: the outer error is failing to start the program;
/// the inner one is it failing or running out of time, with the last line it printed to stderr.
fn stdout_of(out: std::io::Result<Output>) -> std::io::Result<Result<Vec<u8>, String>> {
    let out = match out {
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Ok(Err(e.to_string())),
        out => out?,
    };
    if out.status.success() {
        return Ok(Ok(out.stdout));
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    Ok(Err(if last.is_empty() { "it failed without saying why".into() } else { last.to_string() }))
}

// ---- background updates ------------------------------------------------------------------

/// What the last checks found, saved in update.json: the notice and what was installed outlast a
/// restart, and a running TUI picks up `taskman update` runs.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct State {
    /// When a check last succeeded (Unix seconds).
    #[serde(default)]
    pub checked_at: i64,
    #[serde(default)]
    pub latest: Option<String>,
    /// The last release installed, by the TUI or `taskman update`; it applies at the next start.
    #[serde(default)]
    pub installed: Option<Installed>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Installed {
    pub version: String,
    pub dir: PathBuf,
}

fn state_path() -> PathBuf {
    store::state_dir().join("update.json")
}

fn load() -> State {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(s: &State) {
    if let Ok(bytes) = serde_json::to_vec_pretty(s) {
        let _ = store::write_atomic(&state_path(), &bytes);
    }
}

fn modified() -> Option<SystemTime> {
    std::fs::metadata(state_path()).and_then(|m| m.modified()).ok()
}

fn due(s: &State, now: i64) -> bool {
    now - s.checked_at >= CHECK_EVERY || now < s.checked_at
}

/// Whether this copy of taskman may replace itself, or only say a release is out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Installs {
    Yes,
    /// Built from source rather than by the release workflow: it updates with git.
    DevBuild,
    /// In cargo's bin folder, which cargo manages.
    Cargo,
    /// In Homebrew's Cellar: `brew upgrade` updates it.
    Homebrew,
    /// In a folder taskman can't write to.
    ReadOnly,
}

/// Whether the release workflow built this binary: it sets TASKMAN_RELEASE_BUILD for the build.
/// Any other build is a development build, wherever it's put, as a `CARGO_TARGET_DIR` can be any
/// folder.
const RELEASE_BUILD: bool = option_env!("TASKMAN_RELEASE_BUILD").is_some();

/// A release build, or a copy of a development build that tests run as one
/// (TASKMAN_TEST_RELEASE_BUILD): they set it only on a copy, never the binary under test.
pub(crate) fn released() -> bool {
    RELEASE_BUILD || std::env::var_os("TASKMAN_TEST_RELEASE_BUILD").is_some_and(|v| !v.is_empty())
}

fn is_homebrew(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "Cellar")
}

/// How the binary at `exe` was installed. Homebrew and cargo build from source, so their folders
/// come before how it was built.
pub(crate) fn installs(exe: &Path, cargo_bin: &Path, released: bool, writable: bool) -> Installs {
    if is_homebrew(exe) {
        Installs::Homebrew
    } else if exe.parent() == Some(cargo_bin) {
        Installs::Cargo
    } else if !released {
        Installs::DevBuild
    } else if !writable {
        Installs::ReadOnly
    } else {
        Installs::Yes
    }
}

pub(crate) fn cargo_bin() -> PathBuf {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| store::home().join(".cargo"));
    let bin = home.join("bin");
    bin.canonicalize().unwrap_or(bin)
}

/// An install writes a file beside the binary and renames it over, so the folder is what has to
/// be writable.
pub(crate) fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".taskman-write-test-{}", std::process::id()));
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The release to install into `dir`: the latest, when it's newer than the running one but not a
/// breaking release (`same_minor`), not installed there yet, and hasn't failed to install since
/// taskman started.
fn to_install(s: &State, running: &str, dir: &Path, failed: &[String]) -> Option<String> {
    let latest = s.latest.as_ref()?;
    let there =
        s.installed.as_ref().filter(|i| i.dir == dir).is_some_and(|i| !newer(latest, &i.version));
    let compatible = newer(latest, running) && same_minor(latest, running);
    (compatible && !there && !failed.contains(latest)).then(|| latest.clone())
}

/// At startup the binary in `dir` is the one running, so a record of installing another
/// version there is stale: it was replaced since. True when it changed `s`.
fn forget_replaced(s: &mut State, dir: &Path, running: &str) -> bool {
    let stale = s.installed.as_ref().is_some_and(|i| i.dir == dir && i.version != running);
    if stale {
        s.installed = None;
    }
    stale
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Installed; the next start runs it.
    Restart,
    /// Not installed: `taskman update` installs it.
    Update,
    /// A breaking release, never installed in the background: read its notes, then
    /// `taskman update`.
    Breaking,
    /// A cargo install: running `cargo install` again installs it.
    Cargo,
    /// A development build: rebuild from source.
    Rebuild,
    /// A Homebrew install: `brew upgrade` installs it.
    Brew,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub version: String,
    pub action: Action,
}

fn notice(s: &State, running: &str, dir: Option<&Path>, installs: Installs) -> Option<Notice> {
    if let Some(i) = &s.installed
        && Some(i.dir.as_path()) == dir
        && newer(&i.version, running)
        && s.latest.as_deref().is_none_or(|l| !newer(l, &i.version))
    {
        return Some(Notice { version: i.version.clone(), action: Action::Restart });
    }
    let latest = s.latest.as_ref().filter(|l| newer(l, running))?;
    let action = match installs {
        Installs::DevBuild => Action::Rebuild,
        Installs::Homebrew => Action::Brew,
        Installs::Cargo => Action::Cargo,
        _ if !same_minor(latest, running) => Action::Breaking,
        _ => Action::Update,
    };
    Some(Notice { version: latest.clone(), action })
}

/// The notice for the sidebar, shortened when `width` is too narrow: to the command alone, which
/// matters more than the version (Settings shows it), then to the version.
pub fn notice_text(n: &Notice, width: usize) -> String {
    let v = &n.version;
    let texts = match n.action {
        Action::Restart => vec![format!("↑ restart to get {v}")],
        // One column more than the sidebar has once the version is 7 characters (v0.4.10).
        Action::Update => vec![format!("↑ {v}: run taskman update"), "↑ run taskman update".into()],
        // Settings says what to do: read the release's notes, then run taskman update.
        Action::Breaking => vec![format!("↑ {v}: breaking changes")],
        Action::Rebuild | Action::Cargo => vec![format!("↑ {v} available")],
        Action::Brew => vec![format!("↑ {v} · brew upgrade")],
    };
    texts.into_iter().find(|t| crate::util::width(t) <= width).unwrap_or_else(|| format!("↑ {v}"))
}

/// Finished background work.
struct Done {
    state: State,
    /// A release that didn't install, and why.
    failed: Option<(String, String)>,
}

/// The TUI's background updates: checks for a release at startup and every hour, installs a
/// newer one when this copy may, and tells the sidebar what to say. Never restarts taskman.
pub struct Updater {
    state: State,
    /// The running binary, links resolved, found at startup: once the file is replaced, the OS
    /// reports the running binary as deleted. Installs go over it, whatever it's named.
    exe: Option<PathBuf>,
    installs: Installs,
    /// TASKMAN_NO_AUTOUPDATE: no checks and no notice.
    off: bool,
    busy: bool,
    /// Whether this run has checked yet: a start always checks, whatever update.json says.
    checked: bool,
    /// After a failed check, wait this long before the next one.
    retry_at: Option<Instant>,
    /// Releases that failed to install since taskman started.
    failed: Vec<String>,
    /// Why the last of them failed, for Settings.
    why_failed: Option<String>,
    mtime: Option<SystemTime>,
    reloaded: Instant,
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
}

impl Updater {
    pub fn new(exe: &Path) -> Self {
        let off = std::env::var("TASKMAN_NO_AUTOUPDATE").is_ok_and(|v| !v.is_empty() && v != "0");
        let exe = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
        let installs = match installs(&exe, &cargo_bin(), released(), true) {
            Installs::Yes if off || !exe.parent().is_some_and(writable) => Installs::ReadOnly,
            i => i,
        };
        let mut state = load();
        if !off
            && let Some(d) = exe.parent()
            && forget_replaced(&mut state, d, &running())
        {
            save(&state);
        }
        let (tx, rx) = mpsc::channel();
        Self {
            state,
            exe: Some(exe),
            installs,
            off,
            busy: false,
            checked: false,
            retry_at: None,
            failed: Vec::new(),
            why_failed: None,
            mtime: modified(),
            reloaded: Instant::now(),
            tx,
            rx,
        }
    }

    /// For commands that build the TUI's state without running it: checks and shows nothing.
    pub fn disabled() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            state: State::default(),
            exe: None,
            installs: Installs::ReadOnly,
            off: true,
            busy: false,
            checked: false,
            retry_at: None,
            failed: Vec::new(),
            why_failed: None,
            mtime: None,
            reloaded: Instant::now(),
            tx,
            rx,
        }
    }

    /// Takes in finished work and `taskman update` runs, and starts a check or an install when
    /// one is due. `auto` is the Settings toggle. True when the notice may have changed.
    pub fn poll(&mut self, auto: bool) -> bool {
        if self.off {
            return false;
        }
        let mut changed = false;
        while let Ok(done) = self.rx.try_recv() {
            self.busy = false;
            self.state = done.state;
            if let Some((tag, why)) = done.failed {
                self.failed.push(tag);
                self.why_failed = Some(why);
            }
            self.mtime = modified();
            changed = true;
        }
        if self.busy {
            return changed;
        }
        if self.reloaded.elapsed() >= RELOAD_EVERY {
            self.reloaded = Instant::now();
            let m = modified();
            if m != self.mtime {
                self.mtime = m;
                self.state = load();
                changed = true;
            }
        }
        let check = (!self.checked || due(&self.state, util::now()))
            && self.retry_at.is_none_or(|t| Instant::now() >= t);
        let exe = self.exe.clone().filter(|_| self.installs_automatically(auto));
        let install = exe
            .as_deref()
            .and_then(Path::parent)
            .and_then(|d| to_install(&self.state, &running(), d, &self.failed))
            .is_some();
        if check || install {
            if check {
                self.checked = true;
                self.retry_at = Some(Instant::now() + Duration::from_secs(CHECK_EVERY as u64));
            }
            self.busy = true;
            let (state, failed, tx) = (self.state.clone(), self.failed.clone(), self.tx.clone());
            std::thread::spawn(move || {
                // A panic would leave `busy` set, and no check or install would run again.
                let done = util::caught(|| work(state.clone(), check, exe, &failed));
                let _ = tx.send(done.unwrap_or(Done { state, failed: None }));
            });
        }
        changed
    }

    /// Whether a newer release gets installed, given the Settings toggle.
    pub fn installs_automatically(&self, auto: bool) -> bool {
        !self.off && auto && self.installs == Installs::Yes
    }

    /// The running binary's folder, which `State::installed` records installs by.
    fn dir(&self) -> Option<&Path> {
        self.exe.as_deref().and_then(Path::parent)
    }

    /// What the sidebar says, if anything. Nothing while a release is about to be installed.
    pub fn notice(&self, auto: bool) -> Option<Notice> {
        if self.off {
            return None;
        }
        let n = notice(&self.state, &running(), self.dir(), self.installs)?;
        let installing = n.action == Action::Update
            && self.installs_automatically(auto)
            && !self.failed.contains(&n.version);
        (!installing).then_some(n)
    }

    /// A line for Settings: the version, what the last check found, and why this copy
    /// only notifies.
    pub fn describe(&self) -> String {
        if self.off {
            return "TASKMAN_NO_AUTOUPDATE is set: taskman doesn't check for updates.".into();
        }
        let mut out = format!("taskman {}", running());
        if let Some(l) = &self.state.latest {
            let when = util::ago_words(util::now() - self.state.checked_at);
            out.push_str(&format!(" · latest release {l}, checked {when}"));
        }
        match notice(&self.state, &running(), self.dir(), self.installs) {
            Some(Notice { version, action: Action::Restart }) => out.push_str(&format!(
                "\n{version} is installed: it runs when taskman restarts (sessions resume)."
            )),
            Some(Notice { version, action: Action::Breaking }) => out.push_str(&format!(
                "\n{version} has breaking changes, so it isn't installed in the background. Read its notes ({REPO_URL}/releases/tag/{version}), then run taskman update."
            )),
            Some(Notice { version, action: Action::Update }) => {
                if self.failed.contains(&version)
                    && let Some(why) = &self.why_failed
                {
                    out.push_str(&format!(
                        "\n{version} wasn't installed in the background: {why}. taskman update installs it, or says why it can't."
                    ));
                }
            }
            _ => {}
        }
        let why = match self.installs {
            Installs::Yes => None,
            Installs::DevBuild => {
                Some("A development build only notifies: rebuild it from source.")
            }
            Installs::Cargo => {
                Some("Installed with cargo, so it only notifies: cargo manages that folder.")
            }
            Installs::Homebrew => {
                Some("Installed with Homebrew, so it only notifies: update it with brew upgrade.")
            }
            Installs::ReadOnly => {
                Some("Only notifies: taskman can't write to the folder it's installed in.")
            }
        };
        if let Some(why) = why {
            out.push('\n');
            out.push_str(why);
        }
        if skip_attestation() {
            out.push_str(&format!(
                "\n{SKIP_ATTESTATION} is set: taskman update checks releases against their checksum only, not their attestation, and none is installed in the background."
            ));
        }
        out
    }
}

/// Checks for a release (when `check`), then installs over `exe` what `to_install` picks.
/// Failures stay quiet: a failed check keeps the old state, a failed install is reported back.
fn work(mut state: State, check: bool, exe: Option<PathBuf>, failed: &[String]) -> Done {
    let before = state.clone();
    if check && let Ok(tag) = latest_release(CHECK_TIMEOUT) {
        state.latest = Some(tag);
        state.checked_at = util::now();
    }
    let mut failed_tag = None;
    if let Some(exe) = exe
        && let Some(dir) = exe.parent().map(Path::to_path_buf)
        && let Some(tag) = to_install(&state, &running(), &dir, failed)
    {
        // A panic counts as a failure, so the release isn't tried again on every tick.
        match util::caught(|| install(&exe, &tag, By::Background)).and_then(|r| r) {
            Ok(()) => state.installed = Some(Installed { version: tag, dir }),
            Err(why) => failed_tag = Some((tag, why)),
        }
    }
    if state != before {
        save(&state);
    }
    Done { state, failed: failed_tag }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arguments() {
        let args = |a: &[&str]| parse_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(args(&[]), Ok(false));
        assert_eq!(args(&["--force"]), Ok(true));
        // A misspelt --force, or anything else, is refused rather than ignored.
        assert_eq!(args(&["--frce"]), Err(format!("unknown option --frce\n\n{USAGE}")));
        assert_eq!(args(&["now"]), Err(format!("unknown argument now\n\n{USAGE}")));
        assert_eq!(args(&["--force=no"]), Err("--force takes no value".into()));
    }

    #[test]
    fn compares_versions_numerically() {
        assert_eq!(parse("v0.3.1"), Some((0, 3, 1)));
        assert!(parse("v0.10.0") > parse("v0.9.9"));
        assert_eq!(parse("v1.2"), None);
        assert!(newer("v0.4.3", "v0.4.2"));
        assert!(!newer("v0.4.2", "v0.4.2"));
        assert!(!newer("garbage", "v0.4.2"));
    }

    #[test]
    fn checks_every_hour() {
        let s = State { checked_at: 1_000_000, ..Default::default() };
        assert!(due(&State::default(), 1_000_000));
        assert!(!due(&s, 1_000_000 + 60));
        assert!(!due(&s, 1_000_000 + CHECK_EVERY - 1));
        assert!(due(&s, 1_000_000 + CHECK_EVERY));
        // A clock set back shouldn't stop checks until it catches up.
        assert!(due(&s, 1_000_000 - 60));
    }

    #[test]
    fn only_release_installs_replace_themselves() {
        let cargo = Path::new("/home/me/.cargo/bin");
        let release = |exe: &str, writable| installs(Path::new(exe), cargo, true, writable);
        assert_eq!(release("/home/me/.cargo/bin/taskman", true), Installs::Cargo);
        assert_eq!(release("/usr/local/bin/taskman", false), Installs::ReadOnly);
        assert_eq!(release("/home/me/.local/bin/taskman", true), Installs::Yes);
        assert_eq!(
            release("/opt/homebrew/Cellar/taskman/0.4.3/bin/taskman", true),
            Installs::Homebrew
        );
        // A release in a folder that happens to be called target is still a release.
        assert_eq!(release("/home/me/target/bin/taskman", true), Installs::Yes);
    }

    #[test]
    fn a_development_build_is_one_wherever_it_is() {
        let cargo = Path::new("/home/me/.cargo/bin");
        let dev = |exe: &str| installs(Path::new(exe), cargo, false, true);
        for exe in [
            "/home/me/src/taskman/target/release/taskman",
            // A CARGO_TARGET_DIR by another name, and a build copied out of its checkout.
            "/home/me/builds/shared/debug/taskman",
            "/home/me/.local/bin/taskman",
            "/usr/local/bin/taskman",
        ] {
            assert_eq!(dev(exe), Installs::DevBuild, "{exe}");
        }
        // Homebrew and cargo build from source themselves.
        assert_eq!(dev("/home/me/.cargo/bin/taskman"), Installs::Cargo);
        assert_eq!(dev("/opt/homebrew/Cellar/taskman/0.4.3/bin/taskman"), Installs::Homebrew);
    }

    /// The tests run a development build, as CI does, so the tests in tests/ can't remove or
    /// replace the binary they run; only the release workflow's builds are releases.
    #[test]
    fn the_tests_run_a_development_build() {
        assert_eq!(option_env!("TASKMAN_RELEASE_BUILD"), None, "built as a release");
        let released = include_str!("../.github/workflows/release.yml");
        let build = released.find("- run: cargo build --profile dist").unwrap();
        assert!(released[build..].starts_with(
            "- run: cargo build --profile dist --locked --target \"$TARGET\"\n        env:\n          TASKMAN_RELEASE_BUILD: \"1\"\n"
        ));
    }

    #[test]
    fn repository_comes_from_cargo_toml() {
        assert!(REPO_URL.starts_with("https://github.com/"));
        let (owner, name) = repo().split_once('/').unwrap();
        assert!(!owner.is_empty() && !name.is_empty() && !name.contains('/'));
    }

    #[test]
    fn reads_the_latest_tag_from_the_redirect() {
        let tag = |l| tag_from_location(l);
        assert_eq!(
            tag("https://github.com/acme/taskman/releases/tag/v0.4.3"),
            Some("v0.4.3".into())
        );
        assert_eq!(tag("http://127.0.0.1:8000/releases/tag/v1.0.0?x=1"), Some("v1.0.0".into()));
        // No releases yet: GitHub sends the releases page.
        assert_eq!(tag("https://github.com/acme/taskman/releases"), None);
        assert_eq!(tag("https://github.com/acme/taskman/releases/tag/nightly"), None);
        assert_eq!(tag("https://github.com/acme/taskman/releases/tag/v1.0.0/../x"), None);
    }

    #[test]
    fn finds_the_archive_in_checksum_files() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!(
            "{a}  taskman-v0.4.3-aarch64-apple-darwin.tar.gz\n{b} *taskman-v0.4.3-x86_64-unknown-linux-gnu.tar.gz\n"
        );
        assert_eq!(
            expected_sum(&sums, "taskman-v0.4.3-aarch64-apple-darwin.tar.gz"),
            Some(a.clone())
        );
        assert_eq!(
            expected_sum(&sums, "taskman-v0.4.3-x86_64-unknown-linux-gnu.tar.gz"),
            Some("b".repeat(64))
        );
        assert_eq!(expected_sum(&sums, "taskman-v0.4.3-x86_64-apple-darwin.tar.gz"), None);
        // Not a SHA-256.
        assert_eq!(expected_sum("abc123  taskman.tar.gz", "taskman.tar.gz"), None);
        assert_eq!(expected_sum("", "taskman.tar.gz"), None);
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn falls_back_to_gh_only_when_https_fails() {
        let ok = || -> Result<u8, GhError> { Ok(2) };
        let missing = || -> Result<u8, GhError> { Err(GhError::Missing) };
        let failed = || -> Result<u8, GhError> { Err(GhError::Failed("HTTP 404".into())) };
        assert_eq!(
            either(Ok(1), || -> Result<u8, GhError> { panic!("gh ran") }),
            Ok((1, Via::Https))
        );
        assert_eq!(either(Err("x wasn't found".into()), ok), Ok((2, Via::Gh)));
        assert_eq!(
            either(Err("x wasn't found".into()), missing),
            Err::<(u8, Via), _>("x wasn't found".into())
        );
        assert_eq!(
            either(Err("x wasn't found".into()), failed),
            Err::<(u8, Via), _>("x wasn't found; through gh: HTTP 404".into())
        );
    }

    fn state(latest: &str, installed: Option<(&str, &str)>) -> State {
        State {
            checked_at: 0,
            latest: Some(latest.into()),
            installed: installed.map(|(v, d)| Installed { version: v.into(), dir: d.into() }),
        }
    }

    #[test]
    fn says_why_a_program_failed() {
        let sh = |script: &str, timeout| {
            stdout_of(proc::run(Command::new("sh").args(["-c", script]), timeout))
        };
        assert_eq!(sh("echo ok", Duration::from_secs(10)).unwrap(), Ok(b"ok\n".to_vec()));
        let failed =
            sh("echo first >&2; echo last >&2; echo >&2; exit 1", Duration::from_secs(10)).unwrap();
        assert_eq!(failed, Err("last".to_string()));
        assert_eq!(
            sh("exit 1", Duration::from_secs(10)).unwrap(),
            Err("it failed without saying why".to_string())
        );
        // Running out of time is the program failing; only not starting is the outer error.
        let start = Instant::now();
        let slow = sh("sleep 10 & echo ready", Duration::from_millis(300)).unwrap();
        assert!(slow.is_err_and(|e| e.contains("didn't finish")));
        assert!(start.elapsed() < Duration::from_secs(3), "took {:?}", start.elapsed());
        let missing = stdout_of(proc::run(
            &mut Command::new("taskman-no-such-program"),
            Duration::from_secs(1),
        ));
        assert_eq!(missing.unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }

    /// What `gh` would return: its exit code and stderr.
    fn gh_out(code: i32, stderr: &str) -> std::io::Result<Output> {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::ExitStatus::from_raw(code << 8);
        Ok(Output { status, stdout: Vec::new(), stderr: stderr.as_bytes().to_vec() })
    }

    #[test]
    fn attestations_fall_back_to_the_checksum_only_when_there_is_nothing_to_check() {
        let unchecked = |why: &str| Ok(Attestation::Unchecked(why.into()));
        let a = |out| attestation(out, "v1.2.3", None);
        assert_eq!(a(gh_out(0, "")), Ok(Attestation::Verified));
        let missing = std::io::Error::new(std::io::ErrorKind::NotFound, "no gh");
        assert_eq!(
            a(Err(missing)),
            unchecked("the GitHub CLI (gh) isn't installed to verify its attestation")
        );
        let logged_out = "To get started with GitHub CLI, please run:  gh auth login\n";
        let login = unchecked("gh isn't logged in (gh auth login) to verify its attestation");
        assert_eq!(a(gh_out(4, logged_out)), login);
        assert_eq!(
            a(gh_out(1, "Error: HTTP 401: Bad credentials (https://api.github.com/)")),
            login
        );
        assert_eq!(
            a(gh_out(1, "unknown command \"attestation\" for \"gh\"\n\nUsage: gh <command>\n")),
            unchecked("this gh has no attestation command; update gh to verify it")
        );
        // What gh 2.86 says for a public repository and a private one.
        let none = unchecked("release v1.2.3 has no attestation to verify");
        assert_eq!(a(gh_out(1, "\nError: no attestations found\n")), none);
        let private = "\nError: HTTP 404: Not Found (https://api.github.com/repos/acme/taskman/attestations/sha256:00?per_page=30)\n";
        assert_eq!(a(gh_out(1, private)), none);
        // Anything else refuses the archive, with gh's last line.
        let refused = |why: &str, network| Err(Refusal { why: why.into(), network });
        assert_eq!(
            a(gh_out(1, "✗ Verification failed\nError: certificate's SAN doesn't match\n\n")),
            refused("Error: certificate's SAN doesn't match", false)
        );
        assert_eq!(a(gh_out(1, "")), refused("it failed without saying why", false));
        let slow = std::io::Error::new(std::io::ErrorKind::TimedOut, "it didn't finish within 60s");
        assert_eq!(a(Err(slow)), refused("it didn't finish within 60s", true));
    }

    #[test]
    fn names_the_way_around_only_when_it_may_be_the_network() {
        let network = |err: &str| network_trouble(err);
        for err in [
            "Error: HTTP 502: Bad Gateway (https://api.github.com/repos/acme/taskman/attestations/sha256:00)",
            "Error: HTTP 429: Too Many Requests",
            "Error: HTTP 403: API rate limit exceeded for user ID 1.",
            "Error: Get \"https://api.github.com/\": dial tcp: lookup api.github.com: no such host",
            "failed to create TUF client: Get \"https://tuf-repo-cdn.sigstore.dev/\": net/http: TLS handshake timeout",
            "Error: Get \"https://api.github.com/\": x509: certificate signed by unknown authority",
            "Error: Get \"https://api.github.com/\": proxyconnect tcp: connection refused",
        ] {
            assert!(network(err), "{err}");
        }
        for err in [
            "✗ Verification failed\nError: verifying with issuer \"sigstore.dev\"",
            // A signer named to look like the network is still a verdict on the archive.
            "Error: verifying with issuer \"sigstore.dev\": failed to verify certificate identity: \
             expected SAN to match \"https://github.com/acme/taskman/.github/workflows/release.yml\", \
             got \"https://github.com/acme/dial-tcp/.github/workflows/no-such-host.yml\"",
            "Error: digest mismatch",
            "Error: HTTP 5",
            "Error: HTTP 5xx",
            "it failed without saying why",
        ] {
            assert!(!network(err), "{err}");
        }
        let refusal = |network| Refusal { why: "HTTP 503".into(), network };
        let archive = "taskman-v1.2.3-x.tar.gz";
        assert_eq!(
            refusal(false).text(archive),
            "gh couldn't verify taskman-v1.2.3-x.tar.gz's attestation (HTTP 503); nothing was installed"
        );
        assert_eq!(
            refusal(true).text(archive),
            "gh couldn't verify taskman-v1.2.3-x.tar.gz's attestation (HTTP 503); nothing was installed. \
             Try again later; if your network blocks gh, TASKMAN_INSECURE_SKIP_ATTESTATION=1 installs \
             on the checksum alone"
        );
    }

    #[test]
    fn requires_attestations_from_the_switched_on_release() {
        // The switch is a release, and install.sh's is the same one.
        assert!(REQUIRE_ATTESTATION_FROM.is_none_or(|v| parse(v).is_some()));
        let script = include_str!("../scripts/install.sh");
        let line = script.lines().find_map(|l| l.trim().strip_prefix("require_from="));
        assert_eq!(line, Some(REQUIRE_ATTESTATION_FROM.unwrap_or("")));
        let none = || gh_out(1, "\nError: no attestations found\n");
        let private = || gh_out(1, "Error: HTTP 404: Not Found (https://api.github.com/)");
        let unchecked = |tag: &str| {
            Ok(Attestation::Unchecked(format!("release {tag} has no attestation to verify")))
        };
        assert_eq!(attestation(none(), "v9.9.9", None), unchecked("v9.9.9"));
        // On: a release from then on with none is refused, an older one still falls back.
        let on = Some("v1.0.0");
        for tag in ["v1.0.0", "v1.0.1", "v1.10.0", "v2.0.0"] {
            let refused = Err(Refusal {
                why: format!("release {tag} has none, though every release from v1.0.0 on has one"),
                network: false,
            });
            assert_eq!(attestation(none(), tag, on), refused, "{tag}");
            assert_eq!(attestation(private(), tag, on), refused, "{tag}");
        }
        assert_eq!(attestation(none(), "v0.9.9", on), unchecked("v0.9.9"));
        assert_eq!(attestation(gh_out(0, ""), "v1.0.0", on), Ok(Attestation::Verified));
        // As shipped: the releases from before the repository was public still fall back.
        assert_eq!(attestation(none(), "v0.0.9", REQUIRE_ATTESTATION_FROM), unchecked("v0.0.9"));
        assert!(attestation(none(), "v0.1.0", REQUIRE_ATTESTATION_FROM).is_err());
        // Without gh, or logged out, there's still nothing to check with.
        let missing = Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no gh"));
        assert!(matches!(attestation(missing, "v1.0.0", on), Ok(Attestation::Unchecked(_))));
        assert!(matches!(attestation(gh_out(4, ""), "v1.0.0", on), Ok(Attestation::Unchecked(_))));
    }

    #[test]
    fn verifies_against_this_repository() {
        let cmd = attest_command(Path::new("/tmp/taskman-v1.2.3-x.tar.gz"));
        assert_eq!(cmd.get_program(), "gh");
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let identity = format!(
            r"^https://github\.com/{}/\.github/workflows/release\.yml@refs/(tags/v[0-9]+\.[0-9]+\.[0-9]+|heads/main)$",
            repo().replace('.', r"\.")
        );
        assert_eq!(
            args,
            [
                "attestation",
                "verify",
                "/tmp/taskman-v1.2.3-x.tar.gz",
                "--repo",
                repo(),
                "--cert-identity-regex",
                &identity,
                "--deny-self-hosted-runners",
            ]
        );
        // install.sh asks for the same refs, and the same flags.
        let script = include_str!("../scripts/install.sh");
        let refs = script.lines().find_map(|l| l.trim().strip_prefix("signer_refs="));
        assert_eq!(refs, Some(format!("'{SIGNER_REFS}'").as_str()));
        assert!(
            script.contains("--cert-identity-regex")
                && script.contains("--deny-self-hosted-runners")
        );
        assert!(!script.contains("--signer-workflow"));
    }

    #[test]
    fn updates_to_a_build_the_release_has_and_install_sh_picks() {
        let released = include_str!("../.github/workflows/release.yml");
        let script = include_str!("../scripts/install.sh");
        let mut targets = vec![];
        for os in ["macos", "linux", "freebsd"] {
            for arch in ["aarch64", "x86_64", "arm"] {
                for libc in ["gnu", "musl", ""] {
                    targets.extend(target_for(os, arch, libc));
                }
            }
        }
        targets.sort();
        targets.dedup();
        assert_eq!(targets.len(), 6, "{targets:?}");
        for t in &targets {
            assert!(released.contains(&format!("- {{ target: {t},")), "release.yml builds {t}");
            assert!(script.contains(&format!(") target={t} ;;")), "install.sh picks {t}");
        }
        assert_eq!(released.matches("- { target: ").count(), targets.len());
        // The release workflow wants an archive for each of install.sh's `) target=` lines.
        assert_eq!(script.matches(") target=").count(), targets.len());
        assert!(released.contains(r"s/.*) target=\([a-z0-9_-]*\) ;;.*/\1/p"));
        // A Linux build with neither C library has no release; this one has, where there's one.
        assert_eq!(target_for("linux", "x86_64", ""), None);
        if cfg!(any(target_os = "macos", target_os = "linux"))
            && cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
        {
            assert!(target().is_some());
        }
    }

    #[test]
    fn writes_the_archive_to_verify_into_a_private_folder() {
        use std::os::unix::fs::PermissionsExt;
        let parent = store::state_dir();
        std::fs::create_dir_all(&parent).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let a = Private::write(&parent, "taskman-v1.2.3-x.tar.gz", b"archive").unwrap();
        assert_eq!(a.file.parent(), Some(a.dir.as_path()));
        assert_eq!(a.dir.parent(), Some(parent.as_path()));
        assert_eq!(a.file.file_name().unwrap(), "taskman-v1.2.3-x.tar.gz");
        assert_eq!(mode(&a.dir), 0o700);
        assert_eq!(mode(&a.file), 0o600);
        assert_eq!(std::fs::read(&a.file).unwrap(), b"archive");
        // A new folder each time, and gone with the copy.
        let b = Private::write(&parent, "taskman-v1.2.3-x.tar.gz", b"archive").unwrap();
        assert_ne!(a.dir, b.dir);
        let dir = a.dir.clone();
        drop(a);
        assert!(!dir.exists());
        assert!(b.file.exists());
    }

    mod attestations {
        use super::{Attestation, attestation, gh_out};
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// gh failing with words it isn't known to use never lets an archive through.
            #[test]
            fn unknown_failures_refuse(
                code in prop_oneof![Just(1), 2..4, 5..256],
                stderr in "[ -~\n✗✓]{0,200}",
            ) {
                prop_assume!(!["HTTP 401", "HTTP 404", "no attestations found", "unknown command"]
                    .iter()
                    .any(|known| stderr.contains(known)));
                prop_assert!(attestation(gh_out(code, &stderr), "v1.2.3", None).is_err());
                let verified = attestation(gh_out(0, &stderr), "v1.2.3", None);
                prop_assert_eq!(verified, Ok(Attestation::Verified));
            }
        }
    }

    #[test]
    fn background_installs_only_verified_releases() {
        let unchecked = Attestation::Unchecked(NO_GH.into());
        assert_eq!(accepts(By::Background, &Attestation::Verified), Ok(()));
        assert_eq!(accepts(By::Background, &unchecked), Err(NO_GH.into()));
        assert_eq!(
            accepts(By::Background, &Attestation::Skipped),
            Err(format!("{SKIP_ATTESTATION} is set, so its attestation isn't verified"))
        );
        // taskman update installs on the checksum alone, and says so.
        assert_eq!(accepts(By::Command, &Attestation::Verified), Ok(()));
        assert_eq!(accepts(By::Command, &unchecked), Ok(()));
        assert_eq!(accepts(By::Command, &Attestation::Skipped), Ok(()));
    }

    #[test]
    fn background_installs_stay_within_the_minor_version() {
        assert!(same_minor("v0.1.10", "v0.1.2"));
        assert!(!same_minor("v0.2.0", "v0.1.10"));
        assert!(!same_minor("v1.1.0", "v0.1.0"));
        assert!(!same_minor("garbage", "v0.1.0"));
        let dir = Path::new("/home/me/.local/bin");
        // A patch release installs; a new minor or major doesn't.
        assert_eq!(to_install(&state("v0.4.9", None), "v0.4.2", dir, &[]), Some("v0.4.9".into()));
        assert_eq!(to_install(&state("v0.5.0", None), "v0.4.2", dir, &[]), None);
        assert_eq!(to_install(&state("v1.4.3", None), "v0.4.2", dir, &[]), None);
        // It's announced as breaking instead, whether or not this copy installs.
        let n = |s: &State, installs| notice(s, "v0.4.2", Some(dir), installs).map(|n| n.action);
        assert_eq!(n(&state("v0.5.0", None), Installs::Yes), Some(Action::Breaking));
        assert_eq!(n(&state("v0.5.0", None), Installs::ReadOnly), Some(Action::Breaking));
        assert_eq!(n(&state("v0.4.3", None), Installs::Yes), Some(Action::Update));
        assert_eq!(n(&state("v0.5.0", None), Installs::Cargo), Some(Action::Cargo));
        // Once taskman update has installed it, the restart.
        let ready = state("v0.5.0", Some(("v0.5.0", "/home/me/.local/bin")));
        assert_eq!(n(&ready, Installs::Yes), Some(Action::Restart));
    }

    #[test]
    fn installs_each_newer_release_once() {
        let dir = Path::new("/home/me/.local/bin");
        assert_eq!(to_install(&state("v0.4.3", None), "v0.4.2", dir, &[]), Some("v0.4.3".into()));
        assert_eq!(to_install(&state("v0.4.2", None), "v0.4.2", dir, &[]), None);
        assert_eq!(to_install(&state("v0.4.1", None), "v0.4.2", dir, &[]), None);
        // Already installed here, waiting for a restart.
        assert_eq!(
            to_install(
                &state("v0.4.3", Some(("v0.4.3", "/home/me/.local/bin"))),
                "v0.4.2",
                dir,
                &[]
            ),
            None
        );
        // Installed somewhere else, or an older one installed here.
        assert_eq!(
            to_install(&state("v0.4.3", Some(("v0.4.3", "/opt/bin"))), "v0.4.2", dir, &[]),
            Some("v0.4.3".into())
        );
        assert_eq!(
            to_install(
                &state("v0.4.4", Some(("v0.4.3", "/home/me/.local/bin"))),
                "v0.4.2",
                dir,
                &[]
            ),
            Some("v0.4.4".into())
        );
        // Failed since taskman started.
        assert_eq!(to_install(&state("v0.4.3", None), "v0.4.2", dir, &["v0.4.3".into()]), None);
    }

    #[test]
    fn forgets_installs_replaced_since() {
        let dir = Path::new("/home/me/.local/bin");
        // Restarted into what was installed: keep it, another TUI may still be waiting on it.
        let mut s = state("v0.4.3", Some(("v0.4.3", "/home/me/.local/bin")));
        assert!(!forget_replaced(&mut s, dir, "v0.4.3"));
        // Something else is installed there now.
        assert!(forget_replaced(&mut s, dir, "v0.4.1"));
        assert_eq!(s.installed, None);
        let mut s = state("v0.4.3", Some(("v0.4.3", "/opt/bin")));
        assert!(!forget_replaced(&mut s, dir, "v0.4.1"));
    }

    #[test]
    fn notice_says_what_to_do() {
        let dir = Some(Path::new("/home/me/.local/bin"));
        let n =
            |s: &State, installs| notice(s, "v0.4.2", dir, installs).map(|n| (n.version, n.action));
        assert_eq!(n(&state("v0.4.2", None), Installs::Yes), None);
        assert_eq!(
            n(&state("v0.4.3", None), Installs::Yes),
            Some(("v0.4.3".into(), Action::Update))
        );
        assert_eq!(
            n(&state("v0.4.3", None), Installs::Cargo),
            Some(("v0.4.3".into(), Action::Cargo))
        );
        assert_eq!(
            n(&state("v0.4.3", None), Installs::DevBuild),
            Some(("v0.4.3".into(), Action::Rebuild))
        );
        assert_eq!(
            n(&state("v0.4.3", None), Installs::Homebrew),
            Some(("v0.4.3".into(), Action::Brew))
        );
        let ready = state("v0.4.3", Some(("v0.4.3", "/home/me/.local/bin")));
        assert_eq!(n(&ready, Installs::Yes), Some(("v0.4.3".into(), Action::Restart)));
        // Installed in another folder: this copy still needs updating.
        assert_eq!(
            n(&state("v0.4.3", Some(("v0.4.3", "/opt/bin"))), Installs::Yes),
            Some(("v0.4.3".into(), Action::Update))
        );
        // A newer release than the one installed.
        assert_eq!(
            n(&state("v0.4.4", Some(("v0.4.3", "/home/me/.local/bin"))), Installs::Yes),
            Some(("v0.4.4".into(), Action::Update))
        );
        // After restarting into it.
        assert_eq!(notice(&ready, "v0.4.3", dir, Installs::Yes), None);
    }

    #[test]
    fn notice_shortens_when_narrow() {
        let ready = Notice { version: "v0.4.3".into(), action: Action::Restart };
        let update = Notice { version: "v0.4.3".into(), action: Action::Update };
        // The sidebar is 30 columns: 28 after its border and the leading space.
        assert_eq!(notice_text(&ready, 28), "↑ restart to get v0.4.3");
        // Fits whatever the version's length.
        let later = Notice { version: "v0.10.100".into(), action: Action::Restart };
        assert_eq!(notice_text(&later, 28), "↑ restart to get v0.10.100");
        assert_eq!(notice_text(&update, 28), "↑ v0.4.3: run taskman update");
        // Too long with the version: the command alone.
        let update_later = Notice { version: "v0.4.24".into(), action: Action::Update };
        assert_eq!(notice_text(&update_later, 28), "↑ run taskman update");
        assert_eq!(notice_text(&update_later, 29), "↑ v0.4.24: run taskman update");
        assert_eq!(notice_text(&ready, 11), "↑ v0.4.3");
        assert_eq!(notice_text(&update, 11), "↑ v0.4.3");
        assert_eq!(
            notice_text(&Notice { version: "v0.4.3".into(), action: Action::Rebuild }, 28),
            "↑ v0.4.3 available"
        );
        assert_eq!(
            notice_text(&Notice { version: "v0.4.24".into(), action: Action::Cargo }, 28),
            "↑ v0.4.24 available"
        );
        assert_eq!(
            notice_text(&Notice { version: "v0.4.3".into(), action: Action::Brew }, 28),
            "↑ v0.4.3 · brew upgrade"
        );
        let breaking = Notice { version: "v0.10.0".into(), action: Action::Breaking };
        assert_eq!(notice_text(&breaking, 28), "↑ v0.10.0: breaking changes");
        assert_eq!(notice_text(&breaking, 20), "↑ v0.10.0");
    }

    #[test]
    fn reads_older_update_files() {
        let s: State = serde_json::from_str(r#"{"checked_at":1700000000}"#).unwrap();
        assert_eq!(s.latest, None);
        let s: State = serde_json::from_str(
            r#"{"checked_at":1,"latest":"v0.4.3","installed":{"version":"v0.4.3","dir":"/x"},"later":true}"#,
        )
        .unwrap();
        assert_eq!(s.installed.unwrap().dir, PathBuf::from("/x"));
    }
}
