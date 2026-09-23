---
id: 31
title: Async LSP client and server lifecycle
type: feature
status: done
milestone: m4
assignee: Oddur Sigurdsson
created: 2026-09-10
updated: 2026-09-22
closed_at: 2026-09-22
priority: p0
effort: xl
area: lsp
---

## Problem

The largest single piece of work in the project, and the one most able to make
the editor feel slow or unstable if it is wired into the render path.

## Proposal

One tokio task per server. Requests are futures resolved by id; notifications
become messages on the editor's event channel. The render loop never awaits a
server and never holds a lock a server could be behind.

Servers are started lazily on first matching buffer, restarted with backoff on
crash, and shut down cleanly on exit.

## Acceptance criteria

- [x] A hung server degrades the editor to no-LSP, never to unresponsive
- [x] Cancellation is sent for requests whose answer is no longer wanted
- [x] Document sync is incremental and provably matches the buffer
- [x] A crashed server restarts with backoff, and gives up loudly after N tries
- [x] `nun --lsp-log` captures the full conversation for a bug report

## 2026-09-22

New crate nun-lsp beside nun-syntax: one current-thread tokio runtime on its own thread, a router task, one task per server (plus reader/writer tasks so a server that stops reading can only block its writer). The main-thread handle Lsp never waits; events come back as nun_ui::Event::Lsp and Lsp::handle folds them in. Sync: Buffer keeps a journal of every edit applied to the rope (keep_edits/take_edits), which is exactly the protocol's incremental shape; the sync layer converts each edit against a shadow of the text as it stood before that edit, and falls back to whole text whenever the text holds a carriage return (the protocol breaks lines at a lone CR, nun does not) - see 0074. Position encodings negotiated utf-32 > utf-8 > utf-16, all converted exactly. A default server that exits before saying a byte on its first run is taken as not installed (a rustup proxy for a missing component does exactly that), so defaults are quiet; one configured by hand crashes loudly.
