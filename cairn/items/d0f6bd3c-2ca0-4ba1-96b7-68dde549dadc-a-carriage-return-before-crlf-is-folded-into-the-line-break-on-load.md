---
id: d0f6bd3c-2ca0-4ba1-96b7-68dde549dadc
title: A carriage return before CRLF is folded into the line break on load
type: bug
status: backlog
milestone: m6
created: 2026-09-24
updated: 2026-09-24
priority: p2
effort: s
area: core
---

## What happens

Loading text containing `\r\r\n` turns it into `\r\n`, which leaves a lone
`\r` and a line break as one two-character cluster across the break (found
by the text-edge-cases review of 0043). Restored and opened files with
mixed LF and CRLF endings also skip the "mixed line endings" warning when
opened from the tree or restored from a session.

## Acceptance criteria

- [ ] `\r\r\n` round-trips byte for byte, with the lone `\r` kept as content
- [ ] Every way of opening a file warns about mixed line endings the same way
