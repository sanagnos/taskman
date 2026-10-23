# Security

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub: on the repository's **Security** tab, choose **Report a vulnerability**. Don't open a public issue, pull request or discussion for it.

Include what you found, how to reproduce it, the taskman version (`taskman --version`), your OS, and which CLI was involved (`claude --version` or `codex --version`). We aim to acknowledge a report within a week and to agree on a fix and a disclosure date with you. taskman is maintained in spare time, so there's no bounty.

## Supported versions

Only the latest release gets fixes. taskman installs a release with the same minor version itself when `gh` can verify it (see below), so those installs are on a fix within an hour of its release; others get a notice to run `taskman update`.

## What taskman trusts

taskman runs as you. The sessions it starts are the real `claude` and `codex` CLIs, with your permissions, your environment and your own CLI settings; taskman adds to them but doesn't sandbox them. Their own permission prompts and sandboxes are what stand between a session and your machine. What taskman itself is responsible for:

### Tracker and PR text is untrusted

On a public repository anyone can open an issue or comment on a PR, and on a shared tracker many people can write. Sessions act on what they read, so that text could try to instruct a session (prompt injection) or reach a terminal as escape codes.

- **Briefs hold no tracker or PR text.** A session's startup instructions name its ticket's key and URL, and an agent's list tickets by key, worktree, branch and PR state; titles, descriptions and comments stay out.
- **`taskman status` labels and cleans it.** What it prints from the tracker and GitHub sits under a note saying it's data, not instructions, with terminal control characters removed (so text can't set your clipboard with OSC 52 or spoof a link with OSC 8), the characters that hide text from a reader removed too (Unicode tag characters, variation selectors and zero-width characters, which can carry instructions you wouldn't see, and bidirectional overrides; an emoji keeps its one selector and the joiners inside it), inline fields kept to one line and bodies indented. A Local files ticket's file and column names, which whoever commits to the repository chooses, are printed on one line too, in `taskman status` and `taskman move`. The panels draw it without control characters too.
- **Local files tickets follow no links.** A repository can't point a ticket at a file elsewhere on your disk: only regular files are read, and the board and its columns are real folders, not links.

### What a session can run without asking

taskman gives Claude sessions permission (through `--settings`, for that session only) to run three of its own commands without a prompt, each kept to the session's own project:

- `taskman status`: reads the project's tickets and their PRs, with the project's own tracker account.
- `taskman add`: adds tickets. Adding an **agent** or changing an agent's **role**, which becomes part of a session's system prompt, waits for you to approve it in taskman, so text a session read can't create one unattended. So does creating a ticket with **`--new`**: its title and description are published in the tracker under your account, so taskman shows you both in full and creates nothing until you approve, so text a session read can't have it post a secret, such as a `.env` file, as an issue.
- `taskman move`: moves a Local files ticket between columns.

Another project's ticket, board or repository is refused in a session, by `--project` or by the key's prefix, so text a session read in a public project can't have it read a private project's tracker with that project's account, or open sessions there, without asking you. Claude's rules match a command's text and can't tell its arguments apart, so taskman checks this itself, by the project in the session's `TASKMAN_REPO`. To go beyond it, the session runs the command with `env -u TASKMAN_REPO` in front, which no rule allows, so Claude asks you first; from your own shell any project works as before.

`taskman close` is deliberately not allowed: a session asks you before closing tickets or removing worktrees. Codex sessions get no allowances; Codex's own sandbox asks. The project check applies in Codex too, in case you let a command through for good.

### Tokens are never stored

An account's token field says where to read the token, never the token itself:

- `env:VAR` reads an environment variable. Sessions inherit taskman's environment, so every session can see that variable, as can anything else you run from that shell.
- `keychain:NAME` reads taskman's item `NAME` (the account's id) under the service `taskman` in the macOS Keychain or the Linux Secret Service, through `security` or `secret-tool`, as `cmd:` runs a command. The account form writes it there when you paste a token: the token goes to the program on its standard input, never on its command line, where other users' `ps` could see it, and is kept in memory only until it's written, then cleared from the form. A session runs as you, so it could read the item the same way: macOS lets `security` read an item it saved without asking, and an unlocked Linux keyring doesn't ask either.
- `cmd:COMMAND` runs a command (a password manager, say) through `sh`, as you, without a terminal and with a time limit. The token isn't in the sessions' environment, so `cmd:` and `keychain:` are the ones to use to keep a token out of what sessions see by default. A session runs as you, though, so it could run the same command; a password manager that asks before handing a secret out is what stops that.

