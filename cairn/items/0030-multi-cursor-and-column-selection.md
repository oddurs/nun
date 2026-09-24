---
id: 86417e95-8c47-4099-802a-d22a25815006
title: Multi-cursor and column selection
type: feature
status: done
milestone: m3
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-22
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

## 2026-09-22

Review round (architecture-guard + text-edge-cases) found: no mouse path for next/all occurrences (fixed with a right-click menu in the text); quadratic costs at many carets — prev/next_grapheme copied the whole line per call, selections were mapped through every edit in turn, and a 1 MB minified line took over ten minutes to select-all; select-all had no bound (now MOST_OCCURRENCES = 10k, refusing past it); secondary carets drew the glyph at about 1.9:1; caret keys were taken from the sidebar and the query; overlapping or cluster-rejected matches hid real ones; split-into-lines left a caret on the line after whole-line selections. Undo now coalesces runs at several carets, so undo granularity no longer depends on caret count. Adjacent occurrences (catcat) cannot both be selected: touching ranges are one selection by design of the model.
