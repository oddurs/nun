---
id: 6079f5ce-0c3c-4694-b2ed-3c16766ec192
title: 'Session restore: keep untitled buffers, unsaved edits and the sidebar'
type: feature
status: backlog
milestone: m6
created: 2026-09-24
updated: 2026-09-24
priority: p2
effort: m
area: core
---

## Problem

Session restore (0043) brings back files, panes, carets and the terminal
panel, but not an untitled buffer, not unsaved edits to a named file, and
not the sidebar — whether it was shown, its width, which folders were open.
A pane holding only untitled buffers comes back empty and collapses.

## Acceptance criteria

- [ ] Untitled buffers and unsaved edits come back, stored outside the repository and deleted once saved
- [ ] The sidebar comes back as it was
- [ ] A file changed on disk since, with unsaved edits kept, asks which to keep rather than choosing
