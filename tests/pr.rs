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
/// in `dir/gh.unexpected` too. While `dir/gh-hold` exists, `gh auth token` waits until the test
/// writes `dir/gh-release`, and touches `dir/gh-answered` as it answers.
fn stub_gh(dir: &Path) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/github");
    let text = r#"#!/bin/sh
echo "$*" | head -1 >> 'DIR/gh.calls'
case "$*" in
  "auth token"*) if [ -e 'DIR/gh-hold' ]; then
      i=0; while [ ! -e 'DIR/gh-release' ] && [ $i -lt 1200 ]; do sleep 0.05; i=$((i+1)); done
      touch 'DIR/gh-answered'
    fi;;
esac
if [ ! -e 'DIR/gh-logged-in' ]; then
  case "$*" in
    "auth token"*) echo 'no oauth token found for github.com' >&2;;
    *) printf 'To get started with GitHub CLI, please run:  gh auth login\n' >&2;;
  esac
  exit 4
fi
case "$*" in
  "auth token"*) echo gho_mock_token;;
  "pr view 45 "*) cat 'FIXTURES/pr-view.json';;
  "api graphql"*reviewThreads*) cat 'FIXTURES/review-threads.json';;
  "api graphql"*pullRequests\(headRefName*) cat 'FIXTURES/pr-branch.json';;
  "api graphql"*search\(*) cat 'FIXTURES/pr-search.json';;
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

    // gh is checked in the background, and its answer saved: logged out, so no panel. Turning
    // the panel off and on again says why.
    tui.step("panel hidden while logged out");
    let tickets = dir.join("state/taskman/tickets.json");
    let saved = || std::fs::read_to_string(&tickets).unwrap_or_default();
    tui.wait_until("gh's answer saved", || saved().contains(r#""logged_out""#));
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

/// A project on GitHub, with gh logged in and its `gh auth token` held (`stub_gh`), and a start
/// of taskman on it that lets gh go if the test runs past its limit. Returns the repo and the
/// start.
fn held_gh(dir: &Path) -> (std::path::PathBuf, impl Fn(&'static str) -> Tui) {
    let repo = project(dir, &["ENG-52"]);
    sh(&repo, "git init -q && git remote add origin https://github.com/acme/web.git");
    stub_gh(dir);
    std::fs::write(dir.join("gh-logged-in"), "").unwrap();
    std::fs::write(dir.join("gh-hold"), "").unwrap();
    let dir = dir.to_path_buf();
    let start = move |name| {
        let mut cmd = stubbed(&dir);
        cmd.env("GIT_ALLOW_PROTOCOL", "file");
        let release = dir.join("gh-release");
        Tui::start_with(name, &dir, cmd, LIMIT, move || _ = std::fs::write(&release, ""))
    };
    (repo, start)
}

/// The first frame doesn't wait for gh: a folder never checked draws without the PR panel until gh
/// answers, and the answer is saved for the next start.
#[test]
fn the_first_frame_does_not_wait_for_gh() {
    let dir = scratch_dir("pr-held");
    let (_, start) = held_gh(&dir);
    let calls = || std::fs::read_to_string(dir.join("gh.calls")).unwrap_or_default();
    let mut tui = start("the_first_frame_does_not_wait_for_gh");
    tui.step("sidebar while gh is held");
    tui.wait_text("stub ready for ENG-52");
    tui.wait_until("gh asked", || calls().contains("auth token"));
    assert!(!dir.join("gh-answered").exists(), "gh answered before the test let it");
    assert!(!tui.text().contains("GitHub PR"), "a PR panel before gh answered:\n{}", tui.text());

    tui.step("panel once gh answers");
    std::fs::write(dir.join("gh-release"), "").unwrap();
    tui.wait_text("GitHub PR #45 open");
    let tickets = dir.join("state/taskman/tickets.json");
    let saved = || std::fs::read_to_string(&tickets).unwrap_or_default();
    tui.wait_until("gh's answer saved", || saved().contains(r#""ready""#));
    assert_eq!(calls().matches("auth token").count(), 1, "{}", calls());
    tui.quit();
}

/// A folder whose last check was saved draws the PR panel in the first frame, while gh is asked
/// again.
#[test]
fn the_first_frame_draws_the_pr_panel_gh_last_allowed() {
    let dir = scratch_dir("pr-saved");
    let (repo, start) = held_gh(&dir);
    let tickets = dir.join("state/taskman/tickets.json");
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&tickets).unwrap()).unwrap();
    saved["ui"] = serde_json::json!({"github": {repo.display().to_string(): "ready"}});
    std::fs::write(&tickets, saved.to_string()).unwrap();
    let mut tui = start("the_first_frame_draws_the_pr_panel_gh_last_allowed");
    tui.step("panel from the saved answer while gh is held");
    tui.wait_text("GitHub PR #45 open");
    let calls = || std::fs::read_to_string(dir.join("gh.calls")).unwrap_or_default();
    tui.wait_until("gh asked", || calls().contains("auth token"));
    assert!(!dir.join("gh-answered").exists(), "gh answered before the test let it");
    std::fs::write(dir.join("gh-release"), "").unwrap();
    tui.wait_until("gh answered", || dir.join("gh-answered").exists());
    tui.quit();
}
