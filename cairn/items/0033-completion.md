---
id: 33
title: Completion
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-22
closed_at: 2026-09-22
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

- [x] The popup never blocks typing, even with a slow server
- [x] Stale responses for a superseded prefix are discarded
- [x] Snippet insertions place the caret correctly, and tab-stops work
- [x] Mouse: hover to preview, click to accept, wheel to scroll
- [x] Popup flips above the caret when there is no room below

## 2026-09-22

Triggers: server trigger characters plus lsp.complete (Ctrl+Space, palette, right-click). No auto-trigger on identifier characters; that is a follow-up if wanted. Keys the popup takes: Up/Down/PgUp/PgDn, Enter/Tab (insert range), Shift+Enter (replace range), Esc. Staleness: any change other than typing at the caret closes the popup and cancels its request; answers are matched by id and version. Multi-caret: the item goes in at every caret with the same preceding text; other carets stay put. Snippet choices take their first option; regex transforms are not run. Documentation is resolved lazily; additionalTextEdits are not declared lazy, so an item accepted before its resolve lands still gets its imports.
