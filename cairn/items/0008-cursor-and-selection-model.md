---
id: 8
title: Cursor and selection model
type: feature
status: planned
milestone: m1
depends_on:
- 7
created: 2026-09-10
updated: 2026-09-10
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
