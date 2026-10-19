#[cfg(not(unix))]
compile_error!("taskman needs macOS or Linux (use WSL on Windows)");

mod add;
mod app;
mod background;
mod cli;
mod close;
mod github;
mod github_issues;
mod hook;
mod jira;
mod keychain;
mod keys;
mod linear;
mod links;
mod local;
mod proc;
mod proctree;
#[cfg(test)]
mod replay;
mod resume;
mod selection;
mod session;
mod session_ids;
mod stack;
mod status;
mod status_file;
mod store;
mod term;
mod theme;
mod token_file;
mod tracker;
mod ui;
mod untrusted;
mod update;
mod util;
mod worktree;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::EnterAlternateScreen;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Whether `take_terminal` pushed the kitty keyboard flags, for the panic hook to pop them.
static ENHANCED: AtomicBool = AtomicBool::new(false);

/// Whether the loop has ended and `shutdown` runs, which may wait for git: the watchdog gives it
/// that long.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// How long the terminal must stay gone before the watchdog ends taskman. The loop notices within
/// 16ms and stopping the sessions takes a fraction of a second, so this is only reached when
/// something hangs.
const GRACE: Duration = Duration::from_secs(3);

/// How long a quit nobody watches, after a signal or with the terminal gone, waits for a worktree
/// still being created or removed (`App::may_quit`) before it ends anyway. `prefix q` waits as
/// long as it takes, and says so on screen.
const QUIT_WAIT: Duration = Duration::from_secs(30);

/// How long after a second SIGTERM or SIGHUP the watchdog ends taskman if it's still running: the
/// shutdown ends within a moment of it, so this is only reached when the loop is stuck.
const SECOND_SIGNAL: Duration = Duration::from_secs(1);

/// Signals that end the TUI the way `ctrl+a q` does (see `shutdown`).
struct Signals {
    /// How many SIGTERMs and SIGHUPs (the terminal closed) have come. The first quits as `prefix q`
    /// does; a second ends the shutdown's wait, and the watchdog ends the process `SECOND_SIGNAL`
    /// after it in case the loop is stuck.
    stops: Arc<AtomicUsize>,
    /// How many SIGINTs. Only `kill -INT` sends it while the terminal is raw; ctrl+c in an editor
    /// that waits in cooked mode (`code --wait`) sends it too, so it's cleared when the editor
    /// returns.
    interrupts: Arc<AtomicUsize>,
    /// SIGHUP alone, for the watchdog.
    hangup: Arc<AtomicBool>,
}

impl Signals {
    fn register() -> std::io::Result<Self> {
        let s = Self { stops: Arc::default(), interrupts: Arc::default(), hangup: Arc::default() };
        for (sig, count) in [(SIGTERM, &s.stops), (SIGHUP, &s.stops), (SIGINT, &s.interrupts)] {
            let count = count.clone();
            // SAFETY: the handler only adds to an atomic, which is async-signal-safe.
            unsafe {
                signal_hook::low_level::register(sig, move || {
                    count.fetch_add(1, Ordering::Relaxed);
                })?;
            }
        }
        signal_hook::flag::register(SIGHUP, s.hangup.clone())?;
        Ok(s)
    }

    /// Whether a signal has asked taskman to quit.
    fn received(&self) -> bool {
        self.stops.load(Ordering::Relaxed) > 0 || self.interrupts.load(Ordering::Relaxed) > 0
    }

    /// Whether a second signal has come: the shutdown stops waiting for git and ends.
    fn again(&self) -> bool {
        self.stops.load(Ordering::Relaxed) + self.interrupts.load(Ordering::Relaxed) >= 2
    }
}

