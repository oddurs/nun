---
id: 20
title: Splits and drag-to-split
type: feature
status: done
milestone: m2
assignee: Oddur Sigurdsson
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

- [x] Split horizontally and vertically, nested arbitrarily
- [x] Drag a divider to resize; double-click evens the siblings
- [x] Drop zones on all four edges, with a preview of the resulting layout
- [x] Closing the last tab in a pane collapses the pane and rebalances

## 2026-09-19

0019 left its criterion 2 open: dragging a tab onto another pane moves the tab and its full editing state. The pieces are there — App::docs with per-document scroll, TabDrag in app/tabs.rs, and TabStrip::drop_index — so this item should finish it and tick 0019's criterion 2.

## 2026-09-19

The pane tree is nun-ui::Layout: a binary tree with a direction and a ratio per node, so closing a pane is a node collapsing and its sibling taking the rectangle back. A keyboard split opens the new pane empty rather than with a second copy of the file: two panes editing copies of one path would race each other on save. Dragging a tab is the way to work on a file in another pane, which is also 0019's criterion 2.

## 2026-09-19

Closing a pane moves whatever was open in it into the pane that takes its space: closing a pane is about the screen, not about the files, and the first cut of it silently destroyed unsaved documents (they also escaped the quit check, because they were gone from the document list). A tab dragged out of its own pane onto that pane's edge leaves an empty buffer behind rather than collapsing the split that was just asked for. A file with a name always has a tab now, so there is always something to drag: an untitled scratch buffer has nothing to label, and gives its row back to the text.
