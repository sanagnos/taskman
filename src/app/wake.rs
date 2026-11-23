//! The TUI's side of `taskman wake`: starts a ticket's sleeping or not yet started session in its
//! pane, without selecting it, and answers once the session's CLI has reported its start, so the
//! agent that asked can message it. A wake waits in `App::waking`, one per ticket, until it's
//! answered: up, refused, too slow (still starting), or taskman quitting.

use super::{App, Id, TicketView};
use crate::add::{self, Item, Line, Request};
use crate::{untrusted, util};
use std::sync::atomic::Ordering;

/// How long before the command stops waiting a wake still starting is answered so: the reply
/// must land while the command reads it (`add::reply` writes none after `reply_by`).
const MARGIN: i64 = 2;

/// A wake taken from the inbox and not answered yet.
pub(super) struct Waking {
    id: String,
    req: Request,
    ticket: Id,
    /// The start the session had when the wake came, if any: a start after it is the wake's.
    launch: Option<String>,
}

/// What a wake does for a ticket that can be woken.
#[derive(Debug, PartialEq)]
pub(super) enum Wake {
    /// Its session is dormant (asleep, or not started since taskman did): start it.
    Start,
    /// It's running already.
    Awake,
}

/// What a wake does with `tv`, or why it doesn't: by the session's state alone.
pub(super) fn wake_answer(tv: &TicketView) -> Result<Wake, String> {
    if tv.t.agent.is_some() {
        return Err("it's an agent, and agents don't sleep".into());
    }
    if tv.away.is_some() {
        return Err("its session runs in another terminal (taskman resume); the user brings it \
                    back to taskman"
            .into());
    }
    if !tv.t.cli.messages_agents() {
        let cli = tv.t.cli.label();
        return Err(format!("it runs in {cli}, whose sessions can't be messaged"));
    }
    let running = tv.session.as_ref().is_some_and(|s| !s.exited());
    if running {
        return Ok(Wake::Awake);
    }
    // Ended, or couldn't start: the pane says why, which the user should see before it runs again.
    let ended = tv.session.is_some()
        || tv.status.state == super::AgentState::Exited
        || tv.gone.is_some()
        || tv.lock_error.is_some();
    if ended {
        return Err("its session ended; taskman shows the user why, and they start it again".into());
    }
    if tv.waiting() {
        return Err("its session waits for its worktree; try again once taskman has made it".into());
    }
    if tv.dormant() {
        return Ok(Wake::Start);
    }
    Err("its session isn't running, and only the user can start it from here".into())
}

