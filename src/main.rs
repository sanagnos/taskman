// Here rather than in Cargo.toml's [lints], which would reach tests/ too: a test fails by
// panicking, and clippy.toml lets the unit tests do it.
#![warn(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(not(unix))]
compile_error!("taskman needs macOS or Linux (use WSL on Windows)");

mod add;
mod app;
mod backend;
mod background;
mod cli;
mod close;
mod coding_cli;
mod events_log;
mod git;
mod github;
mod github_issues;
mod hook;
mod jira;
mod keychain;
mod keys;
mod linear;
mod links;
mod local;
mod markdown;
mod net;
mod notify;
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
mod ticket_key;
mod token_file;
mod tracker;
mod ui;
mod uninstall;
mod untrusted;
mod update;
mod util;
mod worktree;

use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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

/// How often the watchdog looks. It acts after `SECOND_SIGNAL` at the soonest, and this late at
/// most past it.
const WATCH_EVERY: Duration = Duration::from_millis(500);

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
///
/// `wait` also wakes for the sessions' output and the threads' replies (`session::Wake`), and for
/// the signals that quit, so the loop can wait long between frames without the selected session's
/// echo, a reply or a quit waiting with it.
struct Tty {
    fd: RawFd,
    /// The file `fd` belongs to when it isn't stdin: `/dev/tty`, or the watchdog's copy.
    _file: Option<std::fs::File>,
    /// Readable after a SIGWINCH. The watchdog's copy has none: it only looks for the terminal
    /// going.
    resized: Option<UnixStream>,
    /// Readable after a session's output, a thread's reply or a signal that quits. None in the
    /// watchdog's copy too.
    woken: Option<UnixStream>,
}

#[derive(Debug, PartialEq)]
enum Input {
    Ready,
    Idle,
    /// A session wrote output, a thread replied or a signal came; the terminal may still hold
    /// input (see `wait_held`).
    Woken,
    Gone,
}

impl Tty {
    /// The file crossterm picks: stdin if it's a terminal, else `/dev/tty`. With what wakes its
    /// `wait`, for the sessions and the App's threads.
    fn open() -> std::io::Result<(Self, session::Wake)> {
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
        let (woken, wake) = UnixStream::pair()?;
        woken.set_nonblocking(true)?;
        wake.set_nonblocking(true)?;
        // After `Signals::register`'s handlers, which count the signal before this one wakes the
        // loop to look: a signal needn't come to the loop's thread, whose wait it would end.
        for sig in [SIGTERM, SIGHUP, SIGINT] {
            signal_hook::low_level::pipe::register(sig, wake.try_clone()?)?;
        }
        let tty = Self { fd, _file: file, resized: Some(resized), woken: Some(woken) };
        Ok((tty, session::Wake::new(wake)))
    }

    fn try_clone(&self) -> std::io::Result<Self> {
        // SAFETY: `fd` is stdin or `_file`'s, open while `self` is, and the borrow ends with the
        // clone.
        let f =
            std::fs::File::from(unsafe { BorrowedFd::borrow_raw(self.fd) }.try_clone_to_owned()?);
        Ok(Self { fd: f.as_raw_fd(), _file: Some(f), resized: None, woken: None })
    }

    /// Waits up to `timeout` for input or a resize, which crossterm reads, or a session's output.
    /// Through `filedescriptor::poll`, which uses select on macOS, where poll doesn't work on
    /// terminals.
    fn wait(&self, timeout: Duration) -> Input {
        self.poll(timeout, false)
    }

    /// As `wait`, for input crossterm won't read until more comes (see `Tty`): that input keeps
    /// the terminal readable, so only a resize, a session's output or the terminal going ends the
    /// wait early. Ready while the input is still there, for `run` to try crossterm again.
    fn wait_held(&self, timeout: Duration) -> Input {
        self.poll(timeout, true)
    }

