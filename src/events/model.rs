//! A model of what `taskman events` should tell of a ticket's PRs, which tests drive the real code
//! against: `Prs::see` here, over every short sequence (`explore`), and the TUI's feed in
//! `app/events/feed.rs`, over random ones.
//!
//! It isn't `Prs` written twice. There are three parts, and a test compares the code with the last
//! two after every lookup:
//!
//! - `World` is the PRs as GitHub has them, moved only as GitHub lets a PR move (a merged one stays
//!   merged, a closed one can reopen), and a lookup (`Look`) reads it. The PR a lookup shows is read
//!   as it is: that's the model's one assumption about lookups, as its details come from the PR
//!   itself. A PR it only weighs can be read as it was earlier, as GitHub's search index lags.
//! - `Trace` holds the rules that need no memory of the code's kind, only what the lookups read
//!   and what was told: a merge is told once, nothing is told that didn't happen or wasn't
//!   watched happening, and an end seen straight after the PR was seen open is told.
//! - `Model` is the events owed, exactly: a map of what each remembered PR last said, by number,
//!   forgetting the ones seen longest ago past the bound.

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

/// A lookup: the PR it shows, when there is one by that number, and those it weighs, each with how
/// many states behind its list is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Look {
    pub shown: Option<u64>,
    pub weighed: Vec<(u64, u8)>,
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
            (Move::Checks(_, checks), _) if *checks != pr.checks => pr.checks = *checks,
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
        let old = |&(n, lag): &(u64, u8)| {
            lag > 0 && self.0.get(&n).is_some_and(|pr| pr.history.len() > 1)
        };
        look.repo == Repo::Here && look.weighed.iter().any(old)
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
        for &(n, lag) in &look.weighed {
            if let Some(pr) = pr(n)
                && !weighed.iter().any(|&(m, _)| m == n)
            {
                let last = pr.history.len() - 1;
                weighed.push((n, pr.history[last.saturating_sub(usize::from(lag))]));
            }
        }
        Seen { shown, weighed, elsewhere: look.repo != Repo::Here }
    }
}

// ---- the rules that need no memory ---------------------------------------------------------------

/// What the lookups that counted read, and what was told, since taskman started: enough to say
/// whether an event could be true and whether one is missing, without knowing what the code
/// remembers.
#[derive(Clone, Debug)]
pub(crate) struct Trace {
    kept: usize,
    baselined: bool,
    looks: Vec<BTreeMap<u64, State>>,
    /// Each PR told merged, and how many lookups had counted by then.
    merged: Vec<(u64, usize)>,
    /// The comments told of each PR, for as long as taskman has run or not.
    comments: BTreeMap<u64, u32>,
}

impl Trace {
    /// For a feed that remembers `kept` PRs; `baselined` when its first lookup tells too, as
    /// `Prs::see` does on its own.
    pub(crate) fn new(kept: usize, baselined: bool) -> Trace {
        Trace { kept, baselined, looks: Vec::new(), merged: Vec::new(), comments: BTreeMap::new() }
    }

    pub(crate) fn restart(&mut self) {
        (self.baselined, self.looks, self.merged) = (false, Vec::new(), Vec::new());
    }

    /// Whether `told` is something a lookup that read `seen` may tell, and all it must.
    pub(crate) fn check(&mut self, seen: &Seen, told: &[Event]) -> Result<(), String> {
        if seen.elsewhere {
            return match told {
                [] => Ok(()),
                _ => Err("a lookup of another repository's PRs, or an unknown one's, told".into()),
            };
        }
        let now = seen.states();
        if !self.baselined {
            self.baselined = true;
            self.looks.push(now);
            return if told.is_empty() { Ok(()) } else { Err("the first lookup told".into()) };
        }
        for (i, e) in told.iter().enumerate() {
            if told[..i].iter().any(|d| (d.kind(), d.pr()) == (e.kind(), e.pr())) {
                return Err(format!("{e} told twice by one lookup"));
            }
            self.may_tell(seen, &now, *e).map_err(|why| format!("{e} told, but {why}"))?;
        }
        // An end is owed for a PR the lookup before read open, when that one read no more PRs than
        // are remembered, so this one can't have been forgotten. But for a PR once read merged: a
        // list that then says open or closed is behind.
        if let Some(before) = self.looks.last()
            && before.len() <= self.kept
        {
            for (&n, &state) in &now {
                let owed = match state {
                    State::Merged => Event::PrMerged(n),
                    State::Closed => Event::PrClosed(n),
                    State::Open => continue,
                };
                let merged = self.looks.iter().any(|l| l.get(&n) == Some(&State::Merged));
                if before.get(&n) == Some(&State::Open) && !merged && !told.contains(&owed) {
                    return Err(format!("{owed} not told, the lookup before having read it open"));
                }
            }
        }
        for e in told {
            match *e {
                Event::PrMerged(n) => self.merged.push((n, self.looks.len())),
                Event::Comments { pr, count } => *self.comments.entry(pr).or_default() += count,
                _ => {}
            }
        }
        self.looks.push(now);
        Ok(())
    }

