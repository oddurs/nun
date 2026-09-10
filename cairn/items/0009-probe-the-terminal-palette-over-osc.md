---
id: 9
title: Probe the terminal palette over OSC
type: feature
status: planned
milestone: m1
created: 2026-09-10
updated: 2026-09-10
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
