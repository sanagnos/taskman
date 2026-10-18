# taskman

A Rust terminal UI for working on several tickets at once. A project is a folder (usually a repository) with its sessions: real `claude` (or `codex`) CLIs in embedded terminals. Project agents are sessions with no ticket that can message the others. Integrations are optional and per project: a tracker (Jira, Linear, GitHub Issues or Local files) adds tickets, each with a tracker panel and usually its own worktree, and a GitHub remote with `gh` adds a PR panel. The README and [docs/guide.md](docs/guide.md) describe the product; this file is for working on the code, whichever coding agent you are (`CLAUDE.md` only imports it).

Read these before changing behaviour:

- [docs/architecture.md](docs/architecture.md): the code map (which file holds what), how the pieces fit, the invariants and the cross-cutting concerns. Keep it current in the same pull request when files or structure change.
- [docs/design.md](docs/design.md): why things are built the way they are (the embedded CLI, hooks, session names, accounts, PR lookup, refresh rules). Several choices look simplifiable and aren't.
- [docs/roadmap.md](docs/roadmap.md): ideas agreed but not built, and what hasn't been verified. Update it when something ships.

## Rules in the code

The invariants in docs/architecture.md, as rules; that file and design.md say why.

- A ticket is its project's main checkout and its key (`Ticket::is`): keys repeat across projects, so never look one up by key alone.
- Never start a session without `start_session`, which takes the owner lock that keeps a session to one copy.
- The user's own Claude settings are never touched: hooks and permissions go in through `--settings` (Codex: `-c` overrides).
- Text from trackers and PRs is untrusted: print it through `untrusted` and keep it out of briefs (see "Untrusted text" in docs/design.md).
- Tokens are never stored: `env:VAR` or `cmd:COMMAND`, read in the background by `tracker::Clients`.
- Keep network calls and token commands off the UI thread; commands go through `proc::run` so a hung one gives up. Background work reports back as a `Msg`; there is no async runtime.
- Agents add and close tickets through the inbox (`taskman add`, `taskman close`), never by editing `tickets.json`. A change to the request needs a new `add::INBOX_FORMAT`, so a command never asks an older running TUI for something it can't do. `taskman close` is deliberately not on the `--settings` allow-list.
- Each difference between trackers is a method on `Provider` or `Tracker` in `tracker.rs`: use those rather than matching on a provider.
- Hints name the prefix through `keys::prefix()`, never a literal `ctrl+a`.

## Commands

```bash
cargo build                 # debug build: target/debug/taskman
cargo test                  # unit tests live next to the code (#[cfg(test)] mod tests); tests/ runs the binary
INSTA_UPDATE=always cargo test screens  # rewrite the screen snapshots after a change to drawing; review the diff
cargo fmt                   # CI fails on unformatted code (rustfmt.toml)
cargo clippy --all-targets  # keep it at zero warnings; CI runs it with -D warnings
cargo install --path .      # install the local build as `taskman`
scripts/release.sh X.Y.Z    # cut a release (see "Releasing")
scripts/test-install.sh     # install.sh and `taskman update` against fake releases (after cargo build)
scripts/coverage.sh         # coverage per file, test-install.sh included; CI reports it on Linux (not a gate)
cargo about generate --locked --fail about.hbs -o LICENSE-THIRD-PARTY   # the third-party notices, after a dependency change (needs cargo-about)
```

Rust edition 2024; the code uses `let` chains, so it needs Rust 1.88 or newer.

## Conventions

