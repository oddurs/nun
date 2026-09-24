---
id: dc9570d6-a994-45df-ab96-96e1b92ff133
title: Reuse parses of injected languages
type: chore
status: backlog
milestone: perf
created: 2026-09-22
updated: 2026-09-22
priority: p2
effort: s
area: syntax
---

## Problem

Each injected region (Markdown code blocks, for instance) gets a new `Parser`, a
`to_string` copy, and a full parse on every highlight request
(`crates/nun-syntax/src/highlight.rs:399-445`). A long README with many code
blocks reparses all of them on every keystroke.

## Proposal

Cache injected trees by byte range and language, and edit them incrementally
along with the host tree.

## Acceptance criteria

- [ ] An edit outside an injected region does not reparse it
- [ ] Bench: keystroke in a Markdown file with 200 code blocks under budget
