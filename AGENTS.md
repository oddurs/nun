# Working in this repository

The contract for anyone automating work here, human or otherwise. It is
vendor-neutral on purpose: `CLAUDE.md` points at this file rather than
duplicating it, so there is one copy to keep true.

## Non-negotiable

**Never attribute work to a model, an AI, or an assistant.** No co-author
trailers, no "generated with" footers, no robot emoji, no narration of
authorship in code comments, docs, commit messages, PR bodies or changelogs.
Everything here is published under the repository owner's name. The `commit-msg`
hook rejects these patterns, but do not rely on it — do not write them.

**`main` only ever advances through a merged pull request.** Never commit to it,
never push to it. The `pre-push` hook refuses, and a server-side ruleset refuses
again with no bypass, including for admins.

**Never use `--no-verify`, `|| true`, or `continue-on-error` to get past a
failing check.** If a check is wrong, fix the check in its own commit and say so
in the PR.

**One unit of work → one branch → one worktree → one PR.** Parallel agents never
share a checkout. The one accepted reason to group several cairn items into a
single PR is a hard dependency edge, where shipping them apart would mean the
second PR immediately rewriting the first one's API. Say so in the PR body when
you do it.

## The loop

```sh
scripts/agent doctor                       # first, every session
cairn next                                 # what is ready to work on
cairn claim <ID>                           # take it before starting

scripts/agent start feat/0007-core-buffer  # branch + worktree
cd ../.worktrees/nun/feat/0007-core-buffer

# ... work ...

scripts/agent check                        # must be green
scripts/agent commit "feat(core): add the rope-backed buffer"
scripts/agent pr

# after the PR merges
cairn close 7
scripts/agent done                         # from inside the worktree
```

`scripts/agent done` deletes the directory you are standing in. It prints the
path to move back to; it will not `cd` for you.

Branch names are `<type>/<slug>` where type is one of `feat` `fix` `chore`
`docs` `perf` `refactor` `test`. When the work has a cairn item, the slug starts
with its id: `feat/0007-core-buffer`.

## The seam

All automation reaches this project through `scripts/task`, and nothing else.
CI runs the same targets the hooks do, so the two cannot drift.

```sh
scripts/task fmt        # format in place
scripts/task fmt:check  # verify formatting
scripts/task lint       # clippy, warnings denied
scripts/task test       # full suite
scripts/task build      # compile
scripts/task check      # all of the above
```

Never put a `cargo` invocation into CI, a git hook, or any script other than
`scripts/task`. Add a target instead.

Lints are strict by workspace policy: `unsafe_code` is **forbidden**, and clippy
runs with `pedantic` denied. Do not sprinkle `#[allow]` to get green — if a lint
is genuinely wrong for a case, allow it at the narrowest possible scope with a
comment saying why.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/): `type(scope):
subject`, imperative mood, subject ≤ 72 characters, no trailing full stop.

```
fix(theme): clamp lightness before mixing toward foreground

A background at L=0.98 produced a `raised` surface above 1.0, which wrapped to
black after conversion back to sRGB. Clamp before the mix rather than after, so
the mix ratio still means what it says.

Refs: 0010
```

The body explains **why**. The diff already says what. Reference the cairn item
in a `Refs:` trailer.

## Architecture rules

These exist because breaking them is expensive to undo later. A change that
violates one is wrong even if it passes CI.

1. **Dependencies point downward only.** The layering is: `nun` (binary) →
   `nun-ui`, `nun-input` → `nun-syntax`, `nun-lsp`, `nun-vcs`, `nun-workspace`,
   `nun-theme` → `nun-core`, `nun-config`. Nothing imports from the layer above
   it. If you need to, the abstraction is in the wrong crate.

2. **The render path never blocks and never locks.** Editor state has one owner
   on the main thread, mutated only by messages drained from a channel.
   Everything slow — LSP round-trips, reparses, git status, search, file
   watching — runs on the tokio pool and reports back as a message. If rendering
   can wait on a language server, the design is wrong.

3. **Nothing hardcodes a colour.** Widgets read semantic roles from the derived
   ramp in `nun-theme`. A literal hex value, a `Color::Red`, or an `ansi(N)` call
   outside `nun-theme` is a bug. The whole premise is that the palette comes from
   the terminal.

4. **Every feature has a mouse path.** If a capability is reachable only by
   keystroke, it is not finished. State the click, drag or hover equivalent in
   the PR description.

5. **No modal editing, ever.** There is one editing mode. This is not a gap
   waiting to be filled.

