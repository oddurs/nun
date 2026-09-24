---
id: bbcd2699-49d6-409f-bbd5-4a571663e9bd
title: Probe the terminal palette over OSC
type: feature
status: done
milestone: m1
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-11
priority: p0
effort: m
area: theme
---

## Problem

nun ships no themes, so it needs the terminal's actual colours before it can
draw its first frame.

## Proposal

Query OSC 4 for slots 0-15, OSC 10/11 for default foreground and background,
and OSC 12 for the cursor. Parse the replies off the input stream without
disturbing normal key handling.

Fall back in order: no reply within 120 ms → `COLORFGBG` → a neutral built-in.
The probe must never block the first frame; it renders with the fallback and
re-themes in place when the answers arrive.

## Acceptance criteria

- [ ] Correct parse of both `rgb:RRRR/GGGG/BBBB` and `#RRGGBB` reply forms
- [ ] Replies interleaved with real key input do not corrupt either
- [ ] Timeout is honoured on a terminal that never answers
- [ ] Verified by hand on Ghostty, kitty, WezTerm, iTerm2, Alacritty and foot
- [ ] `nun theme dump` prints what was probed

## 2026-09-11

Probe is sans-I/O: ProbeSession emits the query bytes and is fed whatever comes back, returning non-reply bytes for the input layer. Raw mode, the read and the timeout live in the nun binary, which keeps the escape-sequence parsing testable with no tty. Hand-verification across the terminal support matrix is NOT done and is tracked by 0046.
