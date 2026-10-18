//! A key typed after the terminal is resized arrives on its own, without waiting for the next key,
//! and taskman doesn't spin while nothing is typed. crossterm 0.29 reads a resize and the input
//! that comes with it in one batch, reports the resize and drops the input, so `Tty::wait` (in
//! src/main.rs) has it read each resize as it comes.

mod common;

use common::{Tui, find, project, scratch_dir, stubbed};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// How much processor time taskman may use in `IDLE` doing nothing: a fraction of a core, where a
/// loop that spins takes most of one.
const BUSY: Duration = Duration::from_millis(500);
const IDLE: Duration = Duration::from_secs(2);

#[test]
fn a_key_after_a_resize_arrives_on_its_own() {
    let dir = scratch_dir("resize");
    project(&dir, &["ENG-1"]);
    let mut tui = Tui::start("a_key_after_a_resize_arrives_on_its_own", &dir, stubbed(&dir), LIMIT);
    let (col, row) = tui.wait_text("stub ready for ENG-1");

    resize(&tui, 30, 120);
    // One byte, which the session's terminal echoes below its line.
    tui.send(b"z");
    tui.wait_for("the z typed after the resize", |s| {
        (s.cell(row + 1, col)?.contents() == "z").then_some(())
    });
    let before = cpu_time(&tui);
    std::thread::sleep(IDLE);
    let used = cpu_time(&tui).saturating_sub(before);
    assert!(used < BUSY, "taskman used {used:?} of processor time in {IDLE:?} idle");

    // A lone prefix key opens the menu.
    resize(&tui, 40, 160);
    tui.send(b"\x01");
    tui.wait_text("press a command key");
    tui.send(b"q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}

/// Resizes the terminal and waits for taskman to draw at the new size: the hints on the last row.
fn resize(tui: &Tui, rows: u16, cols: u16) {
    tui.resize(rows, cols);
    tui.wait_for("the hints on the resized terminal's last row", |s| {
        find(s, "menu").filter(|&(_, r)| r == rows - 1)
    });
}

/// The processor time taskman has used.
fn cpu_time(tui: &Tui) -> Duration {
    let pid = tui.child.id();
    #[cfg(target_os = "linux")]
    {
        // utime and stime, in clock ticks: fields 14 and 15, counted after the command's
        // parenthesis since the command may hold spaces.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..].split(' ').collect();
        let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
        // SAFETY: sysconf only reads a configuration value.
        let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).unwrap();
        Duration::from_millis(ticks * 1000 / hz)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS's ps prints it to the hundredth: `0:01.25`, minutes then seconds.
        let out = std::process::Command::new("ps")
            .args(["-o", "time=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let time = String::from_utf8(out.stdout).unwrap();
        let secs =
            time.trim().split(':').fold(0.0, |acc, part| acc * 60.0 + part.parse::<f64>().unwrap());
        Duration::from_secs_f64(secs)
    }
}
