---
id: 0c6cbc3a-54bc-407a-ba45-0ff19b1a12fa
title: Open large files without tripling their size in memory
type: chore
status: backlog
milestone: perf
depends_on:
- 96804f61-ec66-4c00-be35-042193b2b370
- 98e7cf7d-4a57-4a62-8bd9-5346b7a2bf5f
created: 2026-09-22
updated: 2026-09-22
priority: p0
effort: m
area: core
---

## Problem

Loading reads the whole file, scans it three times for encoding and line
endings (`crates/nun-core/src/buffer.rs:189-191`), copies it again when there
are no CRLFs (`buffer.rs:196`), and only then builds the rope (`buffer.rs:200`).
Peak memory is about three times the file, and nothing refuses or warns: a 1 GB
log is several seconds and three gigabytes. Syntax offsets are `u32` and clamp
silently past 4G chars (`crates/nun/src/app/syntax.rs:375`).

## Proposal

Stream into the rope in chunks, deciding line endings and encoding from a
prefix and carrying the decoder across chunk boundaries. Above a size threshold,
open without syntax and say so; above a second, ask before opening at all. The
threshold should keep every file under the `u32` limit away from syntax.

## Acceptance criteria

- [ ] Peak memory while loading stays under 1.5× the file size (measured in the bench)
- [ ] Bench: a 100 MB file opens in under 1 s on the reference machine
- [ ] Oversized files open without syntax, with the reason in the status line
- [ ] BOM, CRLF, mixed endings and a multibyte character split across a chunk boundary all round-trip
