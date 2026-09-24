---
id: b863b33a-7608-4cc2-9078-ee26233588f8
title: Code actions and quick fixes
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
depends_on:
- 5feb5467-ec21-48f0-86f6-8cefc73566df
created: 2026-09-22
updated: 2026-09-23
closed_at: 2026-09-23
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

- [x] Quick fixes appear as clickable buttons on the diagnostic card
- [x] Applying one is a single undo step, across every file it touches
- [x] A slow server never delays the card; the fixes appear when they arrive
- [x] The same fixes are reachable from the keyboard

## 2026-09-23

Rename's WorkspaceEdit machinery moved to app/workspace_edit.rs, shared by rename and code actions. Rule: an edit chosen with the text in front of the person (a code action, or the applyEdit of a command it just ran, text unchanged) that touches one open file goes straight in as one undo step; anything wider, anything touching a closed file, and any server edit sent unprompted or after the text moved is previewed like a rename, with the cross-file Undo. workspace/applyEdit now reaches the editor as an event and is answered when the edit is done with (applied, refused, or the preview put away). The combined hover+diagnostic card from 0034 asks for fixes too. Not done: a lightbulb in the gutter (filed separately).
