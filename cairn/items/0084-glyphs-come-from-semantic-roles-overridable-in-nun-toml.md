---
id: 84
title: Glyphs come from semantic roles, overridable in nun.toml
type: feature
status: done
milestone: m6
assignee: Oddur Sigurdsson
created: 2026-09-23
updated: 2026-09-23
closed_at: 2026-09-23
priority: p1
area: ui
effort: m
---

## Problem

About fifty glyphs — fold arrows, the lightbulb, the rail's bars, the tab's
cross, the sidebar's buttons, the diagnostic tally, the truncating `…` — were
string literals in a dozen widgets. Colours come from semantic roles and are
named in one place (rule 3); glyphs had no equivalent, so a font without box
drawing, a console, or a person who wants a different fold arrow had no recourse,
and nothing guaranteed a glyph kept the one-cell width the layout is built on.

## Proposal

A glyph role table in `nun-ui` (`glyph.rs`): every role named by meaning
(`fold.open`, `tab.close`, `search.icon`), grouped by area, with its required
cell width and a one-line description. Presets are exhaustive match functions
(`default`, which is exactly the old glyphs, and `ascii`), so a new preset is
one function. `Palette` carries the resolved glyphs beside the ramp, so no
signature grows a parameter and the render path does an array index. A
`[glyphs]` table in nun.toml picks a preset and overrides roles; every override
is checked (one grapheme cluster, exactly the role's width as ratatui measures a
cell, no control or format characters, no emoji, no variation selectors, no
noncharacters) and a refused one is reported with the reason while the preset's
glyph stands in. `nun glyphs` lists every role; `docs/glyphs.md` is the same
list, held in sync by a test.

## Acceptance criteria

- [x] Every glyph a widget draws comes from a role lookup; no glyph literal is left in widget code
- [x] The default preset draws exactly what nun drew before, and the render tests pass unchanged
- [x] An `ascii` preset, all ASCII, one cell wide even where ambiguous characters are wide
- [x] `[glyphs]` takes `preset` and role overrides, written dotted, quoted or as sections
- [x] An unknown role is reported with a "did you mean"; an unknown preset names the real ones
- [x] A refused glyph is reported with its reason and the preset's glyph is drawn instead; every rejection path is unit tested
- [x] A glyph that is wider where ambiguous characters are wide is noted by `nun glyphs` and `nun config`
- [x] `nun glyphs` lists every role by area with code points, width and description; `docs/glyphs.md` is generated from it and verified by a test
- [x] `nun config` shows the resolved glyphs
- [x] No lock, no global: glyphs travel with the palette

## 2026-09-23

Prose punctuation in messages (Formatting…, curly quotes, em dashes, …and 3 more) is copy, not glyphs, and stays as it is. The five clipping writers were folded into one (clip.rs) since each had to take the ellipsis anyway; it now measures a cluster the way ratatui's diff does, skips zero-width clusters and draws control characters as U+FFFD, which fixes a write one cell past the room on text ending in a zero-width space.
