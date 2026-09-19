---
id: 17
title: Click, drag and multi-click selection
type: feature
status: done
milestone: m2
assignee: Oddur Sigurdsson
depends_on:
- 16
created: 2026-09-10
updated: 2026-09-19
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

- [x] Multi-click threshold matches platform convention and is configurable
- [x] Drag-extend by word and by line behaves like every other editor
- [x] Column selection produces one caret per line, skipping short lines correctly
- [x] Autoscroll is smooth and stops at the buffer bounds

## 2026-09-19

Multi-click threshold defaults to 500 ms on macOS and Windows and 400 ms elsewhere (GTK); neither system setting is readable from inside a terminal, so ui.double_click_ms is the override. Word boundaries are code-style (runs of word chars, whitespace, or one repeated symbol) rather than UAX #29, which joins self.value. Alt-click adds a caret; Alt-drag replaces the selection with a column. Autoscroll is deadline-driven: one line per 100 ms at one row past the edge, down to 16 ms further out, and it stops at the buffer's ends. Found and fixed on the way: undo of any multi-edit revision (typing at two carets) replayed inverses in the wrong coordinates and corrupted the text, and overlapping deletes from carets inside one cluster tripped the disjointness assertion.
