# Roadmap and known gaps

Ideas discussed but not built yet, with the reasoning behind them, and what hasn't been verified. Update this when something ships or a new idea is agreed.

## Ideas

- **Sleep by memory use:** sleeping is by time and an optional count of sessions. Reading the CLIs' resident memory (and the machine's free memory) could pick sessions by what they cost, or sleep earlier when memory is short; it needs a per-platform read and a rule for "short".

- **GitHub behind an interface,** as the trackers are (`tracker::Client`, tested against recorded responses in `tests/fixtures/`): the `gh` lookups (PR, checks, reviews) behind one interface with a fixture-backed implementation, so the PR fetching and refresh logic get unit tests, and the demo can run on recorded data with no special mode.
- **Colours in the screen snapshots**: `src/ui/screens.rs` compares the text drawn, not its colours or styles, so a badge drawn in the wrong colour, or a selected card without its background, passes. The `TestBackend` keeps each cell's style; checking the cells that carry meaning would catch those.
- **Open file links in `$EDITOR`**: Codex and Claude link file paths (`file://`, editor URIs); `ctrl+a l` lists and copies them but only http(s) opens. Opening a path inside the session's folder in `$EDITOR`, through the same hand-off as `ctrl+a v`, would make them useful.
- **Plain URLs broken over rows**: `ctrl+a l` lists plain URLs too, but one a CLI breaks at a `/` or `-` on a row that isn't full is found only in part. #45 completed such a URL from an OSC 8 link the session printed that starts with it.
- **A board view of the Local files columns**: the columns side by side, in the agents' project panel or a popup, with moving a ticket by key. The tracker shipped without it, to see how it's used first.
- **Local files comments:** a `## Comments` section parsed into the panel's comments, with authors and dates. Left out of v1; comments written in the body show as part of it.
- **Removing worktrees without a remote:** closing a ticket only offers to remove its worktree when its commits are on a remote or merged into origin's default branch, so in a repository with no remote it never does. It needs a local-merge check (into the local default branch), which Local files projects without a remote need.
- **Open an existing conversation on a ticket** (`ctrl+a s`): scan `~/.claude/projects` for transcripts whose branch, folder or first prompt matches the ticket, and switch to one (resume in its original folder) or fork it. Today a ticket only tracks the conversation taskman started; a user can still `/resume` inside the pane.
- **Import an outside session** (`ctrl+a i`): the same picker across all repos, creating a ticket from the session's branch. A session still running in another terminal can't be attached; offer to take over once it's closed, or fork.
- **Strip token variables from Claude sessions:** sessions inherit taskman's environment, so an `env:` token is visible to Claude's shell. taskman knows which variables hold tokens and could remove them from the sessions it starts.
- **Tracker MCP server per session:** pass each session (Claude `--mcp-config`, Codex `-c mcp_servers…`) the tracker's hosted MCP server with the project's token, so sessions can also comment and move tickets in the right workspace. Check first that Linear's and Atlassian's servers take a token in a header, and that the token can stay out of the arguments (an environment variable the config refers to). `taskman status` covers reading.
- **The failing CI log inside taskman:** each check in the GitHub panel links to its run; a view of `gh run view --log-failed` in taskman would save opening the browser.
- **Tracker status in the sidebar cards** (In Progress, In Review).
- **Names for accounts:** an optional Name (`Work Jira`) that pickers show instead of `who · source`. The short labels only clash when one person keeps several accounts for one email, which is rare since a Jira token covers every site; add it if that changes. Optional in config, so older files read as unnamed.
- **Symbols-only sidebar badges**: a Settings option to show each state's symbol without its word, so long names fit.
- **Sessions' own clipboard writes inside tmux:** Claude Code's copy sends OSC 52, which taskman passes on, so tmux's default `set-clipboard external` drops it. Sessions don't see `$TMUX` (removed so they don't think they're nested); taskman could decode the write and copy it the way it copies a selection.
- **Signed macOS binaries** (Apple Developer ID and notarization) if people download releases through a browser, which quarantines them.
- **Dropping the old status layout**: readers fall back to `status/<KEY>.json` for sessions an older TUI started, from before a folder per project. Remove `store::legacy_status_file` once no install can still be on such a version.
- **Selecting a full-screen session's history**: Codex (alternate screen by default) and Claude's full-screen mode keep their history themselves, so only their screen can be selected and `ctrl+a v` opens only the screen. Codex's `--no-alt-screen` doesn't help with vt100 0.16: Codex prints history through a scroll region (`ESC[1;Nr` then newlines at its bottom), and vt100 drops lines scrolled out of a region instead of keeping them, as xterm does when the region starts at the top row. It would take a vt100 change (keep those lines; make `ESC[3J` clear the history, or every resize duplicates it, since Codex clears and reprints on resize), and even then Codex reprints only a tail on resume and hard-wraps lines at the width it printed them. Meanwhile `ctrl+a t` hands the session to a real terminal.
- **Taking a session over from a fresh terminal**: `taskman resume KEY` refuses while the TUI runs the session and says to press `ctrl+a t` there. It could instead ask the running TUI to hand it over through the inbox, which needs a new inbox format.
- **Selecting with the outer terminal:** a key that turns mouse capture off, so the terminal's own selection works (whole rows across panes, no history). Left out so far: most terminals already skip mouse capture on shift+drag (option+drag in iTerm2).
- **Terminal polish:** poll and redraw less when idle (16ms poll, 100ms redraw); answer the background-colour query from the outer terminal rather than always dark; expand tabs instead of dropping them; cut text by display width in `util::truncate`.
- **Panels in `$EDITOR`:** `ctrl+a v` opens the session only; a panel's text can be selected whole by dragging past its edge, and `ctrl+a o` / `p` open the ticket and PR.
- **Telling projects apart in toasts and the sidebar** when they share a key: toasts and replies say `beta/WP-1` then, but the sidebar shows the key under each project's heading, which is enough while project groups are expanded.

