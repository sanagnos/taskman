//! A model of what `taskman events` should tell of a ticket's PRs, which tests drive the real code
//! against: `Prs::see` here, over short sequences taken one and all (`explore`), and the TUI's
//! feed in `app/events/feed.rs`, over random ones.
//!
//! It isn't `Prs` written twice. There are three parts, and a test compares the code with the last
//! two after every lookup:
//!
//! - `World` is the PRs as GitHub has them, moved only as GitHub lets a PR move (a merged one stays
//!   merged, a closed one can reopen), and a lookup (`Look`) reads it. Two things are assumed of a
//!   lookup. The PR it shows is read as it is, its details coming from the PR itself. A PR it only
//!   weighs may be read as still open after it merged or closed, GitHub's search index being
//!   behind; a list that says closed after a reopen isn't modelled.
//! - `Trace` holds the rules that need no memory of the code's kind, only what GitHub did, what
//!   the lookups read and what was told. Nothing is told that didn't happen or wasn't watched
//!   happening; nothing is told more often than it happened, unless the PR was forgotten in
//!   between; and an end read of a PR still remembered is told.
//! - `Model` is the events owed, exactly: a map of what each remembered PR last said, by number,
//!   forgetting the ones seen longest ago past the bound. It follows `Prs`'s rules closely, so it
//!   catches what the rules above can't know (a check untold), and they catch what it shares
//!   with the code.

use super::{Event, Overall, PrSnap, Prs};
use crate::github::PrState;
use proptest::prelude::*;
use std::collections::BTreeMap;

/// The PR numbers the generated sequences use. Few, so that they meet.
pub(crate) const PRS: u64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Open,
    Closed,
    Merged,
}

impl State {
    pub(crate) fn pr_state(self) -> PrState {
        match self {
            State::Open => PrState::Open,
            State::Closed => PrState::Closed,
            State::Merged => PrState::Merged,
        }
    }
}

/// A PR as GitHub has it: every state it has been in, the last being its state now, and what it
/// says.
#[derive(Clone, Debug)]
struct WorldPr {
    history: Vec<State>,
    head: u32,
    checks: Overall,
    /// How often its checks came to pass, and to fail.
    passes: u32,
    fails: u32,
    approvals: u32,
    change_requests: u32,
    comments: u32,
}

/// The checkout's repository's PRs, by number.
#[derive(Clone, Debug, Default)]
pub(crate) struct World(BTreeMap<u64, WorldPr>);

/// Something that happens to a PR on GitHub. One that can't (merging a closed PR) changes nothing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Move {
    Open(u64),
    Merge(u64),
    Close(u64),
    Reopen(u64),
    /// A new head commit, with no checks yet.
    Push(u64),
    Checks(u64, Overall),
    Approve(u64),
    RequestChanges(u64),
    Comment(u64),
}

/// Whose PRs a lookup found (`Lookup::elsewhere`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Repo {
    Here,
    /// Another repository's, whose numbers name other PRs.
    Other,
    /// The checkout's, which the lookup couldn't tell.
    Unknown,
}

/// A lookup: the PR it shows, when there is one by that number, and those it weighs, each with
/// whether its list is behind, and still says open.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Look {
    pub shown: Option<u64>,
    pub weighed: Vec<(u64, bool)>,
    pub repo: Repo,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Move(Move),
    Look(Look),
    /// taskman starts again: the next lookup is a baseline.
    Restart,
}

/// The PR a lookup shows, as it read it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    pub number: u64,
    pub state: State,
    pub head: u32,
    pub checks: Overall,
    pub approvals: u32,
    pub change_requests: u32,
    pub comments: u32,
}

impl Shown {
    pub(crate) fn snap(&self) -> PrSnap {
        let ids = |prefix: &str, n: u32| (0..n).map(|i| format!("{prefix}{i}")).collect();
        PrSnap {
            number: self.number,
            state: self.state.pr_state(),
            head: format!("h{}", self.head),
            checks: self.checks,
            approvals: ids("a", self.approvals),
            change_requests: ids("x", self.change_requests),
            comments: ids("c:", self.comments),
            detailed: true,
        }
    }
}

/// What a lookup read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Seen {
    pub shown: Option<Shown>,
    /// Each number once.
    pub weighed: Vec<(u64, State)>,
    pub elsewhere: bool,
}

