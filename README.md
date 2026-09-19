# nun

[![ci](https://github.com/oddurs/nun/actions/workflows/ci.yml/badge.svg)](https://github.com/oddurs/nun/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A terminal code editor you drive with the mouse, that borrows its colours from
the terminal it is already running in, and that ships with one config file you
will mostly never open.

## Status

Early. The editor is being built one milestone at a time; the roadmap lives in
[`cairn/`](cairn/) and is the source of truth.

What works today — milestone 1, "buffer and screen":

```sh
nun <file>         # open it, edit it, save it
nun config         # print the effective configuration and where each value came from
nun theme dump     # probe this terminal and print the derived ramp as TOML
```

You get a full-screen editor with line numbers, a status line, and colours
derived from your own terminal. Typing, arrow keys and shift-selection, undo and
redo, select all, page up and down, save, and a quit that asks twice if there
are unsaved changes.

The mouse does what it does everywhere else. Click places the caret,
double-click selects a word and triple-click a line, and dragging after either
extends by that unit. Shift-click extends, Alt-click adds a caret, Alt-drag
selects a column, and dragging a selection moves it (Ctrl at the drop copies).
Drag past the top or bottom and the view scrolls with you. Clicking the line
numbers selects whole lines.

| | |
|---|---|
| `Ctrl+S` | Save |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo (`Ctrl+Shift+Z` too, where the terminal can report it) |
| `Ctrl+A` | Select all |
| `Ctrl+Q` | Quit |

`nun keys` lists every binding, including the `Cmd` set a terminal with the
Kitty keyboard protocol adds; the same list is in [`docs/keys.md`](docs/keys.md).
Add your own under `[keys]` in `nun.toml`.

Configuration is optional and lives in `~/.config/nun/nun.toml`. Zero config is
a supported configuration and the one most people should stay on:

```toml
[editor]
tab_width = 4

[theme]
polarity = "auto"        # "dark" or "light" for a translucent background

[theme.roles]
accent = "#e0a44b"       # nudge one role; the rest stay derived

[ui]
mouse = true
```

A bad value names the line and falls back rather than taking the editor down,
and an unknown key is reported rather than silently ignored. Run `nun config` to
see what actually took effect.

Not there yet: `nun <directory>`, the sidebar, tabs, splits, the command
palette, syntax highlighting, search, language servers and git. Those are
milestones 2 to 5.

Do not install this yet.

## Why

Editors in the terminal assume you came for modal editing and stayed for the
keybindings. nun assumes the opposite: that you want the thing you already know
how to use, in the window you already have open.

- **Mouse first.** Every action is reachable by click, drag or scroll. Alt-click
  for a second caret, drag a tab onto a pane edge to split, click a gutter mark
  to stage a hunk. The keyboard is the accelerator, not the prerequisite.
- **No modes.** Arrow keys move, typing types, `Ctrl+C` copies.
- **It inherits your colours.** nun ships no themes. At startup it asks the
  terminal for its palette over OSC escape sequences and derives a full semantic
  ramp from the answer in OKLCH, so it always matches the window it is in.
- **One file of overrides.** Zero config is a supported configuration and the one
  most people should stay on.
- **A fenced scope.** LSP, git, search, splits, terminal. Not debuggers, not
  notebooks, not an extension marketplace.

## Scope

| In v0.1 | Later, maybe | Never |
|---|---|---|
| File tree sidebar, tabs, splits | WASM plugin runtime | Modal (vim/emacs) editing |
| `⌘K` palette: files, commands, symbols | Remote editing over SSH | A theme gallery |
| Project find and replace | Snippets, refactor menu | A debugger / DAP client |
| Tree-sitter highlight, folds, structural select | Persistent undo | Notebooks |
| Multi-cursor and column selection | Full git UI — log, blame, staging | An extension marketplace |
| LSP: diagnostics, completion, hover, definition, references, rename, format | Kitty-graphics image preview | A second config format |
| Git gutter, hunk revert and stage, diff view | | Telemetry of any kind |
| Integrated terminal panel | | |
| Terminal-derived theming, hot config reload | | |

The `never` column is a decision, not a backlog.

## A note on modifier keys

Terminals historically cannot report `Cmd`, or distinguish `Ctrl+I` from `Tab`.
nun negotiates the Kitty keyboard protocol at startup; where it is supported
(Ghostty, kitty, WezTerm, foot, recent iTerm2) you get the full bindings. Where
it is not, nun falls back to a Ctrl/Alt set and says so once in the status line.
That is a terminal limitation rather than a design choice, and it is the main
reason to run nun in a modern one.

## Development

```sh
git clone https://github.com/oddurs/nun.git
cd nun
scripts/setup          # wires the tracked git hooks; run once
scripts/task check     # format, lint, test, build
```

All automation reaches the project through one seam, `scripts/task`, so CI and
the local hooks can never drift apart:

| Target | Does |
|---|---|
| `fmt` / `fmt:check` | Format in place / verify formatting |
| `lint` | Clippy with warnings denied |
| `test` | Full test suite |
| `build` | Compile the workspace |
| `check` | All of the above |

Work happens one branch per worktree, driven by `scripts/agent` — see
[CONTRIBUTING.md](CONTRIBUTING.md). `main` only ever advances through a merged
pull request.

Automated contributors should read [AGENTS.md](AGENTS.md), which is the
canonical contract: the workflow, the `scripts/task` seam, the commit
convention, and the architecture rules that a passing CI run will not catch.

## Roadmap

Tracked in [cairn](https://github.com/oddurs/cairn), as Markdown files in this
repository:

```sh
cairn board       # kanban by status
cairn roadmap     # milestones with progress
cairn next        # what is ready to work on
```

## License

MIT — see [LICENSE](LICENSE).