/// The terminal's input, the file crossterm reads keys from. When the terminal goes away (a closed
/// window, `tmux kill-server`, a dropped SSH connection), reading it gives end of file, and
/// crossterm 0.29 retries that forever inside `event::poll`: the loop never sees the SIGHUP and
/// spins. So `run` waits for input here and hands it to crossterm only while the terminal is there.
/// crossterm can still spin when the terminal goes while it's part-way through a key, mouse report
/// or paste, since it reads on until the sequence ends: `watchdog` ends taskman then.
///
/// crossterm 0.29 also loses input that comes with a resize: when a SIGWINCH and input are ready
/// at once, it reports the resize and drops the input, and as it waits for new input (edge
/// triggered) it won't read that input until more comes. So `wait` wakes on a SIGWINCH too, for
/// `run` to read each resize before input can arrive with it.
struct Tty {
    fd: RawFd,
    /// The file `fd` belongs to when it isn't stdin: `/dev/tty`, or the watchdog's copy.
    _file: Option<std::fs::File>,
    /// Readable after a SIGWINCH. The watchdog's copy has none: it only looks for the terminal
    /// going.
    resized: Option<UnixStream>,
}

#[derive(Debug, PartialEq)]
enum Input {
    Ready,
    Idle,
    Gone,
}

impl Tty {
    /// The file crossterm picks: stdin if it's a terminal, else `/dev/tty`.
    fn open() -> std::io::Result<Self> {
        // SAFETY: isatty only looks at a descriptor number; it reads or writes no memory.
        let (fd, file) = if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
            (libc::STDIN_FILENO, None)
        } else {
            let f = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty")?;
            (f.as_raw_fd(), Some(f))
        };
        let (resized, wake) = UnixStream::pair()?;
        resized.set_nonblocking(true)?;
        signal_hook::low_level::pipe::register(SIGWINCH, wake)?;
        Ok(Self { fd, _file: file, resized: Some(resized) })
    }

    fn try_clone(&self) -> std::io::Result<Self> {
        // SAFETY: `fd` is stdin or `_file`'s, open while `self` is, and the borrow ends with the
        // clone.
        let f =
            std::fs::File::from(unsafe { BorrowedFd::borrow_raw(self.fd) }.try_clone_to_owned()?);
        Ok(Self { fd: f.as_raw_fd(), _file: Some(f), resized: None })
    }

    /// Waits up to `timeout` for input or a resize, which crossterm reads. Through
    /// `filedescriptor::poll`, which uses select on macOS, where poll doesn't work on terminals.
    fn wait(&self, timeout: Duration) -> Input {
        self.poll(timeout, false)
    }

    /// As `wait`, for input crossterm won't read until more comes (see `Tty`): that input keeps
    /// the terminal readable, so only a resize or the terminal going ends the wait early. Ready
    /// while the input is still there, for `run` to try crossterm again.
    fn wait_held(&self, timeout: Duration) -> Input {
        self.poll(timeout, true)
    }

    fn poll(&self, timeout: Duration, held: bool) -> Input {
        // Without POLLIN, poll still reports a hangup, and select (macOS) an exception.
        let events = if held { 0 } else { libc::POLLIN };
        let resized = self.resized.as_ref().map_or(-1, AsRawFd::as_raw_fd);
        let mut fds = [
            libc::pollfd { fd: self.fd, events, revents: 0 },
            libc::pollfd { fd: resized, events: libc::POLLIN, revents: 0 },
        ];
        let fds = &mut fds[..if self.resized.is_some() { 2 } else { 1 }];
        match filedescriptor::poll(fds, Some(timeout)) {
            // An error is a signal arriving; the caller checks for those.
            Ok(0) | Err(_) if !held => return Input::Idle,
            _ => {}
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Input::Gone;
        }
        if let Some(r) = &self.resized
            && fds[1].revents & libc::POLLIN != 0
        {
            // Writes collate, so one read usually takes them all.
            let mut buf = [0u8; 64];
            while matches!((&*r).read(&mut buf), Ok(n) if n > 0) {}
            return Input::Ready;
        }
        // Readable with nothing to read is the end of file; select on macOS reports no hangup.
        let mut n: libc::c_int = 0;
        // SAFETY: FIONREAD writes one c_int, into `n`, which outlives the call.
        let read = unsafe { libc::ioctl(self.fd, libc::FIONREAD, &mut n) } == 0;
        match (read && n > 0, held) {
            (true, _) => Input::Ready,
            (false, false) => Input::Gone,
            // Not known to be readable, so not the end of file: the next `wait` tells.
            (false, true) => Input::Idle,
        }
    }
}

