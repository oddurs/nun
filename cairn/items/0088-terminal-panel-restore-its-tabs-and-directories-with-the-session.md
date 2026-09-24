---
id: 88
title: 'Terminal panel: restore its tabs and directories with the session'
type: feature
status: done
milestone: m5
assignee: Oddur Sigurdsson
depends_on:
- 41
- 43
created: 2026-09-23
updated: 2026-09-24
closed_at: 2026-09-24
priority: p2
effort: s
area: ui
---

## Problem

Session restore (0043) brings back files, panes and folds, and gives panels
an extension point: an optional `[panels.<kind>]` table, stored as
`State::panels` in `crates/nun/src/restore.rs`. The terminal panel (0041)
landed before it and does not use it yet, so after a restart the panel is
gone.

## Proposal

Fill `panels["terminal"]` in `App::snapshot` and read it back in
`App::restore_session` (`crates/nun/src/app/restore.rs`). Save whether the
panel was open, its height, its tabs and splits, and each shell's directory:
the one the shell last reported with OSC 7, or where it started. Start fresh
shells in those directories. Scrollback and running programs are not
restored, because there is no honest way to bring them back.

## Acceptance criteria

- [x] The panel's visibility, height, tabs and splits come back after a restart
- [x] Each shell starts in the directory it was last in, where the shell reported one
- [x] A session file with no terminal table restores as it did before

## Notes

- The panel's table is `[panels.terminal]`: `visible`, `height` (only once
  dragged), `active`, and `[[tabs]]` each with `focus` and `dirs`. Written
  from `crates/nun/src/app/panel/keep.rs`.
- Shells are started once the panel is attached, after `restore_session`;
  until then the table is kept and written back unchanged.
- A directory that has gone since starts its shell where a new one would,
  with a notice. Scrollback is never written: it holds whatever went past in
  a shell, secrets included, and the session file outlives it.
