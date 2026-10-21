//! Editing a form's text field at its cursor, with the keys any text input has. The cursor is a
//! byte offset on a grapheme boundary, so a wide character or an emoji moves and deletes as one.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    Insert(char),
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    Backspace,
    Delete,
    /// The word before the cursor.
    DeleteWord,
    Clear,
}

impl Edit {
    /// The edit `key` makes in a text field, if any. ctrl+a stays the prefix: Home goes to the
    /// start.
    pub fn from_key(key: &KeyEvent) -> Option<Self> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        Some(match key.code {
            KeyCode::Left if ctrl || alt => Self::WordLeft,
            KeyCode::Right if ctrl || alt => Self::WordRight,
            KeyCode::Left => Self::Left,
            KeyCode::Right => Self::Right,
            KeyCode::Home => Self::Home,
            KeyCode::End => Self::End,
            KeyCode::Backspace if ctrl || alt => Self::DeleteWord,
            KeyCode::Backspace => Self::Backspace,
            KeyCode::Delete => Self::Delete,
            KeyCode::Char('u') if ctrl => Self::Clear,
            KeyCode::Char('w') if ctrl => Self::DeleteWord,
            // What macOS terminals send for Option+←/→ unless set to send them as Alt.
            KeyCode::Char('b') if alt => Self::WordLeft,
            KeyCode::Char('f') if alt => Self::WordRight,
            KeyCode::Char(c) if !ctrl => Self::Insert(c),
            _ => return None,
        })
    }
}

/// Applies `edit` to `value` with the cursor at `at`, and returns where the cursor is after it.
/// Without `words` (a secret's), word moves and deletes go to the value's ends: stopping at its
/// punctuation would show where that is.
pub fn apply(value: &mut String, at: usize, edit: Edit, words: bool) -> usize {
    let at = snap(value, at);
    let word_left = |v: &str| if words { word_left(v, at) } else { 0 };
    match edit {
        Edit::Insert(c) => insert(value, at, c.encode_utf8(&mut [0; 4])),
        Edit::Left => prev(value, at),
        Edit::Right => next(value, at),
        Edit::WordLeft => word_left(value),
        Edit::WordRight if words => word_right(value, at),
        Edit::WordRight | Edit::End => value.len(),
        Edit::Home => 0,
        Edit::Backspace => remove(value, prev(value, at)..at),
        Edit::Delete => remove(value, at..next(value, at)),
        Edit::DeleteWord => remove(value, word_left(value)..at),
        Edit::Clear => remove(value, 0..value.len()),
    }
}

/// Inserts `text` at the cursor `at`, and returns where the cursor is after it: after the text.
pub fn insert(value: &mut String, at: usize, text: &str) -> usize {
    let at = snap(value, at);
    value.insert_str(at, text);
    snap(value, at + text.len())
}

/// Removes `range` and returns where the cursor goes: where it was.
fn remove(value: &mut String, range: std::ops::Range<usize>) -> usize {
    let at = range.start;
    value.replace_range(range, "");
    snap(value, at)
}

/// The grapheme boundaries of `s`, its end included.
fn stops(s: &str) -> impl DoubleEndedIterator<Item = usize> + '_ {
    s.grapheme_indices(true).map(|(i, _)| i).chain(std::iter::once(s.len()))
}

/// The first grapheme boundary at or after `at`: an edit can join what's either side of the
/// cursor into one grapheme (a combining accent typed before a letter), which it can't sit inside.
pub fn snap(s: &str, at: usize) -> usize {
    stops(s).find(|&i| i >= at).unwrap_or(s.len())
}

fn prev(s: &str, at: usize) -> usize {
    stops(s).rev().find(|&i| i < at).unwrap_or(0)
}

fn next(s: &str, at: usize) -> usize {
    stops(s).find(|&i| i > at).unwrap_or(s.len())
}

/// Words are letters and digits, so a move stops at the `/`, `-` and `.` of a branch or a path.
fn is_word(g: &str) -> bool {
    g.chars().next().is_some_and(char::is_alphanumeric)
}

