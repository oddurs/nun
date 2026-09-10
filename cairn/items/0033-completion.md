---
id: 33
title: Completion
type: feature
status: backlog
milestone: m4
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: l
area: lsp
---

## Problem

Completion is judged on latency more than on quality, and it is the feature most
likely to fight the render loop.

## Proposal

Trigger on characters the server declares plus an explicit binding. Results are
filtered and re-sorted locally as typing continues so the popup never stalls
waiting for a round trip. Resolve documentation lazily on selection.

## Acceptance criteria

- [ ] The popup never blocks typing, even with a slow server
- [ ] Stale responses for a superseded prefix are discarded
- [ ] Snippet insertions place the caret correctly, and tab-stops work
- [ ] Mouse: hover to preview, click to accept, wheel to scroll
- [ ] Popup flips above the caret when there is no room below
