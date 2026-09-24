---
id: 26e8cdec-15ed-4831-b034-f5188e535540
title: Project search
type: feature
status: done
milestone: m3
assignee: Oddur Sigurdsson
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

- [x] First results appear before the search completes
- [x] Cancels immediately when the query changes
- [x] Literal, regex, case-sensitivity, and whole-word toggles
- [x] Honours .gitignore by default, with an override
- [x] Clicking a result opens the file at that line

## 2026-09-19

The engine is nun-workspace::grep, on a worker thread of its own rather than on Jobs: a search of a large repository would otherwise sit in front of the directory listings the tree is waiting on. Cancellation is an AtomicU64 of the newest generation, checked between files and between the lines of a file, so a search already inside a big file still stops. Only the first match on a line becomes a Hit, because the panel shows lines. A line longer than MOST_CHARS is windowed around its match rather than truncated from the left, so a hit in a minified bundle is still worth clicking.

## 2026-09-19

Still to do for the acceptance criteria: the panel itself, and clicking a result to open the file at that line.

## 2026-09-19

Hit.matched carries every match on the line, not just the first, because 0029 (find and replace) is built on this engine: a replace that changed the first match and left the rest is a bug found after the commit. Done{hits} still counts lines, to stay consistent with the panel's row count. On a windowed long line matched is a subset, so a replace path must re-scan the real line.

## 2026-09-19

The panel is the sidebar's second view rather than an overlay or a bottom pane. The file tree keeps its columns and gains a magnifier in its header; the magnifier swaps the panel in, an arrow in the panel's header swaps the tree back, and Ctrl+B from the panel shows the tree rather than hiding the sidebar — the key is named after the tree, so that is what it should get you.

Typing always goes to the query. Up and down move the selection, left and right move the caret. A panel where the arrow keys might mean the text field or might mean the list is one you have to look at to use.

Results are grouped under their file with a count and a disclosure, because a file with forty hits should be one row until it is worth forty. A keystroke sets a deadline rather than a search: 150 ms, an order longer than the parser's 12 ms and for a different reason — this one walks a directory tree and reads what it finds, where the parser re-reads a rope already in memory. The in-flight search is cancelled on the keystroke itself, so only starting is debounced.

The generation protocol cost an afternoon's worth of confusion in ten minutes. The engine runs a search only when the request's generation equals its atomic exactly, and Grep::cancel advances that atomic by one. The panel was cancelling on every keystroke while incrementing its own counter only once per search, so the two drifted apart and every single search came back cancelled with no hits — a silence rather than an error, which is the worst kind. cancel_search now moves both together, and a test pins it.

The four toggles are one cell each and carry lit-versus-unlit in colour alone, because four marks that each changed shape would make a row of four read as eight things. That leaves them needing an explanation, and the summary line was already there to give transient status: while the pointer is over a toggle it says what that toggle does instead of how the search went.

Ctrl+Shift+F only reaches a terminal with the Kitty keyboard protocol to tell it from Ctrl+F, so the binding everywhere else is Ctrl+K F.

## 2026-09-19

architecture-guard on the finished diff found four bugs, all fixed with a test that fails without its own fix.

Closing the panel left a walk queued behind it: the debounce is armed before the search starts, so cancelling without disarming still began a full walk a few milliseconds later, and every batch it found asked for a redraw of a panel that had gone.

A hit whose file had gone moved the caret in the file you were editing. Opening reports failure in the status line rather than throwing and leaves the current document focused, so placing the caret afterwards placed it in someone else's file. A missing file is harmless, since nun treats it as a new one, but a missing parent directory is an error — which is what a checkout, a build clean, or deleting the folder from the tree while results are up will hand you.

Reopening after closing mid-search showed an empty list under a query with no summary at all, which reads exactly like nothing matched; and if the walk had partly finished it showed part of an answer as though it were all of one. Half a search is now discarded when the panel closes and reopening asks again. The guard offered a 'Stopped' summary as an alternative, but that keeps the worse half of the problem: partial results sitting under a summary that does not admit they are partial.

The fourth was the valuable one, and it was measured rather than described. A frame cost what the repository costs rather than what the screen costs: drawing built a row for every result, 2.39 ms a frame at 205,000 of them, and the row list was rebuilt once per batch, making a search quadratic in what it finds. Drawing now builds only the window; the widget grew a widest_line builder so its gutter stays put as that window moves, since a window-derived gutter would jitter when a four-digit line number scrolled into view; and hits extend the list rather than rebuilding it, falling back to a rebuild only when one lands in a group that is not the newest, which the walk makes rare because it reports a file at a time. A frame is 19.7 microseconds and taking in 200,000 hits is 63 ms, from 2.39 ms and 281 ms.

Verified in a terminal: the broad query 'e' finds 22,865 results in 152 files across nun's own repo, streaming, without stalling.
