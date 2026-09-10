---
id: 29
title: Find and replace with preview
type: feature
status: backlog
milestone: m3
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: m
area: workspace
---

## Problem

Project-wide replace is the most destructive thing an editor does, and it
usually offers the least visibility before it happens.

## Proposal

Every hit is previewed as a before/after pair and individually toggleable. The
whole replace is one undo unit per file, and the panel keeps the list afterwards
so single hits can still be reverted by clicking them.

## Acceptance criteria

- [ ] Capture groups in the replacement work and are previewed accurately
- [ ] Individual hits can be excluded before applying
- [ ] Undo restores every file the replace touched
- [ ] Files changed on disk since the search are re-checked before writing
