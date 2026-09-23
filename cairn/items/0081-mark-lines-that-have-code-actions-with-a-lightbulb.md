---
id: 81
title: Mark lines that have code actions with a lightbulb
type: feature
status: backlog
milestone: m4
depends_on:
- 77
created: 2026-09-23
updated: 2026-09-23
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

- [ ] A line with code actions at the caret shows a mark once the caret settles
- [ ] Clicking the mark opens the code actions chooser
- [ ] Typing never waits on, or triggers, the question
