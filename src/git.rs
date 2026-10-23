//! Running git: every call goes through `proc::run`, so a git stuck on a lock or an unreachable
//! remote gives up at its caller's limit.

use crate::proc;
use std::io;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

/// `git -C dir`, for a call that needs more than [`run`] gives it (an environment, stdin, a lock
/// shared with it).
pub fn command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    // Without the locks git takes only to save work for later, as `git status` writing back the
    // index it refreshed: one stopped at quit, or at its limit, can't leave a `.lock` behind that
    // fails the next commit there, and one still running can't fail the session's commit meanwhile.
    cmd.arg("-C").arg(dir).env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}

/// `git -C dir args`, given up after `limit`.
pub fn run(dir: &Path, args: &[&str], limit: Duration) -> io::Result<Output> {
    proc::run(command(dir).args(args), limit)
}
