---
id: 14
title: Restore the terminal on panic and signal
type: chore
status: done
milestone: m1
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-11
priority: p0
effort: s
area: ui
---

## Problem

A TUI that panics in raw mode leaves the user with a dead shell and no echo.
This is the single rudest failure mode a terminal program has, and it has to be
handled before anyone else runs the binary.

## Proposal

Install a panic hook and a signal handler that leave raw mode, disable mouse
reporting and the keyboard protocol, show the cursor, and leave the alternate
screen — then print the panic normally.

## Acceptance criteria

- [ ] A deliberate panic leaves a usable shell and a readable backtrace
- [ ] SIGTERM and SIGHUP restore the terminal before exiting
- [ ] SIGTSTP suspends cleanly and resumes with the screen intact
- [ ] Any pushed keyboard-protocol flags are popped exactly once

## 2026-09-11

TerminalGuard is generic over a TerminalControl trait so exactly-once teardown, reverse ordering, and unwind-on-partial-failure are all asserted against a recording fake with no tty. The panic path additionally uses a process-wide counter so keyboard flags cannot be double-popped.
