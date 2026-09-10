---
id: 30
title: Multi-cursor and column selection
type: feature
status: backlog
milestone: m3
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: m
area: core
---

## Problem

The model is plural from m1; this is where the commands that create and manage
carets arrive.

## Proposal

Add-next-occurrence, add-all-occurrences, add-caret-above and below, and split
selection into lines. Alt-click and alt-drag are the mouse equivalents. Escape
collapses to the primary caret.

## Acceptance criteria

- [ ] Every editing command is caret-count agnostic
- [ ] Carets that collide after an edit merge without losing the primary
- [ ] The primary caret is visually distinguishable from the rest
- [ ] Typing with 500 carets stays within the frame budget
