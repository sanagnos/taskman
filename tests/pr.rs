//! The PR panel, against the binary in a pty with a gh that answers from the recorded fixtures: it
//! stays hidden while gh is logged out, and `ctrl+a r` after logging in brings it up with the
//! ticket's PR. The repository's remote is on GitHub, but nothing leaves the machine: gh is the
//! stub, and git may use local repositories only.

mod common;

use common::{Tui, project, scratch_dir, script, sh, stub_tools, stubbed};
use std::path::Path;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(60);

/// A gh that's logged out until `dir/gh-logged-in` exists, then answers the PR lookup for ENG-52
/// from tests/fixtures/github. It notes each call in `dir/gh.calls`, and any it has no answer for
/// in `dir/gh.unexpected` too.
fn stub_gh(dir: &Path) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/github");
    let text = r#"#!/bin/sh
echo "$*" | head -1 >> 'DIR/gh.calls'
if [ ! -e 'DIR/gh-logged-in' ]; then
  case "$*" in
    "auth token"*) echo 'no oauth token found for github.com' >&2;;
    *) printf 'To get started with GitHub CLI, please run:  gh auth login\n' >&2;;
  esac
  exit 4
fi
case "$*" in
  "auth token"*) echo gho_mock_token;;
  "pr list"*--head*) cat 'FIXTURES/pr-list-branch.json';;
  "pr list"*--search*) cat 'FIXTURES/pr-list.json';;
  "pr view 45 "*) cat 'FIXTURES/pr-view.json';;
  "api graphql"*reviewThreads*) cat 'FIXTURES/review-threads.json';;
  *) echo "$*" | head -1 >> 'DIR/gh.unexpected'; echo "gh stub: no answer for $*" >&2; exit 1;;
esac
"#;
    let text = text
        .replace("DIR", &dir.display().to_string())
        .replace("FIXTURES", &fixtures.display().to_string());
    script(&stub_tools(dir), "gh", &text);
}

#[test]
fn the_pr_panel_shows_once_gh_is_logged_in() {
    let dir = scratch_dir("pr");
    let repo = project(&dir, &["ENG-52"]);
    sh(&repo, "git init -q && git remote add origin https://github.com/acme/web.git");
    stub_gh(&dir);
    let mut cmd = stubbed(&dir);
    // Any git command that would reach the network fails instead.
    cmd.env("GIT_ALLOW_PROTOCOL", "file");
    let mut tui = Tui::start("the_pr_panel_shows_once_gh_is_logged_in", &dir, cmd, LIMIT);
    tui.wait_text("stub ready for ENG-52");

    // gh was checked before the first frame: logged out, so no panel. Turning the panel off and
    // on again says why.
    tui.step("panel hidden while logged out");
    tui.send(b"\x01g\x01g");
    tui.wait_text("No PR panel: gh isn't logged in; run gh auth login");
    assert!(!tui.text().contains("GitHub PR"), "a PR panel while logged out:\n{}", tui.text());

    tui.step("panel after logging in and ctrl+a r");
    std::fs::write(dir.join("gh-logged-in"), "").unwrap();
    tui.send(b"\x01r");
    tui.wait_text("GitHub PR #45 open");
    tui.wait_text("Sign in with a magic link");
    tui.wait_text("src/auth/link.rs:42");
    let calls = std::fs::read_to_string(dir.join("gh.calls")).unwrap();
    assert!(calls.lines().any(|l| l.starts_with("pr view 45 ")), "{calls}");
    let unexpected = std::fs::read_to_string(dir.join("gh.unexpected")).unwrap_or_default();
    assert!(unexpected.is_empty(), "gh calls the stub has no answer for:\n{unexpected}");

    tui.send(b"\x01q");
    tui.wait_text("Quit taskman?");
    tui.send(b"\r");
    assert!(tui.wait_exit().success());
}