- Match the surrounding code: comments say why, not what; doc comments on non-obvious items only.
- Code: rustfmt's default 100 columns. Comments: wrap at about 100 (rustfmt doesn't wrap them on stable). Markdown: one line per paragraph and per list item; let the viewer wrap.
- User-facing text (toasts, hints, docs) is short, plain and specific; say what happens, and in errors what to do about it.
- Use mock names in code, tests, docs and commit messages: `ENG`, `ENG-52`, `acme`, `your-site.atlassian.net`, `example.atlassian.net`. Never real company, customer, site or project names.
- New behaviour gets a unit test where it's pure logic (parsing, matching, ordering). Code that parses others' text (tracker responses, ticket files, keys, `untrusted`) also gets `proptest` properties; keep their case counts modest (`ProptestConfig::with_cases`) so CI stays quick.
- Tests never call a real tracker: record a response into `tests/fixtures/` (trimmed by hand, mock data only) and serve it with `replay::serve`.
- Keep docs/guide.md (and the README, for install and the quick start) current when behaviour or setup changes, keep docs/architecture.md current when files or structure change, and add a line under "Unreleased" in CHANGELOG.md for anything users notice.
- Public files don't name internal ticket keys; say what a change does instead.

## Testing the TUI

Start with the unit tests, which need no terminal. `app::test_support::acme_app` is an `App` on a mock project (agents and tickets in every state, issues and PRs fetched, GitHub ready), built with `offline`: no tokens read, no `gh`, no session started. Unit tests can't reach outside the test: their state folder and config file are a scratch folder of their own (`store::scratch`), their home is `/home/tester` (`store::home`) and the CLIs' transcripts are looked for in their state folder (`launch::cli_home`), sessions are `Session::fake` (`test_support::attach`: no process, and what it's sent is recorded), and copying goes out as OSC 52 (`App::clipboard_out`) instead of to the desktop's clipboard. Keep them away from `tick`, which polls status files and starts fetches and sessions; set the layout with `test_support::lay_out` or `draw`. Then:

- **Behaviour** goes in `app/events/`, in the file named after the `app/` file it exercises (`input.rs`, `sessions.rs`, `add_form.rs`; helpers more than one file uses go in `mod.rs`): drive `on_key`, `on_mouse`, `on_paste` and `Msg`s (`settle` waits for a thread's), and check `mode`, `selection`, `toast`, what a fake session was sent and `tickets.json` as saved. Pure decisions still get extracted and tested where they live (see `app/ticket.rs` and `app/poll.rs`).
- **What a screen shows** goes in `ui/screens.rs`: `draw` returns the screen as text, the spinner and version fixed and links left out, compared with its snapshot in `ui/snapshots/`. Times are relative to now (`test_support::iso`), in the middle of an age bucket, so they draw the same on any day. A changed snapshot fails the test and leaves a `.snap.new` beside it; check the difference, then rewrite them with `INSTA_UPDATE=always cargo test screens` (or `cargo insta review`) and commit the `.snap` files with the change.

What needs the real binary (the terminal itself, signals, a CLI's process, the clipboard and browser commands, hooks and other processes) gets a test in `tests/`: `common::Tui` runs the binary in a pty on a `common::project`, with `common::stubbed` for its session CLI and the desktop commands. Wait with `wait_text`, `wait_for` or `wait_until`, which give up with the screen printed, never a fixed sleep; each test's state and config are its own scratch folder. A test that should count towards coverage ends taskman as a user would (`ctrl+a q`, Enter, then `wait_exit`): a binary that's killed writes no profile. Run a new test many times in a row, and under load, before trusting it: CI's macOS runners are slow.

For what neither covers, or to look at something by hand, drive the real binary in a detached tmux session, with state and config kept away from the real ones:

```bash
S=$(mktemp -d); mkdir -p $S/state/taskman
tmux new-session -d -s tmtest -x 160 -y 40 \
  "env XDG_STATE_HOME=$S/state TASKMAN_CONFIG=$S/config.json target/debug/taskman"
tmux send-keys -t tmtest C-a ,        # keys
tmux capture-pane -p -t tmtest        # read the screen (-e for colours)
```

Seed `$S/state/taskman/tickets.json` and `$S/config.json` to set up a scenario. Use `TASKMAN_CONFIG`, not `XDG_CONFIG_HOME`, to isolate the accounts file: `gh` keeps its login under `XDG_CONFIG_HOME` too. Quit with `ctrl+a q`, Enter, so the Claude sessions it started stop.

## Gotchas

- **Esc and ctrl+c interrupts fire no Claude Code hook.** taskman watches for Claude's "Interrupted" line and treats 8 seconds without output as stopped; see `SILENT_STOP`.
- **A closed terminal makes crossterm spin.** crossterm 0.29's reader retries the end of file forever, so the loop never ends. `run` waits on the terminal with `Tty::wait` and calls crossterm only when there's input; don't go back to a blocking `event::poll`. A terminal that closes part-way through a paste or escape sequence still traps crossterm, so a `watchdog` thread ends the process 3 seconds after the terminal goes; `tests/hangup.rs` drives the binary in a pty.
- **crossterm drops input that comes with a resize.** crossterm 0.29 reports the resize and leaves the key unread until the next one, so `Tty::wait` wakes on SIGWINCH for the loop to read resizes as they come, and the loop waits out held input (`Tty::wait_held`) rather than spin. Keep both if the loop changes; `tests/resize.rs` covers the first.
- **Kitty keyboard protocol** terminals can report Shift+p as `p` with SHIFT; `keys::normalize` folds it before anything matches keys.
- **Release builds use `--locked`.** A version bump must update `Cargo.lock` too; the release script does it.
- **A dependency change changes `LICENSE-THIRD-PARTY`.** CI and the release build regenerate the notices and compare: run the `cargo about` command above after changing dependencies and commit the file with `Cargo.lock`. The accepted licenses are listed in `deny.toml` and `about.toml` both; change them together.
- **Portable shell.** Scripts must work with macOS's BSD tools too (no `sed 0,/re/`, no `sed -i` without a suffix); use perl when in doubt.
- **Environment leaking into sessions.** `build_command` removes `CLAUDECODE` and similar (`app::NESTED_ENV`, which `taskman resume` removes too) and `TMUX`, so a session doesn't think it is nested; add to that list if a new marker appears.

## Repository conventions

- The crate is `taskman-app` (`taskman` is taken on crates.io); the binary is still `taskman`. It isn't on crates.io yet: the `vt100` fork in `[patch.crates-io]` can't be published as a dependency, so `cargo install` takes `--git`.
- The repository's owner and name live in two places only: Cargo.toml's `repository` (the code reads `CARGO_PKG_REPOSITORY`) and `repo=` in `scripts/install.sh`. Docs spell the URL out, so a rename is a find-and-replace.
- Commit messages: a summary line, then what changed and why.
- Agent instructions live in this file only, so Claude Code and Codex get the same ones: `CLAUDE.md` is just an import of it, since Claude Code stops reading `AGENTS.md` on its own when a `CLAUDE.md` or `CLAUDE.local.md` exists. Don't put instructions in `CLAUDE.md`, or Codex won't see them.
- Maintainers keep notes for their own checkouts in `CLAUDE.local.md`, which is gitignored.

## Releasing

`scripts/release.sh X.Y.Z` from an up-to-date `main`: refuses while CHANGELOG.md has no entries under "Unreleased", moves them under the version and date, bumps `Cargo.toml` and `Cargo.lock`, builds, tests, commits, tags `vX.Y.Z` and pushes. The tag starts `.github/workflows/release.yml`, which builds macOS (arm64, x86_64) and Linux x86_64 binaries and publishes a GitHub Release, its notes the version's CHANGELOG.md section. If the tag push doesn't start it within a minute, the script starts it by hand (`gh workflow run release.yml -f tag=vX.Y.Z`). A run that failed leaves no release, or a draft the next run clears, so starting it again is enough; a tag that already has a published release is refused, so delete that release first (`gh release delete vX.Y.Z`) to rebuild it. With immutable releases on, a published release's tag can't be reused. Versions follow Cargo's rule for 0.x: before 1.0, the patch number is for anything compatible (fixes, polish and new features) and the minor for breaking changes, such as changed keys, config or saved-state format, or a removed command. Pushing a change to the workflow file needs a GitHub token with the `workflow` scope. Don't rename the release archives (`taskman-vX.Y.Z-<target>.tar.gz`, `SHA256SUMS`): installed copies update from them, and `[package.metadata.binstall]` points at them.
