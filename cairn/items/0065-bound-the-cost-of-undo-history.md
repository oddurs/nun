---
id: db256dcd-c789-4c3d-885c-1df228477232
title: Bound the cost of undo history
type: chore
status: backlog
milestone: perf
depends_on:
- 98e7cf7d-4a57-4a62-8bd9-5346b7a2bf5f
created: 2026-09-22
updated: 2026-09-22
priority: p1
effort: m
area: core
---

## Problem

Grouping keystrokes into one undo step rebuilds the text of the current run
with `format!` on every keystroke, per caret (`crates/nun-core/src/buffer.rs:544`,
`548`), so a long run of typing is quadratic. History is unbounded: each
revision clones the selections before and after (`buffer.rs:429`, `453`), which
with 10k carets is 20k ranges per step, and undo and redo clone the whole
revision to apply it (`history.rs`, `step_back` and `step_forward`).

## Proposal

Append to the run with `push_str`. Apply undo and redo by reference. Cap history
by memory rather than count, dropping the oldest steps, and make the cap a
setting once config lands.

## Acceptance criteria

- [ ] Typing a 100k-character run is linear (bench)
- [ ] Undo and redo do not clone a revision
- [ ] History memory stays under the cap in a 10k-caret soak test
