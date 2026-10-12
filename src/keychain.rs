//! The OS keychain the account form can save a token to: the macOS Keychain through `security`, or
//! the Linux Secret Service (GNOME Keyring, KWallet) through `secret-tool`. Each item is taskman's
//! own: service `taskman`, and as its account the name its `keychain:` source gives (the account's
//! id, like `jira-1`). The token goes to the program on stdin, never on its command line, where
//! `ps` would show it. See "Tokens in the keychain" in docs/design.md.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

/// The service every item of taskman's is saved under.
pub const SERVICE: &str = "taskman";

/// How an account's token field names an item: `keychain:jira-1`.
pub const PREFIX: &str = "keychain:";

/// How long saving or reading may take: long enough to unlock the keychain in its own window.
pub const LIMIT: Duration = Duration::from_secs(60);

/// How long the check for a keychain may take; it opens no window.
const PROBE_LIMIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// macOS's `security`.
    MacOs,
    /// `secret-tool`, from libsecret.
    SecretService,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Keychain {
    kind: Kind,
    program: PathBuf,
}

/// The source that reads the item `name`.
pub fn source(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The item a `keychain:` source names, if it's one and the name is valid.
pub fn name_of(source: &str) -> Option<&str> {
    source.trim().strip_prefix(PREFIX).filter(|n| is_name(n))
}

/// Names are taskman's own (account ids), so they need no quoting anywhere: letters, digits, `.`,
/// `_` and `-`.
pub fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// Refuses what can't be a token, before anything runs: Jira's and Linear's are printable ASCII
/// with no spaces, and `security -i` reads one command a line. Never repeats the value.
pub fn check_token(token: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err("Paste the token into the Token field".into());
    }
    if token.len() > 4096 || !token.chars().all(|c| c.is_ascii_graphic()) {
        return Err(
            "That doesn't look like a token: paste the token alone, with no spaces or line breaks"
                .into(),
        );
    }
    Ok(())
}

