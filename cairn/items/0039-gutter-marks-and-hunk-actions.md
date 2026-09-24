---
id: 39
title: Gutter marks and hunk actions
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-24
closed_at: 2026-09-24
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

- [x] Added, modified and deleted are visually distinct
- [x] Revert restores exactly the hunk, and is undoable
- [x] Stage writes to the index without touching the working tree
- [x] The rail shows hunk positions alongside diagnostics without either winning

## 2026-09-23

nun-vcs (0038) runs in the editor (`App::vcs`, replies arrive as `Event::Vcs`) but only for the tree's status. For the gutter, send `Request::Open` / `Update` (debounced, once typing pauses) / `Moved` / `Close` per document and draw from `Reply::Hunks`. `Diff::marks(range)` gives a gutter window's marks; the last line can carry two marks (a removal above and one below), so draw both. `Request::Stage` and `Diff::revert` are the hunk actions.

## 2026-09-24

Done. The gutter has a change column after the line numbers, always
reserved so the text never moves sideways; the rail gives changes their own
column just inside the diagnostics' one, sharing its thumb. Cards come from
resting on a bar, clicking one, or F7 / Shift+F7 / Ctrl+K g; Revert (Ctrl+K u)
and Stage (Ctrl+K s) are its buttons. Hunks refresh on focus regained, on
leaving the terminal panel, on save, and after staging; a change to the index
made by another program while nun keeps focus is only noticed on one of those.
