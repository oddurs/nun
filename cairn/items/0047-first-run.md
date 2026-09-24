---
id: f8a1110c-7754-494f-9b92-a71787470951
title: First run
type: feature
status: backlog
milestone: m6
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: s
area: ui
---

## Problem

The first thirty seconds decide whether someone keeps the editor, and a
mouse-first terminal editor is unfamiliar enough to need a word of orientation.

## Proposal

On a first launch with no config, a dismissible card naming the three things
that are not guessable: the palette prefix, that dragging works everywhere, and
whether the terminal supports the full key set. Never shown again.

## Acceptance criteria

- [ ] Appears once, dismissible by click, and never returns
- [ ] Says something true and specific about the terminal in use
- [ ] `nun --no-first-run` suppresses it for scripted installs
