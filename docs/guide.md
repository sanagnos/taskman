# taskman guide

Everything beyond the [README](../README.md)'s quick start: setting projects and accounts up, the keys, worktrees and stacked tickets, agents, the `taskman` commands, Local files, Codex and how it all works. Why things are built the way they are is in [design.md](design.md).

- [Install](#install): [platforms](#platforms), [updates](#updates), [by hand](#from-a-release-by-hand), [from source](#from-source), [uninstall](#uninstall)
- [First run](#first-run) and [Set up](#set-up): projects, trackers and accounts
- [Use](#use): keys, [the prefix](#another-prefix), [panels](#panels), [copying](#copying-text), [links](#links)
- [Continuing a session in another terminal](#continuing-a-session-in-another-terminal-taskman-resume)
- [Worktrees](#worktrees), [stacked tickets](#stacked-tickets) and [closing tickets](#closing-a-ticket)
- [Agents](#agents)
- Commands: [`taskman status`](#sessions-read-their-ticket-taskman-status), [`taskman add`](#adding-tickets-from-a-shell-or-a-session-taskman-add), [`taskman close`](#closing-tickets-from-a-shell-or-a-session-taskman-close)
- [Local files](#local-files-tickets-without-an-account), [GitHub Issues](#github-issues-the-repositorys-own-issues) and [Codex sessions](#codex-sessions)
- [How it works](#how-it-works)

## Install

The [README](../README.md#install) has the quick install. The details:

### Platforms

Prebuilt releases cover macOS (Apple Silicon and Intel) and Linux on x86_64 with glibc 2.34 or newer (Ubuntu 22.04, Debian 12, Fedora 35, RHEL 9 and later; WSL). Elsewhere, such as Linux on ARM or Alpine, build it [from source](#from-source). There's no native Windows build: use WSL.

The install script picks the build for your machine and downloads the latest release into `~/.local/bin` (`TASKMAN_BIN_DIR` changes it). The download must match the release's `SHA256SUMS` and the new binary must run before it replaces an installed one; otherwise nothing changes. When the GitHub CLI (`gh`) is installed and logged in, the script also verifies the release's attestation (`gh attestation verify`), which shows the repository's release workflow built the archive, and installs nothing if that fails. Every release from v0.1.0 on has one, so a release from then on without one is refused too: its archive may have been replaced. Without `gh`, or for a v0.0.x release, from before the repository was public and so without one, it says the checksum was the only check. If verification fails because gh can't reach GitHub or Sigstore (a timeout, a connection error, a busy or rate-limited server), the error says so: try again later. On a network that always blocks it, `TASKMAN_INSECURE_SKIP_ATTESTATION=1` installs on the checksum alone and prints a warning that it did; it weakens the install to what it was without `gh`, so use it only there. It then tells you anything still missing (PATH, Claude Code). `TASKMAN_VERSION=vX.Y.Z` installs a specific release.

### Updates

taskman updates itself: it checks for a new release when it starts and every hour, installs it in the background with the same checks (it must match `SHA256SUMS`, pass `gh attestation verify` when `gh` can check it, and run), and the sidebar says `↑ restart to get vX.Y.Z`. The update applies the next time you start taskman (quit with `ctrl+a q`, then Enter; sessions resume); it never restarts on its own. To install only when you choose, set Settings → Updates to "Only notify"; the sidebar then says `↑ vX.Y.Z: run taskman update` (`↑ run taskman update` when the version doesn't fit), and once you have, `↑ restart to get vX.Y.Z`. `TASKMAN_NO_AUTOUPDATE=1` turns off checking altogether. A copy installed with `cargo install` doesn't update itself (cargo manages its folder): the sidebar says `↑ vX.Y.Z available`, and you update by running the same `cargo install` again. `taskman update` says so too, and replaces cargo's copy with the release only with `--force`. `taskman update` also says whether it verified the attestation, or why the checksum was the only check. As with the install script, a release from v0.1.0 on without an attestation is refused when `gh` can check. `TASKMAN_INSECURE_SKIP_ATTESTATION=1` skips the attestation for `taskman update` and the background updates too, as for the install script; `taskman update` warns that it did, and Settings → Updates says so while it's set.

### From a release, by hand

What the quick install does, step by step. Pick your build and download it with the release's checksums:

```bash
target=aarch64-apple-darwin     # Intel Mac: x86_64-apple-darwin; Linux or WSL: x86_64-unknown-linux-gnu
cd "$(mktemp -d)"
curl -fsSLO https://github.com/sanagnos/taskman/releases/latest/download/SHA256SUMS
archive="$(grep -o "taskman-v[0-9.]*-$target.tar.gz" SHA256SUMS)"
curl -fsSLO "https://github.com/sanagnos/taskman/releases/latest/download/$archive"
```

Check the download against its checksum, then unpack the binary into `~/.local/bin`. The archive also holds taskman's license files and `LICENSE-THIRD-PARTY`, the notices of the open-source code built into it, which `taskman --licenses` prints from any installed copy:

```bash
grep " $archive\$" SHA256SUMS | shasum -a 256 -c     # Linux: sha256sum -c instead of shasum -a 256 -c
mkdir -p ~/.local/bin && tar xzf "$archive" -C ~/.local/bin taskman
```

To check it was built by the repository's release workflow too, not only that it matches the release's checksum, verify its attestation with the GitHub CLI: `gh attestation verify "$archive" --repo sanagnos/taskman --signer-workflow sanagnos/taskman/.github/workflows/release.yml`.

Then check it runs with `taskman --version`. If the shell can't find it, put `~/.local/bin` on your PATH: `echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc` on macOS (`~/.bashrc` on Linux/WSL), and open a new terminal.

It updates itself like the quick install does (see [Updates](#updates)), or run `taskman update`. Your projects, tickets and accounts are kept.

### From source

Needs Rust 1.88 or newer (through rustup) and a C compiler for the build.

```bash
# macOS
xcode-select --install                     # skip if the command line tools are installed
brew install rustup && rustup default stable

# Linux / WSL (Ubuntu)
sudo apt update && sudo apt install -y build-essential
curl https://sh.rustup.rs -sSf | sh -s -- -y && . ~/.cargo/env

# Both
cargo install --locked --git https://github.com/sanagnos/taskman
```

Run the same command again to update. Or build from a clone: `cargo install --locked --path .`. taskman isn't on crates.io yet: it builds with a fork of `vt100` that keeps links, which a published crate can't depend on.

### Uninstall

While taskman runs, `taskman close --merged` closes every merged ticket and removes the worktrees taskman created for them where nothing would be lost. Then quit taskman (`ctrl+a q`, then Enter) so its sessions stop, and remove what it keeps:

- **The program:** `rm ~/.local/bin/taskman` for the install script's copy (or wherever `TASKMAN_BIN_DIR` put it; `command -v taskman` says where), or `cargo uninstall taskman-app` for a cargo install.
- **Accounts and settings:** `rm -r ~/.config/taskman` (`$XDG_CONFIG_HOME/taskman` if that's set), or the file `TASKMAN_CONFIG` names.
- **Projects, tickets and session state:** `rm -r ~/.local/state/taskman` (`$XDG_STATE_HOME/taskman` if that's set).
- **Tokens in the keychain,** saved through the account form: on macOS, `while security delete-generic-password -s taskman >/dev/null 2>&1; do :; done` deletes them one by one until none is left; on Linux, `secret-tool clear service taskman`.
- **Lines you added yourself** to your shell's startup file: an `export …_TOKEN=` or `export …_API_KEY=` line, and the PATH line the install script suggested (it never writes one).
- **Worktrees inside the repository:** when a project put them in the repository's folder, taskman added a `# taskman worktrees` line and the folder under it to `.git/info/exclude`; delete both if you like.

What stays, because it's yours: your repositories and branches, the worktrees taskman didn't remove (`git worktree remove <path>` from the repository, once you've kept what you need from them), and the sessions' conversations in `~/.claude` and `~/.codex`, which belong to Claude Code and Codex.

## First run

Start `taskman`, press `ctrl+a P` and enter the path of a folder you work in, a repository or not. Then press `ctrl+a n` to add an agent: a session in that folder. To work on tickets, connect Jira or Linear in the same form or later in the project's settings (`ctrl+a ,`): enter the site or workspace, sign in on the spot if you have no account for it yet, and pick the Jira project or Linear team from the list taskman finds. Or pick Local files to keep tickets as markdown files in the project, with no account (see [Local files](#local-files-tickets-without-an-account)), or GitHub Issues to work on the repository's own issues through `gh` (see [GitHub Issues](#github-issues-the-repositorys-own-issues)). Setup details are below.

The prefix is `ctrl+a` (`ctrl+a a` sends a literal one). Use tmux or screen with `ctrl+a`? Change it in Settings (see [Another prefix](#another-prefix)).

## Set up

Everything is set up in **Settings** (`ctrl+a ,`): the prefix key, updates, sleeping, projects and accounts are listed on the left, the selected one's settings on the right. ↑/↓ choose, Enter edits, Enter again saves, `ctrl+d` deletes, Esc goes back. `ctrl+a P` and `ctrl+a A` open it on a new project or a new account.

A **project** is one folder: usually a repository's main checkout, but any folder works. Adding one takes only its path. Without a tracker it has agents only; pick Jira or Linear under **Tracker** to add tickets, and the form asks, in this order, for the site, the account to sign in with, then the project or team its tickets are in. Local files asks only for its folder and prefix (see [Local files](#local-files-tickets-without-an-account)), and GitHub Issues only for the prefix (see [GitHub Issues](#github-issues-the-repositorys-own-issues)). The project's page also says whether its tickets get a PR panel and which GitHub repo it reads (`acme/web`), and if not, why: `gh` missing or logged out, no GitHub remote, or not a git repository. The repo is the one `gh` picks from the git remotes; in a fork with `origin` and `upstream` that is `upstream` unless you run `gh repo set-default` in the repo. Deleting a project closes its tickets; the folder, worktrees and branches stay.

- **Site** (Jira): `your-site.atlassian.net`. **Workspace** (Linear): the part after `linear.app/`.
- **Account:** the credentials the project uses. A saved account that fits (the same Linear workspace, or your Jira account) is picked for you; ←/→ picks another or **+ new account**. With + new account, the account's own fields show in the same form: **Email** (Jira), **Keep in**, and **Token** or **Read from** (see the accounts below). Saving the project saves the account too, once its connection test passes or you press Enter again to save anyway. The picker shows who each account is and a short source (`you@example.com · cmd:op`); two accounts that would look the same show their whole sources, cut in the middle to fit.
- **Project** (Jira) or **Team** (Linear): where the project's tickets are. Once the form can sign in, which it tries when you leave the token field (or right away with a saved account), it lists the Jira projects or Linear teams the account can see, with their keys and names. Type to narrow the list by key or name, ↑/↓ to pick (past the top, ↑ moves to the field above; past the bottom, ↓ moves to the next field, or stays on the last entry when this is the form's last field); the only one is picked for you. Its key is the **ticket prefix**: the letters before the number in ticket keys (`ENG` in `ENG-12`), a shortcut so typing `12` adds `ENG-12`, and what the Add form lists open issues by. Full keys and links work whatever it says. If the list can't load (no permission to list projects, network down), the field says why, and you can type the key by hand; a key the list doesn't have asks you to press Enter again to save it anyway.
- **Ticket prefix** (GitHub Issues, Local files): the same shortcut, typed, and suggested for you (see their sections).

Under **Worktrees**, a project with a tracker in a git repository says how its new tickets get their worktrees (see [Worktrees](#worktrees)):

- **New tickets:** create a worktree for each (the default), or don't. It is the Add form's default; you can change it per ticket there. If your workflow makes its own worktrees, set "Don't create" once here.
- **Branch:** `{tracker}` (the default) is the tracker's own name: Linear's branch name (what "Copy git branch name" gives, like `you/eng-52-fix-login`), or `ENG-52-fix-login` for Jira, whose links need the key in capitals, and GitHub Issues. Or your team's convention, with `{KEY}`, `{key}` and `{slug}` (the title): `feature/{KEY}-{slug}`. It must include the key, so the tracker and taskman can link the branch. ←/→ cycles presets.
- **Folder:** beside the repository (the default, `~/src/acme-web-ENG-52`), or inside it like Claude Code's own `claude --worktree` (`~/src/acme-web/.claude/worktrees/ENG-52`). Inside, taskman adds the folder to `.git/info/exclude` so it doesn't show as untracked; your `.gitignore` is left alone.

An **account** is a set of credentials: a provider, an email (Jira) and where the token comes from.

- **Jira:** an API token (id.atlassian.com → Security → API tokens) belongs to you and works on every site you can reach, so one Jira account can serve all your Jira projects.
- **Linear:** a personal API key (Settings → Security & access → Personal API keys) belongs to one workspace, so each workspace gets its own account. Saving one checks the key works and belongs to the workspace entered. A second workspace's suggested variable is its own (`env:LINEAR_API_KEY_<WORKSPACE>`) so two workspaces never share a key by accident.

taskman never saves a token in its own files, only where to read it from. On macOS, and on Linux with a desktop keyring, **Keep in** starts on **Keychain**: paste the token into **Token** (it shows as dots) and saving puts it in your keychain and saves only `keychain:jira-1`. The form forgets the token once it's there; to replace it, paste a new one on the account's Settings page. Pick **Variable or command** instead, or where there's no keychain (WSL, a server), and **Read from** says where to read the token: `env:JIRA_API_TOKEN` reads an environment variable (`$JIRA_API_TOKEN` works too and is saved as `env:JIRA_API_TOKEN`), `cmd:op read op://vault/jira/token` runs a command (a password manager, say). Read from refuses anything else, so a token pasted there is never saved; an account an older version saved with the token itself says so on its Settings page, and its tracker panels say what to change. A command runs once, in the background and without a terminal, so it can't ask for a password there (a password manager's own window is fine); after 60 s it gives up. Saving an account tests the connection (a Jira account against its site, or the project's). Accounts are added with a project, or on their own under Settings' **+ new account** (`ctrl+a A`); a saved account is edited or deleted on its own page there. An account a project uses can't be deleted until no project does; deleting one whose token is in the keychain offers `d` to delete the token there too. Accounts live in `~/.config/taskman/config.json`.

### Storing a token

Make the token first. Jira: [id.atlassian.com → Security → API tokens](https://id.atlassian.com/manage-profile/security/api-tokens), Create API token. Linear: Settings → Security & access → Personal API keys, at `https://linear.app/<workspace>/settings/account/security`, where `<workspace>` is the part after linear.app/ in your workspace's address. The account form shows the page's address, with the workspace you typed. Paste the token only into taskman's Token field, your keychain or your password manager, never into a chat with Claude: a chat would send it to the model.

- **Your keychain, through the account form,** the simplest: leave Keep in on Keychain and paste the token. taskman hands it to `security` (macOS Keychain) or `secret-tool` (Linux: GNOME Keyring or KWallet) on its input, never on a command line, where other programs could see it, and reads it back to check. It's saved as the item `taskman` / `jira-1` (the account's id), which Keychain Access or Seahorse shows. If macOS asks whether `security` may use the item, choose Always Allow. On Linux the keyring must be unlocked, as it is once you've logged in to the desktop; `secret-tool` comes in the package `libsecret-tools` on Debian and Ubuntu, and taskman offers the keychain only when it finds it working. WSL usually has none, so use one of the ways below there.
- **A password manager's command line,** if you use one: pick Variable or command, then Read from `cmd:op read op://Private/Jira/credential` for 1Password (the app's Copy Secret Reference gives the exact `op://` path), or `cmd:pass show taskman/jira` for `pass`. It must work without a terminal: a desktop app's unlock window is fine, a password typed in the terminal isn't.
- **An environment variable,** in a plain file: anything you run can read it, every session included. Add the line to your shell's startup file, creating it if it doesn't exist. `echo $SHELL` says which shell you use: for `/bin/zsh` the file is `~/.zshrc` (zsh is macOS's default since Catalina, and Oh My Zsh changes nothing, as `~/.zshrc` loads it); for `/bin/bash` it's `~/.bash_profile` on macOS and `~/.bashrc` on Linux and WSL:

  ```bash
  export JIRA_API_TOKEN='paste-the-token-here'
  ```

  Then open a new terminal and start taskman there: one opened before doesn't have the variable. Read from: `env:JIRA_API_TOKEN`. Keep that file out of any dotfiles repository you share. Replace `JIRA_API_TOKEN` with `LINEAR_API_KEY` (or `LINEAR_API_KEY_ACME` for a second workspace) for a Linear key.

taskman reads the token when it starts and again when you save the account. After changing a token in your password manager, save the account again (Settings, the account, Enter) and it's read anew; after changing an environment variable, quit taskman and start it from a new terminal, which has the new value.

## Use

Press `ctrl+a n` (just `n` while the list is empty) to add a ticket to the current project (the form opens on Agent; press ← or → on Type for Ticket): a number (`52` becomes `ENG-52`), a key, or a link. Its session starts in the ticket's own worktree (see [Worktrees](#worktrees)). In a project without a tracker, `ctrl+a n` adds an agent. From a shell, or from an agent, `taskman add 52` does the same without switching to it (see [taskman add](#adding-tickets-from-a-shell-or-a-session-taskman-add)). Every key goes to the session except after the `ctrl+a` prefix (and `ctrl+z`, which would suspend the session with no way to resume it, and `ctrl+c` or `Esc` while text is selected). Pressing `ctrl+a` shows a menu of the command keys; `ctrl+a ?` explains them. The ones you'll use most (with the default prefix):

The Add form opens on Type, set to Agent; ←/→ switches it to Ticket. The Ticket field is the last one, below Project, Type, CLI, Worktree and Base (an agent's Name is last the same way), and lists the project's open issues under it (they are fetched when the form opens, so they are ready), those assigned to you first, and narrows the list as you type a number or words of a title: on Jira and Linear the project or team whose key is the ticket prefix (the 100 most recently updated, not done), on GitHub Issues the repository's, on Local files the board's tickets not done. Enter adds the one issue that matches. ↑/↓ highlight an issue in the list, scrolling through more than five, and Enter adds the highlighted one; typing narrows the list again and drops the highlight. ↑ on the first issue goes back to what you typed, and ↑ again moves up to Base and the fields above without picking one; ↓ stops at the last issue, since nothing is below. Tab and Shift+Tab always move between fields. The Worktree and Base above follow the ticket typed or highlighted. Elsewhere in the form, or with nothing listed, ↑/↓ move between fields. A closed issue isn't listed, but its number or key adds it.

| Keys | Action |
|---|---|
| `ctrl+a 1-9`, `ctrl+a [` / `]` | switch ticket |
| `ctrl+a {` / `}` | move the selected agent or ticket up / down, then keep going (see [Sidebar](#sidebar)) |
| `ctrl+a ctrl+a` | jump to the next ticket waiting on you |
| `ctrl+a /` | jump to a ticket, agent or project by typing (see below) |
| `ctrl+a n` / `x` | add / close a ticket |
| `ctrl+a ,` | settings: prefix key, updates, projects and accounts |
| `ctrl+a P` / `A` / `z` | new project / new account / fold the current project |
| `ctrl+a j` / `c` / `g` / `b` | toggle tracker, comments, GitHub, collapse sidebar |
| `ctrl+a o` / `p` | open the ticket / PR in your browser (a Local files ticket opens in `$EDITOR`) |
| `ctrl+a r` / `m` | refresh now (and check GitHub again) / mark new comments read |
| `ctrl+a v` | open the session's history in `$EDITOR` |
| `ctrl+a l` | the ticket's links (issue, PR, checks), then the session's, newest first: Enter opens one, `c` copies it |
| `ctrl+a t` | continue the session in another terminal (see [taskman resume](#continuing-a-session-in-another-terminal-taskman-resume)) |
| `ctrl+a a` | send the prefix itself to the session |

### Jumping by typing

`ctrl+a /` puts a text box at the top of the sidebar, and the sidebar narrows to the cards that match what you type: a ticket's key (a bare number finds its key, so `52` finds ENG-52), a title, an agent's name, or a project's name, which shows all that project's cards, folded or not. It's a case-insensitive substring match, not a fuzzy one, over the tickets and agents taskman holds, not the tracker's other issues. ↑/↓ move the highlight, Enter selects the highlighted card as a click would, and Esc closes the box leaving the selection as it was. Either way the full sidebar comes back; this is a one-off jump, not a filter that stays on. With no match the sidebar says so and Enter does nothing. Nothing typed in the box reaches the session. It needs the sidebar showing, so unfold it with `ctrl+a b` or widen the window first.

### Another prefix

ctrl+a is also GNU screen's prefix, often tmux's, and start of line in Claude's prompt. Pick another under **Prefix key** in Settings (`ctrl+a ,`, then General), or set `"prefix"` in the config file; every hint follows it, and `a` after the prefix always sends the prefix itself. The choices, and what else uses them:

| Prefix | Also used by |
|---|---|
| `ctrl+a` (default) | GNU screen's prefix, often tmux's; start of line in Claude's prompt |
| `ctrl+b` | tmux's prefix; Claude runs a command in the background with it |
| `ctrl+]` | telnet's escape key; Claude reopens its latest artifact with it, where artifacts are on |
| `ctrl+g` | Claude opens the prompt in `$EDITOR` with it |
| `ctrl+space` | switches input source on macOS; some terminals don't send it |
| `ctrl+\` | free in Claude; a plain shell's quit key |

### Panels

The tracker panel shows for tickets in a project with a tracker, and the GitHub panel when the repository has a GitHub remote and `gh` is installed and logged in. One of them takes the column's full height; with neither, the session gets the width. On a narrow window the panels narrow, then the sidebar folds, then the panels hide, so the session keeps 60 columns (at 80 columns: a folded sidebar and the session). After you install `gh` or log in, `ctrl+a r` brings the GitHub panel up.

### Sidebar

The sidebar lists each project's agents and tickets, each with its session's state beside its name; a folded project's header shows `●` when one of its tickets needs you. A list longer than the screen scrolls: selecting a card out of view, by key, number or click, scrolls just enough to show it, and the mouse wheel over the sidebar scrolls the list without moving the selection; selecting the selected card again (its number, or a click on what shows of it) brings it back into view. The top and bottom rows then say what's hidden past them (`↑ 3 more`, `↓ 5 more`), with `●` when a hidden ticket needs you.

The order is yours to change. An agent moves among its project's agents, a ticket among its project's tickets, and a project among the projects; a card never crosses into another group or project. Press `ctrl+a }` or `ctrl+a {` to move the selected card down or up one place. That starts move mode, where `{` and `}` or ↑ and ↓ go on without the prefix, `p` switches to moving the card's project (every project then shows folded, so each step passes one project, and their folds come back afterwards), Enter or any other key ends it, and Esc puts the order back as it was before the first move. The footer says what is moving and which keys do what. A card stops at the edge of its group.

With the mouse, press on a card and drag it up or down. It becomes a drag only once the pointer leaves the card's rows, so a click selects as before. While you drag, the card stays where it is, dimmed, a line shows where it would land and the footer says where (`move ENG-52 below ENG-61 · Esc cancels`). The line snaps to the nearest place the card can go, wherever the pointer is. A long list scrolls while the pointer rests at its top or bottom edge. Release to drop; Esc cancels. Dragging a project's header moves the project, with every project drawn folded for the drag; a click on the header still folds it, on release.

The order is saved as it is: projects and cards in `tickets.json` keep the order they are listed in, so `ctrl+a 1-9` numbers follow it, and nothing else in the file changes.

### Sleeping sessions

Each session is a full Claude Code or Codex process, a few hundred MB with its servers, so many open tickets can use up a small machine's memory (WSL gets only part of it by default). A session you haven't selected for an hour is put to sleep: stopped, shown as `☾` in the sidebar, and started again on the same conversation when you select it, as after a restart. Only a session that is idle or waiting for your turn sleeps; never the one in front of you, a project agent, one that is working or needs you, one handed to another terminal (`↗ outside`), or one with a background task running, such as a dev server or a watcher (taskman looks at the processes under the CLI). A task started in the first half minute after the session starts is taken as the CLI's own. A turn you haven't looked at stays marked `● your turn` while the session sleeps.

**Sleep idle sessions after** in Settings → General sets the time: 15 minutes, 30 minutes, 1 hour (the default), 2 hours, 4 hours or never. **Keep awake at most** optionally limits the sessions running at once, 2 to 20: past it, the idle ones you selected longest ago sleep, whatever the time. A sleeping session can't be messaged: while a project agent runs, taskman starts its tickets' sessions so they can be messaged, but not sleeping ones, and `taskman status` lists them as asleep so the agent can say so. `ctrl+a t` and `taskman resume` continue a sleeping session in another terminal.

### Copying text

Drag over any pane, or double-click a word (a key, path, URL or id) or triple-click a line: a word runs to the next space, quote, bracket or `,` `;` `|` `<` `>`, without a full stop, comma or other sentence punctuation at its end, and clicks must come within 0.4 seconds on the same cell. The text is copied when you let go and stays highlighted until you click, press a key or `Esc`; meanwhile the pane holds still, so the session's output can't move it. To select more than the pane shows, drag past its top or bottom edge, which scrolls it, or use the wheel while dragging; the selection stays on its text as the pane scrolls. `ctrl+a v` opens the session's whole history in `$EDITOR` instead. A full-screen CLI (Codex, or Claude's full-screen mode) keeps its history itself, so only its screen can be selected. `ctrl+c` with text selected copies it again and clears it instead of interrupting the session (the next `ctrl+c` interrupts as usual). Inside tmux the text goes to tmux's buffer, which passes it on to your terminal; otherwise to `pbcopy`, `clip.exe` (WSL), `wl-copy` or `xclip`, and failing those to the terminal as OSC 52. The toast says which.

### Without colour

Set `NO_COLOR` to any value but an empty one ([no-color.org](https://no-color.org)) and taskman draws without colour, the sessions' output included. Bold, underline and reverse video still show, and what a background colour marked out (the selected ticket, the footer, a focused field, a toast) shows in reverse video instead, so selections stay visible.

### Links

Claude and Codex often show a link's text with the URL hidden behind it. taskman passes those links on to your terminal, so ctrl+click (cmd+click on macOS) opens them as anywhere else. While taskman has the mouse, some terminals want Shift as well: in WezTerm it's ctrl+shift+click. Over SSH they open on your own machine. Inside tmux, links need tmux 3.4 or later with its hyperlinks feature on (`set -as terminal-features ',*:hyperlinks'`).

The tracker and GitHub panels are linked the same way: the ticket's title opens the issue (Jira and Linear; a Local files ticket opens in `$EDITOR` with `o`), or its key on the border while the issue loads or can't be fetched, the PR's title opens the PR, each check opens its run, a reviewer's name opens their review, a thread's file and line open the thread, and http(s) URLs in descriptions, comments and reviews open themselves. On an agent's project board, a ticket's key opens its issue and a failing or running check its run.

`ctrl+a l` lists the selected ticket's links first (the issue, the PR, failing then running checks, reviews, threads, URLs in their text, passing checks), then the session's links and the plain URLs in its text, newest first, including ones scrolled away: Enter opens one in your browser (`$BROWSER`, else `wslview`, `explorer.exe` on WSL, `xdg-open` or `open`), `c` copies it. Use it in terminals without links, such as macOS Terminal. Only http(s) links are passed on or opened; others (`file:` paths, editor links) are listed, dimmed, so you can copy them.

### Links on WSL with Windows Terminal

WSL doesn't pass on `WT_SESSION`, so the CLIs don't know links are clickable and print them as plain text. The quickest fix, for Claude only (right here, since Windows Terminal supports links):

```bash
echo 'export FORCE_HYPERLINK=1' >> ~/.bashrc    # or to try it once: FORCE_HYPERLINK=1 taskman
```

For every tool, Codex too, run once in PowerShell: `setx WSLENV "WT_SESSION:WT_PROFILE_ID"`. Windows Terminal can keep running in the background (launch on startup, quake mode), so closing its windows isn't enough: end it in Task Manager, or sign out and in. `echo $WT_SESSION` in a new WSL tab should then print an id.

## Continuing a session in another terminal: `taskman resume`

For what the embedded terminal can't do (select a full-screen CLI's history, search with your terminal, a bigger window), hand the session off: `ctrl+a t` asks the CLI to quit, marks the ticket `↗ outside` and copies `taskman resume ENG-52`. Run it in any terminal: it continues the same conversation, started as taskman starts it, in the ticket's folder with its brief, hooks and name (if the pane's CLI is still quitting, it waits a moment for it). So the sidebar still shows its state (`⠋ working ↗`, `▲ needs you ↗`) and agents can still message it. Inside tmux, taskman offers to open it in a new window.

When you quit it there (`/exit`, `ctrl+d`, or closing the terminal), the ticket shows `↩ back`; press Enter on it to continue in taskman, with the turns from outside. Nothing comes back by itself. Only one copy runs at a time: `taskman resume` refuses while taskman runs the session, and taskman won't start one that runs elsewhere. If taskman can't take the lock that keeps it to one copy (its state folder isn't writable, say), the session doesn't start and the pane says why; fix it and press Enter. A session outside keeps running when taskman quits, and shows as outside again when it starts. It works for agents too (`taskman resume lead`), and for Codex as for Claude. `--project NAME` picks the project when the key is open in several, and `--agent` or `--ticket` picks one when an agent's name is also a ticket's key (`tester-1`); the copied command adds them when needed.

## Worktrees

Each ticket's session works in its own git worktree, so parallel sessions never edit the same files or switch each other's branch, and Codex's sandbox lets it write there without asking.

When you add a ticket that has no worktree yet, taskman creates one: it fetches `origin`, then makes the project's branch for the ticket from origin's default branch, so work starts from the latest main. A branch that already exists, locally or on origin, is checked out instead. The session's pane says "Creating a worktree…" for the few seconds it takes, and the session starts inside it. Its startup instructions name the worktree and branch, and ask for the PR from that branch with the key in its title.

- **Already has one:** a worktree whose branch has the key (`ENG-52-…`, or `change/ENG-52` from a workflow that makes its own) is used as it is; the Add form shows it. Once taskman has created a ticket's worktree, that one stays the ticket's, even if another with its key appears later.
- **Don't want one:** set Worktree to "Don't create" on the Add form; the session runs in the main checkout, as do agents.
- **No remote, or the fetch fails** (or takes over 30 seconds): the branch starts from your local `HEAD`, and a message says so.
- **The folder already exists** and isn't the ticket's worktree, or git fails: nothing is touched and the session doesn't start; its pane says why. Fix it and press Enter to try again, or press `ctrl+a M` to run the session in the main checkout instead. If taskman quits before the worktree is made, it carries on at the next start.

### Stacked tickets

When one ticket builds on another that hasn't merged yet, set **Base** on the Add form to that ticket (or `taskman add 62 --on 61`). The new worktree starts from the base's branch: the local one or origin's, whichever is newer. The new branch is still named by the project's format. Base applies only when taskman creates the worktree. The session is told to open its PR against the base's branch and to say in the PR that it depends on the base.

```
 2 ENG-62            your turn
   Add SSO
   #15 →ENG-61 ✓2              the PR goes into ENG-61's branch
   #15 →ENG-61 merged ✓2       ENG-61 merged but the PR still targets its branch: retarget it
```

The sidebar shows where a PR goes whenever that isn't the default branch, by ticket key when it's another ticket's branch. Merge the base first. With "Automatically delete head branches" on, GitHub moves the upper PR to main as the base merges, and the arrow goes away; the GitHub panel and `taskman status` then say "ENG-61 merged; PR now targets main". After a squash merge the upper PR still carries the base's commits, so rebase it onto main. taskman never rebases or retargets anything itself; tools like Graphite or git-spice can manage longer chains in the worktree.

### Closing a ticket

Closing a ticket (`ctrl+a x`) offers to remove the worktree taskman created for it, but only when nothing would be lost: no uncommitted, untracked or ignored files, and no commits that are neither on origin nor in its merged PR (commits made after the merge count as unpushed). Ignored files count because git would delete them with the worktree: a `.env`, a local database, `node_modules/`; untracked files count whatever `status.showUntrackedFiles` says, and so do edits to files marked assume-unchanged or skip-worktree, which `git status` doesn't show either. Caches that tag themselves with a `CACHEDIR.TAG` (Cargo's `target/`, pytest's, mypy's and ruff's caches) don't count, since their tools rebuild them, nor do the files macOS and Windows leave in folders (`.DS_Store`, `Thumbs.db`, `desktop.ini`), nor symlinks, which git deletes without touching what they point to. Press `r` to close and remove it, with its local branch if that's merged; Enter closes and keeps both. When ignored files are all that's in the way, the dialog lists them (`ignored: .env, data.db and 2 more`) and says that removing the worktree deletes them: `d` closes and deletes them with the worktree, Enter closes and keeps the worktree; ignored files that appear after the dialog opened keep it. Otherwise the dialog says what's there and keeps it, and when git can't check the worktree (it isn't a repository any more, say) it says why and keeps it too. A ticket whose session runs in another terminal (`taskman resume`) doesn't close until you quit it there. Removing stops the session, waits for its CLI to end and checks again first, so a commit made while the dialog was open keeps the worktree and the branch. It also claims the ticket's session, so nothing can start in the folder meanwhile; if that claim can't be made (the state folder can't be written, say), the ticket closes but the worktree stays, and the toast says why. Worktrees taskman didn't create, like your workflow's or your own, are never offered for removal, and the remote branch is left to GitHub. If you close a ticket while git is still creating its worktree, the worktree is removed the same way once git is done (nothing ran in it), unless you closed it with `taskman close --keep-worktree`. From a shell, `taskman close --merged` does this for every merged ticket at once (see [taskman close](#closing-tickets-from-a-shell-or-a-session-taskman-close)).

## Agents

An **agent** is a session in a project with no ticket: for a conversation, an investigation, or coordinating the project's tickets. In a project without a tracker, agents are all there is: sessions in the project's folder, at full width. Add one with `ctrl+a n`, set Type to Agent and give it a name (`main`, `oss-planner`; about 12 characters fit the sidebar beside the state, longer names are cut with `…`) and, optionally, a role: a standing instruction added to its startup instructions, like "Review PRs against their tickets". Agents sit first in their project's group, run in the project's folder (the repo's main checkout), and, once the project has tickets, show a project board on the right: for each ticket, why it needs you, its branch, failing and running checks by name, the latest review and the latest comment, then the project's agents with their roles. `ctrl+a e` renames an agent: its session restarts on the same conversation under the new name, which taskman claims before saving the rename, so `taskman resume` under it is refused rather than starting a second copy while the old session is still stopping; a name whose session runs in another terminal can't be taken. Closing an agent (`ctrl+a x`) stops its session but keeps its conversation: add an agent with the same name to that project again and it picks up where it left off, role included.

Every session taskman starts is named (`claude --name`): ticket sessions by their key (`ENG-52`), agents as `<project>-<name>`. Keys only have to be unique within a project (two Local files boards can both have `WP-1`), so while another project has the same key, a session starting is named `<project>/<KEY>` (`beta/WP-1`) instead, with a short id from the folder when two project names look alike (`alpha-web-3f2a/WP-1`); the agents' list and `taskman status` say so. Claude Code sessions on one machine can message each other by name (ListAgents and SendMessage), so an agent can ask `ENG-52` to fix its failing check and hear when it's done. Its startup instructions list the project's tickets, worktrees and PRs. While a project's agent runs, taskman starts that project's ticket sessions too, since only running sessions can be reached. Claude Code's own rules apply to these messages: a session in a different permission mode holds them for your approval.

## Sessions read their ticket: `taskman status`

Every session taskman starts can read its ticket and PR with `taskman status`, and its startup instructions tell it to. It fetches them fresh with the project's own tracker account, so a session needs no Jira or Linear tools of its own, and they can't point at the wrong workspace:

```
$ taskman status
ENG-52 · Fix login
Linear · In Progress · Ana · Bug
https://linear.app/acme/issue/ENG-52

Description:
  Users get logged out after a password reset. …

Comments (newest first, 3 of 5; --json has all):
- Ana, 2h ago:
    Repro: reset the password, then refresh.

Worktree: ~/acme-ENG-52 · branch eng-52-login
PR: #31 open · Fix login · https://github.com/acme/web/pull/31
  Checks: 1 failing (lint), 4 passed
  Review: changes requested; latest: changes requested by bo, 1h ago
  Review text (newest first):
  - changes requested by bo, 1h ago:
      Handle an expired token too.
  Review threads: 1 unresolved
  - src/auth/session.rs:88:
    bo, 1h ago:
      This drops the refresh token; keep it.
  Latest comment: bo, 1h ago
    Please add a test.
```

In an agent's session, `taskman status` lists every ticket in the project with its status, its session's state (working, needs you, your turn), branch and PR, and the project's agents with their roles, so the agent works from what's happening now rather than from when it started. `taskman status ENG-52` shows any ticket, from any shell; `--json` gives the same data with every comment, review and review thread. Past the newest 100 tracker comments or review threads it says how many older ones it left out. In a session or a project's folder, a key means that project's ticket, even one taskman doesn't have open, unless the key has another project's prefix; `--project NAME` picks the project. It only reads: commenting and moving tickets stay with the session's own tools. In a project without a tracker it reports what there is instead of failing: the agents and their sessions' states.

From your own shell, `taskman status` with no key goes by the folder you're in: in a ticket's worktree it shows that ticket, in a project's repository that project's tickets, and anywhere else one line per ticket across every project:

```
$ taskman status
acme-web  ~/src/acme-web
  ENG-52  In Progress  needs you    #31 open, 1 failing (lint)  Fix login
  ENG-60  Todo         not started  no PR                       Rate-limit uploads

taskman status KEY shows one ticket in full.
```

Claude sessions run it without asking. In Codex it needs network access, which Codex's sandbox blocks, so Codex asks before running it; answer "don't ask again for commands that start with `taskman status`" once to allow it from then on.

## Adding tickets from a shell or a session: `taskman add`

A ticket you or an agent just created can go straight into taskman, with the project's defaults:

```
$ taskman add 61
Added ENG-61 "Rate-limit uploads" to acme-web; creating worktree ~/acme-web-ENG-61
```

- **What it takes:** a number (`61` becomes `ENG-61`), a key or a link; several at once. On GitHub Issues, `#61` and `acme/web#61` too. `--codex` runs the session in Codex, `--worktree` / `--no-worktree` overrides the project's worktree setting, `--on KEY` stacks it on another open ticket (see [Stacked tickets](#stacked-tickets)), and `--agent NAME` adds a project agent instead. With a Local files tracker, `--new "Fix login"` creates the ticket's file first (see [Local files](#local-files-tickets-without-an-account)).
- **Agents adding agents:** `taskman add --agent tester --role "Run the TUI in tmux and check PRs against their tickets' 'Done when'."` adds a helper whose startup instructions carry the role. taskman shows you the agent and its role first, and adds it when you press Enter: sessions run `taskman add` without asking, and an issue or PR comment could talk one into it. Once you approve, its session starts, so the agent that asked can message it; it does nothing until someone does. `--role` on an agent that's already open replaces its role from its next session start (`ctrl+a R` restarts it), also after you approve it. Tickets are added without asking.
- **Which project:** the session's, else the one whose folder you're in, else the one whose ticket prefix the key has. `--project NAME` picks one by its name, ignoring case, or its name with dashes for spaces; when that could mean several projects it asks for the exact name. A project without a tracker takes `--agent` only.
- **It doesn't take your focus.** The ticket appears in the sidebar marked `✦ new` until you select it, and a message says who added it ("✦ planner added ENG-61"). While a project's agent runs, the new ticket's session starts so the agent can message it.
- **It checks the key** with the project's tracker account, so a typo isn't added; if the tracker can't be reached, the key is added unchecked.
- **taskman closed?** The ticket is added when it starts: "taskman isn't running; ENG-61 is added when it starts."
- **Scriptable:** a ticket that's already open does nothing; one line per key, and exit 0 unless a key failed.

Agents' startup instructions mention it, and Claude sessions run it without asking. Codex asks, because it writes to taskman's state folder, outside Codex's sandbox.

## Closing tickets from a shell or a session: `taskman close`

After a wave of merges, close them all at once:

```
$ taskman close --merged
Closed ENG-61. Removed ~/acme-web-ENG-61 and branch eng-61-rate-limit
Closed ENG-62. Kept ~/acme-web-ENG-62: it has 2 unpushed commits
ENG-63's PR #88 isn't merged: not closed
```

- **What it takes:** numbers, keys or links (`taskman close 61 62`), or `--merged` for every ticket whose PR has merged; with keys too, only those of them that merged. `--keep-worktree` closes without removing anything, and `--agent NAME` closes a project agent.
- **Each ticket closes as `ctrl+a x` closes it:** its session stops, then, once its CLI has ended, the worktree taskman created is removed, with its branch once merged, only if nothing would be lost (see [Worktrees](#worktrees)). Otherwise it stays and the line names the folder and says why, like `Kept ~/src/acme-web-ENG-52: it has ignored files: .env`. Nothing is forced. Ignored files are never deleted from here, and the ticket is closed by then, so the dialog can't either: keep what you need from the folder, then remove it yourself with `git worktree remove` and its path, which deletes ignored files too. taskman looks each PR up fresh first, so a merge from a minute ago counts.
- **Which project:** as for `taskman add`; `--merged` run outside any project covers them all.
- **Not closed:** a ticket whose session you continued in another terminal with `taskman resume` ("quit it there first"), and with `--merged` one whose PR can't be looked up.
- **taskman closed?** The tickets close when it starts. If a dialog is open in taskman, they close once it's answered.
- **It asks first in a session.** Unlike `taskman add`, sessions aren't allowed to run it without your approval, so text a session read can't close your tickets unattended.

## Local files: tickets without an account

A project's tracker can be a folder of markdown files in the project, with no account and no network. Each subfolder is a column, and the folder a ticket is in is its status:

```
workplan/
  backlog/
  todo/      WP-4-add-export.md
  doing/     WP-3-fix-login.md
  review/
  done/      WP-1-first-run.md
```

- **Set up:** in the project's settings pick **Tracker: Local files**. **Tickets folder** defaults to `workplan`, and **Ticket prefix** to the one the folder's ticket files use already, else one from the project's name (`AW` for `acme-web`). Saving creates `backlog/ todo/ doing/ review/ done/` if the folder is missing or empty.
- **Files:** a ticket is `<KEY>-<slug>.md`, a regular file: links are ignored, so a repository can't point a ticket at a file elsewhere on your disk. The key comes from the name; the slug is free, and with the default branch format the whole name is the ticket's branch (`WP-3-fix-login`). The first `# ` heading is the title (else the slug), the rest is the description, and optional frontmatter adds type, priority and assignee:

  ```markdown
  ---
  type: bug
  priority: High
  assignee: ana
  ---
  # Fix login

  The login form forgets the workspace.
  ```
- **Columns:** the usual five in their order, then any other folders alphabetically; a number prefix (`01-ideas`) sets the order and isn't shown. `done` shows green, `doing` and `review` yellow, the rest blue.
- **Adding:** `n` takes a number or key for a file already on the board, or any other text as the title of a new ticket, written into `todo/` with the next number (the form says which before you press Enter). The tickets not done (outside `done/`, or the last column on a board without one) are listed under the field, narrowed as you type: to add one of them by its title rather than create a new ticket, highlight it with ↑/↓ and press Enter. An assignee in a ticket's frontmatter is just text, so none is marked as yours. From a shell or an agent: `taskman add 3`, or `taskman add --new "Fix login"`.
- **Moving:** move the file by hand, or run `taskman move WP-3 review`. The panel follows within a moment: it rereads the files as they change. `ctrl+a o` opens the ticket's file in `$VISUAL` or `$EDITOR` (vi if neither is set).
- **Worktrees:** if the folder is committed, each worktree has its own copy of it. taskman only ever reads and writes the one in the project folder: the panel, `taskman status`, `taskman move` and `taskman add --new` all use it, wherever they run, and sessions are told a worktree's copy isn't the board.
- **Committing is up to you.** taskman never commits these files. Commit `workplan/` for a board the team shares (moves then show as changes on main), or add it to `.gitignore` for a private one.

Comments aren't separate yet: write them in the file's body.

## GitHub Issues: the repository's own issues

A project whose work is tracked in its GitHub repository's issues can use them as its tracker. There is no account and no token: taskman asks `gh`, as the PR panel does.

- **Set up:** in the project's settings pick **Tracker: GitHub Issues**. The repository is the one `gh` uses in the folder, read from its git remotes when you save (see [Set up](#set-up)), so the project needs a GitHub remote and `gh` logged in; without a remote the form says so and doesn't save. **Ticket prefix** defaults to one from the repository's name (`WEB` for `acme/web`).
- **Keys:** an issue's key is the prefix and its number: issue #12 is `WEB-12`. That key names its session, branch (`WEB-12-fix-login` by default) and worktree folder. The Add form and `taskman add` take `12`, `#12`, `WEB-12`, `acme/web#12` or the issue's link; a pull request's link or number is refused, as is an issue of another repository.
- **Adding:** `n` lists the repository's open issues under the Ticket field, those assigned to you first, and narrows the list as you type a number or words of a title (see [Use](#use)); Enter adds the one issue that matches, or the one highlighted with ↑/↓. A closed issue isn't listed, but its number adds it, and its panel says it's closed.
- **The panel** shows the issue's title, whether it's open or closed (and why: completed, not planned, duplicate), its labels and assignees, its body, the newest 100 comments and the pull requests that close it.
- **The PR panel** shows the pull request GitHub links as closing the issue ("Fixes #12" in its description, or linked in the issue's sidebar) when there is an open or merged one, and otherwise finds it by branch and key as for other trackers.
- **Several GitHub accounts:** `gh` uses its active account for the issue's host. If the panel says `gh` can't see the repository, switch with `gh auth switch`, or give the token access to private repositories with `gh auth refresh -s repo`.

## A session that won't resume

If the pane says a session didn't resume and shows "That session is running in the background", Claude Code is running that conversation as one of its own background sessions (`claude --bg`). Press Enter to run `claude stop` on it and resume it in the pane. If that fails, run the `claude stop <id>` command Claude names yourself, then press Enter.

## Codex sessions

A ticket or agent can run in OpenAI's Codex CLI instead of Claude Code: in the Add form (`ctrl+a n`), set CLI to Codex. Codex sessions get the same status in the sidebar, the same panels and resume after a restart. What they can't do is messaging: agents only message Claude sessions, so no agent can reach a Codex session, and a Codex agent can't message the tickets.

So keep a project's tickets on one CLI, and use the other for one-offs, like a review agent that reads a ticket's worktree or `gh pr diff`. A project can be all Codex; its agents just can't coordinate its tickets. The board marks Codex tickets `codex · can't message`. Set `TASKMAN_CODEX` to run another `codex` command.

## How it works

- **Ticket → branch → PR.** taskman looks for a worktree branch ending in `/<KEY>` (worktrees your workflows create), then any branch containing the key as a whole (`ENG-5` doesn't match `eng-52-login`), then a PR whose title or branch mentions the key. The worktrees it creates use the project's branch format, which always contains the key.
- **`taskman status`.** Sessions get a `taskman` first on their PATH that runs the binary they were started by (a link in `~/.local/state/taskman/bin`), and Claude's `--settings` allow `Bash(taskman status:*)`. What it prints from the tracker and GitHub is labelled as data and cleaned of terminal escape codes, and briefs carry no tracker text (see [design.md](design.md#untrusted-text)). The ticket is the session's `TASKMAN_TICKET`; outside sessions, the ticket whose worktree it runs in (its session's folder, or the worktree of a branch with its key), else the project whose repository it runs in. `TASKMAN_REPO` says which project the session's ticket is in, since the same key can be open in two. A ticket taskman doesn't have is matched to a project by its prefix (`ENG`), else by the repository it's run in.
- **`taskman add`** never edits `tickets.json`, which the running taskman would overwrite. It leaves a request in `~/.local/state/taskman/inbox/`; taskman adds it within a second, through the same code as the Add form, and writes a reply the command prints. taskman rewrites `inbox/alive` (its version) while it runs, so the command knows not to wait when it's closed (the request then waits for the next start) and refuses options an older running taskman would ignore. Claude's `--settings` also allow `Bash(taskman add:*)` and `Bash(taskman move:*)`; an agent or role still waits for you to approve it. `taskman close` goes through the same inbox, but isn't allowed, so a session asks you before running it.
- **Status.** taskman starts each `claude` with `--settings` adding hooks that call `taskman hook <state>`, which writes `~/.local/state/taskman/status/<project>-<hash>/<KEY>.json` (a folder per project, since keys repeat across projects). Codex gets the same hooks with `-c`, marked trusted for that session only. Your own settings, Claude's and Codex's, are untouched.
- **Refresh.** The ticket on screen refreshes Jira every 30s and GitHub every 20s; the others every 3 min and 1 min. It also refreshes right away when Claude uses a Jira tool, pushes, or runs `gh pr`/ `gh run` (a hook writes a signal file), when Claude finishes a turn, and when you switch to a ticket whose data is over 30s old. `ctrl+a r` refreshes now. Each panel shows how fresh its data is, in minutes; routine refreshes are silent, and a spinner shows only for ones you're waiting on.
- **New comments.** Jira comments, PR comments and reviews that arrived since you last viewed a ticket are marked `●`, and the sidebar shows `N new` on the other tickets. Leaving a ticket marks what its visible panels showed as read (hidden panels stay new); `ctrl+a m` marks it read now.
- **What no hook reports.** Approving a permission prompt fires no hook in either CLI, so the key that answers it (Enter, a digit or Esc in Claude; Enter, `y`, `n` or Esc in Codex) shows the session working at once; if Claude stays still after it, the prompt is still waiting and it shows "needs you" again. Answering a question or a plan doesn't: it waits until all are answered. Claude Code fires no hook when you stop it with Esc or ctrl+c either, so taskman watches for its "Interrupted" line, or 2 seconds of stillness, after those keys, and treats a "working" session that has drawn nothing for 8 seconds as stopped (a working CLI redraws constantly). Codex reports interrupts with a hook.
- **Resume.** The hooks also record the session id, so after a restart a ticket resumes its last conversation when you select it (`claude --resume`, `codex resume`). Two sessions never share one: if Claude Code switches an agent onto another session's conversation (a rare bug with agents in one folder), taskman keeps the agent's own and tells you to restart it.
- **Quitting.** SIGTERM, SIGHUP and SIGINT quit like `ctrl+a q`: the terminal is restored, the sessions stopped, and a worktree still being removed or created gets up to 30 seconds to finish; a second signal ends taskman at once. Closing the terminal (or losing the SSH connection) stops taskman and its sessions too, within 3 seconds. `ctrl+a q` first waits for worktrees still being removed, and for git to finish creating the worktree of a ticket you closed meanwhile (unless closed with `--keep-worktree`), and says so; if taskman stops some other way meanwhile, git finishes the removal and the ticket can't be added back into that folder until it's done. A crash in one session's output ends that session only; the error goes to `~/.local/state/taskman/panics.log`.
- State (projects, tickets) lives in `~/.local/state/taskman/tickets.json`. If it or the accounts file can't be read (a typo, or written by a newer taskman), taskman says so and doesn't start, leaving the file as it is. Only one taskman runs per state folder: a second one is refused, since each would overwrite the other's saves.
