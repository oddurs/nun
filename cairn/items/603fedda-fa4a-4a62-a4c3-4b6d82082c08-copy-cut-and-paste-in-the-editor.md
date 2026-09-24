---
id: 603fedda-fa4a-4a62-a4c3-4b6d82082c08
title: Copy, cut and paste in the editor
type: feature
status: backlog
milestone: m6
created: 2026-09-24
updated: 2026-09-24
priority: p0
effort: m
area: core
---

## Problem

The editor has no copy, cut or paste command. There is no key, no palette
entry and no menu item: text can reach a buffer only through the terminal's
own bracketed paste, and nothing can take text out of one. The terminal
panel copies (0089), and its routing — system clipboard, OSC 52, tmux —
lives in `crates/nun/src/clipboard.rs`, ready for the editor to use.

## Proposal

Copy, Cut and Paste commands on the platform keys (Ctrl/Cmd+C, X, V, with
the basic key set's Ctrl bindings checked for clashes), in the palette and
in the text's right-click menu. Copy and Cut go through `clipboard.rs`, so
they work locally, in tmux and over ssh the way the panel's copy does, and
report where the text went rather than assuming. Paste reads the system
clipboard where it can and falls back to asking the terminal to paste.

## Acceptance criteria

- [ ] Copy, Cut and Paste work from keys, the palette and the right-click menu
- [ ] With several carets, each caret's selection is one entry, and pasting as many entries as carets puts one at each
- [ ] Cut with an empty selection cuts the line, and Copy copies it
- [ ] Copy and Cut report where the text went, through `clipboard.rs`, and never claim success they did not have
- [ ] Cut and Paste are each one undo step
