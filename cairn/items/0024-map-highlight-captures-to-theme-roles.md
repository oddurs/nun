---
id: 15032ade-8116-4934-832d-ac061e0eb51e
title: Map highlight captures to theme roles
type: feature
status: done
milestone: m3
assignee: Oddur Sigurdsson
depends_on:
- 4e3ee334-4b6c-4a70-ba1e-a9ce7a599c71
created: 2026-09-10
updated: 2026-09-19
priority: p0
effort: s
area: syntax
---

## Problem

Capture names are per-grammar and open-ended. Binding them to colours directly
would put a theme back into the editor, which is the thing being avoided.

## Proposal

One table from capture name to semantic role, with longest-prefix fallback so
`@function.method.builtin` resolves through `@function.method` to `@function`.
Roles come from the derived ramp and nowhere else.

## Acceptance criteria

- [x] Every capture in the shipped grammars resolves to a role
- [x] Unknown captures fall back predictably rather than rendering unstyled
- [x] Changing the terminal palette restyles syntax with no restart

## 2026-09-19

The table lives in nun-ui, which is the one place that already knows about both captures and roles: nun-syntax must not depend on nun-theme (they are the same layer) and hands back capture names as strings. Resolution is longest-prefix, and it is total — an unknown capture lands on body text rather than rendering unstyled.
