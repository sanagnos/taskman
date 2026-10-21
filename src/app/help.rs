//! Settings' Help & feedback page: the guide, and the repository's issue forms, the bug form filled
//! in with what taskman knows of where it runs (`Machine`, looked up in the background the first
//! time the page shows). Over SSH, or with no browser, Enter copies the link and shows it.

use super::sync::Msg;
use super::{App, Field, Form, FormKind, Mode, SettingsItem};
use crate::store::Cli;
use crate::util;
use std::process::Command;
use std::time::Duration;

const REPO: &str = env!("CARGO_PKG_REPOSITORY");

/// What the bug form asks that taskman finds out itself, beyond its own version.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Machine {
    /// The coding CLIs' versions, as `Claude Code 2.1.3, Codex 0.50.0`; empty when none answered.
    pub cli: String,
    pub os: String,
    /// The terminal by name, when it says: `Windows Terminal`, `iTerm2 3.5.0`.
    pub terminal: Option<String>,
    /// The multiplexer taskman runs in: `tmux 3.4`, `GNU screen`.
    pub inside: Option<String>,
    /// Over SSH a browser would open on the far machine, so Enter copies the link instead.
    pub ssh: bool,
}

/// What the bug form gets filled in with.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub taskman: String,
    pub cli: String,
    pub os: String,
    pub terminal: String,
}

impl Report {
    /// The report for a terminal `size` columns by rows, with what's known of the machine.
    pub fn new(machine: Option<&Machine>, size: Option<(u16, u16)>) -> Self {
        let m = machine.cloned().unwrap_or_default();
        let size = size.map(|(w, h)| format!("{w}x{h}"));
        let inside = m.inside.map(|i| format!("inside {i}"));
        let terminal = [m.terminal, size, inside].into_iter().flatten().collect::<Vec<_>>();
        Report {
            taskman: format!("taskman {}", env!("CARGO_PKG_VERSION")),
            cli: m.cli,
            os: m.os,
            terminal: terminal.join(", "),
        }
    }

    /// The bug form's field ids and values, the empty ones left out.
    fn fields(&self) -> Vec<(&'static str, &str)> {
        let all = [
            ("taskman", &self.taskman),
            ("cli", &self.cli),
            ("os", &self.os),
            ("terminal", &self.terminal),
        ];
        all.into_iter().filter(|(_, v)| !v.is_empty()).map(|(id, v)| (id, v.as_str())).collect()
    }
}

pub fn guide_url() -> String {
    format!("{REPO}/blob/main/docs/guide.md")
}

/// The bug form, with `report`'s values in its fields: GitHub fills an issue form's text fields
/// from query parameters named by their ids.
pub fn bug_url(report: &Report) -> String {
    let mut url = format!("{REPO}/issues/new?template=bug.yml");
    for (id, value) in report.fields() {
        url.push_str(&format!("&{id}={}", encode(value)));
    }
    url
}

pub fn feature_url() -> String {
    format!("{REPO}/issues/new?template=feature.yml")
}

pub fn security_url() -> String {
    format!("{REPO}/security/advisories/new")
}

/// What the bug link's hint says it fills in; while `pending`, the lookup hasn't answered.
fn filled_in(report: &Report, pending: bool) -> String {
    if pending {
        return format!("fills in {}; looking up the CLI, OS and terminal…", report.taskman);
    }
    let terminal =
        Some(&report.terminal).filter(|t| !t.is_empty()).map(|t| format!("the terminal: {t}"));
    let known = [&report.taskman, &report.cli, &report.os].into_iter().filter(|v| !v.is_empty());
    let items: Vec<String> = known.cloned().chain(terminal).collect();
    match items.as_slice() {
        [rest @ .., last] if !rest.is_empty() => format!("fills in {} and {last}", rest.join(", ")),
        _ => format!("fills in {}", items.join("")),
    }
}

/// `s` as a query value: every byte but the unreserved characters percent-encoded.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The version in what `--version` printed: its first word that starts with a digit
/// (`2.1.3 (Claude Code)`, `codex-cli 0.50.0`).
fn parse_version(out: &str) -> Option<String> {
    let line = out.lines().next()?;
    let word = line.split_whitespace().find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    crate::untrusted::line(word).split(' ').next().map(String::from)
}

