# nun

**Read [AGENTS.md](AGENTS.md) first.** It is the canonical contract — the
workflow, the `scripts/task` seam, the commit convention, the attribution ban,
the seven architecture rules, and the cairn schema. Everything there applies
here. This file carries only what is specific to Claude Code.

## Quick reference

```sh
scripts/agent doctor     # environment check; run first
cairn next               # what is ready to work on
scripts/agent start <type>/<slug>
scripts/task check       # fmt, lint, test, build — must be green before a PR
```

Never commit to `main`. Never write attribution naming a model or assistant.

## Repository map

```
crates/nun/          the binary: boot, cli, event loop
crates/nun-core/     rope buffer, edits, undo, selections   (no terminal)
crates/nun-theme/    OSC probe, OKLCH ramp, role tokens     (no terminal)
crates/nun-config/   layered toml, schema, hot reload       (no terminal)
crates/nun-ui/       ratatui widgets, layout, damage tracking
crates/nun-input/    hit-testing, gestures, keymap resolution
cairn/items/         the roadmap and issues, as Markdown
scripts/task         the only place a cargo invocation belongs
```

Crates marked *no terminal* are unit-testable directly and must stay that way.

## Subagents

Three live in `.claude/agents/` and are worth reaching for by name:

- **architecture-guard** — run before opening any PR that touches more than one
  crate. It checks the diff against the seven architecture rules, most usefully
  the colour rule and the layering rule, which are easy to break by accident and
  expensive to unpick later.
- **terminal-compat** — run when a change uses a terminal capability (OSC query,
  keyboard protocol, undercurl, mouse mode, hyperlinks). It works out what each
  terminal in the support matrix actually does with it.
- **text-edge-cases** — run when a change touches indexing, movement, width, or
  file I/O in `nun-core`. Most text bugs in an editor are grapheme, width, CRLF
  or BOM bugs, and they do not show up in ASCII tests.

## Things that will bite you here

- **Clippy runs with `pedantic` denied and `unsafe_code` forbidden.** Expect
  `missing_errors_doc`, `missing_panics_doc`, `must_use_candidate` and the
  numeric-cast lints on any new public API. Write the docs rather than allowing
  the lint.
- **`scripts/agent merge` and `done` remove the directory you are standing in.**
  They print where to go; shell state will not follow.
- **`cairn claim` refuses an item whose dependencies are unfinished.** That is
  correct — pick something from `cairn next` instead of forcing it, unless you
  are deliberately grouping a dependency edge into one PR.
- **ropey indexes by char, not byte.** Mixing the two is the single most likely
  source of a panic in `nun-core`.
- **The `cairn` block in AGENTS.md is generated.** Change the schema in
  `cairn.toml`, then run `cairn agent --write AGENTS.md`. Do not hand-edit
  between the markers.
