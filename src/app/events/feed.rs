//! What the TUI notes for `taskman events` (`app/feed.rs`): each change in a ticket's PRs and
//! session once, from a baseline, nothing of agents, and a TUI that goes on when the file can't be
//! written.

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

/// Ticket `key`'s PR lookup finds `pr`, and weighs only it, as its thread would send it.
fn found(app: &mut App, key: &str, pr: crate::github::Pr) {
    let weighed = [(pr.number, pr.state.clone())];
    found_among(app, key, pr, &weighed);
}

/// As `found`, the lookup having weighed the PRs `weighed` (`Lookup::weighed`).
fn found_among(app: &mut App, key: &str, pr: crate::github::Pr, weighed: &[(u64, PrState)]) {
    app.fetches += 1;
    let (n, i) = (app.fetches, index(app, key));
    app.tickets[i].pr.start(n);
    let id = (app.tickets[i].t.repo.clone(), key.to_string());
    let weighed = weighed.to_vec();
    let lookup = Lookup { pr: Some(pr), weighed, ..Lookup::default() };
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
fn a_pr_closed_while_an_older_merged_one_is_shown_is_closed_not_merged_again() {
    let mut app = acme_app();
    let mut merged = pr_52();
    (merged.number, merged.state) = (96, PrState::Merged);
    found(&mut app, "ENG-52", merged.clone());
    // #97 opens, then closes without merging: the lookup goes back to showing merged #96.
    let open = [(96, PrState::Merged), (97, PrState::Open)];
    found_among(&mut app, "ENG-52", pr_52_as(97), &open);
    let closed = [(96, PrState::Merged), (97, PrState::Closed)];
    found_among(&mut app, "ENG-52", merged.clone(), &closed);
    found_among(&mut app, "ENG-52", merged, &closed);
    assert_eq!(noted(), ["ENG-52  PR #97 opened", "ENG-52  PR #97 closed"]);
    // Two open PRs the lookup switches between: each opened once.
    for n in [98, 99, 98, 99, 98] {
        found(&mut app, "ENG-52", pr_52_as(n));
    }
    assert_eq!(noted()[2..], ["ENG-52  PR #98 opened", "ENG-52  PR #99 opened"]);
}

#[test]
fn a_pr_merged_while_an_open_one_that_mentions_the_key_takes_its_place_is_told_merged() {
    let mut app = acme_app();
    // ENG-52's own #10 is open; #50, open too, only mentions ENG-52.
    found(&mut app, "ENG-52", pr_52_as(10));
    // #10 merges, so the lookup weighs the PRs that mention the key too and shows #50.
    let weighed = [(10, PrState::Merged), (50, PrState::Open)];
    found_among(&mut app, "ENG-52", pr_52_as(50), &weighed);
    found_among(&mut app, "ENG-52", pr_52_as(50), &weighed);
    assert_eq!(noted(), ["ENG-52  PR #10 merged", "ENG-52  PR #50 opened"]);
}

#[test]
fn prs_of_another_repository_or_an_unknown_one_have_no_events() {
    let mut app = acme_app();
    let send = |app: &mut App, lookup: Lookup| {
        app.fetches += 1;
        let (n, i) = (app.fetches, index(app, "ENG-52"));
        app.tickets[i].pr.start(n);
        let id = (app.tickets[i].t.repo.clone(), "ENG-52".to_string());
        app.tx.send(Msg::Pr(n, id, Box::new(Ok(lookup)), None)).unwrap();
        app.drain_messages();
    };
    // The issue's repository is another than the checkout's (or the checkout's can't be told),
    // and its #45 closes the issue: its numbers aren't the checkout's, so none of it is told.
    let theirs = |state: PrState| {
        let pr = crate::github::Pr { state: state.clone(), ..pr_52_as(45) };
        Lookup { pr: Some(pr), weighed: vec![(45, state)], elsewhere: true, ..Lookup::default() }
    };
    send(&mut app, theirs(PrState::Open));
    send(&mut app, theirs(PrState::Open));
    send(&mut app, theirs(PrState::Merged));
    let mut closed = theirs(PrState::Open);
    closed.pr = None;
    closed.weighed = vec![(45, PrState::Closed)];
    send(&mut app, closed);
    assert_eq!(noted(), Vec::<String>::new());
    // Lookups of the checkout's own PRs count, from the first of them as the baseline.
    found(&mut app, "ENG-52", pr_52_as(46));
    found(&mut app, "ENG-52", crate::github::Pr { state: PrState::Merged, ..pr_52_as(46) });
    assert_eq!(noted(), ["ENG-52  PR #46 merged"]);
    // One in between that can't be told is skipped, and changes nothing.
    send(&mut app, theirs(PrState::Open));
    found(&mut app, "ENG-52", crate::github::Pr { state: PrState::Merged, ..pr_52_as(46) });
    assert_eq!(noted().len(), 1);
}

/// ENG-52's PR as number `number`, with no checks: only its opening and closing are news.
fn pr_52_as(number: u64) -> crate::github::Pr {
    crate::github::Pr { number, checks: Vec::new(), ..pr_52() }
}

#[test]
fn agents_have_no_events_so_two_cant_wake_each_other() {
    let mut app = acme_app();
    app.poll_status();
    // A lead and a reviewer agent, both told to follow, taking turns: neither hears of the other.
    for _ in 0..3 {
        for (key, state) in [
            ("acme-lead", AgentState::Working),
            ("acme-release-notes-reviewer", AgentState::Done),
            ("acme-lead", AgentState::Done),
            ("acme-release-notes-reviewer", AgentState::Working),
        ] {
            let i = index(&app, key);
            app.tickets[i].status.state = state;
            app.poll_status();
        }
    }
    let i = index(&app, "acme-lead");
    app.tickets[i].status.state = AgentState::Needs;
    app.poll_status();
    app.tickets[i].status.state = AgentState::Exited;
    app.poll_status();
    assert_eq!(noted(), Vec::<String>::new());
    assert!(!events::path(Path::new("/src/notes")).exists());
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
    set(&mut app, "ENG-53", AgentState::Working);
    app.poll_status();
    set(&mut app, "ENG-53", AgentState::Exited);
    app.poll_status();
    assert_eq!(
        noted(),
        ["ENG-61  session needs you", "ENG-61  session finished its turn", "ENG-53  session ended",]
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
fn a_file_that_cant_be_moved_aside_is_warned_of_once_and_still_written() {
    let mut app = acme_app();
    app.poll_status();
    let path = events::path(Path::new("/src/acme"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let full = usize::try_from(events::ROTATE_AT).unwrap();
    std::fs::write(&path, "x".repeat(full) + "\n").unwrap();
    let mut older = path.clone().into_os_string();
    older.push(".1");
    std::fs::create_dir(&older).unwrap();
    let i = index(&app, "ENG-61");
    let mut warned = 0;
    for state in [AgentState::Needs, AgentState::Done] {
        app.tickets[i].status.state = state;
        app.poll_status();
        warned += usize::from(toast(&app).is_some_and(|t| t.starts_with("Couldn't move")));
        app.toast = None;
    }
    assert_eq!(warned, 1);
    assert_eq!(noted(), ["ENG-61  session needs you", "ENG-61  session finished its turn"]);
    assert!(std::path::Path::new(&older).is_dir());
}

#[test]
fn events_of_a_project_go_to_its_own_file() {
    let mut app = acme_app();
    // A ticket of another project, with the same key as one of acme's.
    let notes: crate::store::Ticket = serde_json::from_value(
        serde_json::json!({ "key": "ENG-52", "repo": "/src/notes", "cwd": "/src/notes" }),
    )
    .unwrap();
    app.tickets.push(super::super::TicketView::new(notes));
    app.poll_status();
    let i = app.tickets.len() - 1;
    app.tickets[i].status.state = AgentState::Done;
    app.poll_status();
    assert_eq!(noted(), Vec::<String>::new(), "notes isn't acme");
    let text = std::fs::read_to_string(events::path(Path::new("/src/notes"))).unwrap();
    let lines: Vec<Line> = text.lines().filter_map(Line::parse).collect();
    assert_eq!(lines.len(), 1);
    assert_eq!((lines[0].key.to_string(), lines[0].event), ("ENG-52".into(), Event::YourTurn));
}

// ---- the feed against a model --------------------------------------------------------------------

mod modelled {
    //! Random sequences of what happens to a ticket's PRs on GitHub and of lookups of them
    //! (`events::model`), through the TUI as a lookup's thread would send them: after each
    //! lookup, what the events file gained is what the model owes, and what its rules allow.

    use super::*;
    use crate::events::Overall;
    use crate::events::model::{self, Action, Model, Seen, Shown, Trace, World};
    use crate::github::{Pr, PrComment, Review, ReviewState, Stamp};
    use proptest::prelude::*;
    use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};

    /// How many PRs the feed under test remembers: fewer than the sequences have (`model::PRS`),
    /// so they get past the bound.
    const KEPT: usize = 2;

    /// The model's side: GitHub's PRs, and for the last action, when it was a lookup, what it
    /// read and the events owed for it.
    #[derive(Clone, Debug)]
    struct Owed {
        world: World,
        model: Model,
        last: Option<(Seen, Vec<Event>)>,
    }

    impl ReferenceStateMachine for Owed {
        type State = Owed;
        type Transition = Action;

        fn init_state() -> BoxedStrategy<Owed> {
            let model = Model::new(KEPT, false);
            Just(Owed { world: World::default(), model, last: None }).boxed()
        }

        fn transitions(state: &Owed) -> BoxedStrategy<Action> {
            model::action(&state.world).boxed()
        }

        fn apply(mut state: Owed, action: &Action) -> Owed {
            state.last = None;
            match action {
                Action::Move(m) => {
                    state.world.apply(m);
                }
                Action::Restart => state.model.restart(),
                Action::Look(look) => {
                    let seen = state.world.read(look);
                    let owed = state.model.expect(&seen);
                    state.last = Some((seen, owed));
                }
            }
            state
        }
    }

    /// The TUI, the rules its events are held to, and how many of the file's events were read.
    struct Tui {
        app: App,
        trace: Trace,
        read: usize,
    }

    /// The shown PR as the lookup's thread would send it.
    fn pr_of(shown: &Shown) -> Pr {
        let reviews = |prefix: &'static str, n: u32, state: ReviewState| {
            (0..n).map(move |i| Review {
                id: format!("{prefix}{i}"),
                state: state.clone(),
                ..pr_52().reviews[0].clone()
            })
        };
        let rollup = match shown.checks {
            Overall::None => "",
            Overall::Pending => "PENDING",
            Overall::Pass => "SUCCESS",
            Overall::Fail => "FAILURE",
        };
        Pr {
            number: shown.number,
            state: shown.state.pr_state(),
            head_oid: format!("h{}", shown.head),
            checks: Vec::new(),
            stamp: Stamp { checks: rollup.into(), ..Stamp::default() },
            reviews: reviews("a", shown.approvals, ReviewState::Approved)
                .chain(reviews("x", shown.change_requests, ReviewState::ChangesRequested))
                .collect(),
            comments: (0..shown.comments)
                .map(|i| PrComment { id: format!("c{i}"), ..PrComment::default() })
                .collect(),
            ..pr_52()
        }
    }

    fn lookup_of(seen: &Seen) -> Lookup {
        Lookup {
            pr: seen.shown.as_ref().map(pr_of),
            weighed: seen.weighed(),
            elsewhere: seen.elsewhere,
            ..Lookup::default()
        }
    }

    impl StateMachineTest for Tui {
        type SystemUnderTest = Tui;
        type Reference = Owed;

        fn init_test(_: &Owed) -> Tui {
            // The case before's, in this thread's state folder.
            let _ = std::fs::remove_file(events::path(Path::new("/src/acme")));
            let mut app = acme_app();
            let i = index(&app, "ENG-52");
            app.tickets[i].restart_feed(KEPT);
            Tui { app, trace: Trace::new(KEPT, false), read: 0 }
        }

        fn apply(mut tui: Tui, owed: &Owed, action: Action) -> Tui {
            let app = &mut tui.app;
            match action {
                Action::Move(_) => {}
                Action::Restart => {
                    let i = index(app, "ENG-52");
                    app.tickets[i].restart_feed(KEPT);
                    tui.trace.restart();
                }
                Action::Look(_) => {
                    let (seen, owed) = owed.last.clone().expect("a lookup read something");
                    // An agent finds the same, and has no events.
                    let agent = app.tickets.iter().position(|tv| tv.t.agent.is_some()).unwrap();
                    app.tickets[agent].take_pr(Ok(lookup_of(&seen)), None);
                    app.fetches += 1;
                    let (n, i) = (app.fetches, index(app, "ENG-52"));
                    app.tickets[i].pr.start(n);
                    let id = (app.tickets[i].t.repo.clone(), "ENG-52".to_string());
                    app.tx.send(Msg::Pr(n, id, Box::new(Ok(lookup_of(&seen))), None)).unwrap();
                    app.drain_messages();
                    let path = events::path(Path::new("/src/acme"));
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    let lines: Vec<Line> = text.lines().filter_map(Line::parse).collect();
                    let new = &lines[tui.read..];
                    assert!(new.iter().all(|l| l.key.to_string() == "ENG-52"), "{new:?}");
                    let told: Vec<Event> = new.iter().map(|l| l.event).collect();
                    tui.read = lines.len();
                    if let Err(why) = tui.trace.check(&seen, &told) {
                        panic!("{why}: read {seen:?}");
                    }
                    assert_eq!(model::sorted(&told), model::sorted(&owed), "read {seen:?}");
                }
            }
            tui
        }
    }

    prop_state_machine! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        #[test]
        fn the_feed_tells_what_the_model_owes(sequential 1..40 => Tui);
    }
}
