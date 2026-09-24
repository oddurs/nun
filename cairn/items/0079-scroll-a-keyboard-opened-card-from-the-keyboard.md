---
id: b6609e18-e82c-4749-b942-cbc3fa5062bf
title: Scroll a keyboard-opened card from the keyboard
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
created: 2026-09-23
updated: 2026-09-23
closed_at: 2026-09-23
priority: p2
effort: s
area: lsp
---

## Problem

A card opened with Show hover (or F8) closes on the next key, so a hover longer than the card's fourteen rows can only be scrolled with the wheel. That is a keyboard path that stops halfway.

## Proposal

While a keyboard-opened card is up, give it a small set of keys that scroll it (for example Alt+Up/Down or Page Up/Down with a modifier) without closing it, and say so in the card's bottom row.

## Acceptance criteria

- [x] A keyboard-opened card can be scrolled to its end without the mouse
- [x] Any other key still closes it and does what it does

## 2026-09-23

Alt+PgUp/PgDn, not Alt+Up/Down: those add a caret above/below in the full set. Alt+PgUp/PgDn are unbound in both sets and arrive as CSI 5;3~ in legacy and Kitty alike. Terminal.app swallows Page Up/Down and iTerm2 needs 'Option as Alt for function keys'; the wheel covers both. Card buttons get no keys of their own: F8/Shift+F8 step and Code actions lists the same fixes.
