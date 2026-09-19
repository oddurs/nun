---
id: 21
title: Command palette
type: feature
status: done
milestone: m2
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-19
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

- [x] File mode is responsive on a 100k-file repository
- [x] Matched characters are highlighted in the results
- [x] Results are keyboard and mouse navigable, including the wheel
- [x] Alt-enter opens in a split
- [x] Frecency ranks recently and often opened files above cold matches

## 2026-09-19

Both halves of file search are on the worker: the project is listed once with the ignore crate, and each keystroke is scored there with nucleo-matcher, so the editor never waits on a hundred thousand paths. Searches the user has already typed past are skipped rather than answered, and an answer that arrives for an older keystroke is dropped. Frecency is per session; persisting it belongs with session restore (0043). @ and # say that symbols arrive with tree-sitter (m3) and the language servers (m4), which is what those milestones are for. New dependency: nucleo-matcher (MPL-2.0), the matcher the roadmap names.

## 2026-09-19

Fixes from review: the project is walked once and the list kept (it was re-walked on every Ctrl+P, on the same worker as the tree and every file operation); the search generation is monotonic across the editor's life, so a search from a palette just closed can never look newer than one from the palette just opened; rows from an older keystroke cannot be picked while the current answer is in flight; the keyboard goes back where it was on Esc. Alt-click opens a result in a split, as Alt+Enter does, and the rows of the ? list are clickable, which is the mouse path into every other prefix.
