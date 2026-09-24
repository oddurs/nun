---
id: 42a1f698-ca43-4a44-a429-09de2964bdae
title: Session restore
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-23
closed_at: 2026-09-23
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

- [x] Restores layout and carets exactly
- [x] A file deleted since last time is skipped with a notice, not an error
- [x] Corrupt session state falls back to an empty session rather than failing to start
- [x] `nun --no-session` opens clean

## 2026-09-23

Session per folder in $XDG_STATE_HOME/nun/sessions/<name>-<fnv64>.toml, versioned (version = 1); folds stay in the existing per-file store. Carets are kept as [line, char column] so they clamp and snap to graphemes when the file changed. Started with a file, the session is restored and then that file is focused, or opened beside the others. [panels.<kind>] tables are the extension point for 0041's terminal panel and are carried through untouched. Untitled buffers and unsaved edits are not kept, and neither is sidebar state.
