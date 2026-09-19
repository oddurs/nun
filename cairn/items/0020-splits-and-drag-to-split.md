---
id: 20
title: Splits and drag-to-split
type: feature
status: backlog
milestone: m2
depends_on:
- 19
created: 2026-09-10
updated: 2026-09-19
priority: p1
effort: l
area: ui
---

## Problem

Two files side by side is the main reason to want panes at all.

## Proposal

A binary tree of panes with a direction and a ratio per node. Dividers are drag
targets. Dropping a tab on a pane edge splits in that direction, which is the
mouse-first version of the keyboard command.

## Acceptance criteria

- [ ] Split horizontally and vertically, nested arbitrarily
- [ ] Drag a divider to resize; double-click evens the siblings
- [ ] Drop zones on all four edges, with a preview of the resulting layout
- [ ] Closing the last tab in a pane collapses the pane and rebalances

## 2026-09-19

0019 left its criterion 2 open: dragging a tab onto another pane moves the tab and its full editing state. The pieces are there — App::docs with per-document scroll, TabDrag in app/tabs.rs, and TabStrip::drop_index — so this item should finish it and tick 0019's criterion 2.
