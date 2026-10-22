# Contributing to taskman

Thanks for helping. Bug reports, ideas and pull requests are all welcome; for anything bigger than a fix, open an issue first so we can agree on the approach before you build it.

Security problems: please don't open an issue; see [SECURITY.md](SECURITY.md).

## Before you change behaviour

Start with [docs/architecture.md](docs/architecture.md), the map of the code for people and coding agents alike: which file holds what, how the pieces fit, and what must stay true. Keep it current in the same pull request when files or structure change.

Then read [docs/design.md](docs/design.md). Several choices look simplifiable and aren't: the embedded CLI, the hooks, session names, the inbox, the refresh rules. [docs/roadmap.md](docs/roadmap.md) lists ideas already agreed and what hasn't been verified; update it when something there ships. [AGENTS.md](AGENTS.md) holds the instructions coding agents follow (Claude Code reads it through `CLAUDE.md`), including the gotchas.

## Build and check

Rust 1.88 or newer (edition 2024; the code uses `let` chains). With rustup, a clone builds with the Rust that `rust-toolchain.toml` names, the one CI uses.

```bash
cargo build                 # debug build: target/debug/taskman
cargo test                  # unit tests live next to the code; tests/ drives the binary in a pty
cargo nextest run --profile ci  # the tests as CI runs them, if you have cargo-nextest (.config/nextest.toml)
cargo fmt                   # CI fails on unformatted code (rustfmt.toml)
cargo clippy --all-targets -- -D warnings  # as CI runs it: zero warnings
scripts/test-install.sh     # install.sh and `taskman update` against fake releases (after cargo build)
scripts/test-release.sh     # release.sh against a local origin, with gh and cargo faked
scripts/coverage.sh         # coverage per file, test-install.sh included (cargo install cargo-llvm-cov)
cargo about generate --locked --fail about.hbs -o LICENSE-THIRD-PARTY   # after a dependency change (cargo install cargo-about --locked --features cli)
```

CI runs fmt, clippy, the tests and the install and release scripts' tests on Linux and macOS (the tests weekly too), checks the oldest supported Rust on Linux, runs `cargo deny` (deny.toml) for advisories, bans (wildcard versions; duplicate versions warn), licences and sources, and checks that `LICENSE-THIRD-PARTY` (the notices of the crates in the binary, which the release archives carry and `taskman --licenses` prints) matches `Cargo.lock`: after a dependency change, regenerate it with the `cargo about` command above and commit it. It also reports coverage on Linux, in the run's summary and as a notice on the pull request; nothing fails on the figure, and a failed coverage job leaves the run green, but a change to logic should come with tests that run it.

CI runs the tests with cargo-nextest (`.config/nextest.toml`): each test in a process of its own, with a time limit. A test from `tests/` that fails runs once more, and if it passes then it shows as flaky in the run's summary and as a warning on the pull request, and the run passes; in `tests/ownership.rs` and `tests/stopping.rs`, which guard one copy of a session, a pass on the rerun still fails the run. `cargo test` runs the same tests, without the reruns.

Code that parses what others write (tracker responses, ticket files, keys and links, the text `src/untrusted.rs` cleans) has property tests (`proptest`) beside its example tests: they state what must hold for any input, like "no terminal control character survives `clean`". The Jira and Linear clients are tested against recorded responses served locally (`src/replay.rs`), kept in `tests/fixtures/`: trim them by hand and use mock data only (`acme`, `ENG-52`, `example.atlassian.net`), never a real workspace's output. No test calls a real tracker.

## Testing the TUI

The TUI has unit tests that need no terminal. `src/app/test_support.rs` builds the App on a mock project with nothing started, sessions that run no process, and a state folder of each test's own. Behaviour (keys, mouse, paste, dialogs, what reaches a session) is tested in `src/app/events/`, a file for each part of the App; what the screens show, in `src/ui/screens.rs`, against snapshots in `src/ui/snapshots/`. What only the real binary can show (the terminal, signals, a CLI's process) is tested in `tests/`, which drives it in a pty. [AGENTS.md](AGENTS.md) says which to use when.

### Updating snapshots

A change to what's drawn fails the screens' tests, each showing the difference, and leaves a `.snap.new` file beside the snapshot. If the new screen is what you meant:

```bash
INSTA_UPDATE=always cargo test screens   # or `cargo insta review`, with cargo-insta installed
git diff src/ui/snapshots/               # check each change
```

Commit the updated `.snap` files with the change, so reviewers see the screens it changes. A new screen gets its snapshot the same way.

## Trying the TUI

To look at something by hand, drive the real binary in a detached tmux session, with its state and config kept away from your own:

```bash
S=$(mktemp -d); mkdir -p $S/state/taskman
tmux new-session -d -s tmtest -x 160 -y 40 \
  "env XDG_STATE_HOME=$S/state TASKMAN_CONFIG=$S/config.json target/debug/taskman"
tmux send-keys -t tmtest C-a ,        # keys
tmux capture-pane -p -t tmtest        # read the screen (-e for colours)
```

