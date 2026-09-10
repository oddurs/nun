---
id: 26
title: Structural selection
type: feature
status: backlog
milestone: m3
created: 2026-09-10
updated: 2026-09-10
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

- [ ] Grow and shrink are exact inverses along one path
- [ ] Works with multiple selections independently
- [ ] Sensible on an empty selection: starts from the node under the caret
