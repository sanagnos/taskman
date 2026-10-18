//! One `claude` process in a pseudo-terminal, with a vt100 emulator holding its screen.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

const SCROLLBACK: usize = 10_000;

/// Answers the terminal queries a real terminal would, and captures title/clipboard requests.
#[derive(Default)]
pub struct Callbacks {
    replies: Vec<u8>,
    pub title: String,
    pub clipboard: Vec<String>,
    /// What a colour query is answered with: the terminal's own, as `theme` found them.
    pub colours: crate::theme::Colours,
}

impl vt100::Callbacks for Callbacks {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).trim().to_string();
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _ty: &[u8], data: &[u8]) {
        self.clipboard.push(String::from_utf8_lossy(data).into_owned());
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

pub struct Session {
    pub parser: Arc<Mutex<Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    /// The CLI's process; None once `reap` has taken it.
    child: Option<Box<dyn Child + Send + Sync>>,
    exited: Arc<AtomicBool>,
    /// When the child last wrote output, in ms since the epoch.
    last_output: Arc<AtomicU64>,
    size: (u16, u16),
}

impl Session {
    pub fn spawn(
        cmd: CommandBuilder,
        rows: u16,
        cols: u16,
        dirty: Arc<AtomicBool>,
        colours: crate::theme::Colours,
    ) -> anyhow::Result<Self> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair =
            native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows,
            cols,
            SCROLLBACK,
            Callbacks { colours, ..Callbacks::default() },
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let last_output = Arc::new(AtomicU64::new(now_ms()));

        {
            let (parser, writer, exited, last_output) =
                (parser.clone(), writer.clone(), exited.clone(), last_output.clone());
            let mut killer = child.clone_killer();
            std::thread::spawn(move || {
                // A panic here (in vt100, say) ends this session only; the others and the TUI
                // go on.
                let read = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut buf = [0u8; 16384];
                    let panic_on = test_panic();
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if panic_on
                                    .as_ref()
                                    .is_some_and(|m| buf[..n].windows(m.len()).any(|w| w == m))
                                {
                                    panic!("test panic in the session's reader");
                                }
                                let replies = {
                                    let mut p = lock(&parser);
                                    p.process(&buf[..n]);
                                    std::mem::take(&mut p.callbacks_mut().replies)
                                };
                                if !replies.is_empty() {
                                    let mut w = lock(&writer);
                                    let _ = w.write_all(&replies);
                                    let _ = w.flush();
                                }
                                last_output.store(now_ms(), Ordering::Relaxed);
                                dirty.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }));
                if read.is_err() {
                    // Nothing reads its output any more, so the CLI would hang on its next write.
                    let _ = killer.kill();
                }
                exited.store(true, Ordering::Relaxed);
                dirty.store(true, Ordering::Relaxed);
            });
        }

        Ok(Self {
            parser,
            writer,
            master: pair.master,
            child: Some(child),
            exited,
            last_output,
            size: (rows, cols),
        })
    }

    pub fn write(&self, bytes: &[u8]) {
        let mut w = lock(&self.writer);
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }

    pub fn paste(&self, text: &str) {
        let bracketed = lock(&self.parser).screen().bracketed_paste();
        if bracketed {
            self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.write(text.replace('\n', "\r").as_bytes());
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        lock(&self.parser).screen_mut().set_size(rows, cols);
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
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "scrollback is capped at the parser's history length, far below i32::MAX"
    )]
    pub fn scroll(&self, delta: i32) {
        let mut p = lock(&self.parser);
        let cur = p.screen().scrollback() as i32;
        p.screen_mut().set_scrollback((cur + delta).max(0) as usize);
    }

    /// True when the program has turned on mouse reporting (Claude Code's full-screen mode does).
    pub fn wants_mouse(&self) -> bool {
        lock(&self.parser).screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    /// Sends one mouse-wheel step at a cell of the pane, encoded the way the program asked for.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the legacy encoding clamps each coordinate to 223, so 32 + it fits a byte"
    )]
    pub fn send_wheel(&self, col: u16, row: u16, up: bool) {
        let enc = lock(&self.parser).screen().mouse_protocol_encoding();
        let button = if up { 64u8 } else { 65 };
        let (c, r) = (col as u32 + 1, row as u32 + 1);
        match enc {
            vt100::MouseProtocolEncoding::Sgr => {
                self.write(format!("\x1b[<{button};{c};{r}M").as_bytes());
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

    pub fn take_clipboard(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.parser).callbacks_mut().clipboard)
    }
}

