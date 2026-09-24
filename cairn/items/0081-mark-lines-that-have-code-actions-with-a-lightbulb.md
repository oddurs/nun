---
id: fffa7361-2f04-4012-a7b2-47a3b6245618
title: Mark lines that have code actions with a lightbulb
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
depends_on:
- b863b33a-7608-4cc2-9078-ee26233588f8
created: 2026-09-23
updated: 2026-09-23
closed_at: 2026-09-23
priority: p3
effort: s
area: lsp
---

## Problem

Quick fixes are offered on a diagnostic's card and through the Code actions
chooser (0077), but nothing on screen says a line has any. A person only
finds out by resting on an underline or pressing Ctrl+. on the off chance.
Refactorings with no diagnostic behind them are only ever found that way.

## Proposal

Mark the caret's line in the gutter (or at its end) when the server offers
actions there, asking `textDocument/codeAction` after the caret settles and
cancelling as it moves on, the way hover's dwell does. Clicking the mark opens
the chooser from 0077. Keep it cheap: one question per settled caret, none
while typing, and none for servers that do not offer code actions. The glyph
must read from a theme role, never a literal colour.

## Acceptance criteria

- [x] A line with code actions at the caret shows a mark once the caret settles
- [x] Clicking the mark opens the code actions chooser
- [x] Typing never waits on, or triggers, the question

## 2026-09-23

Mark is ◊ in the gutter's last column (between the fold arrows and the text), Role::Accent. Chosen over 💡 (wide, emoji presentation) and • (East Asian ambiguous width, two cells in CJK-ambiguous-wide terminals): ◊ is neutral width, has no emoji form, and is in WGL4. The question covers the caret's whole line with its diagnostics as context (trigger kind Automatic), after 300 ms of rest; edits never arm it. Clicking the mark opens the kept answer in the chooser without asking again; Ctrl+. still asks afresh (Invoked). ui.lightbulb turns it off.
