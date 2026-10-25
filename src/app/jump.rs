//! `prefix /`: a one-off jump to a ticket, agent or project by typing. The sidebar narrows to the
//! cards that match what's typed, ↑/↓ move among them and Enter selects one. With the sidebar
//! folded, a picker over the screen lists them instead.

use super::{App, Mode};
use crate::untrusted;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The jump box while it's open.
#[derive(Clone, Debug, Default)]
pub struct Jump {
    pub query: String,
    /// The highlighted match, as an index into `App::jump_matches`.
    pub at: usize,
    /// The picker's first row in view, as it was last drawn.
    pub top: usize,
}

/// What a card is matched on, as the sidebar shows it.
pub struct Card<'a> {
    pub project: &'a str,
    pub key: &'a str,
    /// An agent's name; its key is the session's name, which the project and name already cover.
    pub agent: Option<&'a str>,
    pub title: Option<&'a str>,
}

/// Whether `card` matches what was typed: a case-insensitive substring of its key (so a bare
/// number finds the key with that number, `52` finds ENG-52), of its title, of an agent's name or
/// of its project's name. Nothing typed matches everything.
pub fn matches(query: &str, card: &Card) -> bool {
    let q = query.trim().to_lowercase();
    let has = |s: &str| untrusted::line(s).to_lowercase().contains(&q);
    card.agent.is_none() && has(card.key)
        || card.agent.is_some_and(has)
        || card.title.is_some_and(has)
        || has(card.project)
}

impl App {
    /// The tickets and agents the jump box matches, in the sidebar's order, folded projects
    /// included.
    pub fn jump_matches(&self, query: &str) -> Vec<usize> {
        self.ordered(false)
            .into_iter()
            .filter(|&i| {
                let tv = &self.tickets[i];
                let project = self.project_of(&tv.t).map_or("", |p| self.projects[p].name.as_str());
                let title = tv.title();
                let card = Card {
                    project,
                    key: &tv.t.key,
                    agent: tv.t.agent.as_deref(),
                    title: title.as_deref(),
                };
                matches(query, &card)
            })
            .collect()
    }

    /// Whether the jump box is a picker over the screen rather than the sidebar's top row: a
    /// folded sidebar, folded by the user or by a narrow window, has no titles to narrow.
    pub fn jump_picker(&self) -> bool {
        self.prefs.sidebar_collapsed || self.areas.sidebar_folded
    }

    pub(super) fn open_jump(&mut self) {
        self.mode = Mode::Jump(Jump::default());
        self.sidebar_scroll = Default::default();
    }

    pub(super) fn jump_key(&mut self, key: KeyEvent) {
        let Mode::Jump(jump) = &self.mode else { return };
        let (mut query, at, top) = (jump.query.clone(), jump.at, jump.top);
        let found = self.jump_matches(&query);
        let typed = !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.close_jump(),
            KeyCode::Enter => {
                if let Some(&i) = found.get(at) {
                    self.close_jump();
                    self.select(i);
                }
            }
            KeyCode::Up => self.jump_to(query, at.saturating_sub(1), top),
            KeyCode::Down => {
                self.jump_to(query, (at + 1).min(found.len().saturating_sub(1)), top);
            }
            KeyCode::Backspace => {
                query.pop();
                self.jump_edited(query);
            }
            KeyCode::Char(c) if typed => {
                query.push(c);
                self.jump_edited(query);
            }
            _ => {}
        }
    }

    fn jump_to(&mut self, query: String, at: usize, top: usize) {
        self.mode = Mode::Jump(Jump { query, at, top });
    }

    /// The matches changed: the first is highlighted and the list starts from the top.
    fn jump_edited(&mut self, query: String) {
        self.jump_to(query, 0, 0);
        self.sidebar_scroll = Default::default();
    }

    fn close_jump(&mut self) {
        self.mode = Mode::Normal;
        self.sidebar_scroll = Default::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ticket<'a>(key: &'a str, title: &'a str) -> Card<'a> {
        Card { project: "acme", key, agent: None, title: Some(title) }
    }

    #[test]
    fn matches_key_number_title_agent_and_project() {
        let eng52 = ticket("ENG-52", "Fix the login redirect");
        assert!(matches("ENG-52", &eng52));
        assert!(matches("eng-5", &eng52));
        // A bare number finds the key with that number.
        assert!(matches("52", &eng52));
        assert!(!matches("53", &eng52));
        assert!(matches("LOGIN", &eng52));
        assert!(!matches("logout", &eng52));
        // Substring, not fuzzy: letters in order but apart don't match.
        assert!(!matches("flr", &eng52));
        // A project's name matches all its cards.
        assert!(matches("Acm", &eng52));
        let agent = Card { project: "acme", key: "acme-lead", agent: Some("lead"), title: None };
        assert!(matches("lead", &agent));
        assert!(matches("acme", &agent));
        assert!(!matches("reviewer", &agent));
        // An agent's key is the session's name, so only its name and project count.
        let agent = Card { key: "other-lead", ..agent };
        assert!(!matches("other", &agent));
        // Nothing typed, or only spaces, matches everything.
        assert!(matches("", &agent));
        assert!(matches("  ", &agent));
    }

    #[test]
    fn matches_what_the_sidebar_shows_of_a_title() {
        let card = ticket("ENG-9", "Fix\u{1b}[31m the\nlogin");
        assert!(matches("fix [31m the login", &card));
        assert!(!matches("\u{1b}", &card));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn a_word_of_the_key_or_title_always_matches(
            key in "[A-Z]{2,4}-[0-9]{1,4}",
            title in "[a-zA-Z0-9]{1,8}( [a-zA-Z0-9]{1,8}){0,3}",
            from in 0usize..30,
            len in 1usize..8,
        ) {
            let card = ticket(&key, &title);
            let part: String = title.chars().skip(from).take(len).collect();
            prop_assert!(matches(&part, &card));
            prop_assert!(matches(&key.to_lowercase(), &card));
            let number = key.rsplit('-').next().unwrap();
            prop_assert!(matches(number, &card));
        }

        #[test]
        fn never_panics_and_ignores_case(query in "\\PC{0,12}", title in "\\PC{0,30}") {
            let card = ticket("ENG-1", &title);
            prop_assert_eq!(matches(&query, &card), matches(&query.to_ascii_uppercase(), &card));
        }
    }
}
