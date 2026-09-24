---
id: 46b00a9e-e420-4257-9767-0ecd19e07476
title: Symbol palette
type: feature
status: done
milestone: m3
assignee: Oddur Sigurdsson
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

## 2026-09-19

Two more from architecture-guard, both in nun's own supplement rather than in the grammar's query.

The impl supplement matched only a plain or generic type identifier, so an implementation for a reference, a slice, a tuple, a raw pointer, a dyn trait or a path got no heading and its methods sat at the top level beside the file's free functions. 'impl Trait for &str' is exactly as much an implementation as 'impl Point'. It is now one pattern matching any self type — one rather than several, because two patterns matching the same impl would tag it twice under different names, and a definition that encloses itself reads as nesting inside itself.

And a free function inside an inline mod was labelled a method, because Rust's query tags every function as a function and any function in a declaration list as a method, and a module body is a declaration list. Both patterns land on the same node, so which kind survived was decided by the order the query happened to match them — stable today, and a silent change on a grammar bump. The dedup now keeps the more specific kind deliberately, and a pass over the finished outline relabels a method whose enclosing definition is a module, or nothing, back to a function. The rule is about the outline's own shape rather than about Rust node kinds.

The guard also measured the cost the fixes removed: on a generated file of 20,000 declarations, filtering cost 2.73 ms per keystroke, of which 3.98 million iterations were the ancestor walk — against 19,900 for the same number of matches sitting at the start of the file rather than the end. And it confirmed PARSE_BUDGET is reachable on a 4.6 MB file, which is what makes the silent-timeout fix matter rather than being hygiene.

## 2026-09-19

Three more from the same review, the first of them a comment of mine that claimed something the code did not do.

The doc comment said trait implementations got 'the separate headings they have in the file'. They did not: impl Point and impl Display for Point both produced a row reading 'Point', because the type field is Point either way. A type with an inherent impl and three trait impls was four rows with the same name and no way to tell them apart. The heading now carries the trait, via a second capture the extractor reads — and the two patterns are written so they cannot both match, with !trait against trait:, because tagging one impl twice would give it two headings sharing a range and the second would read as nesting inside the first, pushing everything really inside the block a level deeper again. The guard confirmed that is exactly what would have happened had the any-self-type pattern been added as a third rather than replacing the two.

After a grammar was switched off, the palette said 'nun does not know this file's language' — for a file whose language it knows perfectly well and had to give up on. That sends someone looking for a grammar they already have, and the timeout fix makes the path more reachable rather than less. Disabled now keeps the language and records that it was given up on, so the status line still says rust and the palette says what actually happened.

And an outline arriving after the palette had moved on to Files mode refreshed it anyway, which starts a project search for an answer that has nothing to do with one. Only the mode that asked is refreshed now.
