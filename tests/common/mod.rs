//! Running the taskman binary in a pty, as in a terminal window, with state and config kept in a
//! scratch folder.
// Each test binary uses its own part of this.
#![allow(dead_code)]

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Held from opening a pty until taskman has its copy, so no other test's child inherits it before
/// it's close-on-exec: a leftover copy would keep the terminal open.
static SPAWN: Mutex<()> = Mutex::new(());

/// The taskman binary with its state and config in `dir`, and none of the environment that would
/// make it think it runs inside a session or change how it draws, so tests pass in any shell: a
/// test that wants NO_COLOR sets it. `spawn_in_pty` sets TERM. Its home is `dir/home`, and
/// `stub_tools` go first on PATH, so no test reaches the developer's own gh login or keychain.
pub fn taskman(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_taskman"));
    for (k, _) in std::env::vars_os() {
        if k.to_str().is_some_and(|k| {
            k.starts_with("TASKMAN_")
                || ["TMUX", "CLAUDECODE", "NO_COLOR", "COLORFGBG", "COLORTERM"].contains(&k)
        }) {
            cmd.env_remove(k);
        }
    }
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let path =
        format!("{}:{}", stub_tools(dir).display(), std::env::var("PATH").unwrap_or_default());
    cmd.env("XDG_STATE_HOME", dir.join("state"))
        .env("TASKMAN_CONFIG", dir.join("config.json"))
        .env("TASKMAN_NO_AUTOUPDATE", "1")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PATH", path);
    cmd
}

/// `dir/bin`, where tests put the commands they stub, with gh and the keychain tools (`security`,
/// `secret-tool`) that fail saying they're stubbed, unless the test put its own there first.
/// Returns the folder, which `taskman` puts first on PATH.
pub fn stub_tools(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["gh", "security", "secret-tool"] {
        if !bin.join(name).exists() {
            script(
                &bin,
                name,
                "#!/bin/sh\necho \"$(basename \"$0\") is stubbed in the tests\" >&2\nexit 1\n",
            );
        }
    }
    bin
}

