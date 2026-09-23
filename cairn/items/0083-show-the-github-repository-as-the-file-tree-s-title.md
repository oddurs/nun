---
id: 83
title: Show the GitHub repository as the file tree's title
type: feature
status: done
milestone: m6
assignee: Oddur Sigurdsson
created: 2026-09-23
updated: 2026-09-23
closed_at: 2026-09-23
priority: p2
area: workspace
effort: s
---

## Problem

The sidebar's file-tree header shows the workspace root folder's name,
uppercased: `NUN`. For a checkout of a GitHub repository the useful name is the
repository itself, `oddurs/nun`, and uppercasing mangles a case-sensitive name.

## Proposal

Look the name up on the workspace jobs worker when the root is opened or
changes, by reading git config files directly rather than running `git`: walk
up to the nearest `.git` (a directory, or a `gitdir:` file for worktrees and
submodules, following `commondir`), parse the `[remote "…"]` sections, prefer
`origin`, else the first remote on github.com. Show the folder name until the
answer arrives, and show it as it is on disk.

## Acceptance criteria

- [x] The header shows `user/repo` for a GitHub checkout and the folder name otherwise, with no uppercasing; bold and focus colour kept
- [x] No file is read on the main thread; the lookup runs on the jobs worker and reports back as a message
- [x] GitHub URLs parse in https, http, scp-style, ssh (with and without a port), git, `www.` and `user@` forms, with or without `.git` and a trailing slash
- [x] gitlab, bare paths and file URLs fall back to the folder name
- [x] Config parsing handles quoted section names, spacing, `#` and `;` comments, several remotes, and origin not first
- [x] Worktree (`gitdir:` + `commondir`), submodule-style `.git` file, walking up from a subdirectory, and no repository are all tested with tempdirs

## 2026-09-23

No other place shows the root folder as a project name: the palette and workspace-edit views show paths relative to the root, and nun sets no window title. insteadOf rewrites and include directives are deliberately not followed.
