---
id: 13
title: Open, edit and save a file end to end
type: feature
status: done
milestone: m1
assignee: Oddur Sigurdsson
depends_on:
- 7
- 8
- 10
- 11
created: 2026-09-10
updated: 2026-09-11
priority: p0
effort: s
area: core
---

## Problem

The milestone is only real when the pieces meet.

## Proposal

`nun <path>` opens the file, draws it with derived colours, accepts typing and
arrow keys, and writes it back safely — write to a temporary file in the same
directory, fsync, then rename, so an interrupted save cannot truncate the file.

## Acceptance criteria

- [ ] Round-trips a file byte-for-byte when nothing was changed
- [ ] Preserves permissions, and follows symlinks rather than replacing them
- [ ] Refuses to save over a file changed on disk since it was read, and says so
- [ ] Unsaved-changes state is visible in the status line

## 2026-09-11

App holds no terminal I/O: it takes an Event and mutates itself, so the whole keymap, the scrolling and the mouse mapping are tested headlessly. The palette is probed before the input reader starts, because both want raw stdin and only one can have it — a keystroke inside the 120ms probe window is dropped.
