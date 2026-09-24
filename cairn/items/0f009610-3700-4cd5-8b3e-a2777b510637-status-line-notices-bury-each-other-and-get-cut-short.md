---
id: 0f009610-3700-4cd5-8b3e-a2777b510637
title: Status line notices bury each other and get cut short
type: bug
status: backlog
milestone: m6
created: 2026-09-24
updated: 2026-09-24
priority: p1
effort: s
area: ui
---

## What happens

The status line shows one notice at a time, and a notice raised at startup
waits behind any raised before it, so the Kitty key-set notice hides the
next one: a config warning (0084), a terminal directory that has gone
(0088). A long notice is cut at the right edge — the project trust prompt
reads "…would stop formatting rust files on" at 100 columns (0075) — and
pushes the status line's own buttons off the edge, so the Undo button is
gone while a long partial-failure message shows (0078).

## What should happen

Every notice is seen: queued notices say how many are waiting and can be
stepped through, or collect in a list the status line opens. A notice
longer than the line is shortened with an ellipsis and opens in full on
click. The status line's buttons are never pushed off by a message.

## Acceptance criteria

- [ ] No notice is lost behind another, at startup or later
- [ ] A notice longer than the line shows in full on click
- [ ] Status-line buttons keep their place whatever the message
