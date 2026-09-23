# Keys

nun has two key sets. The **full** set is used when the terminal speaks the
Kitty keyboard protocol, which is what lets it report Cmd and tell
Ctrl+Shift+Z from Ctrl+Z. The **basic** set is used everywhere else, and
nun says so once in the status line when it falls back to it. Every command
is in both.

Typing, the arrows, Home, End, Page Up and Down, Backspace, Delete, Enter,
Tab and Esc are editing keys and work the same in both.

Add or replace bindings in `~/.config/nun/nun.toml`; the defaults you do not
mention stay as they are:

```toml
[keys]
"ctrl+k ctrl+s" = "file.save"
```

This file is generated from the source by `nun keys`. Do not edit it by hand.

| Command | Id | Full set | Basic set |
|---|---|---|---|
| Save | `file.save` | `Ctrl+S`, `Cmd+S` | `Ctrl+S` |
| Undo | `edit.undo` | `Ctrl+Z`, `Cmd+Z` | `Ctrl+Z` |
| Redo | `edit.redo` | `Ctrl+Y`, `Ctrl+Shift+Z`, `Cmd+Shift+Z` | `Ctrl+Y` |
| Select all | `edit.select_all` | `Ctrl+A`, `Cmd+A` | `Ctrl+A` |
| Quit | `app.quit` | `Ctrl+Q`, `Cmd+Q` | `Ctrl+Q` |
| Toggle the file tree | `view.toggle_sidebar` | `Ctrl+B`, `Cmd+B` | `Ctrl+B` |
| New file | `files.new_file` | `Ctrl+K N` | `Ctrl+K N` |
| New folder | `files.new_folder` | `Ctrl+K Shift+N` | `Ctrl+K Shift+N` |
| Rename | `files.rename` | `F2` | `F2` |
| Delete | `files.delete` | `Ctrl+K Delete` | `Ctrl+K Delete` |
| Show or hide ignored files | `files.toggle_ignored` | `Ctrl+K I` | `Ctrl+K I` |
| Close tab | `tab.close` | `Ctrl+W`, `Cmd+W` | `Ctrl+W` |
| Next tab | `tab.next` | `Ctrl+PageDown` | `Ctrl+PageDown` |
| Previous tab | `tab.previous` | `Ctrl+PageUp` | `Ctrl+PageUp` |
| Split beside | `pane.split_beside` | `Ctrl+K V` | `Ctrl+K V` |
| Split below | `pane.split_below` | `Ctrl+K B` | `Ctrl+K B` |
| Close pane | `pane.close` | `Ctrl+K W` | `Ctrl+K W` |
| Next pane | `pane.next` | `Ctrl+K O` | `Ctrl+K O` |
| Go to file | `palette.files` | `Ctrl+P`, `Cmd+P` | `Ctrl+P` |
| Run a command | `palette.commands` | `Ctrl+Shift+P`, `Cmd+Shift+P`, `F1` | `F1` |
| Search the project | `search.project` | `Ctrl+Shift+F`, `Cmd+Shift+F`, `Ctrl+K F` | `Ctrl+K F` |
| Search: toggle regular expressions | `search.toggle_regex` | `Ctrl+K R` | `Ctrl+K R` |
| Search: toggle match case | `search.toggle_case` | `Ctrl+K C` | `Ctrl+K C` |
| Search: toggle whole words | `search.toggle_word` | `Ctrl+K Shift+W` | `Ctrl+K Shift+W` |
| Search: toggle ignored files | `search.toggle_ignored` | `Ctrl+K Shift+I` | `Ctrl+K Shift+I` |
| Add a caret above | `caret.add_above` | `Alt+Up`, `Ctrl+K Up` | `Ctrl+K Up` |
| Add a caret below | `caret.add_below` | `Alt+Down`, `Ctrl+K Down` | `Ctrl+K Down` |
| Select the next occurrence | `caret.add_next` | `Ctrl+D`, `Cmd+D` | `Ctrl+D` |
| Select every occurrence | `caret.add_all` | `Ctrl+Shift+L`, `Cmd+Shift+L`, `Ctrl+K D` | `Ctrl+K D` |
| One caret per line | `caret.split_lines` | `Ctrl+K L` | `Ctrl+K L` |
| Grow the selection | `select.grow` | `Alt+Shift+Right`, `Ctrl+K Right` | `Ctrl+K Right` |
| Shrink the selection | `select.shrink` | `Alt+Shift+Left`, `Ctrl+K Left` | `Ctrl+K Left` |
| Fold | `fold.fold` | `Cmd+Alt+[`, `Ctrl+K [` | `Ctrl+K [` |
| Unfold | `fold.unfold` | `Cmd+Alt+]`, `Ctrl+K ]` | `Ctrl+K ]` |
| Fold everything | `fold.fold_all` | `Ctrl+K 0` | `Ctrl+K 0` |
| Unfold everything | `fold.unfold_all` | `Ctrl+K J` | `Ctrl+K J` |
| Restart the language server | `lsp.restart` | `Ctrl+K Shift+R` | `Ctrl+K Shift+R` |
| Format document | `lsp.format` | `Alt+Shift+F`, `Ctrl+K Shift+F` | `Ctrl+K Shift+F` |
