//! The terminal's colours and the ones taskman draws with. At start taskman asks the terminal for
//! its foreground and background (OSC 10 and 11), else reads COLORFGBG, else takes it to be dark;
//! the `theme` setting can say light or dark instead. The selection, hints and yellow text are
//! chosen against that background, and sessions asking for the colours are told the same ones.

use ratatui::style::Color;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::Cell;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

pub type Rgb = [u8; 3];

/// The `theme` setting: follow the terminal, or say which it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Setting {
    #[default]
    Auto,
    Light,
    Dark,
}

impl Setting {
    pub const ALL: [Setting; 3] = [Setting::Auto, Setting::Light, Setting::Dark];

    /// As config.json spells it.
    pub fn name(self) -> &'static str {
        match self {
            Setting::Auto => "auto",
            Setting::Light => "light",
            Setting::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Option<Setting> {
        Setting::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(s.trim()))
    }
}

impl Serialize for Setting {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Setting {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Setting::parse(&String::deserialize(d)?).unwrap_or_default())
    }
}

/// What the terminal said about its colours at start.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Probe {
    /// The answers to OSC 10 and 11.
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    /// Whether COLORFGBG says the background is light, if it says.
    pub colorfgbg: Option<bool>,
}

impl Probe {
    /// What was found, for the setting's hint.
    pub fn describe(&self) -> String {
        let shade = |light| if light { "light" } else { "dark" };
        match (self.bg, self.colorfgbg) {
            (Some(bg), _) => {
                format!("the terminal says its background is {}, {}", hex(bg), shade(is_light(bg)))
            }
            (None, Some(light)) => format!("COLORFGBG says the background is {}", shade(light)),
            (None, None) => "the terminal doesn't say its background: dark".into(),
        }
    }
}

/// The colours a session is told when it asks (OSC 10 and 11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Colours {
    pub fg: Rgb,
    pub bg: Rgb,
}

const DARK: Colours = Colours { fg: [0xd0, 0xd0, 0xd0], bg: [0x1e, 0x1e, 0x1e] };
const LIGHT: Colours = Colours { fg: [0x00, 0x00, 0x00], bg: [0xff, 0xff, 0xff] };

impl Default for Colours {
    fn default() -> Self {
        DARK
    }
}

