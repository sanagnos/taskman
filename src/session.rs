//! One `claude` process in a pseudo-terminal, with a vt100 emulator holding its screen.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};

/// How much input may wait for a session's CLI to read it. Past this, `Session::write` drops what
/// it's given rather than queue more for a CLI that isn't reading.
pub const QUEUE: usize = 4 << 20;

/// Answers the terminal queries a real terminal would, and captures title/clipboard requests.
#[derive(Default)]
pub struct Callbacks {
    replies: Vec<u8>,
    pub title: String,
    /// The last clipboard write since `take_clipboard`, numbered across sessions (`WRITES`) so
    /// the newest of theirs can be told.
    pub clipboard: Option<(u64, ClipboardWrite)>,
    /// Why clipboard requests weren't forwarded since `take_clipboard`, each once.
    pub refused: Vec<Refused>,
    /// What a colour query is answered with: the terminal's own, as `theme` found them.
    pub colours: crate::theme::Colours,
}

impl Callbacks {
    /// Notes why a request was refused, once for each kind (the latest size for `TooLarge`).
    fn refuse(&mut self, why: Refused) {
        match self.refused.iter_mut().find(|r| r.same(why)) {
            Some(r) => *r = why,
            None => self.refused.push(why),
        }
    }
}

/// The most base64 a session's clipboard write may have, so that the whole sequence taskman
/// writes (`ESC ] 52 ; c ;`, the data, BEL) stays under tmux's `input-buffer-size`, 1 MiB by
/// default, past which tmux drops it without a word. Claude Code sets no limit of its own, and
/// other terminals take more, but taskman is often run in tmux. 750 KB of text.
pub const CLIPBOARD_MAX: usize = 1_000_000;

/// How many clipboard writes the sessions have made, which numbers the next.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// A clipboard write to forward: the selection the session named (`c`, `p`, `s`, a digit, or
/// none), which vt100 checked, and the base64.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardWrite {
    pub target: String,
    pub data: String,
}

impl ClipboardWrite {
    /// One of taskman's own copies, to the clipboard.
    pub fn own(text: &str) -> Self {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(text);
        Self { target: "c".into(), data }
    }
}

/// Why a session's OSC 52 request didn't reach the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It asked to read the clipboard.
    Read,
    /// An OSC 52 that isn't a plain write: not base64, an unknown selection, extra parameters.
    Malformed,
    /// A write over `CLIPBOARD_MAX`, with the size of the text it held.
    TooLarge(usize),
}

impl Refused {
    /// Which to warn of first when several come at once: a read, which a program may have meant
    /// to be quiet, then what the user tried to copy.
    pub fn rank(self) -> u8 {
        match self {
            Self::Read => 0,
            Self::TooLarge(_) => 1,
            Self::Malformed => 3,
        }
    }

    /// Whether `self` and `other` are the same kind of refusal, whatever the size.
    pub fn same(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }

    /// The toast, for the session called `name`. Never what it tried to copy.
    pub fn toast(self, name: &str) -> String {
        match self {
            Self::Read => format!("{name} asked to read your clipboard; taskman refused"),
            Self::Malformed => {
                format!("{name} sent a clipboard request taskman can't read; ignored")
            }
            Self::TooLarge(bytes) => {
                format!("{name} copied {}, more than tmux takes; not copied", size(bytes))
            }
        }
    }
}

/// "750 KB", "1.2 MB": a size for a toast, in thousands as file managers count.
fn size(bytes: usize) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", (bytes + 500) / 1000)
    } else {
        let tenths = (bytes + 50_000) / 100_000;
        format!("{}.{} MB", tenths / 10, tenths % 10)
    }
}

impl vt100::Callbacks for Callbacks {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = crate::untrusted::line(&String::from_utf8_lossy(title)).trim().to_string();
    }

    // vt100 calls this only for a plain write: a known selection and base64 alone, so nothing in
    // `data` can end the escape sequence it's forwarded in (the tests below hold vt100 to that).
    // The rest goes to `paste_from_clipboard` or `unhandled_osc`.
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, ty: &[u8], data: &[u8]) {
        // Only the last of a burst matters: each write replaces the clipboard (an empty one
        // clears it).
        if data.len() > CLIPBOARD_MAX {
            // Refused whole: a cut-off copy would pass for the real thing.
            self.clipboard = None;
            self.refuse(Refused::TooLarge((data.len() * 3 + 2) / 4));
        } else {
            let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            let write = ClipboardWrite { target: text(ty), data: text(data) };
            self.clipboard = Some((WRITES.fetch_add(1, Ordering::Relaxed), write));
        }
    }

    // Reads are never forwarded: a terminal that answers one types the user's clipboard back as
    // input, and taskman's input goes to the selected session, so any program in any session
    // could read the clipboard. The answer is an empty clipboard, as zellij gives: with none,
    // Neovim and others wait seconds for one. vt100 doesn't say how the request ended, so BEL.
    fn paste_from_clipboard(&mut self, _: &mut vt100::Screen, ty: &[u8]) {
        self.replies.extend_from_slice(b"\x1b]52;");
        self.replies.extend_from_slice(ty);
        self.replies.extend_from_slice(b";\x07");
        self.refuse(Refused::Read);
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let p0 = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c) {
            // Device status report / cursor position report.
            (None, 'n') if p0 == 5 => self.replies.extend_from_slice(b"\x1b[0n"),
            (None, 'n') if p0 == 6 => {
                let (row, col) = screen.cursor_position();
                self.replies.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            // Primary and secondary device attributes.
            (None, 'c') => self.replies.extend_from_slice(b"\x1b[?62;22c"),
            (Some(b'>'), 'c') => self.replies.extend_from_slice(b"\x1b[>1;10;0c"),
            _ => {}
        }
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        if params.first() == Some(&&b"52"[..]) {
            return self.refuse(Refused::Malformed);
        }
        // Foreground and background colour queries.
        if params.len() >= 2
            && params[1] == b"?"
            && let Some(reply) = self.colours.answer(params[0])
        {
            self.replies.extend(reply);
        }
    }
}

