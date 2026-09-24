---
id: 62de7eb7-38c7-4324-b97c-ce0531e6afd1
title: Mouse event plumbing
type: feature
status: done
milestone: m2
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-19
priority: p0
effort: m
area: input
---

## Problem

Default terminal mouse reporting caps at column 223 and reports presses only.
Neither is enough for an editor driven by dragging.

## Proposal

Enable SGR extended reporting (1006) plus button-motion tracking (1002), and
any-motion (1003) only while a hover target is live, because always-on motion
floods the input stream on a busy terminal.

## Acceptance criteria

- [x] Correct coordinates past column 223 and row 223
- [x] Press, drag, release and wheel distinguished, with modifier state
- [x] Motion tracking is enabled and disabled around hover, not left on
- [x] Reporting is disabled on every exit path, including panic

## 2026-09-19

Mouse entry now writes 1000/1002/1006 by hand; crossterm's EnableMouseCapture also turned on 1003 for the whole session. Any-motion is TerminalGuard::track_motion / Screen::track_motion, off by default, re-applied after suspend, and dropped with ?1003l ?1002h because resetting 1003 turns tracking off entirely in xterm, Ghostty, kitty, iTerm2, Alacritty, foot and tmux (WezTerm keeps independent flags; re-setting 1002 is harmless there). Exit keeps crossterm's DisableMouseCapture, which resets every tracking mode. Decoding is crossterm's SGR parser, checked by feeding raw SGR bytes through a pty: column 300 / row 250, press, shift-drag, release, ctrl-wheel, right button and bare motion all arrive correctly. Nothing calls track_motion yet; 0016 adds the first hover target. Stale input at exit is 0052.
