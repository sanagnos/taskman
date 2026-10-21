# taskman

**One window for all your parallel Claude and Codex work: every ticket's session, issue and PR in one place.**

https://github.com/user-attachments/assets/91479d13-8671-4676-a53d-403ce580c604

taskman is a terminal UI for working on several tickets at once. Each ticket gets the real `claude` CLI (or `codex`) working in its own git worktree, with its Jira, Linear, GitHub or Local files issue and its GitHub PR beside it. The sidebar tells you which session needs you, and project agents coordinate the tickets by messaging their sessions.

## Install

On macOS or Linux, including Windows with WSL:

```bash
curl -fsSL https://raw.githubusercontent.com/sanagnos/taskman/main/scripts/install.sh | bash
```

It installs into `~/.local/bin` and keeps taskman up to date, checking every download against the release's checksums and, with `gh` logged in, its signed attestation. [Updates](docs/guide.md#updates) has the details and how to turn it off. Or build it with Rust 1.88 or newer:

```bash
cargo install --locked --git https://github.com/sanagnos/taskman
```

Prebuilt binaries cover macOS and Linux on x86_64 and arm64, WSL included ([Platforms](docs/guide.md#platforms)); elsewhere, build from source. The [guide](docs/guide.md#install) has other ways to install, and `taskman uninstall` removes it.

## Requirements

- **Claude Code**, installed and logged in (`claude` on your PATH), and **git**.
- Optional: the **GitHub CLI** (`gh`, logged in) for the PR panel, and **Codex** (`codex`) to run some sessions in Codex instead.
- Optional: a **Jira or Linear plugin** in Claude Code (an MCP server in Codex) if you want sessions to comment on or move their tickets; taskman reads tickets without one.
- A wide terminal: about 150 columns gives the session room beside the panels. It works down to 80x24, where the sidebar folds and the panels hide. On Windows, use Windows Terminal with WSL.

## Quick start

1. Run `taskman`, press `ctrl+a P` and enter the path of a folder you work in.
2. Press `ctrl+a n` to add an agent: a session in that folder. Every command starts with the prefix, `ctrl+a`, which shows the command keys; other keys go to the session. `ctrl+a ?` explains them.
3. For tickets, pick a tracker in the same form, or later with `ctrl+a ,`: Jira, Linear, GitHub Issues or Local files. A Jira or Linear token goes in your keychain, or a file only you can read where there's none, never in the config ([Storing a token](docs/guide.md#storing-a-token)). Then `ctrl+a n` adds a ticket by number, key or link.
4. `ctrl+a [` / `]` switch tickets, `ctrl+a /` jumps to one by name or key, and `ctrl+a ctrl+a` jumps to the next one waiting on you.
5. `ctrl+a q`, then Enter, quits; sessions resume where they left off next time.

Use tmux or screen with `ctrl+a`? Pick another prefix in Settings. The [guide](docs/guide.md) covers the rest.

## Why

AI lets you work on many tickets at once, but each ticket is really three windows: the **session** doing the work, the **issue** where the comments land, and the **pull request** with its checks and reviews. With *N* projects of *M* tickets, that's *N × M × 3* windows, and juggling them is what slows you down.

taskman organises the work the way you think about it, **project → ticket**, and shows each ticket's three side by side:

```text
+- Tickets -----------+-------------------------+- Jira: ENG-52 -----------+
| v checkout-service  |                         | Bulk import: validate    |
| > ENG-52 NEEDS YOU  |  Claude Code, the real  | In Progress              |
|   PR #412: 1 fails  |  CLI, working in this   | -- 3 comments, 1 new --  |
|   ENG-53 working    |  ticket's worktree      | * Ana, 2m: can we also   |
|   PR #415 approved  |                         +- GitHub: PR #412 --------+
| v billing-api       |                         | 8 passed, 1 failing      |
|   * orchestrator    |                         | x e2e-tests, 2m ago      |
|   ENG-7 your turn   |                         | changes requested        |
+---------------------+-------------------------+--------------------------+
```

- **One place per ticket.** Its session, its issue and its PR together, and the next ticket one keystroke away.
- **You see where you're needed.** The sidebar says which session is waiting on you, so you don't check each one.
- **Grounded in the sources of truth.** The issue comes from your tracker and the checks and reviews from GitHub, refreshed as they change and at once when a session comments or pushes.
- **Agents carry the coordination.** A planner can split the work into tickets, start a session for each and follow up with them, so you direct the work instead of relaying it.

## How

![taskman: three projects on Jira, Linear and Local files, a stacked ticket, a session waiting on you, and a project agent messaging a ticket](https://github.com/sanagnos/taskman/raw/main/docs/taskman.gif)

- **Ticket:** an issue with its own session, git worktree and PR; the session reads them with `taskman status`, so it needs no tracker tools.
- **Session:** the real `claude` (or `codex`) CLI, not a wrapper, so every command, skill and plugin works, and it resumes after a restart.
- **Prefix:** every key goes to the session except after `ctrl+a`; `ctrl+a ?` lists what follows it.
- **Where you're needed:** `▲ needs you` when a session asks for permission or a decision, `● your turn` when it finished while you were elsewhere; `ctrl+a ctrl+a` jumps to the next one.
- **Links:** ctrl+click (cmd+click on macOS) opens a session's links, the issue, the PR, a check or a review; `ctrl+a l` lists them.
- **Agents:** sessions with no ticket, for planning, reviewing or coordinating; a role gives one a standing job.
- **Messaging:** sessions are named after their ticket (`ENG-52`) or agent, so any Claude session can message any other: "ENG-52, your e2e check fails; can you fix it?"
- **Fan out:** a planner adds tickets with `taskman add`, each with its own session it can direct; agents it adds wait for your OK.

None of the integrations are needed to start. A project is just a folder, and taskman shows only what each project has connected:

| The project has | You get |
|---|---|
| a folder | agents: sessions in that folder |
| + a tracker (Jira, Linear, GitHub Issues or Local files) | tickets, each with its own session and the issue beside it; in a git repository, each in its own worktree |
| + a GitHub remote, with `gh` logged in | the PR panel on its tickets |

Pointing taskman at a public repository's issues? Text from trackers and PRs is treated as untrusted; [SECURITY.md](SECURITY.md#tracker-and-pr-text-is-untrusted) says how.

## How it compares

With one or two sessions and no tracker, tmux or a few terminal tabs running `claude --worktree` (or `codex`) are enough, as are the CLIs' own ways of running several sessions. taskman doesn't change what a session can do: it runs the same CLI. What it adds is around the sessions:

- **The ticket and PR beside each session,** fetched from Jira, Linear, GitHub Issues or Local files, and the PR from GitHub, so a ticket's comments, checks and reviews are on the same screen as the work.
- **"Needs you" across every session and project** in one sidebar, and a key that jumps to the next one waiting.
- **A worktree per ticket,** created from the latest main when the ticket is added, and offered for removal when it closes if nothing in it would be lost.
- **Sessions that resume after a restart,** each on its own conversation, with one copy of a session running at a time.
- **Sessions that message each other,** by ticket key or agent name, so a project agent can split work into tickets and follow up with their sessions (Claude sessions only).

## When Claude Code or Codex changes

Each session is the real CLI, so a new Claude Code or Codex version works in taskman as it does in a terminal. If a CLI changes the hooks, output or options taskman relies on (last checked with Claude Code 2.1.296 and Codex 0.162), the sidebar's state can lag or be wrong and resuming or messaging can fail; the [guide](docs/guide.md#when-claude-code-or-codex-changes) has the details.

If you see that after a CLI update, [open an issue](https://github.com/sanagnos/taskman/issues/new/choose) with `taskman --version` and `claude --version` or `codex --version`, or use **Report a bug** in Settings → Help & feedback, which fills them in.

## Learn more

- [Guide](docs/guide.md): setting up, the keys, worktrees, agents, the commands, how it works.
- [Design](docs/design.md): why things are built the way they are.
- [Roadmap](docs/roadmap.md): ideas not built yet, and known gaps.
- [Security](SECURITY.md): what taskman trusts, and how to report a vulnerability.
- [Architecture](docs/architecture.md): a map of the code, for contributors.
- [Contributing](CONTRIBUTING.md) and the [changelog](CHANGELOG.md).

taskman is an independent project, not affiliated with or endorsed by Anthropic or OpenAI.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

taskman is built from open-source Rust crates under MIT, Apache-2.0 and similar licenses. Their notices are in [LICENSE-THIRD-PARTY](LICENSE-THIRD-PARTY), which every release archive carries beside the binary and `taskman --licenses` prints.

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in taskman, as defined in the Apache-2.0 license, is dual licensed as above, without any additional terms or conditions.
