//! Each difference between the coding CLIs a session can run (`store::Cli`) as a method on `Cli`, as
//! `tracker.rs` has them for trackers: how it starts, where it keeps its transcripts, which keys
//! answer its prompts, and what its sessions can do.

use crate::hook;
use crate::store::{self, Cli};
use crossterm::event::KeyCode;
use std::path::{Path, PathBuf};

/// A fork's first launch (`Cli::fork_args`): the conversation it copies, and what it starts with.
pub struct ForkStart<'a> {
    /// The source's conversation id.
    pub source: &'a str,
    /// The fork's own id, for a CLI that takes one up front (`Cli::chooses_fork_id`).
    pub id: Option<&'a str>,
    /// Its first message, which tells it who it is now.
    pub message: &'a str,
    pub cwd: &'a Path,
    /// Record no system prompt, so the fork's own brief replaces the one the source recorded
    /// (`Ticket::prompt_snapshot_off`).
    pub snapshot_off: bool,
}

/// What the installed CLI says about forking (`Cli::probe_fork`).
#[derive(Clone, Debug, PartialEq)]
pub struct ForkProbe {
    /// None when it didn't answer in time.
    pub version: Option<String>,
    /// Whether it can be told not to record the system prompt (`ForkStart::snapshot_off`).
    pub snapshot_off: bool,
}

