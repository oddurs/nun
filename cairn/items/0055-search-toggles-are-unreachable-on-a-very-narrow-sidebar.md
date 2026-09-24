---
id: 69b840ab-65f0-459d-86d6-f5b4033fc101
title: Search toggles are unreachable on a very narrow sidebar
type: bug
status: done
milestone: m3
assignee: Oddur Sigurdsson
created: 2026-09-19
updated: 2026-09-22
priority: p2
---

## What happens

Below about eight columns of search panel the row of toggles runs out of room,
and the buttons that do not fit are neither drawn nor hit-tested. They had no
key binding, so regex, match case, whole word and include-ignored became
unreachable by any means.

## What should happen

Every toggle is reachable at any width: by its button where there is room, and
from the keyboard and the command palette everywhere.

## Reproduction

1. Open a folder, open the search panel.
2. Shrink the terminal to about fourteen columns.
3. The include-ignored toggle is gone, and nothing else flips it.

## 2026-09-19

Found by architecture-guard reviewing 0028.

SearchView::button_area returns None once the panel is under about eight columns, so the four toggles stop being drawn — and they have no key binding, so at that width regex, case, whole word and include-ignored become unreachable by any means. sidebar_area clamps the sidebar to half the viewport, so a terminal narrower than roughly eighteen columns gets there.

Rule 4 asks for a mouse path for every feature and that is satisfied at any usable width; this is the other end, where the mouse path disappears and nothing takes over. The fix is probably a binding rather than more geometry — at eight columns there is nowhere to put four buttons, and a terminal that narrow is degraded everywhere.

Not worth holding 0028 for: the width where it bites is far below the width where the panel is any use.

## 2026-09-22

Fixed with four commands (search.toggle_regex, _case, _word, _ignored) bound to Ctrl+K R, Ctrl+K C, Ctrl+K Shift+W and Ctrl+K Shift+I in both key sets, and listed in the command palette like every command. Chords rather than Alt+R/C/W for the reason the file operations are chords: Option types characters on a Mac. They work with the panel closed, since the toggles belong to the search.
