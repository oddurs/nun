---
id: 31
title: Async LSP client and server lifecycle
type: feature
status: backlog
milestone: m4
created: 2026-09-10
updated: 2026-09-10
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

- [ ] A hung server degrades the editor to no-LSP, never to unresponsive
- [ ] Cancellation is sent for requests whose answer is no longer wanted
- [ ] Document sync is incremental and provably matches the buffer
- [ ] A crashed server restarts with backoff, and gives up loudly after N tries
- [ ] `nun --lsp-log` captures the full conversation for a bug report
