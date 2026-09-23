---
id: 74
title: Pasted text can put a bare carriage return in the buffer
type: bug
status: backlog
milestone: m4
created: 2026-09-22
updated: 2026-09-22
priority: p1
effort: s
area: core
---

## What happens

A bracketed paste goes straight into `Buffer::insert` with its line endings
as the terminal sent them (`app.rs`, the `Event::Paste` arm). Several
terminals send the newlines of a paste as `\r` — xterm documents it — and a
paste copied from a CRLF source carries `\r\n`. Either way the buffer ends up
holding carriage returns, which breaks its own rule that text is held with
`\n` endings only: a pasted block shows as one long line, and saving a CRLF
file writes `\r\r\n`.

It matters more now that there are language servers. The protocol starts a
line after a lone `\r` and nun does not, so every position after one —
requests, diagnostics — names a different place to each side. The sync layer
keeps the server's copy exact by sending the whole text whenever a `\r` is in
it (`nun-lsp/src/sync.rs`), so every keystroke re-sends the document until the
`\r` is gone.

## What should happen

Pasted text is normalised to `\n` before it is inserted: `\r\n` and a lone
`\r` both become `\n`, the same as loading does for `\r\n`. Whether a lone
`\r` in a file on disk should also be normalised on load is a separate,
data-changing question and is not part of this.

## Reproduction

1. In xterm (or any terminal that sends pasted newlines as CR), open a file.
2. Paste two lines of text.
3. The two lines show as one; `nun --lsp-log` shows whole-text `didChange`
   messages from then on.