/// The OS from /etc/os-release's `PRETTY_NAME` (or `NAME`), as `Ubuntu 24.04.1 LTS`; under WSL,
/// as the bug form's placeholder has it.
fn linux_os(os_release: Option<&str>, wsl: bool) -> String {
    let field = |key: &str| {
        os_release?.lines().find_map(|l| {
            let value = l.trim().strip_prefix(key)?.strip_prefix('=')?;
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            Some(crate::untrusted::line(value)).filter(|v| !v.trim().is_empty())
        })
    };
    let distro = field("PRETTY_NAME").or_else(|| field("NAME"));
    match (wsl, distro) {
        (true, Some(d)) => format!("Windows with WSL ({d})"),
        (true, None) => "Windows with WSL".into(),
        (false, Some(d)) => d,
        (false, None) => "Linux".into(),
    }
}

/// The terminal's name and the multiplexer taskman runs in, from the environment. Inside tmux,
/// `TERM_PROGRAM` is tmux's own, so the terminal outside is told only by what it set before tmux
/// started (Windows Terminal's, kitty's or iTerm2's variables).
fn terminal(var: impl Fn(&str) -> Option<String>) -> (Option<String>, Option<String>) {
    let var = |v: &str| var(v).map(|s| crate::untrusted::line(&s)).filter(|s| !s.trim().is_empty());
    let program = var("TERM_PROGRAM");
    let with_version = |name: String, version: Option<String>| match version {
        Some(v) => format!("{name} {v}"),
        None => name,
    };
    let multiplexer = matches!(program.as_deref(), Some("tmux" | "screen"));
    let name = match program.as_deref() {
        Some(p) if !multiplexer => {
            let name = match p {
                "Apple_Terminal" => "Terminal.app",
                "iTerm.app" => "iTerm2",
                "vscode" => "VS Code",
                other => other,
            };
            Some(with_version(name.into(), var("TERM_PROGRAM_VERSION")))
        }
        _ if var("WT_SESSION").is_some() => Some("Windows Terminal".into()),
        _ if var("KITTY_WINDOW_ID").is_some() => Some("kitty".into()),
        _ if var("ALACRITTY_WINDOW_ID").is_some() => Some("Alacritty".into()),
        // iTerm2 sets these to get through ssh and tmux.
        _ => var("LC_TERMINAL").map(|t| with_version(t, var("LC_TERMINAL_VERSION"))),
    };
    let inside = if var("TMUX").is_some() {
        let version = (program.as_deref() == Some("tmux")).then(|| var("TERM_PROGRAM_VERSION"));
        Some(with_version("tmux".into(), version.flatten()))
    } else if var("STY").is_some() {
        Some("GNU screen".into())
    } else {
        None
    };
    (name, inside)
}

/// `cli`'s version, from its `--version`; None when it doesn't answer in time.
fn cli_version(cli: Cli) -> Option<String> {
    let mut cmd = Command::new(cli.program());
    cmd.arg("--version");
    let out = crate::proc::run(&mut cmd, Duration::from_secs(5)).ok()?;
    let version = parse_version(&String::from_utf8_lossy(&out.stdout))?;
    Some(format!("{} {version}", cli.product()))
}

fn os() -> String {
    if cfg!(target_os = "macos") {
        let mut cmd = Command::new("sw_vers");
        cmd.arg("-productVersion");
        let version = crate::proc::run(&mut cmd, Duration::from_secs(5))
            .ok()
            .and_then(|out| parse_version(&String::from_utf8_lossy(&out.stdout)));
        let chip = if cfg!(target_arch = "aarch64") { "Apple Silicon" } else { "Intel" };
        return match version {
            Some(v) => format!("macOS {v} ({chip})"),
            None => format!("macOS ({chip})"),
        };
    }
    let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists();
    linux_os(std::fs::read_to_string("/etc/os-release").ok().as_deref(), wsl)
}

/// Looks up the machine: `clis`' versions, the OS and the terminal. It runs commands, so not on
/// the UI thread.
fn find(clis: &[Cli]) -> Machine {
    let cli = clis.iter().filter_map(|c| cli_version(*c)).collect::<Vec<_>>().join(", ");
    let (terminal, inside) = terminal(|v| std::env::var(v).ok());
    let ssh = ["SSH_CONNECTION", "SSH_TTY"].iter().any(|v| std::env::var_os(v).is_some());
    Machine { cli, os: os(), terminal, inside, ssh }
}