/// Ends taskman when the terminal has been gone for `GRACE` and it's still running: the loop is
/// stuck, most likely in crossterm (see `Tty`). On its own thread, as the loop can't be relied on.
/// Once `shutdown` runs, it may be waiting for git, and gets `QUIT_WAIT` more. A second SIGTERM
/// or SIGHUP ends taskman the same way, `SECOND_SIGNAL` later, if the shutdown hasn't. The
/// sessions' ptys close with the process, which hangs up their CLIs.
fn watchdog(tty: Tty, hangup: Arc<AtomicBool>, stops: Arc<AtomicUsize>) -> std::io::Result<()> {
    std::thread::Builder::new().name("watchdog".into()).spawn(move || {
        let (mut gone, mut again) = (Gone::default(), Gone::default());
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let now = Instant::now();
            // A SIGHUP that isn't the terminal closing ends the loop too.
            let left = hangup.load(Ordering::Relaxed) || tty.wait(Duration::ZERO) == Input::Gone;
            let grace =
                if SHUTTING_DOWN.load(Ordering::Relaxed) { GRACE + QUIT_WAIT } else { GRACE };
            let second = stops.load(Ordering::Relaxed) >= 2;
            if gone.for_at_least(left, now, grace) || again.for_at_least(second, now, SECOND_SIGNAL)
            {
                add::stopped();
                // SAFETY: _exit ends the process without running destructors or touching memory.
                unsafe { libc::_exit(1) };
            }
        }
    })?;
    Ok(())
}

/// How long the terminal has looked gone. Each check must find it gone: on macOS, input another
/// thread reads between `Tty::wait`'s select and its byte count looks like the end of file once.
#[derive(Default)]
struct Gone(Option<Instant>);

impl Gone {
    fn for_at_least(&mut self, gone: bool, now: Instant, grace: Duration) -> bool {
        if !gone {
            self.0 = None;
            return false;
        }
        now.duration_since(*self.0.get_or_insert(now)) >= grace
    }
}

