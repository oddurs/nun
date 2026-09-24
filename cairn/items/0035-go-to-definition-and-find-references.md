---
id: df3e7b76-67ea-4377-98bb-cada91231eaf
title: Go to definition and find references
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-22
closed_at: 2026-09-22
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

- [x] Ctrl-hover underlines only symbols that actually have a definition
- [x] Multiple definition results present a chooser rather than picking one
- [x] Back and forward traverse the jump list across files and panes
- [x] Alt-ctrl-click opens the definition in a split

## 2026-09-22

Ctrl-hover needs any-motion reporting, so it is on whenever the focused file's server offers definitions; terminals report Ctrl on motion everywhere but Terminal.app (and iTerm2 is unverified). iTerm2 by default and Terminal.app turn Ctrl-click into a context menu, and iTerm2 drops Option-clicks; F12, Ctrl+K F12 and the text menu cover them. With a server, Ctrl-click on a linkable symbol jumps rather than starting a Ctrl-double-click grow; grow still works elsewhere and from the keys.
