---
id: 76
title: Say where Ctrl-click is taken by the terminal
type: feature
status: backlog
milestone: m4
created: 2026-09-22
updated: 2026-09-22
priority: p2
effort: s
area: lsp
---

## Problem

Go to definition is Ctrl-click (0035). iTerm2 with default settings and
Terminal.app turn a Ctrl-click into a context menu and never report it, and
iTerm2 drops Option-clicks entirely, so Alt-Ctrl-click (open beside) is lost
there too. nun cannot see the click, so it cannot say anything; the underline
from Ctrl-hover can even promise a click that will open a menu instead.

## Proposal

Identify the terminal by probing XTVERSION (never `$TERM`) and, on iTerm2,
say once that Ctrl-click needs Settings > Pointer > "Ctrl-click reported to
apps". Where the Kitty keyboard probe got no answer (Terminal.app is the only
terminal in the matrix without it), name F12 in the hint instead.

## Acceptance criteria

- [ ] iTerm2 users are told once how to let Ctrl-click through
- [ ] Terminals known to take Ctrl-click get the F12 hint instead
- [ ] The probe honours a timeout and says nothing when it gets no answer