fn main() -> anyhow::Result<()> {
    let Some(args) =
        std::env::args_os().map(|a| a.into_string().ok()).collect::<Option<Vec<String>>>()
    else {
        eprintln!("taskman: arguments must be valid UTF-8");
        std::process::exit(2);
    };
    // Hints in every command name the prefix; a hook prints none and runs often.
    if args.get(1).is_none_or(|a| a != "hook")
        && let Ok(config) = store::load_config()
    {
        keys::set_prefix(config.prefix);
    }
    match args.get(1).map(String::as_str) {
        Some("hook") => {
            hook::run(args.get(2).map(String::as_str).unwrap_or("idle"));
            return Ok(());
        }
        Some("status") => std::process::exit(status::run(&args[2..])),
        Some("add") => std::process::exit(add::run(&args[2..])),
        Some("close") => std::process::exit(close::run(&args[2..])),
        Some("move") => std::process::exit(local::run(&args[2..])),
        Some("resume") => std::process::exit(resume::run(&args[2..])),
        Some("update") => std::process::exit(update::run(&args[2..])),
        Some("-V" | "--version") => {
            cli::out!("taskman {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        // The notices of the crates built into the binary, which the install script and
        // `taskman update` don't put beside it as the archives do. `write_all`, as `print!` would
        // panic when a pager closes the pipe early.
        Some("--licenses") => {
            let _ = std::io::stdout().write_all(include_str!("../LICENSE-THIRD-PARTY").as_bytes());
            return Ok(());
        }
        Some("-h" | "--help") => {
            cli::out!(
                "taskman {}: several tickets at once, each with its session (Claude or Codex),",
                env!("CARGO_PKG_VERSION")
            );
            cli::out!(
                "tracker issue (Jira, Linear, GitHub Issues or a local file) and GitHub PR, grouped by project.\n"
            );
            cli::out!(
                "Usage: taskman              start the TUI ({} shows the command keys)",
                keys::prefix()
            );
            cli::out!(
                "       taskman status [KEY] a ticket's issue, worktree and PR, fetched fresh; without KEY,"
            );
            cli::out!(
                "                            the ticket or project of the current folder, else every"
            );
            cli::out!(
                "                            project (--project NAME, --json; --help for more)"
            );
            cli::out!(
                "       taskman add KEY...   add tickets to taskman without switching to them, with the"
            );
            cli::out!(
                "                            project's defaults; queued if taskman isn't running"
            );
            cli::out!(
                "                            (--agent NAME, --codex, --no-worktree; --help for more)"
            );
            cli::out!(
                "       taskman close KEY... close tickets, removing a worktree only if nothing would be lost;"
            );
            cli::out!(
                "                            queued if taskman isn't running (--merged, --keep-worktree,"
            );
            cli::out!("                            --agent NAME; --help for more)");
            cli::out!(
                "       taskman move KEY COLUMN  move a Local files ticket to another column of the board in"
            );
            cli::out!("                            its project folder (--help for more)");
            cli::out!(
                "       taskman resume KEY   continue a ticket's or agent's session in this terminal, as"
            );
            cli::out!(
                "                            taskman starts it; hand it off there first with {} t",
                keys::prefix()
            );
            cli::out!(
                "                            (--project NAME, --ticket, --agent; --help for more)"
            );
            cli::out!(
                "       taskman update       install the latest release (--force reinstalls it)"
            );
            cli::out!("       taskman --version    print the version");
            cli::out!(
                "       taskman --licenses   print the licenses of the third-party code in the binary"
            );
            cli::out!(
                "       taskman hook STATE   used by the Claude Code and Codex hooks taskman adds\n"
            );
            cli::out!(
                "Set up projects and accounts inside taskman: {} , opens Settings.\n",
                keys::prefix()
            );
            cli::out!("Environment: TASKMAN_CLAUDE  the claude command to run (default: claude)");
            cli::out!("             TASKMAN_CODEX   the codex command to run (default: codex)");
            cli::out!("             TASKMAN_CONFIG  another accounts file");
            cli::out!("             TASKMAN_NO_AUTOUPDATE=1  don't check for updates or show them");
            cli::out!("             XDG_STATE_HOME, XDG_CONFIG_HOME  move state and config\n");
            cli::out!("State:  {}", store::state_dir().display());
            cli::out!("Config: {}", store::config_path().display());
            return Ok(());
        }
        _ => {}
    }

    let exe = std::env::current_exe()?;
    // Before the alternate screen, so these stay on the terminal.
    let (_lock, saved, config) = match open_state() {
        Ok(state) => state,
        Err(e) => {
            eprintln!("taskman: {e}");
            std::process::exit(1);
        }
    };
    let mut app = app::App::new(&exe, saved, config);
    app.check_prefix_tip();
    let signals = Signals::register()?;
    let tty = Tty::open()?;
    watchdog(tty.try_clone()?, signals.hangup.clone(), signals.stops.clone())?;
    set_panic_hook();

    // Under NO_COLOR crossterm 0.29 writes each colour as `ESC[m`, a reset that also clears bold
    // and reverse (the selection); fixed on its master, not yet released. taskman drops the colours
    // itself instead (`ui::drop_colours`).
    crossterm::style::force_color_output(true);
    // Raw mode first: the keyboard query reads the terminal's answer.
    crossterm::terminal::enable_raw_mode()?;
    // Before crossterm reads the terminal, which would take the colour replies for keys.
    app.set_probe(theme::ask(tty.fd));
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    ENHANCED.store(enhanced, Ordering::Relaxed);
    let result = take_terminal(enhanced).map_err(anyhow::Error::from).and_then(|()| {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(
            links::Runs::new(std::io::stdout()),
        ))?;
        let result = run(&mut terminal, &mut app, enhanced, &signals, &tty);
        if tty.wait(Duration::ZERO) == Input::Gone {
            detach_output();
            // A draw that failed was only how the loop found out (first, on macOS): quit as on
            // prefix q.
            return Ok(());
        }
        result
    });

    release_terminal(enhanced);
    shutdown(app, &signals);
    result
}

/// Ends taskman as `prefix q` does, however the loop ended (the quit dialog, a signal, the
/// terminal gone): commands queue from now on, the state is saved, the sessions stop, and a
/// worktree still being created or removed gets up to `QUIT_WAIT` to finish, less when a second
/// signal comes. After `prefix q` the loop has already waited (`App::may_quit`), on screen.
fn shutdown(mut app: app::App, signals: &Signals) {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
    add::stopped();
    app.request_quit();
    app.stop_sessions();
    let until = Instant::now() + QUIT_WAIT;
    let mut said = false;
    while !app.may_quit() && Instant::now() < until && !signals.again() {
        // `may_quit`'s toast says what it waits for; the terminal is the shell's again.
        if !std::mem::replace(&mut said, true)
            && let Some(t) = &app.toast
        {
            let _ = writeln!(std::io::stdout(), "taskman: {}", t.msg);
        }
        std::thread::sleep(Duration::from_millis(50));
        app.drain_messages();
    }
}

/// Points stdout and stderr at /dev/null once the terminal is gone. Writing to it fails, and
/// `eprintln!` panics on that: ratatui's `Terminal` uses it when it can't show the cursor as it's
/// dropped, as does returning an error from `main`. That panic would end taskman before it stops
/// the sessions.
fn detach_output() {
    if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
        for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            // SAFETY: dup2 on descriptors this process owns; it reads or writes no memory.
            unsafe { libc::dup2(null.as_raw_fd(), fd) };
        }
    }
}

