---
id: 10
title: Derive the role ramp in OKLCH
type: feature
status: planned
milestone: m1
depends_on:
- 9
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: theme
---

## Problem

Sixteen ANSI colours plus a foreground and background are not enough to draw a
UI. Surfaces, borders and dim text have to be derived, and derived in a space
where a lightness step looks like a lightness step.

## Proposal

`nun-theme::derive(&Probe) -> Ramp`. Background lightness picks polarity.
Surfaces step in OKLCH lightness; borders and dim text are ground-to-foreground
mixes so they can never clash with either. ANSI 1-6 map to the semantic roles.

Everything downstream — syntax, diagnostics, git marks, chrome — reads roles,
never colours.

## Acceptance criteria

- [ ] sRGB to OKLCH and back, round-tripping within 1/255 per channel
- [ ] Every derived pair meets a stated contrast floor on both polarities
- [ ] A low-contrast or near-monochrome terminal palette still yields a usable ramp
- [ ] `[theme.roles]` overrides one role and leaves the rest derived
- [ ] Snapshot tests over a corpus of real terminal palettes
