---
id: 98e7cf7d-4a57-4a62-8bd9-5346b7a2bf5f
title: Benchmarks and a release-mode perf job
type: chore
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p0
effort: m
area: perf
---

## Problem

Nothing in the repository measures speed. There are no benches, and the few
timing checks that exist (`nun-ui/tests/render.rs`, `nun-core/tests/carets.rs`,
`nun-syntax/tests/parsing.rs`, and a handful more) assert wall-clock bounds
inside `cargo test`, under the dev profile at `opt-level = 1`. They are loose
enough to pass on a slow machine and noisy enough to fail on a busy one, so
they catch neither regressions nor wins.

Every other item in this milestone needs a before and an after. Without a
harness each of them argues from intuition.

## Proposal

A `scripts/task bench` target that builds in release and runs a fixed set of
benches over committed reference inputs:

- keystroke: one character inserted into a 10k-line Rust file, event to frame
- frame: a full render of a populated view into a buffer backend
- load: open a 100 MB file, and a file with one 10 MB line
- reparse: an incremental reparse after one edit, and after a multi-caret edit
- carets: an edit with 1k carets, and select-all-occurrences on a large file

CI runs it as its own job through `scripts/task`, never through cargo directly.
Each bench has an absolute ceiling that fails the build; relative comparison
against `main` is reported but does not gate, because shared runners are too
noisy to gate on a few percent.

Move the existing wall-clock assertions out of the test suite into the benches,
so `cargo test` goes back to asserting behaviour only.

A new dependency (criterion or similar) needs its case made in the PR; a small
`Instant`-and-percentiles harness may well be enough.

## Acceptance criteria

- [ ] `scripts/task bench` runs every bench above in release and prints p50, p99 and max
- [ ] CI runs it as a separate job and fails when a ceiling is crossed
- [ ] No test under `cargo test` asserts on elapsed time
- [ ] Reference inputs are committed, generated deterministically, or both
