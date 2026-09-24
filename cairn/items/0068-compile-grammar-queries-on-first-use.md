---
id: 468df801-7d66-422a-93c5-0b246a951125
title: Compile grammar queries on first use
type: chore
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p1
effort: s
area: syntax
---

## Problem

Before the first frame, `attach_syntax` calls `language::all()`, which compiles
the highlight queries for all eight grammars on the main thread
(`crates/nun-syntax/src/language.rs:79`, `202-218`, via `crates/nun/src/app/syntax.rs:560`
from `crates/nun/src/main.rs:110`). Opening a Markdown file pays for Rust, and
the cost grows with every grammar added.

## Proposal

Compile a grammar's queries the first time a document needs it, on the syntax
worker, and draw the first frame uncoloured if they are not ready yet.

## Acceptance criteria

- [ ] Only the grammar in use is compiled at startup, and not on the main thread
- [ ] Bench: time to first frame measured and recorded; under 50 ms for a small file