/// `s` in double quotes, as `security -i` splits its lines.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Keychain {
    /// This OS's keychain, through its program on PATH. Unit tests get a stub in their scratch
    /// folder instead (`stub`), so none touches a real keychain; without one, it isn't found.
    pub fn os() -> Self {
        let kind = if cfg!(target_os = "macos") { Kind::MacOs } else { Kind::SecretService };
        let name = match kind {
            Kind::MacOs => "security",
            Kind::SecretService => "secret-tool",
        };
        #[cfg(test)]
        let program = crate::store::scratch::dir().join("bin").join(name);
        #[cfg(not(test))]
        let program = PathBuf::from(name);
        Self { kind, program }
    }

    /// What the user calls it, after "your".
    pub fn label(&self) -> &'static str {
        match self.kind {
            Kind::MacOs => "macOS Keychain",
            Kind::SecretService => "keyring",
        }
    }

    /// Whether there's a keychain to save to: `security` answers, or `secret-tool` reaches a
    /// Secret Service. Without a desktop session (WSL, a server) it finds none and says so on
    /// stderr; an item that isn't there is only exit status 1. Asks for no unlocking. Runs a
    /// program, so not on the UI thread.
    pub fn probe(&self) -> bool {
        let out = match self.kind {
            Kind::MacOs => self.run(&["default-keychain"], None, PROBE_LIMIT),
            Kind::SecretService => {
                self.run(&["lookup", "service", SERVICE, "account", "probe"], None, PROBE_LIMIT)
            }
        };
        match out {
            Ok(o) if o.status.success() => true,
            Ok(o) => {
                self.kind == Kind::SecretService
                    && o.status.code() == Some(1)
                    && o.stderr.is_empty()
            }
            Err(_) => false,
        }
    }

    /// Saves `token` as item `name`, replacing one saved before, then reads it back to be sure the
    /// keychain kept it whole.
    pub fn store(&self, name: &str, token: &str) -> Result<(), String> {
        check_token(token)?;
        if !is_name(name) {
            return Err(format!("{name} can't name a keychain item"));
        }
        let label = format!("{SERVICE} {name}");
        let out = match self.kind {
            // `-w` with no value would ask on the terminal, and a value on the command line shows in
            // `ps`; `security -i` reads the whole command from stdin instead. `-U` updates an item
            // saved before.
            Kind::MacOs => {
                let line = format!(
                    "add-generic-password -U -s {} -a {} -l {} -w {}\n",
                    quote(SERVICE),
                    quote(name),
                    quote(&label),
                    quote(token)
                );
                self.run(&["-i"], Some(line.into_bytes()), LIMIT)
            }
            // Reads the secret from stdin when it isn't a terminal; an item with the same
            // attributes is replaced.
            Kind::SecretService => self.run(
                &["store", &format!("--label={label}"), "service", SERVICE, "account", name],
                Some(token.as_bytes().to_vec()),
                LIMIT,
            ),
        }
        .map_err(|e| self.failed("save the token to", &e))?;
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let failed = |why: &str| format!("couldn't save the token to your {}: {why}", self.label());
        if !out.status.success() {
            return Err(failed(&why));
        }
        // `security -i` may exit 0 when its command failed, so reading back is the test.
        match self.read(name) {
            Ok(read) if read == token => Ok(()),
            _ if !why.is_empty() => Err(failed(&why)),
            Ok(_) => Err(failed("it kept a different token")),
            Err(e) => Err(e),
        }
    }

    /// The token saved as item `name`.
    pub fn read(&self, name: &str) -> Result<String, String> {
        if !is_name(name) {
            return Err(format!("{name} can't name a keychain item"));
        }
        let out = match self.kind {
            Kind::MacOs => {
                self.run(&["find-generic-password", "-s", SERVICE, "-a", name, "-w"], None, LIMIT)
            }
            Kind::SecretService => {
                self.run(&["lookup", "service", SERVICE, "account", name], None, LIMIT)
            }
        }
        .map_err(|e| self.failed("read the token from", &e))?;
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // Not found: `security` exits 44, `secret-tool` 1 with nothing to say.
        let missing = match self.kind {
            Kind::MacOs => out.status.code() == Some(44),
            Kind::SecretService => stderr.is_empty(),
        };
        match () {
            _ if out.status.success() && !token.is_empty() => Ok(token),
            _ if missing || out.status.success() => Err(format!(
                "no token saved as {SERVICE} {name} in your {}; paste it in the account's settings",
                self.label()
            )),
            _ => Err(format!("couldn't read the token from your {}: {stderr}", self.label())),
        }
    }

    /// Deletes item `name`; one that's already gone is fine.
    pub fn delete(&self, name: &str) -> Result<(), String> {
        if !is_name(name) {
            return Err(format!("{name} can't name a keychain item"));
        }
        let out = match self.kind {
            Kind::MacOs => {
                self.run(&["delete-generic-password", "-s", SERVICE, "-a", name], None, LIMIT)
            }
            Kind::SecretService => {
                self.run(&["clear", "service", SERVICE, "account", name], None, LIMIT)
            }
        }
        .map_err(|e| self.failed("delete the token from", &e))?;
        if out.status.success() || (self.kind == Kind::MacOs && out.status.code() == Some(44)) {
            return Ok(());
        }
        Err(format!(
            "couldn't delete the token from your {}: {}",
            self.label(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }

    fn run(
        &self,
        args: &[&str],
        input: Option<Vec<u8>>,
        limit: Duration,
    ) -> std::io::Result<Output> {
        let cmd = &mut Command::new(&self.program);
        cmd.args(args);
        match input {
            Some(input) => crate::proc::run_with_input(cmd, input, limit),
            None => crate::proc::run(cmd, limit),
        }
    }

    /// Why running the program failed, for "couldn't `what` your keychain".
    fn failed(&self, what: &str, e: &std::io::Error) -> String {
        match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "couldn't {what} your {}: {} isn't installed",
                self.label(),
                self.program.display()
            ),
            std::io::ErrorKind::TimedOut => format!(
                "couldn't {what} your {}: no answer within {}s; is it locked?",
                self.label(),
                LIMIT.as_secs()
            ),
            _ => format!("couldn't {what} your {}: {e}", self.label()),
        }
    }
}

/// A stub keychain (tests/fixtures/keychain-stub.sh) as this OS's, where `Keychain::os` looks
/// in unit tests; returns its folder, where it keeps its items and logs its calls.
#[cfg(test)]
pub fn stub() -> PathBuf {
    let k = Keychain::os();
    write_stub(&k.program);
    k.program.parent().unwrap().to_path_buf()
}

/// A stub of the macOS Keychain, whatever the OS, for screens that name it.
#[cfg(test)]
pub fn macos_stub() -> Keychain {
    let program = crate::store::scratch::dir().join("bin-macos").join("security");
    write_stub(&program);
    Keychain { kind: Kind::MacOs, program }
}

