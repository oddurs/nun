---
id: 29
title: Find and replace with preview
type: feature
status: done
milestone: m3
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-19
priority: p1
effort: m
area: workspace
---

## Problem

Project-wide replace is the most destructive thing an editor does, and it
usually offers the least visibility before it happens.

## Proposal

Every hit is previewed as a before/after pair and individually toggleable. The
whole replace is one undo unit per file, and the panel keeps the list afterwards
so single hits can still be reverted by clicking them.

## Acceptance criteria

- [x] Capture groups in the replacement work and are previewed accurately
- [x] Individual hits can be excluded before applying
- [x] Undo restores every file the replace touched
- [x] Files changed on disk since the search are re-checked before writing

## 2026-09-19

The panel previews before it writes: each line that would change is drawn as it is now and as it would be, and any of them can be struck out by clicking its mark. Nothing is asked of the filesystem until the button is pressed, and that button is the only control in the panel drawn in the warning colour — it keeps that colour under the pointer instead of lighting up, because a destructive button should not look more inviting when you are about to press it.

architecture-guard found seven bugs. Two of them lose work silently and neither would have been found by using the feature.

An undo that could not save the current version overwrote it anyway. The exchange wrote the project file first, reasoning that the file is the side that can refuse — true, and why a refusal there is cheap, but it put the only copy of the version being taken back in the one place about to be overwritten. If the copy then could not be written that version was gone, and the undo reported partial success over the top of it. The order is save, overwrite, commit now.

And a line that still matched but was not the line previewed was rewritten. The two staleness checks were never independent: both fail on a rewrite that leaves the chosen line number matching, so the line re-check could not cover the timestamp's granularity hole, that being the case it cannot see either. The hole is real — a search time in nanoseconds against an mtime the filesystem rounds to a second — and narrow, except that the writes landing in it are the correlated ones, a format-on-save firing as the search key goes down. The check is now whether the chosen line is still the line the recorded text came from.

That needed the search to say whether what it recorded was a line or a window onto one. Inferring it from length looks sound and is not: a window is taken from a quarter-window before the match, so one near the end of a long line comes out shorter than the cap and cannot be told from a whole short line.

The other five: the strike-out mark lost its hit region once the results scrolled a page; strikes survived a query change, so a line came back struck out under a mark nobody clicked; taking a replace back left open buffers showing the replacement, so the next save would have put it straight back; reloading a buffer kept its scroll but lost its caret; and applying while the walk was still running rewrote whatever fraction had arrived and called it the whole job.