/// Takes the state folder's lock and reads the saved state and config. Fails rather than start
/// with less than was saved, since the first save would write that over the files.
fn open_state() -> Result<(store::Lock, store::Saved, store::Config), String> {
    let lock = store::lock()?;
    if let Some(dir) = store::own_config_dir().filter(|d| d.exists()) {
        let _ = store::private_dir(&dir);
    }
    // A release from before the lock doesn't take it, but it keeps the inbox's `alive` file fresh.
    if add::running_elsewhere() {
        return Err(store::already_running(&store::state_dir(), ""));
    }
    Ok((lock, store::load()?, store::load_config()?))
}

/// Restores the terminal on a panic of the main thread, which ends the TUI. Other threads (a
/// session's reader, a fetch) leave it alone, as the TUI goes on; their message would land in the
/// middle of the screen, so it goes to `panics.log` in the state folder instead.
fn set_panic_hook() {
    let main_thread = std::thread::current().id();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == main_thread {
            release_terminal(ENHANCED.load(Ordering::Relaxed));
            hook(info);
            return;
        }
        let thread = std::thread::current();
        let mut entry =
            format!("{} thread '{}' {info}\n", util::now(), thread.name().unwrap_or("<unnamed>"));
        let trace = std::backtrace::Backtrace::capture();
        if trace.status() == std::backtrace::BacktraceStatus::Captured {
            entry.push_str(&format!("{trace}\n"));
        }
        let dir = store::state_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) =
            std::fs::OpenOptions::new().create(true).append(true).open(dir.join("panics.log"))
        {
            let _ = f.write_all(entry.as_bytes());
        }
    }));
}

/// The terminal, its output sending each link once per run of cells (see `links::Runs`).
type Term = ratatui::Terminal<ratatui::backend::CrosstermBackend<links::Runs<std::io::Stdout>>>;