pub type Parser = vt100::Parser<Callbacks>;

/// Wakes the event loop, which waits on the other end (`Tty::wait`), for a session's output and
/// the threads' replies (`app::sync::Sender`). Unit tests' Apps have none.
#[derive(Clone, Default)]
pub struct Wake(Option<Arc<UnixStream>>);

impl Wake {
    /// `stream` must be non-blocking: a full buffer already wakes the loop.
    pub fn new(stream: UnixStream) -> Self {
        Self(Some(Arc::new(stream)))
    }

    pub fn wake(&self) {
        if let Some(s) = &self.0 {
            let _ = (&**s).write(&[0]);
        }
    }
}

/// Why a session ended, for its pane to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// The CLI exited: how, once it's been waited for.
    Exited(Option<Exit>),
    /// The CLI didn't start: why (`start_error`).
    StartFailed(String),
    /// The session's reader panicked (in vt100, say), so taskman stopped its CLI.
    ReaderCrashed,
}

/// How a CLI's process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(u32),
    /// The signal's description, as `strsignal` gives it: "Killed", "Hangup".
    Signal(String),
}

impl From<portable_pty::ExitStatus> for Exit {
    fn from(status: portable_pty::ExitStatus) -> Self {
        match status.signal() {
            Some(signal) => Self::Signal(signal.to_string()),
            None => Self::Code(status.exit_code()),
        }
    }
}

pub struct Session {
    pub parser: Arc<Mutex<Parser>>,
    input: Input,
    master: Box<dyn MasterPty + Send>,
    /// The CLI's process; None once `reap` has taken it.
    child: Option<Box<dyn Child + Send + Sync>>,
    exited: Arc<AtomicBool>,
    /// The reader panicked; set before `exited`.
    crashed: Arc<AtomicBool>,
    /// When the child last wrote output, in ms since the epoch.
    last_output: Arc<AtomicU64>,
    /// Output since the pane was last drawn (`drawn`). Only the selected session's is drawn
    /// for; the others' stays set, so their readers don't wake the loop again.
    fresh: Arc<AtomicBool>,
    size: (u16, u16),
}

impl Session {
    /// Starts `cmd` in a pty. `name` names the session's reader thread, so panics.log says whose
    /// reader it was.
    pub fn spawn(
        cmd: CommandBuilder,
        name: &str,
        rows: u16,
        cols: u16,
        scrollback: usize,
        wake: Wake,
        colours: crate::theme::Colours,
    ) -> anyhow::Result<Self> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair =
            native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let input = Input::spawn(pair.master.take_writer()?)?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            crate::term::dim(rows),
            crate::term::dim(cols),
            scrollback,
            Callbacks { colours, ..Callbacks::default() },
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let crashed = Arc::new(AtomicBool::new(false));
        let last_output = Arc::new(AtomicU64::new(now_ms()));
        let fresh = Arc::new(AtomicBool::new(false));

