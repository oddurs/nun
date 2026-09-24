---
id: 39
title: Gutter marks and hunk actions
type: feature
status: backlog
milestone: m5
created: 2026-09-10
updated: 2026-09-23
priority: p0
effort: m
area: vcs
---

## Problem

Staging a hunk is the most common git operation while writing code, and it
normally means leaving the editor.

## Proposal

A coloured bar per changed line. Hovering opens a popover with the previous
text and clickable `Revert` and `Stage` buttons. Clicking the mark jumps between
hunks.

## Acceptance criteria

- [ ] Added, modified and deleted are visually distinct
- [ ] Revert restores exactly the hunk, and is undoable
- [ ] Stage writes to the index without touching the working tree
- [ ] The rail shows hunk positions alongside diagnostics without either winning

## 2026-09-23

nun-vcs (0038) runs in the editor (`App::vcs`, replies arrive as `Event::Vcs`) but only for the tree's status. For the gutter, send `Request::Open` / `Update` (debounced, once typing pauses) / `Moved` / `Close` per document and draw from `Reply::Hunks`. `Diff::marks(range)` gives a gutter window's marks; the last line can carry two marks (a removal above and one below), so draw both. `Request::Stage` and `Diff::revert` are the hunk actions.
