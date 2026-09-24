---
id: 42
title: Layered config with hot reload
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-23
closed_at: 2026-09-23
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

- [x] `nun config` prints the merged result annotated by originating layer
- [x] `nun config --explain <key>` says what it resolved to here, and why
- [x] Saving any layer re-applies live, theme included
- [x] An untrusted project file is inert until accepted, and the prompt says what it would change
- [x] A malformed file degrades to the previous good value with a visible notice

## 2026-09-23

Trust is keyed by canonical directory + SHA-256 of the settings whose scope needs trust (lsp.*). Editing [editor] in a trusted project applies at once; changing [lsp] asks again, and until then only the risky keys are withheld. schema::Scope is the API for a setting that may come from a project.