impl Seen {
    pub(crate) fn weighed(&self) -> Vec<(u64, PrState)> {
        self.weighed.iter().map(|&(n, s)| (n, s.pr_state())).collect()
    }

    /// Each PR's state as this lookup has it: the shown one's over a list's.
    fn states(&self) -> BTreeMap<u64, State> {
        let mut states: BTreeMap<u64, State> = self.weighed.iter().copied().collect();
        if let Some(shown) = &self.shown {
            states.insert(shown.number, shown.state);
        }
        states
    }
}

impl World {
    /// Whether `m` changed anything.
    pub(crate) fn apply(&mut self, m: &Move) -> bool {
        let n = match *m {
            Move::Open(n)
            | Move::Merge(n)
            | Move::Close(n)
            | Move::Reopen(n)
            | Move::Push(n)
            | Move::Checks(n, _)
            | Move::Approve(n)
            | Move::RequestChanges(n)
            | Move::Comment(n) => n,
        };
        let Some(pr) = self.0.get_mut(&n) else {
            if *m != Move::Open(n) {
                return false;
            }
            let pr = WorldPr {
                history: vec![State::Open],
                head: 1,
                checks: Overall::None,
                passes: 0,
                fails: 0,
                approvals: 0,
                change_requests: 0,
                comments: 0,
            };
            self.0.insert(n, pr);
            return true;
        };
        let state = pr.history[pr.history.len() - 1];
        match (m, state) {
            (Move::Merge(_), State::Open) => pr.history.push(State::Merged),
            (Move::Close(_), State::Open) => pr.history.push(State::Closed),
            (Move::Reopen(_), State::Closed) => pr.history.push(State::Open),
            (Move::Push(_), State::Open) => (pr.head, pr.checks) = (pr.head + 1, Overall::None),
            (Move::Checks(_, checks), _) if *checks != pr.checks => {
                pr.checks = *checks;
                pr.passes += u32::from(*checks == Overall::Pass);
                pr.fails += u32::from(*checks == Overall::Fail);
            }
            (Move::Approve(_), _) => pr.approvals += 1,
            (Move::RequestChanges(_), _) => pr.change_requests += 1,
            (Move::Comment(_), _) => pr.comments += 1,
            _ => return false,
        }
        true
    }

    /// The moves that would change something.
    fn moves(&self) -> Vec<Move> {
        let mut moves = Vec::new();
        for n in 1..=PRS {
            let Some(pr) = self.0.get(&n) else {
                moves.push(Move::Open(n));
                continue;
            };
            match pr.history[pr.history.len() - 1] {
                State::Open => moves.extend([Move::Merge(n), Move::Close(n), Move::Push(n)]),
                State::Closed => moves.push(Move::Reopen(n)),
                State::Merged => {}
            }
            let checks = [Overall::None, Overall::Pending, Overall::Pass, Overall::Fail];
            moves
                .extend(checks.into_iter().filter(|c| *c != pr.checks).map(|c| Move::Checks(n, c)));
            moves.extend([Move::Approve(n), Move::RequestChanges(n), Move::Comment(n)]);
        }
        moves
    }

    /// Whether a list of `look` reads a PR as it no longer is.
    fn behind(&self, look: &Look) -> bool {
        let ended =
            |n: &u64| self.0.get(n).is_some_and(|pr| pr.history.last() != Some(&State::Open));
        look.repo == Repo::Here && look.weighed.iter().any(|(n, behind)| *behind && ended(n))
    }

    /// How often the thing an event of `kind` tells of has happened to PR `n`.
    fn happened(&self, n: u64, kind: &str) -> u32 {
        let Some(pr) = self.0.get(&n) else { return 0 };
        let been = |state: State| pr.history.iter().filter(|s| **s == state).count();
        match kind {
            "pr_opened" => u32::try_from(been(State::Open)).unwrap_or(u32::MAX),
            "pr_closed" => u32::try_from(been(State::Closed)).unwrap_or(u32::MAX),
            "pr_merged" => u32::try_from(been(State::Merged)).unwrap_or(u32::MAX),
            "checks_passed" => pr.passes,
            "checks_failed" => pr.fails,
            "approved" => pr.approvals,
            "changes_requested" => pr.change_requests,
            _ => 0,
        }
    }

