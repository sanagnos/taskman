//! Reordering the sidebar, by keys (`prefix {` and `}`, then move mode) and by dragging. A card
//! moves among the cards of its own group (a project's agents, or its tickets) and a project
//! among the projects; neither crosses into another group. Both inputs end in `place`, and the
//! order is nothing but the order of `App::tickets` and `App::projects`, saved as they are.

use super::{App, Id, Mode};
use crate::store::Ticket;
use ratatui::layout::{Position, Rect};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How often the sidebar scrolls while a drag is held at its top or bottom edge.
const SCROLL_EVERY: Duration = Duration::from_millis(80);

/// What a move moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unit {
    Card,
    Project,
}

/// The thing being moved, by name: its place changes with every step.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Card(Id),
    Project(PathBuf),
}

/// The order of everything, as it was when move mode began; Esc puts it back.
struct Order {
    tickets: Vec<Id>,
    projects: Vec<PathBuf>,
}

/// Move mode (`Mode::Move`): the selected card, or its project, moves with the arrow keys.
pub struct Moving {
    pub unit: Unit,
    before: Order,
}

/// A mouse drag in the sidebar, from the press on a card or a project's header.
pub struct Drag {
    pub piece: Piece,
    /// The rows the press landed on: the drag starts once the pointer leaves them, so a click is
    /// a click.
    grip: Rect,
    pub active: bool,
    pub pointer: Position,
    /// Where a drop would put the piece, once the drag has started.
    pub target: Option<Target>,
    pub(super) scrolled_at: Option<Instant>,
}

/// A place between the group's members: before member `gap` (the group's length for the end),
/// and the screen row the landing line is drawn on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub gap: usize,
    pub line: u16,
}

/// Moves `v[group[from]]` to where `v[group[to]]` is, the group's members between them shifting
/// by one and everything outside the group keeping its order. `group` lists the members' indices
/// in `v`, ascending.
pub fn place<T>(v: &mut Vec<T>, group: &[usize], from: usize, to: usize) {
    if from == to || from >= group.len() || to >= group.len() {
        return;
    }
    let item = v.remove(group[from]);
    v.insert(group[to], item);
}

/// The place in the group a piece ends up in when dropped in gap `gap` (before member `gap`), or
/// None when that leaves it where it is: the gaps on either side of it are the same place.
pub fn slot_for_gap(from: usize, gap: usize) -> Option<usize> {
    if gap == from || gap == from + 1 {
        return None;
    }
    Some(if gap > from { gap - 1 } else { gap })
}

/// The gap nearest to row `y`, among those the visible members `spans` (their top and bottom
/// rows, `None` when scrolled out of view) bound. A gap is the blank between two members, or the
/// edge of the first or last; the landing line goes on its first row, or just above the member
/// that follows when there's no row between.
pub fn nearest_gap(spans: &[Option<(u16, u16)>], y: u16) -> Option<Target> {
    let n = spans.len();
    // Doubled, so the middle of a gap and of a row are whole numbers.
    let at = i32::from(y) * 2 + 1;
    (0..=n)
        .filter_map(|gap| {
            let before = gap.checked_sub(1).and_then(|k| spans[k]);
            let after = spans.get(gap).copied().flatten();
            let (pos, line) = match (before, after) {
                (Some((_, bottom)), Some((top, _))) => {
                    (i32::from(bottom) + i32::from(top), bottom.min(top.saturating_sub(1)))
                }
                (None, Some((top, _))) => (i32::from(top) * 2, top.saturating_sub(1)),
                (Some((_, bottom)), None) => (i32::from(bottom) * 2, bottom),
                (None, None) => return None,
            };
            // A gap beside a hidden member would stand for a place further off.
            let hidden = (gap > 0 && before.is_none()) || (gap < n && after.is_none());
            (!hidden).then_some(((pos - at).abs(), Target { gap, line }))
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, t)| t)
}

