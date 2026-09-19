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
| Quit | `app.quit` | `Ctrl+Q`, `Cmd+Q`, `Ctrl+W`, `Cmd+W` | `Ctrl+Q`, `Ctrl+W` |
| Toggle the file tree | `view.toggle_sidebar` | `Ctrl+B`, `Cmd+B` | `Ctrl+B` |
| New file | `files.new_file` | `Ctrl+K N` | `Ctrl+K N` |
| New folder | `files.new_folder` | `Ctrl+K Shift+N` | `Ctrl+K Shift+N` |
| Rename | `files.rename` | `F2` | `F2` |
| Delete | `files.delete` | `Ctrl+K Delete` | `Ctrl+K Delete` |
| Show or hide ignored files | `files.toggle_ignored` | `Ctrl+K I` | `Ctrl+K I` |
