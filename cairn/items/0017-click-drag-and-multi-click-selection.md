---
id: 17
title: Click, drag and multi-click selection
type: feature
status: backlog
milestone: m2
depends_on:
- 16
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: input
---

## Problem

This is the premise of the whole editor. If it does not feel right, nothing
else matters.

## Proposal

Click places the caret; shift-click extends from the anchor. Double and triple
click select word and line, and dragging after either extends by that unit
rather than by character. Alt-click adds a caret, alt-drag makes a column
selection. Dragging an existing selection moves the text, with ctrl to copy.

Autoscroll when a drag reaches the viewport edge, at a rate proportional to how
far past the edge the pointer is.

## Acceptance criteria

- [ ] Multi-click threshold matches platform convention and is configurable
- [ ] Drag-extend by word and by line behaves like every other editor
- [ ] Column selection produces one caret per line, skipping short lines correctly
- [ ] Autoscroll is smooth and stops at the buffer bounds
