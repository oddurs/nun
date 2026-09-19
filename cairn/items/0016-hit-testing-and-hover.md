---
id: 16
title: Hit-testing and hover
type: feature
status: done
milestone: m2
assignee: Oddur Sigurdsson
depends_on:
- 15
created: 2026-09-10
updated: 2026-09-19
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

- [x] Resolution is O(log n) or better in the number of regions
- [x] Overlays such as the palette correctly capture clicks beneath them
- [x] Hover enter/leave fire exactly once per crossing
- [ ] Cell-to-buffer-position mapping is correct across tabs, wide chars and folds

## 2026-09-19

0015 added Screen::track_motion(bool): call it each frame with whether a hover target is on screen. 1003 support is not probed; a terminal without it simply sends no hover. If that should be detected rather than silently absent (rule 6), DECRQM CSI ?1003$p answers unambiguously on at least iTerm2; treat a timeout as unknown and still enable.

## 2026-09-19

Criterion 4 is met for tab characters, wide characters, combining marks and emoji, including a property test that position_at round-trips nun-core's column_of. Folds do not exist yet (0025, m3), so that part is left open and noted on 0025.
