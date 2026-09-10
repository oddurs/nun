---
id: 35
title: Go to definition and find references
type: feature
status: backlog
milestone: m4
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: lsp
---

## Problem

Navigation is the reason to run a language server at all.

## Proposal

Ctrl-click jumps to the definition, and holding ctrl underlines the symbol under
the pointer to advertise that it is clickable. References open in the panel as a
grouped, clickable list. A jump list makes navigation reversible.

## Acceptance criteria

- [ ] Ctrl-hover underlines only symbols that actually have a definition
- [ ] Multiple definition results present a chooser rather than picking one
- [ ] Back and forward traverse the jump list across files and panes
- [ ] Alt-ctrl-click opens the definition in a split