    pub(crate) fn read(&self, look: &Look) -> Seen {
        // The other repository's #n is another PR than the checkout's #n: the next one's here.
        let pr = |n: u64| match look.repo {
            Repo::Other => self.0.get(&(n % PRS + 1)),
            Repo::Here | Repo::Unknown => self.0.get(&n),
        };
        let shown = look.shown.and_then(|n| {
            pr(n).map(|pr| Shown {
                number: n,
                state: pr.history[pr.history.len() - 1],
                head: pr.head,
                checks: pr.checks,
                approvals: pr.approvals,
                change_requests: pr.change_requests,
                comments: pr.comments,
            })
        });
        let mut weighed: Vec<(u64, State)> = Vec::new();
        for &(n, behind) in &look.weighed {
            if let Some(pr) = pr(n)
                && !weighed.iter().any(|&(m, _)| m == n)
            {
                let state = if behind { State::Open } else { pr.history[pr.history.len() - 1] };
                weighed.push((n, state));
            }
        }
        Seen { shown, weighed, elsewhere: look.repo != Repo::Here }
    }
}

// ---- the rules that need no memory ---------------------------------------------------------

/// What the lookups that counted read, and what was told, since taskman started: enough to say
/// whether an event could be true, whether it was told too often and whether one is missing,
/// without knowing what the code remembers.
#[derive(Clone, Debug)]
pub(crate) struct Trace {
    kept: usize,
    baselined: bool,
    looks: Vec<BTreeMap<u64, State>>,
    /// Each PR's last reading: in which of `looks`, and its state there.
    last: BTreeMap<u64, (usize, State)>,
    /// Each event told but the comments: its PR, its kind and how many lookups had counted by
    /// then.
    told: Vec<(u64, &'static str, usize)>,
    /// The comments told of each PR, for as long as taskman has run or not.
    comments: BTreeMap<u64, u32>,
}

impl Trace {
    /// For a feed that remembers `kept` PRs; `baselined` when its first lookup tells too, as
    /// `Prs::see` does on its own.
    pub(crate) fn new(kept: usize, baselined: bool) -> Trace {
        Trace {
            kept,
            baselined,
            looks: Vec::new(),
            last: BTreeMap::new(),
            told: Vec::new(),
            comments: BTreeMap::new(),
        }
    }

    pub(crate) fn restart(&mut self) {
        self.baselined = false;
        (self.looks, self.last, self.told) = (Vec::new(), BTreeMap::new(), Vec::new());
    }

    /// Whether `told` is something a lookup that read `seen` of `world` may tell, and all it
    /// must.
    pub(crate) fn check(
        &mut self,
        world: &World,
        seen: &Seen,
        told: &[Event],
    ) -> Result<(), String> {
        if seen.elsewhere {
            return match told {
                [] => Ok(()),
                _ => Err("a lookup of another repository's PRs, or an unknown one's, told".into()),
            };
        }
        let now = seen.states();
        if !self.baselined && !told.is_empty() {
            return Err("the first lookup told".into());
        }
        if self.baselined {
            for (i, e) in told.iter().enumerate() {
                if told[..i].iter().any(|d| (d.kind(), d.pr()) == (e.kind(), e.pr())) {
                    return Err(format!("{e} told twice by one lookup"));
                }
                self.may_tell(world, seen, &now, *e)
                    .map_err(|why| format!("{e} told, but {why}"))?;
            }
            self.owed(&now, told)?;
        }
        self.baselined = true;
        for e in told {
            match (*e, e.pr()) {
                (Event::Comments { pr, count }, _) => {
                    *self.comments.entry(pr).or_default() += count
                }
                (_, Some(n)) => self.told.push((n, e.kind(), self.looks.len())),
                (_, None) => {}
            }
        }
        for (&n, &state) in &now {
            self.last.insert(n, (self.looks.len(), state));
        }
        self.looks.push(now);
        Ok(())
    }

    /// How many PRs other than `n` the lookups from `from` on read, this one's (`now`) too: once
    /// it's as many as are remembered, `n` may have been forgotten.
    fn others_since(&self, n: u64, from: usize, now: Option<&BTreeMap<u64, State>>) -> usize {
        let mut others: Vec<u64> = self.looks[from..]
            .iter()
            .chain(now)
            .flat_map(|l| l.keys().copied())
            .filter(|m| *m != n)
            .collect();
        others.sort_unstable();
        others.dedup();
        others.len()
    }

