---
id: 2be43025-2478-4315-b647-3356a0394364
title: 'Gutter: follow outside index changes, and draw both marks on the last line'
type: bug
status: backlog
milestone: m6
created: 2026-09-24
updated: 2026-09-24
priority: p2
effort: s
area: vcs
---

## What happens

Nothing watches `.git/index`, so a `git add` from another program while
nun keeps focus only shows once something else triggers a refresh (focus,
save, a watched file changing). And when lines are removed both above and
below the last line, the gutter draws only the "above" mark, though
`Diff::marks` yields both.

## Acceptance criteria

- [ ] A `git add` or `git reset` from outside updates the marks within a second, without focus changing
- [ ] Both removal marks on the last line are visible
