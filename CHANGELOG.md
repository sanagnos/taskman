# Changelog

What changed in each release; a release's notes on GitHub are its section here. Versions follow Cargo's rule for 0.x: the patch number (0.1.1) for anything compatible, fixes and new features alike, and the minor (0.2.0) for breaking changes, such as changed keys, config or saved-state format, or a removed command.

## Unreleased

- `ctrl+a w` lists every session waiting on you, oldest first, with how long it has waited, why (permission or question) and what it asks for: for a permission prompt the tool and its command, as the Claude Code or Codex hook reports it. Enter goes to that session. A session that has waited a minute shows its time in the sidebar (`▲ 4m`) in place of "needs you". Sessions started by an older taskman show the prompt's message until their next prompt.

## 0.1.15 - 2026-10-19

- Scrolling a session back over some emoji, such as ⚠️ (the warning sign with its emoji selector), no longer leaves stale characters on screen and pushes the pane's right edge a column out on those rows. taskman counted that emoji as one column where terminals such as Windows Terminal draw two, so everything after it on the row was drawn a column off and some of it was never redrawn.

## 0.1.14 - 2026-10-19

- Help (`ctrl+a ?`) wraps its lines instead of cutting them at 80 columns, so each key's description is whole (`ctrl+a {` / `}` ends with Enter), and lists the sidebar's badges in columns that no longer run together. It scrolls by its wrapped rows. The legend adds `✦`, which marks an agent; a card added by `taskman add` is now marked `+ new` rather than `✦ new`.
- The footer counts sessions waiting on you with `▲` when one of them asks you something, and leaves the count out rather than run it into the keys on a narrow window.
- Deleting a project counts its tickets and agents apart (`Its 4 tickets and 2 agents close too`), and the quit dialog says when quitting waits for a closed ticket's worktree to be removed.
- In a repository with no remote, the Add form's Base says the worktree starts from your local `HEAD` and names its branch, instead of `origin/main`.
- On an agent, `ctrl+a j`, `c` and `g` say what they change for tickets, or, in a project with no board to show, that agents have no panels, instead of silently flipping a setting; `p` and `m` say agents have no PR or comments instead of calling the agent a ticket.
- The project board no longer shows a ticket's branch twice when the project has no GitHub, and Project settings' Branch hint reads "Local files' own".
- The Welcome screen and the messages in a session's pane keep a margin, so wrapped lines don't run into the pane's edges.
- Where there's no keychain (WSL, a server), the account form's Keep in starts on File: paste the token and taskman writes it to a file only you can read, `~/.config/taskman/tokens/<account id>`, and saves only the command that reads it. No variable to set and no restart. Where there's a keychain, nothing changes. The guide also shows how to add a token variable to your startup file without the token showing on screen or in your shell history.
- A ticket session's startup instructions no longer ask for the key in the PR title, which taskman doesn't need: they ask for the PR from the ticket's branch, pushed under its own name, since the key in the branch is how taskman and the tracker link the PR. In the main checkout they still ask for the key in the branch's name.

## 0.1.13 - 2026-10-19