impl App {
    /// Looks up the machine for the bug form, once, the first time the page shows.
    pub(super) fn ask_machine(&mut self) {
        if self.machine.is_some() || self.machine_asked {
            return;
        }
        self.machine_asked = true;
        // The CLIs the sessions run; Claude Code when there are none yet.
        let mut used: Vec<Cli> =
            Cli::ALL.into_iter().filter(|c| self.tickets.iter().any(|tv| tv.t.cli == *c)).collect();
        if used.is_empty() {
            used.push(Cli::default());
        }
        self.reporter().spawn(
            "looking up the versions for a bug report".into(),
            move || Msg::Machine(find(&used)),
            |_| Msg::Machine(Machine::default()),
        );
    }

    pub(super) fn machine_found(&mut self, machine: Machine) {
        self.machine = Some(machine);
        let Mode::Settings(s) = &self.mode else { return };
        if s.items[s.cursor] != SettingsItem::Help {
            return;
        }
        let mut form = self.help_form();
        if let Mode::Settings(s) = &mut self.mode {
            form.focus = s.form.focus;
            s.form = form;
        }
    }

    /// The terminal's size, as last drawn.
    fn screen_size(&self) -> Option<(u16, u16)> {
        let f = self.areas.footer;
        (f.width > 0).then_some((f.width, f.y + f.height))
    }

    pub(super) fn help_form(&self) -> Box<Form> {
        let report = Report::new(self.machine.as_ref(), self.screen_size());
        let bug_hint = filled_in(&report, self.machine.is_none());
        let link = |label, value: &str, hint: String, url: String| Field {
            url: Some(url),
            ..Field::choice(label, vec![value.to_string()], value, hint)
        };
        let fields = vec![
            link(
                "Guide",
                "Read the guide",
                "setting up, the keys, worktrees, agents and the commands".into(),
                guide_url(),
            ),
            link("Bug", "Report a bug", bug_hint, bug_url(&report)),
            link(
                "Feature",
                "Request a feature",
                "what you're trying to do, and what would help".into(),
                feature_url(),
            ),
            link(
                "Security",
                "Report it privately",
                "a security advisory, seen only by the maintainers".into(),
                security_url(),
            ),
        ];
        Box::new(Form::new(FormKind::Help, "Help & feedback", fields))
    }