        {
            let (parser, input, exited, crashed, last_output, fresh) = (
                parser.clone(),
                input.clone(),
                exited.clone(),
                crashed.clone(),
                last_output.clone(),
                fresh.clone(),
            );
            let printed = move || {
                if !fresh.swap(true, Ordering::Relaxed) {
                    wake.wake();
                }
            };
            let mut killer = child.clone_killer();
            std::thread::Builder::new().name(format!("reader {name}")).spawn(move || {
                // A panic here (in vt100, say) ends this session only; the others and the TUI
                // go on.
                let read = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut buf = [0u8; 16384];
                    let panic_on = test_panic();
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let replies = {
                                    let mut p = lock(&parser);
                                    // As vt100 would: part-way through the output, holding the
                                    // lock, so the screen is left half-updated and the lock
                                    // poisoned.
                                    if let Some(at) = panic_on.as_ref().and_then(|m| {
                                        buf[..n].windows(m.len()).position(|w| w == m)
                                    }) {
                                        p.process(&buf[..at]);
                                        #[expect(
                                            clippy::panic,
                                            reason = "the debug builds' test of a reader's panic"
                                        )]
                                        {
                                            panic!("test panic in the session's reader");
                                        }
                                    }
                                    p.process(&buf[..n]);
                                    std::mem::take(&mut p.callbacks_mut().replies)
                                };
                                input.send(replies);
                                last_output.store(now_ms(), Ordering::Relaxed);
                                printed();
                            }
                        }
                    }
                }));
                if read.is_err() {
                    // Nothing reads its output any more, so the CLI would hang on its next write.
                    let _ = killer.kill();
                    crashed.store(true, Ordering::Relaxed);
                }
                exited.store(true, Ordering::Relaxed);
                printed();
            })?;
        }

        Ok(Self {
            parser,
            input,
            master: pair.master,
            child: Some(child),
            exited,
            crashed,
            last_output,
            fresh,
            size: (rows, cols),
        })
    }

    /// Queues `bytes` for the CLI and returns at once, however slowly it reads them. False if
    /// they were dropped: they'd have taken more than `QUEUE` waiting for it.
    #[must_use]
    pub fn write(&self, bytes: &[u8]) -> bool {
        self.input.send(bytes.to_vec())
    }

    /// As `write`, for text pasted: bracketed if the CLI asked for that.
    #[must_use]
    pub fn paste(&self, text: &str) -> bool {
        let bracketed = lock(&self.parser).screen().bracketed_paste();
        if bracketed {
            self.input.send(format!("\x1b[200~{text}\x1b[201~").into_bytes())
        } else {
            self.input.send(text.replace('\n', "\r").into_bytes())
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        // The screen first: the CLI redraws for its new size as soon as the pty has it, and
        // that output must land on a screen of that size. vt100 0.18 moves rows between the screen
        // and its history on a resize, so the redraw of a CLI that got there first would be
        // taken as drawn on the old screen, and rows of its history overwritten.
        lock(&self.parser).screen_mut().set_size(crate::term::dim(rows), crate::term::dim(cols));
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    /// Whether the CLI has written anything, or ended, since `drawn`.
    pub fn fresh(&self) -> bool {
        self.fresh.load(Ordering::Relaxed)
    }

    /// The pane is about to be drawn, with all the CLI has written: the next output wakes the
    /// loop.
    pub fn drawn(&self) {
        self.fresh.store(false, Ordering::Relaxed);
    }

    /// How long since the child last wrote anything.
    pub fn quiet_for(&self) -> std::time::Duration {
        std::time::Duration::from_millis(
            now_ms().saturating_sub(self.last_output.load(Ordering::Relaxed)),
        )
    }

    /// How many lines on the screen show `needle`, as at the bottom however far the view is
    /// scrolled back (a selection keeps it scrolled back a row; see `selection::Snapshot`).
    pub fn screen_count(&self, needle: &str) -> usize {
        let mut p = lock(&self.parser);
        let back = p.screen().scrollback();
        p.screen_mut().set_scrollback(0);
        let found = p.screen().contents().lines().filter(|l| l.contains(needle)).count();
        p.screen_mut().set_scrollback(back);
        found
    }

    /// The screen's text as at the bottom, however far the view is scrolled back.
    pub fn screen_text(&self) -> String {
        let mut p = lock(&self.parser);
        let back = p.screen().scrollback();
        p.screen_mut().set_scrollback(0);
        let text = p.screen().contents();
        p.screen_mut().set_scrollback(back);
        text
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    /// Why the session ended; None while it runs. An exit's code may come a moment after its
    /// output closed, once the CLI's process has ended too.
    pub fn end(&mut self) -> Option<End> {
        if !self.exited() {
            return None;
        }
        if self.crashed.load(Ordering::Relaxed) {
            return Some(End::ReaderCrashed);
        }
        let status = self.child.as_mut().and_then(|c| c.try_wait().ok().flatten());
        Some(End::Exited(status.map(Exit::from)))
    }

    /// The screen as it is at the bottom, without its history: what an ended session's pane goes
    /// on showing once the session is let go.
    pub fn last_screen(&self) -> Parser {
        let mut p = lock(&self.parser);
        let back = p.screen().scrollback();
        p.screen_mut().set_scrollback(0);
        let (rows, cols) = p.screen().size();
        let mut copy = vt100::Parser::new_with_callbacks(rows, cols, 0, Callbacks::default());
        copy.process(p.screen().contents_formatted().as_bytes());
        p.screen_mut().set_scrollback(back);
        copy
    }

    /// Asks the CLI to quit as closing its terminal would, and lets it finish on its own time
    /// (dropping the session gives it only a moment before killing it).
    pub fn hang_up(&self) {
        let pid = self.child.as_ref().and_then(|c| c.process_id());
        if let Some(pid) = pid.and_then(|p| libc::pid_t::try_from(p).ok()) {
            // SAFETY: sends a signal; no memory is involved.
            unsafe { libc::kill(pid, libc::SIGHUP) };
        }
    }

    /// The CLI's process id, None for a fake or once reaped.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.process_id())
    }

    /// Whether the CLI's process has ended; its output may still be on its way.
    pub fn ended(&mut self) -> bool {
        self.exited() || self.child.as_mut().is_none_or(|c| matches!(c.try_wait(), Ok(Some(_))))
    }

    /// Ends the CLI as closing its terminal would, and reaps it, on a thread of its own: `kill`
    /// hangs it up, gives it 200ms to end on that and then kills it, too long for the UI thread
    /// to wait, and without the wait an ended CLI stays a zombie until taskman quits. Returns
    /// the thread, unless the CLI was reaped already.
    fn reap(&mut self) -> Option<Reaper> {
        let mut child = self.child.take()?;
        if matches!(child.try_wait(), Ok(Some(_))) {
            return None;
        }
        std::thread::Builder::new()
            .name("reaper".into())
            .spawn(move || {
                let _ = child.kill();
                let _ = child.wait();
            })
            .ok()
            .map(Reaper)
    }

    /// Stops the session: its CLI is hung up and reaped on a thread (`reap`), returned so the
    /// caller can wait for it, or see that it's done, before anything takes the CLI's place.
    /// Dropping a session does the same without the handle.
    pub fn stop(mut self) -> Option<Reaper> {
        self.reap()
    }

    /// Scrolls the view into history; positive goes back in time.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "scrollback is capped at the parser's history length, far below i32::MAX"
    )]
    pub fn scroll(&self, delta: i32) {
        let mut p = lock(&self.parser);
        let cur = p.screen().scrollback() as i32;
        p.screen_mut().set_scrollback(usize::try_from(cur + delta).unwrap_or(0));
    }

    /// True when the program has turned on mouse reporting (Claude Code's full-screen mode does).
    pub fn wants_mouse(&self) -> bool {
        lock(&self.parser).screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    /// Sends one mouse-wheel step at a cell of the pane, encoded the way the program asked for.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the legacy encoding clamps each coordinate to 223, so 32 + it fits a byte"
    )]
    #[must_use]
    pub fn send_wheel(&self, col: u16, row: u16, up: bool) -> bool {
        let enc = lock(&self.parser).screen().mouse_protocol_encoding();
        let button = if up { 64u8 } else { 65 };
        let (c, r) = (u32::from(col) + 1, u32::from(row) + 1);
        match enc {
            vt100::MouseProtocolEncoding::Sgr => {
                self.write(format!("\x1b[<{button};{c};{r}M").as_bytes())
            }
            _ => self.write(&[
                0x1b,
                b'[',
                b'M',
                32 + button,
                (32 + c.min(223)) as u8,
                (32 + r.min(223)) as u8,
            ]),
        }
    }

    pub fn reset_scroll(&self) {
        let mut p = lock(&self.parser);
        if p.screen().scrollback() != 0 {
            p.screen_mut().set_scrollback(0);
        }
    }

    pub fn application_cursor(&self) -> bool {
        lock(&self.parser).screen().application_cursor()
    }

    pub fn title(&self) -> String {
        lock(&self.parser).callbacks().title.clone()
    }

    /// The clipboard write to forward, and why any requests weren't.
    pub fn take_clipboard(&self) -> (Option<(u64, ClipboardWrite)>, Vec<Refused>) {
        let mut parser = lock(&self.parser);
        let cb = parser.callbacks_mut();
        (std::mem::take(&mut cb.clipboard), std::mem::take(&mut cb.refused))
    }
}

