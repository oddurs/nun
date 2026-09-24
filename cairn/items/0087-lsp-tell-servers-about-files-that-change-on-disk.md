---
id: 87
title: 'LSP: tell servers about files that change on disk'
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
created: 2026-09-23
updated: 2026-09-24
closed_at: 2026-09-24
priority: p2
effort: m
area: lsp
---

## Problem

nun doesn't declare or send `workspace/didChangeWatchedFiles`, apart from the
files a workspace edit itself writes, creates, moves or deletes (0078). A
server that expects its client to watch files, like rust-analyzer by
default, never hears about any other change on disk: a `git checkout`, a
save from another editor, or a move in the file tree. In a real rust-analyzer
session, a module file moved outside the server's knowledge left an
"unresolved module" error that stayed after saving.

The paths 0078 reports are written the way the folder was opened, so a
folder opened through a symbolic link (every macOS temporary folder is one)
names them differently from the server.

## Proposal

Declare `workspace.didChangeWatchedFiles` with dynamic registration, honour
the server's `client/registerCapability` glob patterns, and feed the
workspace watcher's changes into them, coalesced. Report paths the way the
server spells them.

## Acceptance criteria

- [x] A file moved in the tree is reported to the server that watches it
- [x] A change made outside nun is reported, coalesced, within a second
- [x] Reported paths match how the server spells them, including through a symbolic link