impl App {
    /// A `taskman wake` request (`queued`: taken after the command stopped waiting, which it
    /// does only if taskman quit or hung meanwhile; nobody waits for that session, so it's
    /// dropped).
    pub(super) fn wake_requested(&mut self, id: String, req: Request, queued: bool) {
        if queued {
            return;
        }
        // The command sends one, but anything can write to the inbox.
        let [Item::Wake { key }] = &req.items[..] else {
            add::reply(&id, &req, &[Line::err("a wake names one ticket and nothing else")]);
            return;
        };
        let Some(i) = self.tickets.iter().position(|tv| tv.t.is(&req.repo, key)) else {
            let msg = format!("{} isn't open in taskman", untrusted::line(key));
            add::reply(&id, &req, &[Line::err(msg)]);
            return;
        };
        let tv = &self.tickets[i];
        let ticket: Id = (tv.t.repo.clone(), tv.t.key.clone());
        let (shown, name) = (tv.t.display().to_string(), self.message_name(i));
        // One wake waits per ticket, so the list never outgrows the tickets.
        if self.waking.iter().any(|w| w.ticket == ticket) {
            let msg = format!(
                "{shown} is already starting for an earlier wake; message it as {name} once \
                 ListAgents lists it"
            );
            add::reply(&id, &req, &[Line::ok(msg)]);
            return;
        }
        match wake_answer(tv) {
            Err(e) => add::reply(&id, &req, &[Line::err(format!("{shown} wasn't woken: {e}"))]),
            Ok(Wake::Awake) => {
                let msg = format!("{shown} is awake; message it as {name}");
                add::reply(&id, &req, &[Line::ok(msg)]);
            }
            Ok(Wake::Start) => {
                let launch = tv.launch.clone();
                let tv = &mut self.tickets[i];
                // `ensure_selected_session` starts it, once its last CLI is gone.
                (tv.start, tv.woken) = (true, true);
                self.waking.push(Waking { id, req, ticket, launch });
                self.dirty.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Answers the wakes whose sessions are up, couldn't start, or are taking too long. Runs on
    /// every status poll, after the hooks' reports are read.
    pub(super) fn answer_wakes(&mut self) {
        let now = util::now();
        let waking = std::mem::take(&mut self.waking);
        for w in waking {
            match self.wake_state(&w, now) {
                Some(line) => add::reply(&w.id, &w.req, &[line]),
                None => self.waking.push(w),
            }
        }
    }

    /// The answer for wake `w` at `now`, or None while it waits.
    fn wake_state(&self, w: &Waking, now: i64) -> Option<Line> {
        let Some(i) = self.tickets.iter().position(|tv| tv.t.is(&w.ticket.0, &w.ticket.1)) else {
            let key = untrusted::line(&w.ticket.1);
            return Some(Line::err(format!("{key} was closed before its session was up")));
        };
        let tv = &self.tickets[i];
        let (shown, name) = (tv.t.display(), self.message_name(i));
        let started = tv.session.is_some() && tv.launch != w.launch;
        if started && tv.session.as_ref().is_some_and(|s| s.exited()) {
            return Some(Line::err(format!(
                "{shown}'s session ended as it started; taskman shows the user why"
            )));
        }
        if started && tv.status.reported() {
            return Some(Line::ok(format!("{shown} is awake; message it as {name}")));
        }
        if !started && !tv.start {
            // The start was tried and didn't happen: taken in another terminal meanwhile, or
            // refused (a lock, a folder that's gone), which its pane says.
            return Some(Line::err(match tv.away {
                Some(_) => format!("{shown} runs in another terminal now; it wasn't woken"),
                None => format!("{shown} couldn't start; taskman shows the user why in its pane"),
            }));
        }
        (now >= w.req.reply_by - MARGIN).then(|| {
            Line::ok(format!("{shown} is starting; message it as {name} once ListAgents lists it"))
        })
    }

    /// Answers every wake still waiting: taskman is quitting, so none of them will be up.
    pub(super) fn end_wakes(&mut self) {
        for w in std::mem::take(&mut self.waking) {
            let key = untrusted::line(&w.ticket.1);
            add::reply(&w.id, &w.req, &[Line::err(format!("taskman quit before {key} was up"))]);
        }
    }

    /// The name ticket `i`'s session is messaged by: the one it started with, else the one it
    /// will start with.
    fn message_name(&self, i: usize) -> String {
        let tv = &self.tickets[i];
        tv.name.clone().unwrap_or_else(|| self.qualified(&tv.t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AgentState;
    use crate::app::launch::Gone;
    use crate::app::test_support::ticket;
    use crate::app::ticket::Away;
    use crate::session::Session;
    use crate::store::{Cli, Ticket};

    fn view(t: Ticket) -> TicketView {
        TicketView::new(t)
    }

    #[test]
    fn starts_only_a_dormant_ticket_a_session_can_message() {
        // Not started since taskman did, or asleep: started.
        assert_eq!(wake_answer(&view(ticket())), Ok(Wake::Start));
        let mut asleep = view(ticket());
        (asleep.asleep, asleep.status.state) = (true, AgentState::Done);
        assert_eq!(wake_answer(&asleep), Ok(Wake::Start));
        // Running: nothing to do.
        let mut running = view(ticket());
        running.session = Some(Session::fake(24, 80).0);
        assert_eq!(wake_answer(&running), Ok(Wake::Awake));

        let refused = |tv: &TicketView, why: &str| {
            let e = wake_answer(tv).unwrap_err();
            assert!(e.contains(why), "{e}");
        };
        refused(&view(Ticket { agent: Some("lead".into()), ..ticket() }), "agents don't sleep");
        refused(&view(Ticket { cli: Cli::Codex, ..ticket() }), "can't be messaged");
        for away in [Away::HandedOff, Away::Outside, Away::Back] {
            let mut tv = view(ticket());
            (tv.asleep, tv.away) = (true, Some(away));
            refused(&tv, "another terminal");
        }
        // Ended, or couldn't start: the user sees why first.
        let mut ended = view(ticket());
        let (mut session, _) = Session::fake(24, 80);
        session.exit(1);
        ended.session = Some(session);
        refused(&ended, "ended");
        let mut exited = view(ticket());
        exited.status.state = AgentState::Exited;
        refused(&exited, "ended");
        let mut gone = view(ticket());
        gone.gone = Some(Gone::Worktree);
        refused(&gone, "ended");
        let mut locked = view(ticket());
        locked.lock_error = Some("read-only".into());
        refused(&locked, "ended");
        let mut creating = view(ticket());
        creating.creating = true;
        refused(&creating, "worktree");
    }
}
