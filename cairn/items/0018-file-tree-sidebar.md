---
id: ebdf64de-95b1-4b06-bab6-562f9f962383
title: File tree sidebar
type: feature
status: done
milestone: m2
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-23
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

- [x] Opens a 100k-file repository without a visible pause
- [x] Respects .gitignore, with a toggle to show ignored files
- [x] Create, rename, delete and move, each undoable
- [x] External filesystem changes appear without a manual refresh
- [x] Git status colours rows from nun-vcs once that exists

## 2026-09-19

Model half landed in crates/nun-workspace (FileTree, FsHistory, Watcher). Trash under the platform state dir is never emptied yet; it needs a purge policy (for example, entries older than N days at startup) once the binary wires FsHistory in.

## 2026-09-19

The model is nun-workspace: a lazily loaded FileTree (one directory level per expand, through the ignore crate so parent .gitignore rules still apply), FsHistory where every operation including delete is undoable (delete moves the entry into nun's own trash under the platform state dir), and a notify Watcher that only posts messages — the main thread stays the single owner of the tree. nun-ui draws it (TreeView, Menu) and the binary wires it up. Criterion 5, git status colours, needs nun-vcs, which is 0038 in m5; noted there.

## 2026-09-19

Listing and file operations run on a worker thread (nun-workspace::Jobs) and come back as messages: FileTree never touches the disk, it marks directories wanted() and takes apply_listing() results. That was a rule 2 violation in the first cut, where expanding a folder or dropping a file read or copied inline on the main thread. FileTree::load_blocking exists for tests and one-shot tools and is documented as such.

## 2026-09-19

Buffer load and save still happen on the main thread, which predates this item (it has been that way since m1) and is now easier to reach from a tree click. Filed as 0053.
