---
id: 7ee1aab2-58e8-4323-9c96-ca2669588d08
title: Flush pending mouse input before leaving raw mode
type: bug
status: done
milestone: m2
assignee: Oddur Sigurdsson
created: 2026-09-19
updated: 2026-09-19
priority: p1
effort: s
area: ui
---

## What happens

Mouse reports already in flight when nun exits sit in the tty input queue. Once
raw mode is off the shell reads them and prints `^[[<35;…M` junk at the prompt.
It is most likely with any-motion tracking (1003) live — a hover target on
screen while the pointer moves — but a drag in progress at exit does it too.

## What should happen

On every exit path (guard drop, panic hook, signal), after mouse reporting is
disabled and before raw mode is left, discard pending input with
`tcflush(fd, TCIFLUSH)`.

`unsafe_code` is forbidden, so this needs a safe wrapper — `rustix::termios::tcflush`
is the likely one. That is a new dependency and needs a decision first.

## Reproduction

1. Once a hover target exists (0016), open a file and keep moving the pointer
   over it.
2. Quit with Ctrl+Q while it is still moving.
3. Escape-sequence junk appears at the shell prompt.

## 2026-09-19

Uses rustix::termios::tcflush, which is already in the build through crossterm, so no new crate. Flushed only when mouse reporting was on, so a user typing ahead of a no-mouse session keeps their keystrokes. A report the terminal generates after the flush but before it has processed the disable can still slip through; that window is a few microseconds on a local terminal.
