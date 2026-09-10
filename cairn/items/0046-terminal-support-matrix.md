---
id: 46
title: Terminal support matrix
type: docs
status: backlog
milestone: m6
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: ui
---

## Problem

nun leans on optional terminal capabilities — OSC palette queries, the Kitty
keyboard protocol, undercurl, OSC 8, SGR mouse. Each degrades differently, and
users need to know what they are getting before they file a bug.

## Proposal

Test on Ghostty, kitty, WezTerm, iTerm2, Alacritty, foot, tmux and Terminal.app.
Publish the matrix. Every unsupported capability degrades visibly rather than
silently producing something subtly wrong.

## Acceptance criteria

- [ ] Matrix published in the README with what is lost in each cell
- [ ] tmux passthrough handled, or explicitly documented as unsupported
- [ ] No capability is assumed from $TERM alone
- [ ] `nun --capabilities` prints what this terminal was detected to support
