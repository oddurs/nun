---
id: 15
title: Mouse event plumbing
type: feature
status: backlog
milestone: m2
created: 2026-09-10
updated: 2026-09-10
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

- [ ] Correct coordinates past column 223 and row 223
- [ ] Press, drag, release and wheel distinguished, with modifier state
- [ ] Motion tracking is enabled and disabled around hover, not left on
- [ ] Reporting is disabled on every exit path, including panic
