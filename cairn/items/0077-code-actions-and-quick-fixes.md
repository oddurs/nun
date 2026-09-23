---
id: 77
title: Code actions and quick fixes
type: feature
status: backlog
milestone: m4
created: 2026-09-22
updated: 2026-09-22
depends_on:
- 32
priority: p1
effort: m
area: lsp
---

## Problem

A diagnostic card says what is wrong but offers no way to fix it. Servers
offer quick fixes through `textDocument/codeAction`, and nun does not ask for
them. 0032 shipped the card with the message only and left this for its own
piece of work.

## Proposal

Ask for code actions for the diagnostics under a card when it opens, cancelling
the request if the card closes first. Show the quick fixes as buttons on the
card: `Card` in `crates/nun/src/app/card.rs` already takes `(label, Command)`
buttons, and it will need a variant that carries an action. Apply a
`WorkspaceEdit` through `Buffer::apply_batch`, one batch per buffer. Answer
`workspace/applyEdit` properly, since it is `applied: false` today, and handle
actions that return a `command` with `workspace/executeCommand`. Add a keyboard
path as well: a command that opens the card at the caret with the fixes
focused.

## Acceptance criteria

- [ ] Quick fixes appear as clickable buttons on the diagnostic card
- [ ] Applying one is a single undo step, across every file it touches
- [ ] A slow server never delays the card; the fixes appear when they arrive
- [ ] The same fixes are reachable from the keyboard
