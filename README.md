# nun

[![ci](https://github.com/oddurs/nun/actions/workflows/ci.yml/badge.svg)](https://github.com/oddurs/nun/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A terminal code editor you drive with the mouse, that borrows its colours from
the terminal it is already running in, and that ships with one config file you
will mostly never open.

## Status

Early. The editor is being built one milestone at a time; the roadmap lives in
[`cairn/`](cairn/) and is the source of truth.

What works today — milestones 1 and 2, and the start of 3:

```sh
nun <file>         # open it, edit it, save it
nun <folder>       # open it with the file tree beside it
nun keys           # every command and the keys bound to it
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

The file tree sits beside the editor. Click a file to open it, click a folder
to unfold it, drag a file onto a folder to move it there. The header's buttons
make a file or a folder, and show the files your ignore rules leave out. A
right-click offers rename and delete, and every one of those can be undone
from the message it leaves behind, delete included — deleted files go to
nun's own trash, not to nowhere. Anything another program changes on disk
shows up without a refresh. `Ctrl+B` shows and hides it, and so does the
button at the left of the status line.

Open files are tabs. Click one to go to it, drag one along the strip to
reorder it, close it with its cross or the middle button, and closing one with
unsaved changes asks first. Two files with the same name say which folder
they are in.

Panes split the screen. Drag a tab onto another pane to move it there, or
onto a pane's edge to split — the preview shows the half it will take. Drag
the divider between two panes to resize them, double-click it to even them
out, and closing a pane's last tab closes the pane and gives its space back.

The palette is `Ctrl+P`, or the button in the status line. It opens on the
project's files; `>` runs a command and shows the key that does the same, `:`
goes to a line, `@` goes to something the open file declares, and `?` lists
the prefixes. The outline behind `@` comes from the parse tree rather than
from a language server, so it works in a project that has none; it is
indented by nesting, and filtering it keeps the ancestors of a match, because
a method's name means little without the type it hangs off. Matching happens off the drawing
thread, so it stays answerable on a large repository, and files you have
opened before come first among equals. Alt+Enter opens the file in a split.

Code is highlighted by tree-sitter: Rust, JavaScript, Python, HTML, CSS,
JSON, TOML and SQL, with embedded languages picked up where they appear —
the SQL inside a `query!` macro, the CSS inside a `<style>` element. Parsing
happens on its own thread and reparses only what changed, so the colours keep
up with typing rather than the other way round. There are no themes to pick:
captures resolve to semantic roles, and the roles come from your terminal's
own palette.

The sidebar searches the project as well as listing it. Results stream in
while the walk is still going, grouped under the file they are in and folded
away by clicking it; clicking a line opens the file there. Literal or regex,
case, whole word and whether to look at ignored files are four toggles in the
panel, and the search running when you type again is cancelled rather than
finished.

It replaces as well as finds, and shows you every change before making any of
them: each line that would change is drawn as it is now and as it would be,
and any of them can be struck out by clicking its mark. Capture groups work,
and are previewed with the matcher that will do the writing, so what you see
is what you get. A file written to since the search is left alone rather than
rewritten from a stale preview, and one undo takes the whole thing back.

Not there yet: folds, language servers and git. Those are the rest of
milestone 3 and milestones 4 and 5.

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
scripts/task install   # try it: puts this working copy on your PATH
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
| `install` | Release build, onto your `PATH` |

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
