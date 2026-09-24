---
id: bc1e4abe-f895-406f-a77f-e0c51270d32a
title: Cursor and selection model
type: feature
status: done
milestone: m1
assignee: Oddur Sigurdsson
depends_on:
- 8f973ca1-e168-4056-9965-9f042259019b
created: 2026-09-10
updated: 2026-09-11
priority: p0
effort: m
area: core
---

## Problem

Multi-cursor bolted on later forces a rewrite of every edit path. The model
needs to be plural from the first commit even though the UI is not.

## Proposal

A `Selections` set that is always non-empty, always sorted, and self-merging
when two ranges overlap after an edit. A single caret is the degenerate case of
one empty range, so there is no separate code path.

## Acceptance criteria

- [ ] Range with anchor and head; direction preserved through edits
- [ ] Overlapping ranges merge deterministically after every apply
- [ ] Grapheme-cluster movement, not byte or char movement
- [ ] Sticky column survives vertical movement across short lines
- [ ] Tested against emoji, combining marks and CJK width

## 2026-09-11

Range holds the sticky column, and PartialEq deliberately ignores it — two ranges covering the same text are the same selection regardless of where vertical movement is aiming.
