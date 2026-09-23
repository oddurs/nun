---
id: 45
title: Zero idle cost
type: chore
status: backlog
milestone: perf
created: 2026-09-10
updated: 2026-09-22
priority: p0
effort: s
area: perf
---

## Problem

A terminal editor that wakes the CPU while nothing is happening is a laptop
battery problem, and it is invisible until someone measures it.

## Proposal

Assert it: 0% CPU and no wakeups with the editor open, focused, and untouched,
including with language servers attached.

The main loop already gets this right: with nothing pending it blocks on
`recv`, and otherwise sleeps until the earliest real deadline — hover, chord,
autoscroll, the syntax debounce, search (`crates/nun/src/app.rs:533-545`).
There is no cursor blink; the caret is a painted cell. What is missing is a
guarantee that it stays this way, and a check of the threads beside it: the
syntax worker, the jobs worker, grep, the file watcher, and the language
servers once they land.

## Acceptance criteria

- [ ] No polling loops anywhere, on any thread; every wait is a blocking receive or a real deadline
- [ ] A test asserts that an idle editor has no pending deadline
- [ ] Measured with the editor idle for 60 s, with and without a language server attached, and recorded in the PR
- [ ] If a cursor blink is ever added, it repaints only the caret cell and stops when unfocused
