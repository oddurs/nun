---
id: 69
title: Give project replace its own worker
type: chore
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p1
effort: s
area: workspace
---

## Problem

Project-wide replace runs on the jobs thread (`crates/nun-workspace/src/jobs.rs:243`),
which is the same single thread that lists directories and searches file names.
A large replace freezes the file tree until it finishes.

## Proposal

Run replace on its own thread, reporting progress as messages, so the tree and
file search stay live while it works.

## Acceptance criteria

- [ ] The file tree expands and file search answers while a replace is running
- [ ] Replace reports progress and can be cancelled
