---
id: cc837ecb-1b28-42d2-a734-61b108f0bdfc
title: Typed keys never reach zsh in the terminal panel
type: bug
status: backlog
milestone: m5
created: 2026-09-24
updated: 2026-09-24
priority: p2
area: term
---

## What happens

In a smoke test after m5 merged, zsh in the terminal panel drew its prompt
but typed keys never appeared, and nothing ran. Re-sending the text 5s later
failed too. A tmux resize redrew the panel correctly, so output and SIGWINCH
worked. `/bin/sh` worked right after with the same steps. It failed three
runs in a row (real config with starship; `--no-session`; empty `ZDOTDIR`)
around 01:41-01:43 on 2026-09-24, with nun built from 99eed15 at 01:40,
while rust-analyzer ran a first index and the machine was loaded. The same
binary and steps then passed every time.

## What should happen

Keys typed while a terminal has the keyboard reach its program.

## Reproduction

Not reproduced since. The attempt, in tmux (110x22):

1. `ZDOTDIR=<empty dir> SHELL=/bin/zsh nun --no-session main.rs` in a git repo.
2. `F6`, wait, then `send-keys -l 'echo hello'` and `Enter`.

To catch it next time, run nun with `NUN_TERM_LOG=<path>`. Every byte sent
to each pty (`sent`) and read from it (`got`) is appended with a timestamp.
If the keys show up as `sent`, the pty or the shell lost them. If nothing
is `sent`, the editor never routed them to the terminal.

## Ruled out

- **Key encoding and modes.** A byte log of zsh 5.9 (`script -r` around it,
  and later `NUN_TERM_LOG`) shows zsh sending only `ESC[?2004h`/`ESC[?2004l`
  with an empty config, and starship adding `ESC[?1h ESC=` (smkx). nun sends
  plain bytes and `\r` in both, and zsh echoes and runs them.
- **Queries needing an answer.** zsh sends no DA, DSR or CPR, so nothing is
  waiting on a reply.
- **Typing before the line editor is up.** Keys sent 0 to 0.5s after F6, and
  F6 pressed 0.1 to 1s after launch, all arrive.
- **Load.** 22 runs with `yes` on every core (load average above 30) and a
  fresh `cargo init` project, so rust-analyzer indexed during each run. Some
  runs used an empty ZDOTDIR, some the real config, some run A's
  sidebar/diff-view steps first. zsh ran the command in every run. (Four
  real-config runs looked like failures on screen only because starship's
  4-line prompt scrolled `hello` out of the 6-row panel. The log shows it
  ran.)
- **bash 3.2, sh and fish 4.9** all work in the same setup.
- **Writer and reader threads.** The writer only stops on a write error,
  and the reader only on EOF or an error, which reports the terminal as
  exited. Neither was seen, and both are now logged.

## Seen on the way, not this bug

On the first launch of a freshly built binary under load, nun took several
seconds to start. An `F6` sent during that time was lost, and the text typed
after it went into the editor. The lead confirmed that in the failing runs
the panel was open with "F6 to the editor" showing, so that is not this bug.

## Hypotheses left

- Focus was not on the terminal although the header said so. The header
  reads the same `focus` field that routes keys, so this is unlikely.
- Something outside nun held the pty or changed its termios, for example a
  process that inherited the pty fds. On macOS, `openpty` sets CLOEXEC
  non-atomically (rustix-openpty), so a process spawned on another thread
  in that window (LSP, git) could inherit the fds.
