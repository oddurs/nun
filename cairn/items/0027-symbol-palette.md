---
id: 27
title: Symbol palette
type: feature
status: backlog
milestone: m3
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: s
area: ui
---

## Problem

Jumping within a file should not require scrolling or a search.

## Proposal

`@` in the palette lists symbols from the tree-sitter tree, indented by nesting.
Once LSP lands, `#` does the same across the workspace via the language server.

## Acceptance criteria

- [ ] Symbols are derived from the tree, so no language server is required
- [ ] Nesting is visible, and filtering keeps ancestors of a match
- [ ] Selecting a symbol scrolls it into view with context above it