Where there's no keychain (WSL, a server), the account form can instead write a pasted token to a file only you can read, `tokens/<account id>` beside config.json (`~/.config/taskman/tokens/jira-1`), and save `cmd:cat '<that path>'` as where to read it, so config.json still holds no token. The folder is created 0700 and the file 0600, both with that mode from the start; the token is written to a temporary file in the same folder and renamed over the old one; a link or anything but a folder or file in their place is refused, as is a folder that others can open or that isn't yours. Errors never repeat the token. A session runs as you, so it could read the file, as it could run the command or read the keychain item.

The account form refuses a source that is none of these, before testing or saving anything.

The config file (`~/.config/taskman/config.json`, or `TASKMAN_CONFIG`) and the state folder (`~/.local/state/taskman`) are created readable by you only. A save keeps the file's permissions, and writes through a link to the file it leads to rather than replacing the link. An empty or relative `XDG_STATE_HOME` or `XDG_CONFIG_HOME` is ignored, so neither folder is ever one in the folder taskman runs from, and `taskman uninstall --purge` removes only full paths.

### Updates

taskman updates itself: at every start and every hour it looks up the latest GitHub release of its repository, downloads it over HTTPS, and installs it beside the running binary. A download is installed only if:

- it matches its entry in the release's `SHA256SUMS` (a release without one installs nothing),
- its attestation verifies (below), and
- the new binary runs and reports the release's version.

Only then is it renamed over the old one, so a failed or broken download leaves the working copy in place. It applies at the next start; taskman never restarts itself. The checksum comes from the same release, so it catches broken and truncated downloads, not a compromised release; the release workflow guards that, with pinned actions and a write token only in the job that runs no project code.

What a checksum can't show, an attestation can: each release's archives and `SHA256SUMS` carry a GitHub artifact attestation, a signed record that this repository's release workflow built the file. taskman and the install script verify it with the GitHub CLI (`gh attestation verify`), which must find it signed by `.github/workflows/release.yml` run from a version tag or `main`, on a GitHub-hosted runner, and install nothing if it fails. gh reads the archive from a new folder only you can open. Every release from v0.1.0 on has an attestation, so one from then on without one is refused, as a replaced archive would have none. You can verify a file you downloaded yourself:

```bash
gh attestation verify taskman-vX.Y.Z-<target>.tar.gz --repo sanagnos/taskman \
  --cert-identity-regex '^https://github\.com/sanagnos/taskman/\.github/workflows/release\.yml@refs/(tags/v[0-9]+\.[0-9]+\.[0-9]+|heads/main)$' \
  --deny-self-hosted-runners
```

The background install takes nothing less:

- **Only a verified attestation.** Without `gh`, with `gh` logged out or too old for attestations, or with the check skipped, it doesn't install: the sidebar says to run `taskman update`, and Settings → General → Updates says why. Otherwise anyone able to publish a release, with a stolen token say, would have their binary installed on every such copy within the hour.
- **Only the same minor version.** Before 1.0 a new minor version is a breaking release (CHANGELOG.md), so v0.2.0 is announced, never installed in the background: Settings → General → Updates links its notes, and `taskman update` installs it.

`taskman update` and the install script, which you run, install on the checksum alone where `gh` can't check (without it, logged out, too old, or for a v0.0.x release, from before the repository was public), and say so.

**`TASKMAN_INSECURE_SKIP_ATTESTATION=1`** skips the verification for the install script and `taskman update`, and stops background installs. It's for a network that blocks GitHub's API or Sigstore, where the check can't run at all: an install then rests on the checksum alone, as it does without `gh`; `taskman update` and the install script warn each time, and Settings → General → Updates says so while it's set. A verification that fails because the attestation is wrong (its signature, its signer or the file's digest) never suggests it; only one that could be the network does.

To go back to an earlier release, set "Only notify" (below) and run the install script with `TASKMAN_VERSION=vX.Y.Z`.

To turn updates off:

- **Settings → General → Updates → "Only notify"** (`"auto_update": false` in the config file): taskman says when a release is out, and `taskman update` installs it.
- **`TASKMAN_NO_AUTOUPDATE=1`**: no checks at all.

Copies installed with `cargo install`, development builds and folders taskman can't write to never update themselves.

### One copy of a session

Two CLIs on one conversation would interleave their turns in one transcript. Whoever runs a session, taskman's pane or `taskman resume` in another terminal, holds a lock on a file beside its status file (outside taskman, the CLI holds it too), and the other side refuses to start it. The system drops the lock however the process ends, so a crash leaves nothing stale.

## Out of scope

- **Anything already running as you.** A process with your user's access can read taskman's state and config, write its inbox, or run the CLIs directly; taskman doesn't defend against it.
- **Bugs in Claude Code, Codex, `gh` or git.** Report them to their projects. That includes a session being talked into something through files, web pages or other text it reads: the label on tracker text helps the model, but only the CLI's own permission prompts stop it.
- **What you allow a session to do,** such as running in a mode that skips permission prompts.
