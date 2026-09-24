---
id: 73a4f3fd-0f33-47cb-8222-637700daf69b
title: A profiling build and trace spans
type: chore
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p1
effort: s
area: perf
---

## Problem

The release profile sets `strip = true`, so a profile of the shipped binary has
no symbols, and there is no profile that keeps them. Nor is there any way to see
where a slow frame spent its time short of attaching a sampler and guessing.

## Proposal

Add `[profile.profiling]`, inheriting release with `debug = "line-tables-only"`
and `strip = false`, and a `scripts/task profile` target that builds it.

Add trace spans around the stages of a frame — event handling, relayout,
render, flush — and around each worker request (parse, highlight, folds, grep,
jobs), compiled out unless a feature is enabled. Whether that is `tracing` or a
dozen lines of our own is for the PR to argue; the default build must pay
nothing for it.

## Acceptance criteria

- [ ] `scripts/task profile` produces a symbolicated binary that samply or Instruments can read
- [ ] With the feature on, one frame's stages and each worker request show up as spans
- [ ] With the feature off, the release binary is byte-for-byte unaffected in size to within noise
- [ ] CONTRIBUTING or AGENTS.md says how to take a profile
