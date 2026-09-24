---
id: 74a17705-feb2-462a-a359-dde4363ff376
title: Format on save per project
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
depends_on:
- faa72f74-0b99-4aa8-b168-d64d1d9b46b4
created: 2026-09-22
updated: 2026-09-23
closed_at: 2026-09-23
priority: p2
area: lsp
effort: s
part_of:
- aa351b86-faaa-49dc-98c1-e71c3eb97b8e
---

## Problem

Format on save is set per language in the user's `nun.toml`
(`[lsp.<language>] format_on_save`, 0037). A project cannot say it wants its
files formatted, or that it does not — a repository whose TypeScript is
formatted by the language server's formatter, or a Rust project that
deliberately does not run rustfmt, still gets the user's setting.

## Proposal

When 0042 adds the project `.nun.toml` layer, let it carry
`[lsp.<language>] format_on_save`, over the user's value. Choosing whether a
file is rewritten on save is something 0042 already says an untrusted project
file must not do silently, so this rides on its trust prompt rather than
getting a path of its own. Nothing in the app needs to change beyond reading
the merged value: `App::set_format_on_save` already takes the languages from
the effective config.

## Acceptance criteria

- [x] A trusted project `.nun.toml` can turn format on save on or off per language
- [x] An untrusted one cannot, and the trust prompt names the change
- [x] `nun config` says which layer the value came from
