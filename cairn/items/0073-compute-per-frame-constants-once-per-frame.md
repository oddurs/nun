---
id: ff8325cf-c1c9-424f-a8e3-7043a6eb7e3a
title: Compute per-frame constants once per frame
type: chore
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p2
effort: s
area: ui
---

## Problem

Every gutter row calls `gutter_width()`, which formats `len_lines()` into a
`String`, and allocates its own line-number label
(`crates/nun-ui/src/view.rs:254-255`, `82`). `draw_line` recomputes
`line_of(primary)` for each row (`view.rs:283`).

## Proposal

Compute the gutter width and the primary caret's line once per frame, and write
line numbers into a reused buffer.

## Acceptance criteria

- [ ] No per-row allocation in the gutter
- [ ] Bench: frame time recorded before and after
