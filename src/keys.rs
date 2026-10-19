//! Translates crossterm key events back into the bytes a terminal would send to the child.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::atomic::{AtomicU8, Ordering};

/// The key that starts a taskman command; every other key goes to the session. A short list
/// rather than any key, since each has to reach taskman unchanged from every terminal, and one
/// Claude or the terminal needs would be lost (the reason each is noted).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum Prefix {
    #[default]
    CtrlA,
    CtrlB,
    CtrlBracket,
    CtrlG,
    CtrlSpace,
    CtrlBackslash,
}

impl Prefix {
    pub const ALL: [Prefix; 6] = [
        Prefix::CtrlA,
        Prefix::CtrlB,
        Prefix::CtrlBracket,
        Prefix::CtrlG,
        Prefix::CtrlSpace,
        Prefix::CtrlBackslash,
    ];

    /// The character the key is ctrl plus, as kitty-protocol terminals report it.
    fn char(self) -> char {
        match self {
            Prefix::CtrlA => 'a',
            Prefix::CtrlB => 'b',
            Prefix::CtrlBracket => ']',
            Prefix::CtrlG => 'g',
            Prefix::CtrlSpace => ' ',
            Prefix::CtrlBackslash => '\\',
        }
    }

    /// `ctrl+a`, as hints and config.json spell it.
    pub fn name(self) -> &'static str {
        match self {
            Prefix::CtrlA => "ctrl+a",
            Prefix::CtrlB => "ctrl+b",
            Prefix::CtrlBracket => "ctrl+]",
            Prefix::CtrlG => "ctrl+g",
            Prefix::CtrlSpace => "ctrl+space",
            Prefix::CtrlBackslash => "ctrl+\\",
        }
    }

    /// `^a`, for the footer.
    pub fn short(self) -> String {
        match self {
            Prefix::CtrlSpace => "^space".into(),
            p => format!("^{}", p.char()),
        }
    }

    /// What else uses the key, for Settings and the README.
    pub fn conflict(self) -> &'static str {
        match self {
            Prefix::CtrlA => "GNU screen's prefix, often tmux's; start of line in Claude's prompt",
            Prefix::CtrlB => "tmux's prefix; Claude runs a command in the background with it",
            Prefix::CtrlBracket => {
                "telnet's escape key; Claude reopens its latest artifact with it, where artifacts are on"
            }
            Prefix::CtrlG => "Claude opens the prompt in $EDITOR with it",
            Prefix::CtrlSpace => "switches input source on macOS; some terminals don't send it",
            Prefix::CtrlBackslash => "free in Claude; a plain shell's quit key",
        }
    }

    /// Accepts `ctrl+b`, `ctrl-b`, `C-b` and `^b`, in any case.
    pub fn parse(s: &str) -> Option<Prefix> {
        let s = s.trim().to_ascii_lowercase();
        let rest = ["ctrl+", "ctrl-", "c-", "^"].iter().find_map(|p| s.strip_prefix(p))?;
        Prefix::ALL.into_iter().find(|p| p.name()["ctrl+".len()..] == *rest)
    }

    /// Whether `key` is this prefix. Without the kitty protocol ctrl+\ and ctrl+] arrive as the
    /// bytes 0x1c and 0x1d, which crossterm reports as ctrl+4 and ctrl+5.
    pub fn matches(self, key: &KeyEvent) -> bool {
        let KeyCode::Char(c) = key.code else { return false };
        if !key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        let legacy = match self {
            Prefix::CtrlBackslash => '4',
            Prefix::CtrlBracket => '5',
            p => p.char(),
        };
        c == self.char() || c == legacy
    }

    /// The byte the key sends, for passing it on to the session.
    pub fn byte(self) -> u8 {
        ctrl_byte(self.char()).unwrap_or(0)
    }
}

impl std::fmt::Display for Prefix {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl Serialize for Prefix {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

/// An unknown key falls back to ctrl+a rather than failing the whole config, which would stop
/// taskman starting.
impl<'de> Deserialize<'de> for Prefix {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Prefix::parse(&String::deserialize(d)?).unwrap_or_default())
    }
}

