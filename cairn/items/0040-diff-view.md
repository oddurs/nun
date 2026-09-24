---
id: 40
title: Diff view
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-24
closed_at: 2026-09-24
priority: p2
effort: m
area: vcs
---

## Problem

Reviewing a whole file's changes needs more room than a gutter popover.

## Proposal

A side-by-side diff in a pane, against the index or against HEAD. Same rendering
and same theme roles as the editor, with synchronised scrolling.

## Acceptance criteria

- [x] Side-by-side and unified, toggleable
- [x] Intra-line changes highlighted, not just whole lines
- [x] Hunks can be staged from the diff too
- [x] Scroll stays synchronised across differing line counts

## 2026-09-24

Done. The view replaces the text of the pane it is opened from (Ctrl+K Shift+G, or the text's right-click menu) instead of opening a tab or a split. It is a way of looking at the document, not a document, so panes keep holding only files, and side by side keeps the pane's full width. It tracks its own copy of the document in nun-vcs under a reserved id (`VCS_ID`, u32::MAX - 1) and never depends on the gutter's requests. Both sides are highlighted under parser ids of their own. Filler rows are hatched with the `diff.filler` glyph on the sunken surface. The new theme roles are added_wash, removed_wash, added_emphasis and removed_emphasis. Against HEAD, a hunk can be staged when a hunk against the index makes the same change. A partly staged one is staged from the index view.
