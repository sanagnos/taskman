# Security

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub: on the repository's **Security** tab, choose **Report a vulnerability**. Don't open a public issue, pull request or discussion for it.

Include what you found, how to reproduce it, the taskman version (`taskman --version`), your OS, and which CLI was involved (`claude --version` or `codex --version`). We aim to acknowledge a report within a week and to agree on a fix and a disclosure date with you. taskman is maintained in spare time, so there's no bounty.

## Supported versions

Only the latest release gets fixes. taskman updates itself (see below), so most installs are on it within an hour of a release.

## What taskman trusts

taskman runs as you. The sessions it starts are the real `claude` and `codex` CLIs, with your permissions, your environment and your own CLI settings; taskman adds to them but doesn't sandbox them. Their own permission prompts and sandboxes are what stand between a session and your machine. What taskman itself is responsible for:

### Tracker and PR text is untrusted

On a public repository anyone can open an issue or comment on a PR, and on a shared tracker many people can write. Sessions act on what they read, so that text could try to instruct a session (prompt injection) or reach a terminal as escape codes.

- **Briefs hold no tracker or PR text.** A session's startup instructions name its ticket's key and URL, and an agent's list tickets by key, worktree, branch and PR state; titles, descriptions and comments stay out.
- **`taskman status` labels and cleans it.** What it prints from the tracker and GitHub sits under a note saying it's data, not instructions, with terminal control characters removed (so text can't set your clipboard with OSC 52 or spoof a link with OSC 8), the characters that hide text from a reader removed too (Unicode tag characters and bidirectional overrides, which can carry instructions you wouldn't see), inline fields kept to one line and bodies indented. The panels draw it without control characters too.
- **Local files tickets follow no links.** A repository can't point a ticket at a file elsewhere on your disk: only regular files are read, and the board and its columns are real folders, not links.

### What a session can run without asking

taskman gives Claude sessions permission (through `--settings`, for that session only) to run three of its own commands without a prompt:

- `taskman status`: reads the ticket and PR.
- `taskman add`: adds tickets. Adding an **agent** or changing an agent's **role**, which becomes part of a session's system prompt, waits for you to approve it in taskman, so text a session read can't create one unattended.
- `taskman move`: moves a Local files ticket between columns.

`taskman close` is deliberately not allowed: a session asks you before closing tickets or removing worktrees. Codex sessions get no allowances; Codex's own sandbox asks.

### Tokens are never stored

An account's token field says where to read the token, never the token itself:

- `env:VAR` reads an environment variable. Sessions inherit taskman's environment, so every session can see that variable, as can anything else you run from that shell.
- `keychain:NAME` reads taskman's item `NAME` (the account's id) under the service `taskman` in the macOS Keychain or the Linux Secret Service, through `security` or `secret-tool`, as `cmd:` runs a command. The account form writes it there when you paste a token: the token goes to the program on its standard input, never on its command line, where other users' `ps` could see it, and is kept in memory only until it's written, then cleared from the form. A session runs as you, so it could read the item the same way: macOS lets `security` read an item it saved without asking, and an unlocked Linux keyring doesn't ask either.
- `cmd:COMMAND` runs a command (a password manager, say) through `sh`, as you, without a terminal and with a time limit. The token isn't in the sessions' environment, so `cmd:` and `keychain:` are the ones to use to keep a token out of what sessions see by default. A session runs as you, though, so it could run the same command; a password manager that asks before handing a secret out is what stops that.

The account form refuses a source that is none of these, before testing or saving anything.

The config file (`~/.config/taskman/config.json`, or `TASKMAN_CONFIG`) and the state folder (`~/.local/state/taskman`) are created readable by you only.

### Updates

taskman updates itself: at every start and every hour it looks up the latest GitHub release of its repository, downloads it over HTTPS, and installs it beside the running binary. A download is installed only if:

- it matches its entry in the release's `SHA256SUMS` (a release without one installs nothing), and
- the new binary runs and reports the release's version.

Only then is it renamed over the old one, so a failed or broken download leaves the working copy in place. It applies at the next start; taskman never restarts itself. The checksum comes from the same release, so it catches broken and truncated downloads, not a compromised release; the release workflow guards that, with pinned actions and a write token only in the job that runs no project code. The install script (`scripts/install.sh`) makes the same checks.

What a checksum can't show, an attestation can: each release's archives and `SHA256SUMS` carry a GitHub artifact attestation, a signed record that this repository's release workflow built the file from the release's tag. When the GitHub CLI (`gh`) is installed and logged in, taskman and the install script verify it (`gh attestation verify`, pinned to the release workflow) and install nothing if it fails. Every release from v0.1.0 on has one, so one from then on without an attestation is refused, as a replaced archive would have none. Without `gh`, or for a v0.0.x release, from before the repository was public, the checksum is the only check, and `taskman update` and the install script say so. You can verify a file you downloaded yourself:

```bash
gh attestation verify taskman-vX.Y.Z-<target>.tar.gz --repo sanagnos/taskman \
  --signer-workflow sanagnos/taskman/.github/workflows/release.yml
```

**`TASKMAN_INSECURE_SKIP_ATTESTATION=1`** skips that verification, for the install script, `taskman update` and the background updates. It's for a network that blocks GitHub's API or Sigstore, where the check can't run at all: an install then rests on the checksum alone, as it does without `gh`; `taskman update` and the install script warn each time, and Settings → Updates says so while it's set. A verification that fails because the attestation is wrong (its signature, its signer or the file's digest) never suggests it; only one that could be the network does.

To turn updates off:

- **Settings → Updates → "Only notify"** (`"auto_update": false` in the config file): taskman says when a release is out, and `taskman update` installs it.
- **`TASKMAN_NO_AUTOUPDATE=1`**: no checks at all.

Copies installed with `cargo install`, development builds and folders taskman can't write to never update themselves.

### One copy of a session

Two CLIs on one conversation would interleave their turns in one transcript. Whoever runs a session, taskman's pane or `taskman resume` in another terminal, holds a lock on a file beside its status file (outside taskman, the CLI holds it too), and the other side refuses to start it. The system drops the lock however the process ends, so a crash leaves nothing stale.

## Out of scope

- **Anything already running as you.** A process with your user's access can read taskman's state and config, write its inbox, or run the CLIs directly; taskman doesn't defend against it.
- **Bugs in Claude Code, Codex, `gh` or git.** Report them to their projects. That includes a session being talked into something through files, web pages or other text it reads: the label on tracker text helps the model, but only the CLI's own permission prompts stop it.
- **What you allow a session to do,** such as running in a mode that skips permission prompts.