    /// An end is owed for a PR last read open and not forgotten since: the lookups from that one
    /// on have read fewer other PRs than are remembered. But for a PR once read merged: a list
    /// that then says open is behind.
    fn owed(&self, now: &BTreeMap<u64, State>, told: &[Event]) -> Result<(), String> {
        for (&n, &state) in now {
            let owed = match state {
                State::Merged => Event::PrMerged(n),
                State::Closed => Event::PrClosed(n),
                State::Open => continue,
            };
            let Some(&(at, State::Open)) = self.last.get(&n) else { continue };
            let merged = self.looks.iter().any(|l| l.get(&n) == Some(&State::Merged));
            if !merged && self.others_since(n, at, None) < self.kept && !told.contains(&owed) {
                return Err(format!(
                    "{owed} not told, though it was last read open and is remembered"
                ));
            }
        }
        Ok(())
    }

    fn may_tell(
        &self,
        world: &World,
        seen: &Seen,
        now: &BTreeMap<u64, State>,
        e: Event,
    ) -> Result<(), String> {
        let shown = |n: u64| seen.shown.as_ref().filter(|s| s.number == n).ok_or("it isn't shown");
        let read = |n: u64, state: State| self.looks.iter().any(|l| l.get(&n) == Some(&state));
        // What it says is so, and was watched becoming so.
        let n = match e {
            Event::PrOpened(n) if shown(n)?.state != State::Open => {
                return Err("it isn't open".into());
            }
            Event::PrMerged(n) if now.get(&n) != Some(&State::Merged) => {
                return Err("this lookup didn't read it merged".into());
            }
            Event::PrMerged(n) if !read(n, State::Open) && !read(n, State::Closed) => {
                return Err("no lookup read it before it merged".into());
            }
            Event::PrClosed(n) if now.get(&n) != Some(&State::Closed) => {
                return Err("this lookup didn't read it closed".into());
            }
            Event::PrClosed(n) if !read(n, State::Open) => {
                return Err("no lookup read it open".into());
            }
            Event::ChecksPassed(n) if shown(n)?.checks != Overall::Pass => {
                return Err("its checks haven't passed".into());
            }
            Event::ChecksFailed(n) if shown(n)?.checks != Overall::Fail => {
                return Err("its checks haven't failed".into());
            }
            Event::Approved(n) | Event::ChangesRequested(n) => shown(n)?.number,
            Event::Comments { pr, count } => {
                let before = self.comments.get(&pr).copied().unwrap_or(0);
                return match shown(pr)?.comments {
                    all if before + count > all => Err(format!("it has {all}, {before} told")),
                    _ => Ok(()),
                };
            }
            Event::NeedsYou | Event::YourTurn | Event::Ended => {
                return Err("it's a session's".into());
            }
            Event::PrOpened(n)
            | Event::PrMerged(n)
            | Event::PrClosed(n)
            | Event::ChecksPassed(n)
            | Event::ChecksFailed(n) => n,
        };
        // And no more often than it happened, but for a PR forgotten since it was last told.
        let times = self.told.iter().filter(|(m, kind, _)| (*m, *kind) == (n, e.kind()));
        let (times, last) = (times.clone().count(), times.last().map(|(_, _, at)| *at));
        let happened = world.happened(n, e.kind());
        match last {
            _ if u32::try_from(times).is_ok_and(|times| times < happened) => Ok(()),
            None => Err("that never happened".into()),
            Some(at) if self.others_since(n, at, Some(now)) >= self.kept => Ok(()),
            Some(_) => Err(format!("that happened {happened} times, told {times} times already")),
        }
    }
}

// ---- the events owed ------------------------------------------------------------------------

/// What a shown PR said when it was last shown.
#[derive(Clone, Debug)]
struct Said {
    head: u32,
    checks: Overall,
    approvals: u32,
    change_requests: u32,
    comments: u32,
}

/// A PR the feed remembers.
#[derive(Clone, Debug)]
struct Known {
    state: State,
    /// None for one only ever weighed.
    said: Option<Said>,
    /// When it was last seen, by `Model::clock`.
    at: u64,
}

/// What the feed should tell, exactly: the same events as the code, in any order.
#[derive(Clone, Debug)]
pub(crate) struct Model {
    kept: usize,
    baselined: bool,
    known: BTreeMap<u64, Known>,
    clock: u64,
    /// The PRs forgotten so far, for `Reached`.
    pub forgotten: u32,
}

impl Model {
    pub(crate) fn new(kept: usize, baselined: bool) -> Model {
        Model { kept, baselined, known: BTreeMap::new(), clock: 0, forgotten: 0 }
    }

