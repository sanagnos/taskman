//! A conversation that Claude Code runs as one of its background sessions (`claude --bg`): a
//! resume refuses and exits at once, naming the job. The pane keeps that message and offers to
//! stop the background copy (`claude stop <id>`), then resumes as usual.
//!
//! The screen it is read from is the CLI's output, so only a short id-shaped token is taken from
//! it, and that token is the only part of it that reaches a command (as one argument, with no
//! shell).

use std::process::Command;
use std::time::Duration;

/// Claude's wording, matched loosely: this phrase, then the job's id in parentheses.
const PHRASE: &str = "running in the background";

/// How far after the phrase the id's parenthesis may be.
const REACH: usize = 50;

/// How long `claude stop` may take.
const STOP_TIMEOUT: Duration = Duration::from_secs(20);

/// The job id the ended `screen` names, if it says the session runs in the background. An id
/// starts with a letter or digit (so it can't pass for an option) and is 4 to 40 letters, digits,
/// `-` or `_`.
pub fn job_id(screen: &str) -> Option<String> {
    let text = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = text.to_ascii_lowercase();
    // The last mention: what the CLI said last is what it exited on.
    let at = lower.rfind(PHRASE)? + PHRASE.len();
    let rest = text.get(at..)?;
    let window: String = rest.chars().take(REACH).collect();
    let inner = window.strip_prefix(" (").or_else(|| window.strip_prefix('('))?;
    let id = inner.split(')').next().filter(|_| inner.contains(')'))?.trim();
    let ok = (4..=40).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| id.to_string())
}

/// The lines of `screen` to keep showing: the last few that have text, cleaned of control
/// characters.
pub fn message(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let from = lines.len().saturating_sub(6);
    crate::untrusted::clean(&lines[from..].join("\n"))
}

/// `claude stop <id>`, to the end. The error is what to tell the user.
pub fn stop(id: &str) -> Result<(), String> {
    let program = std::env::var("TASKMAN_CLAUDE").unwrap_or_else(|_| "claude".into());
    let out = crate::proc::run(Command::new(&program).arg("stop").arg(id), STOP_TIMEOUT)
        .map_err(|e| format!("couldn't run {program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said =
        String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr });
    let said = crate::untrusted::clean(said.lines().next().unwrap_or("").trim());
    Err(if said.is_empty() { format!("{program} stop exited with {}", out.status) } else { said })
}

/// The command to run by hand when `stop` fails.
pub fn by_hand(id: &str) -> String {
    format!("claude stop {id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SAID: &str = "That session is running in the background (a1b2c3d4). Run `claude attach a1b2c3d4` to open it, or `claude stop a1b2c3d4` first to resume it here. Add --fork-session to branch off a copy instead.";

    #[test]
    fn takes_the_id_from_claudes_message() {
        assert_eq!(job_id(SAID).as_deref(), Some("a1b2c3d4"));
        // Wrapped over rows between the words; an id split by a wrap isn't one.
        assert_eq!(job_id("running in the\nbackground (a1b2c3d4).").as_deref(), Some("a1b2c3d4"));
        assert_eq!(job_id("running in the background (a1b2\nc3d4)."), None);
        assert_eq!(job_id("It is RUNNING IN THE BACKGROUND (job-7_x)").as_deref(), Some("job-7_x"));
    }

    #[test]
    fn leaves_other_screens_alone() {
        for s in [
            "",
            "The session has ended",
            "running in the background",
            "running in the background (",
            "running in the background ()",
            "running in the background (abc)",
            "running in the background (-rf)",
            "running in the background (a b c d e)",
            "running in the background (`rm -rf ~`)",
            "running in the background (a1b2c3d4",
            "(a1b2c3d4) running in the background",
        ] {
            assert_eq!(job_id(s), None, "{s:?}");
        }
    }

    #[test]
    fn keeps_the_last_lines_without_control_characters() {
        let screen = "1\n2\n3\n\n4\n5\n6\n\u{1b}]52;c;AAAA\u{7}7\n";
        assert_eq!(message(screen), "2\n3\n4\n5\n6\n]52;c;AAAA7");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Whatever the screen holds, an id is short, starts alphanumeric and has only safe characters.
        #[test]
        fn an_id_is_always_safe(s in "\\PC{0,200}", pre in "[ -~]{0,30}") {
            for text in [s.clone(), format!("{pre} running in the background ({s}) {pre}")] {
                if let Some(id) = job_id(&text) {
                    prop_assert!((4..=40).contains(&id.len()));
                    prop_assert!(id.starts_with(|c: char| c.is_ascii_alphanumeric()));
                    prop_assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
                }
            }
        }

        #[test]
        fn a_well_formed_id_is_found(id in "[a-z0-9][a-z0-9_-]{3,39}", pre in "[ -~]{0,30}") {
            let text = format!("{pre}\nThat session is running in the background ({id}). Run it.");
            prop_assert_eq!(job_id(&text), Some(id));
        }
    }
}
