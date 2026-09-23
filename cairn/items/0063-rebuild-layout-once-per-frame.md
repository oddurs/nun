---
id: 63
title: Rebuild layout once per frame
type: chore
status: backlog
milestone: perf
depends_on:
- 57
created: 2026-09-22
updated: 2026-09-22
priority: p1
effort: m
area: ui
---

## Problem

A single keystroke runs `relayout()` up to three times: in `run()`
(`crates/nun/src/app.rs:757`), in `handle_at` (`app.rs:609`) and in `tick`
(`app.rs:579`). Each run rebuilds the whole hit map, one `Vec` per row
(`crates/nun-input/src/hit.rs:60`), and recomputes the replace previews with a
regex over every visible row (`app.rs:450` → `app/search.rs:820-858`).

## Proposal

Mark layout dirty where state changes, and rebuild once, just before drawing.
Reuse the hit map's row storage between frames instead of reallocating it.

## Acceptance criteria

- [ ] At most one relayout per drawn frame (asserted with a counter in a test)
- [ ] Hit-testing between a change and the next frame still sees current geometry
- [ ] Bench: keystroke p99 improves on the reference file, with the numbers in the PR