    /// Opens a form field's link (Enter), or copies it (`c`, or where no browser opens here) and
    /// shows it under the field.
    pub(super) fn open_link(&mut self, url: String, copy: bool) {
        let ssh = self.machine.as_ref().is_some_and(|m| m.ssh);
        if !copy && !ssh && (self.open_url)(&url) {
            self.toast(format!("Opened {}", util::truncate(&url, 80)));
            return;
        }
        let why = match (copy, ssh) {
            (true, _) => None,
            (false, true) => Some("Over SSH a browser would open on the other machine"),
            (false, false) => Some("No browser found (set $BROWSER)"),
        };
        if let Mode::Settings(s) = &mut self.mode {
            let field = &mut s.form.fields[s.form.focus];
            field.links = vec![(String::new(), url.clone(), url.clone())];
            s.form.error = why.map(|w| format!("{w}: the link is copied, and shown above."));
        }
        self.copy_as(url, Some("the link".into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn builds_the_links_from_the_repository() {
        assert_eq!(guide_url(), format!("{REPO}/blob/main/docs/guide.md"));
        assert_eq!(feature_url(), format!("{REPO}/issues/new?template=feature.yml"));
        assert_eq!(security_url(), format!("{REPO}/security/advisories/new"));
    }

    #[test]
    fn fills_in_the_bug_form_encoded() {
        let machine = Machine {
            cli: "Claude Code 2.1.3, Codex 0.50.0".into(),
            os: "Windows with WSL (Ubuntu 24.04.1 LTS)".into(),
            terminal: Some("Windows Terminal".into()),
            inside: Some("tmux 3.4".into()),
            ssh: false,
        };
        let report = Report::new(Some(&machine), Some((160, 45)));
        assert_eq!(report.terminal, "Windows Terminal, 160x45, inside tmux 3.4");
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            bug_url(&report),
            format!(
                "{REPO}/issues/new?template=bug.yml&taskman=taskman%20{version}\
                 &cli=Claude%20Code%202.1.3%2C%20Codex%200.50.0\
                 &os=Windows%20with%20WSL%20%28Ubuntu%2024.04.1%20LTS%29\
                 &terminal=Windows%20Terminal%2C%20160x45%2C%20inside%20tmux%203.4"
            )
        );
        // What isn't known is left for the user to fill in.
        let report = Report::new(None, None);
        assert_eq!(
            bug_url(&report),
            format!("{REPO}/issues/new?template=bug.yml&taskman=taskman%20{version}")
        );
    }

    #[test]
    fn says_what_the_bug_link_fills_in() {
        let report = |cli: &str, os: &str, terminal: &str| Report {
            taskman: "taskman 0.1.24".into(),
            cli: cli.into(),
            os: os.into(),
            terminal: terminal.into(),
        };
        let all = report("Claude Code 2.1.3", "Ubuntu 24.04.1 LTS", "160x45, inside tmux 3.4");
        assert_eq!(
            filled_in(&all, false),
            "fills in taskman 0.1.24, Claude Code 2.1.3, Ubuntu 24.04.1 LTS and the terminal: \
             160x45, inside tmux 3.4"
        );
        assert_eq!(
            filled_in(&all, true),
            "fills in taskman 0.1.24; looking up the CLI, OS and terminal…"
        );
        assert_eq!(
            filled_in(&report("", "macOS 15.5", ""), false),
            "fills in taskman 0.1.24 and macOS 15.5"
        );
        assert_eq!(filled_in(&report("", "", ""), false), "fills in taskman 0.1.24");
    }

    #[test]
    fn encodes_all_but_unreserved_characters() {
        assert_eq!(encode("a-Z_0.9~"), "a-Z_0.9~");
        assert_eq!(encode("a b&c=d#e?f/g+h%"), "a%20b%26c%3Dd%23e%3Ff%2Fg%2Bh%25");
        assert_eq!(encode("é ✦"), "%C3%A9%20%E2%9C%A6");
    }

    #[test]
    fn reads_versions_as_the_clis_print_them() {
        assert_eq!(parse_version("2.1.3 (Claude Code)\n").as_deref(), Some("2.1.3"));
        assert_eq!(parse_version("codex-cli 0.50.0\n").as_deref(), Some("0.50.0"));
        assert_eq!(parse_version("15.5\n").as_deref(), Some("15.5"));
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn names_the_linux_distribution() {
        let ubuntu = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n";
        assert_eq!(linux_os(Some(ubuntu), false), "Ubuntu 24.04.1 LTS");
        assert_eq!(linux_os(Some(ubuntu), true), "Windows with WSL (Ubuntu 24.04.1 LTS)");
        assert_eq!(linux_os(Some("NAME=Alpine Linux\n"), false), "Alpine Linux");
        assert_eq!(linux_os(Some("PRETTY_NAME=''\nNAME='Arch'\n"), false), "Arch");
        assert_eq!(linux_os(None, false), "Linux");
        assert_eq!(linux_os(Some("ID=nixos"), true), "Windows with WSL");
    }

    #[test]
    fn names_the_terminal_and_the_multiplexer() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |v: &str| pairs.iter().find(|(k, _)| *k == v).map(|(_, x)| x.to_string())
        };
        assert_eq!(
            terminal(env(&[("TERM_PROGRAM", "iTerm.app"), ("TERM_PROGRAM_VERSION", "3.5.0")])),
            (Some("iTerm2 3.5.0".into()), None)
        );
        assert_eq!(
            terminal(env(&[
                ("TERM_PROGRAM", "tmux"),
                ("TERM_PROGRAM_VERSION", "3.4"),
                ("TMUX", "/tmp/tmux-1000/default,1,0"),
                ("WT_SESSION", "0f6f"),
            ])),
            (Some("Windows Terminal".into()), Some("tmux 3.4".into()))
        );
        assert_eq!(
            terminal(env(&[("TERM_PROGRAM", "tmux"), ("TMUX", "x"), ("LC_TERMINAL", "iTerm2")])),
            (Some("iTerm2".into()), Some("tmux".into()))
        );
        assert_eq!(
            terminal(env(&[("STY", "1234.pts-0"), ("KITTY_WINDOW_ID", "1")])).1.as_deref(),
            Some("GNU screen")
        );
        assert_eq!(terminal(env(&[("TERM", "xterm-256color")])), (None, None));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn encoded_values_stay_one_query_value(s in any::<String>()) {
            let e = encode(&s);
            prop_assert!(e.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b)));
        }

        #[test]
        fn os_release_reads_as_one_clean_line(text in any::<String>(), wsl in any::<bool>()) {
            let os = linux_os(Some(&text), wsl);
            prop_assert!(!os.is_empty());
            prop_assert!(!os.chars().any(char::is_control));
        }

        #[test]
        fn versions_are_one_clean_word(text in any::<String>()) {
            if let Some(v) = parse_version(&text) {
                prop_assert!(v.starts_with(|c: char| c.is_ascii_digit()));
                prop_assert!(!v.chars().any(|c| c.is_whitespace() || c.is_control()));
            }
        }
    }
}
