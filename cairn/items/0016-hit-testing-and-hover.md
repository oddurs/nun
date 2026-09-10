---
id: 16
title: Hit-testing and hover
type: feature
status: backlog
milestone: m2
depends_on:
- 15
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: input
---

## Problem

Every region of the screen has to answer "what is at this cell", or nothing else
about the mouse can be built.

## Proposal

The layout pass records a z-ordered list of regions with their owning widget.
A point resolves to the topmost region, which then maps the cell to something
meaningful — a buffer position, a tree row, a tab, a gutter mark, a divider.

Hover is derived from the same resolution, with enter and leave events and a
per-target dwell delay.

## Acceptance criteria

- [ ] Resolution is O(log n) or better in the number of regions
- [ ] Overlays such as the palette correctly capture clicks beneath them
- [ ] Hover enter/leave fire exactly once per crossing
- [ ] Cell-to-buffer-position mapping is correct across tabs, wide chars and folds
