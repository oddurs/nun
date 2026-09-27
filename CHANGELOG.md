# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.1] - 2026-09-26

The first tagged build: everything from milestones 1 to 5. It is a
pre-release, ahead of the 0.1 scope freeze.

### Added

- Open a file or a folder, edit it and save it, with undo, multiple carets,
  column selection and structural selection.
- Do everything with the mouse: click, drag and multi-click selection, drag to
  move text, a file tree you can drag files around in, tabs you can reorder,
  and splits you make by dragging a tab onto a pane's edge.
- Find files, commands, lines and symbols from one palette, and search and
  replace across the project with a preview of every change.
- Read code highlighted by tree-sitter in eight languages, embedded languages
  included, and fold it where the parse says a region starts.
- Get diagnostics, completion, hover, go to definition, references, rename,
  code actions and format on save from the language servers you already have.
- See git changes in the gutter, stage or revert a hunk from it, and diff a
  file against the index or HEAD.
- Run your shell in a terminal panel with tabs and splits, and click paths and
  links in its output.
- Pick up where you left off: files, splits, carets, folds and terminals come
  back with the session.
- Change `nun.toml` and see it apply without a restart, and get colours
  derived from your own terminal's palette rather than a theme.
- Choose how marks are drawn: `default`, `ascii`, or the Nerd Font presets
  `nerd` and `nerd-mono`, with any glyph overridable.

[Unreleased]: https://github.com/oddurs/nun/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/oddurs/nun/releases/tag/v0.1.0-alpha.1
