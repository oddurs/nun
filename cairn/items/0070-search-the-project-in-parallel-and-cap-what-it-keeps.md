---
id: 8e5cb8ea-873c-4ee9-8354-792beb98b79b
title: Search the project in parallel, and cap what it keeps
type: chore
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p2
effort: m
area: workspace
---

## Problem

The grep walk is a serial `ignore::Walk` (`crates/nun-workspace/src/grep.rs:310-318`),
and there is no cap on the number of hits held in memory. Searching for `e` in a
large monorepo is slow and unbounded.

## Proposal

Use `build_parallel()`, keeping result order stable per file. Stop collecting
past a limit and show a "more results" row that continues on demand.

## Acceptance criteria

- [ ] Bench: search over a large fixture tree scales with cores
- [ ] Hits held in memory are capped; the cap is visible in the results panel
- [ ] Mouse: clicking the "more results" row continues the search