- `taskman add` adds a key unchecked when the account's token command isn't installed, as it does whenever the tracker can't be asked, instead of refusing it as not found. A tracker error that merely mentions "not found" (a name that didn't resolve, say) no longer reads as a missing ticket either, and Codex's sandbox hint follows the kind of failure rather than the HTTP library's wording.
- taskman starts without waiting for `gh` to say whether it's logged in, which took a second or more on some machines: the PR panel shows as it did last time, and changes if gh's answer has. A project taskman hasn't checked before shows its PR panel once gh answers. Projects on the same host ask gh once between them.
- Settings and the links list (`ctrl+a l`) scroll to keep the selected row in view, and say how many rows are hidden above and below. With more projects and accounts than fit, Settings could select a row below the bottom of the list, so the project `ctrl+d` would delete wasn't shown. The links list no longer shifts on every move while the selected link is in view.
- A session whose CLI exits keeps its last screen in the pane, with a bar saying how it ended (`Claude exited with code 1 · Enter to resume · ctrl+a x to close the ticket`), so what the CLI said before it went stays in view. A CLI that didn't start says so in its pane, and, when it isn't installed, what to do, instead of a toast cut off at the window's width. A session taskman stopped reading after an error says that and points to `panics.log`, which now names the session. An ended session you have seen and then left alone for the sleep time keeps only its last screen, freeing the memory its history took.
- Releases include Linux builds for arm64 (Graviton, Linux on Apple Silicon or Windows on ARM, Raspberry Pi with a 64-bit OS) and builds with musl for x86_64 and arm64, which run on Alpine and on distributions whose glibc is older than 2.34. The install script picks the right one; `taskman update` keeps an installed copy on its own build. A release is published only with every build, so the install script and `taskman update` never find one without the archive they want.
- Sessions' scrollback takes less of taskman's memory: rows are kept only up to their last character, so a busy session in an 80-column pane levels off at about 22 MB instead of 31 MB, and one in a 147-column pane at about 27 MB instead of 55 MB.
- Settings → General → **Scrollback** sets how many rows of history each session keeps, 1,000 to 50,000 (10,000, as before, by default). It applies to sessions started after the change.

## 0.1.12 - 2026-10-19

- taskman rewrites `inbox/alive` in its state folder once a second instead of about three times a second, so tools that watch the folder (backups, indexers, file watchers) are woken less. `taskman add` and `taskman close` still answer within a second.
- `taskman --help` and the bug report form name GitHub Issues among the trackers, and the install script, on a platform with no prebuilt release, links the guide's section on building from source.
- `taskman update --help` prints its usage instead of updating, and `taskman update` refuses arguments it doesn't know (such as a misspelt `--force`) instead of ignoring them. Every command now reads its options the same way: an empty `--project=` is refused with `--project needs a value`, and an option that takes no value refuses one (`taskman close --merged=no` was read as `--merged`).
- `taskman --help | head`, and the same with a command's usage or output, no longer ends in a "Broken pipe" panic when the reader stops early.
- A long paste into a session whose CLI reads slowly no longer freezes taskman until the CLI has read it all: keys, switching cards and quitting work meanwhile. Input that would leave more than 4 MB waiting for a session to read is dropped, and a toast says so.

## 0.1.11 - 2026-10-19

- `taskman --licenses` and the release archives' `LICENSE-THIRD-PARTY` list the crates without their versions.
- A click on a project's header, or a drag of it, makes it the current project: `ctrl+a n` and `ctrl+a ,` open on it until you select a card, instead of on the selected card's project. An empty project's header counts too, so the Add form can open on a project with no cards.
- taskman installs an update in the background only when `gh` verified its attestation, and only within the same minor version. Without `gh`, logged out, or with `TASKMAN_INSECURE_SKIP_ATTESTATION` set, the sidebar says `↑ vX.Y.Z: run taskman update` and Settings → Updates says why. A new minor version, such as 0.2.0, is a breaking release: the sidebar says `↑ v0.2.0: breaking changes`, Settings links its notes, and `taskman update` installs it. To go back to an earlier release, set Settings → Updates to "Only notify" and run the install script with `TASKMAN_VERSION=vX.Y.Z` (see the guide).
- The install script, `taskman update` and the background updates accept an attestation only from the release workflow run from a version tag or `main`, on a GitHub-hosted runner, and `taskman update` hands gh the archive in a new folder only you can open.
- `taskman status` no longer avoids the name of a session that was killed before it could say it ended, when it says what a session that isn't running would be called: like `taskman resume`, it counts a session as running while taskman or `taskman resume` runs it.
- The release binaries are about a fifth smaller, built with link-time optimisation. A release is now published only once CI has passed on its commit and each binary has run and reported the release's version, so a broken build no longer reaches the installs that update to it.

## 0.1.10 - 2026-10-18