/// Why `cmd` didn't start, for the pane: what to do when its program isn't there (as with no
/// Claude Code installed, or a `TASKMAN_CLAUDE` that names nothing), else portable_pty's words.
pub fn start_error(
    cmd: &CommandBuilder,
    error: &anyhow::Error,
    install: &str,
    var: &str,
) -> String {
    let program = cmd.get_argv().first().map(|p| p.to_string_lossy().into_owned());
    let text = format!("{error:#}");
    // portable_pty's words for a program it found nowhere (`search_path`), rather than one on
    // PATH that "exists but" can't be run.
    let missing = ["No viable candidates found in PATH", "doesn't exist", "does not exist"];
    match program {
        Some(program)
            if missing.iter().any(|m| text.contains(m)) && !text.contains("exists but") =>
        {
            format!(
                "{program} isn't installed, or isn't on PATH: install {install}, or start taskman \
                 with {var} set to its path."
            )
        }
        _ => text,
    }
}

/// In a debug build, output that makes the session's reader panic, as vt100 might: what
/// `TASKMAN_TEST_PANIC` holds. For tests/lifecycle.rs.
fn test_panic() -> Option<Vec<u8>> {
    let marker = std::env::var_os("TASKMAN_TEST_PANIC")
        .filter(|m| cfg!(debug_assertions) && !m.is_empty())?;
    Some(marker.into_encoded_bytes())
}

/// What a session sends its CLI, written to the pty by a thread of its own, so neither the UI
/// thread nor the reader (answering a query) waits on a CLI that's slow to read: with them
/// writing it themselves, a long paste to a CLI that read slowly and sent queries as it did held
/// all three up for good. Each clone sends to the same thread, which ends once they're all gone.
#[derive(Clone)]
struct Input {
    tx: mpsc::Sender<Vec<u8>>,
    /// Bytes sent and not yet written.
    queued: Arc<AtomicUsize>,
}

impl Input {
    fn spawn(mut w: Box<dyn Write + Send>) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let left = queued.clone();
        std::thread::Builder::new().name("pty writer".into()).spawn(move || {
            // After a failed write (the CLI gone), what's left is dropped as it comes.
            let mut open = true;
            for bytes in rx {
                open = open && w.write_all(&bytes).and_then(|()| w.flush()).is_ok();
                left.fetch_sub(bytes.len(), Ordering::Release);
            }
        })?;
        Ok(Self { tx, queued })
    }

    /// Queues `bytes` for the writer thread, unless they'd take the queue past `QUEUE`.
    fn send(&self, bytes: Vec<u8>) -> bool {
        let n = bytes.len();
        if n == 0 {
            return true;
        }
        if self.queued.fetch_add(n, Ordering::AcqRel) + n > QUEUE {
            self.queued.fetch_sub(n, Ordering::AcqRel);
            return false;
        }
        // The thread lives as long as any clone, so this only fails if it panicked.
        if self.tx.send(bytes).is_err() {
            self.queued.fetch_sub(n, Ordering::AcqRel);
        }
        true
    }
}

/// Locks a session's parser even after a thread panicked holding it: the reader thread
/// can panic inside vt100, and that must end only its own session, not the TUI on its next draw.
pub fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "milliseconds since 1970 fit u64 for millions of years"
)]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reap();
    }
}

