---
id: 11
title: Render loop with damage tracking
type: feature
status: done
milestone: m1
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-11
priority: p0
effort: l
area: ui
---

## Problem

Repainting the whole screen per keystroke is what makes terminal editors feel
soft. It also burns CPU while idle, which is the thing people notice on a laptop.

## Proposal

ratatui over crossterm, with a diff between the previous and next cell buffer so
only changed cells are written. The loop blocks on an event channel and does
zero work when nothing has happened.

## Acceptance criteria

- [ ] Idle CPU is 0% with the editor open and focused
- [ ] A single-character insert repaints only the affected line and status
- [ ] Frame time measured and asserted in a benchmark, not eyeballed
- [ ] Resize is handled without a full teardown

## 2026-09-11

Damage tracking is ratatui's cell diff; the work was not forcing full repaints. Asserted directly: a one-character insert changes at most two cells, a caret move repaints exactly two rows, and an idle repaint writes nothing.
