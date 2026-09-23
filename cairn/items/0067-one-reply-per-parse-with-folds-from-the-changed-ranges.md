---
id: 67
title: One reply per parse, with folds from the changed ranges
type: chore
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p1
effort: m
area: syntax
---

## Problem

After every parse the worker walks the entire tree to recompute folds
(`crates/nun-syntax/src/worker.rs:276`, `highlight.rs:274`), which is linear in
the file on every keystroke and delays the highlights sent alongside. It then
sends two separate replies, `Highlights` and `Folds`
(`worker.rs:273-278`), and each one asks for a redraw.

## Proposal

Send a single reply per parse. Recompute folds only within the tree's changed
ranges and splice them into the previous set.

## Acceptance criteria

- [ ] One redraw per parse
- [ ] Fold work after a one-character edit is independent of file size (bench)
- [ ] Folds after a sequence of edits match a full recomputation (property test)
