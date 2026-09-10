---
id: 37
title: Format on save
type: feature
status: backlog
milestone: m4
created: 2026-09-10
updated: 2026-09-10
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

- [ ] Caret and selections land where a human would expect after reformatting
- [ ] Timeout does not block the save
- [ ] Configurable per language, and per project
- [ ] Format-on-save off by default for languages with no stable formatter
