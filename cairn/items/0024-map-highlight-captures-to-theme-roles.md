---
id: 24
title: Map highlight captures to theme roles
type: feature
status: backlog
milestone: m3
depends_on:
- 22
created: 2026-09-10
updated: 2026-09-10
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

- [ ] Every capture in the shipped grammars resolves to a role
- [ ] Unknown captures fall back predictably rather than rendering unstyled
- [ ] Changing the terminal palette restyles syntax with no restart
