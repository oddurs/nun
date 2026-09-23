---
id: 61
title: Scroll horizontally past the right edge
type: feature
status: backlog
milestone: perf
depends_on:
- 60
created: 2026-09-22
updated: 2026-09-22
priority: p1
effort: m
area: ui
---

## Problem

There is no horizontal scroll and no wrap. A caret past the right edge of the
view is simply not visible, which makes long lines uneditable rather than
merely slow.

## Proposal

A horizontal scroll offset per view, kept so the caret stays visible the way
the vertical offset already is, and applied in the row walk from the item this
depends on so it costs nothing extra. Wrapping is a separate decision and not
part of this.

## Acceptance criteria

- [ ] Moving the caret past either edge scrolls to keep it in view
- [ ] Mouse: horizontal wheel and shift+wheel scroll the view; click and drag map through the offset
- [ ] The gutter and the fold markers stay put while the text scrolls
