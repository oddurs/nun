---
id: 55
title: Search toggles are unreachable on a very narrow sidebar
type: bug
status: backlog
milestone: m3
created: 2026-09-19
updated: 2026-09-19
priority: p2
---

## What happens

## What should happen

## Reproduction

1.

## 2026-09-19

Found by architecture-guard reviewing 0028.

SearchView::button_area returns None once the panel is under about eight columns, so the four toggles stop being drawn — and they have no key binding, so at that width regex, case, whole word and include-ignored become unreachable by any means. sidebar_area clamps the sidebar to half the viewport, so a terminal narrower than roughly eighteen columns gets there.

Rule 4 asks for a mouse path for every feature and that is satisfied at any usable width; this is the other end, where the mouse path disappears and nothing takes over. The fix is probably a binding rather than more geometry — at eight columns there is nowhere to put four buttons, and a terminal that narrow is degraded everywhere.

Not worth holding 0028 for: the width where it bites is far below the width where the panel is any use.