impl App {
    /// The members of `piece`'s group (indices into `tickets` or `projects`, in order) and the
    /// piece's place among them.
    fn group_of(&self, piece: &Piece) -> Option<(Vec<usize>, usize)> {
        let (members, i): (Vec<usize>, usize) = match piece {
            Piece::Card(id) => {
                let i = self.tickets.iter().position(|tv| tv.t.is(&id.0, &id.1))?;
                let (p, agent) =
                    (self.project_of(&self.tickets[i].t), self.tickets[i].t.agent.is_some());
                let same = |t: &Ticket| self.project_of(t) == p && t.agent.is_some() == agent;
                ((0..self.tickets.len()).filter(|&j| same(&self.tickets[j].t)).collect(), i)
            }
            Piece::Project(repo) => (
                (0..self.projects.len()).collect(),
                self.projects.iter().position(|p| &p.repo == repo)?,
            ),
        };
        let at = members.iter().position(|&m| m == i)?;
        Some((members, at))
    }

    /// The piece a unit means right now: the selected card, or its project.
    fn piece_of(&self, unit: Unit) -> Option<Piece> {
        let t = &self.selected()?.t;
        Some(match unit {
            Unit::Card => Piece::Card((t.repo.clone(), t.key.clone())),
            Unit::Project => Piece::Project(self.projects.get(self.project_of(t)?)?.repo.clone()),
        })
    }

    fn selected_id(&self) -> Option<Id> {
        self.selected().map(|tv| (tv.t.repo.clone(), tv.t.key.clone()))
    }

    /// Moves `piece` to be member `to` of its group, keeping the selection on the same card, and
    /// saves. False when it's there already.
    fn move_piece(&mut self, piece: &Piece, to: usize) -> bool {
        let Some((group, from)) = self.group_of(piece) else { return false };
        if from == to || to >= group.len() {
            return false;
        }
        let keep = self.selected_id();
        match piece {
            Piece::Card(_) => place(&mut self.tickets, &group, from, to),
            Piece::Project(_) => place(&mut self.projects, &group, from, to),
        }
        self.follow(keep);
        self.sidebar_scroll.asked = true;
        self.save();
        true
    }

    /// Points `sel` at card `id` again after the cards moved.
    fn follow(&mut self, id: Option<Id>) {
        if let Some(i) =
            id.and_then(|(repo, key)| self.tickets.iter().position(|tv| tv.t.is(&repo, &key)))
        {
            self.sel = i;
        }
    }

    /// One place up or down; false at the edge of the group.
    fn nudge(&mut self, unit: Unit, dir: isize) -> bool {
        let Some(piece) = self.piece_of(unit) else { return false };
        let Some((group, at)) = self.group_of(&piece) else { return false };
        match at.checked_add_signed(dir).filter(|&to| to < group.len()) {
            Some(to) => self.move_piece(&piece, to),
            None => false,
        }
    }

    // ---- move mode ------------------------------------------------------------------------

    fn order(&self) -> Order {
        Order {
            tickets: self.tickets.iter().map(|tv| (tv.t.repo.clone(), tv.t.key.clone())).collect(),
            projects: self.projects.iter().map(|p| p.repo.clone()).collect(),
        }
    }

    fn restore(&mut self, o: &Order) {
        let keep = self.selected_id();
        // Stable, and anything added since goes last.
        self.tickets.sort_by_key(|tv| {
            o.tickets.iter().position(|id| tv.t.is(&id.0, &id.1)).unwrap_or(usize::MAX)
        });
        self.projects
            .sort_by_key(|p| o.projects.iter().position(|r| *r == p.repo).unwrap_or(usize::MAX));
        self.follow(keep);
        self.sidebar_scroll.asked = true;
        self.save();
    }

    /// `prefix {` and `prefix }`: starts move mode with the first step.
    pub(super) fn start_move(&mut self, dir: isize) {
        if self.tickets.is_empty() {
            return;
        }
        self.drag = None;
        self.mode = Mode::Move(Box::new(Moving { unit: Unit::Card, before: self.order() }));
        self.nudge(Unit::Card, dir);
    }