    pub(crate) fn restart(&mut self) {
        (self.baselined, self.known) = (false, BTreeMap::new());
    }

    pub(crate) fn remembered(&self) -> usize {
        self.known.len()
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// The events owed for a lookup that read `seen`.
    pub(crate) fn expect(&mut self, seen: &Seen) -> Vec<Event> {
        if seen.elsewhere {
            return Vec::new();
        }
        let mut events = Vec::new();
        let shown = seen.shown.as_ref();
        for &(n, state) in seen.weighed.iter().filter(|(n, _)| Some(*n) != shown.map(|s| s.number))
        {
            let at = self.tick();
            let Some(known) = self.known.get_mut(&n) else {
                // Whatever its state, it's no news: opened is for a PR the lookup shows.
                self.known.insert(n, Known { state, said: None, at });
                continue;
            };
            match (known.state, state) {
                (State::Open, State::Merged) => events.push(Event::PrMerged(n)),
                (State::Open, State::Closed) => events.push(Event::PrClosed(n)),
                _ => {}
            }
            // Merged is for good, whatever a list says after.
            if known.state != State::Merged {
                known.state = state;
            }
            known.at = at;
        }
        if let Some(shown) = shown {
            let n = shown.number;
            let at = self.tick();
            let known = self.known.remove(&n);
            let news = match (known.as_ref().map(|k| k.state), shown.state) {
                (None, State::Open) => Some(Event::PrOpened(n)),
                // First seen ended: an old PR, maybe.
                (None, _) => None,
                (Some(before), now) if before == now => None,
                (Some(_), State::Open) => Some(Event::PrOpened(n)),
                (Some(_), State::Closed) => Some(Event::PrClosed(n)),
                (Some(_), State::Merged) => Some(Event::PrMerged(n)),
            };
            events.extend(news);
            if known.is_some() || shown.state == State::Open {
                let said = known.and_then(|k| k.said);
                // Its checks are told as they're first seen; its reviews and comments are taken
                // as read then.
                let checked = said.as_ref().map(|s| (s.checks, s.head));
                if checked != Some((shown.checks, shown.head)) {
                    match shown.checks {
                        Overall::Pass => events.push(Event::ChecksPassed(n)),
                        Overall::Fail => events.push(Event::ChecksFailed(n)),
                        Overall::None | Overall::Pending => {}
                    }
                }
                if let Some(said) = said {
                    if shown.approvals > said.approvals {
                        events.push(Event::Approved(n));
                    }
                    if shown.change_requests > said.change_requests {
                        events.push(Event::ChangesRequested(n));
                    }
                    if shown.comments > said.comments {
                        let count = shown.comments - said.comments;
                        events.push(Event::Comments { pr: n, count });
                    }
                }
            }
            let said = Said {
                head: shown.head,
                checks: shown.checks,
                approvals: shown.approvals,
                change_requests: shown.change_requests,
                comments: shown.comments,
            };
            self.known.insert(n, Known { state: shown.state, said: Some(said), at });
        }
        // Once the whole lookup is in: the PRs seen longest ago go.
        while self.known.len() > self.kept {
            let oldest = self.known.iter().min_by_key(|(_, k)| k.at).map(|(n, _)| *n);
            self.known.remove(&oldest.expect("it isn't empty"));
            self.forgotten += 1;
        }
        if !self.baselined {
            self.baselined = true;
            return Vec::new();
        }
        events
    }
}

/// `events` in an order of their own, to compare what was told with what's owed: the order within
/// a lookup is nobody's promise.
pub(crate) fn sorted(events: &[Event]) -> Vec<(Option<u64>, &'static str, u32)> {
    let count = |e: &Event| match e {
        Event::Comments { count, .. } => *count,
        _ => 0,
    };
    let mut all: Vec<_> = events.iter().map(|e| (e.pr(), e.kind(), count(e))).collect();
    all.sort_unstable();
    all
}

// ---- generated sequences --------------------------------------------------------------------

/// An action for sequences that go somewhere: a move `world` can make, or a lookup.
pub(crate) fn action(world: &World) -> impl Strategy<Value = Action> + use<> {
    let moved = proptest::sample::select(world.moves());
    let repo = prop_oneof![8 => Just(Repo::Here), 1 => Just(Repo::Other), 1 => Just(Repo::Unknown)];
    let look = (
        proptest::option::weighted(0.8, 1..=PRS),
        proptest::collection::vec((1..=PRS, proptest::bool::weighted(0.25)), 0..=4),
        repo,
    )
        .prop_map(|(shown, weighed, repo)| Look { shown, weighed, repo });
    prop_oneof![
        10 => moved.prop_map(Action::Move),
        10 => look.prop_map(Action::Look),
        1 => Just(Action::Restart),
    ]
}

/// What a run of sequences got to, so a test can say its sequences aren't idle ones: a generator
/// that never merges a PR passes every rule about merges.
#[derive(Debug, Default)]
pub(crate) struct Reached {
    pub moves: BTreeMap<&'static str, u32>,
    pub events: BTreeMap<&'static str, u32>,
    pub lookups: u32,
    pub behind: u32,
    pub elsewhere: u32,
    pub restarts: u32,
    pub forgotten: u32,
}

impl Reached {
    pub(crate) fn note(&mut self, world: &World, action: &Action, told: &[Event]) {
        match action {
            Action::Move(m) => {
                let name = match m {
                    Move::Open(_) => "open",
                    Move::Merge(_) => "merge",
                    Move::Close(_) => "close",
                    Move::Reopen(_) => "reopen",
                    Move::Push(_) => "push",
                    Move::Checks(..) => "checks",
                    Move::Approve(_) => "approve",
                    Move::RequestChanges(_) => "request changes",
                    Move::Comment(_) => "comment",
                };
                *self.moves.entry(name).or_default() += 1;
            }
            Action::Look(look) => {
                self.lookups += 1;
                self.elsewhere += u32::from(look.repo != Repo::Here);
                self.behind += u32::from(world.behind(look));
            }
            Action::Restart => self.restarts += 1,
        }
        for e in told {
            *self.events.entry(e.kind()).or_default() += 1;
        }
    }
}

// ---- `Prs::see` against the model -----------------------------------------------------------

/// A world, the code's memory of it and the model's, after the same actions.
#[derive(Clone)]
struct Run {
    world: World,
    prs: Prs,
    model: Model,
    trace: Trace,
    /// Whether what's told is held to `Model` too, or only to `Trace`'s rules.
    exactly: bool,
}

impl Run {
    /// As a feed past its baseline, remembering `kept` PRs.
    fn new(kept: usize, exactly: bool) -> Run {
        Run {
            world: World::default(),
            prs: Prs::bounded(kept),
            model: Model::new(kept, true),
            trace: Trace::new(kept, true),
            exactly,
        }
    }

