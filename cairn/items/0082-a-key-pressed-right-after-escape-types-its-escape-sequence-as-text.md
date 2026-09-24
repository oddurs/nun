---
id: f7a245a5-1917-4334-bda0-7beff6306f81
title: A key pressed right after Escape types its escape sequence as text
type: bug
status: backlog
milestone: m6
created: 2026-09-23
updated: 2026-09-23
priority: p1
effort: s
area: input
---

## What happens

When Escape and another key that sends an escape sequence arrive in the same
read, the second key's sequence is typed into the buffer as text. Escape then
Home arrives as `ESC ESC [ 1 ~`, and the buffer gets `[1~`. The same happens
with arrows, Page Up/Down, F-keys and the rest.

A person can hit this whenever keystrokes are batched: inside tmux, over SSH,
or by pressing Escape and an arrow quickly — Escape to close a card and an
arrow to move on is exactly that pattern.

It predates m4: the build at f8bfe6c behaves the same way.

## What should happen

`ESC` followed by a complete escape sequence is Escape, then that key. A lone
`ESC` with nothing after it inside the timeout is Escape. Nothing that arrives
as part of a sequence is ever inserted as text.

## Reproduction

1. `tmux new-session -d -s t -x 100 -y 20 "nun some-file.rs"`
2. `tmux send-keys -t t Escape Home`
3. `tmux capture-pane -p -t t` — the caret's line now starts with `[1~`

Sending `Home` alone works, so the bug is in how a doubled `ESC` is split, in
crossterm's parser or in nun's own feed in front of it
(`crates/nun-input/src/negotiate.rs`, `crates/nun/src/terminal.rs`).

## Acceptance criteria

- [ ] Escape followed by any key's sequence, in one read, is two key events
- [ ] A test feeds the doubled sequence for Home, arrows, F-keys and a Kitty-protocol key
