---
id: f520259c-9f2e-48f8-a053-aaa4879c66ee
title: Structural selection
type: feature
status: done
milestone: m3
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-22
priority: p2
effort: s
area: syntax
---

## Problem

Expanding a selection by syntax node is the one genuinely better selection
primitive modal editors have, and it needs no modes.

## Proposal

Grow the selection to the parent node, shrink back to the previous child.
On the mouse, ctrl-double-click grows from the node under the pointer.

## Acceptance criteria

- [x] Grow and shrink are exact inverses along one path
- [x] Works with multiple selections independently
- [x] Sensible on an empty selection: starts from the node under the caret

## 2026-09-22

Grow is asked of the syntax worker (the tree lives there), shrink walks back a trail of the selections each grow started from, which is what makes them exact inverses. architecture-guard found: an answer to a grow abandoned by a shrink was taken for a later grow from the same caret (grows now carry a serial); the click counter wraps to 1 on the fourth Ctrl-click, which started a move-drag instead of growing; a grow asked while a stale one was out was counted and then dropped with it; the last key of a chord was handed to a focused panel as if pressed alone.
