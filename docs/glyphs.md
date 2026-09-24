# Glyphs

Every mark nun draws — the fold arrows, the lightbulb, the rail, the
sidebar's buttons — has a role, the way every colour it draws has one. A
preset gives each role a glyph, and `[glyphs]` in `~/.config/nun/nun.toml`
changes any of them:

```toml
[glyphs]
preset = "ascii"   # or "default"
lightbulb = "?"    # any role below; the rest keep the preset's glyph
```

A glyph must be one character as a terminal draws it, exactly as many
cells wide as its role takes, which is one for every role here. nun
refuses control characters, a glyph that draws nothing, emoji, and
variation selectors, whose width terminals disagree about; it draws the
preset's glyph instead and `nun config` says why. Cells says "2 in CJK"
where a terminal that draws ambiguous-width characters wide, as most
Chinese, Japanese and Korean setups do, would give the glyph two cells;
the `ascii` preset has none of those.

This file is generated from the source by `nun glyphs`. Do not edit it by hand.

Presets:

- `default`: What nun has always drawn: box drawing, arrows and geometric shapes.
- `ascii`: Nothing outside ASCII, for a font or a console that has nothing else.

In use: `default`.

## Editor

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `fold.open` | `▾` | U+25BE | 1 | `v` | Beside a line that opens a region that can be folded |
| `fold.closed` | `▸` | U+25B8 | 1 | `>` | Beside a folded region's first line |
| `fold.hidden` | `⋯` | U+22EF | 1 | `~` | The chip after a folded line, standing in for the lines it hides |
| `lightbulb` | `◊` | U+25CA | 1 | `*` | In the gutter, on the caret's line, when its language server has code actions there |

## Rail

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `rail.1` | `▎` | U+258E | 1, 2 in CJK | `.` | One problem on the stretch of file a rail row stands for |
| `rail.2` | `▌` | U+258C | 1, 2 in CJK | `:` | Two problems on one rail row |
| `rail.3` | `▊` | U+258A | 1, 2 in CJK | `\|` | Three or four problems on one rail row |
| `rail.4` | `█` | U+2588 | 1, 2 in CJK | `#` | Five or more problems on one rail row |

## Tabs

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `tab.close` | `×` | U+00D7 | 1, 2 in CJK | `x` | Closes a tab: on the tab, and on the status line |
| `tab.modified` | `•` | U+2022 | 1, 2 in CJK | `+` | A file with unsaved changes: on its tab, and after its name on the status line |
| `tab.drop` | `▏` | U+258F | 1, 2 in CJK | `\|` | Where a dragged tab will land |

## Status line

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `status.sidebar.shown` | `◧` | U+25E7 | 1 | `<` | The sidebar button while the sidebar is showing |
| `status.sidebar.hidden` | `▯` | U+25AF | 1 | `>` | The sidebar button while the sidebar is hidden |
| `diagnostic.error` | `✕` | U+2715 | 1 | `x` | Before the count of errors |
| `diagnostic.warning` | `▲` | U+25B2 | 1, 2 in CJK | `!` | Before the count of warnings |
| `diagnostic.info` | `●` | U+25CF | 1, 2 in CJK | `i` | Before the count of information and hints, which are counted together |
| `status.terminal` | `❯` | U+276F | 1 | `$` | The status line's button that opens the terminal panel, or goes to it |

## File tree

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `tree.new_file` | `+` | U+002B | 1 | `+` | The header button that makes a file |
| `tree.new_folder` | `▪` | U+25AA | 1 | `#` | The header button that makes a folder |
| `tree.ignored.hidden` | `○` | U+25CB | 1, 2 in CJK | `o` | The header button that shows ignored files, while they are hidden |
| `tree.ignored.shown` | `●` | U+25CF | 1, 2 in CJK | `O` | The header button that hides ignored files, while they are shown |
| `tree.expanded` | `▾` | U+25BE | 1 | `v` | An open folder, and a search result's file whose lines are showing |
| `tree.collapsed` | `▸` | U+25B8 | 1 | `>` | A closed folder, and a search result's file whose lines are folded away |
| `tree.symlink` | `↪` | U+21AA | 1 | `@` | A symbolic link |

## Search

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `search.icon` | `⌕` | U+2315 | 1 | `/` | Search: the status line's button, the file tree's button, and the query's prompt |
| `search.replace` | `→` | U+2192 | 1, 2 in CJK | `>` | The replacement's prompt |
| `search.back` | `▤` | U+25A4 | 1, 2 in CJK | `=` | The button that hands the sidebar back to the file tree |
| `search.apply` | `⇓` | U+21D3 | 1 | `v` | The button that writes the replacement into the files |
| `search.regex` | `*` | U+002A | 1 | `*` | The toggle for matching as a regular expression |
| `search.case` | `A` | U+0041 | 1 | `A` | The toggle for matching case |
| `search.word` | `▭` | U+25AD | 1 | `w` | The toggle for matching whole words |
| `search.ignored` | `○` | U+25CB | 1, 2 in CJK | `o` | The toggle for searching ignored files |

## Replace preview

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `replace.removed` | `-` | U+002D | 1 | `-` | A line as it is now, which the replacement will change |
| `replace.added` | `+` | U+002B | 1 | `+` | A line as the replacement will write it |
| `replace.included` | `✓` | U+2713 | 1 | `v` | A file whose lines will be replaced |
| `replace.excluded` | `·` | U+00B7 | 1, 2 in CJK | `.` | A line or file struck out of the replacement |
| `replace.line_break` | `↵` | U+21B5 | 1 | `$` | Where a line ends, in a row that shows an edit across more than one line |

## Cards

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `card.previous` | `‹` | U+2039 | 1 | `<` | Before a problem card's Previous button |
| `card.next` | `›` | U+203A | 1 | `>` | After a problem card's Next button |
| `card.above` | `▴` | U+25B4 | 1 | `^` | A card has more above what it shows |
| `card.below` | `▾` | U+25BE | 1 | `v` | A card has more below what it shows |
| `card.bullet` | `•` | U+2022 | 1, 2 in CJK | `*` | A bullet in a card's markdown list |
| `card.quote` | `│` | U+2502 | 1, 2 in CJK | `\|` | The bar beside a quote in a card's markdown |
| `card.task.done` | `☑` | U+2611 | 1 | `x` | A ticked task in a card's markdown |
| `card.task.open` | `☐` | U+2610 | 1 | `_` | An unticked task in a card's markdown |

## Terminal

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `terminal.new` | `+` | U+002B | 1 | `+` | Starts another terminal, in a tab of its own |
| `terminal.split` | `◫` | U+25EB | 1 | `\|` | Starts another terminal beside the one being used |
| `terminal.close` | `×` | U+00D7 | 1, 2 in CJK | `x` | Closes the terminal being used, ending what runs in it |
| `terminal.hide` | `▾` | U+25BE | 1 | `v` | Puts the panel away, leaving its shells running |

## Everywhere

| Role | Glyph | Code points | Cells | ascii | Description |
|---|---|---|---|---|---|
| `ellipsis` | `…` | U+2026 | 1, 2 in CJK | `~` | Where text was cut short to fit, and after a key that waits for another |
| `rule.horizontal` | `─` | U+2500 | 1, 2 in CJK | `-` | A horizontal line: under the palette's query, between stacked panes |
| `rule.vertical` | `│` | U+2502 | 1, 2 in CJK | `\|` | A vertical line: the sidebar's edge, between panes side by side |
