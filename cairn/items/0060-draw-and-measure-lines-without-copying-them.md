---
id: ab80cc7e-76f2-44f1-9afb-4cc44b521b17
title: Draw and measure lines without copying them
type: chore
status: backlog
milestone: perf
depends_on:
- 98e7cf7d-4a57-4a62-8bd9-5346b7a2bf5f
created: 2026-09-22
updated: 2026-09-22
priority: p0
effort: m
area: core
---

## Problem

Anything that looks at a line copies it into a `String` first. Every visible row
is copied whole to draw it, although drawing stops at the right edge
(`crates/nun-ui/src/view.rs:299` → `crates/nun-core/src/buffer.rs:352`). The
status bar calls `column_of` every frame, which copies the caret's line and
segments it from the start (`buffer.rs:358-362`). Vertical movement copies two
lines per caret per keypress (`buffer.rs:736-739`), and so do adding carets
vertically and column selection (`buffer.rs:943`, `1203`, `1247`).

For ordinary code this is invisible. A minified bundle or a JSON log with a
10 MB line turns each of these into ten megabytes of copying and grapheme
segmentation per frame and per keystroke.

## Proposal

Walk graphemes over a `RopeSlice` — `grapheme::next_boundary_in` already does —
and stop as soon as the answer is known: at the viewport's right edge for
drawing, at the caret for a column, at the target column for movement. Cache the
caret's display column per revision for the status bar.

## Acceptance criteria

- [ ] No per-frame or per-keystroke path allocates a whole line
- [ ] Bench: frame and vertical movement on a 10 MB single line each under 2 ms
- [ ] Wide, combining and tab-containing lines still measure correctly (text-edge-cases review)