    /// `action` taken, the lookups going to `see`: what was told, or None for an action that
    /// changed nothing and a lookup that read nothing; Err when it isn't what the rules and the
    /// model allow.
    fn step(&mut self, action: &Action, see: &See) -> Result<Option<Vec<Event>>, String> {
        let look = match action {
            Action::Move(m) => return Ok(self.world.apply(m).then(Vec::new)),
            // The feed's to do, not `Prs`'s.
            Action::Restart => return Ok(None),
            Action::Look(look) => look,
        };
        let seen = self.world.read(look);
        if seen.elsewhere || (seen.shown.is_none() && seen.weighed.is_empty()) {
            // The feed's to skip, not `Prs`'s; or it found no PR, which changes nothing.
            return Ok(None);
        }
        let told = see(&mut self.prs, &seen);
        self.trace.check(&self.world, &seen, &told)?;
        if self.exactly {
            let owed = self.model.expect(&seen);
            if sorted(&told) != sorted(&owed) {
                return Err(format!("told {:?}, owed {:?}", sorted(&told), sorted(&owed)));
            }
            if self.prs.seen.len() != self.model.remembered() {
                let (has, owed) = (self.prs.seen.len(), self.model.remembered());
                return Err(format!("{has} PRs remembered, not {owed}"));
            }
        }
        Ok(Some(told))
    }
}

type See = dyn Fn(&mut Prs, &Seen) -> Vec<Event>;

fn see(prs: &mut Prs, seen: &Seen) -> Vec<Event> {
    prs.see(seen.shown.as_ref().map(Shown::snap), &seen.weighed())
}

/// Every sequence of up to `depth` of `actions` that each change something or read something,
/// from `run`: how many there were, or the first that went wrong.
fn explore(
    run: &Run,
    actions: &[Action],
    depth: usize,
    path: &mut Vec<Action>,
    reached: &mut Reached,
) -> Result<u64, String> {
    if depth == 0 {
        return Ok(1);
    }
    let mut count = 0;
    for action in actions {
        let mut next = run.clone();
        path.push(action.clone());
        match next.step(action, &see) {
            Ok(Some(told)) => {
                reached.note(&next.world, action, &told);
                reached.forgotten = reached.forgotten.max(next.model.forgotten);
                count += explore(&next, actions, depth - 1, path, reached)?;
            }
            Ok(None) => {}
            Err(why) => return Err(format!("{why}, after {path:#?}")),
        }
        path.pop();
    }
    Ok(count)
}

/// The actions `explore` takes: PRs 1 to `prs` opening, merging, closing and reopening (and, with
/// `says`, their checks passing and a comment), and lookups that show each or none and weigh
/// them all, in either order, as they are or all still said open, or weigh nothing.
fn alphabet(prs: u64, says: bool) -> Vec<Action> {
    let mut all = Vec::new();
    for n in 1..=prs {
        all.extend([Move::Open(n), Move::Merge(n), Move::Close(n), Move::Reopen(n)]);
        if says {
            all.extend([Move::Checks(n, Overall::Pass), Move::Comment(n)]);
        }
    }
    let mut all: Vec<Action> = all.into_iter().map(Action::Move).collect();
    let weigh = |behind: bool| (1..=prs).map(move |n| (n, behind));
    for shown in [None].into_iter().chain((1..=prs).map(Some)) {
        let lists = [
            Vec::new(),
            weigh(false).collect(),
            weigh(false).rev().collect(),
            weigh(true).collect(),
        ];
        for weighed in lists {
            all.push(Action::Look(Look { shown, weighed, repo: Repo::Here }));
        }
    }
    all
}

mod tests {
    use super::*;
    use proptest::strategy::ValueTree as _;
    use proptest::test_runner::{Config, RngSeed, TestRunner};

