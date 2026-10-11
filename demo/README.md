# Demo

Records `demo/taskman.gif`: taskman with three made-up projects, real Claude sessions, and Jira and
Linear issues and PRs read from `demo/fixtures` instead of Jira, Linear and GitHub (demo mode,
`TASKMAN_DEMO`). The third project uses a real Local files board in its own `workplan/` folder,
and has no git remote, so it shows no PR panel.

## Record

```bash
demo/setup.sh          # throwaway repos, projects, tickets and an agent in /tmp/taskman-demo
vhs demo/taskman.tape  # writes demo/taskman.gif
```

Needs [VHS](https://github.com/charmbracelet/vhs) (with `ttyd` and `ffmpeg`) and Claude Code logged
in. Without Go or a package manager, the release binaries work: `vhs_*_Linux_x86_64.tar.gz` from
charmbracelet/vhs and `ttyd.x86_64` from tsl0922/ttyd, in a folder on `PATH`. The recording runs real Claude sessions in the demo repos, so it uses a few prompts and its
answers differ each time.

**First time only:** Claude asks whether to trust each new folder, and the tape can't answer
that. Run the demo once by hand and accept for each repo (the prompt's default is "No, exit", so
choose "Yes, I trust this folder"):

```bash
source /tmp/taskman-demo/env.sh && taskman   # then ctrl+a 6 and ctrl+a 8 for the other repos; ctrl+a q to quit
```

Claude remembers the answer per folder, so re-running `setup.sh` doesn't ask again.

The tape picks tickets by their sidebar number (`ctrl+a 6`); renumber it when `setup.sh` changes
the tickets. To catch a session waiting on you whatever your Claude permission mode, ENG-52 is
asked to put a question to you before editing.

## Change the data

`demo/fixtures` has one `<KEY>.issue.json` per ticket and a `<KEY>.pr.json` for tickets with a PR
(no file: "no PR yet"). Times are ages (`"6m"`, `"2h"`, `"1d"`). `src/demo.rs` documents the
fields; a PR's `base` (default `main`) is the branch it merges into, another ticket's for a stacked
ticket. The projects, tickets and the Local files board are written by `demo/setup.sh`.
