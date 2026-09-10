---
id: 23
title: Tree-sitter integration and incremental reparse
type: feature
status: backlog
milestone: m3
depends_on:
- 22
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: l
area: syntax
---

## Problem

Regex highlighting is wrong on any file large or nested enough to matter, and
folding and structural selection need a real tree regardless.

## Proposal

Parse on a worker thread, debounced at 12 ms, feeding the previous tree and the
edit for incremental reparse. Highlighting reads the last good tree while a new
one is in flight, so fast typing never flickers or goes grey.

Grammars are compiled in for the initial language set rather than loaded at
runtime — no plugin runtime is in scope for v0.1.

## Acceptance criteria

- [ ] Reparse of a 10k-line file stays under one frame
- [ ] Highlighting never blanks or flickers during sustained typing
- [ ] Injections work: SQL in a Rust string, CSS in HTML
- [ ] A grammar that panics or hangs is contained and disabled, not fatal
