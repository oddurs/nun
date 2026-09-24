---
id: 246a531a-e9f3-48f9-905d-4ddfdce30c60
title: Cache fold visibility and update folds in place
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

`hidden()` and `folded()` rebuild and sort a `Vec` from every fold, with two
`char_to_line` lookups each, on every call (`crates/nun-core/src/buffer.rs:887-907`).
They are called from render, `position_at`, `line_at_row`, `follow_caret` and
`syntax_window` — several times per event. Separately, every edit reallocates
the fold list (`crates/nun-core/src/fold.rs:91`), once per caret
(`buffer.rs:392`), so an edit costs carets × folds.

After "fold all" on a large file, both become visible.

## Proposal

Keep the hidden-line set inside `Buffer`, invalidated by edits and fold
changes. Update folds in place with `retain_mut`, and skip folds that end
before the edit with a binary search.

## Acceptance criteria

- [ ] The hidden set is computed at most once per revision
- [ ] An edit touches only the folds at or after it
- [ ] Bench: keystroke with 5k folds closed and 100 carets stays under budget
- [ ] The fold tests from #27–#30 still pass unchanged
