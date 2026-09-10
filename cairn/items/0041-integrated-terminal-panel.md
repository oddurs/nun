---
id: 41
title: Integrated terminal panel
type: feature
status: backlog
milestone: m5
created: 2026-09-10
updated: 2026-09-10
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

- [ ] Full-screen programs work: less, top, another editor
- [ ] Resize propagates a correct SIGWINCH
- [ ] Mouse selection and copy, plus mouse passthrough for programs that want it
- [ ] Clicking `src/main.rs:42:8` in output opens that position
- [ ] Panel state survives a session restore where the shell allows it
