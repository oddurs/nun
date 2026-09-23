---
id: 86
title: Custom glyphs drawn as cell images
type: feature
status: backlog
milestone: m6
depends_on:
- 84
created: 2026-09-23
updated: 2026-09-23
priority: p3
area: ui
effort: l
---

## Problem

A glyph has to be a character some font has, one cell wide. A person who wants
a mark no font has — a real lightbulb, a folder with a colour of its own — has
no way to get it, and never will from the text table alone.

## Proposal

An experiment, not a commitment. kitty's graphics protocol can put an image in
a cell through Unicode placeholders (U+10EEEE with diacritics saying which
image and cell), which kitty, Ghostty and WezTerm support, and which pass
through tmux. A role could name a small image as well as its text glyph; where
the terminal can show it, the placeholder is drawn instead of the text.

Detected, never assumed (rule 6): send a graphics query (`a=q`) at startup
alongside the other probes, honour the timeout, and use images only on a
positive answer. The text glyph from the role table is the fallback
everywhere else, and it is what the layout is measured with, so a terminal
that answers wrongly costs a picture, never a column.

Things to find out before building it: whether a placeholder survives ratatui's
diff (the diacritics make a cluster ratatui will measure), how images are
cleaned up on exit (rule 7), what tmux needs (`allow-passthrough`), and whether
the colour of an image can follow the theme roles or must be fixed, which
would sit badly with rule 3.

## Acceptance criteria

- [ ] A written finding, as a note on this item, on each open question above
- [ ] If it goes ahead: a role can name an image; the text glyph is drawn wherever the query did not say yes
- [ ] The query is part of the startup probe, with its timeout, and `nun --capabilities` reports it
- [ ] No image is left on screen after nun exits, on every exit path
