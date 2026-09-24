---
id: 90
title: 'Terminal panel: detect true colour, and answer pixel-size queries'
type: feature
status: backlog
milestone: m5
depends_on:
- 41
created: 2026-09-23
updated: 2026-09-23
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

- [ ] `COLORTERM` inside the panel matches what the outer terminal was found to support
- [ ] Where 24-bit colour is missing, exact colours are visibly downsampled
- [ ] Pixel-size queries are answered or refused at once, never left to time out