Seed `$S/state/taskman/tickets.json` and `$S/config.json` to set up a scenario. Use `TASKMAN_CONFIG`, not `XDG_CONFIG_HOME`, to isolate the accounts file: `gh` keeps its login under `XDG_CONFIG_HOME` too. Quit with `ctrl+a q`, Enter, so the sessions it started stop. In tmux, send Escape on its own: Escape followed by a key arrives as Alt+key.

## Conventions

- Match the surrounding code: comments say why, not what; doc comments on non-obvious items only.
- Line widths: code at rustfmt's default 100 columns; comments wrapped by hand at about 100 (rustfmt doesn't wrap them on stable); Markdown one line per paragraph and per list item, left to the viewer to wrap.
- New behaviour gets a unit test where it's pure logic (parsing, matching, ordering).
- User-facing text (toasts, hints, docs) is short, plain and specific: say what happens, and in errors what to do about it. Hints name the prefix through `keys::prefix()`, never a literal `ctrl+a`.
- Use mock names in code, tests, docs and commit messages: `ENG`, `ENG-52`, `acme`, `your-site.atlassian.net`. Never real company, customer, site or project names.
- Text from trackers and PRs is untrusted: print it through `src/untrusted.rs` and keep it out of briefs (see "Untrusted text" in docs/design.md).
- Scripts must work with macOS's BSD tools too (no `sed -i` without a suffix); use perl when in doubt.
- Keep [docs/guide.md](docs/guide.md) current when behaviour or setup changes, and add a file under [changelog.d/](changelog.d/) for anything a user would notice; its [README](changelog.d/README.md) says how.

## Pull requests

- One change per pull request. Fill in the template: what changed and why, how you checked it (tests, or what you saw in the TUI), and the tools you used.
- Commit messages: a summary line, then what changed and why.
- Note a change users would notice in a new file under [changelog.d/](changelog.d/), not in CHANGELOG.md, so pull requests don't conflict there.
- CI must pass: fmt, clippy, tests, the install script's tests, MSRV and cargo-deny.
- Dependabot opens weekly pull requests for the crates and the actions, each release a week old at least. `LICENSE-THIRD-PARTY` names crates, not versions, so one that only changes versions passes CI as it is. One that adds or drops a crate, or brings a changed license text, fails the notices check: check out its branch, regenerate the file with the `cargo about` command above and push it.
- By contributing you agree that your contribution is dual licensed under MIT OR Apache-2.0, as the README says.

### AI-assisted contributions

Coding agents are welcome to help; [AGENTS.md](AGENTS.md) is written for them. Say in the pull request which tools you used and how much of the change they wrote. You remain responsible for it: be ready to explain any part of it, and run it yourself, not only its tests. An issue an agent drafted should be trimmed by a person to what matters (what happened, what you expected, how to get there) before it's opened.

## Releases

Maintainers cut releases with `scripts/release.sh X.Y.Z` from an up-to-date `main`. It refuses while there is nothing to release (no files in `changelog.d/`, and no lines left under "Unreleased" in the changelog from before) or a file doesn't start with `- `, waits for CI on `main` to pass on the commit (starting it by hand if GitHub hasn't within two minutes), then moves the entries into `CHANGELOG.md` under the new version and date, in the order they were merged, deletes the files, bumps `Cargo.toml` and `Cargo.lock`, builds, commits "Release vX.Y.Z", tags `vX.Y.Z` and pushes both together. If anything fails before the push, the checkout is put back as it was, so fix it and run the script again. The tag starts the Release workflow (`.github/workflows/release.yml`), which builds the macOS (Apple Silicon and Intel) and Linux binaries, checks that each one runs and says the tag's version, and, once every build the install script picks has its archive and CI has passed on the release commit, publishes a GitHub Release with one `SHA256SUMS` and the version's changelog section as its notes. If GitHub hasn't started the workflow within three minutes, the script starts it by hand; `gh run list --workflow release.yml` shows which happened. A run for a tag that's already released builds nothing and ends green. If a run fails waiting for CI on the release commit, get that CI run to pass, then carry on with the same run, its builds kept: `gh run rerun <run id> --failed`. A run that failed leaves no release, or a draft the next run clears, so starting it again is enough (once CI on the commit has been re-run, or started by hand with `gh workflow run ci.yml --ref main`, and passed, if that was what failed): `gh workflow run release.yml -f tag=vX.Y.Z`. The workflow builds nothing for a tag that already has a published release; to rebuild one, delete it first (`gh release delete vX.Y.Z`), then start the workflow. With immutable releases on, a published release's tag can't be reused.

To try the workflow without releasing, after changing it for instance, start a dry run from your branch: `gh workflow run release.yml --ref <branch> -f dry_run=true`. It needs no tag: it builds and checks every binary and archive for the version in `Cargo.toml`, keeps the archives and `SHA256SUMS` as the run's artifacts (`gh run download <id>`), and publishes nothing.

Versions follow Cargo's rule for 0.x: the patch (0.1.0 to 0.1.1) for anything compatible, fixes and new features alike, and the minor (0.1.1 to 0.2.0) for breaking changes such as changed keys, config or saved-state format, or a removed command. The release archives' names (`taskman-vX.Y.Z-<target>.tar.gz` holding just `taskman`, and `SHA256SUMS`) are what installed copies update from, so they must not change.

## Code of conduct

Everyone taking part is expected to follow the [code of conduct](CODE_OF_CONDUCT.md).