## `taskman add`: what's next

`taskman add` adds tickets and agents from any shell or session, and agents can add helper agents with a role (see design.md). Not built yet:

- **`--start "prompt"`** to give the new session its first message. It would spend tokens the user didn't see being spent; agree on how it shows before building it.
- **A default CLI per project** (also under Codex below), which `taskman add` would use too.
- **Moving tickets on Linear, Jira and GitHub Issues,** as `taskman move` does on a board, so agents can work the board as well as fill it (`taskman add --new` creates tickets on every tracker).
- **Limits on agents adding agents** (how many, or removing them from the command line). None yet: every agent shows in the sidebar, and the user closes the ones they don't want.

## Codex: what's next

Codex sessions are built (see design.md). Not built yet:

- **A CLI default per project,** so a project worked in Codex doesn't need the choice on every ticket. The Add form defaults to Claude today.
- **Codex agents messaging Codex tickets** with `codex queue --thread <id> --message …`, the id coming from the SessionStart hook. Tested: it delivers a turn to a running session, including one started with `-c`; names (`/rename`) only resolve after a session's first turn, so use the id. From inside a Codex session the sandbox blocks it until approved or allowed by a rule. There's no session list and no idle notification. Worth building only if whole projects run on Codex.
- **Claude agents reaching Codex tickets** the same way, through Bash. Left out to keep one rule.

Things learnt about Codex 0.161 in a taskman pane: it renders, resizes and scrolls back fine (it drew inline then; 0.162 runs on the alternate screen, with mouse reporting, and keeps its history itself; see "Selecting a full-screen session's history"); its window title is "<thread name> | <folder>"; text followed at once by Enter counts as a paste and the Enter becomes a newline, which matters only if taskman ever types into a Codex pane.

## Worktrees: what's next

taskman creates each ticket's worktree (see design.md). Not built yet:

- **The CLIs' own worktree flags.** Revisit `claude --worktree` if Claude Code gets a setting to keep the worktree on exit without asking (a `worktree.cleanupOnExit` request is open), and `codex --worktree` if it takes a branch name and folder. Both would still need taskman to know the folder before the session starts.
- **A worktree command per project,** the escape hatch instead of more settings: taskman runs it with the key and branch name and starts the session in the folder it prints, like Claude Code's `WorktreeCreate` hook but for both CLIs. Build it when a team needs something the settings can't say (another base branch, a different layout, setup steps).
- **A base branch per project,** for teams that branch from `develop`. Today it's origin's default branch; the worktree command could cover it instead.
- **Settings committed in the repository** (`.taskman.json`), so a team shares its tracker, branch format and "New tickets" default (say, "Don't create" when its workflow makes its own worktrees).
- **Per-CLI project settings** (default CLI for new tickets, model, permission or sandbox mode), once there are enough of them to deserve a section.
- **"Close all merged" in the TUI:** `taskman close --merged` does it from a shell; a dialog listing each merged ticket with its removal check, answered once, would do it without leaving taskman.
- **Cleaning up worktrees left behind:** tickets closed before removal was offered, or closed with Enter, leave their worktrees. A command (or a Settings action) could list a project's worktrees that no open ticket uses, with the same safety check, and remove the safe ones.
- **Following a session into a worktree it makes itself** partway through: its folder still stays where it started.
- **Setting up a new worktree** (`.env`, `node_modules`): Claude Code copies files listed in `.worktreeinclude` for its own worktrees; taskman's start bare.

## Stacked tickets: what's next

A ticket can stack on another (see design.md). Not built yet:

- **GitHub's own stacks.** GitHub's stacked PRs (public preview since July 2026) add a stack map, merging a whole stack in one go and CI against main; `gh stack link <base-branch> <branch>` turns a chained pair into one, and GitHub offers it in a banner. The brief could suggest it when the `gh-stack` extension is installed. Wait for it to leave preview.
- **The stack's position** ("2/3") in the sidebar: only GitHub's GraphQL API has it (`pullRequest.stack`, `stackEntry`), not `gh pr view --json`.
- **Saying when a rebase is due:** after a squash merge, notice that the PR still carries the base's commits rather than always mentioning it.
- **Chains in the agents' board**, drawn as a tree.

## Updates: what's next

taskman installs new releases in the background and applies them on restart (see design.md). Not built yet:

- **A "restart now" key** that quits and starts the new binary in place (sessions resume). It must exec the binary's path, not the running file: on Linux that reads as `taskman (deleted)` after an install.
- **Release channels** (stable, latest) and signed binaries.

From the pre-open-source review; the install and integrity fixes shipped, these didn't:

- **On the public repository:** turn on immutable releases and tag protection for `v*`, so a published release's assets can't be replaced, then check the curl install and `taskman update` against a real release with no `gh` installed.
- **Linux arm64 and musl builds** (`aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`) and their `install.sh` cases. Try them in a test release first: GitHub's arm64 Linux runners are free for public repositories, and a failing build holds up every other target's release.
- **Check attestations against a real release.** From v0.1.0 a release without an attestation is refused when `gh` can check, and `scripts/test-install.sh` covers that with a fake `gh`; once v0.1.0 is out, run `gh attestation verify` against its archives, and the install script and `taskman update` with `gh` logged in, to see a real one verify.
- **cargo-dist,** evaluated and not adopted for now: it would replace `release.yml`, the install script and the asset names that `taskman update` and older releases' updaters rely on, for Homebrew, binstall and installer extras the launch doesn't need. Reconsider for a Homebrew tap. cargo-binstall needs only `[package.metadata.binstall]` pointing at the existing archives, which Cargo.toml has; untried until the repository is public. taskman isn't on crates.io (the `vt100` fork can't be a published dependency), so it would be `cargo binstall --git`.

## Known gaps

What hasn't been checked on a real setup yet. Reports from other platforms and terminals are welcome as issues.

