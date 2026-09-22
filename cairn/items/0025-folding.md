---
id: 25
title: Folding
type: feature
status: done
milestone: m3
assignee: Oddur Sigurdsson
depends_on:
- 22
created: 2026-09-10
updated: 2026-09-22
priority: p1
effort: m
area: syntax
---

## Problem

Long files need collapsing, and the tree already knows where the foldable
regions are.

## Proposal

Fold ranges from tree-sitter node boundaries. A click on the gutter arrow folds;
alt-click folds every sibling at that depth. Folds survive edits outside them and
are part of the session.

## Acceptance criteria

- [x] Arrows appear only on genuinely foldable nodes
- [x] Cell-to-buffer mapping stays correct across folds
- [x] A fold containing the caret unfolds when the caret is moved into it
- [x] Folds persist across a restart

## 2026-09-19

0016's cell-to-position mapping (EditorView::position_at, which shares cells_of with drawing) has to learn about folded ranges when folding lands: a click on a fold marker, and on rows below a fold, must map through it. 0016 left that criterion open for this item.

## 2026-09-22

Regions come from the tree with no per-language query: the multi-line named node starting furthest right on a line (what the line opens), minus bare containers that start on their first child (Python's block), stopping a line short of the next header (`} else {`) or of a clause back at the header's indentation after an indented body (Python's else/except). Folds are char offsets in the buffer, mapped in apply_to_rope; an edit reaching in, or a newline typed at the header's end, opens the fold. Session file: $XDG_STATE_HOME/nun/session, canonical paths, merged on save so two processes do not clobber each other; unsaved line numbers are not remembered. Review (architecture-guard, text-edge-cases) found eleven bugs, all fixed with tests.
