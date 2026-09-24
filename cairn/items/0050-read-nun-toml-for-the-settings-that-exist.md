---
id: a1cb2257-16d1-4f0f-be68-6fb253b43676
title: Read nun.toml for the settings that exist
type: feature
status: done
milestone: m1
created: 2026-09-12
updated: 2026-09-12
priority: p1
effort: m
area: config
part_of:
- faa72f74-0b99-4aa8-b168-d64d1d9b46b4
---

## Problem

nun is usable enough to be someone's `$EDITOR`, and there is no way to change
anything about it. The full layered scheme is `0042` in m5 — project files,
trust prompts, `.editorconfig`, hot reload — a long way off for someone who just
wants a different tab width.

## Proposal

The first two layers only: built-in defaults, then `~/.config/nun/nun.toml`.
Exactly the settings that exist in the code today and no invented ones, using
the key names `0042` will use, so the file keeps working when the rest arrives.

`nun config` prints the merged result annotated by the layer each value came
from, which is the discoverability half of the design.

## Acceptance criteria

- [ ] Zero config works and is the documented default
- [ ] A malformed file names the offending line and falls back rather than dying
- [ ] An unknown key is reported, not silently ignored
- [ ] `nun config` shows the effective values and where each came from
- [ ] Keys match the schema `0042` will implement

## 2026-09-12

First two layers only: defaults, then ~/.config/nun/nun.toml. Key names match the schema 0042 will implement, so the file keeps working when project files, .editorconfig and hot reload land. nun-config cannot depend on nun-theme (that would point sideways), so the config-to-role binding lives in the binary.
