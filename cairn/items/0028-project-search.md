---
id: 28
title: Project search
type: feature
status: backlog
milestone: m3
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: workspace
---

## Problem

Searching a repository is table stakes, and shelling out to ripgrep means
parsing its output and inheriting its process lifetime.

## Proposal

Use ripgrep's own crates in-process: `ignore` for traversal, `grep-searcher` and
`grep-regex` for matching. Results stream into the panel as they are found and
the search is cancellable mid-flight.

## Acceptance criteria

- [ ] First results appear before the search completes
- [ ] Cancels immediately when the query changes
- [ ] Literal, regex, case-sensitivity, and whole-word toggles
- [ ] Honours .gitignore by default, with an override
- [ ] Clicking a result opens the file at that line
