//! Each difference between the coding CLIs a session can run (`store::Cli`) as a method on `Cli`, as
//! `tracker.rs` has them for trackers: how it starts, where it keeps its transcripts, which keys
//! answer its prompts, and what its sessions can do.

use crate::hook;
use crate::store::{self, Cli};
use crossterm::event::KeyCode;
use std::path::{Path, PathBuf};

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
        let mut args: Vec<String> = Vec::new();
        match self {
            Cli::Claude => {
                if let Some(sid) = resume {
                    args.extend(["--resume".into(), sid.into()]);
                }
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
                if let Some(sid) = resume {
                    args.extend(["resume".into(), sid.into()]);
                }
            }
        }
        args
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
