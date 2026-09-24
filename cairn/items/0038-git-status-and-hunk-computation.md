---
id: 38
title: Git status and hunk computation
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
depends_on:
- 37
created: 2026-09-10
updated: 2026-09-23
closed_at: 2026-09-23
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

- [x] Hunks reflect unsaved buffer state, not the file on disk
- [x] A large repository does not stall the editor on open
- [x] Correct in a worktree, a submodule, and a detached HEAD
- [x] Absence of git is not an error; marks are simply absent

## 2026-09-19

0018 left the file tree's rows uncoloured by git status: its criterion 5 depends on this item. Once nun-vcs can report per-path status, colour nun-ui's TreeView rows from it (Role::Added / Changed / Removed) and tick 0018's last criterion.

## 2026-09-23

Done as the nun-vcs crate, tested against real repositories in crates/nun-vcs/tests/repos.rs. The editor starts `Vcs` and colours the file tree from its status (0018's last criterion), refreshed on open, save, a watcher change and focus regained; the tree draws before the walk finishes. Hunks, staging and compare are implemented and tested but nothing in the editor asks for them yet: 0039 wires them into the gutter.
