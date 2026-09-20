---
id: 27
title: Symbol palette
type: feature
status: doing
milestone: m3
assignee: Oddur Sigurdsson
claimed: 2026-09-19
created: 2026-09-10
updated: 2026-09-19
priority: p1
effort: s
area: ui
---

## Problem

Jumping within a file should not require scrolling or a search.

## Proposal

`@` in the palette lists symbols from the tree-sitter tree, indented by nesting.
Once LSP lands, `#` does the same across the workspace via the language server.

## Acceptance criteria

- [x] Symbols are derived from the tree, so no language server is required
- [x] Nesting is visible, and filtering keeps ancestors of a match
- [x] Selecting a symbol scrolls it into view with context above it

## 2026-09-19

The outline comes from each grammar's own tags query — the one that exists to build a symbol index for code search — so it needs no language server. Nesting is not in the query, because a tag is a fact about one node; it comes from containment of the definitions' byte ranges, with each symbol recording its parent as the stack computes it.

Rust's tags query tags a method inside a declaration_list but does not tag the impl block holding it, so every method in a file came out at the top level beside the free functions: an outline with no outline in it. nun supplements the query to tag impl blocks, the same way it supplements Rust's injections for SQL. That also gives a type's inherent and trait implementations the separate headings they have in the file.

@ and # were one mode; they are now two. @ reads this file's tree, # needs a language server and says so rather than both claiming to.

architecture-guard found three bugs, all fixed with tests verified to fail without their fix. A grammar in trouble produced no reply at all, so the palette said 'Reading the file…' for the rest of the session and the document lost its colours with nothing said. The request was sent without waiting for the text it would be read from to reach the worker, and since the reply is stamped with the version that was asked for, the editor accepted an outline of the old text under the new text's name and never asked again — made deterministic rather than racy by the coalescing, which folded an outline request backwards in front of the update it should have followed. And filtering was per-keystroke work proportional to the file: every name cloned, no cap on rows, and an ancestor walk whose break was unreachable for a match at depth zero because a usize is never less than zero, so it scanned to the start of the file doing nothing.
