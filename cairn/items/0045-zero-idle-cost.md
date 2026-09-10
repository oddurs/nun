---
id: 45
title: Zero idle cost
type: chore
status: backlog
milestone: m6
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: s
area: perf
---

## Problem

A terminal editor that wakes the CPU while nothing is happening is a laptop
battery problem, and it is invisible until someone measures it.

## Proposal

Assert it: 0% CPU and no wakeups with the editor open, focused, and untouched,
including with language servers attached and cursor blink on.

## Acceptance criteria

- [ ] No polling loops anywhere; every wait is a blocking receive
- [ ] Cursor blink does not repaint the whole frame
- [ ] Measured with the editor idle for 60 s and recorded in the PR
