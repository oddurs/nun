# Contributing

## Once

```sh
scripts/setup          # points git at .githooks
scripts/agent doctor   # reports every environment problem, not just the first
```

## Every unit of work

One unit of work, one branch, one worktree, one pull request. Parallel work
never shares a checkout, so two changes can never fight over the index or the
target directory.

```sh
scripts/agent start feat/0007-terminal-colour-probe
cd ../.worktrees/nun/feat/0007-terminal-colour-probe

# ... work ...

scripts/agent check
scripts/agent commit "feat(theme): probe the terminal palette over OSC 4"
scripts/agent pr
scripts/agent merge
```

`scripts/agent merge` rebases the branch if `main` has moved, pushes it again so
the check re-runs on what will actually land, squash-merges, and removes the
worktree. `scripts/agent done` cleans up after a PR merged some other way.

`scripts/agent list` shows every worktree with its branch and PR state.

### Branch names

`<type>/<slug>`, where type is one of `feat` `fix` `chore` `docs` `perf`
`refactor` `test`. When the work has a cairn item, the slug starts with its id:

```
feat/0007-terminal-colour-probe
fix/0041-attach-to-a-terminal
```

### Commits

[Conventional Commits](https://www.conventionalcommits.org/), imperative mood,
subject under 72 characters, no trailing full stop. The `commit-msg` hook
enforces this; there is no way to skip it and `--no-verify` is never the answer.

```
fix(proc): start the pty reader before the child

The reader thread was spawned after the child, so a command that wrote and
exited immediately could finish before anything was reading.

Refs: 0037
```

The body explains **why**. The diff already says what. Reference the cairn item
in a `Refs:` trailer.

### Green before it is a PR

Every push runs `scripts/task check` before anything leaves the machine. The
hooks split that work by how often you pay for it:

- **pre-commit** — formatting and lint. Fast enough to run on every commit.
- **pre-push** — the full check, plus a refusal to push to `main`.

That local check is the gate. CI runs the same `scripts/task check` on `main`
after each merge, as a net for what only Linux or a cold toolchain would catch;
it does not block a merge, and a red run on `main` is the next thing to fix.

### Pull requests

State the problem, the approach, and the part a reviewer should look at
sceptically. If a change has a known weakness, name it yourself — that is faster
than a reviewer finding it. Never pad the description.

Approvals are **not** required to merge, because this is currently a solo
repository and requiring one would deadlock it. Everything else binds: the PR
gate, a green `required` check, linear history, and no force-pushes or deletion
of `main`. Admins are not exempt. That moves to one required approval as soon as
there is a second maintainer.

Merges are squash-only, and the head branch is deleted automatically.

## Scope

nun has a deliberately fenced scope — see the table in [README.md](README.md).
The `never` column is a settled decision rather than a gap, and a PR that
implements something from it will be closed with thanks and no hard feelings.
If you think something belongs in the fence, open an issue and argue the case
before writing the code.

## Attribution

Everything committed here is published under the repository owner's name. Do not
add co-author trailers, "generated with" footers, or any other authorship
narration naming a tool, model or assistant. The `commit-msg` hook rejects them.

## Code of conduct

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
