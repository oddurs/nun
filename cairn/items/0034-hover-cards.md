---
id: 34
title: Hover cards
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-23
closed_at: 2026-09-23
priority: p1
effort: s
area: lsp
---

## Problem

The mouse is already over the symbol; asking for a keystroke to learn what it
is undoes the premise.

## Proposal

Hover for 400 ms over a symbol shows the LSP hover card, with the markdown
rendered rather than dumped. The card is scrollable and does not steal focus.

## Acceptance criteria

- [x] Delay is configurable, and moving away cancels the pending request
- [x] Markdown renders: code blocks highlighted, links via OSC 8
- [x] The card is positioned to stay on screen near an edge
- [x] It never covers the symbol it describes

## 2026-09-23

Pointer dwell is watched for in app/hover.rs rather than through the hit map: text is not a hover target, so each move onto a new symbol starts a wait (ui.hover_delay_ms, default 400), moving on cancels it, and moving on after asking sends $/cancelRequest via lsp.cancel. A diagnostic on the same symbol shares the card (what is wrong first), and the underline's own dwell gives way to a held hover card. Markdown is parsed with pulldown-cmark (no default features) and fenced code highlighted with nun-syntax on the main thread when the card is built, under one per-card allowance of 16 KiB and 20 ms; past it code is plain. Links are nun's own (underlined, Target::CardLink, http/https/mailto handed to open/xdg-open on a thread, file: links opened in nun, anything else refused); OSC 8 is added per cell behind ui.hyperlinks, and the backend merges adjacent cells of one link into one sequence.