fn run(
    terminal: &mut Term,
    app: &mut app::App,
    enhanced: bool,
    signals: &Signals,
    tty: &Tty,
) -> anyhow::Result<()> {
    let mut last_draw =
        Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now);
    // Keys typed during the keyboard query wait in crossterm's queue, not on the terminal.
    let mut input = true;
    // Passes in a row that found input and read nothing from crossterm.
    let mut misses = 0u8;
    let mut last_size = ratatui::layout::Size::default();
    let no_color = no_color();
    loop {
        let size = terminal.size()?;
        app.tick(ratatui::layout::Rect::new(0, 0, size.width, size.height));
        // crossterm's resize events are dropped (`read_input`), so resizes are noticed here.
        if size != std::mem::replace(&mut last_size, size) {
            app.dirty.store(true, Ordering::Relaxed);
            // Read the resize before drawing at the new size, or a key typed after the redraw
            // could come with it (see `Tty`). Only while the terminal is there, as below.
            if tty.wait(Duration::ZERO) == Input::Gone {
                return Ok(());
            }
            read_input(app)?;
        }

        // Redraw on output, input, or at 10fps for spinners and clocks.
        if app.dirty.swap(false, Ordering::Relaxed)
            || last_draw.elapsed() >= Duration::from_millis(100)
        {
            terminal.draw(|f| {
                ui::draw(f, app);
                if no_color {
                    ui::drop_colours(f.buffer_mut());
                }
            })?;
            last_draw = Instant::now();
            let mut out = std::io::stdout();
            for data in app.clipboard_out.drain(..) {
                let _ = write!(out, "\x1b]52;c;{data}\x07");
            }
            if std::mem::take(&mut app.bell) {
                let _ = write!(out, "\x07");
            }
            let _ = out.flush();
        }

        // Input crossterm read nothing from twice is held (see `Tty`): waiting on it would spin.
        let wait = if misses >= 2 { Tty::wait_held } else { Tty::wait };
        match wait(tty, Duration::from_millis(16)) {
            // The terminal is gone, as with SIGHUP: stop the sessions and end.
            Input::Gone => return Ok(()),
            Input::Ready => input = true,
            Input::Idle => misses = 0,
        }
        if std::mem::take(&mut input) {
            misses = if read_input(app)? { 0 } else { misses.saturating_add(1) };
        }
        if let Some(path) = app.edit.take() {
            if let Err(e) = edit(terminal, &path, enhanced) {
                app.error(e);
            }
            signals.interrupts.store(0, Ordering::Relaxed);
            app.dirty.store(true, Ordering::Relaxed);
        }
        if app.may_quit() || signals.received() {
            return Ok(());
        }
    }
}

/// Hands what crossterm has read to the app. Whether there was anything.
fn read_input(app: &mut app::App) -> std::io::Result<bool> {
    if !event::poll(Duration::ZERO)? {
        return Ok(false);
    }
    loop {
        match event::read()? {
            Event::Key(k) => app.on_key(keys::normalize(k)),
            Event::Paste(s) => app.on_paste(&s),
            Event::Mouse(m) => app.on_mouse(m),
            Event::Resize(..) => {}
            _ => {}
        }
        if !event::poll(Duration::ZERO)? {
            break;
        }
    }
    app.dirty.store(true, Ordering::Relaxed);
    Ok(true)
}

/// Whether NO_COLOR asks for no colours: set and not empty, as no-color.org says.
fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// Puts the terminal in the TUI's mode: raw, on the alternate screen, with mouse reports, bracketed
/// paste and, where the terminal has them, the kitty keyboard flags. At start and after the editor.
fn take_terminal(enhanced: bool) -> std::io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    if enhanced {
        execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS,
            )
        )?;
    }
    Ok(())
}

/// Undoes `take_terminal`: on quit, on a signal, on a panic, and while the editor runs.
fn release_terminal(enhanced: bool) {
    if enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    // Not `ratatui::restore`: it prints its error, which panics once the terminal is gone, and in
    // the panic hook that aborts.
    let _ = ratatui::try_restore();
}

