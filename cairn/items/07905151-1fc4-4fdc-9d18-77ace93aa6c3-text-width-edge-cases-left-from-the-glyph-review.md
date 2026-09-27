---
id: 07905151-1fc4-4fdc-9d18-77ace93aa6c3
title: Text width edge cases left from the glyph review
type: bug
status: done
milestone: m6
assignee: Oddur Sigurdsson
created: 2026-09-24
updated: 2026-09-26
closed_at: 2026-09-26
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

- [x] The references list clips through `clip.rs`, and a combining mark stays with its letter
- [x] An unassigned code point is refused as a glyph override, with the reason

## 2026-09-26

The references list's own clipping already went through clip.rs; the split came from window() in the binary, which cut a long line a fixed count of chars before the reference. It now cuts with clip::lead_in, a cluster at a time and counted in cells, so wide characters and emoji are measured too. Unassigned is general category Cn from Unicode 17.0.0 UnicodeData.txt, the version unicode-width and unicode-segmentation use; regex-syntax has Cn tables but only Unicode 16 and privately. Private use (Co) passes.
