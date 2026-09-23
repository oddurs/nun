---
id: 36
title: Rename
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-23
closed_at: 2026-09-23
priority: p1
effort: m
area: lsp
---

## Problem

Rename touches files that are not open, which makes it the second most
destructive operation after project replace.

## Proposal

Preview every edit grouped by file before anything is written, in the same panel
find-and-replace uses. Applying is one undo unit across all files.

## Acceptance criteria

- [x] Prepare-rename is honoured where the server supports it
- [x] Every affected file is previewed, including unopened ones
- [x] Undo restores all of them
- [x] A partial failure mid-apply is reported with exactly what was written

## 2026-09-23

Design: open files are edited through Buffer::apply_batch (one undo step each, not saved); files that are not open are read and written on the workspace worker (Job::Read / Job::Rewrite), each write guarded by the exact text that was previewed, all checked before any is written. 'Undo rename' (status-line button, palette, Ctrl+K Z) takes back both: it checks every buffer still holds what the rename left and every written file still holds what was written, then rewrites the files back and undoes each buffer's step, verifying the undo reversed exactly the rename. A WorkspaceEdit is taken in both forms; stale versions, file operations, a file named twice, overlapping edits and bare-CR text refuse the whole edit. The client now declares documentChanges with no resource operations. Preview and write come from one splice that mirrors apply_batch's ordering rules, held together by a property test.
