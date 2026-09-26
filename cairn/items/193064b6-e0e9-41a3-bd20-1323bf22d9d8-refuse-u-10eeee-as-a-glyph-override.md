---
id: 193064b6-e0e9-41a3-bd20-1323bf22d9d8
title: Refuse U+10EEEE as a glyph override
type: bug
status: backlog
milestone: m6
created: 2026-09-26
updated: 2026-09-26
priority: p3
effort: s
area: ui
---

## What happens

`glyph::check` accepts U+10EEEE, kitty's image placeholder. It is plane-16
Private Use, one cell wide, so it passes like any Nerd Font icon. kitty,
Ghostty and iTerm2 draw it blank when no image is placed behind it (`BLANK_FONT`
in kitty's `fonts.c`), which is the draws-nothing case `check` exists to refuse.
Found while researching 6378e359.

## What should happen

`check` refuses U+10EEEE with a reason that says it is an image placeholder
and draws nothing on its own. If 6378e359 is ever built, its placeholders are
emitted by nun, never taken from `[glyphs]`, so this rule stands either way.

## Reproduction

1. Put `lightbulb = "\U0010EEEE"` under `[glyphs]` in `nun.toml`
2. Run `nun config`: it reports no problem
3. Open a file with code actions in kitty: the lightbulb column is blank

## Acceptance criteria

- [ ] `check` refuses U+10EEEE, alone or with combining marks, with the reason
- [ ] Other plane-16 Private Use code points still pass
