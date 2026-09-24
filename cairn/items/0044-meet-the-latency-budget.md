---
id: f1909df7-0b28-460b-856f-871f294291c6
title: Meet the latency budget
type: chore
status: backlog
milestone: perf
depends_on:
- 5feb5467-ec21-48f0-86f6-8cefc73566df
- 98e7cf7d-4a57-4a62-8bd9-5346b7a2bf5f
created: 2026-09-10
updated: 2026-09-22
priority: p0
effort: m
area: perf
---

## Problem

"Feels fast" is not a criterion anyone can hold a change to.

## Proposal

An end-to-end benchmark from input event to flushed frame, asserted in CI on
representative files. The budget is 8 ms at p99 for a single-character insert in
a 10k-line file with syntax and diagnostics active.

The harness and the CI job come from 0057; this item holds the budget itself.
Diagnostics need the language-server work in m4 (0032), which is why this item
depends on it. The syntax-only half can be measured, and fixed, well before
that.

## Acceptance criteria

- [ ] Benchmark exists, runs in CI, and fails the build on regression
- [ ] p99 under 8 ms on the reference file
- [ ] A profile is recorded for the three worst paths, and each is understood
