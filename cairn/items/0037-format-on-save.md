---
id: 37
title: Format on save
type: feature
status: backlog
milestone: m4
created: 2026-09-10
updated: 2026-09-22
priority: p1
effort: s
area: lsp
---

## Problem

Formatting on save is where an editor most often loses a cursor position or,
worse, some text.

## Proposal

Request formatting, apply the returned edits, then restore selections by mapping
them through those edits rather than by absolute offset. A server that fails or
times out leaves the file unformatted and saves it anyway, with a notice.

## Acceptance criteria

- [x] Caret and selections land where a human would expect after reformatting
- [x] Timeout does not block the save
- [ ] Configurable per language, and per project — per language is done
      (`[lsp.<language>] format_on_save`); per project waits on the project
      layer of 0042 and is 0075
- [x] Format-on-save off by default for languages with no stable formatter

## 2026-09-22

Save is two steps: request formatting, then save when the answer comes, the server's timeout (3 s) passes, or a 1 s grace deadline of the editor's own passes. Text edited while formatting is in flight drops the formatting and saves the current text unformatted, with a notice, rather than re-requesting (which could chase typing indefinitely). Quitting, or a signal, saves anything still waiting, unformatted. Edits go through Buffer::apply_batch (nun-core), built for rename (0036) to reuse. Per-project config is 0075, after 0042.
