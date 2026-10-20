//! Desktop notifications for a session that starts waiting on you while you're away from taskman:
//! when the terminal says it lost the focus (focus reports, CSI ?1004), or, in a terminal that
//! never says, when the session isn't the one on screen, as for the bell. One per wait: they're
//! sent as a session turns "needs you", and a reminder of the same prompt isn't a turn.
//!
//! They go out through the terminal's own notifications (OSC 9) in a terminal known to show them
//! and over SSH, where a command would notify the far machine's desktop; otherwise through the
//! desktop's command (terminal-notifier or osascript on macOS, a Windows toast through
//! powershell.exe on WSL, notify-send on Linux), and through the terminal when none works. The
//! text is taskman's own: the ticket's key or agent's name and why it waits, never the CLI's
//! words, and it reaches the commands as arguments, with no shell.

use crate::untrusted;
use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::process::Command;
use std::time::Duration;

/// The `notify` setting.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Setting {
    /// The terminal's own where it shows them, else the desktop's command.
    #[default]
    Auto,
    /// Only the terminal's own, for one taskman can't tell shows them (inside tmux, say).
    Terminal,
    Off,
}

impl Setting {
    pub const ALL: [Setting; 3] = [Setting::Auto, Setting::Terminal, Setting::Off];

    /// As config.json spells it.
    pub fn name(self) -> &'static str {
        match self {
            Setting::Auto => "auto",
            Setting::Terminal => "terminal",
            Setting::Off => "off",
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

/// Whether a session that just started waiting on you is worth a notification: `focused` is what
/// the terminal last said about its focus (None if it never has), `selected` whether the session
/// is the one on screen.
pub fn wanted(setting: Setting, focused: Option<bool>, selected: bool) -> bool {
    setting != Setting::Off
        && match focused {
            Some(focused) => !focused,
            None => !selected,
        }
}

/// What a notification says: `label` (a key, or an agent's name) and why it waits.
pub fn text(label: &str, permission: bool) -> String {
    let label: String = untrusted::line(label).chars().take(60).collect();
    let why = if permission { "a permission prompt" } else { "a question" };
    format!("{label} needs you: {why}")
}

/// The terminal's notification (OSC 9) saying `text`, passed through tmux to the terminal outside
/// it, which needs tmux's `allow-passthrough` on. `text` has no control characters (`text`).
pub fn escape(text: &str, tmux: bool) -> String {
    let osc = format!("\x1b]9;taskman · {}\x07", untrusted::line(text));
    if tmux { format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b")) } else { osc }
}

/// One way to send a notification, in the order `ways` tries them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Way {
    Command(&'static str),
    /// OSC 9, which the UI thread writes: whether the terminal shows it can't be known.
    Terminal,
}

#[derive(Default)]
struct Env {
    ssh: bool,
    tmux: bool,
    macos: bool,
    wsl: bool,
    /// A display or a session bus, which notify-send needs.
    desktop: bool,
    /// The terminal is one known to show OSC 9: iTerm2, WezTerm, Ghostty or kitty.
    shows_osc: bool,
}

impl Env {
    fn current() -> Self {
        let var = |v: &str| std::env::var(v).unwrap_or_default();
        let set = |v: &str| !var(v).is_empty();
        Env {
            ssh: set("SSH_CONNECTION") || set("SSH_TTY"),
            tmux: set("TMUX"),
            macos: cfg!(target_os = "macos"),
            wsl: set("WSL_DISTRO_NAME")
                || std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists(),
            desktop: set("DISPLAY") || set("WAYLAND_DISPLAY") || set("DBUS_SESSION_BUS_ADDRESS"),
            shows_osc: ["iTerm.app", "WezTerm", "ghostty"].contains(&var("TERM_PROGRAM").as_str())
                || ["xterm-kitty", "xterm-ghostty"].contains(&var("TERM").as_str())
                || set("KITTY_WINDOW_ID"),
        }
    }
}

/// The ways to try, in order. The terminal's own first where it shows them, as clicking one goes
/// back to the terminal; over SSH only that, as a command would run on the far machine. Inside
/// tmux the terminal outside isn't known, and tmux drops the escape unless `allow-passthrough` is
/// on, so the desktop's command comes first there. WSL has no notification service of its own
/// (notify-send fails), so it gets a Windows toast.
fn ways(setting: Setting, env: &Env) -> Vec<Way> {
    match setting {
        Setting::Off => return Vec::new(),
        Setting::Terminal => return vec![Way::Terminal],
        Setting::Auto => {}
    }
    if env.ssh || (env.shows_osc && !env.tmux) {
        return vec![Way::Terminal];
    }
    let mut out = Vec::new();
    if env.macos {
        out.extend([Way::Command("terminal-notifier"), Way::Command("osascript")]);
    } else if env.wsl {
        out.push(Way::Command("powershell.exe"));
    } else if env.desktop {
        out.push(Way::Command("notify-send"));
    }
    out.push(Way::Terminal);
    out
}

/// PowerShell's own app id, which Windows shows toasts for without registering one of taskman's.
const POWERSHELL_APP: &str =
    r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";

/// The command that shows `text` with `program`.
fn command(program: &str, text: &str) -> Command {
    let mut cmd = Command::new(program);
    match program {
        "terminal-notifier" => {
            cmd.args(["-title", "taskman", "-message", text]);
        }
        "osascript" => {
            cmd.args([
                "-e",
                "on run argv",
                "-e",
                "display notification (item 2 of argv) with title (item 1 of argv)",
                "-e",
                "end run",
                "taskman",
                text,
            ]);
        }
        "powershell.exe" => {
            cmd.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &toast_script(text)]);
        }
        // Servers read the body as markup.
        _ => {
            let body = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            cmd.args(["--app-name=taskman", "--", "taskman", &body]);
        }
    }
    cmd
}

/// The PowerShell that shows a Windows toast saying `text`, encoded as `-EncodedCommand` takes it
/// (UTF-16LE, base64). The text is in it as base64, so no quoting of it can go wrong.
fn toast_script(text: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let script = format!(
        "$t = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))\n\
         $m = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]\n\
         $x = $m::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)\n\
         $n = $x.GetElementsByTagName('text')\n\
         $null = $n.Item(0).AppendChild($x.CreateTextNode('taskman'))\n\
         $null = $n.Item(1).AppendChild($x.CreateTextNode($t))\n\
         $m::CreateToastNotifier('{POWERSHELL_APP}').Show([Windows.UI.Notifications.ToastNotification]::new($x))\n",
        b64.encode(text)
    );
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    b64.encode(utf16)
}

/// How long a command may take: powershell.exe can take seconds to start the first time.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Shows `text` the first way that works. Runs commands, so call it off the UI thread. Returns
/// whether it's left to the terminal: the caller writes `escape`.
pub fn send(setting: Setting, text: &str) -> bool {
    for way in ways(setting, &Env::current()) {
        match way {
            Way::Terminal => return true,
            Way::Command(program) => {
                let ran = crate::proc::run(&mut command(program, text), TIMEOUT);
                if ran.is_ok_and(|out| out.status.success()) {
                    return false;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn notifies_only_when_you_are_away() {
        use Setting::*;
        // The terminal said it lost the focus: any session, the one on screen too.
        assert!(wanted(Auto, Some(false), true));
        assert!(wanted(Terminal, Some(false), false));
        // Focused: you see the sidebar.
        assert!(!wanted(Auto, Some(true), false));
        // A terminal that never says: as for the bell, only a session out of sight.
        assert!(wanted(Auto, None, false));
        assert!(!wanted(Auto, None, true));
        assert!(!wanted(Off, Some(false), false));
        assert!(!wanted(Off, None, false));
    }

    #[test]
    fn says_the_key_and_why() {
        assert_eq!(text("ENG-52", true), "ENG-52 needs you: a permission prompt");
        assert_eq!(text("lead", false), "lead needs you: a question");
        assert_eq!(
            text("ENG\x1b]52;c;x\x07-1\nnext", false),
            "ENG ]52;c;x -1 next needs you: a question"
        );
        assert_eq!(
            escape("ENG-52 needs you: a question", false),
            "\x1b]9;taskman · ENG-52 needs you: a question\x07"
        );
        assert_eq!(
            escape("ENG-52 needs you: a question", true),
            "\x1bPtmux;\x1b\x1b]9;taskman · ENG-52 needs you: a question\x07\x1b\\"
        );
    }

    #[test]
    fn picks_the_way_per_platform() {
        use Way::*;
        let auto = |env: Env| ways(Setting::Auto, &env);
        assert_eq!(
            auto(Env { macos: true, ..Env::default() }),
            [Command("terminal-notifier"), Command("osascript"), Terminal]
        );
        // WSLg sets the display variables too, but has no notification service.
        let wsl = Env { wsl: true, desktop: true, ..Env::default() };
        assert_eq!(auto(wsl), [Command("powershell.exe"), Terminal]);
        assert_eq!(
            auto(Env { desktop: true, ..Env::default() }),
            [Command("notify-send"), Terminal]
        );
        assert_eq!(auto(Env::default()), [Terminal]);
        // A terminal that shows them, or one over SSH: only its own.
        assert_eq!(auto(Env { macos: true, shows_osc: true, ..Env::default() }), [Terminal]);
        assert_eq!(auto(Env { macos: true, ssh: true, tmux: true, ..Env::default() }), [Terminal]);
        // Inside tmux, which may not pass it on, the desktop's command first.
        let tmux = Env { desktop: true, shows_osc: true, tmux: true, ..Env::default() };
        assert_eq!(auto(tmux), [Command("notify-send"), Terminal]);
        assert_eq!(ways(Setting::Terminal, &Env { macos: true, ..Env::default() }), [Terminal]);
        assert_eq!(ways(Setting::Off, &Env { macos: true, ..Env::default() }), []);
    }

    #[test]
    fn commands_take_the_text_as_one_argument() {
        let args = |program| {
            let cmd = command(program, "ENG-52 needs you: <a> & b");
            cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>()
        };
        assert_eq!(
            args("notify-send"),
            ["--app-name=taskman", "--", "taskman", "ENG-52 needs you: &lt;a&gt; &amp; b"]
        );
        assert_eq!(args("osascript").last().unwrap(), "ENG-52 needs you: <a> & b");
        assert_eq!(args("terminal-notifier")[3], "ENG-52 needs you: <a> & b");
        // The toast's script carries the text as base64.
        let b64 = base64::engine::general_purpose::STANDARD;
        let script = args("powershell.exe").pop().unwrap();
        let utf16 = b64.decode(script).unwrap();
        let units: Vec<u16> = utf16.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.contains(&b64.encode("ENG-52 needs you: <a> & b")), "{script}");
        assert!(!script.contains("needs you"));
    }

    #[test]
    fn reads_the_setting_as_config_spells_it() {
        for s in Setting::ALL {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(serde_json::from_str::<Setting>(&json).unwrap(), s);
        }
        assert_eq!(serde_json::from_str::<Setting>(r#""OFF""#).unwrap(), Setting::Off);
        assert_eq!(serde_json::from_str::<Setting>(r#""bell""#).unwrap(), Setting::Auto);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn the_escape_carries_no_controls_of_the_label(label in "\\PC*|[\\x00-\\x1f\\x7f-\\x9f]*") {
            let osc = escape(&text(&label, false), false);
            let inner = osc.strip_prefix("\x1b]9;").unwrap().strip_suffix('\x07').unwrap();
            prop_assert!(!inner.chars().any(char::is_control), "{inner:?}");
        }
    }
}