/// A stopped session's CLI on its way out: the thread hanging it up and reaping it (`reap`).
/// Until it's done, the CLI may still be running, on its conversation and in its folder.
pub struct Reaper(std::thread::JoinHandle<()>);

impl Reaper {
    /// Whether the CLI is gone: reaped, so not even a zombie.
    pub fn done(&self) -> bool {
        self.0.is_finished()
    }

    /// Waits until the CLI is gone. Not on the UI thread: it can take `kill`'s 200ms.
    pub fn wait(self) {
        let _ = self.0.join();
    }
}

/// Stops `sessions` together and waits until each has ended, and for `reapers`, from sessions
/// stopped earlier: at quit, so no CLI runs on unattended. It takes about as long as one (see
/// `reap`), however many there are.
pub fn stop_all(sessions: Vec<Session>, reapers: Vec<Reaper>) {
    let reapers: Vec<_> = sessions.into_iter().filter_map(Session::stop).chain(reapers).collect();
    for reaper in reapers {
        reaper.wait();
    }
}

/// A session with no process behind it, for tests: what's written to it lands in the returned
/// `Sent`, through a writer thread as a real session's does, and what it shows is whatever the
/// test feeds its parser (`Session::output`).
#[cfg(test)]
pub mod fake {
    use super::{Callbacks, Input, Session, now_ms};
    use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// The bytes a fake session was sent.
    pub struct Sent {
        pub(super) bytes: Arc<Mutex<Vec<u8>>>,
        queued: Arc<AtomicUsize>,
    }

    impl Sent {
        /// What the session was sent, once its writer thread has written all of it: none of it
        /// can still be on its way, so a test can tell that nothing was sent.
        pub fn bytes(&self) -> Vec<u8> {
            let until = Instant::now() + Duration::from_secs(5);
            while self.queued.load(Ordering::Acquire) != 0 {
                assert!(Instant::now() < until, "the fake session's writer is stuck");
                std::thread::yield_now();
            }
            super::lock(&self.bytes).clone()
        }
    }

    /// The fake pty's writer: given a gate, each write waits until the test drops its `Release`,
    /// as a CLI that doesn't read holds a real one up.
    struct Writer {
        sent: Arc<Mutex<Vec<u8>>>,
        gate: Option<std::sync::mpsc::Receiver<()>>,
    }

    impl std::io::Write for Writer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Some(gate) = &self.gate {
                let _ = gate.recv();
            }
            super::lock(&self.sent).extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct Master;