- **Links in sessions:** clicked in Windows Terminal on WSL with `FORCE_HYPERLINK=1`: Markdown links, plain URLs, and a link wrapped over two rows, which opened in full from its last row. Checked on Linux (WSL) in tmux with real Claude Code 2.1.296 and Codex 0.162 inside taskman: the links reach the outer terminal as OSC 8, and `ctrl+a l` lists them and opens one through `$BROWSER`. A link goes out once per run of cells, left open over the cursor moves and colours between them; checked with a stand-in CLI printing Claude's and Codex's bytes. Not tried: clicking in other terminals (WezTerm, kitty, iTerm2, Ghostty) and which modifier each needs with taskman's mouse capture on, macOS Terminal (no OSC 8: `ctrl+a l` only), and tmux with `hyperlinks` passing them on.
- **Links in panels**: checked on Linux (WSL) in a private tmux server with real Linear and GitHub data (a PR of taskman's own, and a public PR with reviews and threads): the raw output has OSC 8 around the ticket key and title, the PR number and title, each check, the review, the thread and a URL in the description, each with its URL, and `ctrl+a l` lists them first and opens one through `$BROWSER`. Not tried: a real click on a panel link, and Jira (its issue URL is the one `ctrl+a o` opens). The border's key and PR number link only without a title line: checked the same way, the key of an issue that failed to load links and a loaded one's doesn't.
- **Approving agents**: checked in tmux on Linux (WSL) with a stand-in `claude`: an Enter in the first 1.5 s is ignored, Enter adds the agent, Esc declines, and a role change on an open agent says so, and a request queued while taskman was closed asks at the next start. Not tried from a real Codex session.
- **Session names with a `/`**: checked only with Claude Code 2.1.295, which lists `beta/WP-1` in ListAgents and delivers SendMessage to it. Codex sessions take no name.
- **GitHub Enterprise,** for the GitHub check (`github::probe`) and the repo Settings names (`github::default_repo`). Both were checked on Linux (WSL) with github.com remotes: HTTPS, SSH and an SSH alias (`github.com-work`), a repository with no remote and a plain folder, and `origin` + `upstream` before and after `gh repo set-default`. An alias whose name doesn't contain "github" counts as no GitHub remote. When no remote's host contains "github", every remote counts for the repo name, so a non-GitHub remote named `upstream` beside an Enterprise `origin` would be named.
- **The project form's list of Jira projects and Linear teams:** tested only against recorded responses, trimmed by hand from the documented shapes of Jira's `GET /rest/api/3/project/search` and Linear's `teams` query, and with the listing's answer stood in for in the form's tests. Not tried against a real Jira site or Linear workspace: owed are the paging past 100 projects, what an account without the Browse Projects permission gets (an empty list is expected), and how long a large site takes to list.
- **Creating tickets (`taskman add --new`) on Jira and GitHub Issues:** tested only against recorded responses, trimmed by hand from the documented shapes of Jira's `project/{key}` and `POST /rest/api/3/issue` (its 400 with per-field `errors`) and GitHub's `POST /repos/{owner}/{repo}/issues`. Not tried against a real site or repository: owed are the wording of Jira's refusal for a required custom field, a team-managed Jira project's issue types, and which `gh` and ureq failures really mean the request never left. Linear was checked against a real workspace: the issue was created in the team's default state with its title and description, the key and link were printed, and a prefix naming no team stopped before anything was created.
- **Saving a token to the keychain:** tested only against stand-ins for `security` and `secret-tool` (`tests/fixtures/keychain-stub.sh`): unit tests drive both, `tests/keychain.rs` the one for the OS it runs on, so CI's macOS runner covers the `security` path with the stand-in. Nothing was checked against a real keychain, since development was on WSL, which has neither. Owed on macOS: that `security -i` parses the double-quoted line as expected and exits as assumed, whether the Always Allow prompt shows when taskman reads the item it saved, and the error a locked keychain over SSH gives. Owed on Linux: GNOME Keyring and KWallet through `secret-tool`, the unlock prompt for a locked keyring within the 60 s limit, and that `secret-tool lookup` with no Secret Service writes to stderr as the probe expects (from libsecret's messages, not seen). The token pages (Atlassian's API tokens page and Linear's Security & access settings) were checked current on 2026-10-12.
- **Account labels on macOS**: the long `cmd:security …` label was seen on macOS; the fix was checked on Linux (WSL) only, with two Jira accounts for one email and different commands.
- **Sidebar badges**: `✦ new` and an unseen `■ exited` were covered by tests but not seen live. `▲`, `●` and `■` are ambiguous-width characters, which terminals set up for CJK locales may draw two columns wide; unchecked.
- **Background updates on macOS:** checked on Linux (WSL) only, with a copy built as an older version installing the latest release while it ran. An install writes beside the binary and renames it over, so it doesn't depend on how BSD `install` replaces a file, but a real run on macOS is still owed. The HTTPS download and checks were tried against local fake releases (`scripts/test-install.sh`), not GitHub's real redirects.
- **Inferred states** (see "Inferred states" in design.md): measured with Claude Code 2.1.296 and Codex 0.162 on Linux (WSL) only, through tmux rather than taskman's own pane; the end-to-end tests are still to come. Not tried: Esc on a Claude question or plan (whether it ends the turn without a hook, which would leave "needs you" until the next one), ctrl+c on either CLI's permission prompt, Codex's prompts other than running a command (network, permissions, MCP requests) and its subagents and compaction, and an automatic compaction in the middle of a Claude turn (the `compact` SessionStart was seen after `/compact` only). A network stall was simulated with a proxy holding the traffic, not a real outage.
- **Codex on macOS:** checked on Linux (WSL) only.
- **Stacked tickets with GitHub's native stacks:** checked with plain chained PRs only (a squash merge and a merge commit, with and without branch deletion), on Linux. A native stack's server-side rebase of the upper branch wasn't seen.
- **`taskman add` with Jira, from Codex, and on macOS:** checked with Linear on Linux (WSL), from a Claude agent, a simulated ticket session and plain shells. Jira's "not found" is a 404 the command refuses; unchecked with real credentials.
- **`taskman status` with Jira:** checked with Linear only; the Jira path uses the panel's client. Also unchecked on macOS, whose Codex sandbox (Seatbelt) may report a blocked request differently, which would only lose the hint. Picking the ticket or project from the folder is checked on Linux only.
- **The Add form's open issues on Jira and Linear:** tested against recorded responses only. Jira's `search/jql` query and Linear's team and state filter haven't been run against a real site or workspace. Listing and ↑/↓ picking were tried in a real terminal on a Local files board, on Linux (WSL).
- **A successful Jira connection test and the "Jira project exists" check** with real credentials (only the failure paths were exercised during development).
- **`taskman close`**: checked on Linux (WSL) with real repositories and a stand-in `gh` answering merged, open and missing PRs (including a squash merge whose commit is on no remote), a close dialog open while a request came in, and an owner lock held as `taskman resume` holds it. Not tried with real GitHub PRs, from Codex, or on macOS. Adding a ticket back while its worktree is still being removed, also after taskman was stopped mid-removal or quit, is covered by `tests/close.rs`, with a git that holds up `worktree remove`.
- **Closing the terminal mid-input**: checked on Linux (WSL) in a pty (20 runs each mid-paste, mid-mouse report and mid-key) and with `tmux kill-server`; macOS only through CI's `tests/hangup.rs`. A real dropped SSH connection wasn't tried.
- **One copy when `taskman resume` is killed, and `prefix R`**: checked on Linux (WSL) with a stub CLI (`tests/ownership.rs`, which fails on the code before the fix); macOS only through CI. Not tried with real Claude Code or Codex, whose own child processes also inherit the lock.
- **Signals, ctrl+z, a panic in a session, a second taskman, an unreadable tickets.json, selecting, copying, scrolling a selection and `prefix v`**: once checked by hand in tmux, now covered by `tests/lifecycle.rs` and `tests/selection.rs` (the binary in a pty, a stub CLI, stub clipboard commands); macOS only through CI. The panic comes from a debug-only test hook rather than a real vt100 bug, and the copies reach stubs, so the real clipboard commands are still as below.
- **Shift+letter on kitty-protocol terminals** (Ghostty, kitty, WezTerm, iTerm2 with CSI u): covered by a unit test, not by a real macOS terminal.
- **Copying on macOS, Wayland and X11**: checked on Linux (WSL) only, inside tmux (`set-clipboard` on, external and off), with clip.exe, and with no command (OSC 52). `pbcopy`, `wl-copy`, `xclip` and `xsel` weren't run.
- **"Needs you" on real permission prompts:** development ran in auto mode, which rarely prompts. The Notification handling relies on `notification_type` values (`idle_prompt`, `auth_success`) that should be checked against the current Claude Code hook docs.
