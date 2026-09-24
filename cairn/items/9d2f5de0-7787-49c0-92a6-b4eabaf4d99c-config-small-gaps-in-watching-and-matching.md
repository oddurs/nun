---
id: 9d2f5de0-7787-49c0-92a6-b4eabaf4d99c
title: 'Config: small gaps in watching and matching'
type: chore
status: backlog
milestone: m6
created: 2026-09-24
updated: 2026-09-24
priority: p3
effort: s
area: config
---

## Problem

Loose ends from layered config (0042): an `.editorconfig` created above
the project root while nun runs is not seen until restart; EditorConfig
globs do not normalise Unicode, so a composed "é" in a glob misses a
decomposed file name; and if the config watcher cannot start, a trust
decision is kept in memory only.

## Acceptance criteria

- [ ] A new `.editorconfig` anywhere above the root is picked up live
- [ ] Glob matching compares names in one normalisation form
- [ ] A trust decision is saved even when the watcher cannot start