/// Opens `path` in $VISUAL or $EDITOR (else vi), giving the editor the terminal until it exits.
/// The sessions keep running meanwhile; only their drawing waits.
fn edit(terminal: &mut Term, path: &std::path::Path, enhanced: bool) -> Result<(), String> {
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|e| !e.trim().is_empty()))
        .unwrap_or_else(|| "vi".into());
    release_terminal(enhanced);
    // Through the shell, so an editor with arguments (`code --wait`) works.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status();
    let _ = take_terminal(enhanced);
    let _ = terminal.clear();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{editor} exited with {s}; set $EDITOR to the editor you use")),
        Err(e) => Err(format!("couldn't run {editor}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{FromRawFd, OwnedFd};

    #[test]
    fn gone_needs_every_check_through_the_grace() {
        let (mut gone, t, grace) = (Gone::default(), Instant::now(), Duration::from_secs(3));
        let at = |s: u64| t + Duration::from_secs(s);
        assert!(!gone.for_at_least(true, at(0), grace));
        assert!(!gone.for_at_least(true, at(2), grace));
        // Back for one check, as when another thread read the input: it starts over.
        assert!(!gone.for_at_least(false, at(3), grace));
        assert!(!gone.for_at_least(true, at(4), grace));
        assert!(!gone.for_at_least(true, at(6), grace));
        assert!(gone.for_at_least(true, at(7), grace));
    }

    /// A pty's master and slave sides.
    fn pty() -> (OwnedFd, OwnedFd) {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty writes two descriptors into `master` and `slave`; the null name, termios
        // and window size are optional.
        let r = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(r, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: openpty succeeded, so both are open descriptors that nothing else owns.
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        // SAFETY: as above.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        // So the commands other tests run don't keep the pty open past their exec.
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            // SAFETY: sets a flag on descriptors this test owns; it reads or writes no memory.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        (master, slave)
    }

    #[test]
    fn tty_wait_sees_the_terminal_close() {
        let (master, slave) = pty();
        let tty = Tty { fd: slave.as_raw_fd(), _file: None, resized: None };
        let wait = Duration::from_millis(200);

        assert_eq!(tty.wait(wait), Input::Idle);
        // A whole line, as the pty is in canonical mode.
        let mut m = std::fs::File::from(master);
        m.write_all(b"x\n").unwrap();
        assert_eq!(tty.wait(wait), Input::Ready);
        // Read it, as crossterm would: with input waiting, macOS still says Ready after the close.
        let mut buf = [0u8; 8];
        // SAFETY: reads at most `buf.len()` bytes into `buf`.
        assert_eq!(unsafe { libc::read(slave.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) }, 2);
        assert_eq!(tty.wait(Duration::ZERO), Input::Idle);
        drop(m);
        // A child forked by another test before the line above shares the master until it execs.
        let until = Instant::now() + Duration::from_secs(5);
        while tty.wait(Duration::from_millis(10)) != Input::Gone {
            assert!(Instant::now() < until, "the terminal closed but its input didn't say so");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn tty_wait_wakes_on_a_resize() {
        let (_master, slave) = pty();
        let (resized, mut wake) = UnixStream::pair().unwrap();
        resized.set_nonblocking(true).unwrap();
        let tty = Tty { fd: slave.as_raw_fd(), _file: None, resized: Some(resized) };
        let wait = Duration::from_millis(200);

        assert_eq!(tty.wait(wait), Input::Idle);
        // Two signals before the wait: one wake.
        wake.write_all(b"\0\0").unwrap();
        assert_eq!(tty.wait(wait), Input::Ready);
        assert_eq!(tty.wait(Duration::ZERO), Input::Idle);
    }

    #[test]
    fn tty_wait_held_waits_out_input_crossterm_left() {
        let (master, slave) = pty();
        let tty = Tty { fd: slave.as_raw_fd(), _file: None, resized: None };
        let wait = Duration::from_millis(100);

        assert_eq!(tty.wait_held(Duration::ZERO), Input::Idle);
        std::fs::File::from(master.try_clone().unwrap()).write_all(b"x\n").unwrap();
        assert_eq!(tty.wait(wait), Input::Ready);
        // The input stays, so `wait` would return at once: `wait_held` waits the time out first.
        let start = Instant::now();
        assert_eq!(tty.wait_held(wait), Input::Ready);
        assert!(start.elapsed() >= wait, "returned after {:?}", start.elapsed());
    }
}