/// The prefix in use. Global rather than passed around, as hints far from the TUI name it too:
/// `taskman status`, `taskman add` and error messages.
static PREFIX: AtomicU8 = AtomicU8::new(0);

pub fn prefix() -> Prefix {
    Prefix::ALL.get(PREFIX.load(Ordering::Relaxed) as usize).copied().unwrap_or_default()
}

pub fn set_prefix(p: Prefix) {
    PREFIX.store(p as u8, Ordering::Relaxed);
}

/// Terminals using the kitty keyboard protocol may report Shift+p as `p` with SHIFT held rather
/// than as `P`. Folds that into the character, so commands and typed text see `P` either way.
pub fn normalize(mut key: KeyEvent) -> KeyEvent {
    if let KeyCode::Char(c) = key.code
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && c.is_ascii_lowercase()
    {
        key.code = KeyCode::Char(c.to_ascii_uppercase());
    }
    if matches!(key.code, KeyCode::Char(_)) {
        key.modifiers.remove(KeyModifiers::SHIFT);
    }
    key
}

pub fn encode(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let m = key.modifiers;
    let (ctrl, alt, shift) = (
        m.contains(KeyModifiers::CONTROL),
        m.contains(KeyModifiers::ALT),
        m.contains(KeyModifiers::SHIFT),
    );
    // xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    let modp = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi = |final_: char| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[1;{modp}{final_}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_}").into_bytes()
        } else {
            format!("\x1b[{final_}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[{n};{modp}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };

    let mut out = match key.code {
        // ctrl+z would suspend the CLI, and nothing in its pty can resume it: see docs/design.md.
        KeyCode::Char('z' | 'Z') if ctrl => return None,
        KeyCode::Char(c) if ctrl => vec![ctrl_byte(c)?],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // Shift/Alt+Enter: meta-enter, which Claude Code treats as a newline.
        KeyCode::Enter if shift || alt => return Some(b"\x1b\r".to_vec()),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace if ctrl => vec![0x08],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => return Some(b"\x1b[Z".to_vec()),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => return Some(csi('A')),
        KeyCode::Down => return Some(csi('B')),
        KeyCode::Right => return Some(csi('C')),
        KeyCode::Left => return Some(csi('D')),
        KeyCode::Home => return Some(csi('H')),
        KeyCode::End => return Some(csi('F')),
        KeyCode::Insert => return Some(tilde(2)),
        KeyCode::Delete => return Some(tilde(3)),
        KeyCode::PageUp => return Some(tilde(5)),
        KeyCode::PageDown => return Some(tilde(6)),
        KeyCode::F(n @ 1..=4) => {
            let f = b"PQRS"[(n - 1) as usize] as char;
            return Some(
                if modp > 1 { format!("\x1b[1;{modp}{f}") } else { format!("\x1bO{f}") }
                    .into_bytes(),
            );
        }
        KeyCode::F(n @ 5..=12) => {
            return Some(tilde([15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize]));
        }
        _ => return None,
    };
    if alt {
        out.insert(0, 0x1b);
    }
    Some(out)
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        // Legacy terminals send 0x1f for ctrl+/ too; the kitty protocol reports it as the key.
        '_' | '-' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use proptest::prelude::*;

    fn k(code: KeyCode, m: KeyModifiers) -> Vec<u8> {
        encode(&KeyEvent::new(code, m), false).unwrap()
    }

    #[test]
    fn braces_arrive_the_same_with_or_without_the_kitty_protocol_reporting_shift() {
        // Shift+[ as a kitty terminal reports it: `{` with SHIFT, or `[` with SHIFT where the
        // layout is reported by key. Only the first is a brace; both must not leave SHIFT on.
        let shifted = normalize(KeyEvent::new(KeyCode::Char('{'), KeyModifiers::SHIFT));
        assert_eq!(shifted, KeyEvent::new(KeyCode::Char('{'), KeyModifiers::NONE));
        let shifted = normalize(KeyEvent::new(KeyCode::Char('}'), KeyModifiers::SHIFT));
        assert_eq!(shifted, KeyEvent::new(KeyCode::Char('}'), KeyModifiers::NONE));
    }

    #[test]
    fn encodes_common_keys() {
        assert_eq!(k(KeyCode::Char('x'), KeyModifiers::NONE), b"x");
        assert_eq!(k(KeyCode::Char('c'), KeyModifiers::CONTROL), [3]);
        assert_eq!(k(KeyCode::Char('/'), KeyModifiers::CONTROL), [0x1f]);
        assert_eq!(k(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
        assert_eq!(k(KeyCode::Up, KeyModifiers::NONE), b"\x1b[A");
        assert_eq!(k(KeyCode::Right, KeyModifiers::CONTROL), b"\x1b[1;5C");
        assert_eq!(k(KeyCode::Enter, KeyModifiers::SHIFT), b"\x1b\r");
        assert_eq!(
            encode(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true).unwrap(),
            b"\x1bOA"
        );
        let shifted = normalize(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::SHIFT));
        assert_eq!((shifted.code, shifted.modifiers), (KeyCode::Char('P'), KeyModifiers::NONE));
    }

    #[test]
    fn parses_prefixes() {
        assert_eq!(Prefix::parse("ctrl+a"), Some(Prefix::CtrlA));
        assert_eq!(Prefix::parse("C-b"), Some(Prefix::CtrlB));
        assert_eq!(Prefix::parse("^]"), Some(Prefix::CtrlBracket));
        assert_eq!(Prefix::parse(" Ctrl-Space "), Some(Prefix::CtrlSpace));
        assert_eq!(Prefix::parse("ctrl+\\"), Some(Prefix::CtrlBackslash));
        assert_eq!(Prefix::parse("ctrl+x"), None);
        assert_eq!(Prefix::parse("a"), None);
        for p in Prefix::ALL {
            assert_eq!(Prefix::parse(p.name()), Some(p));
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<Prefix>(&json).unwrap(), p);
        }
        assert_eq!(serde_json::from_str::<Prefix>(r#""ctrl+q""#).unwrap(), Prefix::CtrlA);
        assert_eq!(Prefix::CtrlA.short(), "^a");
        assert_eq!(Prefix::CtrlSpace.short(), "^space");
        assert_eq!(Prefix::CtrlBackslash.short(), "^\\");
    }

    #[test]
    fn matches_prefix_keys() {
        let ctrl = |c| normalize(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        assert!(Prefix::CtrlA.matches(&ctrl('a')));
        assert!(!Prefix::CtrlA.matches(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert!(!Prefix::CtrlA.matches(&ctrl('b')));
        assert!(Prefix::CtrlB.matches(&ctrl('b')));
        // Legacy terminals send 0x1c / 0x1d, which crossterm reports as ctrl+4 / ctrl+5; the kitty
        // protocol reports the key itself.
        assert!(Prefix::CtrlBackslash.matches(&ctrl('4')));
        assert!(Prefix::CtrlBackslash.matches(&ctrl('\\')));
        assert!(Prefix::CtrlBracket.matches(&ctrl('5')));
        assert!(Prefix::CtrlBracket.matches(&ctrl(']')));
        assert!(Prefix::CtrlSpace.matches(&ctrl(' ')));
        assert!(
            !Prefix::CtrlBracket.matches(&KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE))
        );
        // A kitty terminal reporting ctrl+shift+g as g with SHIFT: normalize makes it G, not
        // the prefix.
        let shifted = normalize(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert!(!Prefix::CtrlG.matches(&shifted));
        let bytes: Vec<u8> = Prefix::ALL.iter().map(|p| p.byte()).collect();
        assert_eq!(bytes, [0x01, 0x02, 0x1d, 0x07, 0x00, 0x1c]);
    }

    #[test]
    fn swallows_ctrl_z() {
        for m in [KeyModifiers::CONTROL, KeyModifiers::CONTROL | KeyModifiers::ALT] {
            assert_eq!(encode(&KeyEvent::new(KeyCode::Char('z'), m), false), None);
            assert_eq!(encode(&KeyEvent::new(KeyCode::Char('Z'), m), false), None);
        }
        assert_eq!(k(KeyCode::Char('z'), KeyModifiers::NONE), b"z");
    }

    /// Each set of modifiers and the parameter xterm sends for it (ctlseqs, "PC-Style Function
    /// Keys"), written out rather than worked out, so a broken formula fails here.
    fn modifier_sets() -> [(KeyModifiers, u8); 8] {
        let (s, a, c) = (KeyModifiers::SHIFT, KeyModifiers::ALT, KeyModifiers::CONTROL);
        [
            (KeyModifiers::NONE, 1),
            (s, 2),
            (a, 3),
            (s | a, 4),
            (c, 5),
            (s | c, 6),
            (a | c, 7),
            (s | a | c, 8),
        ]
    }

    #[test]
    fn encodes_cursor_editing_and_function_keys_as_xterm_does() {
        use KeyCode::*;
        // The key; what it sends unmodified, in normal and in application cursor mode; and the
        // number and final byte of its CSI with a modifier: CSI 1;<mod> A-D/H/F, CSI <n>;<mod> ~,
        // and SS3 P-S for F1-F4 becoming CSI 1;<mod> P-S.
        let table: [(KeyCode, &str, &str, u8, char); 22] = [
            (Up, "\x1b[A", "\x1bOA", 1, 'A'),
            (Down, "\x1b[B", "\x1bOB", 1, 'B'),
            (Right, "\x1b[C", "\x1bOC", 1, 'C'),
            (Left, "\x1b[D", "\x1bOD", 1, 'D'),
            (Home, "\x1b[H", "\x1bOH", 1, 'H'),
            (End, "\x1b[F", "\x1bOF", 1, 'F'),
            (Insert, "\x1b[2~", "\x1b[2~", 2, '~'),
            (Delete, "\x1b[3~", "\x1b[3~", 3, '~'),
            (PageUp, "\x1b[5~", "\x1b[5~", 5, '~'),
            (PageDown, "\x1b[6~", "\x1b[6~", 6, '~'),
            (F(1), "\x1bOP", "\x1bOP", 1, 'P'),
            (F(2), "\x1bOQ", "\x1bOQ", 1, 'Q'),
            (F(3), "\x1bOR", "\x1bOR", 1, 'R'),
            (F(4), "\x1bOS", "\x1bOS", 1, 'S'),
            (F(5), "\x1b[15~", "\x1b[15~", 15, '~'),
            (F(6), "\x1b[17~", "\x1b[17~", 17, '~'),
            (F(7), "\x1b[18~", "\x1b[18~", 18, '~'),
            (F(8), "\x1b[19~", "\x1b[19~", 19, '~'),
            (F(9), "\x1b[20~", "\x1b[20~", 20, '~'),
            (F(10), "\x1b[21~", "\x1b[21~", 21, '~'),
            (F(11), "\x1b[23~", "\x1b[23~", 23, '~'),
            (F(12), "\x1b[24~", "\x1b[24~", 24, '~'),
        ];
        for (code, normal, application, n, last) in table {
            for (mods, m) in modifier_sets() {
                for app_cursor in [false, true] {
                    let want = match (m, app_cursor) {
                        (1, false) => normal.to_string(),
                        (1, true) => application.to_string(),
                        _ => format!("\x1b[{n};{m}{last}"),
                    };
                    let got = encode(&KeyEvent::new(code, mods), app_cursor);
                    assert_eq!(got, Some(want.into_bytes()), "{code:?} {mods:?} app {app_cursor}");
                }
            }
        }
    }

    #[test]
    fn encodes_keys_that_send_a_byte_or_two() {
        use KeyCode::*;
        let (none, s, a, c) =
            (KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::ALT, KeyModifiers::CONTROL);
        let table: &[(KeyCode, KeyModifiers, &[u8])] = &[
            (Enter, none, b"\r"),
            (Enter, s, b"\x1b\r"),
            (Enter, a, b"\x1b\r"),
            (Enter, s | a, b"\x1b\r"),
            (Enter, c, b"\r"),
            (Backspace, none, b"\x7f"),
            (Backspace, s, b"\x7f"),
            (Backspace, a, b"\x1b\x7f"),
            (Backspace, c, b"\x08"),
            (Backspace, c | a, b"\x1b\x08"),
            (Tab, none, b"\t"),
            (Tab, a, b"\x1b\t"),
            // crossterm reports Shift+Tab as BackTab, with SHIFT or without.
            (BackTab, none, b"\x1b[Z"),
            (BackTab, s, b"\x1b[Z"),
            (Esc, none, b"\x1b"),
            (Esc, a, b"\x1b\x1b"),
            (Char('x'), none, b"x"),
            (Char('X'), none, b"X"),
            (Char('x'), a, b"\x1bx"),
            (Char('é'), none, "é".as_bytes()),
            (Char('日'), a, "\x1b日".as_bytes()),
            (Char('c'), c, b"\x03"),
            (Char('c'), c | a, b"\x1b\x03"),
        ];
        for &(code, mods, want) in table {
            for app_cursor in [false, true] {
                let got = encode(&KeyEvent::new(code, mods), app_cursor);
                assert_eq!(got.as_deref(), Some(want), "{code:?} {mods:?} app {app_cursor}");
            }
        }
        for code in [F(0), F(13), F(255), Null, CapsLock, Menu, KeypadBegin] {
            assert_eq!(encode(&KeyEvent::new(code, none), false), None, "{code:?}");
        }
    }

    #[test]
    fn encodes_ctrl_with_each_key_that_has_a_control_byte() {
        let table: &[(&str, Option<u8>)] = &[
            ("@ 2", Some(0)),
            ("[3", Some(0x1b)),
            ("\\4", Some(0x1c)),
            ("]5", Some(0x1d)),
            ("^6", Some(0x1e)),
            ("_-/7", Some(0x1f)),
            ("?8", Some(0x7f)),
            ("aA", Some(0x01)),
            ("mM", Some(0x0d)),
            ("yY", Some(0x19)),
            ("019!{é", None),
        ];
        for &(chars, want) in table {
            for ch in chars.chars() {
                let got = encode(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL), false);
                assert_eq!(got, want.map(|b| vec![b]), "ctrl+{ch:?}");
            }
        }
    }

    fn key_code() -> impl Strategy<Value = KeyCode> {
        use KeyCode::*;
        prop_oneof![
            2 => any::<char>().prop_map(Char),
            2 => (0x20u8..0x7f).prop_map(|b| Char(b as char)),
            1 => any::<u8>().prop_map(F),
            2 => prop::sample::select(vec![
                Backspace, Enter, Left, Right, Up, Down, Home, End, PageUp, PageDown, Tab,
                BackTab, Delete, Insert, Null, Esc, CapsLock, ScrollLock, NumLock, PrintScreen,
                Pause, Menu, KeypadBegin,
            ]),
        ]
    }

    /// Any modifiers, SUPER, HYPER and META included.
    fn modifiers() -> impl Strategy<Value = KeyModifiers> {
        any::<u8>().prop_map(KeyModifiers::from_bits_truncate)
    }

    fn key_event() -> impl Strategy<Value = KeyEvent> {
        use crossterm::event::{KeyEventKind, KeyEventState};
        let kind = prop::sample::select(vec![
            KeyEventKind::Press,
            KeyEventKind::Repeat,
            KeyEventKind::Release,
        ]);
        let state = any::<u8>().prop_map(KeyEventState::from_bits_truncate);
        (key_code(), modifiers(), kind, state).prop_map(|(code, m, kind, state)| {
            KeyEvent::new_with_kind_and_state(code, m, kind, state)
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn the_modifier_parameter_is_xterms(
            code in prop::sample::select(vec![
                KeyCode::Up, KeyCode::Down, KeyCode::Right, KeyCode::Left, KeyCode::Home,
                KeyCode::End, KeyCode::Insert, KeyCode::Delete, KeyCode::PageUp,
                KeyCode::PageDown, KeyCode::F(1), KeyCode::F(4), KeyCode::F(5), KeyCode::F(12),
            ]),
            m in modifiers(),
            app_cursor in any::<bool>(),
        ) {
            let want = 1
                + u8::from(m.contains(KeyModifiers::SHIFT))
                + 2 * u8::from(m.contains(KeyModifiers::ALT))
                + 4 * u8::from(m.contains(KeyModifiers::CONTROL));
            let out = encode(&KeyEvent::new(code, m), app_cursor).unwrap();
            let out = String::from_utf8(out).unwrap();
            match out.strip_prefix("\x1b[").and_then(|p| p.split_once(';')) {
                Some((_, param)) => {
                    let param = param[..param.len() - 1].parse::<u8>().ok();
                    prop_assert_eq!(param, Some(want), "{:?}", out);
                }
                None => prop_assert_eq!(want, 1, "{:?}", out),
            }
        }

        #[test]
        fn encode_never_sends_nothing(key in key_event(), app_cursor in any::<bool>()) {
            if let Some(bytes) = encode(&key, app_cursor) {
                prop_assert!(!bytes.is_empty(), "{:?}", key);
            }
        }

        #[test]
        fn ctrl_and_a_letter_is_the_letter_masked(
            c in prop::sample::select(('a'..='z').chain('A'..='Z').collect::<Vec<_>>()),
            alt in any::<bool>(),
        ) {
            let alt_held = if alt { KeyModifiers::ALT } else { KeyModifiers::NONE };
            let m = KeyModifiers::CONTROL | alt_held;
            let got = encode(&KeyEvent::new(KeyCode::Char(c), m), false);
            if c.eq_ignore_ascii_case(&'z') {
                prop_assert_eq!(got, None);
            } else {
                let byte = c as u8 & 0x1f;
                prop_assert_eq!(got, Some(if alt { vec![0x1b, byte] } else { vec![byte] }));
            }
        }

        #[test]
        fn normalize_is_idempotent_and_leaves_no_shift_on_a_character(key in key_event()) {
            let once = normalize(key);
            prop_assert_eq!(normalize(once), once);
            if let KeyCode::Char(_) = once.code {
                prop_assert!(!once.modifiers.contains(KeyModifiers::SHIFT));
            } else {
                prop_assert_eq!(once, key);
            }
        }

        #[test]
        fn a_kitty_shifted_letter_sends_what_the_capital_sends(
            c in prop::char::range('a', 'z'),
            m in modifiers(),
        ) {
            // Shift+c as a kitty terminal reports it: c with SHIFT, beside whatever else is held.
            let kitty = normalize(KeyEvent::new(KeyCode::Char(c), m | KeyModifiers::SHIFT));
            let legacy = normalize(KeyEvent::new(KeyCode::Char(c.to_ascii_uppercase()), m));
            prop_assert_eq!(kitty, legacy);
        }

        #[test]
        fn a_prefix_parses_back_however_it_is_spelled(
            p in prop::sample::select(Prefix::ALL.to_vec()),
            lead in prop::sample::select(vec!["ctrl+", "ctrl-", "c-", "^"]),
            upper in prop::collection::vec(any::<bool>(), 16),
            pad in ("[ \t]{0,2}", "[ \t\n]{0,2}"),
        ) {
            let spelled: String = format!("{lead}{}", &p.name()["ctrl+".len()..])
                .chars()
                .zip(&upper)
                .map(|(c, &up)| if up { c.to_ascii_uppercase() } else { c })
                .collect();
            prop_assert_eq!(Prefix::parse(&format!("{}{spelled}{}", pad.0, pad.1)), Some(p));
            let json = serde_json::to_string(&p).unwrap();
            prop_assert_eq!(serde_json::from_str::<Prefix>(&json).unwrap(), p);
        }

        #[test]
        fn a_parsed_prefix_is_named_as_it_parses(s in "(\\PC|[+\\-^ ]){0,12}") {
            if let Some(p) = Prefix::parse(&s) {
                prop_assert_eq!(Prefix::parse(p.name()), Some(p));
                let rest = s.trim().to_ascii_lowercase();
                prop_assert!(rest.ends_with(&p.name()["ctrl+".len()..]), "{:?} as {:?}", s, p);
            }
        }
    }
}
