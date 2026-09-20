---
id: 30
title: Multi-cursor and column selection
type: feature
status: doing
milestone: m3
assignee: Oddur Sigurdsson
claimed: 2026-09-19
created: 2026-09-10
updated: 2026-09-19
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

- [x] Every editing command is caret-count agnostic
- [x] Carets that collide after an edit merge without losing the primary
- [x] The primary caret is visually distinguishable from the rest
- [x] Typing with 500 carets stays within the frame budget
