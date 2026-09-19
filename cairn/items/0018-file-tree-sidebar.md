---
id: 18
title: File tree sidebar
type: feature
status: doing
milestone: m2
assignee: Oddur Sigurdsson
claimed: 2026-09-19
created: 2026-09-10
updated: 2026-09-19
priority: p0
effort: l
area: workspace
---

## Problem

The sidebar is the second thing anyone touches and the first place a mouse-first
editor can prove itself.

## Proposal

A virtualised tree over `ignore`, so a repository with a large `target/` opens
instantly. Rows expand on click, files open on click, and drag moves a file on
disk with an undo toast. `notify` keeps it live.

## Acceptance criteria

- [ ] Opens a 100k-file repository without a visible pause
- [ ] Respects .gitignore, with a toggle to show ignored files
- [ ] Create, rename, delete and move, each undoable
- [ ] External filesystem changes appear without a manual refresh
- [ ] Git status colours rows from nun-vcs once that exists

## 2026-09-19

Model half landed in crates/nun-workspace (FileTree, FsHistory, Watcher). Trash under the platform state dir is never emptied yet; it needs a purge policy (for example, entries older than N days at startup) once the binary wires FsHistory in.
