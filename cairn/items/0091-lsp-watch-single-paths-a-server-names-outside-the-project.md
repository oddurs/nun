---
id: 91
title: 'LSP: watch single paths a server names outside the project'
type: feature
status: backlog
milestone: m6
created: 2026-09-24
updated: 2026-09-24
priority: p3
effort: s
area: lsp
---

## Problem

0087 watches the folders a server registers for `didChangeWatchedFiles`,
recursively, minus what the ignore rules leave out. A pattern naming one path
outright, with no wildcard, is told of only when that path is inside a folder
watched for another pattern. rust-analyzer registers its settings folder that
way (`~/Library/Application Support/rust-analyzer` on macOS), and watching
everything beside it for one name would mean watching all of
`Application Support`. So a change to rust-analyzer's user settings while nun
runs goes unheard until the server restarts.

## Proposal

Watch such a path on its own: the path itself when it is a folder, and its
parent non-recursively, filtered to that one name, when it is a file or does
not exist yet. `DiskWatcher` would need a second kind of watch alongside its
recursive folders.

## Acceptance criteria

- [ ] A change to a single path a server names outside its project reaches it
- [ ] Nothing beside that path is read or watched
