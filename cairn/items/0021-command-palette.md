---
id: 21
title: Command palette
type: feature
status: backlog
milestone: m2
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: l
area: ui
---

## Problem

One palette with prefixes beats six separate dialogs, and it is the only
discovery surface the editor needs.

## Proposal

A single overlay whose mode is chosen by a prefix character: files by default,
`>` commands, `@` symbols in file, `#` workspace symbols, `:` line, `?` help.
`nucleo` does the matching, off the main thread, cancelled on each keystroke.

Every command shows its current binding, so the palette is also the keymap
reference and there is nothing separate to document.

## Acceptance criteria

- [ ] File mode is responsive on a 100k-file repository
- [ ] Matched characters are highlighted in the results
- [ ] Results are keyboard and mouse navigable, including the wheel
- [ ] Alt-enter opens in a split
- [ ] Frecency ranks recently and often opened files above cold matches
