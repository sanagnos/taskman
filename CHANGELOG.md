# Changelog

What changed in each release; a release's notes on GitHub are its section here. Versions follow Cargo's rule for 0.x: the patch number (0.1.1) for anything compatible, fixes and new features alike, and the minor (0.2.0) for breaking changes, such as changed keys, config or saved-state format, or a removed command.

## Unreleased

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