/// In a debug build, output that makes the session's reader panic, as vt100 might: what
/// `TASKMAN_TEST_PANIC` holds. For tests/lifecycle.rs.
fn test_panic() -> Option<Vec<u8>> {
    let marker = std::env::var_os("TASKMAN_TEST_PANIC")
        .filter(|m| cfg!(debug_assertions) && !m.is_empty())?;
    Some(marker.into_encoded_bytes())
}

/// Locks a session's parser or writer even after a thread panicked holding it: the reader thread
/// can panic inside vt100, and that must end only its own session, not the TUI on its next draw.
pub fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[allow(
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
/// buffer, and what it shows is whatever the test feeds its parser (`Session::output`).
#[cfg(test)]
pub mod fake {
    use super::{Callbacks, Session, now_ms};
    use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex};

    /// The bytes a fake session was sent.
    pub type Sent = Arc<Mutex<Vec<u8>>>;

    struct Writer(Sent);

    impl std::io::Write for Writer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            super::lock(&self.0).extend_from_slice(buf);
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

    /// The fake CLI's process: running until it's waited for. Given `exit`, waiting lasts until
    /// the test sends on it or drops it, as a CLI slow to quit makes the reaper wait.
    #[derive(Debug)]
    struct Process {
        exit: Option<Mutex<std::sync::mpsc::Receiver<()>>>,
    }

    impl ChildKiller for Process {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Process { exit: None })
        }
    }

    impl Child for Process {
        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            Ok(None)
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

    /// Ends a `Session::fake_slow`'s CLI when sent on, or dropped.
    pub type Release = std::sync::mpsc::Sender<()>;

    impl Session {
        pub fn fake(rows: u16, cols: u16) -> (Self, Sent) {
            let (session, sent) = Self::fake_with(rows, cols, None);
            (session, sent)
        }

        /// A fake whose CLI goes on after it's stopped until the `Release` is used: its reaper
        /// runs that long, as with a CLI that takes a moment to quit.
        pub fn fake_slow(rows: u16, cols: u16) -> (Self, Sent, Release) {
            let (release, exit) = std::sync::mpsc::channel();
            let (session, sent) = Self::fake_with(rows, cols, Some(exit));
            (session, sent, release)
        }

        fn fake_with(
            rows: u16,
            cols: u16,
            exit: Option<std::sync::mpsc::Receiver<()>>,
        ) -> (Self, Sent) {
            let sent = Sent::default();
            let parser = vt100::Parser::new_with_callbacks(
                rows,
                cols,
                super::SCROLLBACK,
                Callbacks::default(),
            );
            let session = Self {
                parser: Arc::new(Mutex::new(parser)),
                writer: Arc::new(Mutex::new(Box::new(Writer(sent.clone())))),
                master: Box::new(Master),
                child: Some(Box::new(Process { exit: exit.map(Mutex::new) })),
                exited: Arc::new(AtomicBool::new(false)),
                last_output: Arc::new(AtomicU64::new(now_ms())),
                size: (rows, cols),
            };
            (session, sent)
        }

        /// The CLI ended, as the reader noticing its output closed would say.
        pub fn end(&self) {
            self.exited.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        /// Shows `bytes` as if the CLI had written them.
        pub fn output(&self, bytes: &[u8]) {
            super::lock(&self.parser).process(bytes);
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

    fn parser(rows: u16, cols: u16) -> Parser {
        vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK, Callbacks::default())
    }

    /// A narrower window cuts a wide character at the end of a line in half; erasing or printing
    /// over what's left of it used to panic in vt100 and kill the session's CLI.
    #[test]
    fn a_wide_character_cut_by_a_resize_can_be_erased_and_overprinted() {
        for then in [&b"\r\x1b[73C\x1b[K"[..], b"\r\x1b[K", b"\x1b[1K", b"\x1b[J", b"\r\x1b[73Cx"] {
            let mut p = parser(38, 75);
            p.process(format!("{}漢", "a".repeat(73)).as_bytes());
            p.screen_mut().set_size(38, 74);
            p.process(then);
            assert!(!p.screen().contents().contains('漢'), "{then:?}");
        }
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
                        Err((rows, cols)) => p.screen_mut().set_size(rows, cols),
                    }
                }
            }
        }
    }
}
