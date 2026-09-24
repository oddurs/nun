---
id: 4e3ee334-4b6c-4a70-ba1e-a9ce7a599c71
title: Keymap resolution and Kitty protocol negotiation
type: feature
status: done
milestone: m2
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-19
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

- [x] Capability is detected, never assumed from $TERM
- [x] Both binding sets are complete; no command is reachable in only one
- [x] Chords time out and fall back to the prefix binding
- [x] Flags are popped on every exit path
- [x] The degraded set is documented, and the notice links to it

## 2026-09-19

Detection is CSI ? u followed by a DA1 sentinel in the same write as the palette probe; the sentinel's reply ends the probe, so a terminal that answers is not held to the timeout. The full set is the basic set plus Cmd and the Ctrl+Shift chords, so completeness holds by construction and a test checks it. Commands are keyboard-only until the palette (0021) lands as their mouse path.
