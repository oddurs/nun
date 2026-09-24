---
id: 70bb75d5-3ed1-4ff9-8981-37e7e9bb8592
title: 'Terminal panel: open the pty close-on-exec atomically'
type: bug
status: backlog
milestone: m6
created: 2026-09-24
updated: 2026-09-24
priority: p3
effort: s
area: term
---

## What happens

On macOS, rustix's openpty marks the new pty's descriptors close-on-exec in a
separate step after opening them. A process nun spawns in that window — a
language server starting, a git job, another shell — inherits the pty, which
can keep a closed terminal's pty open and stop its shell from seeing the
hang-up. Found while ruling out causes for the zsh input report (cc837ecb).

## What should happen

No child nun starts ever holds another terminal's pty.

## Acceptance criteria

- [ ] The pty is opened close-on-exec atomically where the platform allows, or opening is serialised against every other spawn nun makes
- [ ] A test spawns a child during pty creation and checks it holds no pty descriptor