    fn may_tell(&self, seen: &Seen, now: &BTreeMap<u64, State>, e: Event) -> Result<(), String> {
        let shown = |n: u64| seen.shown.as_ref().filter(|s| s.number == n).ok_or("it isn't shown");
        let read = |n: u64, state: State| self.looks.iter().any(|l| l.get(&n) == Some(&state));
        match e {
            Event::PrOpened(n) if shown(n)?.state != State::Open => Err("it isn't open".into()),
            Event::PrOpened(_) => Ok(()),
            Event::PrMerged(n) if now.get(&n) != Some(&State::Merged) => {
                Err("this lookup didn't read it merged".into())
            }
            Event::PrMerged(n) if !read(n, State::Open) && !read(n, State::Closed) => {
                Err("no lookup read it before it merged".into())
            }
            Event::PrMerged(n) => {
                // Told before: only once it was forgotten, so after more PRs than are remembered.
                for &(_, at) in self.merged.iter().filter(|(m, _)| *m == n) {
                    let since = self.looks[at..].iter().chain([now]);
                    let mut others: Vec<u64> =
                        since.flat_map(|l| l.keys().copied()).filter(|m| *m != n).collect();
                    others.sort_unstable();
                    others.dedup();
                    if others.len() < self.kept {
                        return Err(format!("it was told before, {} other PRs ago", others.len()));
                    }
                }
                Ok(())
            }
            Event::PrClosed(n) if now.get(&n) != Some(&State::Closed) => {
                Err("this lookup didn't read it closed".into())
            }
            Event::PrClosed(n) if !read(n, State::Open) => Err("no lookup read it open".into()),
            Event::PrClosed(_) => Ok(()),
            Event::ChecksPassed(n) if shown(n)?.checks != Overall::Pass => {
                Err("its checks haven't passed".into())
            }
            Event::ChecksFailed(n) if shown(n)?.checks != Overall::Fail => {
                Err("its checks haven't failed".into())
            }
            Event::ChecksPassed(_) | Event::ChecksFailed(_) => Ok(()),
            Event::Approved(n) if shown(n)?.approvals == 0 => Err("nobody approved it".into()),
            Event::ChangesRequested(n) if shown(n)?.change_requests == 0 => {
                Err("nobody requested changes".into())
            }
            Event::Approved(_) | Event::ChangesRequested(_) => Ok(()),
            Event::Comments { pr, count } => {
                let before = self.comments.get(&pr).copied().unwrap_or(0);
                match shown(pr)?.comments {
                    all if before + count > all => Err(format!("it has {all}, {before} told")),
                    _ => Ok(()),
                }
            }
            Event::NeedsYou | Event::YourTurn | Event::Ended => Err("it's a session's".into()),
        }
    }
}

// ---- the events owed -----------------------------------------------------------------------------

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

// ---- generated sequences -------------------------------------------------------------------------

