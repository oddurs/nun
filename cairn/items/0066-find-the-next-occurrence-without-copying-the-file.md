---
id: cde36112-cb56-4863-96a7-87cd3f2aee7f
title: Find the next occurrence without copying the file
type: chore
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p1
effort: s
area: core
---

## Problem

Selecting the next occurrence (Ctrl+D) and selecting all occurrences both call
`rope.to_string()` on every press (`crates/nun-core/src/buffer.rs:1024`,
`1095`). On a large file that is the whole file copied per keypress.

## Proposal

Search the rope's chunks directly, handling a match that straddles a chunk
boundary, starting from the caret and wrapping.

## Acceptance criteria

- [ ] Neither command allocates in proportion to the file size
- [ ] Matches across chunk boundaries are found (test with a small chunk size)
- [ ] Bench: Ctrl+D on a 100 MB file under 5 ms
