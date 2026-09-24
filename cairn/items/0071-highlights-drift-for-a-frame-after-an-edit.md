---
id: f0b7a33c-2f4a-42e1-b7d7-fb06f9f737f9
title: Highlights drift for a frame after an edit
type: bug
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p2
effort: s
area: syntax
---

## What happens

Until the worker's reply arrives, the previous highlight spans are drawn over
the edited text without being shifted (`crates/nun/src/app/syntax.rs:486-487`),
so colours sit a few characters off for a frame after every edit.

## What should happen

Pending edits are applied to the held spans when they are made, so the old
colours move with the text until the new ones arrive.

## Reproduction

1. Open a highlighted file.
2. Insert text at the start of a coloured line and watch the colours on the rest of it.

## Acceptance criteria

- [ ] Held spans are mapped through each edit as it is applied
