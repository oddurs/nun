---
id: 7
title: Rope-backed text buffer with edits and undo
type: feature
status: planned
milestone: m1
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: core
---

## Problem

Everything else in the editor sits on top of the buffer, so its shape decides
what is cheap and what is impossible later. A `String` makes an insert at the
top of a large file O(n) and makes undo a diffing problem.

## Proposal

`nun-core::Buffer` wrapping `ropey::Rope`. All mutation goes through a single
`apply(Edit) -> EditResult` so undo, cursor fixup and the syntax reparse hook
have exactly one place to observe a change. Undo is a stack of inverse edits
with coalescing by time and adjacency, not snapshots.

## Acceptance criteria

- [ ] Insert, delete and replace over byte, char and line indices
- [ ] Undo and redo, with typing coalesced into one undoable unit
- [ ] Line-ending and encoding detected on load, preserved on save
- [ ] No panics on invalid UTF-8; lossy load is explicit and flagged
- [ ] Property tests: apply-then-invert restores the original rope
