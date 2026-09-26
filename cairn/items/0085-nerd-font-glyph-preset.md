---
id: b5747a9d-00fe-49d5-95e9-26f6af06fede
title: Nerd Font glyph preset
type: feature
status: doing
milestone: m6
assignee: Oddur Sigurdsson
claimed: 2026-09-26
depends_on:
- 567291ce-56e2-43d5-93d9-bfb7459c6798
created: 2026-09-23
updated: 2026-09-26
priority: p2
area: ui
effort: s
---

## Problem

Many people run a Nerd Font, and its icons would suit nun's marks — a real
folder, a lightbulb, a magnifier. But a terminal cannot report which font it
draws with, so nun cannot know whether those code points will show as icons or
as boxes, and rule 6 says capabilities are detected, never assumed.

## Proposal

Add a `nerd` preset: one more match function beside `default` and `ascii` in
`crates/nun-ui/src/glyph.rs`, and one line in `Preset::ALL`. It is opt-in only,
`glyphs.preset = "nerd"`, and never chosen automatically. Nerd Font icons are
in the Private Use Area, which is ambiguous width, and several are drawn a
cell and a half wide by the font; choose only icons designed to fill one cell,
and let `nun glyphs` say which are wider in CJK.

## Acceptance criteria

- [x] `glyphs.preset = "nerd"` draws Nerd Font icons for the roles where one reads better; the rest keep the default's glyph
- [x] Never selected without the person asking; nothing inspects `$TERM` or the font
- [x] Every glyph in the preset passes `check`, like every other preset's (the existing test covers it once the preset is in `Preset::ALL`)
- [x] `docs/glyphs.md` gains a `nerd` column, regenerated
- [ ] Checked by eye in at least Ghostty, kitty and WezTerm with a Nerd Font, and the capture put in the PR

## 2026-09-26

Measured every icon's outline against the cell in JetBrains Mono, Fira Code and Hack Nerd Font 3.x (fontTools). In the regular variant nearly every icon is 1.2-1.6 cells wide on a one-cell advance (folder 1.46, magnifier 1.35-1.55); in the Mono variant every one fits. So the item became two presets: nerd, only icons inside the cell in every regular variant; nerd-mono, codicons throughout for a Mono font or a terminal that shrinks icons. Checked by eye in Ghostty (both presets) and kitty (both) with JetBrainsMono Nerd Font; WezTerm not yet, and the lightbulb was not seen because rust-analyzer had not answered before the capture.
