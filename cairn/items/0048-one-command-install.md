---
id: 48
title: One-command install
type: chore
status: backlog
milestone: m6
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: m
area: chore
---

## Problem

An editor nobody can install is an editor nobody uses.

## Proposal

Prebuilt binaries for macOS and Linux on arm64 and x86_64, attached to the
release by CI. `cargo install nun` works from crates.io. A Homebrew tap.

## Acceptance criteria

- [ ] Release workflow builds and attaches all four targets
- [ ] Binaries are checksummed, and the checksums are published
- [ ] `cargo install nun` succeeds on a clean machine
- [ ] Install instructions in the README are the ones that were actually tested