/// An action for sequences that go somewhere: a move `world` can make, or a lookup.
pub(crate) fn action(world: &World) -> impl Strategy<Value = Action> + use<> {
    let moved = proptest::sample::select(world.moves());
    let lag = prop_oneof![4 => Just(0u8), 1 => 1..3u8];
    let repo = prop_oneof![8 => Just(Repo::Here), 1 => Just(Repo::Other), 1 => Just(Repo::Unknown)];
    let look = (
        proptest::option::weighted(0.8, 1..=PRS),
        proptest::collection::vec((1..=PRS, lag), 0..=4),
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

// ---- `Prs::see` against the model ----------------------------------------------------------------

/// A world, the code's memory of it and the model's, after the same actions.
#[derive(Clone)]
struct Run {
    world: World,
    prs: Prs,
    model: Model,
    trace: Trace,
}

impl Run {
    /// As a feed past its baseline, remembering `kept` PRs.
    fn new(kept: usize) -> Run {
        Run {
            world: World::default(),
            prs: Prs::bounded(kept),
            model: Model::new(kept, true),
            trace: Trace::new(kept, true),
        }
    }

    /// `action` taken, the lookups going to `see`: what was told, or None for an action that
    /// changed nothing and a lookup that read nothing; Err when it isn't what the model and the rules allow.
    fn step(&mut self, action: &Action, see: &See) -> Result<Option<Vec<Event>>, String> {
        let look = match action {
            Action::Move(m) => return Ok(self.world.apply(m).then(Vec::new)),
            Action::Restart => return Ok(None),
            Action::Look(look) => look,
        };
        let seen = self.world.read(look);
        if seen.elsewhere || (seen.shown.is_none() && seen.weighed.is_empty()) {
            // The feed's to skip, not `Prs`'s; or it found no PR, which changes nothing.
            return Ok(None);
        }
        let told = see(&mut self.prs, &seen);
        self.trace.check(&seen, &told)?;
        let owed = self.model.expect(&seen);
        if sorted(&told) != sorted(&owed) {
            return Err(format!("told {:?}, owed {:?}", sorted(&told), sorted(&owed)));
        }
        if self.prs.seen.len() != self.model.remembered() {
            let (has, owed) = (self.prs.seen.len(), self.model.remembered());
            return Err(format!("{has} PRs remembered, not {owed}"));
        }
        Ok(Some(told))
    }
}

type See = dyn Fn(&mut Prs, &Seen) -> Vec<Event>;

fn see(prs: &mut Prs, seen: &Seen) -> Vec<Event> {
    prs.see(seen.shown.as_ref().map(Shown::snap), &seen.weighed())
}

/// Every sequence of up to `depth` of `actions` that each change something or look, from `run`:
/// how many there were, or the first that went wrong.
fn explore(
    run: &Run,
    actions: &[Action],
    depth: usize,
    see: &See,
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
        match next.step(action, see) {
            Ok(Some(told)) => {
                reached.note(&next.world, action, &told);
                reached.forgotten = reached.forgotten.max(next.model.forgotten);
                count += explore(&next, actions, depth - 1, see, path, reached)?;
            }
            Ok(None) => {}
            Err(why) => return Err(format!("{why}, after {path:#?}")),
        }
        path.pop();
    }
    Ok(count)
}

/// The actions `explore` takes: each move of PRs 1 to `prs` (what they say too, with `says`), and
/// lookups that show each or none and weigh them all, in either order, as they are or one state
/// behind, or weigh nothing.
fn alphabet(prs: u64, says: bool) -> Vec<Action> {
    let mut all = Vec::new();
    for n in 1..=prs {
        all.extend([Move::Open(n), Move::Merge(n), Move::Close(n), Move::Reopen(n)]);
        if says {
            all.extend([Move::Checks(n, Overall::Pass), Move::Comment(n)]);
        }
    }
    let mut all: Vec<Action> = all.into_iter().map(Action::Move).collect();
    let weigh = |lag: u8| (1..=prs).map(move |n| (n, lag));
    for shown in [None].into_iter().chain((1..=prs).map(Some)) {
        let lists = [Vec::new(), weigh(0).collect(), weigh(0).rev().collect(), weigh(1).collect()];
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

    /// Every sequence of five actions on two PRs with one remembered, where a lookup that weighs
    /// both is past the bound at once, and of four on three PRs and what they say with two
    /// remembered. A step more of each takes eight seconds.
    #[test]
    fn every_short_sequence_tells_what_the_model_owes() {
        let mut reached = Reached::default();
        for (prs, says, kept, depth) in [(2, false, 1, 5), (3, true, 2, 4)] {
            let (run, actions) = (Run::new(kept), alphabet(prs, says));
            let count = explore(&run, &actions, depth, &see, &mut Vec::new(), &mut reached)
                .unwrap_or_else(|why| panic!("remembering {kept}: {why}"));
            assert!(count > 10_000, "{count} sequences");
        }
        // They got somewhere: each of these was told, a list was behind, a PR forgotten.
        for kind in ["pr_opened", "pr_closed", "pr_merged", "checks_passed", "comments"] {
            assert!(reached.events.get(kind).is_some_and(|n| *n > 0), "{kind}: {reached:?}");
        }
        assert!(reached.behind > 0 && reached.forgotten > 0, "{reached:?}");
    }

    /// The comparison isn't one that can't fail: a feed that does one thing wrong is caught.
    #[test]
    fn a_feed_that_does_one_thing_wrong_is_caught() {
        let wrong: [(&str, Box<See>); 4] = [
            (
                "merges untold",
                Box::new(|prs, seen| {
                    let told = see(prs, seen);
                    told.into_iter().filter(|e| !matches!(e, Event::PrMerged(_))).collect()
                }),
            ),
            (
                "a merge told of a PR only listed",
                Box::new(|prs, seen| {
                    let mut told = see(prs, seen);
                    told.extend(seen.weighed.first().map(|(n, _)| Event::PrMerged(*n)));
                    told
                }),
            ),
            (
                "a PR that isn't shown told opened",
                Box::new(|prs, seen| {
                    let mut told = see(prs, seen);
                    let unseen = |n: &u64| seen.shown.as_ref().is_none_or(|s| s.number != *n);
                    told.extend(
                        seen.weighed.iter().map(|(n, _)| *n).find(unseen).map(Event::PrOpened),
                    );
                    told
                }),
            ),
            (
                "passing checks untold",
                Box::new(|prs, seen| {
                    // Nothing a rule forbids, but not all that's owed.
                    let told = see(prs, seen);
                    told.into_iter().filter(|e| !matches!(e, Event::ChecksPassed(_))).collect()
                }),
            ),
        ];
        for (wrong, see) in wrong {
            let mut reached = Reached::default();
            let found =
                explore(&Run::new(2), &alphabet(3, true), 4, &*see, &mut Vec::new(), &mut reached);
            assert!(found.is_err(), "{wrong}: not caught");
        }
    }

    /// The random sequences (`action`), as `app/events/feed.rs` runs them through the TUI's feed,
    /// with one seed: they make every move, tell every event, read lists that are behind and
    /// other repositories, restart and forget. And `Prs::see` agrees with the model on them.
    #[test]
    fn the_random_sequences_reach_every_move_and_event() {
        let config = Config { rng_seed: RngSeed::Fixed(52), ..Config::default() };
        let mut runner = TestRunner::new(config);
        let mut reached = Reached::default();
        for _ in 0..64 {
            let mut run = Run::new(3);
            let mut actions = Vec::new();
            for _ in 0..40 {
                let action = action(&run.world).new_tree(&mut runner).unwrap().current();
                actions.push(action.clone());
                let told = run
                    .step(&action, &see)
                    .unwrap_or_else(|why| panic!("{why}, after {actions:#?}"));
                reached.note(&run.world, &action, &told.unwrap_or_default());
            }
            reached.forgotten += run.model.forgotten;
        }
        for name in [
            "open",
            "merge",
            "close",
            "reopen",
            "push",
            "checks",
            "approve",
            "request changes",
            "comment",
        ] {
            assert!(reached.moves.get(name).is_some_and(|n| *n > 0), "{name}: {reached:?}");
        }
        for kind in [
            "pr_opened",
            "pr_closed",
            "pr_merged",
            "checks_passed",
            "checks_failed",
            "approved",
            "changes_requested",
            "comments",
        ] {
            assert!(reached.events.get(kind).is_some_and(|n| *n > 0), "{kind}: {reached:?}");
        }
        let rest = [reached.lookups, reached.behind, reached.elsewhere, reached.restarts];
        assert!(rest.iter().all(|n| *n > 0) && reached.forgotten > 0, "{reached:?}");
    }
}