- Typing in the Add form (`ctrl+a n`) while it's on Type, or on another choice, now goes to the Ticket or Name field instead of being dropped: a number, a key of the project or a link switches Type to Ticket, so `ctrl+a n` then `52` adds ENG-52, and anything else names an agent. A paste lands there too.
- Branch names are treated as others' text, like titles and comments. A session's brief names a branch only when it is plain (letters, digits, `.`, `_`, `/`, `-`) and otherwise says to run `taskman status` for it; the sidebar, the panels and `taskman status` clean branch names of characters that hide text. A Local files ticket's branch is now made from its key and the rest of its file name as a slug, rather than taken from the file name as it is. A ticket no longer takes the branch of a PR from a fork as its own. Branches tickets already have are kept.
- taskman asks the terminal for its background when it starts, and on a light one draws the selected card, the footer, hints and toasts so they read (they were dark grey bars and yellow text on white), and tells sessions the real colours, so Claude Code and Codex pick their light themes. Without an answer it reads `COLORFGBG`, else draws as before. Settings → General → **Theme** sets light or dark instead.
- After a project is renamed, `taskman add --agent`, `taskman close --agent`, the Add form and the agent dialog find the agents added before the rename, by name: `taskman close --agent` said they weren't open, and `taskman add --agent` asked for a second agent of the same name.
- Narrowing the window no longer ends a session that has a wide character (CJK text or an emoji such as ✅) at the end of a line: the session's CLI was killed when that line was next erased or written over.

## 0.1.9 - 2026-10-18

- Running an older taskman no longer drops settings and state a newer one saved, such as `"sleep_after": null` (never sleep): fields it doesn't know in `tickets.json` and `config.json` are kept when it saves. Both files now carry a `"format"` number, and a taskman refuses a file of a newer format, leaving it as it is, instead of misreading it. Older releases still drop fields they don't know.
- A key typed after the terminal is resized now arrives at once. It used to wait for the next key (the Enter that sends a prompt, the `y` that answers a permission prompt), with taskman using most of a processor core meanwhile.
- In a project with a tracker, a project agent now hands work that changes code or files to a ticket session: it creates a ticket, adds it with `taskman add` and briefs its session, unless you ask it to make the change itself or its role says it may. It creates the ticket with `taskman add --new`, which now works on every tracker.
- `taskman add --new "Title"` creates the ticket in Linear, Jira and GitHub Issues too, with the project's account, then adds it: in Linear's team or Jira's project the ticket prefix names (Jira: as a Task; a project that requires other fields is refused with Jira's reasons), or in the project's GitHub repository. `--description TEXT` gives it a description, on Local files too. A create that may have reached the tracker but failed says the ticket may exist, so look before running it again.
- A worktree whose `git worktree add` was stopped partway (it ran past the 5-minute limit, or was killed) is removed and made again instead of used: it had every file staged for deletion, which a session's next commit could delete. With worktrees inside the repository, the `.git/info/exclude` line is now appended, so the file's other rules stay even when it isn't UTF-8.

## 0.1.8 - 2026-10-18

- The faint line between a project's agents and its tickets in the sidebar is gone: agents and tickets are spaced like any other cards again.

## 0.1.7 - 2026-10-18

- Reorder the sidebar. `ctrl+a }` and `ctrl+a {` move the selected agent or ticket down or up one place and start move mode, where `{`/`}` or ↑/↓ go on, `p` moves its project instead (every project shows folded meanwhile), Enter keeps the order and Esc puts it back. Or drag a card, or a project's header, with the mouse: a line shows where it would land, the sidebar scrolls at its edges, release drops and Esc cancels. A card stays among its project's agents or tickets, and the order is saved in the order the lists already had, so `ctrl+a 1-9` follow it. A project with both agents and tickets now shows a faint line between them. A click on a project's header folds it when the button comes up, not when it goes down.

## 0.1.6 - 2026-10-18