    pub(super) fn move_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Mode::Move(m) = &mut self.mode else { return };
        let unit = m.unit;
        match key.code {
            KeyCode::Up | KeyCode::Char('{') => _ = self.nudge(unit, -1),
            KeyCode::Down | KeyCode::Char('}') => _ = self.nudge(unit, 1),
            KeyCode::Char('p') => {
                m.unit = Unit::Project;
                self.sidebar_scroll.asked = true;
            }
            KeyCode::Esc => {
                let Mode::Move(m) = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
                self.restore(&m.before);
            }
            // Enter or anything else keeps the order; the key is spent on ending the mode.
            _ => self.mode = Mode::Normal,
        }
    }

    // ---- dragging -------------------------------------------------------------------------

    /// A press on a card (`Hit::Ticket`) or a project's header: a drag, once the pointer leaves
    /// `grip`.
    pub(super) fn press(&mut self, piece: Piece, grip: Rect, pointer: Position) {
        self.drag =
            Some(Drag { piece, grip, active: false, pointer, target: None, scrolled_at: None });
    }

    /// The pointer moved with the button held.
    pub(super) fn drag_to(&mut self, pos: Position) {
        let Some(d) = self.drag.as_mut() else { return };
        d.pointer = pos;
        if !d.active && !d.grip.contains(pos) {
            d.active = true;
        }
        self.retarget();
    }

    /// Where a drop would land, from where the sidebar was last drawn.
    pub(crate) fn retarget(&mut self) {
        let Some(d) = self.drag.as_ref().filter(|d| d.active) else { return };
        let Some((group, _)) = self.group_of(&d.piece) else { return };
        let spans: Vec<Option<(u16, u16)>> = group
            .iter()
            .map(|&i| {
                let hit = match d.piece {
                    Piece::Card(_) => super::Hit::Ticket(i),
                    Piece::Project(_) => super::Hit::Project(i),
                };
                self.sidebar_hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| (r.y, r.bottom()))
            })
            .collect();
        let target = nearest_gap(&spans, d.pointer.y);
        if let Some(d) = self.drag.as_mut() {
            d.target = target;
        }
    }

    /// The button came up: drops an active drag where it points. False when it wasn't one, so the
    /// press was a click.
    pub(super) fn drop_drag(&mut self) -> bool {
        self.retarget();
        let Some(d) = self.drag.take().filter(|d| d.active) else { return false };
        if let (Some(t), Some((_, from))) = (d.target, self.group_of(&d.piece))
            && let Some(to) = slot_for_gap(from, t.gap)
        {
            self.move_piece(&d.piece, to);
        }
        true
    }

    /// Esc, or any key, ends a drag; true when it was a drag under way and the key is spent.
    pub(super) fn cancel_drag(&mut self, esc: bool) -> bool {
        let active = self.drag.as_ref().is_some_and(|d| d.active);
        self.drag = None;
        active && esc
    }

    /// Scrolls the sidebar while the pointer rests at its top or bottom edge. Runs on every tick.
    pub(super) fn follow_drag(&mut self) {
        let sb = self.areas.sidebar;
        let (first, last) = (sb.y + 2, sb.bottom().saturating_sub(2));
        let Some(d) = self.drag.as_mut().filter(|d| d.active) else { return };
        if d.scrolled_at.is_some_and(|t| t.elapsed() < SCROLL_EVERY) {
            return;
        }
        let top = &mut self.sidebar_scroll.top;
        if d.pointer.y <= first && *top > 0 {
            *top -= 1;
        } else if d.pointer.y >= last {
            *top += 1;
        } else {
            return;
        }
        d.scrolled_at = Some(Instant::now());
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // ---- what the screen shows ------------------------------------------------------------

    /// Whether every project is drawn folded: while one is moved, so each step passes one.
    pub fn moving_projects(&self) -> bool {
        matches!(&self.mode, Mode::Move(m) if m.unit == Unit::Project)
            || self.drag.as_ref().is_some_and(|d| d.active && matches!(d.piece, Piece::Project(_)))
    }

    /// The piece a drag under way holds.
    pub fn dragged(&self) -> Option<&Piece> {
        self.drag.as_ref().filter(|d| d.active).map(|d| &d.piece)
    }

    fn name_of(&self, piece: &Piece) -> Option<String> {
        match piece {
            Piece::Card(id) => self
                .tickets
                .iter()
                .find(|tv| tv.t.is(&id.0, &id.1))
                .map(|tv| tv.t.display().to_string()),
            Piece::Project(repo) => {
                self.projects.iter().find(|p| &p.repo == repo).map(|p| p.name.clone())
            }
        }
    }

    /// The footer's words while something moves.
    pub fn reorder_hint(&self) -> Option<String> {
        if let Mode::Move(m) = &self.mode {
            let piece = self.piece_of(m.unit)?;
            let what = self.name_of(&piece)?;
            return Some(match m.unit {
                Unit::Card => {
                    format!("moving {what} · ↑/↓ move · p project · Enter done · Esc undo")
                }
                Unit::Project => {
                    format!("moving project {what} · ↑/↓ move · Enter done · Esc undo")
                }
            });
        }
        let d = self.drag.as_ref().filter(|d| d.active)?;
        let what = self.name_of(&d.piece)?;
        let (group, from) = self.group_of(&d.piece)?;
        let next = |i: usize| match d.piece {
            Piece::Card(_) => self.tickets.get(i).map(|tv| tv.t.display().to_string()),
            Piece::Project(_) => self.projects.get(i).map(|p| p.name.clone()),
        };
        let at = match d.target {
            Some(t) if slot_for_gap(from, t.gap).is_some() => match t.gap.checked_sub(1) {
                Some(k) => format!("below {}", next(group[k])?),
                None => format!("above {}", next(group[0])?),
            },
            _ => "· no change".to_string(),
        };
        let kind = if matches!(d.piece, Piece::Project(_)) { "project " } else { "" };
        Some(format!("move {kind}{what} {at} · Esc cancels"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::acme_app;

    /// The tickets' keys in the order they're held.
    fn eng(app: &App) -> Vec<&str> {
        let all = app.tickets.iter().map(|tv| tv.t.key.as_str());
        all.filter(|k| k.starts_with("ENG")).collect()
    }

    #[test]
    fn place_moves_one_member_and_leaves_the_rest() {
        // Members at 0, 2, 3 of a list with strangers between.
        let mut v = vec!['a', 'x', 'b', 'c', 'y'];
        place(&mut v, &[0, 2, 3], 0, 2);
        assert_eq!(v, ['x', 'b', 'c', 'a', 'y']);
        let mut v = vec!['a', 'x', 'b', 'c', 'y'];
        place(&mut v, &[0, 2, 3], 2, 0);
        assert_eq!(v, ['c', 'a', 'x', 'b', 'y']);
        // Nowhere, or out of the group: nothing.
        let mut v = vec!['a', 'b'];
        place(&mut v, &[0, 1], 1, 1);
        place(&mut v, &[0, 1], 0, 5);
        assert_eq!(v, ['a', 'b']);
    }

    #[test]
    fn a_drop_either_side_of_the_piece_changes_nothing() {
        assert_eq!(slot_for_gap(2, 2), None);
        assert_eq!(slot_for_gap(2, 3), None);
        assert_eq!(slot_for_gap(2, 0), Some(0));
        assert_eq!(slot_for_gap(2, 1), Some(1));
        assert_eq!(slot_for_gap(2, 4), Some(3));
        assert_eq!(slot_for_gap(0, 3), Some(2));
    }

    #[test]
    fn the_nearest_gap_snaps_to_a_place_between_members() {
        // Three cards of three rows with a blank row between: rows 2-4, 6-8, 10-12.
        let spans = [Some((2, 5)), Some((6, 9)), Some((10, 13))];
        let gap = |y| nearest_gap(&spans, y).unwrap();
        // The top of a card is before it, the bottom after it.
        assert_eq!(gap(2), Target { gap: 0, line: 1 });
        assert_eq!(gap(4), Target { gap: 1, line: 5 });
        assert_eq!(gap(6), Target { gap: 1, line: 5 });
        assert_eq!(gap(8), Target { gap: 2, line: 9 });
        // Past either end, and far from the list altogether: the edges.
        assert_eq!(gap(0), Target { gap: 0, line: 1 });
        assert_eq!(gap(12), Target { gap: 3, line: 13 });
        assert_eq!(gap(200), Target { gap: 3, line: 13 });
    }

    #[test]
    fn gaps_beside_hidden_members_are_not_offered() {
        // The first member is scrolled out: the gaps around it would be somewhere else.
        let spans = [None, Some((2, 5)), Some((6, 9))];
        assert_eq!(nearest_gap(&spans, 0).unwrap().gap, 2);
        assert_eq!(nearest_gap(&[None, None], 3), None);
        assert_eq!(nearest_gap(&[], 3), None);
    }

    #[test]
    fn cards_move_within_their_group_and_stop_at_its_edge() {
        let mut app = acme_app();
        // ENG-52 is the first ticket: up is the edge, though an agent sits above it.
        assert!(!app.nudge(Unit::Card, -1));
        assert_eq!(eng(&app), ["ENG-52", "ENG-53", "ENG-61", "ENG-70"]);
        assert!(app.nudge(Unit::Card, 1));
        assert!(app.nudge(Unit::Card, 1));
        assert_eq!(eng(&app), ["ENG-53", "ENG-61", "ENG-52", "ENG-70"]);
        assert_eq!(app.tickets[app.sel].t.key, "ENG-52", "the selection goes with the card");
        assert!(app.nudge(Unit::Card, 1));
        assert!(!app.nudge(Unit::Card, 1), "the last ticket has nowhere further to go");
        assert_eq!(eng(&app), ["ENG-53", "ENG-61", "ENG-70", "ENG-52"]);
        // The order members shows is the order of the cards.
        let order: Vec<_> = app.members(0).iter().map(|&i| app.tickets[i].t.key.clone()).collect();
        assert_eq!(order[..2], ["acme-lead", "acme-release-notes-reviewer"]);
        assert_eq!(order[2..], ["ENG-53", "ENG-61", "ENG-70", "ENG-52"]);
    }

    #[test]
    fn an_agent_stays_among_its_projects_agents() {
        let mut app = acme_app();
        app.select(0);
        assert!(!app.nudge(Unit::Card, -1));
        assert!(app.nudge(Unit::Card, 1));
        assert!(!app.nudge(Unit::Card, 1), "the second agent can't pass the tickets");
        let order: Vec<_> = app.members(0).iter().map(|&i| app.tickets[i].t.key.clone()).collect();
        assert_eq!(order[..2], ["acme-release-notes-reviewer", "acme-lead"]);
        assert_eq!(app.tickets[app.sel].t.key, "acme-lead");
    }

    #[test]
    fn projects_move_among_projects_with_their_cards() {
        let mut app = acme_app();
        assert!(!app.nudge(Unit::Project, -1));
        assert!(app.nudge(Unit::Project, 1));
        assert!(!app.nudge(Unit::Project, 1));
        let names: Vec<_> = app.projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["notes", "acme"]);
        // The cards follow their project, and the numbers follow the order.
        let visible: Vec<_> =
            app.ordered(true).iter().map(|&i| app.tickets[i].t.key.clone()).collect();
        assert_eq!(visible[0], "acme-lead");
        assert_eq!(app.tickets[app.sel].t.key, "ENG-52");
    }
}