/// The start of the word before `at`, past what isn't a word.
fn word_left(s: &str, at: usize) -> usize {
    let mut gs: Vec<_> = s.grapheme_indices(true).take_while(|&(i, _)| i < at).collect();
    while gs.pop_if(|(_, g)| !is_word(g)).is_some() {}
    while gs.pop_if(|(_, g)| is_word(g)).is_some() {}
    gs.last().map_or(0, |(i, g)| i + g.len())
}

/// The end of the word after `at`, past what isn't a word.
fn word_right(s: &str, at: usize) -> usize {
    let mut gs = s.grapheme_indices(true).skip_while(|&(i, _)| i < at).peekable();
    while gs.next_if(|(_, g)| !is_word(g)).is_some() {}
    while gs.next_if(|(_, g)| is_word(g)).is_some() {}
    gs.peek().map_or(s.len(), |&(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `value` with the cursor at `|`, after `edits`, written the same way.
    fn after(value: &str, edits: &[Edit], words: bool) -> String {
        let mut at = value.find('|').unwrap();
        let mut value = value.replacen('|', "", 1);
        for &e in edits {
            at = apply(&mut value, at, e, words);
        }
        value.insert(at, '|');
        value
    }

    fn edited(value: &str, edits: &[Edit]) -> String {
        after(value, edits, true)
    }

    fn typed(s: &str) -> Vec<Edit> {
        s.chars().map(Edit::Insert).collect()
    }

    #[test]
    fn typing_and_deleting_happen_at_the_cursor() {
        use Edit::*;
        assert_eq!(edited("eng-|52", &typed("x")), "eng-x|52");
        assert_eq!(edited("eng-|52", &[Backspace]), "eng|52");
        assert_eq!(edited("eng-|52", &[Delete]), "eng-|2");
        assert_eq!(edited("|eng", &[Backspace]), "|eng");
        assert_eq!(edited("eng|", &[Delete]), "eng|");
        assert_eq!(edited("eng-|52", &[Clear]), "|");
        assert_eq!(edited("ab|", &[Left, Left, Left, Right]), "a|b");
        assert_eq!(edited("ab|", &[Home, Insert('x'), End, Insert('y')]), "xaby|");
    }

    #[test]
    fn word_moves_stop_at_punctuation() {
        use Edit::*;
        let branch = "feature/{KEY}-{slug}|";
        assert_eq!(edited(branch, &[WordLeft]), "feature/{KEY}-{|slug}");
        assert_eq!(edited(branch, &[WordLeft, WordLeft]), "feature/{|KEY}-{slug}");
        assert_eq!(edited(branch, &[Home, WordRight]), "feature|/{KEY}-{slug}");
        assert_eq!(edited(branch, &[Home, WordRight, WordRight]), "feature/{KEY|}-{slug}");
        assert_eq!(edited("|  ", &[WordRight]), "  |");
        assert_eq!(edited("  |", &[WordLeft]), "|  ");
        assert_eq!(edited("fix the login |", &[DeleteWord]), "fix the |");
        assert_eq!(edited("~/src/acme-web|", &[DeleteWord, DeleteWord]), "~/src/|");
        assert_eq!(edited("fix th|e", &[DeleteWord]), "fix |e");
    }

    #[test]
    fn a_secrets_word_moves_go_to_its_ends() {
        use Edit::*;
        assert_eq!(after("lin_api_ab|cd", &[WordLeft], false), "|lin_api_abcd");
        assert_eq!(after("lin_|api_abcd", &[WordRight], false), "lin_api_abcd|");
        assert_eq!(after("lin_api_ab|cd", &[DeleteWord], false), "|cd");
    }

    #[test]
    fn wide_characters_and_emoji_move_and_delete_as_one() {
        use Edit::*;
        // A family is one grapheme of several characters joined; é here is e and an accent.
        let family = "👨\u{200d}👩\u{200d}👧";
        let value = format!("a{family}e\u{301}日|");
        assert_eq!(edited(&value, &[Left, Left]), format!("a{family}|e\u{301}日"));
        assert_eq!(edited(&value, &[Left, Left, Left]), format!("a|{family}e\u{301}日"));
        assert_eq!(edited(&value, &[Left, Backspace]), format!("a{family}|日"));
        assert_eq!(edited(&value, &[Home, Right, Delete]), "a|e\u{301}日");
        assert_eq!(edited("日本|語", &[Insert('x')]), "日本x|語");
        // An accent typed before a letter joins it: the cursor goes after the two.
        assert_eq!(edited("|\u{301}", &typed("e")), "e\u{301}|");
        assert_eq!(edited("ü|ber", &[WordLeft]), "|über");
    }

    #[test]
    fn inserts_text_at_the_cursor() {
        let mut value = "ENG-".to_string();
        let at = insert(&mut value, 0, "acme ");
        assert_eq!((value.as_str(), at), ("acme ENG-", 5));
        // A cursor off a boundary, as no edit leaves it, is moved to one first.
        let mut value = "日本".to_string();
        assert_eq!(insert(&mut value, 1, "x"), 4);
        assert_eq!(value, "日x本");
    }

    #[test]
    fn keys_make_the_edits_a_text_input_does() {
        use crossterm::event::KeyModifiers as M;
        let k = |code, m| Edit::from_key(&KeyEvent::new(code, m));
        assert_eq!(k(KeyCode::Left, M::NONE), Some(Edit::Left));
        assert_eq!(k(KeyCode::Left, M::CONTROL), Some(Edit::WordLeft));
        assert_eq!(k(KeyCode::Right, M::ALT), Some(Edit::WordRight));
        assert_eq!(k(KeyCode::Char('b'), M::ALT), Some(Edit::WordLeft));
        assert_eq!(k(KeyCode::Char('f'), M::ALT), Some(Edit::WordRight));
        assert_eq!(k(KeyCode::Char('w'), M::CONTROL), Some(Edit::DeleteWord));
        assert_eq!(k(KeyCode::Backspace, M::ALT), Some(Edit::DeleteWord));
        assert_eq!(k(KeyCode::Char('u'), M::CONTROL), Some(Edit::Clear));
        assert_eq!(k(KeyCode::Char('a'), M::CONTROL), None);
        assert_eq!(k(KeyCode::Char('é'), M::NONE), Some(Edit::Insert('é')));
    }

    fn any_edit() -> impl proptest::strategy::Strategy<Value = Edit> {
        use proptest::prelude::*;
        prop_oneof![
            proptest::sample::select(vec!['a', ' ', '-', '日', '\u{301}', '\u{200d}', '👩', '🇬'])
                .prop_map(Edit::Insert),
            proptest::sample::select(vec![
                Edit::Left,
                Edit::Right,
                Edit::WordLeft,
                Edit::WordRight,
                Edit::Home,
                Edit::End,
                Edit::Backspace,
                Edit::Delete,
                Edit::DeleteWord,
                Edit::Clear,
            ]),
        ]
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(300))]

        /// Whatever is typed and pressed, the cursor stays on a grapheme boundary in the value,
        /// and only Insert, Delete and the deletes before the cursor change the value.
        #[test]
        fn the_cursor_stays_on_a_grapheme_boundary(
            start in "\\PC{0,12}",
            at in 0usize..40,
            edits in proptest::collection::vec(any_edit(), 0..30),
            words: bool,
        ) {
            let mut value = start;
            let mut at = snap(&value, at);
            for e in edits {
                let before = value.clone();
                at = apply(&mut value, at, e, words);
                proptest::prop_assert!(stops(&value).any(|i| i == at), "{at} in {value:?}");
                if matches!(e, Edit::Left | Edit::Right | Edit::WordLeft | Edit::WordRight
                    | Edit::Home | Edit::End)
                {
                    proptest::prop_assert_eq!(&value, &before);
                }
            }
        }

        /// Text inserted anywhere is in the value whole, with what was either side of it.
        #[test]
        fn inserting_keeps_what_was_there(
            value in "\\PC{0,12}",
            text in "\\PC{0,6}",
            at in 0usize..40,
        ) {
            let mut v = value.clone();
            let at = snap(&value, at);
            let after = insert(&mut v, at, &text);
            proptest::prop_assert_eq!(&v, &format!("{}{text}{}", &value[..at], &value[at..]));
            proptest::prop_assert!(after >= at + text.len() && stops(&v).any(|i| i == after));
        }
    }
}