    fn poll(&self, timeout: Duration, held: bool) -> Input {
        // Without POLLIN, poll still reports a hangup, and select (macOS) an exception.
        let events = if held { 0 } else { libc::POLLIN };
        // Only the pipes there are: select (macOS) refuses a negative descriptor.
        let mut fds = [libc::pollfd { fd: self.fd, events, revents: 0 }; 3];
        let mut len = 1;
        for pipe in [&self.resized, &self.woken].into_iter().flatten() {
            fds[len] = libc::pollfd { fd: pipe.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            len += 1;
        }
        let fds = &mut fds[..len];
        match filedescriptor::poll(fds, Some(whole_millis(timeout))) {
            // An error is a signal arriving; the caller checks for those.
            Ok(0) | Err(_) if !held => return Input::Idle,
            _ => {}
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Input::Gone;
        }
        let readable = |pipe: &UnixStream| {
            fds.iter().any(|f| f.fd == pipe.as_raw_fd() && f.revents & libc::POLLIN != 0)
        };
        if let Some(r) = &self.resized
            && readable(r)
        {
            drain(r);
            return Input::Ready;
        }
        if let Some(w) = &self.woken
            && readable(w)
        {
            drain(w);
            // Input that came too is read as usual; held input is still held.
            if held || fds[0].revents == 0 {
                return Input::Woken;
            }
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

/// `timeout` rounded up to whole milliseconds. poll (Linux) takes milliseconds and
/// `filedescriptor` truncates to them, so a wait under one would return at once: the loop would
/// spend the last millisecond before each frame passing again and again. Zero stays zero.
fn whole_millis(timeout: Duration) -> Duration {
    let ms = timeout.as_nanos().div_ceil(1_000_000);
    Duration::from_millis(ms.try_into().unwrap_or(u64::MAX))
}

/// Empties a wake-up pipe. Writes collate, so one read usually takes them all.
fn drain(mut r: &UnixStream) {
    let mut buf = [0u8; 64];
    while matches!(r.read(&mut buf), Ok(n) if n > 0) {}
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
            std::thread::sleep(WATCH_EVERY);
            let now = Instant::now();
            // A SIGHUP that isn't the terminal closing ends the loop too.
            let left = hangup.load(Ordering::Relaxed) || tty.wait(Duration::ZERO) == Input::Gone;
            let grace =
                if SHUTTING_DOWN.load(Ordering::Relaxed) { GRACE + QUIT_WAIT } else { GRACE };
            let second = stops.load(Ordering::Relaxed) >= 2;
            if gone.for_at_least(left, now, grace) || again.for_at_least(second, now, SECOND_SIGNAL)
            {
                add::stopped();
                proc::stop_all();
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
    let command: Option<fn(&[String]) -> i32> = match args.get(1).map(String::as_str) {
        Some("status") => Some(status::run),
        Some("add") => Some(add::run),
        Some("close") => Some(close::run),
        Some("move") => Some(local::run),
        Some("resume") => Some(resume::run),
        Some("update") => Some(update::run),
        Some("uninstall") => Some(uninstall::run),
        _ => None,
    };
    if let Some(run) = command {
        // Before any helper starts: a ctrl+c stops them with the command.
        proc::stop_on_signals();
        std::process::exit(proc::unless_interrupted(run(&args[2..])));
    }
    match args.get(1).map(String::as_str) {
        Some("hook") => {
            hook::run(args.get(2).map(String::as_str).unwrap_or("idle"));
            return Ok(());
        }
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
        Some("-h" | "--help" | "help") => {
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
            cli::out!(
                "       taskman uninstall    remove the program, and if you say so your data and tokens"
            );
            cli::out!("                            (--yes, --purge; --help for more)");
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
        // A typo would otherwise start the TUI, or say another one is running, and exit 0.
        Some(other) => {
            eprintln!("{}", unknown_command(other));
            std::process::exit(2);
        }
        None => {}
    }

    let exe = std::env::current_exe()?;
    // Before the alternate screen, so these stay on the terminal.
    let (running, saved, config, before) = match open_state() {
        Ok(state) => state,
        Err(e) => {
            eprintln!("taskman: {e}");
            std::process::exit(1);
        }
    };
    let signals = Signals::register()?;
    let (tty, wake) = match Tty::open() {
        Ok(tty) => tty,
        Err(e) => {
            drop(running);
            eprintln!("{}", no_terminal(&e));
            std::process::exit(1);
        }
    };
    let mut app = app::App::new(&exe, saved, config, wake);
    if before == add::Before::Crashed {
        app.warn("taskman didn't quit cleanly last time: it was killed or crashed");
    }
    app.check_prefix_tip();
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
        let mut terminal =
            ratatui::Terminal::new(backend::Backend::new(links::Runs::new(std::io::stdout())))?;
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
/// terminal gone): commands queue from now on, the state is saved, the sessions stop, a worktree
/// still being created or removed gets up to `QUIT_WAIT` to finish, less when a second signal
/// comes, and the helpers still running (gh, git, token commands) are stopped. After `prefix q`
/// the loop has already waited (`App::may_quit`), on screen.
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
    proc::stop_all();
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

/// The state folder's lock, held while the TUI runs, and the inbox's heartbeat that says so,
/// removed before the lock goes however `main` ends (an error, say), so that commands queue and
/// the next taskman doesn't take it for one that crashed; but kept after a panic, so that it does.
struct Running {
    _lock: store::Lock,
}

impl Drop for Running {
    fn drop(&mut self) {
        add::ending();
    }
}

/// Takes the state folder's lock and reads the saved state and config, with what the taskman
/// before this one left. Fails rather than start with less than was saved, since the first save
/// would write that over the files.
fn open_state() -> Result<(Running, store::Saved, store::Config, add::Before), String> {
    let lock = store::lock()?;
    if let Some(dir) = store::own_config_dir().filter(|d| d.exists()) {
        let _ = store::private_dir(&dir);
    }
    // A release from before the lock doesn't take it, and a lock file removed while taskman ran
    // was made again by this one, but either keeps the inbox's `alive` file fresh.
    let before = add::before();
    if let add::Before::Running(pid, host) = before {
        // Read from a folder another machine writes too.
        let host = host.map(|h| untrusted::line(&h));
        let who = match (pid, host) {
            (Some(p), Some(h)) => format!(" (pid {p} on {h})"),
            (None, Some(h)) => format!(" (on {h})"),
            (Some(p), None) => format!(" (pid {p})"),
            (None, None) => String::new(),
        };
        return Err(store::already_running(&store::state_dir(), &who));
    }
    let (saved, config) = (store::load()?, store::load_config()?);
    // Commands count taskman as running from here, before its first frame (`add::tui_running`).
    add::heartbeat();
    Ok((Running { _lock: lock }, saved, config, before))
}

/// Why `Tty::open` failed, and what to do, on stderr. With no terminal at all (`ssh host taskman` without
/// -t, a cron job), opening /dev/tty fails with ENXIO, or ENOTTY on some systems.
fn no_terminal(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::ENXIO | libc::ENOTTY) => {
            "taskman needs a terminal: run it in one (ssh -t for a remote machine)".into()
        }
        _ => format!("taskman: couldn't open the terminal: {e}"),
    }
}

/// What a first argument that isn't a command gets, on stderr.
fn unknown_command(arg: &str) -> String {
    // Quoted and escaped, so what was typed shows as typed, a control character too.
    format!("taskman: unknown command {arg:?}; taskman --help lists them")
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

/// The closest frames follow each other: for a change on screen or the selected session's output.
const FRAME: Duration = Duration::from_millis(16);
/// Frames while a spinner shows: its pace.
const SPIN: Duration = Duration::from_millis(100);
/// Frames otherwise, for the ages and anything that changed without saying so: with the first tick
/// after it (see `wait_for`).
const TICK: Duration = Duration::from_secs(1);

/// What calls for the next frame.
#[derive(Clone, Copy, Default)]
struct Due {
    /// The terminal changed size this pass.
    resized: bool,
    /// Something on screen changed (`App::dirty`: a key, a reply, a toast).
    changed: bool,
    /// The selected session wrote output (`App::selected_fresh`).
    output: bool,
    /// The last frame showed a spinner (`ui::animated`).
    animated: bool,
}

/// How long after the last frame the next is due: at once for a resize, which has drawn nothing
/// at the new size yet and whose user is waiting on it; soon for a change or the selected
/// session's output; at the spinner's pace while the last frame showed one; else once a second.
/// Other sessions' output is no reason: their cards change with the status poll, which says so.
fn frame_due(due: Due) -> Duration {
    if due.resized {
        Duration::ZERO
    } else if due.changed || due.output {
        FRAME
    } else if due.animated {
        SPIN
    } else {
        TICK
    }
}

/// How long the loop waits after a pass, unless input, a resize, a session's output or a thread's
/// reply ends the wait sooner: until the next frame is due (`due`, `since` the last), or `tick` is
/// (`App::tick_in`), whichever is first. The once-a-second frame (`TICK`) doesn't wake the loop by
/// itself but comes with the tick after it, up to a status poll (300 ms) late, so idle the loop
/// wakes for the status poll alone.
fn wait_for(due: Duration, since: Duration, tick_in: Duration) -> Duration {
    if due >= TICK { tick_in } else { due.saturating_sub(since).min(tick_in) }
}

/// The terminal, clearing what an emoji covers before drawing it (see `backend`), its output
/// sending each link once per run of cells (see `links::Runs`).
type Term = ratatui::Terminal<backend::Backend<links::Runs<std::io::Stdout>>>;

fn run(
    terminal: &mut Term,
    app: &mut app::App,
    enhanced: bool,
    signals: &Signals,
    tty: &Tty,
) -> anyhow::Result<()> {
    let mut last_draw = Instant::now().checked_sub(TICK).unwrap_or_else(Instant::now);
    // Whether the last frame showed a spinner.
    let mut animated = false;
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
        let resized = size != std::mem::replace(&mut last_size, size);
        if resized {
            app.dirty.store(true, Ordering::Relaxed);
            // Read the resize before drawing at the new size, or a key typed after the redraw
            // could come with it (see `Tty`). Only while the terminal is there, as below.
            if tty.wait(Duration::ZERO) == Input::Gone {
                return Ok(());
            }
            read_input(app)?;
        }

        let due = |app: &app::App, resized, animated| {
            frame_due(Due {
                resized,
                changed: app.dirty.load(Ordering::Relaxed),
                output: app.selected_fresh(),
                animated,
            })
        };
        if last_draw.elapsed() >= due(app, resized, animated) {
            app.take_redraw();
            terminal.draw(|f| {
                ui::draw(f, app);
                if no_color {
                    ui::drop_colours(f.buffer_mut());
                }
            })?;
            animated = ui::animated();
            last_draw = Instant::now();
        }
        // Not only with a frame: these come from the sessions and the poll, which needn't draw.
        if !app.clipboard_out.is_empty() || app.bell || !app.notify_out.is_empty() {
            let mut out = std::io::stdout();
            // One clipboard: only the last write of the pass would stay on it.
            if let Some(w) = app.clipboard_to_forward() {
                let _ = write!(out, "\x1b]52;{};{}\x07", w.target, w.data);
            }
            if std::mem::take(&mut app.bell) {
                let _ = write!(out, "\x07");
            }
            for escape in app.notify_out.drain(..) {
                let _ = write!(out, "{escape}");
            }
            let _ = out.flush();
        }

        let timeout = wait_for(due(app, false, animated), last_draw.elapsed(), app.tick_in());
        // Input crossterm read nothing from twice is held (see `Tty`): waiting on it would spin.
        // Only new input frees it, and that doesn't end the wait, so try again soon.
        let wait = if misses >= 2 { Tty::wait_held } else { Tty::wait };
        let timeout = if misses >= 2 { timeout.min(FRAME) } else { timeout };
        match wait(tty, timeout) {
            // The terminal is gone, as with SIGHUP: stop the sessions and end.
            Input::Gone => return Ok(()),
            Input::Ready => input = true,
            Input::Idle => misses = 0,
            // The next pass draws it, if it's the selected session's.
            Input::Woken => {}
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
        on_event(app, event::read()?);
        if !event::poll(Duration::ZERO)? {
            break;
        }
    }
    Ok(true)
}

/// Hands one event to the app, asking for a frame if it may have changed the screen.
fn on_event(app: &mut app::App, ev: Event) {
    let redraw = redraws(&ev);
    match ev {
        Event::Key(k) => app.on_key(keys::normalize(k)),
        Event::Paste(s) => app.on_paste(&s),
        Event::Mouse(m) => app.on_mouse(m),
        Event::FocusGained => app.focused = Some(true),
        Event::FocusLost => app.focused = Some(false),
        Event::Resize(..) => {}
    }
    if redraw {
        app.dirty.store(true, Ordering::Relaxed);
    }
}

/// Whether an event may change the screen. The pointer moving with no button held doesn't: the
/// mouse mode reports every move (a real mouse sends 60-120 a second), and nothing handles them.
fn redraws(ev: &Event) -> bool {
    !matches!(ev, Event::Mouse(m) if m.kind == MouseEventKind::Moved)
}

/// Whether NO_COLOR asks for no colours: set and not empty, as no-color.org says.
fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// Puts the terminal in the TUI's mode: raw, on the alternate screen, with mouse reports, bracketed
/// paste, focus reports (for `notify`) and, where the terminal has them, the kitty keyboard flags. At start and after the editor.
fn take_terminal(enhanced: bool) -> std::io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange
    )?;
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
    let _ =
        execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste, DisableFocusChange);
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

    #[test]
    fn no_terminal_says_to_run_it_in_one() {
        for errno in [libc::ENXIO, libc::ENOTTY] {
            let e = std::io::Error::from_raw_os_error(errno);
            assert_eq!(
                no_terminal(&e),
                "taskman needs a terminal: run it in one (ssh -t for a remote machine)"
            );
        }
        let e = std::io::Error::from_raw_os_error(libc::EACCES);
        let said = no_terminal(&e);
        assert!(said.starts_with("taskman: couldn't open the terminal: "), "{said}");
    }

    #[test]
    fn an_unknown_command_is_quoted_as_typed() {
        assert_eq!(
            unknown_command("stauts"),
            r#"taskman: unknown command "stauts"; taskman --help lists them"#
        );
        // A control character can't reach the terminal as one.
        assert_eq!(
            unknown_command("a\x1b[2Jb"),
            r#"taskman: unknown command "a\u{1b}[2Jb"; taskman --help lists them"#
        );
    }

    #[test]
    fn a_frame_is_due_at_once_for_a_resize_and_soon_only_for_a_change_or_the_selected_output() {
        let none = Due::default();
        // A resize draws at once, however recent the last frame.
        assert_eq!(frame_due(Due { resized: true, ..none }), Duration::ZERO);
        let all = Due { changed: true, output: true, animated: true, ..none };
        assert_eq!(frame_due(Due { resized: true, ..all }), Duration::ZERO);
        // A key, a reply or a toast; or the selected session's output (`App::selected_fresh`,
        // which leaves the other sessions' out).
        assert_eq!(frame_due(Due { changed: true, ..none }), FRAME);
        assert_eq!(frame_due(Due { output: true, ..none }), FRAME);
        assert_eq!(frame_due(all), FRAME);
        // A spinner on screen.
        assert_eq!(frame_due(Due { animated: true, ..none }), SPIN);
        // Nothing: the clock, for the ages.
        assert_eq!(frame_due(none), TICK);
    }

    #[test]
    fn the_loop_waits_for_the_next_frame_or_tick_and_the_clock_for_a_tick() {
        let ms = Duration::from_millis;
        // A change, 6 ms after a frame: the rest of the 16 ms, as the tick comes later.
        assert_eq!(wait_for(FRAME, ms(6), ms(200)), ms(10));
        assert_eq!(wait_for(FRAME, ms(40), ms(200)), Duration::ZERO);
        // The tick first: the status poll, or a drag's next step at an edge.
        assert_eq!(wait_for(SPIN, ms(20), ms(30)), ms(30));
        assert_eq!(wait_for(FRAME, ms(0), Duration::ZERO), Duration::ZERO);
        // The clock's frame doesn't wake the loop: it comes with the next tick.
        assert_eq!(wait_for(TICK, ms(900), ms(300)), ms(300));
        assert_eq!(wait_for(TICK, ms(100), ms(300)), ms(300));
    }

    #[test]
    fn a_reply_from_a_thread_ends_the_loops_wait() {
        let (_master, slave) = pty();
        let (woken, wake) = UnixStream::pair().unwrap();
        woken.set_nonblocking(true).unwrap();
        wake.set_nonblocking(true).unwrap();
        let tty = Tty { fd: slave.as_raw_fd(), _file: None, resized: None, woken: Some(woken) };
        let mut app = app::test_support::acme_app();
        app::test_support::wake_with(&mut app, session::Wake::new(wake));
        assert_eq!(tty.wait(Duration::ZERO), Input::Idle);

        // Copying answers from a thread (`Msg::Copied`), here at once: the tests' clipboard is
        // OSC 52.
        app::test_support::copy(&mut app, "ENG-52");
        let start = Instant::now();
        assert_eq!(tty.wait(Duration::from_secs(10)), Input::Woken);
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        app.drain_messages();
        assert_eq!(app.clipboard_out.len(), 1, "the reply was applied");
        // The pipe was emptied: the next wait waits.
        assert_eq!(tty.wait(Duration::from_millis(50)), Input::Idle);
    }

    #[test]
    fn a_wait_rounds_up_to_whole_milliseconds() {
        assert_eq!(whole_millis(Duration::ZERO), Duration::ZERO);
        // Under a microsecond too, which rounding from microseconds would leave at zero.
        assert_eq!(whole_millis(Duration::from_nanos(1)), Duration::from_millis(1));
        assert_eq!(whole_millis(Duration::from_micros(900)), Duration::from_millis(1));
        assert_eq!(whole_millis(Duration::from_millis(16)), Duration::from_millis(16));
        assert_eq!(whole_millis(Duration::from_nanos(16_000_001)), Duration::from_millis(17));
        assert!(whole_millis(Duration::MAX) >= Duration::from_secs(u64::MAX / 1000));
    }

    #[test]
    fn a_bare_mouse_move_draws_nothing_and_a_drag_does() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent};
        let mut app = app::test_support::acme_app();
        let mouse = |kind| {
            Event::Mouse(MouseEvent { kind, column: 40, row: 10, modifiers: KeyModifiers::NONE })
        };
        app.take_redraw();
        on_event(&mut app, mouse(MouseEventKind::Moved));
        assert!(!app.dirty.load(Ordering::Relaxed));
        on_event(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left)));
        assert!(app.dirty.load(Ordering::Relaxed));
    }
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
        let tty = Tty { fd: slave.as_raw_fd(), _file: None, resized: None, woken: None };
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
        let tty = Tty { fd: slave.as_raw_fd(), _file: None, resized: Some(resized), woken: None };
        let wait = Duration::from_millis(200);

        assert_eq!(tty.wait(wait), Input::Idle);
        // Two signals before the wait: one wake.
        wake.write_all(b"\0\0").unwrap();
        assert_eq!(tty.wait(wait), Input::Ready);
        assert_eq!(tty.wait(Duration::ZERO), Input::Idle);
    }

    #[test]
    fn tty_wait_wakes_on_a_sessions_output() {
        let (master, slave) = pty();
        let (woken, mut wake) = UnixStream::pair().unwrap();
        woken.set_nonblocking(true).unwrap();
        let tty = Tty { fd: slave.as_raw_fd(), _file: None, resized: None, woken: Some(woken) };
        let wait = Duration::from_millis(200);

        assert_eq!(tty.wait(wait), Input::Idle);
        // Two sessions' output before the wait: one wake.
        wake.write_all(b"\0\0").unwrap();
        assert_eq!(tty.wait(wait), Input::Woken);
        assert_eq!(tty.wait(Duration::ZERO), Input::Idle);
        // Input that comes with it is still read.
        let mut m = std::fs::File::from(master);
        m.write_all(b"x\n").unwrap();
        wake.write_all(b"\0").unwrap();
        assert_eq!(tty.wait(wait), Input::Ready);
        // Held, the input doesn't end the wait, but the output does.
        wake.write_all(b"\0").unwrap();
        let start = Instant::now();
        assert_eq!(tty.wait_held(Duration::from_secs(5)), Input::Woken);
        assert!(start.elapsed() < Duration::from_secs(1), "returned after {:?}", start.elapsed());
    }

    #[test]
    fn tty_wait_waits_out_a_timeout_under_a_millisecond() {
        let (_master, slave) = pty();
        let tty = Tty { fd: slave.as_raw_fd(), _file: None, resized: None, woken: None };
        let wait = Duration::from_micros(900);
        let start = Instant::now();
        let mut passes = 0;
        while start.elapsed() < wait {
            assert_eq!(tty.wait(wait.saturating_sub(start.elapsed())), Input::Idle);
            passes += 1;
        }
        assert!(passes <= 2, "{passes} waits in {wait:?}");
    }

    #[test]
    fn tty_wait_held_waits_out_input_crossterm_left() {
        let (master, slave) = pty();
        let tty = Tty { fd: slave.as_raw_fd(), _file: None, resized: None, woken: None };
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
