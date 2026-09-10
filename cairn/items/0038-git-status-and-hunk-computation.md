---
id: 38
title: Git status and hunk computation
type: feature
status: backlog
milestone: m5
depends_on:
- 37
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: vcs
---

## Problem

The gutter needs per-line change state cheaply enough to recompute while typing.

## Proposal

`gix` for status and blob lookup, `imara-diff` for hunks between the index blob
and the buffer as it currently stands — not as it is on disk, so marks track
what is actually on the screen. Recompute is debounced and off-thread.

## Acceptance criteria

- [ ] Hunks reflect unsaved buffer state, not the file on disk
- [ ] A large repository does not stall the editor on open
- [ ] Correct in a worktree, a submodule, and a detached HEAD
- [ ] Absence of git is not an error; marks are simply absent
