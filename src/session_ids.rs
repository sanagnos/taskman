//! One ticket or agent, one conversation id. `claim` is the invariant, checked wherever an id
//! comes in: from the hooks, from a previous run's status files, and before `--resume`.
//!
//! The toast's wording and `session-conflicts.log` are about a Claude Code race (2.1.296): a
//! running session can switch onto the conversation of another session started in the same folder.
//! See "One conversation per session" in docs/design.md, which also says when they can go.

use std::io::Write;

use crate::store::{self, Cli, Ticket};

#[derive(Debug)]
pub enum Claim<'a> {
    /// No other ticket or agent has this id: `who` may take it.
    Adopt,
    /// `other` already has it.
    Conflict { other: &'a Ticket },
}

// The same ticket, not an equal one.
impl PartialEq for Claim<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Claim::Adopt, Claim::Adopt) => true,
            (Claim::Conflict { other: a }, Claim::Conflict { other: b }) => std::ptr::eq(*a, *b),
            _ => false,
        }
    }
}

/// Whether `who` may take conversation `id`, given every ticket and agent taskman has (in all
/// projects: ids are unique across folders, and `who` itself may be in the list).
pub fn claim<'a>(
    tickets: impl IntoIterator<Item = &'a Ticket>,
    who: &Ticket,
    id: &str,
) -> Claim<'a> {
    match tickets
        .into_iter()
        .find(|o| !o.is(&who.repo, &who.key) && o.session_id.as_deref() == Some(id))
    {
        Some(other) => Claim::Conflict { other },
        None => Claim::Adopt,
    }
}

/// `who`'s hooks reported `other`'s conversation: logs it and returns the toast.
pub fn switched(who: &Ticket, other: &Ticket, id: &str) -> String {
    log(who, other, id);
    switched_text(who.display(), other.display(), who.cli)
}

fn switched_text(who: &str, other: &str, cli: Cli) -> String {
    let bug = if cli.switches_conversations() {
        format!(" (a {} bug)", cli.product())
    } else {
        String::new()
    };
    format!(
        "{who}'s {} switched onto {other}'s conversation{bug}. Restart {who} ({} R) to give it its own.",
        cli.label(),
        crate::keys::prefix()
    )
}

/// A line per trip in the state folder's `session-conflicts.log`, to see how often it happens.
/// It stays on this machine.
fn log(who: &Ticket, other: &Ticket, id: &str) {
    let path = store::state_dir().join("session-conflicts.log");
    let line =
        format!("{} {} {} {id} {}\n", crate::util::now(), who.key, other.key, who.repo.display());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(repo: &str, name: &str, sid: Option<&str>) -> Ticket {
        serde_json::from_value(serde_json::json!({
            "key": format!("acme-{name}"), "repo": repo, "cwd": repo, "agent": name, "session_id": sid
        }))
        .unwrap()
    }

    #[test]
    fn two_agents_in_one_folder_never_share_a_conversation() {
        let tickets = [
            agent("/src/acme", "lead", Some("sid-lead")),
            agent("/src/acme", "tester", Some("sid-tester")),
        ];
        // The tester's hooks report the lead's conversation: it may not take it.
        assert_eq!(
            claim(&tickets, &tickets[1], "sid-lead"),
            Claim::Conflict { other: &tickets[0] }
        );
        // Its own, or a new one after /clear, is fine; so is the lead's own id for the lead.
        assert_eq!(claim(&tickets, &tickets[1], "sid-tester"), Claim::Adopt);
        assert_eq!(claim(&tickets, &tickets[1], "sid-new"), Claim::Adopt);
        assert_eq!(claim(&tickets, &tickets[0], "sid-lead"), Claim::Adopt);
        // Keys repeat across projects; ids don't, so another project's holder counts too.
        let elsewhere = agent("/src/notes", "tester", None);
        assert_eq!(
            claim(&tickets, &elsewhere, "sid-tester"),
            Claim::Conflict { other: &tickets[1] }
        );
    }

    #[test]
    fn the_toast_says_what_happened_and_what_to_do() {
        let prefix = crate::keys::prefix();
        assert_eq!(
            switched_text("tester", "lead", Cli::Claude),
            format!(
                "tester's Claude switched onto lead's conversation (a Claude Code bug). Restart tester ({prefix} R) to give it its own."
            )
        );
        assert!(
            switched_text("tester", "lead", Cli::Codex)
                .starts_with("tester's Codex switched onto lead's conversation. ")
        );
    }
}
