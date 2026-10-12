# Changelog

What changed in each release; a release's notes on GitHub are its section here. Versions follow Cargo's rule for 0.x: the patch number (0.1.1) for anything compatible, fixes and new features alike, and the minor (0.2.0) for breaking changes, such as changed keys, config or saved-state format, or a removed command.

## Unreleased

- Arrow keys now leave a form's list: in the Add form's open issues (and the project form's projects or teams), ↑ on the first entry goes back to what's typed and on to the field above, and ↓ past the last entry goes to the field below, instead of stopping there.
- Adding a project with Jira or Linear is one form that signs in first: enter the site or workspace, then a new account's email and token right there (no second form, no repeated provider or site), and taskman lists the Jira projects or Linear teams the account can see to pick the ticket prefix from. A saved account that fits is picked for you; a key can still be typed when the list can't load. A project's settings work the same way. GitHub Issues and Local files suggest a prefix, a board's from the ticket files it has.
- The account form can save a Jira or Linear token to your keychain in one step: on macOS and on Linux with a desktop keyring, paste it into the form and taskman keeps it in the macOS Keychain or Secret Service, saving only `keychain:jira-1`. Deleting the account offers to delete the token there too.
- Clearer first-run token setup: the account form no longer starts with `env:JIRA_API_TOKEN` filled in, links the page that makes a token, and says taskman never saves the token in its files. A variable that isn't set says what to add to which file, and a token command that fails points to the guide.

## 0.1.0 - 2026-10-12

- First public release. taskman is a terminal UI for working on several tickets at once: each ticket gets the real `claude` or `codex` CLI in its own git worktree, with its Jira, Linear, GitHub Issues or Local files issue and its GitHub PR beside it.
- The sidebar shows which session needs you, and project agents coordinate the tickets by messaging their sessions. Sessions read their ticket and PR with `taskman status`; `taskman add` and `taskman close` work from any shell.
- Prebuilt binaries for macOS (Apple Silicon and Intel) and Linux x86_64, an install script that checks the release's `SHA256SUMS` and, with the GitHub CLI, its build attestation, and updates in the background with the same checks.
