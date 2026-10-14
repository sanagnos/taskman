//! Sleeping sessions in the real binary: an unselected idle session is stopped after the time set
//! and starts again, as a new process, when it's selected; what the CLI leaves running in the
//! background that its hooks don't report (a stub CLI's child, found through `ps`) keeps it
//! awake, and its own servers, there from the start, don't.

mod common;

use common::{Tui, scratch_dir, script, taskman};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Seconds a session goes unselected before it sleeps: a session settles after a quarter of that.
const AFTER: u64 = 5;

/// A CLI with a server of its own, as its MCP servers are, and a background task started when it's
/// typed `bg`, as a `run_in_background` command is. It notes its pid in `<KEY>.pid` and the task's
/// in `<KEY>.bg.pid` (so the test's cleanup finds it), and ends them with itself.
fn cli(dir: &Path) -> PathBuf {
    let text = r#"#!/bin/sh
d='DIR'; t="$TASKMAN_TICKET"
trap 'kill $(jobs -p) 2>/dev/null; exit 0' HUP TERM
sleep 1000 &
echo $$ > "$d/$t.pid"
echo "stub ready for $t"
while IFS= read -r line; do
  if [ "$line" = bg ]; then
    sleep 1000 &
    echo $! > "$d/$t.bg.pid"
  fi
done
"#;
    script(dir, "claude", &text.replace("DIR", &dir.display().to_string()))
}

/// A `ps` that notes each run once it has finished (`ps.log`), so the test knows when the TUI has
/// looked at the sessions' process trees.
fn ps(dir: &Path) -> PathBuf {
    let bin = dir.join("psbin");
    std::fs::create_dir_all(&bin).unwrap();
    let real = ["/bin/ps", "/usr/bin/ps"].into_iter().find(|p| Path::new(p).exists()).unwrap();
    let text = format!(
        "#!/bin/sh\n{real} \"$@\"\nrc=$?\necho ran >> '{}/ps.log'\nexit $rc\n",
        dir.display()
    );
    script(&bin, "ps", &text);
    bin
}

fn scans(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("ps.log")).map_or(0, |t| t.lines().count())
}

/// Tickets ENG-1 and ENG-2, which sleep after `AFTER` seconds. Returns the running TUI.
fn start(name: &'static str, dir: &Path) -> Tui {
    common::project(dir, &["ENG-1", "ENG-2"]);
    std::fs::write(dir.join("config.json"), format!(r#"{{"sleep_after":{AFTER}}}"#)).unwrap();
    let path = format!("{}:{}", ps(dir).display(), std::env::var("PATH").unwrap_or_default());
    let mut cmd = taskman(dir);
    cmd.env("TASKMAN_CLAUDE", cli(dir)).env("PATH", path);
    Tui::start(name, dir, cmd, Duration::from_secs(120))
}

fn pid(dir: &Path, file: &str) -> Option<i32> {
    std::fs::read_to_string(dir.join(file)).ok()?.trim().parse().ok()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Whether `ENG-2`'s session is marked asleep for `taskman status`.
fn marked(dir: &Path) -> bool {
    let folders = std::fs::read_dir(dir.join("state/taskman/status")).into_iter().flatten();
    folders.flatten().any(|d| d.path().join("ENG-2.asleep").exists())
}

/// Starts both sessions, with ENG-1 in front, once each has had its process tree taken as its own.
/// Returns ENG-2's pid.
fn both_settled(tui: &mut Tui, dir: &Path) -> i32 {
    tui.wait_text("stub ready for ENG-1");
    tui.send(b"\x012");
    tui.wait_text("stub ready for ENG-2");
    let two = pid(dir, "ENG-2.pid").unwrap();
    tui.wait_until("both process trees weren't taken", || scans(dir) >= 2);
    // The answer is applied on the TUI's next tick, a moment after `ps` ended.
    std::thread::sleep(Duration::from_millis(500));
    two
}

fn quit(tui: &mut Tui) {
    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

#[test]
fn an_idle_session_sleeps_when_unselected_and_wakes_when_selected() {
    let dir = scratch_dir("sleep-idle");
    let mut tui = start("sleep-idle", &dir);
    let two = both_settled(&mut tui, &dir);
    let one = pid(&dir, "ENG-1.pid").unwrap();
    // ENG-1 in front: ENG-2 is unselected from now.
    tui.send(b"\x011");
    tui.wait_until("ENG-2 didn't go to sleep", || !alive(two) && marked(&dir));
    tui.wait_text("☾");
    assert!(alive(one), "the session in front was stopped");

    // Selecting it starts the conversation again, as a new process.
    tui.send(b"\x012");
    tui.wait_until("ENG-2 didn't wake", || {
        pid(&dir, "ENG-2.pid").is_some_and(|p| p != two && alive(p))
    });
    tui.wait_until("it still shows as asleep", || !marked(&dir));
    quit(&mut tui);
}

#[test]
fn work_in_the_background_keeps_a_session_awake() {
    let dir = scratch_dir("sleep-busy");
    let mut tui = start("sleep-busy", &dir);
    let two = both_settled(&mut tui, &dir);
    // A task started after the session settled, as a dev server a prompt asked for is.
    tui.send(b"bg\r");
    tui.wait_until("the stub didn't start its task", || pid(&dir, "ENG-2.bg.pid").is_some());
    let task = pid(&dir, "ENG-2.bg.pid").unwrap();
    tui.send(b"\x011");

    // Well past the time: still running, and not marked.
    let until = Instant::now() + Duration::from_secs(AFTER * 5 / 2);
    while Instant::now() < until {
        assert!(alive(two) && !marked(&dir), "slept with a task running:\n{}", tui.text());
        std::thread::sleep(Duration::from_millis(100));
    }

    // The task ends: the next look finds only its own server, and it sleeps.
    // SAFETY: stops a process this test started.
    unsafe { libc::kill(task, libc::SIGKILL) };
    tui.wait_until("ENG-2 didn't go to sleep once its task ended", || !alive(two) && marked(&dir));
    quit(&mut tui);
}
