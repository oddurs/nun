---
id: 78
title: 'Rename: carry out file operations a server asks for'
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
created: 2026-09-23
updated: 2026-09-23
closed_at: 2026-09-23
priority: p2
effort: m
area: lsp
---

## Problem

Rename (0036) refuses any workspace edit that would create, rename or delete a
file, and tells servers it supports no resource operations so they do not send
one. Some renames want exactly that: rust-analyzer renaming a module renames its
file, and a TypeScript server can move a file along with its default export.
Those renames are refused today rather than done.

## Proposal

Declare `resourceOperations: ["create", "rename", "delete"]` and carry them out
on the workspace worker through `FsHistory`, so they go into the trash and the
same undo as every other file operation. Preview them in the rename panel as
their own rows, apply them in the order the edit lists them, and fold them into
"Undo rename" so one undo still takes the whole rename back. Open buffers of a
renamed file follow it, as they do for a rename in the tree.

## Acceptance criteria

- [x] A rename that moves a file previews the move and carries it out
- [x] Create and delete are previewed and carried out, never overwriting
- [x] Undo rename takes back the file operations with the text edits
- [x] A failure partway says exactly which operations happened