/// Starts `cmd` in a new pty, as its controlling terminal. Returns it and the pty's master side,
/// which doesn't block.
pub fn spawn_in_pty(mut cmd: Command) -> (std::process::Child, std::fs::File) {
    let _guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let (mut m, mut s) = (-1, -1);
    // SAFETY: openpty writes two descriptors into `m` and `s`; the null name, termios and window
    // size are optional.
    let r = unsafe {
        libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(r, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: openpty succeeded, so both are open descriptors that nothing else owns.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
    let size = libc::winsize { ws_row: 40, ws_col: 160, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCSWINSZ reads one winsize, from `size`, which outlives the call.
    unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ, &size) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: sets a flag on descriptors this function owns; it reads or writes no memory.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: as above.
    unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
    cmd.env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    // Its own session with the pty as the controlling terminal, as in a terminal window.
    // SAFETY: the closure runs in the child between fork and exec; it allocates nothing and calls
    // only setsid and ioctl, which are async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    (cmd.spawn().unwrap(), std::fs::File::from(master))
}

/// Answers the terminal queries in taskman's output `out`, as a terminal would: crossterm waits on
/// them.
pub fn answer_queries(master: &mut std::fs::File, out: &[u8]) {
    if contains(out, b"\x1b[?u") {
        let _ = master.write_all(b"\x1b[?0u");
    }
    if contains(out, b"\x1b[c") {
        let _ = master.write_all(b"\x1b[?1;2c");
    }
    // Where the cursor is: ratatui asks as it clears the screen, after the editor.
    if contains(out, b"\x1b[6n") {
        let _ = master.write_all(b"\x1b[1;1R");
    }
}

/// As `answer_queries`, from a terminal with black text on white, which also says so when asked
/// (OSC 10 and 11), before the device attributes as a terminal does.
pub fn answer_as_light(master: &mut std::fs::File, out: &[u8]) {
    if contains(out, b"\x1b]10;?") {
        let _ = master.write_all(b"\x1b]10;rgb:0000/0000/0000\x07");
    }
    if contains(out, b"\x1b]11;?") {
        let _ = master.write_all(b"\x1b]11;rgb:ffff/ffff/ffff\x07");
    }
    answer_queries(master, out);
}

/// A stub CLI for sessions: writes its pid to `pid_file` and sleeps.
pub fn stub_cli(dir: &Path, pid_file: &Path) -> PathBuf {
    let stub = dir.join("claude");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\necho $$ > '{}'\nexec sleep 300\n", pid_file.display()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    stub
}

pub fn scratch_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "taskman-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Ends the test binary, saying where a test was, if it runs past its limit: a hung test would
/// otherwise hold CI until the job's timeout and print nothing, as captured output only shows once
/// a test ends.
pub struct Watchdog {
    step: Arc<Mutex<String>>,
    done: Arc<AtomicBool>,
}

impl Watchdog {
    /// `on_timeout` runs first, to stop what the test started and add what it knows.
    pub fn start(
        name: &'static str,
        limit: Duration,
        on_timeout: impl FnOnce() + Send + 'static,
    ) -> Self {
        let step = Arc::new(Mutex::new("starting".to_string()));
        let done = Arc::new(AtomicBool::new(false));
        let (s, d) = (step.clone(), done.clone());
        std::thread::spawn(move || {
            let until = Instant::now() + limit;
            while Instant::now() < until {
                if d.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let at = s.lock().map(|s| s.clone()).unwrap_or_default();
            // Straight to stderr: eprintln! would be captured with the test's output.
            let _ = writeln!(
                std::io::stderr(),
                "\n{name} still at \"{at}\" after {limit:?}; ending it"
            );
            on_timeout();
            std::process::exit(101);
        });
        Watchdog { step, done }
    }

    /// Says what the test does next, for the note.
    pub fn step(&self, what: &str) {
        if let Ok(mut s) = self.step.lock() {
            *s = what.to_string();
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

// ---- the terminal, read on a thread ------------------------------------------------------------

/// What taskman's terminal shows (through the patched vt100, which keeps links), and every byte it
/// wrote there.
pub struct Terminal {
    pub parser: vt100::Parser,
    pub raw: Vec<u8>,
}

impl Terminal {
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }
}

/// How long a wait for the screen or a file gives up after: far past what any step takes, so only a
/// broken behaviour reaches it.
pub const WAIT: Duration = Duration::from_secs(15);

/// taskman in a pty, its output read into a `Terminal` on a thread: the screen is always current,
/// terminal queries are answered, and taskman never blocks on output nobody reads (on macOS a
/// process can't finish exiting while it does). Dropping it kills taskman and the stub CLIs and
/// removes the scratch folder.
pub struct Tui {
    pub child: std::process::Child,
    pub dir: PathBuf,
    term: Arc<Mutex<Terminal>>,
    writer: std::fs::File,
    stop: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
    watchdog: Watchdog,
}

impl Tui {
    /// Starts `cmd` in a pty with a `limit` for the whole test, past which the test binary ends.
    pub fn start(name: &'static str, dir: &Path, cmd: Command, limit: Duration) -> Self {
        Self::start_with(name, dir, cmd, limit, || {})
    }

    /// As `start`, with `on_timeout` run first if the test goes past its limit: to let go of what
    /// holds taskman up, or to print what the test knows.
    pub fn start_with(
        name: &'static str,
        dir: &Path,
        cmd: Command,
        limit: Duration,
        on_timeout: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self::launch(name, dir, cmd, limit, on_timeout, answer_queries)
    }

    /// As `start`, in a terminal with a white background (`answer_as_light`).
    pub fn start_light(name: &'static str, dir: &Path, cmd: Command, limit: Duration) -> Self {
        Self::launch(name, dir, cmd, limit, || {}, answer_as_light)
    }

    fn launch(
        name: &'static str,
        dir: &Path,
        cmd: Command,
        limit: Duration,
        on_timeout: impl FnOnce() + Send + 'static,
        answer: fn(&mut std::fs::File, &[u8]),
    ) -> Self {
        let (child, master) = spawn_in_pty(cmd);
        let pid = child.id().cast_signed();
        let d = dir.to_path_buf();
        let watchdog = Watchdog::start(name, limit, move || {
            on_timeout();
            kill(pid);
            stubs(&d).into_iter().for_each(kill);
        });
        let term = Arc::new(Mutex::new(Terminal {
            parser: vt100::Parser::new(40, 160, 1000),
            raw: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let writer = master.try_clone().unwrap();
        let reader = {
            let (term, stop, mut master) = (term.clone(), stop.clone(), master);
            std::thread::spawn(move || {
                let mut buf = [0u8; 65536];
                while !stop.load(Ordering::Relaxed) {
                    match std::io::Read::read(&mut master, &mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            answer(&mut master, &buf[..n]);
                            let mut t = term.lock().unwrap_or_else(|e| e.into_inner());
                            t.raw.extend_from_slice(&buf[..n]);
                            t.parser.process(&buf[..n]);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Tui { child, dir: dir.to_path_buf(), term, writer, stop, reader: Some(reader), watchdog }
    }

    /// Says what the test does next, for the note if it goes past its limit.
    pub fn step(&self, what: &str) {
        self.watchdog.step(what);
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
    }

    /// Runs `f` on the terminal as it is now.
    pub fn with<T>(&self, f: impl FnOnce(&Terminal) -> T) -> T {
        f(&self.term.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// The screen's text, a line a row.
    pub fn text(&self) -> String {
        self.with(|t| t.screen().contents())
    }

    /// Waits for `found` to find what it looks for on the screen, and returns it. Fails the test
    /// with `what` and the screen if it doesn't within `WAIT`.
    pub fn wait_for<T>(&self, what: &str, found: impl Fn(&vt100::Screen) -> Option<T>) -> T {
        let until = Instant::now() + WAIT;
        loop {
            if let Some(v) = self.with(|t| found(t.screen())) {
                return v;
            }
            assert!(
                Instant::now() < until,
                "{what}: not on screen after {WAIT:?}:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits for `text` on the screen and returns where it starts, as `find` does.
    pub fn wait_text(&self, text: &str) -> (u16, u16) {
        self.wait_for(text, |s| find(s, text))
    }

    /// Waits for `done`, which looks at something other than the screen (a file, a process).
    pub fn wait_until(&self, what: &str, mut done: impl FnMut() -> bool) {
        let until = Instant::now() + WAIT;
        while !done() {
            assert!(Instant::now() < until, "{what} after {WAIT:?}; the screen:\n{}", self.text());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits for taskman to end, and returns how it did.
    pub fn wait_exit(&mut self) -> std::process::ExitStatus {
        let until = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < until,
                "taskman was still running after {WAIT:?}:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether the terminal is in cooked mode, as a shell expects it: lines, echoed. Read through
    /// the master side, which reports the terminal's settings on Linux and macOS alike; macOS
    /// revokes the other side once taskman, which leads its session, has gone.
    pub fn cooked(&self) -> bool {
        // SAFETY: a termios is plain integers; tcgetattr fills it and it outlives the call.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: tcgetattr writes one termios, into `t`, which outlives the call.
        let r = unsafe { libc::tcgetattr(self.writer.as_raw_fd(), &mut t) };
        assert_eq!(r, 0, "tcgetattr: {}", std::io::Error::last_os_error());
        t.c_lflag & libc::ICANON != 0 && t.c_lflag & libc::ECHO != 0
    }

    /// Resizes the terminal, as dragging a window's edge does: taskman gets a SIGWINCH.
    pub fn resize(&self, rows: u16, cols: u16) {
        self.term
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .parser
            .screen_mut()
            .set_size(rows, cols);
        let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ reads one winsize, from `size`, which outlives the call.
        let r = unsafe { libc::ioctl(self.writer.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        assert_eq!(r, 0, "TIOCSWINSZ: {}", std::io::Error::last_os_error());
    }

    // Mouse reports in SGR encoding, as taskman turns on; `col` and `row` count from 0.

    pub fn press(&mut self, col: u16, row: u16) {
        self.send(format!("\x1b[<0;{};{}M", col + 1, row + 1).as_bytes());
    }

    pub fn drag(&mut self, col: u16, row: u16) {
        self.send(format!("\x1b[<32;{};{}M", col + 1, row + 1).as_bytes());
    }

    pub fn release(&mut self, col: u16, row: u16) {
        self.send(format!("\x1b[<0;{};{}m", col + 1, row + 1).as_bytes());
    }

    pub fn wheel(&mut self, up: bool, col: u16, row: u16) {
        self.send(
            format!("\x1b[<{};{};{}M", if up { 64 } else { 65 }, col + 1, row + 1).as_bytes(),
        );
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        // taskman first, while the reader still drains its output.
        kill(self.child.id().cast_signed());
        let _ = self.child.wait();
        stubs(&self.dir).into_iter().for_each(kill);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn kill(pid: i32) {
    if pid > 0 {
        // SAFETY: sends a signal; no memory is involved.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// Where `text` starts on the screen: its column and row.
pub fn find(screen: &vt100::Screen, text: &str) -> Option<(u16, u16)> {
    let (rows, cols) = screen.size();
    (0..rows).find_map(|row| {
        // A cell a character a column wide each, so a column is a character.
        let cell = |col| {
            screen
                .cell(row, col)
                .map(vt100::Cell::contents)
                .filter(|c| !c.is_empty())
                .unwrap_or(" ")
        };
        let line: String = (0..cols).map(cell).collect();
        let at = line.find(text)?;
        Some((u16::try_from(line[..at].chars().count()).unwrap(), row))
    })
}

/// The text of the highlighted (reversed) cells, a line a row: what a selection shows.
pub fn highlighted(screen: &vt100::Screen) -> String {
    let (rows, cols) = screen.size();
    let lines: Vec<String> = (0..rows)
        .filter_map(|row| {
            let cells: String = (0..cols)
                .filter_map(|col| screen.cell(row, col).filter(|c| c.inverse()))
                .map(|c| {
                    if c.contents().is_empty() { " ".to_string() } else { c.contents().to_string() }
                })
                .collect();
            (!cells.is_empty()).then(|| cells.trim_end().to_string())
        })
        .collect();
    lines.join("\n")
}

pub fn script(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

// ---- a project, and stubs for what taskman runs ------------------------------------------------

/// Project `acme` in `dir/repo` with a Local files ticket for each of `keys`, in tickets.json in
/// that order. Returns the repo.
pub fn project(dir: &Path, keys: &[&str]) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("workplan/todo")).unwrap();
    for key in keys {
        std::fs::write(
            repo.join(format!("workplan/todo/{key}-test.md")),
            format!("# {key} test\n"),
        )
        .unwrap();
    }
    let state = dir.join("state/taskman");
    std::fs::create_dir_all(&state).unwrap();
    let tickets: Vec<_> =
        keys.iter().map(|k| serde_json::json!({"key": k, "repo": repo, "cwd": repo})).collect();
    let saved = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "local", "site": "workplan", "key": "ENG"}}],
        "tickets": tickets,
    });
    std::fs::write(state.join("tickets.json"), saved.to_string()).unwrap();
    repo
}

/// The scripted CLI the sessions run (`TASKMAN_CLAUDE`). The session's pty stays in cooked mode, so
/// the kernel turns a ctrl+c or ctrl+z that reaches it into SIGINT or SIGTSTP, which it notes. In
/// `dir`, for each ticket: `<KEY>.pid`, `<KEY>.signals` (one per line; SIGHUP and SIGTERM end
/// it), `<KEY>.input` (each line typed) and `<KEY>.ticks` (how far `stream` got). Lines typed:
///
/// - `lines N` prints `line 001` to `line N`, then `printed N`
/// - `stream` prints `tick 0001`, `tick 0002`... every 20ms
/// - `panic` prints `$TASKMAN_TEST_PANIC`, which makes a debug build's reader panic
/// - `styled` prints `bold red` in bold red and `underlined on green` underlined on green
/// - `colour` asks the terminal for its background (OSC 11) and prints `colour ` and the reply,
///   without its escapes
/// - anything else is answered with `got: <line>`
pub fn stub_session(dir: &Path) -> PathBuf {
    let text = r#"#!/bin/sh
f="DIR/$TASKMAN_TICKET"
echo $$ > "$f.pid"
trap 'echo INT >> "$f.signals"' INT
trap 'echo TSTP >> "$f.signals"' TSTP
stop() { echo $1 >> "$f.signals"; [ -s "$f.stream.pid" ] && kill $(cat "$f.stream.pid"); exit 0; }
trap 'stop HUP' HUP
trap 'stop TERM' TERM
echo "stub ready for $TASKMAN_TICKET"
fails=0
while :; do
  # A trapped signal ends a read early; only a run of failures is the terminal gone.
  if ! IFS= read -r line; then
    fails=$((fails + 1)); [ $fails -gt 100 ] && exit 1; continue
  fi
  fails=0
  printf '%s\n' "$line" >> "$f.input"
  case "$line" in
    "lines "*)
      i=1; n=${line#lines }
      while [ $i -le $n ]; do printf 'line %03d\n' $i; i=$((i + 1)); done
      echo "printed $n";;
    stream)
      (i=1; while printf 'tick %04d\n' $i; do echo $i > "$f.ticks"; i=$((i + 1)); sleep 0.02; done) &
      echo $! > "$f.stream.pid";;
    panic) printf '%s\n' "$TASKMAN_TEST_PANIC";;
    styled) printf '\033[1;31mbold red\033[0m \033[4;42munderlined on green\033[0m\n';;
    colour)
      stty -icanon -echo min 0 time 20
      printf '\033]11;?\007'
      reply=$(dd bs=64 count=1 2>/dev/null | tr -d '\033\007')
      stty icanon echo
      echo "colour $reply";;
    *) echo "got: $line";;
  esac
done
"#;
    script(dir, "claude", &text.replace("DIR", &dir.display().to_string()))
}

/// The pids the stub sessions in `dir` wrote.
pub fn stubs(dir: &Path) -> Vec<i32> {
    let files = std::fs::read_dir(dir).into_iter().flatten().flatten();
    files
        .filter(|e| e.path().extension().is_some_and(|x| x == "pid"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok()?.trim().parse().ok())
        .collect()
}

/// What stub session `key` in `dir` wrote to its file `<key>.<what>`.
pub fn stub_file(dir: &Path, key: &str, what: &str) -> String {
    std::fs::read_to_string(dir.join(format!("{key}.{what}"))).unwrap_or_default()
}

/// Commands taskman may run that would reach the desktop, as stubs in `dir/bin`: every clipboard
/// command taskman tries writes what it's given to `dir/clipboard.<name>`, and the browser does
/// nothing. Returns the folder, which `taskman` puts first on PATH.
pub fn stub_desktop(dir: &Path) -> PathBuf {
    let bin = stub_tools(dir);
    for name in ["pbcopy", "clip.exe", "wl-copy", "xclip", "xsel"] {
        // Whole or not at all: a test may read it as it's written.
        let d = dir.display();
        script(
            &bin,
            name,
            &format!(
                "#!/bin/sh\ncat > '{d}/copying.{name}' && mv '{d}/copying.{name}' '{d}/clipboard.{name}'\n"
            ),
        );
    }
    script(&bin, "browser", "#!/bin/sh\n");
    bin
}

/// What the last copy put on the clipboard, if anything has. clip.exe (on WSL) is given UTF-16LE.
pub fn clipboard(dir: &Path) -> Option<String> {
    let files = std::fs::read_dir(dir).ok()?.flatten();
    let newest = files
        .filter(|e| e.file_name().to_string_lossy().starts_with("clipboard."))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())?;
    let bytes = std::fs::read(newest.path()).ok()?;
    Some(if newest.file_name() == "clipboard.clip.exe" {
        let units: Vec<u16> =
            bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// taskman with the stub session and desktop, on the project in `dir`.
pub fn stubbed(dir: &Path) -> Command {
    let bin = stub_desktop(dir);
    let mut cmd = taskman(dir);
    cmd.env("TASKMAN_CLAUDE", stub_session(dir))
        .env("BROWSER", bin.join("browser"))
        // So Linux without a desktop still has a clipboard command (the stub xclip) to try.
        .env("DISPLAY", ":99")
        .env_remove("WAYLAND_DISPLAY");
    cmd
}

// ---- git repositories, and a git the tests can hold up ------------------------------------------

/// Runs `script` in `cwd` with git set up as the tests need it.
pub fn sh(cwd: &Path, script: &str) {
    let ok = Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .unwrap()
        .success();
    assert!(ok, "{script}");
}

/// A project `acme` in `dir` with an origin and one commit on main, pushed. Returns its folder.
pub fn acme(dir: &Path) -> std::path::PathBuf {
    let repo = dir.join("acme");
    sh(dir, "git init -q --bare -b main origin.git && git clone -q origin.git acme 2>/dev/null");
    sh(
        &repo,
        "git commit -q --allow-empty -m one && git push -q origin main && git remote set-head origin main",
    );
    repo
}

/// A git in `dir/bin` that waits at `worktree remove FOLDER` until the test writes
/// `dir/release-FOLDER`, after touching `dir/removing-FOLDER`, in the folder `taskman` puts first
/// on PATH.
pub fn slow_git(dir: &Path) {
    let real_git = Command::new("sh").args(["-c", "command -v git"]).output().unwrap();
    let real_git = String::from_utf8_lossy(&real_git.stdout).trim().to_string();
    let bin = stub_tools(dir);
    let shim = format!(
        "#!/bin/sh\ncase \"$*\" in *'worktree remove'*)\n  eval last=\\${{$#}}; f=$(basename \"$last\")\n  \
         touch '{d}/removing-'$f; i=0\n  while [ ! -e '{d}/release-'$f ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done;;\n\
         esac\nexec '{real_git}' \"$@\"\n",
        d = dir.display()
    );
    script(&bin, "git", &shim);
}

/// A git in `dir/bin` that waits at `worktree add` until the test writes `dir/release-FOLDER`,
/// after touching `dir/adding-FOLDER`, and touches `dir/added-FOLDER` once the worktree is made, in
/// the folder `taskman` puts first on PATH.
pub fn slow_add_git(dir: &Path) {
    let real_git = Command::new("sh").args(["-c", "command -v git"]).output().unwrap();
    let real_git = String::from_utf8_lossy(&real_git.stdout).trim().to_string();
    let bin = stub_tools(dir);
    // `worktree add ... FOLDER START`: the folder is the argument before the last.
    let shim = format!(
        "#!/bin/sh\ncase \"$*\" in *'worktree add'*)\n  eval folder=\\${{$(($# - 1))}}; f=$(basename \"$folder\")\n  \
         touch '{d}/adding-'$f; i=0\n  while [ ! -e '{d}/release-'$f ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done\n  \
         '{real_git}' \"$@\"; s=$?; touch '{d}/added-'$f; exit $s;;\nesac\nexec '{real_git}' \"$@\"\n",
        d = dir.display()
    );
    script(&bin, "git", &shim);
}
