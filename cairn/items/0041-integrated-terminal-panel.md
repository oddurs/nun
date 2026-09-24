---
id: 41
title: Integrated terminal panel
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-24
closed_at: 2026-09-24
priority: p1
effort: l
area: ui
---

## Problem

Running the tests is part of editing, and switching windows to do it is the
thing an integrated terminal exists to prevent.

## Proposal

A real pty with a VT parser, in a bottom panel that can be split into several.
Selection and copy work with the mouse. OSC 8 hyperlinks are clickable, and file
paths in output are detected and open in the editor.

## Acceptance criteria

- [x] Full-screen programs work: less, top, another editor
- [x] Resize propagates a correct SIGWINCH
- [x] Mouse selection and copy, plus mouse passthrough for programs that want it
- [x] Clicking `src/main.rs:42:8` in output opens that position
- [x] Panel state survives a session restore where the shell allows it

## Notes

- Emulator: alacritty_terminal 0.26, wrapped in `crates/nun-term` so nothing
  else depends on it. It has the grid, scrollback, alternate screen, scroll
  regions, modes, selection, OSC 8 and synchronized updates. vt100 has no
  selection, hyperlinks or reflow, and vte with a grid of our own would mean
  writing the emulator again.
- Pty: alacritty_terminal's own tty layer (setsid, controlling terminal,
  SIGCHLD). The workspace forbids the unsafe a pty needs, and portable-pty
  would be a second pty stack next to the one the emulator already brings.
  rustix handles the resize ioctl and killpg/waitid.
- Session restore of the panel waits for 0043: see 0088, which delivered
  the last criterion.
