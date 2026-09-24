---
id: e17adef1-d0ef-452f-b600-6d3f65b92f57
title: Show where a snippet's tab-stops are
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
depends_on:
- 179d0519-976a-447e-9071-b28a12aa8fcb
created: 2026-09-23
updated: 2026-09-23
closed_at: 2026-09-23
priority: p2
effort: s
area: lsp
---

## Problem

After a completion inserts a snippet, Tab and Shift+Tab move between its
stops until the snippet ends — but nothing on screen says a snippet is live or
where its stops are. The architecture review of 0033 called this state
borderline against the no-modal rule: typing still edits, and it ends on its
own, but a key whose meaning has changed without any visible sign is the thing
the rule exists to prevent.

## Proposal

Mark every remaining stop of the live snippet with a subtle role-coloured
background, the current one more strongly, and drop the marks the moment the
snippet ends. Mirrors of the current stop share its mark.

## Acceptance criteria

- [x] Every remaining stop is visible while a snippet is live, and nothing is once it ends
- [x] Mouse: clicking a marked stop moves to it (this already works; keep it working)
- [x] Colours come from nun-theme roles
