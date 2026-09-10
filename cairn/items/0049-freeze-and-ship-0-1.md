---
id: 49
title: Freeze and ship 0.1
type: chore
status: backlog
milestone: m6
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: s
area: chore
---

## Problem

Scope frozen is only true once something is tagged.

## Proposal

Close or explicitly drop everything remaining in v0.1, write the changelog, tag
`v0.1.0`, and publish. Anything not done moves to a later milestone by decision
rather than by drift.

## Acceptance criteria

- [ ] `cairn check` clean, and every v0.1 item is done or dropped with a reason
- [ ] CHANGELOG.md describes the release in terms of what people can now do
- [ ] The README describes what the binary does, not what it will do
- [ ] Tag pushed and the release workflow green
