---
id: 3c95455c-e148-4d58-9a9e-fd867861aa7e
title: Install nun from the working copy with one command
type: chore
status: done
milestone: m6
created: 2026-09-19
updated: 2026-09-19
priority: p2
effort: s
area: tooling
---

## Problem

Trying a change in the editor means finding the binary under `target/`, or
typing a `cargo install` by hand — and every cargo invocation is supposed to
live behind `scripts/task`, so that one is both awkward and off the seam.

This is not 0048, which is about installing nun without a checkout. This is
for the person who has the checkout.

## Proposal

`scripts/task install` builds in release and installs it wherever cargo puts
binaries, so `nun` on the PATH is the working copy.

## Acceptance criteria

- [x] `scripts/task install` puts the current working copy on the PATH
- [x] It is listed with the other targets, in the script and in the docs
- [x] No cargo invocation anywhere but `scripts/task`
