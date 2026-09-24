---
id: 87899ca9-3262-4083-a6fb-e8f165f78919
title: Tree-sitter integration and incremental reparse
type: feature
status: done
milestone: m3
assignee: Oddur Sigurdsson
depends_on:
- 4e3ee334-4b6c-4a70-ba1e-a9ce7a599c71
created: 2026-09-10
updated: 2026-09-19
priority: p0
effort: l
area: syntax
---

## Problem

Regex highlighting is wrong on any file large or nested enough to matter, and
folding and structural selection need a real tree regardless.

## Proposal

Parse on a worker thread, debounced at 12 ms, feeding the previous tree and the
edit for incremental reparse. Highlighting reads the last good tree while a new
one is in flight, so fast typing never flickers or goes grey.

Grammars are compiled in for the initial language set rather than loaded at
runtime — no plugin runtime is in scope for v0.1.

## Acceptance criteria

- [x] Reparse of a 10k-line file stays under one frame
- [x] Highlighting never blanks or flickers during sustained typing
- [x] Injections work: SQL in a Rust string, CSS in HTML
- [x] A grammar that panics or hangs is contained and disabled, not fatal

## 2026-09-19

Grammars are compiled in: rust, javascript, python, html, css, json, toml, sql. Parsing is on a worker that owns the trees; the editor sends a rope snapshot (cheap to clone) plus the one edit that made it, debounced 12 ms, and superseded requests are dropped rather than parsed. Highlights are asked for by window — the visible text plus a couple of hundred lines — because the query, not the parse, is what costs on a large file: a 32 KB margin made a full-file pass take 25 s, and the fix was a sweep in flatten() plus a 4 KB margin. A grammar is C code, so parsing runs under a budget and inside catch_unwind, and a language that overruns either is switched off for that document with a line in the status line. Injections go one level deep, which covers SQL in Rust and CSS or JS in HTML; Rust's own query does not inject SQL, so nun ships that rule itself.

## 2026-09-19

architecture-guard on the finished diff found three real bugs, all fixed here with tests.

The worst was in the worker's batch coalescing. It kept only the last Update or Window per document and dropped the rest, but an Update carries the edit that produced its text in the coordinates of the text before it. Two Updates in one batch therefore describe two different starting points, and feeding the second one's edit to a parser that never saw the first one's text corrupts the tree silently — and permanently, since every later incremental reparse inherits it. Worse, a Window superseding an Update threw the new text away while keeping the new version number, so the editor accepted an answer computed from text it had already moved past. Coalescing is now a fold rather than a filter: the survivor keeps the newest text, forgets the edit whenever an Update was dropped, and a Window merges into the Update it follows instead of replacing it. An Open or a Close ends a fold, because requests either side of one are about different text whatever the id says. Nine unit tests.

Second, opening a file into the scratch buffer never started the parser. 'nun <folder>' starts on an empty unnamed buffer, and the first file opened from the tree or the palette replaces it in place — a path that did not call syntax_open, so that file stayed unhighlighted and unnamed in the status line for the rest of the session. Since that is how anyone browsing a project opens their first file, it was the common case rather than the corner. Verified fixed in a pty as well as by a test, and the test was confirmed to fail without the fix.

Third, a document whose grammar was switched off was reset on the editor's side but never closed on the worker's, so the worker held the document and a clone of its rope until the tab happened to close.

Two lower-stakes findings fixed too: the status line re-derived the language from the path on every frame, and the first such call compiled all eight grammars' queries on the thread that draws, during startup — the name is now remembered when the document is opened. And a command reached by a chord timing out ran through tick, which never told the parser anything, so the change sat unparsed until the next event.
