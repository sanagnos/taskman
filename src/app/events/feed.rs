//! What the TUI notes for `taskman events` (`app/feed.rs`): each change in a PR and a session
//! once, from a baseline, and a TUI that goes on when the file can't be written.

use super::*;
use crate::events::{self, Event, Line};
use crate::github::{Lookup, PrState};
use std::path::Path;

/// The events noted for project acme so far, as the command reads them: key and words.
fn noted() -> Vec<String> {
    let path = events::path(Path::new("/src/acme"));
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().filter_map(Line::parse).map(|l| l.text()).collect()
}

/// Ticket `key`'s PR lookup finds `pr`, as its thread would send it.
fn found(app: &mut App, key: &str, pr: crate::github::Pr) {
    app.fetches += 1;
    let (n, i) = (app.fetches, index(app, key));
    app.tickets[i].pr.start(n);
    let id = (app.tickets[i].t.repo.clone(), key.to_string());
    let lookup = Lookup { pr: Some(pr), closed: None, branch_gone: false };
    app.tx.send(Msg::Pr(n, id, Box::new(Ok(lookup)), None)).unwrap();
    app.drain_messages();
}

fn pr_52() -> crate::github::Pr {
    crate::app::test_support::pr(128, "eng-52-login")
}

#[test]
fn a_prs_changes_are_noted_once_each_after_the_first_lookup() {
    let mut app = acme_app();
    // The first lookup since taskman started is what later ones are compared with.
    found(&mut app, "ENG-52", pr_52());
    assert_eq!(noted(), Vec::<String>::new());
    found(&mut app, "ENG-52", pr_52());
    assert_eq!(noted(), Vec::<String>::new(), "nothing changed");
    // Checks pass, and two comments arrive, one of them a review.
    let mut pr = pr_52();
    pr.checks[1].state = crate::github::CheckState::Pass;
    pr.comments.push(crate::github::PrComment { id: "p2".into(), ..Default::default() });
    let mut review = pr.reviews[0].clone();
    review.id = "r3".into();
    pr.reviews.push(review);
    found(&mut app, "ENG-52", pr.clone());
    assert_eq!(noted(), ["ENG-52  PR #128 checks passed", "ENG-52  PR #128 2 new comments"]);
    // Looked up again unchanged: nothing more.
    found(&mut app, "ENG-52", pr.clone());
    assert_eq!(noted().len(), 2);
    // Approved and merged in one go.
    let mut approval = pr.reviews[1].clone();
    (approval.id, approval.state) = ("r4".into(), crate::github::ReviewState::Approved);
    pr.reviews.push(approval);
    pr.state = PrState::Merged;
    found(&mut app, "ENG-52", pr);
    assert_eq!(noted()[2..], ["ENG-52  PR #128 merged", "ENG-52  PR #128 approved"]);
    // No title, review or comment text reaches the file.
    let file = std::fs::read_to_string(events::path(Path::new("/src/acme"))).unwrap();
    for text in ["Sign in with a magic link", "Looks close.", "The link must expire.", "dana"] {
        assert!(!file.contains(text), "{file}");
    }
}

#[test]
fn a_session_is_noted_once_per_change_at_each_status_poll() {
    let mut app = acme_app();
    let set = |app: &mut App, key: &str, state| {
        let i = index(app, key);
        app.tickets[i].status.state = state;
    };
    // The states at the first poll are the baseline: ENG-53 already needed the user.
    app.poll_status();
    assert_eq!(noted(), Vec::<String>::new());
    set(&mut app, "ENG-61", AgentState::Needs);
    app.poll_status();
    assert_eq!(noted(), ["ENG-61  session needs you"]);
    app.poll_status();
    assert_eq!(noted().len(), 1, "still needs the user: no news");
    // However often it moves between polls, a poll sees one change at most.
    set(&mut app, "ENG-61", AgentState::Working);
    set(&mut app, "ENG-61", AgentState::Done);
    set(&mut app, "ENG-61", AgentState::Working);
    set(&mut app, "ENG-61", AgentState::Needs);
    app.poll_status();
    assert_eq!(noted().len(), 1);
    set(&mut app, "ENG-61", AgentState::Done);
    set(&mut app, "acme-lead", AgentState::Working);
    app.poll_status();
    set(&mut app, "acme-lead", AgentState::Exited);
    app.poll_status();
    assert_eq!(
        noted(),
        [
            "ENG-61  session needs you",
            "ENG-61  session finished its turn",
            "acme-lead  session ended",
        ]
    );
    // Put to sleep to save memory isn't an end.
    let i = index(&app, "ENG-61");
    app.tickets[i].asleep = true;
    set(&mut app, "ENG-61", AgentState::Exited);
    app.poll_status();
    assert_eq!(noted().len(), 3);
}

#[test]
fn a_file_that_cant_be_written_is_warned_of_once_and_taskman_goes_on() {
    let mut app = acme_app();
    app.poll_status();
    // A file where the events folder should be: no write gets past it, even as root.
    let folder = store::state_dir().join("events");
    std::fs::create_dir_all(store::state_dir()).unwrap();
    std::fs::write(&folder, "").unwrap();
    let warned = |app: &mut App| {
        let shown = toast(app).is_some_and(|t| t.starts_with("Couldn't note changes"));
        app.toast = None;
        shown
    };
    let i = index(&app, "ENG-61");
    app.tickets[i].status.state = AgentState::Needs;
    app.poll_status();
    assert!(warned(&mut app));
    for state in [AgentState::Done, AgentState::Needs] {
        app.tickets[i].status.state = state;
        app.poll_status();
        assert!(!warned(&mut app), "once");
    }
    // Writable again: noted from then on, and warned of again if it fails again.
    std::fs::remove_file(&folder).unwrap();
    app.tickets[i].status.state = AgentState::Done;
    app.poll_status();
    assert_eq!(noted(), ["ENG-61  session finished its turn"]);
    std::fs::remove_dir_all(&folder).unwrap();
    std::fs::write(&folder, "").unwrap();
    app.tickets[i].status.state = AgentState::Needs;
    app.poll_status();
    assert!(warned(&mut app));
}

#[test]
fn events_of_a_project_go_to_its_own_file() {
    let mut app = acme_app();
    app.poll_status();
    let i = index(&app, "notes-writer");
    app.tickets[i].status.state = AgentState::Done;
    app.poll_status();
    assert_eq!(noted(), Vec::<String>::new(), "notes isn't acme");
    let notes = std::fs::read_to_string(events::path(&app.tickets[i].t.repo)).unwrap();
    let lines: Vec<Line> = notes.lines().filter_map(Line::parse).collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(
        (lines[0].key.to_string(), lines[0].event),
        ("notes-writer".into(), Event::YourTurn)
    );
}
