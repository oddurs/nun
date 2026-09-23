---
id: 59
title: Highlighting switches itself off after a burst of edits
type: bug
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p0
effort: m
area: syntax
---

## What happens

The syntax layer keeps at most one pending edit. A second edit inside the 12 ms
debounce drops it (`crates/nun/src/app/syntax.rs:381`), and the worker's
batching drops it again (`crates/nun-syntax/src/worker.rs:245`). Multi-caret
edits, undo and redo always arrive as several edits
(`crates/nun-core/src/buffer.rs:418`, `626`, `638`). With the edit gone,
`update` throws the old tree away and parses from scratch
(`crates/nun-syntax/src/highlight.rs:159`).

On a large file that full parse can exceed `PARSE_BUDGET` (250 ms), and when it
does, highlighting is disabled for that document for the rest of the session
(`syntax.rs:490-505`).

## What should happen

Every edit reaches the tree in order, so reparses stay incremental however the
edits arrive. A parse that runs over budget is retried later rather than
treated as a verdict; highlighting only gives up on a document that can never
be parsed in budget, and says so in the status line.

## Reproduction

1. Open a 50k-line source file.
2. Place 200 carets and type a character, or undo a large paste.
3. Highlighting disappears and does not come back.

## Acceptance criteria

- [ ] Pending edits are queued, not replaced, on both sides of the worker channel
- [ ] Multi-caret edits, undo and redo reparse incrementally (asserted in a test)
- [ ] A timeout schedules a retry; permanent disabling is reported to the user
- [ ] Bench: a 200-caret edit on the 10k-line reference file reparses under 16 ms