    /// Every sequence of five of `alphabet`'s actions on two PRs with one remembered, where a
    /// lookup that weighs both is past the bound at once, and of four on three PRs and what they
    /// say with two remembered: some 77,000 sequences. A step more of each takes eight seconds.
    /// What the alphabet leaves out (a push, a review, failing checks, a list that weighs some PRs
    /// and not others) the random sequences have.
    #[test]
    fn short_sequences_one_and_all_tell_what_the_model_owes() {
        let mut reached = Reached::default();
        for (prs, says, kept, depth) in [(2, false, 1, 5), (3, true, 2, 4)] {
            let (run, actions) = (Run::new(kept, true), alphabet(prs, says));
            let count = explore(&run, &actions, depth, &mut Vec::new(), &mut reached)
                .unwrap_or_else(|why| panic!("remembering {kept}: {why}"));
            assert!(count > 10_000, "{count} sequences");
        }
        // They got somewhere: each of these was told, a list was behind, a PR forgotten.
        for kind in ["pr_opened", "pr_closed", "pr_merged", "checks_passed", "comments"] {
            assert!(reached.events.get(kind).is_some_and(|n| *n > 0), "{kind}: {reached:?}");
        }
        assert!(reached.behind > 0 && reached.forgotten > 0, "{reached:?}");
    }

