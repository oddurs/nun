---
id: 34
title: Hover cards
type: feature
status: backlog
milestone: m4
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: s
area: lsp
---

## Problem

The mouse is already over the symbol; asking for a keystroke to learn what it
is undoes the premise.

## Proposal

Hover for 400 ms over a symbol shows the LSP hover card, with the markdown
rendered rather than dumped. The card is scrollable and does not steal focus.

## Acceptance criteria

- [ ] Delay is configurable, and moving away cancels the pending request
- [ ] Markdown renders: code blocks highlighted, links via OSC 8
- [ ] The card is positioned to stay on screen near an edge
- [ ] It never covers the symbol it describes