6. **Terminal capabilities are detected, never assumed from `$TERM`.** Probe,
   honour a timeout, and degrade visibly. A capability that silently produces
   subtly wrong output is worse than one that is absent.

7. **The terminal is always restored.** Any code that enters raw mode, pushes
   keyboard-protocol flags, or enables mouse reporting must undo it on every
   exit path, including panic and signal. Prefer an RAII guard over a `defer`
   you have to remember.

## Scope

The scope is fenced — see the table in `README.md`. Modal editing, a theme
gallery, a debugger, notebooks, an extension marketplace, a second config
format, and telemetry are settled decisions rather than gaps. Do not implement
them, and do not add a dependency that anticipates them.

Do not add a dependency at all without saying in the PR why the standard library
or an existing dependency will not do.

## Testing

Anything testable without a terminal attached must be. `nun-core`, `nun-theme`
and `nun-config` have no terminal dependency and are unit tested directly. UI is
tested through the headless render harness with snapshot assertions.

Where behaviour has an invariant — apply-then-invert restores the buffer, colour
conversions round-trip, selections stay sorted and disjoint — write a property
test rather than a handful of examples.

Text handling is tested against emoji, combining marks, and CJK width. A test
suite that only uses ASCII proves very little about an editor.

## When you are unsure

Ask rather than guess on anything expensive to reverse: a public API in
`nun-core`, the config schema, the crate layering, or a new dependency. For
everything else make the call, do the work, and say in the PR what you decided
and what the alternative was.

<!-- cairn:begin -->
## Roadmap and issues

This project tracks its roadmap and issues with `cairn`. Every item is a Markdown file under `cairn/items`, described by the schema in `cairn.toml`.

**Do not create ad-hoc TODO, PLAN or NOTES files.** Create a cairn item instead, so the work appears on the board and in the generated roadmap.

### The loop

1. `cairn next` — what is ready to start. It excludes anything blocked by unfinished dependencies and puts work already in progress first.
2. `cairn claim <ID>` — take it before you start, so no one duplicates the work. `cairn claim --next` picks and claims the top-ranked unclaimed item in one step, and prints its body so you can begin immediately.
3. Do the work. Record what you learn: `cairn set <ID> <field>=<value>` for fields, `cairn note <ID> "<TEXT>"` for anything that needs a sentence — why you chose something, what you tried, what to watch for.
4. `cairn close <ID>` when it is done, or `cairn release <ID>` to hand it back.
5. `cairn check` before you report finished. It must pass.

### Commands

```sh
cairn next --json                 # ready work, ranked
cairn claim --next                # take the next ready item
cairn search <TEXT> --json        # titles, bodies and labels
cairn list --json                 # all open items
cairn list --filter 'blocked=false,priority=p0'
cairn show <ID> --json            # one item, including its body
cairn new "<TITLE>" --type <TYPE> --milestone <MILESTONE>
cairn set <ID> status=<STATUS>    # also labels+=x, or any field below
cairn note <ID> "<TEXT>"          # append reasoning; never replaces
cairn close <ID>
cairn check                       # validate; run before finishing
cairn render                      # regenerate ROADMAP.md
```

### Schema

- **Types**: `feature`, `bug`, `chore`, `docs`, `milestone`
- **Statuses**: `backlog` (open), `planned` (open), `doing` (active), `blocked` (active), `done` (done), `dropped` (dropped)
- **`milestone`**: names a `milestone` item, by key — what this ships in
- **`due`**: date, YYYY-MM-DD — when a milestone is meant to land
- **`part_of`**: names any items, by id, several allowed — a larger piece of work this belongs to
- **`priority`**: one of p0, p1, p2, p3 — p0 is a release blocker
- **`effort`**: one of s, m, l, xl — Rough size, not an estimate
- **`area`**: free text — Subsystem this touches
- **Milestones**: `m1` (due 2026-09-24), `m2` (due 2026-10-08), `m3` (due 2026-10-29), `m4` (due 2026-11-19), `m5` (due 2026-12-03), `m6` (due 2026-12-17)
- **Saved views** (`cairn list --view NAME`): `now`, `next`, `triage`

### Rules

1. Before starting work, find or create the item and set it to an active status.
2. Use the fields above rather than inventing new ones; add new fields to `cairn.toml` first.
3. Never hand-edit the generated roadmap file — change items and run `cairn render`.
4. `cairn check` must pass before the work is considered done.

<!-- cairn:end -->
