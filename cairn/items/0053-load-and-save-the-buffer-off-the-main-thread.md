---
id: 53
title: Load and save the buffer off the main thread
type: chore
status: backlog
milestone: perf
created: 2026-09-19
updated: 2026-09-22
priority: p1
effort: m
area: ui
---

## Problem

Opening and saving a file read and write it whole on the main thread, which is
the render path. It has been that way since milestone 1 and nothing has
noticed, because the files opened so far have been small and local. On a
network mount, or with a very large file, it stalls drawing and input — the
thing rule 2 exists to prevent.

Clicking a row in the file tree now reaches the same code, so the cases that
are slow are easier to hit than they were.

## Proposal

Route the buffer's own I/O through the same worker the file tree uses
(`nun-workspace::Jobs`): `Load` and `Save` jobs reporting back as `Done`
messages. Saving has to keep its current guarantees — the atomic
write-and-rename, the on-disk change check, and the refusal to write a file
that was decoded lossily — so the check belongs with the write, on the worker.

While a load is in flight the editor needs something to show, and a save that
fails still has to reach the status line.

Two more callers go through the same synchronous path and should move with it:
opening from the palette (`crates/nun/src/app/tabs.rs:196`), and reloading the
open buffers a project replace touched (`crates/nun/src/app/search.rs:974`).
Save currently builds the whole file as one `String` before writing
(`crates/nun-core/src/buffer.rs:366-377`); on the worker it can write the
rope's chunks straight through instead. The worker needs a snapshot of the rope
(cheap, since ropey clones share structure) and the revision it was taken at,
so an edit made while the save is in flight still leaves the buffer dirty.

## Acceptance criteria

- [ ] No filesystem read or write of a buffer happens on the main thread
- [ ] Save keeps the atomic rename, the changed-on-disk refusal and the lossy
      refusal, and still reports both success and failure in the status line
- [ ] Opening a large file leaves the editor responsive, with the frame it
      draws in the meantime saying what is happening
- [ ] Quitting with a save in flight waits for it rather than losing it
