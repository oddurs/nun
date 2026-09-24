---
id: 89
title: 'Terminal panel: copy through OSC 52 where the system clipboard cannot'
type: feature
status: backlog
milestone: m5
depends_on:
- 41
created: 2026-09-23
updated: 2026-09-23
priority: p2
effort: m
area: ui
---

## Problem

The terminal panel copies a selection with the system's clipboard program
(pbcopy, wl-copy, xclip or xsel), and says plainly when none of them took
it. Over ssh that is the remote machine's clipboard, or no clipboard at all,
so the copy never reaches the machine the person is sitting at.

## Proposal

Add a setting: `clipboard = "auto" | "system" | "osc52"`. In `auto`, copy
with OSC 52 through nun's own terminal only when no system program is
available or `SSH_TTY` is set, and a probe (DA1 parameter 52, or XTGETTCAP
`Ms`) says the terminal accepts it. The terminal never confirms an OSC 52
copy, so the status line says the text was sent, not that it was copied.
Under tmux it also names the `set-clipboard on` setting.

## Acceptance criteria

- [ ] Over ssh, in a terminal that says it accepts OSC 52, a selection reaches the local clipboard
- [ ] Where support cannot be detected, the setting decides, and the message says what was done
