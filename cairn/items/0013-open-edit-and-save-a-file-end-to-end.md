---
id: f49531b0-f577-4417-8bf2-eeaf8ebde784
title: Open, edit and save a file end to end
type: feature
status: done
milestone: m1
assignee: Oddur Sigurdsson
depends_on:
- 8f973ca1-e168-4056-9965-9f042259019b
- bc1e4abe-f895-406f-a77f-e0c51270d32a
- d72c3db0-debe-45e8-81ed-cbbda5b3cf35
- f546b22d-2b57-4f4b-91da-471996db2185
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
