# Records demo/taskman.gif: run demo/setup.sh first (see demo/README.md), then `vhs demo/taskman.tape`.
# Claude is real, so its answers differ between recordings. Waits on Claude are hidden (jump cuts).

Output demo/taskman.gif

Set Shell "bash"
Set FontSize 14
Set Width 1600
Set Height 900
Set Padding 12
Set Framerate 20
Set TypingSpeed 20ms
Set Theme "Catppuccin Mocha"

# After ctrl+a the shown steps pause as a person would, so the menu is seen briefly.

# Warm-up, not recorded: start the sessions the story visits (the agent starts its project's
# ticket sessions), so no pane is caught half-way through starting.
Hide
Type "source /tmp/taskman-demo/env.sh && clear && taskman"
Enter
Sleep 8s
Ctrl+A
Type "1"
Sleep 8s
Ctrl+A
Type "8"
Sleep 8s
Ctrl+A
Type "2"
Sleep 4s
Show

# 1. The overview: three projects (Jira, Linear, Local files), each ticket's state and PR, ENG-52's
# Jira issue and PR.
Sleep 2s

# 2. Put Claude to work on ENG-52, then move on while it works. Asking first makes it wait on
# us whatever the permission mode.
Type "The e2e test for empty files fails. Find the cause in src/import.ts, then ask me to pick between two ways to fix it before you edit anything."
Sleep 500ms
Enter
Sleep 3s

# 3. ENG-54 is stacked on ENG-53: its worktree started from ENG-53's branch and its PR goes into it.
Ctrl+A
Sleep 800ms
Type "4"
Sleep 2500ms

# 4. A Local files project: tickets are markdown files in workplan/, no account and no GitHub.
# Its Claude moves the ticket with `taskman move` and the panel follows. Asked to run the command
# itself: asked to move the ticket, Claude may first check the work and ask before moving it.
Ctrl+A
Sleep 800ms
Type "8"
Sleep 2s
Type "Run taskman move WP-3 review."
Sleep 500ms
Enter
Hide
Wait+Screen@90s /Local files WP-3 review/
Show
Sleep 4s

# 5. ENG-52 needs us; ctrl+a ctrl+a jumps to it and Enter answers its question. If Claude
# answered without asking, it's simply our turn there. Matching ENG-52's own sidebar line
# keeps WP-3's finished turn from ending the wait.
Hide
Wait+Screen@180s /ENG-52 +(▲ needs you|● your turn)/
Show
Sleep 2s
Ctrl+A
Sleep 800ms
Ctrl+A
Sleep 5s
Enter
Sleep 3s

# 6. The project's agent asks a ticket session for an update, by name.
Ctrl+A
Sleep 800ms
Type "1"
Sleep 3s
Type "Ask ENG-53 for a one-line status of the work on its branch, then tell me what it said."
Sleep 500ms
Enter
Sleep 3s
Hide
Wait+Screen@240s /Message from @ENG-53/
Sleep 10s
Show
Sleep 6s

# 7. Every command is one ctrl+a away: held long enough to read it.
Ctrl+A
Sleep 8s
Escape
Sleep 1s