    /// Whether some random sequence (`action`, 200 of 40 actions, one seed) catches a feed that
    /// tells `see`'s way, by `Trace`'s rules alone or, with `exactly`, by `Model` too.
    fn caught(see: &See, exactly: bool) -> bool {
        let config = Config { rng_seed: RngSeed::Fixed(52), ..Config::default() };
        let mut runner = TestRunner::new(config);
        (0..200).any(|_| {
            let mut run = Run::new(2, exactly);
            (0..40).any(|_| {
                let action = action(&run.world).new_tree(&mut runner).unwrap().current();
                run.step(&action, see).is_err()
            })
        })
    }

    /// A feed that tells `more` too, of what a lookup read.
    fn adding(more: fn(&Seen) -> Vec<Event>) -> Box<See> {
        Box::new(move |prs, seen| {
            let mut told = see(prs, seen);
            for e in more(seen) {
                if !told.contains(&e) {
                    told.push(e);
                }
            }
            told
        })
    }

    /// `event` of the shown PR, `when` it's so.
    fn shown(seen: &Seen, event: fn(u64) -> Event, when: fn(&Shown) -> bool) -> Vec<Event> {
        seen.shown.iter().filter(|s| when(s)).map(|s| event(s.number)).collect()
    }

    /// `event` of each PR a list read as `state`.
    fn listed(seen: &Seen, event: fn(u64) -> Event, state: State) -> Vec<Event> {
        seen.weighed.iter().filter(|(_, s)| *s == state).map(|(n, _)| event(*n)).collect()
    }

    /// The checks aren't ones that can't fail. A feed that does one thing wrong is caught by
    /// `Trace`'s rules alone, which know nothing of how `Prs` works: so is a `Prs` with one of
    /// its two guards of memory gone. What only `Model` can know, it catches.
    #[test]
    fn a_feed_that_does_one_thing_wrong_is_caught() {
        assert!(!caught(&see, true), "the feed as it is");
        let wrong: Vec<(&str, Box<See>)> = vec![
            (
                "a merge untold",
                Box::new(|prs, seen| {
                    let told = see(prs, seen);
                    told.into_iter().filter(|e| !matches!(e, Event::PrMerged(_))).collect()
                }),
            ),
            (
                "a merged PR taken as open again from a list that's behind",
                Box::new(|prs, seen| {
                    for (n, state) in &seen.weighed {
                        if let Some(known) = prs.seen.iter_mut().find(|p| p.number == *n)
                            && (*state, &known.state) == (State::Open, &PrState::Merged)
                        {
                            known.state = PrState::Open;
                        }
                    }
                    see(prs, seen)
                }),
            ),
            (
                "the oldest PR let go as each one goes in",
                Box::new(|prs, seen| {
                    let mut told = Vec::new();
                    for one in seen.weighed() {
                        if seen.shown.as_ref().is_none_or(|s| s.number != one.0) {
                            told.extend(prs.see(None, &[one]));
                        }
                    }
                    told.extend(prs.see(seen.shown.as_ref().map(Shown::snap), &[]));
                    told
                }),
            ),
            (
                "opened at each lookup that shows it open",
                adding(|seen| shown(seen, Event::PrOpened, |s| s.state == State::Open)),
            ),
            (
                "closed at each lookup that reads it closed",
                adding(|seen| listed(seen, Event::PrClosed, State::Closed)),
            ),
            (
                "merged at each lookup that reads it merged",
                adding(|seen| listed(seen, Event::PrMerged, State::Merged)),
            ),
            (
                "approved at each lookup that shows it approved",
                adding(|seen| shown(seen, Event::Approved, |s| s.approvals > 0)),
            ),
            (
                "changes requested at each lookup that shows them",
                adding(|seen| shown(seen, Event::ChangesRequested, |s| s.change_requests > 0)),
            ),
            (
                "checks passed at each lookup that shows them passing",
                adding(|seen| shown(seen, Event::ChecksPassed, |s| s.checks == Overall::Pass)),
            ),
        ];
        for (wrong, see) in wrong {
            assert!(caught(&*see, false), "{wrong}: not caught by the rules alone");
        }
        // Nothing GitHub did says whether a check was told when it should have been.
        let untold: Box<See> = Box::new(|prs, seen| {
            let told = see(prs, seen);
            told.into_iter().filter(|e| !matches!(e, Event::ChecksPassed(_))).collect()
        });
        assert!(!caught(&*untold, false) && caught(&*untold, true), "passing checks untold");
    }
}
