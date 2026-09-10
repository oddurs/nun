---
id: 36
title: Rename
type: feature
status: backlog
milestone: m4
created: 2026-09-10
updated: 2026-09-10
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

- [ ] Prepare-rename is honoured where the server supports it
- [ ] Every affected file is previewed, including unopened ones
- [ ] Undo restores all of them
- [ ] A partial failure mid-apply is reported with exactly what was written
