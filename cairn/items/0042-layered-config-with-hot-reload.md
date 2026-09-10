---
id: 42
title: Layered config with hot reload
type: feature
status: backlog
milestone: m5
created: 2026-09-10
updated: 2026-09-10
priority: p0
effort: m
area: config
---

## Problem

The config paradigm was a design commitment; this is where it becomes real.

## Proposal

Four layers, each overriding the last: built-in defaults, `~/.config/nun/nun.toml`,
`.editorconfig` for whitespace, and a project `.nun.toml`.

Project files are prompted for on first sight and remembered per directory,
because a config file from a freshly cloned repository should not silently choose
your formatter or point a language server at an arbitrary binary.

`notify` watches all of them. A bad value is a notification naming the exact
line; the rest of the file still applies and the editor never goes down.

## Acceptance criteria

- [ ] `nun config` prints the merged result annotated by originating layer
- [ ] `nun config --explain <key>` says what it resolved to here, and why
- [ ] Saving any layer re-applies live, theme included
- [ ] An untrusted project file is inert until accepted, and the prompt says what it would change
- [ ] A malformed file degrades to the previous good value with a visible notice
