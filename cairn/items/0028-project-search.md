---
id: 28
title: Project search
type: feature
status: doing
milestone: m3
assignee: Oddur Sigurdsson
claimed: 2026-09-19
created: 2026-09-10
updated: 2026-09-19
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

## 2026-09-19

The engine is nun-workspace::grep, on a worker thread of its own rather than on Jobs: a search of a large repository would otherwise sit in front of the directory listings the tree is waiting on. Cancellation is an AtomicU64 of the newest generation, checked between files and between the lines of a file, so a search already inside a big file still stops. Only the first match on a line becomes a Hit, because the panel shows lines. A line longer than MOST_CHARS is windowed around its match rather than truncated from the left, so a hit in a minified bundle is still worth clicking.

## 2026-09-19

Still to do for the acceptance criteria: the panel itself, and clicking a result to open the file at that line.

## 2026-09-19

Hit.matched carries every match on the line, not just the first, because 0029 (find and replace) is built on this engine: a replace that changed the first match and left the rest is a bug found after the commit. Done{hits} still counts lines, to stay consistent with the panel's row count. On a windowed long line matched is a subset, so a replace path must re-scan the real line.
