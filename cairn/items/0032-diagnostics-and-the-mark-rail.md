---
id: 32
title: Diagnostics and the mark rail
type: feature
status: backlog
milestone: m4
depends_on:
- 31
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: lsp
---

## Problem

Diagnostics are the highest-value thing a language server provides and the
easiest to render badly.

## Proposal

Undercurl in the buffer via `CSI 4:3 m` with a coloured underline where the
terminal supports it, a plain underline where it does not. Severity marks on the
scrollbar rail, clickable to jump. A hover popover with the message and any
quick fixes as clickable buttons.

## Acceptance criteria

- [ ] Undercurl capability is detected, with a correct fallback
- [ ] Ranges stay attached to the right text as the buffer is edited
- [ ] The rail shows density honestly when marks collide at one row
- [ ] Counts in the status line match what the rail shows
