---
id: 40
title: Diff view
type: feature
status: backlog
milestone: m5
created: 2026-09-10
updated: 2026-09-10
priority: p2
effort: m
area: vcs
---

## Problem

Reviewing a whole file's changes needs more room than a gutter popover.

## Proposal

A side-by-side diff in a pane, against the index or against HEAD. Same rendering
and same theme roles as the editor, with synchronised scrolling.

## Acceptance criteria

- [ ] Side-by-side and unified, toggleable
- [ ] Intra-line changes highlighted, not just whole lines
- [ ] Hunks can be staged from the diff too
- [ ] Scroll stays synchronised across differing line counts
