---
id: 25
title: Folding
type: feature
status: backlog
milestone: m3
depends_on:
- 22
created: 2026-09-10
updated: 2026-09-19
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

- [ ] Arrows appear only on genuinely foldable nodes
- [ ] Cell-to-buffer mapping stays correct across folds
- [ ] A fold containing the caret unfolds when the caret is moved into it
- [ ] Folds persist across a restart

## 2026-09-19

0016's cell-to-position mapping (EditorView::position_at, which shares cells_of with drawing) has to learn about folded ranges when folding lands: a click on a fold marker, and on rows below a fold, must map through it. 0016 left that criterion open for this item.
