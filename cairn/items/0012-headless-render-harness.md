---
id: 12
title: Headless render harness
type: feature
status: planned
milestone: m1
depends_on:
- 11
created: 2026-09-10
updated: 2026-09-10
priority: p1
effort: m
area: ui
---

## Problem

A UI that can only be tested by looking at it will not stay correct, and none of
this is testable in CI if it needs a real terminal attached.

## Proposal

A test backend that renders into an in-memory cell buffer, plus an assertion
helper that renders a frame to text with its styles as a parallel grid. Snapshot
tests over that text, reviewed with `cargo insta`.

## Acceptance criteria

- [ ] A frame can be rendered and asserted with no tty present
- [ ] Snapshots capture style, not just characters
- [ ] Runs in CI on ubuntu-latest
