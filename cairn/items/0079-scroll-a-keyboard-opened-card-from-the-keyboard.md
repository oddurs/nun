---
id: 79
title: Scroll a keyboard-opened card from the keyboard
type: feature
status: backlog
milestone: m4
created: 2026-09-23
updated: 2026-09-23
priority: p2
effort: s
area: lsp
---

## Problem

A card opened with Show hover (or F8) closes on the next key, so a hover longer than the card's fourteen rows can only be scrolled with the wheel. That is a keyboard path that stops halfway.

## Proposal

While a keyboard-opened card is up, give it a small set of keys that scroll it (for example Alt+Up/Down or Page Up/Down with a modifier) without closing it, and say so in the card's bottom row.

## Acceptance criteria

- [ ] A keyboard-opened card can be scrolled to its end without the mouse
- [ ] Any other key still closes it and does what it does