    impl MasterPty for Master {
        fn resize(&self, _: PtySize) -> anyhow::Result<()> {
            Ok(())
        }
        fn get_size(&self) -> anyhow::Result<PtySize> {
            Ok(PtySize::default())
        }
        fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
            Ok(Box::new(std::io::empty()))
        }
        fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
            Ok(Box::new(std::io::sink()))
        }
        fn process_group_leader(&self) -> Option<libc::pid_t> {
            None
        }
        fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
            None
        }
        fn tty_name(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    /// The fake CLI's process: running until it's waited for, or until it exits with `code`.
    /// Given `exit`, waiting lasts until the test sends on it or drops it, as a CLI slow to quit
    /// makes the reaper wait.
    #[derive(Debug)]
    struct Process {
        exit: Option<Mutex<std::sync::mpsc::Receiver<()>>>,
        code: Option<u32>,
    }

    impl ChildKiller for Process {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Process { exit: None, code: None })
        }
    }

    impl Child for Process {
        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            Ok(self.code.map(ExitStatus::with_exit_code))
        }
        fn wait(&mut self) -> std::io::Result<ExitStatus> {
            if let Some(exit) = &self.exit {
                let _ = super::lock(exit).recv();
            }
            Ok(ExitStatus::with_exit_code(0))
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    /// Ends a `Session::fake_slow`'s CLI when sent on, or dropped; lets a `Session::fake_stuck`
    /// read its input when dropped.
    pub type Release = std::sync::mpsc::Sender<()>;

    impl Session {
        pub fn fake(rows: u16, cols: u16) -> (Self, Sent) {
            Self::fake_with(rows, cols, None, None)
        }

        /// A fake whose CLI goes on after it's stopped until the `Release` is used: its reaper
        /// runs that long, as with a CLI that takes a moment to quit.
        pub fn fake_slow(rows: u16, cols: u16) -> (Self, Sent, Release) {
            let (release, exit) = std::sync::mpsc::channel();
            let (session, sent) = Self::fake_with(rows, cols, Some(exit), None);
            (session, sent, release)
        }

        /// A fake whose CLI reads none of its input until the `Release` is dropped: its writer
        /// thread waits on the first write till then.
        pub fn fake_stuck(rows: u16, cols: u16) -> (Self, Sent, Release) {
            let (release, gate) = std::sync::mpsc::channel();
            let (session, sent) = Self::fake_with(rows, cols, None, Some(gate));
            (session, sent, release)
        }

        fn fake_with(
            rows: u16,
            cols: u16,
            exit: Option<std::sync::mpsc::Receiver<()>>,
            gate: Option<std::sync::mpsc::Receiver<()>>,
        ) -> (Self, Sent) {
            let bytes = Arc::<Mutex<Vec<u8>>>::default();
            let input = Input::spawn(Box::new(Writer { sent: bytes.clone(), gate }))
                .expect("a thread for the fake's writer");
            let sent = Sent { bytes, queued: input.queued.clone() };
            let parser = vt100::Parser::new_with_callbacks(
                crate::term::dim(rows),
                crate::term::dim(cols),
                crate::store::SCROLLBACK,
                Callbacks::default(),
            );
            let session = Self {
                parser: Arc::new(Mutex::new(parser)),
                input,
                master: Box::new(Master),
                child: Some(Box::new(Process { exit: exit.map(Mutex::new), code: None })),
                exited: Arc::new(AtomicBool::new(false)),
                crashed: Arc::new(AtomicBool::new(false)),
                last_output: Arc::new(AtomicU64::new(now_ms())),
                fresh: Arc::new(AtomicBool::new(false)),
                size: (rows, cols),
            };
            (session, sent)
        }

        /// The CLI's output closed, as the reader would find it; its process isn't waited for.
        pub fn close(&self) {
            self.exited.store(true, Ordering::Relaxed);
        }

        /// The CLI exited with `code`.
        pub fn exit(&mut self, code: u32) {
            self.child = Some(Box::new(Process { exit: None, code: Some(code) }));
            self.close();
        }

        /// The reader panicked, and stopped the CLI.
        pub fn crash(&self) {
            self.crashed.store(true, Ordering::Relaxed);
            self.close();
        }

        /// Shows `bytes` as if the CLI had written them, and sends it the replies, as the
        /// reader does.
        pub fn output(&self, bytes: &[u8]) {
            let replies = {
                let mut p = super::lock(&self.parser);
                p.process(bytes);
                std::mem::take(&mut p.callbacks_mut().replies)
            };
            self.input.send(replies);
            self.fresh.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_survives_a_poisoned_mutex() {
        let m = Arc::new(Mutex::new(1));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(m.is_poisoned());
        *lock(&m) += 1;
        assert_eq!(*lock(&m), 2);
    }

    /// A CLI that reads nothing doesn't hold up whoever writes to it: a long paste and the keys
    /// after it return at once, and past `QUEUE` the rest is dropped instead.
    #[test]
    fn writing_to_a_cli_that_doesnt_read_returns_at_once() {
        let (session, sent, release) = Session::fake_stuck(24, 80);
        let start = std::time::Instant::now();
        let paste = "x".repeat(100_000);
        assert!(session.paste(&paste));
        assert!(session.write(b"\r"));
        let big = vec![b'y'; QUEUE];
        assert!(!session.write(&big), "more than the queue holds was taken");
        assert!(session.write(b"z"), "a key that still fits was dropped");
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "{:?}", start.elapsed());

        drop(release);
        assert_eq!(sent.bytes(), [paste.as_bytes(), b"\r", b"z"].concat());
    }

    /// The writer thread ends with its session, even one whose CLI never read what it was sent.
    #[test]
    fn the_writer_thread_ends_with_the_session() {
        for stuck in [false, true] {
            let (session, sent, release) = if stuck {
                Session::fake_stuck(24, 80)
            } else {
                let (session, sent) = Session::fake(24, 80);
                (session, sent, std::sync::mpsc::channel().0)
            };
            assert!(session.write(b"hello"));
            drop(session);
            drop(release);
            // The thread holds the fake pty's writer, which holds the other copy.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while Arc::strong_count(&sent.bytes) > 1 {
                assert!(std::time::Instant::now() < until, "the writer thread is still running");
                std::thread::yield_now();
            }
            assert_eq!(lock(&sent.bytes).as_slice(), b"hello");
        }
    }

    /// A program that isn't there, by path or on PATH, is told apart from other failures,
    /// against portable_pty's own errors.
    #[test]
    fn a_missing_program_says_what_to_do() {
        let dir = crate::store::scratch::dir();
        std::fs::create_dir_all(&dir).unwrap();
        let mut on_path = CommandBuilder::new("taskman-no-such-cli");
        on_path.env("PATH", &dir);
        let not_executable = dir.join("claude");
        std::fs::write(&not_executable, "").unwrap();
        let mut not_executable_on_path = CommandBuilder::new("claude");
        not_executable_on_path.env("PATH", &dir);
        let cases = [
            (CommandBuilder::new("/opt/acme/bin/claude"), Some("/opt/acme/bin/claude")),
            (on_path, Some("taskman-no-such-cli")),
            (CommandBuilder::new(&not_executable), None),
            (not_executable_on_path, None),
        ];
        for (cmd, missing) in cases {
            let e = Session::spawn(
                cmd.clone(),
                "ENG-1",
                24,
                80,
                crate::store::SCROLLBACK,
                Default::default(),
                Default::default(),
            )
            .err()
            .expect("it started");
            let why = start_error(&cmd, &e, "Claude Code", "TASKMAN_CLAUDE");
            match missing {
                Some(program) => assert_eq!(
                    why,
                    format!(
                        "{program} isn't installed, or isn't on PATH: install Claude Code, or \
                         start taskman with TASKMAN_CLAUDE set to its path."
                    )
                ),
                None => assert!(why.contains("not executable"), "{why}"),
            }
        }
    }

    fn parser(rows: u16, cols: u16) -> Parser {
        vt100::Parser::new_with_callbacks(
            crate::term::dim(rows),
            crate::term::dim(cols),
            crate::store::SCROLLBACK,
            Callbacks::default(),
        )
    }

    /// A narrower window cuts a wide character at the end of a line in half; erasing or printing
    /// over what's left of it used to panic in vt100 and kill the session's CLI.
    #[test]
    fn a_wide_character_cut_by_a_resize_can_be_erased_and_overprinted() {
        for then in [&b"\r\x1b[73C\x1b[K"[..], b"\r\x1b[K", b"\x1b[1K", b"\x1b[J", b"\r\x1b[73Cx"] {
            let mut p = parser(38, 75);
            p.process(format!("{}漢", "a".repeat(73)).as_bytes());
            p.screen_mut().set_size(crate::term::dim(38), crate::term::dim(74));
            p.process(then);
            assert!(!p.screen().contents().contains('漢'), "{then:?}");
        }
    }

    /// An emoji that VS16 widens, alone in the bottom-right cell, wraps onto a new row and
    /// scrolls the screen; vt100 used to leave half of it in the row above, and the next print to
    /// that row's last column panicked and killed the session's CLI.
    #[test]
    fn an_emoji_widened_in_the_last_cell_can_be_printed_beside() {
        for then in ["\x1b[23;80He", "\x1b[23;79H\u{26a0}\u{fe0f}", "\x1b[23;79H  "] {
            let mut p = parser(24, 80);
            p.process(format!("\x1b[24;79H \u{26a0}\u{fe0f}{then}").as_bytes());
            let rows: Vec<String> = p.screen().rows(0, 80).collect();
            assert!(rows[23].starts_with("\u{26a0}\u{fe0f}"), "{then:?}: {rows:?}");
        }
    }

    /// What `p` would forward of `output`, and why it refused the rest.
    fn clipboard_of(output: &[u8]) -> (Option<ClipboardWrite>, Vec<Refused>) {
        let mut p = parser(24, 80);
        p.process(output);
        let cb = p.callbacks_mut();
        (cb.clipboard.take().map(|(_, w)| w), std::mem::take(&mut cb.refused))
    }

    fn write(target: &str, data: &str) -> Option<ClipboardWrite> {
        Some(ClipboardWrite { target: target.into(), data: data.into() })
    }

    #[test]
    fn a_clipboard_write_is_forwarded_and_the_last_of_a_burst_wins() {
        assert_eq!(clipboard_of(b"\x1b]52;c;aGVsbG8=\x07"), (write("c", "aGVsbG8="), vec![]));
        assert_eq!(clipboard_of(b"\x1b]52;c;aGVsbG8=\x1b\\"), (write("c", "aGVsbG8="), vec![]));
        let burst = b"\x1b]52;c;b25l\x07\x1b]52;c;dHdv\x07\x1b]52;c;dGhyZWU=\x07";
        assert_eq!(clipboard_of(burst), (write("c", "dGhyZWU="), vec![]));
        // An empty write clears the clipboard, as it would without taskman.
        assert_eq!(clipboard_of(b"\x1b]52;c;\x07"), (write("c", ""), vec![]));
        // Carriage returns and newlines inside are dropped by the parser, as a terminal would.
        assert_eq!(clipboard_of(b"\x1b]52;c;aGVs\r\nbG8=\x07"), (write("c", "aGVsbG8="), vec![]));
    }

    /// The selection is forwarded as the session named it, as tmux does: the primary selection,
    /// several, or none (which a terminal takes as its default).
    #[test]
    fn a_clipboard_write_keeps_its_selection() {
        for target in ["p", "s", "pc", "0", ""] {
            let output = format!("\x1b]52;{target};aGVsbG8=\x07");
            assert_eq!(clipboard_of(output.as_bytes()), (write(target, "aGVsbG8="), vec![]));
        }
    }

    /// The clipboard writes vt100 passes on are written into an escape sequence as they are, so
    /// anything that could end it (ESC, BEL, ST) or isn't base64 must be refused before taskman
    /// sees it; and a read must never reach the terminal (see `paste_from_clipboard`).
    #[test]
    fn a_clipboard_read_or_anything_but_a_plain_write_is_refused() {
        assert_eq!(clipboard_of(b"\x1b]52;c;?\x07"), (None, vec![Refused::Read]));
        assert_eq!(clipboard_of(b"\x1b]52;c;?\x1b\\"), (None, vec![Refused::Read]));
        for output in [
            &b"\x1b]52;c;aGVs bG8=\x07"[..],
            b"\x1b]52;c;aGVs\x7fbG8=\x07",
            b"\x1b]52;c;aGVs\x9cbG8=\x07",
            "\x1b]52;c;aGVs\u{9c}bG8=\x07".as_bytes(),
            b"\x1b]52;z;aGVsbG8=\x07",
            b"\x1b]52;c;aGVs;bG8=\x07",
            b"\x1b]52;c;evil*escape;test\x07",
            b"\x1b]52;aGVsbG8=\x07",
        ] {
            assert_eq!(clipboard_of(output), (None, vec![Refused::Malformed]), "{output:?}");
        }
    }

    /// A read is answered with an empty clipboard, for the selection asked about, so a program
    /// waiting on the answer goes on at once.
    #[test]
    fn a_clipboard_read_is_answered_with_nothing() {
        let mut p = parser(24, 80);
        p.process(b"\x1b]52;c;?\x07\x1b]52;p;?\x1b\\");
        assert_eq!(p.callbacks().replies, b"\x1b]52;c;\x07\x1b]52;p;\x07");
    }

    /// An ESC ends the sequence there, as in a terminal: what came before it is the write, and the
    /// rest is ordinary output.
    #[test]
    fn an_escape_inside_a_clipboard_write_ends_it() {
        let (clipboard, refused) = clipboard_of(b"\x1b]52;c;aGVs\x1b]0;bG8=\x07");
        assert_eq!((clipboard, refused), (write("c", "aGVs"), vec![]));
        let (clipboard, _) = clipboard_of(b"\x1b]52;c;aGVs\x1b[31mbG8=\x07");
        assert_eq!(clipboard, write("c", "aGVs"));
    }

    /// The limit is on the base64, and the toast gives the text's size, never the text.
    #[test]
    fn a_clipboard_write_over_the_limit_is_refused_and_leaves_nothing_to_forward() {
        let at_limit = "A".repeat(CLIPBOARD_MAX);
        let (clipboard, refused) = clipboard_of(format!("\x1b]52;c;{at_limit}\x07").as_bytes());
        assert_eq!((clipboard.map(|w| w.data.len()), refused), (Some(CLIPBOARD_MAX), vec![]));
        let over = format!("\x1b]52;c;aGVsbG8=\x07\x1b]52;c;{at_limit}AAAA\x07\x1b]52;c;?\x07");
        let (clipboard, refused) = clipboard_of(over.as_bytes());
        assert_eq!(
            (clipboard, &refused[..]),
            (None, &[Refused::TooLarge(750_003), Refused::Read][..])
        );
        assert_eq!(
            refused[0].toast("ENG-52"),
            "ENG-52 copied 750 KB, more than tmux takes; not copied"
        );
        assert_eq!(
            Refused::TooLarge(3_400_000).toast("x"),
            "x copied 3.4 MB, more than tmux takes; not copied"
        );
    }

    #[test]
    fn window_title_is_sanitized_against_untrusted_bidi_and_controls() {
        let mut p = parser(24, 80);
        p.process("\x1b]2;Hello\u{202e}World\x07".as_bytes());
        assert_eq!(p.callbacks().title, "HelloWorld");
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Pieces of output that move the cursor, erase, print wide characters, or are any
        /// bytes at all, and resizes within `Session::resize`'s limits.
        fn step() -> impl Strategy<Value = Result<Vec<u8>, (u16, u16)>> {
            let piece = prop_oneof![
                Just(b"\x1b[K".to_vec()),
                Just(b"\x1b[1K".to_vec()),
                Just(b"\x1b[2K".to_vec()),
                Just(b"\x1b[J".to_vec()),
                Just(b"\x1b[1J".to_vec()),
                Just(b"\x1b[3X".to_vec()),
                Just(b"\x1b[2P".to_vec()),
                Just(b"\x1b[2@".to_vec()),
                Just(b"\r\n".to_vec()),
                Just("漢✅🚀".as_bytes().to_vec()),
                // Emoji VS16 widens, one in the bottom-right cell, and the end of the row above,
                // where one wrapping from that cell used to leave half of itself.
                Just("\u{26a0}\u{fe0f}1\u{fe0f}".as_bytes().to_vec()),
                Just("\x1b[999;999H\u{26a0}\u{fe0f}".as_bytes().to_vec()),
                Just(b"\x1b[A\x1b[999C".to_vec()),
                (0u16..200).prop_map(|n| format!("\x1b[{n}C").into_bytes()),
                (0u16..60, 0u16..200).prop_map(|(r, c)| format!("\x1b[{r};{c}H").into_bytes()),
                "[a-z漢❌ ]{0,80}".prop_map(String::into_bytes),
                proptest::collection::vec(any::<u8>(), 0..32),
            ];
            prop_oneof![4 => piece.prop_map(Ok), 1 => (2u16..60, 10u16..200).prop_map(Err)]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn output_and_resizes_never_panic(
                steps in proptest::collection::vec(step(), 1..60),
            ) {
                let mut p = parser(24, 80);
                for step in steps {
                    match step {
                        Ok(bytes) => p.process(&bytes),
                        Err((rows, cols)) => {
                            p.screen_mut().set_size(crate::term::dim(rows), crate::term::dim(cols));
                        }
                    }
                }
            }

            /// Whatever a session prints, what's forwarded to the terminal is base64 alone.
            #[test]
            fn only_base64_is_ever_forwarded_to_the_clipboard(
                pieces in proptest::collection::vec(clipboard_piece(), 1..40),
            ) {
                let mut p = parser(24, 80);
                for piece in pieces {
                    p.process(&piece);
                    if let Some((_, write)) = p.callbacks_mut().clipboard.take() {
                        let base64 = |b: u8| b.is_ascii_alphanumeric() || b"+/=".contains(&b);
                        prop_assert!(write.data.bytes().all(base64), "{write:?}");
                        prop_assert!(write.target.bytes().all(|b| b"cpqs01234567".contains(&b)));
                    }
                }
            }
        }

        /// The start of a clipboard write, base64, what may end it, and any bytes at all.
        fn clipboard_piece() -> impl Strategy<Value = Vec<u8>> {
            prop_oneof![
                3 => "[cpqs0-7z?]{0,2}".prop_map(|ty| format!("\x1b]52;{ty};").into_bytes()),
                3 => "[A-Za-z0-9+/=]{0,40}".prop_map(String::into_bytes),
                1 => Just(b"\x07".to_vec()),
                1 => Just(b"\x1b\\".to_vec()),
                1 => Just(b"\x1b".to_vec()),
                1 => Just(b"\x9c".to_vec()),
                1 => Just("\u{9c}".as_bytes().to_vec()),
                1 => Just(b";".to_vec()),
                3 => proptest::collection::vec(any::<u8>(), 0..16),
            ]
        }
    }
}
