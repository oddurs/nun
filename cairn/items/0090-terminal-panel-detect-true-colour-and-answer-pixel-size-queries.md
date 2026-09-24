---
id: db068b65-6fa3-4697-9d30-99a86bdbe610
title: 'Terminal panel: detect true colour, and answer pixel-size queries'
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
depends_on:
- 4476d7b2-09a0-44fc-9870-bcb66dfba966
created: 2026-09-23
updated: 2026-09-24
closed_at: 2026-09-24
priority: p3
effort: m
area: ui
---

## Problem

Programs in the terminal panel are told `COLORTERM=truecolor` whatever the
outer terminal can draw, and their exact colours are passed through
unchanged. In a terminal without 24-bit colour (Terminal.app before macOS 26,
or tmux without RGB) those colours come out approximated. Separately,
`CSI 14 t` and `CSI 16 t` get no answer, so image viewers like chafa, timg
and yazi wait until their query times out.

## Proposal

Probe for 24-bit colour alongside the underline probe. Set `COLORTERM` for
the panel only when the probe finds 24-bit colour, and otherwise downsample
`Ink::Rgb` to the 256-colour cube. Answer the pixel-size queries from the
cell size when the outer terminal reports one; when it does not, refuse them
straight away rather than guess.

## Acceptance criteria

- [x] `COLORTERM` inside the panel matches what the outer terminal was found to support
- [x] Where 24-bit colour is missing, exact colours are visibly downsampled
- [x] Pixel-size queries are answered or refused at once, never left to time out

## Notes

Where the outer terminal never gave a cell size, `CSI 14 t` is answered
with zero and `CSI 16 t` is left unanswered, as xterm leaves a window
operation it will not do. A zero cell is not a refusal to timg: it believes
it and divides by it. chafa, yazi and notcurses end their questions with the
device attributes, which are answered at once, so they do not wait; timg
gives up on its own after 50 ms and keeps its default.