/// Who the screen shows answering an approval request (`Cli::approver`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approver {
    /// Codex's automatic reviewer.
    Reviewer,
    User,
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

    /// Whether it's the one a session runs unless told otherwise, which tickets.json leaves out
    /// and the pane's label doesn't name.
    pub fn is_default(&self) -> bool {
        *self == Cli::default()
    }

    /// What the user installs, as an error that it's missing names it.
    pub fn product(self) -> &'static str {
        match self {
            Cli::Claude => "Claude Code",
            Cli::Codex => "Codex",
        }
    }

    /// The variable that names another command to run for it.
    pub fn program_var(self) -> &'static str {
        match self {
            Cli::Claude => "TASKMAN_CLAUDE",
            Cli::Codex => "TASKMAN_CODEX",
        }
    }

    /// The command a session runs: `program_var`'s, else the CLI's own.
    pub fn program(self) -> String {
        let default = match self {
            Cli::Claude => "claude",
            Cli::Codex => "codex",
        };
        std::env::var(self.program_var()).unwrap_or_else(|_| default.into())
    }

    /// Whether its sessions message each other, and other sessions can message them: Claude
    /// Code's ListAgents and SendMessage, by the name a session starts with.
    pub fn messages_agents(self) -> bool {
        match self {
            Cli::Claude => true,
            Cli::Codex => false,
        }
    }

    /// Whether it runs commands in a sandbox that blocks network access and writes outside the
    /// session's folder, so a brief says which `taskman` commands to ask to run outside it.
    pub fn sandboxed(self) -> bool {
        match self {
            Cli::Claude => false,
            Cli::Codex => true,
        }
    }

    /// Whether a conversation can be running as one of its background sessions (`claude --bg`),
    /// so a resume refuses and names the job (`background`).
    pub fn runs_in_background(self) -> bool {
        match self {
            Cli::Claude => true,
            Cli::Codex => false,
        }
    }

    /// Whether it has the race that switches a running session onto the conversation of another
    /// started in the same folder (see `session_ids`), so a conflict is its bug to report.
    pub fn switches_conversations(self) -> bool {
        match self {
            Cli::Claude => true,
            Cli::Codex => false,
        }
    }

    /// Whether `code`, unmodified, answers the permission prompt it shows (Enter and the
    /// interrupts do for every CLI).
    pub fn answers_prompt(self, code: KeyCode) -> bool {
        match self {
            // Claude's options are numbered and a digit picks one at once.
            Cli::Claude => matches!(code, KeyCode::Char('1'..='9')),
            // Codex's yes and no. Its other shortcuts (p, a) and digits are on some prompts
            // only, and one that isn't an option does nothing; Codex redraws its prompt every
            // second, so the quiet check couldn't undo the switch. Those answers show when the
            // next hook fires.
            Cli::Codex => matches!(code, KeyCode::Char('y' | 'n')),
        }
    }

    /// Whether `code` opens a field for adding to the prompt's answer, where digits and letters
    /// are text: Claude's Tab.
    pub fn amends_prompt(self, code: KeyCode) -> bool {
        match self {
            Cli::Claude => code == KeyCode::Tab,
            Cli::Codex => false,
        }
    }

    /// Whether something other than the user may answer the approval request a PermissionRequest
    /// hook reports: Codex's automatic reviewer (`approvals_reviewer = "auto_review"`, or
    /// "Approve for me" in `/permissions`) gets each request after the hook has run. Claude has
    /// no reviewer.
    pub fn reviews_approvals(self) -> bool {
        match self {
            Cli::Claude => false,
            Cli::Codex => true,
        }
    }

    /// Who answers the approval request on `screen` (`Status::review`): Codex's reviewer while
    /// its status line reads "Reviewing approval request" (or "Reviewing 2 approval requests"),
    /// the user while Codex's prompt shows, which goes first, as it waits on the user whatever
    /// else is under review. None while neither shows.
    pub fn approver(self, screen: &str) -> Option<Approver> {
        match self {
            Cli::Claude => None,
            Cli::Codex => {
                // The prompt's footer; Codex's other lists say "to go back".
                let prompt = |l: &str| l.contains(" to confirm or ") && l.contains(" to cancel");
                let reviewing = |l: &str| {
                    l.split_once("Reviewing ").is_some_and(|(_, rest)| {
                        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
                        rest.trim_start().starts_with("approval request")
                    })
                };
                if screen.lines().any(prompt) {
                    Some(Approver::User)
                } else if screen.lines().any(reviewing) {
                    Some(Approver::Reviewer)
                } else {
                    None
                }
            }
        }
    }

    /// Whether `screen` shows the CLI asking whether to trust the session's folder, which it asks
    /// in a folder it hasn't been told to trust, before any hook runs (`Status::trust`). Its
    /// options tell it: Claude's "Yes, I trust this folder" (earlier versions asked "Do you
    /// trust the files in this folder?"), Codex's "Trust and continue".
    pub fn asks_trust(self, screen: &str) -> bool {
        let asks = |needles: &[&str]| screen.lines().any(|l| needles.iter().any(|n| l.contains(n)));
        match self {
            Cli::Claude => {
                asks(&["Yes, I trust this folder", "Do you trust the files in this folder?"])
            }
            Cli::Codex => asks(&["1. Trust and continue"]),
        }
    }

    /// The key that shows the whole conversation, rows the pane's history has dropped included:
    /// Claude's transcript mode, Codex's transcript view. The default of each; both can rebind it.
    pub fn transcript_key(self) -> &'static str {
        match self {
            Cli::Claude => "ctrl+o",
            Cli::Codex => "ctrl+t",
        }
    }

    /// What a session is started with to report its state through `taskman hook`, run as `exe`:
    /// the `--settings` JSON for Claude, with the allow-list; the `-c` overrides for Codex.
    pub fn hooks(self, exe: &Path) -> Vec<String> {
        match self {
            Cli::Claude => vec![hook::settings_json(exe)],
            Cli::Codex => hook::codex_config(exe),
        }
    }

    /// Arguments for a session named `name`, with `brief` as its instructions, resuming
    /// conversation `resume`, and `hooks` from `Cli::hooks`.
    pub fn args(
        self,
        name: &str,
        brief: &str,
        resume: Option<&str>,
        hooks: &[String],
    ) -> Vec<String> {
        let Some(sid) = resume else { return self.with(name, brief, hooks, vec![], vec![]) };
        match self {
            Cli::Claude => {
                self.with(name, brief, hooks, vec!["--resume".into(), sid.into()], vec![])
            }
            Cli::Codex => self.with(name, brief, hooks, vec![], vec!["resume".into(), sid.into()]),
        }
    }

    /// Arguments for a fork's first launch (`ForkStart`), otherwise as `args`.
    pub fn fork_args(
        self,
        name: &str,
        brief: &str,
        fork: &ForkStart,
        hooks: &[String],
    ) -> Vec<String> {
        match self {
            Cli::Claude => {
                let mut before: Vec<String> =
                    vec!["--resume".into(), fork.source.into(), "--fork-session".into()];
                if let Some(id) = fork.id {
                    before.extend(["--session-id".into(), id.into()]);
                }
                if fork.snapshot_off {
                    before.extend(self.no_prompt_snapshot());
                }
                self.with(name, brief, hooks, before, vec![fork.message.into()])
            }
            // Without -C, a fork in another folder than the source's stops at a question about
            // which folder to work in.
            Cli::Codex => {
                let after = vec![
                    "fork".into(),
                    fork.source.into(),
                    "-C".into(),
                    fork.cwd.to_string_lossy().into_owned(),
                    fork.message.into(),
                ];
                self.with(name, brief, hooks, vec![], after)
            }
        }
    }

    /// The arguments every launch has, between `before` and `after`.
    fn with(
        self,
        name: &str,
        brief: &str,
        hooks: &[String],
        before: Vec<String>,
        after: Vec<String>,
    ) -> Vec<String> {
        let mut args = before;
        match self {
            Cli::Claude => {
                for h in hooks {
                    args.extend(["--settings".into(), h.clone()]);
                }
                // The name is how other Claude sessions address this one (ListAgents /
                // SendMessage).
                args.extend([
                    "--name".into(),
                    name.into(),
                    "--append-system-prompt".into(),
                    brief.into(),
                ]);
            }
            Cli::Codex => {
                // -c also makes Codex run on its own rather than on its shared background
                // server, so a session never asks to restart that server, which other sessions
                // may be using.
                for h in hooks {
                    args.extend(["-c".into(), h.clone()]);
                }
                args.extend([
                    "-c".into(),
                    format!("developer_instructions={}", serde_json::Value::from(brief)),
                ]);
            }
        }
        args.extend(after);
        args
    }

    /// Whether a fork's id can be chosen up front (Claude's `--session-id`), so taskman knows it
    /// before the fork's hooks report it. Codex makes its own, which the hooks then report.
    pub fn chooses_fork_id(self) -> bool {
        match self {
            Cli::Claude => true,
            Cli::Codex => false,
        }
    }

    /// The oldest version that forks as `fork_args` asks: Claude Code's `--session-id` with
    /// `--fork-session`, Codex's `fork` command.
    pub fn min_fork_version(self) -> &'static str {
        match self {
            Cli::Claude => "2.0.73",
            Cli::Codex => "0.81.0",
        }
    }

    /// Why the CLI, at `version`, can't fork; None when it can, or its version isn't known.
    pub fn cant_fork(self, version: Option<&str>) -> Option<String> {
        let min = self.min_fork_version();
        let older = matches!(
            (version.and_then(crate::update::parse), crate::update::parse(min)),
            (Some(v), Some(m)) if v < m
        );
        older.then(|| {
            format!(
                "Forking needs {} {min} or newer, and this is {}: update it, then fork again",
                self.product(),
                version.unwrap_or_default()
            )
        })
    }

    /// The option that turns off recording the system prompt (Claude's
    /// `--system-prompt-snapshot`), which would keep a fork on the source's recorded prompt.
    fn snapshot_option(self) -> Option<&'static str> {
        match self {
            Cli::Claude => Some("--system-prompt-snapshot"),
            Cli::Codex => None,
        }
    }

    /// Arguments that turn off recording the system prompt (`snapshot_option`); none for a CLI
    /// that doesn't record it.
    pub fn no_prompt_snapshot(self) -> Vec<String> {
        self.snapshot_option().map_or_else(Vec::new, |o| vec![o.into(), "off".into()])
    }

    /// The installed version, from `--version`; None when it doesn't answer in time. Runs a
    /// command, so not on the UI thread.
    pub fn version(self) -> Option<String> {
        let out = self.run_with("--version")?;
        crate::util::parse_version(&out)
    }

    /// What the installed CLI says about forking. Runs commands, so not on the UI thread.
    pub fn probe_fork(self) -> ForkProbe {
        // Read from its help rather than guessed from a version: when the option came isn't known.
        let snapshot_off = self
            .snapshot_option()
            .is_some_and(|o| self.run_with("--help").is_some_and(|help| help.contains(o)));
        ForkProbe { version: self.version(), snapshot_off }
    }

    /// What the CLI prints for `arg`; None when it doesn't answer in time.
    fn run_with(self, arg: &str) -> Option<String> {
        let mut cmd = std::process::Command::new(self.program());
        cmd.arg(arg);
        let out = crate::proc::run(&mut cmd, std::time::Duration::from_secs(5)).ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Where it keeps its transcripts. Unit tests get a folder in their state folder, so they can
    /// make transcripts and never read the user's, even with `CLAUDE_CONFIG_DIR` set.
    pub fn home(self) -> PathBuf {
        let (var, dir) = match self {
            Cli::Claude => ("CLAUDE_CONFIG_DIR", ".claude"),
            Cli::Codex => ("CODEX_HOME", ".codex"),
        };
        if cfg!(test) {
            return store::state_dir().join(dir);
        }
        std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| store::home().join(dir))
    }

    /// Whether it still has the transcript for conversation `sid`, so resuming will work.
    pub fn has_conversation(self, sid: &str) -> bool {
        let base = self.home();
        match self {
            Cli::Claude => {
                let Ok(dirs) = std::fs::read_dir(base.join("projects")) else { return false };
                dirs.flatten().any(|d| d.path().join(format!("{sid}.jsonl")).exists())
            }
            // Codex keeps sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl.
            Cli::Codex => {
                let suffix = format!("-{sid}.jsonl");
                let mut dirs = vec![base.join("sessions")];
                for _ in 0..3 {
                    dirs = dirs
                        .iter()
                        .filter_map(|d| std::fs::read_dir(d).ok())
                        .flatten()
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.is_dir())
                        .collect();
                }
                dirs.iter()
                    .filter_map(|d| std::fs::read_dir(d).ok())
                    .flatten()
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().ends_with(&suffix))
            }
        }
    }

    /// Leaves a transcript for conversation `sid` where it keeps them (`home`), for a project at
    /// /src/acme.
    #[cfg(test)]
    pub fn leave_transcript(self, sid: &str) {
        let path = match self {
            Cli::Claude => self.home().join(format!("projects/-src-acme/{sid}.jsonl")),
            Cli::Codex => self
                .home()
                .join(format!("sessions/2026/10/09/rollout-2026-10-09T10-00-00-{sid}.jsonl")),
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}\n").unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn labels_round_trip_and_claude_is_the_default() {
        for cli in Cli::ALL {
            assert_eq!(Cli::from_label(cli.label()), cli);
        }
        assert_eq!(Cli::from_label("Gemini"), Cli::Claude);
        assert!(Cli::Claude.is_default() && !Cli::Codex.is_default());
        assert_eq!((Cli::Claude.product(), Cli::Codex.product()), ("Claude Code", "Codex"));
    }

    #[test]
    fn each_runs_its_own_program() {
        for (cli, var, default) in
            [(Cli::Claude, "TASKMAN_CLAUDE", "claude"), (Cli::Codex, "TASKMAN_CODEX", "codex")]
        {
            assert_eq!(cli.program_var(), var);
            assert_eq!(cli.program(), std::env::var(var).unwrap_or_else(|_| default.into()));
        }
    }

    #[test]
    fn only_claude_messages_runs_in_the_background_and_switches_conversations() {
        assert!(Cli::Claude.messages_agents() && !Cli::Codex.messages_agents());
        assert!(Cli::Claude.runs_in_background() && !Cli::Codex.runs_in_background());
        assert!(Cli::Claude.switches_conversations() && !Cli::Codex.switches_conversations());
    }

    #[test]
    fn only_codex_is_sandboxed() {
        assert!(!Cli::Claude.sandboxed() && Cli::Codex.sandboxed());
    }

    #[test]
    fn each_answers_its_prompts_with_its_own_keys() {
        for c in '1'..='9' {
            assert!(Cli::Claude.answers_prompt(KeyCode::Char(c)), "{c}");
            assert!(!Cli::Codex.answers_prompt(KeyCode::Char(c)), "{c}");
        }
        for c in ['y', 'n'] {
            assert!(Cli::Codex.answers_prompt(KeyCode::Char(c)), "{c}");
            assert!(!Cli::Claude.answers_prompt(KeyCode::Char(c)), "{c}");
        }
        for cli in Cli::ALL {
            for code in [KeyCode::Char('0'), KeyCode::Char('p'), KeyCode::Up, KeyCode::Tab] {
                assert!(!cli.answers_prompt(code), "{cli:?} {code:?}");
            }
        }
        assert!(Cli::Claude.amends_prompt(KeyCode::Tab));
        assert!(!Cli::Claude.amends_prompt(KeyCode::Char('1')));
        assert!(!Cli::Codex.amends_prompt(KeyCode::Tab));
    }

    #[test]
    fn codex_shows_who_answers_an_approval() {
        let codex = |screen: &str| Cli::Codex.approver(screen);
        // The status line as Codex draws it while its reviewer decides (the timer and hint
        // after the header), for one request and for several.
        assert_eq!(
            codex(
                "› Run the tests\n\n• Reviewing approval request (4s • esc to interrupt)\n  └ cargo test\n"
            ),
            Some(Approver::Reviewer)
        );
        assert_eq!(
            codex("• Reviewing 2 approval requests (1s • esc to interrupt)\n  • cargo test\n"),
            Some(Approver::Reviewer)
        );
        // Codex's prompt, which also goes first while another request is under review.
        let prompt = "  Would you like to run the following command?\n\n  $ cargo test\n\n› 1. Yes, proceed (y)\n  2. No, and tell Codex what to do differently (esc)\n\n  Press enter to confirm or esc to cancel\n";
        assert_eq!(codex(prompt), Some(Approver::User));
        assert_eq!(
            codex(&format!("• Reviewing 2 approval requests (3s)\n{prompt}")),
            Some(Approver::User)
        );
        // Neither: working, Codex's other lists, a review's outcome in the history.
        for screen in [
            "• Working (3s • esc to interrupt)\n",
            "  Select Model\n  Press enter to confirm or esc to go back\n",
            "✗ Request denied for codex to run cargo test\n",
            "• Reviewing the diff (2s • esc to interrupt)\n",
            "",
        ] {
            assert_eq!(codex(screen), None, "{screen}");
        }
        // Claude has no reviewer, and its prompts are told by its hooks.
        assert!(Cli::Codex.reviews_approvals() && !Cli::Claude.reviews_approvals());
        assert_eq!(Cli::Claude.approver(prompt), None);
    }

    #[test]
    fn each_shows_when_it_asks_to_trust_its_folder() {
        // As Claude Code 2.1.308 and Codex 0.162 draw the question in a folder never trusted.
        let claude = " Accessing workspace:\n\n /src/acme\n\n Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source\n project, or work from your team). If not, take a moment to review what's in this folder first.\n\n Claude Code'll be able to read, edit, and execute files here.\n\n Security guide\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n";
        let codex = "  Folder access\n  /src/acme\n\n  Trust this folder? Codex can read, edit, and run files here, subject to your permission settings. Folder settings\n  can run code automatically, even without a model request. Continue only if you trust these files. Your trust\n  decision will be saved.\n\n› 1. Trust and continue\n  2. Quit\n\n  enter continue · esc quit\n";
        let older = " Do you trust the files in this folder?\n\n /src/acme\n\n ❯ 1. Yes, proceed\n   2. No, exit\n";
        assert!(Cli::Claude.asks_trust(claude) && Cli::Claude.asks_trust(older));
        assert!(Cli::Codex.asks_trust(codex));
        // Each its own question only, and not once it's answered: the input, or a conversation
        // that mentions trust.
        assert!(!Cli::Claude.asks_trust(codex) && !Cli::Codex.asks_trust(claude));
        for screen in [
            "✻ Welcome to Claude Code\n\n> ",
            "> do you trust this folder?\n\n● Yes, it's your own repository.\n",
            "› Explain this codebase\n",
            "",
        ] {
            assert!(!Cli::Claude.asks_trust(screen) && !Cli::Codex.asks_trust(screen), "{screen}");
        }
    }

    #[test]
    fn claude_gets_its_hooks_as_settings_and_codex_as_overrides() {
        let exe = Path::new("/opt/taskman");
        let claude = Cli::Claude.hooks(exe);
        assert_eq!(claude, [hook::settings_json(exe)]);
        let settings: Value = serde_json::from_str(&claude[0]).unwrap();
        assert!(settings["hooks"].is_object() && settings["permissions"].is_object());
        let codex = Cli::Codex.hooks(exe);
        assert_eq!(codex, hook::codex_config(exe));
        assert!(codex.iter().all(|h| h.starts_with("hooks.")), "{codex:?}");
    }

    #[test]
    fn builds_each_clis_arguments() {
        let claude = Cli::Claude.args("ENG-52", "Brief", Some("abc"), &["{}".into()]);
        assert_eq!(
            claude,
            [
                "--resume",
                "abc",
                "--settings",
                "{}",
                "--name",
                "ENG-52",
                "--append-system-prompt",
                "Brief"
            ]
        );
        assert_eq!(
            Cli::Claude.args("ENG-52", "Brief", None, &["{}".into()])[..2],
            ["--settings", "{}"]
        );

        let codex = Cli::Codex.args(
            "ENG-52",
            "Say \"hi\"\nthen stop",
            Some("abc"),
            &["hooks.Stop=[]".into()],
        );
        assert_eq!(
            codex,
            [
                "-c",
                "hooks.Stop=[]",
                "-c",
                r#"developer_instructions="Say \"hi\"\nthen stop""#,
                "resume",
                "abc"
            ]
        );
        assert!(!Cli::Codex.args("ENG-52", "Brief", None, &[]).contains(&"resume".to_string()));
    }

    #[test]
    fn builds_each_clis_fork_arguments() {
        let fork = |id, snapshot_off| ForkStart {
            source: "sid-src",
            id,
            message: "You're now ✦ tester",
            cwd: Path::new("/src/acme"),
            snapshot_off,
        };
        // Claude forks with the id taskman chose, and the source's recorded prompt left behind;
        // the first message is the prompt, last.
        assert_eq!(
            Cli::Claude.fork_args(
                "acme-tester",
                "Brief",
                &fork(Some("sid-new"), true),
                &["{}".into()]
            ),
            [
                "--resume",
                "sid-src",
                "--fork-session",
                "--session-id",
                "sid-new",
                "--system-prompt-snapshot",
                "off",
                "--settings",
                "{}",
                "--name",
                "acme-tester",
                "--append-system-prompt",
                "Brief",
                "You're now ✦ tester"
            ]
        );
        // A Claude that can't be told so gets neither option.
        let older = Cli::Claude.fork_args("acme-tester", "Brief", &fork(None, false), &[]);
        assert_eq!(older[..3], ["--resume", "sid-src", "--fork-session"]);
        assert!(!older.iter().any(|a| a == "--session-id" || a == "--system-prompt-snapshot"));
        // Codex's fork command, in the agent's folder, after the overrides.
        assert_eq!(
            Cli::Codex.fork_args(
                "acme-tester",
                "Brief",
                &fork(None, false),
                &["hooks.Stop=[]".into()]
            ),
            [
                "-c",
                "hooks.Stop=[]",
                "-c",
                r#"developer_instructions="Brief""#,
                "fork",
                "sid-src",
                "-C",
                "/src/acme",
                "You're now ✦ tester"
            ]
        );
        assert!(Cli::Claude.chooses_fork_id() && !Cli::Codex.chooses_fork_id());
    }

    #[test]
    fn says_when_a_cli_is_too_old_to_fork() {
        assert_eq!(
            Cli::Claude.cant_fork(Some("2.0.72")).as_deref(),
            Some(
                "Forking needs Claude Code 2.0.73 or newer, and this is 2.0.72: update it, then fork again"
            )
        );
        assert_eq!(Cli::Claude.cant_fork(Some("2.0.73")), None);
        assert_eq!(Cli::Claude.cant_fork(Some("2.1.314")), None);
        assert!(Cli::Codex.cant_fork(Some("0.80.9")).unwrap().contains("Codex 0.81.0 or newer"));
        assert_eq!(Cli::Codex.cant_fork(Some("0.166.0")), None);
        // Not known: it's tried.
        assert_eq!(Cli::Claude.cant_fork(None), None);
        assert_eq!(Cli::Claude.cant_fork(Some("2.1")), None);
    }

    #[test]
    fn each_finds_only_its_own_transcripts() {
        assert_ne!(Cli::Claude.home(), Cli::Codex.home());
        for cli in Cli::ALL {
            assert!(!cli.has_conversation("sid-1"), "{cli:?}");
        }
        Cli::Claude.leave_transcript("sid-1");
        assert!(Cli::Claude.has_conversation("sid-1"));
        assert!(!Cli::Codex.has_conversation("sid-1"), "each CLI has its own transcripts");
        // Codex keeps its transcripts in dated folders, named by the id after a dash.
        Cli::Codex.leave_transcript("sid-2");
        assert!(Cli::Codex.has_conversation("sid-2"));
        assert!(!Cli::Codex.has_conversation("d-2"), "only the whole id");
        assert!(!Cli::Claude.has_conversation("sid-2"));
    }
}
