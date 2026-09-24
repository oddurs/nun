---
id: 07905151-1fc4-4fdc-9d18-77ace93aa6c3
title: Text width edge cases left from the glyph review
type: bug
status: backlog
milestone: m6
created: 2026-09-24
updated: 2026-09-24
priority: p3
effort: s
area: ui
---

## What happens

Two findings from the text-edge-cases review of the glyph table (0084):
shortening a long row in the references list can split a combining mark
from its letter, because it does not clip through the shared `clip.rs`;
and glyph overrides accept unassigned code points, whose width terminals
disagree on.

## Acceptance criteria

- [ ] The references list clips through `clip.rs`, and a combining mark stays with its letter
- [ ] An unassigned code point is refused as a glyph override, with the reason