- The Add form (`ctrl+a n`) now opens on Type, set to Agent, so ←/→ switches to Ticket right away. The open issues are still fetched when it opens, ready under the Ticket field.
- `ctrl+a /` jumps to a ticket, agent or project by typing: the sidebar narrows to the cards whose key (or number), title, agent name or project name contain what you type, ↑/↓ move among them, Enter selects one and Esc leaves the selection as it was.
- A ticket's title in the sidebar is cleaned of control characters, as the panels' already were.

## 0.1.5 - 2026-10-15

- A session that won't resume because Claude Code runs its conversation as a background session (`claude --bg`) now keeps Claude's message on screen instead of flashing it, and Enter stops that copy with `claude stop` and resumes it in the pane. If the stop fails, the pane says so and gives the command to run yourself.

## 0.1.4 - 2026-10-15

- Double-click a word to select and copy it (a key, path, URL or id), or triple-click a line, in any pane. It behaves like a dragged selection: it stays until a click, a key or Esc, and `ctrl+c` copies it again.
- Sessions you haven't looked at for an hour now go to sleep to free their memory (a Claude Code session with its servers is a few hundred MB, and many open tickets could exhaust a small machine, WSL's by default). A sleeping session shows `☾` in the sidebar and wakes, on the same conversation, when you select it. Only idle sessions and ones waiting on your turn sleep, never the one in front, a project agent, one that's working or needs you, or one with a background task running (a dev server, a watcher). Settings → General sets the time (15 minutes to never) and an optional limit on how many sessions stay awake. `taskman status` says a session is asleep, since it can't be messaged until you select it.

## 0.1.3 - 2026-10-14

- Updated rustls, the TLS library taskman uses for HTTPS, to 0.23.46 for its security advisories (RUSTSEC-2026-0346 to 0349).

## 0.1.2 - 2026-10-12

- The account form shows the page that makes a token as its full address, on a line of its own, so terminals that don't open links can still open or copy it. Linear's now includes the workspace typed, which the page needs.
- A token variable that isn't set now names `~/.bash_profile` for bash on macOS, the file Terminal's bash reads, instead of `~/.bashrc`. The guide says how to find your shell's startup file.

## 0.1.1 - 2026-10-12

- Arrow keys now leave a form's list: in the Add form's open issues (and the project form's projects or teams), ↑ on the first entry goes back to what's typed and on up through the fields, instead of stopping there. The Add form's Ticket field and its list now come last, below CLI, Worktree and Base, so a long list never stands between you and the other fields; the form still opens on Ticket.
- Adding a project with Jira or Linear is one form that signs in first: enter the site or workspace, then a new account's email and token right there (no second form, no repeated provider or site), and taskman lists the Jira projects or Linear teams the account can see to pick the ticket prefix from. A saved account that fits is picked for you; a key can still be typed when the list can't load. A project's settings work the same way. GitHub Issues and Local files suggest a prefix, a board's from the ticket files it has.
- The account form can save a Jira or Linear token to your keychain in one step: on macOS and on Linux with a desktop keyring, paste it into the form and taskman keeps it in the macOS Keychain or Secret Service, saving only `keychain:jira-1`. Deleting the account offers to delete the token there too.
- Clearer first-run token setup: the account form no longer starts with `env:JIRA_API_TOKEN` filled in, links the page that makes a token, and says taskman never saves the token in its files. A variable that isn't set says what to add to which file, and a token command that fails points to the guide.

## 0.1.0 - 2026-10-12

- First public release. taskman is a terminal UI for working on several tickets at once: each ticket gets the real `claude` or `codex` CLI in its own git worktree, with its Jira, Linear, GitHub Issues or Local files issue and its GitHub PR beside it.
- The sidebar shows which session needs you, and project agents coordinate the tickets by messaging their sessions. Sessions read their ticket and PR with `taskman status`; `taskman add` and `taskman close` work from any shell.
- Prebuilt binaries for macOS (Apple Silicon and Intel) and Linux x86_64, an install script that checks the release's `SHA256SUMS` and, with the GitHub CLI, its build attestation, and updates in the background with the same checks.
