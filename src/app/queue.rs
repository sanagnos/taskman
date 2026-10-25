//! `prefix w`: every session waiting on the user, oldest first, with why and for how long. ↑/↓
//! choose a row and Enter selects its session. Answering from here is left out on purpose: a
//! Claude prompt has more choices than yes and no, and one key approving across sessions would
//! widen what a key approves.

use super::{AgentState, App, Mode};
use crate::keys;
use crossterm::event::{KeyCode, KeyEvent};

/// The popover while it's open; the rows are worked out from the sessions each time, so they
/// follow the hooks as they arrive.
#[derive(Clone, Copy, Debug, Default)]
pub struct Queue {
    /// The chosen row.
    pub at: usize,
}

/// Indexes of `waiting` (an index into `App::tickets` and when the session began waiting) oldest
/// first. A session that waits without a recorded time goes last, in the sidebar's order.
pub fn oldest_first(waiting: &[(usize, Option<i64>)]) -> Vec<usize> {
    let mut rows = waiting.to_vec();
    rows.sort_by_key(|&(i, since)| (since.is_none(), since, i));
    rows.into_iter().map(|(i, _)| i).collect()
}

impl App {
    /// The sessions waiting on the user, oldest first, as indexes into `tickets`. Only those that
    /// ask something: a finished turn you haven't looked at is news, not a request.
    pub fn queue(&self) -> Vec<usize> {
        let waiting: Vec<_> = self
            .ordered(false)
            .into_iter()
            .filter(|&i| {
                let tv = &self.tickets[i];
                tv.status.state == AgentState::Needs && tv.needs_attention()
            })
            .map(|i| (i, self.tickets[i].status.waiting_since))
            .collect();
        oldest_first(&waiting)
    }

    /// What the footer counts: the other sessions that ask you something, and those whose turn
    /// finished or that ended since you last looked at them (`TicketView::needs_attention`).
    pub fn wanting(&self) -> (usize, usize) {
        let others = self.tickets.iter().enumerate();
        let wanting = others.filter(|&(i, tv)| i != self.sel && tv.needs_attention());
        let needs = wanting.clone().filter(|(_, tv)| tv.status.state == AgentState::Needs).count();
        (needs, wanting.count() - needs)
    }

    pub(super) fn open_queue(&mut self) {
        if self.queue().is_empty() {
            let msg = match self.wanting().1 {
                0 => "Nothing needs you".to_string(),
                n => {
                    let p = keys::prefix();
                    let them = if n == 1 { "it" } else { "them" };
                    format!("Nothing needs you; {n} finished: {p} {p} goes to {them}")
                }
            };
            self.toast(msg);
        } else {
            self.mode = Mode::Queue(Queue::default());
        }
    }

    pub(super) fn queue_key(&mut self, key: KeyEvent) {
        let Mode::Queue(q) = self.mode else { return };
        let rows = self.queue();
        let last = rows.len().saturating_sub(1);
        // The list may have shrunk since the last key.
        let at = q.at.min(last);
        let to = match key.code {
            KeyCode::Up | KeyCode::Char('k') => at.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (at + 1).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => at,
        };
        self.mode = Mode::Queue(Queue { at: to });
        match key.code {
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                if let Some(&i) = rows.get(at) {
                    self.select(i);
                    self.to_live(i);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_wait_comes_first_and_an_unknown_one_last() {
        let waiting = [(0, Some(300)), (1, None), (2, Some(100)), (3, Some(300)), (4, None)];
        assert_eq!(oldest_first(&waiting), [2, 0, 3, 1, 4]);
        assert!(oldest_first(&[]).is_empty());
    }
}
