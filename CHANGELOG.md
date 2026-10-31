# Changelog

What changed in each release; a release's notes on GitHub are its section here. What's merged but not released yet is in [changelog.d/](changelog.d/), a file for each change, which the release moves in here. Versions follow Cargo's rule for 0.x: the patch number (0.1.1) for anything compatible, fixes and new features alike, and the minor (0.2.0) for breaking changes, such as changed keys, config or saved-state format, or a removed command.

## 0.1.36 - 2026-10-31

- `ctrl+a f` forks a session into a new agent that starts with a copy of its conversation so far, in the same CLI, while the original carries on; sessions ask for one with `taskman add --agent NAME --fork KEY`, which waits for your approval.

## 0.1.35 - 2026-10-28

- `ctrl+a Z` folds every project in the sidebar, or unfolds them all when they're all folded already.

## 0.1.34 - 2026-10-28

- `ctrl+a d` lists what the selected session did, a row per turn, newest first: how long ago, the files it changed or committed, its commits and the first sentence of the CLI's last message, for Claude Code and Codex sessions. Enter shows a turn's whole message, its Markdown as plain text; ←/→ show the next session's without closing, and `g` goes to it.
- The `ctrl+a` menu lists the everyday keys a row for each kind: New, Go to, This ticket, Open, Session, Look back (what the session did, its history, scrolling back), Panels and General, with `? help` in the footer. `ctrl+a ?` (Help) lists every key in the same groups and words; no key changed.
- `ctrl+a /` is now called find rather than jump, in the menu, Help and the box itself. While its box is open in the sidebar, the footer lists its keys, Esc to close first; the empty box says what it finds (a key, title, name or project). No key changed.

## 0.1.33 - 2026-10-25

- The footer counts what needs you apart from what finished (`▲ 1 needs you · ● 2 finished`), as `ctrl+a w` lists only the first and now says where the others are. On a ticket whose tracker or PR panel can't show, the first `ctrl+a j` or `ctrl+a g` says why instead of silently turning the panel off. Settings' first item is called General, and Help lists everything in Settings and points to Help & feedback.
- Closing a ticket, or deleting its project, now deletes the copy of its session's history that `ctrl+a v` saved, and its owner file, from taskman's state folder; they used to stay forever.
- In a repository with no remote, closing a ticket no longer says the worktree has "unpushed" commits it never had: a commit counts once it's on the branch the main checkout is on, so a worktree whose branch you merged into it is removed with its branch, and one that isn't says `it has 2 commits not on main (no remote)`.
- After `ctrl+a PgUp`, PgUp/PgDn, ↑/↓ and Home/End keep scrolling the session's history without the prefix, and Esc or typing returns to the bottom; a page is now the pane's height less a line, how far back you are shows in the header instead of over the session's top row, and a scrolled-back session keeps its place when the pane changes size.
- `ctrl+a /` works with the sidebar folded, at any window width: the jump box opens over the screen, listing the matching cards with their project, title and state.
- On a light terminal, the selected card's name, focused labels, titles and the other cyan text are a dark teal, readable on the light selection, instead of cyan, which was too faint. On a darker light grey, or when the terminal doesn't say its background, they're navy.
- In a narrow window the footer's waiting count drops the finished turns, then its words (`▲ 2`), rather than disappearing, and `ctrl+a ctrl+a` no longer picks a session handed back to taskman over one that needs you.

## 0.1.32 - 2026-10-25

