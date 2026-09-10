---
id: 22
title: Keymap resolution and Kitty protocol negotiation
type: feature
status: backlog
milestone: m2
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: input
---

## Problem

Terminals cannot report Cmd, and cannot distinguish Ctrl+I from Tab or Ctrl+M
from Enter, unless the Kitty keyboard protocol is negotiated. VS Code-style
bindings are not otherwise expressible.

## Proposal

Push the protocol flags at startup and query what was accepted. On success, use
the full binding set. On failure, swap in a Ctrl/Alt set and say so once in the
status line — visible but not modal.

Bindings resolve through a chord trie so `cmd+k cmd+t` works, and user bindings
in `[keys]` are additive over the defaults.

## Acceptance criteria

- [ ] Capability is detected, never assumed from $TERM
- [ ] Both binding sets are complete; no command is reachable in only one
- [ ] Chords time out and fall back to the prefix binding
- [ ] Flags are popped on every exit path
- [ ] The degraded set is documented, and the notice links to it
