---
id: 51
title: Opening a directory reports a raw errno
type: bug
status: done
milestone: m1
created: 2026-09-12
updated: 2026-09-12
priority: p0
effort: s
area: core
---

## What happens

```
$ nun .
nun: Is a directory (os error 21)
```

Permission failures are just as bare, and no error names the path it failed on.

## What should happen

`nun .` is the first thing anyone types. It should say that nun opens one file
at a time, that the file tree is milestone 2, and stop — not surface an errno.
Every error should name the path.

## Reproduction

1. `nun .`

## 2026-09-12

Also covers the two neighbouring cases that were just as bare: a permission failure now names the path, and a new file under a directory that does not exist says which part is missing rather than failing at save time.