impl Colours {
    /// The reply to OSC `which` (`10` or `11`) asked with `?`, as xterm writes it.
    pub fn answer(&self, which: &[u8]) -> Option<Vec<u8>> {
        let [r, g, b] = match which {
            b"10" => self.fg,
            b"11" => self.bg,
            _ => return None,
        };
        let which = std::str::from_utf8(which).ok()?;
        Some(format!("\x1b]{which};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\").into())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub light: bool,
    pub colours: Colours,
    /// The selected card's, the footer's and a focused field's background.
    pub selection: Color,
    /// Hints and other quiet text.
    pub hint: Color,
    /// Toasts and the other yellow text: a dark amber on a light background, where yellow is
    /// unreadable (xterm's is 1.7:1 on white).
    pub yellow: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::new(Setting::Auto, &Probe::default())
    }
}

impl Theme {
    /// The setting first, then the terminal's answer, then COLORFGBG, then dark. The terminal's
    /// own colours are used when they agree with that; otherwise, or when it didn't say, a usual
    /// pair for the shade, and the palette taskman always used.
    pub fn new(setting: Setting, probe: &Probe) -> Self {
        let light = match setting {
            Setting::Light => true,
            Setting::Dark => false,
            Setting::Auto => probe.bg.map(is_light).or(probe.colorfgbg).unwrap_or(false),
        };
        let usual = if light { LIGHT } else { DARK };
        let yellow = if light { Color::Indexed(130) } else { Color::Yellow };
        let Some(bg) = probe.bg.filter(|&bg| is_light(bg) == light) else {
            let selection = Color::Indexed(if light { 254 } else { 236 });
            return Theme { light, colours: usual, selection, hint: Color::DarkGray, yellow };
        };
        Theme {
            light,
            colours: Colours { fg: probe.fg.unwrap_or(usual.fg), bg },
            // Faint, but there: 236 on #1e1e1e and 254 on white, as before.
            selection: grey(bg, 1.25).unwrap_or(Color::Indexed(if light { 254 } else { 236 })),
            // WCAG's 4.5:1 for text. The terminal's dark grey can be its background (Solarized
            // Dark's is), so it's only used when the background isn't known.
            hint: grey(bg, 4.5).unwrap_or(Color::Indexed(if light { 232 } else { 255 })),
            yellow,
        }
    }
}

/// The grey of the 256-colour ramp nearest `bg` (lighter on a dark background, darker on a light
/// one) that has at least `ratio` contrast with it. The ramp rather than RGB, which some
/// terminals (macOS Terminal) don't show, and rather than the 16 colours, which themes change.
fn grey(bg: Rgb, ratio: f64) -> Option<Color> {
    let light = is_light(bg);
    let ramp = (0..24u8).map(|i| (232 + i, [8 + 10 * i; 3]));
    let enough =
        |&(_, g): &(u8, Rgb)| (luminance(g) < luminance(bg)) == light && contrast(g, bg) >= ratio;
    let found = if light { ramp.rev().find(enough) } else { ramp.into_iter().find(enough) };
    found.map(|(index, _)| Color::Indexed(index))
}

/// WCAG's relative luminance.
fn luminance([r, g, b]: Rgb) -> f64 {
    let lin = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Light when black text on it contrasts more than white text does.
pub fn is_light(bg: Rgb) -> bool {
    contrast(bg, [0; 3]) > contrast(bg, [255; 3])
}

fn hex([r, g, b]: Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// COLORFGBG's verdict, as rxvt and Konsole set it (`15;0`, or `15;default;0`): the last field is
/// the background's palette number, light when it's white or light grey.
pub fn colorfgbg(v: &str) -> Option<bool> {
    let bg = v.rsplit(';').next()?.trim().parse::<u8>().ok()?;
    Some(bg == 7 || bg == 15)
}

/// The colour queries, then primary device attributes, which every terminal answers and answers
/// in order: its reply means no colour reply is coming.
const QUERY: &[u8] = b"\x1b]10;?\x07\x1b]11;?\x07\x1b[c";

/// How long to wait for a terminal that answers none of `QUERY`. Long enough for a slow link, as a
/// reply after it would reach the session as typed text; the rest answer within milliseconds.
const ASK_LIMIT: Duration = Duration::from_millis(500);

/// What the terminal has replied to `QUERY` so far.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Replies {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    /// The device attributes came back: nothing more will.
    pub done: bool,
}

/// Reads the terminal's replies in `buf`, ignoring anything else (keys typed meanwhile). A reply
/// cut off at the end of `buf` isn't read yet.
pub fn parse_replies(buf: &[u8]) -> Replies {
    let mut out = Replies::default();
    let mut i = 0;
    while let Some(at) = buf[i..].iter().position(|&b| b == 0x1b) {
        i += at + 1;
        match buf.get(i) {
            Some(b']') => {
                let body = &buf[i + 1..];
                // Ended by BEL or ST, as the query was.
                let Some(end) = body
                    .iter()
                    .position(|&b| b == 0x07 || b == 0x1b)
                    .filter(|&e| body[e] == 0x07 || body.get(e + 1) == Some(&b'\\'))
                else {
                    // An unended reply, or an escape inside one: look for the next.
                    continue;
                };
                let mut parts = body[..end].splitn(2, |&b| b == b';');
                let (which, spec) = (parts.next(), parts.next());
                let colour = spec.and_then(parse_colour);
                match which {
                    Some(b"10") => out.fg = colour.or(out.fg),
                    Some(b"11") => out.bg = colour.or(out.bg),
                    _ => {}
                }
                i += 1 + end;
            }
            Some(b'[') => {
                let rest = &buf[i + 1..];
                if rest.first() == Some(&b'?') {
                    let n = rest[1..].iter().take_while(|b| b.is_ascii_digit() || **b == b';');
                    if rest.get(1 + n.count()) == Some(&b'c') {
                        out.done = true;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// `rgb:RRRR/GGGG/BBBB` with 1 to 4 hex digits a component (`rgba:` too, alpha ignored), scaled
/// to 8 bits as XParseColor does.
fn parse_colour(spec: &[u8]) -> Option<Rgb> {
    let spec = std::str::from_utf8(spec).ok()?;
    let (parts, n) = if let Some(s) = spec.strip_prefix("rgb:") {
        (s, 3)
    } else {
        (spec.strip_prefix("rgba:")?, 4)
    };
    let parts: Vec<&str> = parts.split('/').collect();
    if parts.len() != n {
        return None;
    }
    let mut rgb = [0u8; 3];
    for (c, part) in rgb.iter_mut().zip(&parts) {
        if !(1..=4).contains(&part.len()) || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let max = (1u32 << (4 * part.len())) - 1;
        let v = u32::from_str_radix(part, 16).ok()?;
        *c = u8::try_from((v * 255 + max / 2) / max).ok()?;
    }
    Some(rgb)
}

/// Asks the terminal on `fd` for its colours. In raw mode and before crossterm reads the terminal,
/// which would take the replies for keys: whatever arrives meanwhile is read here, keys typed
/// included.
pub fn ask(fd: RawFd) -> Probe {
    use std::io::Write;
    let colorfgbg = std::env::var("COLORFGBG").ok().and_then(|v| colorfgbg(&v));
    let mut probe = Probe { colorfgbg, ..Probe::default() };
    let mut out = std::io::stdout();
    if out.write_all(QUERY).and_then(|()| out.flush()).is_err() {
        return probe;
    }
    let until = Instant::now() + ASK_LIMIT;
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        let mut fds = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }];
        // As `Tty::wait`: select on macOS, where poll doesn't work on terminals.
        if !matches!(filedescriptor::poll(&mut fds, Some(left)), Ok(n) if n > 0) {
            break;
        }
        // SAFETY: read writes at most `buf.len()` bytes into `buf`, which outlives the call.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        got.extend_from_slice(&buf[..n as usize]);
        if parse_replies(&got).done {
            break;
        }
    }
    let replies = parse_replies(&got);
    (probe.fg, probe.bg) = (replies.fg, replies.bg);
    probe
}

thread_local! {
    /// The theme drawing uses, set by `ui::draw` from the App's: drawing helpers far from the App
    /// need it. Per thread, so tests drawing other themes side by side keep their own.
    static CURRENT: Cell<Theme> = Cell::new(Theme::default());
}

pub fn current() -> Theme {
    CURRENT.with(Cell::get)
}

pub fn set_current(t: Theme) {
    CURRENT.with(|c| c.set(t));
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const WHITE: Rgb = [255; 3];

    #[test]
    fn reads_the_colour_replies() {
        let r = parse_replies(
            b"\x1b]10;rgb:0000/0000/0000\x07\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[?62;22c",
        );
        assert_eq!(r, Replies { fg: Some([0; 3]), bg: Some(WHITE), done: true });
        // ST rather than BEL, fewer digits, rgba (rxvt), keys typed in between.
        let r = parse_replies(b"x\x1b]11;rgba:fd/f6/e3/ff\x1b\\y\x1b]10;rgb:6/5/4\x1b\\");
        assert_eq!(
            r,
            Replies { fg: Some([0x66, 0x55, 0x44]), bg: Some([0xfd, 0xf6, 0xe3]), done: false }
        );
        assert_eq!(parse_replies(b"\x1b]11;rgb:1e1e/1e1e/1e1e").bg, None, "not ended yet");
        assert_eq!(parse_replies(b"\x1b[?1;2c"), Replies { done: true, ..Replies::default() });
        // Not a reply: another OSC, a bad colour, a key with escape, a CSI that isn't DA.
        let r = parse_replies(b"\x1b]12;rgb:0/0/0\x07\x1b]11;rgb:00000/0/0\x07\x1b[A\x1b[?1u\x1bx");
        assert_eq!(r, Replies::default());
        assert_eq!(parse_colour(b"rgb:12/34"), None);
        assert_eq!(parse_colour(b"rgb:/0/0"), None);
        assert_eq!(parse_colour(b"#ffffff"), None);
        assert_eq!(parse_colour(b"rgb:8/80/800"), Some([0x88, 0x80, 0x80]));
    }

    #[test]
    fn answers_sessions_as_xterm_does() {
        let c = Colours { fg: [0x12, 0x34, 0x56], bg: WHITE };
        assert_eq!(c.answer(b"10").unwrap(), b"\x1b]10;rgb:1212/3434/5656\x1b\\");
        assert_eq!(c.answer(b"11").unwrap(), b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\");
        assert_eq!(c.answer(b"12"), None);
        // What a session reads back is what the terminal said.
        let back = parse_replies(&c.answer(b"10").unwrap());
        assert_eq!(back.fg, Some(c.fg));
    }

    #[test]
    fn the_setting_then_the_terminal_then_colorfgbg_then_dark() {
        let says = |bg: Option<Rgb>, colorfgbg: Option<bool>| Probe { fg: None, bg, colorfgbg };
        let light = |s, p: Probe| Theme::new(s, &p).light;
        assert!(!light(Setting::Auto, says(None, None)));
        assert!(light(Setting::Auto, says(None, Some(true))));
        assert!(light(Setting::Auto, says(Some(WHITE), Some(false))), "the terminal wins");
        assert!(!light(Setting::Auto, says(Some([0x1e; 3]), Some(true))));
        assert!(light(Setting::Light, says(Some([0; 3]), Some(false))), "the setting wins");
        assert!(!light(Setting::Dark, says(Some(WHITE), Some(true))));

        assert_eq!(colorfgbg("15;0"), Some(false));
        assert_eq!(colorfgbg("0;15"), Some(true));
        assert_eq!(colorfgbg("0;default;7"), Some(true));
        assert_eq!(colorfgbg("default;default"), None);
        assert_eq!(colorfgbg(""), None);
    }

    /// Without the terminal's colours it draws as it always did; with them, the same on the usual
    /// backgrounds, and what fits on others.
    #[test]
    fn colours_chosen_against_the_background() {
        let dark = Theme::default();
        assert_eq!(
            (dark.selection, dark.hint, dark.yellow),
            (Color::Indexed(236), Color::DarkGray, Color::Yellow)
        );
        assert_eq!(dark.colours, DARK);
        let light = Theme::new(Setting::Light, &Probe::default());
        assert_eq!(
            (light.selection, light.hint, light.yellow),
            (Color::Indexed(254), Color::DarkGray, Color::Indexed(130))
        );
        assert_eq!(light.colours, LIGHT);

        let on = |fg, bg| Theme::new(Setting::Auto, &Probe { fg, bg: Some(bg), colorfgbg: None });
        let white = on(Some([0x20; 3]), WHITE);
        assert_eq!((white.selection, white.hint), (Color::Indexed(254), Color::Indexed(243)));
        assert_eq!(white.colours, Colours { fg: [0x20; 3], bg: WHITE });
        let vscode = on(None, [0x1e; 3]);
        assert_eq!((vscode.selection, vscode.hint), (Color::Indexed(236), Color::Indexed(245)));
        assert_eq!(vscode.colours, Colours { fg: DARK.fg, bg: [0x1e; 3] });
        // tmux's `bg=white`, palette 7: darker greys, not the lighter ones nearer it.
        let silver = on(None, [0xc0; 3]);
        assert_eq!((silver.selection, silver.hint), (Color::Indexed(248), Color::Indexed(239)));
        // Solarized Dark, whose own dark grey is its background.
        let solarized = on(None, [0x00, 0x2b, 0x36]);
        assert!(!solarized.light);
        assert_ne!(solarized.hint, Color::DarkGray);
        // A light background told to be dark isn't believed: the usual dark pair.
        assert_eq!(Theme::new(Setting::Dark, &Probe { bg: Some(WHITE), ..Probe::default() }), dark);
    }

    #[test]
    fn the_setting_reads_leniently() {
        for s in Setting::ALL {
            assert_eq!(Setting::parse(s.name()), Some(s));
        }
        assert_eq!(Setting::parse(" Light "), Some(Setting::Light));
        let read = |j: &str| serde_json::from_str::<Setting>(j).unwrap();
        assert_eq!(read("\"dark\""), Setting::Dark);
        assert_eq!(read("\"sepia\""), Setting::Auto, "a newer taskman's choice");
        assert_eq!(serde_json::to_string(&Setting::Light).unwrap(), "\"light\"");
    }

    /// Replies as a terminal, a slow link or a hostile program could send them: pieces of escape
    /// sequences, colour specs right and wrong, and other bytes.
    fn replies() -> impl Strategy<Value = Vec<u8>> {
        let piece = prop_oneof![
            2 => any::<u8>().prop_map(|b| vec![b]),
            1 => prop::sample::select(vec![
                "\x1b", "\x1b]", "\x1b]10;", "\x1b]11;", "\x1b]11;?", "\x07", "\x1b\\", "\x1b[",
                "\x1b[?", "\x1b[?62;22c", "c", ";", "/", "rgb:", "rgba:", "rgb:ffff/ffff/ffff",
            ])
            .prop_map(|s| s.as_bytes().to_vec()),
            1 => "[0-9a-fA-F]{0,5}".prop_map(String::into_bytes),
        ];
        prop::collection::vec(piece, 0..30).prop_map(|p| p.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Never panics, and reading more never forgets what was read.
        #[test]
        fn replies_read_from_any_bytes(a in replies(), b in replies()) {
            let first = parse_replies(&a);
            let both = parse_replies(&[a.clone(), b].concat());
            prop_assert!(!first.done || both.done);
            prop_assert!(first.fg.is_none() || both.fg.is_some());
            prop_assert!(first.bg.is_none() || both.bg.is_some());
        }

        /// Any colour told to a session reads back as itself, among whatever else arrives.
        #[test]
        fn answers_read_back(fg in any::<Rgb>(), bg in any::<Rgb>(), before in replies()) {
            let c = Colours { fg, bg };
            // What's before ends cleanly, as a terminal's output would between replies.
            let mut buf = before;
            buf.extend_from_slice(b"\x07\x1b\\");
            buf.extend(c.answer(b"11").unwrap());
            buf.extend(c.answer(b"10").unwrap());
            buf.extend_from_slice(b"\x1b[?62;22c");
            let r = parse_replies(&buf);
            prop_assert_eq!((r.fg, r.bg, r.done), (Some(fg), Some(bg), true));
        }
    }
}