- Only the session you're looking at can set your clipboard: a copy from a session in the background is dropped, with a warning to select it and copy again.
- taskman warns, naming the session, when a session asks to read your clipboard (it never passes a read on, nor answers it) or copies more than about 750 KB (over taskman's clipboard limit, so not copied). A session's copy keeps the selection it named, such as the primary one. Session titles are cleaned of characters that hide text (thanks, XdartMc).

## 0.1.31 - 2026-10-24

- When `gh` doesn't answer within 30 seconds (a keyring locked at login, say), a project keeps the GitHub panel it had, now and at the next start, and Settings says the answer is from before until `ctrl+a r` gets a new one. A project with no GitHub remote keeps saying so, rather than that gh didn't answer.
- Since 0.1.30, only taskman downloaded from a release updates itself. One built from source (`cargo build` and copied into `~/.local/bin`, say, or `cargo install --root ~/.local`), or installed by a package manager that builds it, only announces new releases, and `taskman uninstall` leaves it: update it the way it was installed, or switch to a release with the README's quick install. `taskman update` now says so in those words, rather than telling a package's user to `git pull` and rebuild.
- A command started with a signal ignored keeps ignoring it, as before 0.1.30: `nohup taskman update`, or `taskman status &` from a script, carries on when the terminal closes or `ctrl+c` is pressed, instead of ending.
- With a state folder shared between machines (a network folder), a second taskman on one machine no longer starts beside one running on the other, and says which machine that is; `taskman add` and `taskman close` there wait for the running one's reply rather than saying taskman isn't running.

## 0.1.30 - 2026-10-24

- On a project with a tracker, the Add form's Name hint says to set Type to Ticket for a ticket: a title typed while Type is Agent is the new agent's name, on Local files too.
- The install script's advice for putting taskman on your PATH now works for fish (`fish_add_path`) and for bash on macOS (`~/.bash_profile`, not `~/.bashrc`), so `taskman` is found in a new terminal.
- `ctrl+c` on a command such as `taskman status`, `taskman add` or `taskman update` now stops the `gh`, git and token commands it started, instead of leaving them running for up to their time limit; a SIGTERM or a closed terminal does the same. An interrupted `taskman update` no longer leaves the new binary's temporary copy beside the installed one. The command still ends as interrupted (exit status 130 for `ctrl+c`), so a script looping over commands stops. `taskman resume` leaves `ctrl+c` to the CLI it hands the terminal to, as before.
- A taskman you built from source is now a development build wherever its build folder is, so `taskman update` no longer replaces it, nor `taskman uninstall` removes it, when `CARGO_TARGET_DIR` names a folder other than `target`.
- When `taskman close` closes a ticket whose worktree is still being created, the footer keeps saying whether the worktree was removed or kept once git finishes, instead of that note being replaced at times by "Closed ENG-12".

## 0.1.29 - 2026-10-23

- taskman refuses a command it doesn't know (`taskman stauts`) with exit code 2 and says `taskman --help` lists them, instead of starting; `taskman help` is `taskman --help`.
- Started straight after it was killed (`kill -9`, out of memory) or crashed, taskman starts and says the last one didn't quit cleanly, instead of saying another taskman is already running; one that's still running is refused as before, even when the state folder's `lock` file was removed.
- With no terminal (`ssh host taskman` without `-t`), taskman says it needs one instead of "No such device or address", and a tickets.json that isn't text gets the same "move it aside" advice as other unreadable ones.
- When a tracker can't be reached, its panel says why in plain words (no network, a name that doesn't resolve, no answer within 20 seconds, a failure on the tracker's side) and that taskman tries again, instead of the HTTP library's error. A `gh` that doesn't answer within 30 seconds is no longer reported as logged out, and helpers still running when taskman quits (gh, git, a token command) are stopped with it.
- Idle, taskman wakes about 5 times a second instead of 20, and uses less processor time, which helps a laptop's battery: replies from its background work wake it as they come, rather than it checking for them ten times a second.
- Reading a session's history (`ctrl+a l`, `ctrl+a v`, scrolling back) no longer keeps it at the pane's full width afterwards: one read took a session with 10,000 rows from 21 to 51 MB for good, and now leaves it at 21 MB.
- An emoji widened by U+FE0F (⚠️ ❤️ ✔️) printed in a session's bottom-right cell no longer stops the session with "taskman stopped reading this session after an error" when the row above is printed to next.
- Quitting while taskman's own git is running (checking a worktree before removing it, say) no longer leaves an `index.lock` behind that makes your next commit there fail: git is asked to stop and cleans up, and its read-only checks take no lock at all.

## 0.1.28 - 2026-10-22

- A ticket's new comments are counted in plain words: `3 new comments` on its sidebar card, and on an agent's board by where they are, naming the tracker: `2 new Jira comments`, `1 new PR comment`, or `2 new Jira comments, 1 on the PR`. With the tracker panel's comments hidden, a line where they'd be says how many there are and that `ctrl+a c` shows them.
- `taskman status` now also strips variation selectors and zero-width characters, which can hide instructions in a title or comment, keeping the ones emoji need. A newline in a Local files file or column name no longer starts a line of its own in `taskman status` or `taskman move`.
- A session waiting on Claude Code's or Codex's question about trusting its folder now shows as needing you, in the sidebar, the waiting list and notifications, instead of idle. A Codex approval request in a session continued with `taskman resume` shows as needing you again, instead of working until the next hook.
- On Linux and WSL, taskman uses less processor time while a spinner shows or the selected session prints: it no longer polls the terminal over and over in the millisecond before each frame. Moving the mouse over taskman no longer redraws the screen.

## 0.1.27 - 2026-10-22

- The dialog that approves an agent from `taskman add --agent` keeps its keys on screen on small terminals: a long role scrolls with ↑/↓ and says how many lines are hidden. Every line of the role has a `│` bar in front, so none can pass for taskman's own text, and approving after the command stopped waiting no longer says it was added "while taskman was closed".
- `taskman add --new` run by a session no longer creates the ticket by itself: taskman shows you the title and description and creates the ticket only when you approve, so text a session read can't have it publish a secret as an issue. The command waits up to a minute and prints the new key, or says you declined. From your own shell, `--new` creates the ticket at once as before.
- In a short window, such as 80x24, the Settings and Add forms scroll to keep the field you're on in view, and their error and keys stay on screen: the Jira and Linear forms of a first run no longer type into fields you can't see. In a narrow one, each value and its hint go under the label instead of being squeezed beside it.
- An empty or relative `XDG_STATE_HOME` or `XDG_CONFIG_HOME` is ignored, as the XDG spec says, instead of putting taskman's folder in the current directory, where `taskman uninstall --purge` could then remove a folder of yours named `taskman`. `taskman uninstall --purge` also refuses a relative `TASKMAN_CONFIG`, and removes only full paths.
- Saving tickets.json or config.json through a link (stow, home-manager) writes the file the link leads to and keeps the link, and a save keeps the file's permissions: a config.json kept readable by you only stays that way.
- git, gh and token commands taskman runs can no longer reach its terminal. Before, ssh asking for a key's passphrase during a worktree's fetch printed its prompt over the screen again and again, took the keys you typed and used a full core until the time limit; now it fails at once, and the message says to add the key to ssh-agent with `ssh-add`.
- Selecting a ticket marks its new comments and reviews read, whatever panels show; they keep their `●` until you leave it, and `ctrl+a m` takes it off. The sidebar card says where they are (`2 new on issue`, `1 new on PR`), and a comment once read no longer comes back as new after a refresh.
- A session whose worktree was removed by hand, or whose project folder was moved, no longer starts in your home folder. Its pane and a toast say which folder is gone; Enter makes the worktree again on the ticket's branch, and `taskman resume` refuses too. A moved project folder no longer turns its PR panels off as "gh isn't installed".
- `taskman add` and `taskman close` no longer say "taskman isn't running" now and then while it is. They checked how old taskman's heartbeat file was by the wall clock, and a clock that steps forward, as WSL's does every half minute or so, made a running taskman look closed. They now check the lock taskman holds while it runs.

## 0.1.26 - 2026-10-21

- Settings → Help & feedback: each link's hint now lines up with the link above it, and the security link's label is just Security.
- Text fields in forms have a cursor: ←/→ move it, Home and End go to the ends, ctrl+←/→ (or alt+←/→) move a word, Delete deletes after it and ctrl+w the word before it, and typing and pasting go in where it is. Branch's presets are listed under it while it's focused, to pick with ↑/↓ and Enter, rather than cycled with ←/→.

## 0.1.25 - 2026-10-21

- Settings has a Help & feedback page: it opens the guide, the bug and feature forms, and a private security report on GitHub. The bug form comes filled in with the taskman, Claude Code or Codex versions, the OS and the terminal. Over SSH, or with no browser, it copies the link instead.

## 0.1.24 - 2026-10-20

- The sidebar no longer numbers its first nine cards, and `ctrl+a 1-9` no longer switches to them: the names get the room instead. Switch with `ctrl+a /` (jump to a card by name or key), `ctrl+a [` / `]` or a click.
- A Codex session whose automatic reviewer ("Approve for me" in `/permissions`) is deciding an approval now shows as working, with no bell or notification; only a prompt Codex puts to you shows "needs you", up to a second after Codex reports it.
- Updated rustls-webpki and webpki-roots for two certificate advisories (RUSTSEC-2026-0381, RUSTSEC-2026-0382), which could let a distrusted intermediate certificate be accepted on taskman's own connections to Jira, Linear and GitHub releases.

## 0.1.23 - 2026-10-20

- `taskman uninstall` removes taskman in one step: the program, and when you say so (`--purge`, or yes to its second question) your projects, accounts, saved state and the tokens taskman saved in your keychain or its token folder. It lists everything first, refuses while taskman runs, and leaves your repositories, worktrees and branches alone, listing the worktrees taskman made with the command to remove each.

## 0.1.22 - 2026-10-20

- A git, gh or token command that runs past its time limit now stops with everything it started (ssh, a credential helper) also where there's no `kill` program, as in slim container images. Before, what it started could keep running, and keep writing into a worktree taskman had already reported as failed.
- Scrolled back to the oldest row a session's pane still holds, once older rows have dropped off its history, the pane says which key opens the CLI's full transcript: `ctrl+o` in Claude Code, `ctrl+t` in Codex.
- taskman draws only when the screen changes: other sessions' output no longer redraws it, and an idle taskman draws once a second instead of ten times. With nine busy sessions in the background it takes about a third of the processor time it did.

## 0.1.21 - 2026-10-20

- Every dialog, form and settings page shows its keys the same way, each key in bold, and a row of keys too long for the screen breaks between keys instead of in the middle of one. The notices list on a small screen no longer cuts its keys off.
- Titles and names in Chinese, Japanese or Korean, or with emoji such as ⚠️, are cut to the columns they take on screen: they end with `…` at the edge of the sidebar, panels and forms instead of being clipped, the counts and providers in Settings stay in view, and `taskman status` keeps its columns in line.

## 0.1.20 - 2026-10-20

- For contributors: a pull request now notes its change in a file of its own under `changelog.d/` instead of a line under "Unreleased" in CHANGELOG.md, so pull requests no longer conflict there. `scripts/release.sh` moves the files into the version's section, in the order they were merged.
- The agents' board shows a ticket's latest review as the PR panel does: a dismissed review as dismissed rather than as a comment, and a comment with 💬. A PR or review state GitHub adds later shows by its own name rather than passing for open or for a comment.

## 0.1.19 - 2026-10-20

- In a session taskman started, `taskman status`, `taskman add` and `taskman move`, which sessions run without asking, now keep to the session's own project: another project's ticket, by its prefix or `--project`, is refused, so text a session read in one project can't have it read another project's tracker, or open sessions there, unasked. The error says how to go ahead: the same command with `env -u TASKMAN_REPO` in front, which Claude asks you about first. From your own shell, any project works as before.
- When a session starts needing you while you're away from taskman (the terminal says it lost the focus), taskman shows a desktop notification saying which ticket and why: the terminal's own (OSC 9) in iTerm2, WezTerm, Ghostty and kitty and over SSH, otherwise `osascript` or `terminal-notifier` on macOS, a Windows toast on WSL and `notify-send` on Linux. One per wait. Settings → General → Notify when you're away chooses the way or turns them off; inside tmux, turn on `focus-events` (see [Notifications](docs/guide.md#notifications)).
- A worktree taskman was creating when you quit, and git finished afterwards, is still taskman's: closing its ticket offers to remove it, as for any worktree taskman made. A worktree you made yourself in the ticket's folder, on its branch, is still never offered for removal.
- PR panels cost less again: a ticket's PR details (checks, reviews, comments and review threads) come in one GitHub query instead of two `gh` calls, and a ticket in the background fetches them only when the PR changed: a push, a comment or review, a new state, a conflict with its base appearing or going, or its checks passing or failing. With ten tickets in the background that's one `gh` process a minute instead of 21. The ticket on screen still fetches them on each refresh, and `ctrl+a r` too.

## 0.1.18 - 2026-10-20

- The list of recent footer messages moved from `ctrl+a L` to `ctrl+a ~` (tmux's key for its messages; `l` and `L` were easy to mix up) and is called notices: it opens with a line saying what it holds, and the ctrl+a menu, Help and the short-window pointer (`ctrl+a ~ lists it`) say so. The menu's `w` reads "waiting on you".
- `ctrl+a R` asks before restarting a session that's working or waiting on you, since the restart stops its turn part-way, or leaves its question unanswered; Enter restarts, Esc keeps it running. An idle or ended session restarts at once, as before.
- If taskman hits a bug while fetching a ticket or its PR, creating a worktree, testing a connection, listing issues or reading a token, it now says so in a red message pointing to `panics.log`, and the panel or session shows the error, instead of loading forever until you restart. `ctrl+a r` fetches again, and Enter retries the worktree.
- PR panels cost less: each refresh finds the PRs of all of a project's tickets with one GitHub query instead of one or two `gh` calls per ticket. With ten tickets in the background that's one `gh` process a minute instead of ten to twenty, each about 130 ms of CPU. The PR found and shown is the same.

## 0.1.17 - 2026-10-19

- Closing an overlay (help, the quit dialog, the links list) over an emoji such as ⚠️, or a session redrawing the text beside one, no longer leaves a stale character next to the emoji or moves the text after it a column right. Seen in tmux 3.4 and Windows Terminal; terminals that draw ⚠️ one column wide show it the same way.

## 0.1.16 - 2026-10-19

- `ctrl+a w` lists every session waiting on you, oldest first, with how long it has waited, why (permission or question) and what it asks for: for a permission prompt the tool and its command, as the Claude Code or Codex hook reports it. Enter goes to that session. A session that has waited a minute shows its time in the sidebar (`▲ 4m`) in place of "needs you". Sessions started by an older taskman show the prompt's message until their next prompt.
- Messages in the footer have a level. Errors (a worktree that couldn't be created, a file that couldn't be written, a session that didn't start) are red and stay until your next key, and a later message doesn't replace them; warnings are yellow and, like confirmations, clear after 4 seconds. The count of sessions waiting on you stays beside the message instead of being covered, and a long message wraps onto the blank row above the footer (or ends with `ctrl+a L lists it` on a short window).
- `ctrl+a L` lists the last 200 messages, newest first, with how long ago each came, so one that went by while you were typing into a session can be read again.
- A session that clears its history (`ESC[3J`, which `clear` sends in most shells) now loses it in the pane too, as in a terminal. If you're selecting text while it happens, the selection ends with a toast instead of copying the wrong rows.
- Making a session's pane shorter pushes its top rows into the history, and making it taller pulls them back, as in other terminals, rather than dropping rows at the bottom.
- A character that Unicode counts as three columns wide (U+17D8) no longer hides the text after it on its row; it shows as `�`, and selecting and copying still give the character.
- The terminal library, vt100, is now 0.18, with our changes on top of it.

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