#[cfg(test)]
fn write_stub(program: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    // Written aside and renamed, so no test runs it half written ("Text file busy").
    let tmp = program.with_extension("tmp");
    std::fs::write(&tmp, include_str!("../tests/fixtures/keychain-stub.sh")).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, program).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub of `kind` in this test's scratch folder.
    fn stubbed(kind: Kind) -> (Keychain, PathBuf) {
        let name = if kind == Kind::MacOs { "security" } else { "secret-tool" };
        let dir = crate::store::scratch::dir().join(format!("stub-{name}"));
        let program = dir.join(name);
        write_stub(&program);
        (Keychain { kind, program }, dir)
    }

    #[test]
    fn names_and_sources() {
        assert_eq!(source("jira-1"), "keychain:jira-1");
        assert_eq!(name_of("keychain:jira-1"), Some("jira-1"));
        assert_eq!(name_of(" keychain:linear-2 "), Some("linear-2"));
        for bad in ["keychain:", "keychain:a b", "keychain:a;rm", "env:X", "cmd:x", "jira-1"] {
            assert_eq!(name_of(bad), None, "{bad}");
        }
        assert!(!is_name(&"a".repeat(65)));
    }

    #[test]
    fn refuses_what_cannot_be_a_token_without_repeating_it() {
        assert!(check_token("ATATT3xFfGF0-abc_DEF=12AB34CD").is_ok());
        assert!(check_token("lin_api_0123456789abcdef").is_ok());
        assert_eq!(check_token("").unwrap_err(), "Paste the token into the Token field");
        for bad in ["two words", "line\nbreak", "tab\there", "ünïcode"] {
            let e = check_token(bad).unwrap_err();
            assert!(e.starts_with("That doesn't look like a token") && !e.contains(bad), "{e}");
        }
    }

    #[test]
    fn quotes_for_security_i() {
        assert_eq!(quote("plain"), "\"plain\"");
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn saves_reads_replaces_and_deletes_on_both() {
        for kind in [Kind::MacOs, Kind::SecretService] {
            let (k, dir) = stubbed(kind);
            assert!(k.probe(), "{kind:?}");
            let missing = k.read("jira-1").unwrap_err();
            assert!(missing.starts_with("no token saved as taskman jira-1 in your"), "{missing}");
            k.store("jira-1", "ATATT-first").unwrap();
            assert_eq!(k.read("jira-1").unwrap(), "ATATT-first");
            k.store("jira-1", "ATATT-second").unwrap();
            assert_eq!(k.read("jira-1").unwrap(), "ATATT-second");
            k.delete("jira-1").unwrap();
            assert!(k.read("jira-1").is_err());
            // Already gone is fine.
            k.delete("jira-1").unwrap();
            // The token was never an argument.
            let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
            assert!(!calls.contains("ATATT"), "{calls}");
        }
    }

    #[test]
    fn security_gets_the_whole_command_on_stdin() {
        let (k, dir) = stubbed(Kind::MacOs);
        k.store("linear-2", "lin_api_mock").unwrap();
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(calls.starts_with("security -i\n"), "{calls}");
        let stdin = std::fs::read_to_string(dir.join("stdin")).unwrap();
        assert_eq!(
            stdin,
            "stdin: add-generic-password -U -s \"taskman\" -a \"linear-2\" -l \"taskman linear-2\" -w \"lin_api_mock\"\n"
        );
    }

    #[test]
    fn says_why_when_there_is_no_keychain() {
        for kind in [Kind::MacOs, Kind::SecretService] {
            let (k, dir) = stubbed(kind);
            std::fs::write(dir.join("fail"), "Cannot autolaunch D-Bus without X11 $DISPLAY")
                .unwrap();
            assert!(!k.probe());
            let e = k.store("jira-1", "ATATT-mock").unwrap_err();
            assert!(e.contains("Cannot autolaunch D-Bus"), "{e}");
            let e = k.read("jira-1").unwrap_err();
            assert!(
                e.starts_with("couldn't read the token from your") && e.contains("D-Bus"),
                "{e}"
            );
            let gone = Keychain { kind, program: dir.join("missing") };
            assert!(!gone.probe());
            let e = gone.read("jira-1").unwrap_err();
            assert!(e.ends_with("missing isn't installed"), "{e}");
        }
    }
}
