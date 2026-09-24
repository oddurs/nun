---
id: 5feb5467-ec21-48f0-86f6-8cefc73566df
title: Diagnostics and the mark rail
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
depends_on:
- 56f5f8f1-1b29-48b4-b7e1-d06838293421
created: 2026-09-10
updated: 2026-09-22
closed_at: 2026-09-22
priority: p0
effort: m
area: lsp
---

## Problem

Diagnostics are the highest-value thing a language server provides and the
easiest to render badly.

## Proposal

Undercurl in the buffer via `CSI 4:3 m` with a coloured underline where the
terminal supports it, a plain underline where it does not. Severity marks on the
scrollbar rail, clickable to jump. A hover popover with the message and any
quick fixes as clickable buttons.

## Acceptance criteria

- [x] Undercurl capability is detected, with a correct fallback
- [x] Ranges stay attached to the right text as the buffer is edited
- [x] The rail shows density honestly when marks collide at one row
- [x] Counts in the status line match what the rail shows

## 2026-09-22

Undercurl is probed with DECRQSS plus XTGETTCAP Smulx/Setulc, and drawn by NunBackend in nun-ui. ui.undercurl = auto|on|off overrides the probe, and nun --capabilities reports the result. Published ranges are read against the kept text of published.version and mapped through the edits since. Quick fixes are split out as 0077.
