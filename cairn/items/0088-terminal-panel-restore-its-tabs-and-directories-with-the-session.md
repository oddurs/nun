---
id: 88
title: 'Terminal panel: restore its tabs and directories with the session'
type: feature
status: backlog
milestone: m5
depends_on:
- 41
- 43
created: 2026-09-23
updated: 2026-09-23
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

- [ ] The panel's visibility, height, tabs and splits come back after a restart
- [ ] Each shell starts in the directory it was last in, where the shell reported one
- [ ] A session file with no terminal table restores as it did before
