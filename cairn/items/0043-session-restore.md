---
id: 43
title: Session restore
type: feature
status: backlog
milestone: m5
created: 2026-09-10
updated: 2026-09-10
priority: p2
effort: s
area: core
---

## Problem

Reopening a project should not mean rebuilding the layout by hand.

## Proposal

Per-directory session state: open buffers, pane tree, tab order, caret positions,
folds and scroll offsets. Written on change, debounced, outside the repository.

## Acceptance criteria

- [ ] Restores layout and carets exactly
- [ ] A file deleted since last time is skipped with a notice, not an error
- [ ] Corrupt session state falls back to an empty session rather than failing to start
- [ ] `nun --no-session` opens clean
