---
id: 6378e359-81cf-451b-bbca-2ca618b9044d
title: Custom glyphs drawn as cell images
type: feature
status: backlog
milestone: m6
depends_on:
- 567291ce-56e2-43d5-93d9-bfb7459c6798
created: 2026-09-23
updated: 2026-09-26
priority: p3
area: ui
effort: l
---

## Problem

A glyph has to be a character some font has, one cell wide. A person who wants
a mark no font has — a real lightbulb, a folder with a colour of its own — has
no way to get it, and never will from the text table alone.

## Proposal

An experiment, not a commitment. kitty's graphics protocol can put an image in
a cell through Unicode placeholders (U+10EEEE with diacritics saying which
image and cell), which kitty, Ghostty and WezTerm support, and which pass
through tmux. A role could name a small image as well as its text glyph; where
the terminal can show it, the placeholder is drawn instead of the text.

Detected, never assumed (rule 6): send a graphics query (`a=q`) at startup
alongside the other probes, honour the timeout, and use images only on a
positive answer. The text glyph from the role table is the fallback
everywhere else, and it is what the layout is measured with, so a terminal
that answers wrongly costs a picture, never a column.

Things to find out before building it: whether a placeholder survives ratatui's
diff (the diacritics make a cluster ratatui will measure), how images are
cleaned up on exit (rule 7), what tmux needs (`allow-passthrough`), and whether
the colour of an image can follow the theme roles or must be fixed, which
would sit badly with rule 3.

## Acceptance criteria

- [x] A written finding, as a note on this item, on each open question above
- [ ] If it goes ahead: a role can name an image; the text glyph is drawn wherever the query did not say yes
- [ ] The query is part of the startup probe, with its timeout, and `nun --capabilities` reports it
- [ ] No image is left on screen after nun exits, on every exit path

## 2026-09-26

Placeholders survive ratatui's diff. ratatui 0.30.2 measures U+10EEEE plus any of kitty's 297 row/column diacritics (all Mn, width 0) as one cell and one cluster; all 88,209 pairs plus a third mark were checked. NunBackend::draw prints the cluster as is. The collision is the image id, carried in the foreground colour: drawable() folds Rgb to a 256-colour index when truecolor is off, and so does tmux, which would change a 24-bit id. A 38;5;N id with the third mark as the high byte (65,536 ids) survives both, but a Color::Indexed in a widget is what rule 3 calls a bug, so the id must come from one module that hands widgets an opaque style.

## 2026-09-26

Cleanup (rule 7): placeholders go with the alternate screen, the image data does not. Virtual placements (U=1) are removed only by d=i/I/r/R/n/N, never d=a/A, and kitty keeps them across alt-screen clears. Send a=d,d=I,i=<id>,q=2 for every id before LeaveAlternateScreen, since kitty applies graphics commands to the current screen: a tracked step in TerminalGuard::restore before the alt-screen step, and a process-wide id range like KEYBOARD_FLAGS_PUSHED for emergency_restore. Screen::suspend must re-send on resume. q=2 matters: a late reply after raw mode lands at the shell prompt. Nothing covers SIGKILL.

## 2026-09-26

tmux: placeholders pass as text (one cell, fits a 32-byte cell), transmissions and deletes need DCS passthrough, which needs allow-passthrough (3.3+, off by default; 'on' drops sequences for hidden panes, 'all' since 3.4). The query cannot be made: a bare a=q is taken as a pane title (allow-set-title defaults on), and a wrapped one gets its reply typed as keystrokes into whichever pane is active. XTVERSION already tells us we are in tmux; there, never send a=q. Re-attaching from another terminal leaves placeholders pointing at images it never received.

## 2026-09-26

Colour: the protocol has no tint, and the foreground is spent on the id. Following the theme means treating the user's image as an alpha mask, filling it with the role's colour, sending RGBA (f=32) and re-sending under a fresh id on every ramp rebuild (re-sending under the same id deletes its virtual placement). That keeps rule 3 in substance but needs an image decoder (PNG means a new crate). A fixed-colour image breaks the palette-from-the-terminal premise and should only ever be an explicit opt-out.

## 2026-09-26

Support: kitty 0.28.0+ and Ghostty (PR #2015, 2024-07) draw placeholders and answer a=q OK. iTerm2 draws them, with fixes as late as 3.7.3; it needs all three diacritics. WezTerm 20240203 (latest) answers a=q OK but does not draw placeholders (PR #7924 open): it ignores U=1 and draws the image at the cursor, with boxes in the placeholder cells. Alacritty and foot swallow the APC and answer only DA1. Terminal.app unverified. So an OK to a=q means 'speaks the graphics protocol', not 'draws placeholders', and no query tells the two apart.

## 2026-09-26

Recommendation: defer. Rule 6 is the blocker: the only honest detection is an XTVERSION allowlist (kitty >= 0.28.0, Ghostty >= 1.0, iTerm2 >= 3.7.3, never in tmux unless configured), and a wrong answer is a blank cell, the silently wrong output rule 6 ranks worst. Revisit when WezTerm ships placeholders or the spec gains a placeholder query. The spike that settles the rest is a ~40-line printf script, no nun code: send a=q/XTVERSION/DA1 and print replies; in the alt screen, send a one-cell mask with a=T,U=1,q=2, print the placeholder with 38;5;N and three marks beside a plain glyph, d=I, leave, re-enter and reprint to see what leaked. Run it in every terminal in the matrix and in tmux with passthrough off/on/all.
