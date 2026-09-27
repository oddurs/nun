---
id: 145f09d8-5c98-4cfd-a8e8-4966f34aabc5
title: Publish 0.1.0-alpha.1
type: chore
status: doing
milestone: m6
assignee: Oddur Sigurdsson
claimed: 2026-09-26
created: 2026-09-26
updated: 2026-09-26
priority: p1
effort: s
area: chore
---

## Problem

Milestones 1 to 5 are in and nothing has ever been tagged, so there is no
version anyone can name, install or report a bug against. `v0.1.0` is kept
for the scope freeze (0049), which still has open m6 and perf items.

## Proposal

Tag a pre-release, `v0.1.0-alpha.1`, from main as it is. The workspace
version says the same, so `nun --version` names the tag. The release
workflow marks any tag with a hyphen as a pre-release. The changelog says
what people can do with it; the README stops saying not to install it and
gives the install command that was tested. The tag itself is pushed from
main once this merges, since a tag cannot be reviewed in a pull request.

## Acceptance criteria

- [x] The workspace version is `0.1.0-alpha.1`, and `nun --version` prints it
- [x] CHANGELOG.md has a `0.1.0-alpha.1` section written as what people can now do
- [x] The README's status describes what the binary does today, checked claim by claim against the code
- [ ] The README's install command was run, against this branch, into a scratch root
- [x] The release workflow publishes a tag with a hyphen as a pre-release and any other as a release
