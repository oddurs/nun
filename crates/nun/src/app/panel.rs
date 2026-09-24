//! The terminal panel: shells in a strip along the bottom of the editor.
//!
//! The panel sits under the panes and beside the sidebar, and holds tabs; a
//! tab holds one terminal, or several side by side once it has been split.
//! Each terminal is a [`Pty`] running a shell and an [`Emulator`] holding its
//! screen. The pty reads on threads of its own and posts what it read as an
//! event; the screen is parsed here, on the main thread, from that event, so
//! drawing reads a screen nothing else is writing and takes no lock.
//!
//! While a terminal has the keyboard every key goes to the program in it —
//! Escape, Ctrl+C, the arrows — save one: whatever single key is bound to
//! `terminal.toggle` (F6 unless rebound, and Ctrl with the backtick too
//! where the Kitty protocol can report it), which gives the keyboard back to
//! the editor. The panel's header says which key that is while the terminal
//! has it. Keys the program could never be sent anyway, Cmd with anything,
//! still reach the editor's own bindings.
//!
//! The mouse does the rest. A click on a terminal gives it the keyboard; a
//! drag selects and copies; the wheel scrolls back; a click on a path with a
//! line and column in the output opens it. A program that asks for the mouse
//! gets it instead, and Shift gets it back for selecting, as in any terminal.
//! The header's tabs switch, its buttons add, split and hide, and dragging it
//! resizes the panel.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nun_lsp::Encoding;
use nun_lsp::types as lsp;
use nun_term::{Answers, Effect, Emulator, Id, Pick, Pty, Report, Size, Spec, input, links};
use nun_theme::Role;
use nun_ui::{CopyOutcome, Event, Glyph, Tab, TabStrip, TerminalView};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget as _;

use super::navigation::{Open, Place};
use super::{App, Focus, Outcome, Target};
use crate::clipboard;
use crate::commands::Command;

mod keep;

pub(super) use keep::KIND;

/// The fewest rows the panel takes, header included.
const MIN_HEIGHT: u16 = 4;

/// The fewest rows left to the panes above it.
const MIN_ABOVE: u16 = 3;

/// Lines the wheel scrolls the scrollback by, a notch at a time.
const WHEEL_LINES: i32 = 3;

/// One of the buttons at the right of the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermButton {
    /// Start another terminal in a tab of its own.
    New,
    /// Start another terminal beside the one being used.
    Split,
    /// Close the terminal being used, and only that one of a split.
    Close,
    /// Put the panel away.
    Hide,
}

impl TermButton {
    /// Left to right, as drawn.
    const ALL: [Self; 4] = [Self::New, Self::Split, Self::Close, Self::Hide];

    const fn glyph(self) -> Glyph {
        match self {
            Self::New => Glyph::TerminalNew,
            Self::Split => Glyph::TerminalSplit,
            Self::Close => Glyph::TerminalClose,
            Self::Hide => Glyph::TerminalHide,
        }
    }
}

/// One shell and its screen.
#[derive(Debug)]
struct Terminal {
    id: Id,
    /// Gone once the program has.
    pty: Option<Pty>,
    emulator: Emulator,
    /// What the program was called, for its tab until it names itself.
    name: String,
    /// Where it started, which paths in its output are relative to until
    /// the shell says it has moved.
    cwd: PathBuf,
}

/// A tab: one terminal, or several side by side.
#[derive(Debug, Clone)]
struct Group {
    terms: Vec<Id>,
    /// Which of them has, or last had, the keyboard.
    focus: usize,
}

impl Group {
    fn focused(&self) -> Option<Id> {
        self.terms.get(self.focus).copied()
    }
}

/// What the left button is doing in the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drag {
    /// Dragging the header, to resize the panel.
    Resize,
    /// Selecting in a terminal, from a press that may yet be a click on a
    /// link: `moved` says whether it has become a drag.
    Select { id: Id, clicks: u8, moved: bool },
    /// A press the program asked for, whose drag and release it gets too.
    Program { id: Id, button: MouseButton },
}

/// The terminal panel.
#[derive(Default)]
pub(super) struct Panel {
    terms: Vec<Terminal>,
    groups: Vec<Group>,
    /// Which tab is showing.
    active: usize,
    visible: bool,
    /// Rows the panel was dragged to, when it has been.
    height: Option<u16>,
    next_id: Id,
    /// Where the ptys post what they read, and the clipboard says how a
    /// copy went. Without it no shell can start.
    post: Option<Arc<dyn Fn(Event) + Send + Sync>>,
    /// Where shells start.
    start: Option<PathBuf>,
    /// What to run instead of the person's shell.
    program: Option<Vec<String>>,
    /// The panel as the last session left it, until shells can be started
    /// to put it back.
    kept: Option<keep::Kept>,
    drag: Option<Drag>,
    /// The cell the pointer is over, in a terminal: which, row and column.
    pointer: Option<(Id, u16, u16)>,
    /// How far the header's tabs are scrolled.
    strip_scroll: u16,
    /// Terminals being hung up on threads of their own, joined on the way
    /// out so nothing outlives the editor.
    closing: Vec<JoinHandle<()>>,
    /// Whether a terminal had the keyboard as of the last event, to tell a
    /// program that asked when that changes.
    had_focus: Option<Id>,
    /// Escapes waiting to be written to the terminal nun is drawn on.
    escapes: Vec<String>,
    /// Where copies are sent, to be made one at a time, in order.
    #[cfg(not(test))]
    copier: Option<std::sync::mpsc::Sender<clipboard::Job>>,
    /// What the tests would have put on the clipboard.
    #[cfg(test)]
    copied: Vec<String>,
}

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel")
            .field("terms", &self.terms)
            .field("groups", &self.groups)
            .field("active", &self.active)
            .field("visible", &self.visible)
            .finish_non_exhaustive()
    }
}

impl Panel {
    fn term(&self, id: Id) -> Option<&Terminal> {
        self.terms.iter().find(|term| term.id == id)
    }

    fn term_mut(&mut self, id: Id) -> Option<&mut Terminal> {
        self.terms.iter_mut().find(|term| term.id == id)
    }

    /// The terminal with the keyboard, or that last had it.
    fn focused(&self) -> Option<Id> {
        self.groups.get(self.active).and_then(Group::focused)
    }

    /// Whether it is on screen with something in it.
    fn shown(&self) -> bool {
        self.visible && !self.groups.is_empty()
    }

    /// Stop tracking terminal `id`, and hang its program up in the
    /// background.
    fn remove(&mut self, id: Id) {
        if let Some(index) = self.terms.iter().position(|term| term.id == id) {
            let term = self.terms.remove(index);
            if let Some(pty) = term.pty {
                self.closing.push(pty.close_in_background());
            }
        }
        for group in &mut self.groups {
            if let Some(at) = group.terms.iter().position(|term| *term == id) {
                group.terms.remove(at);
                group.focus = group.focus.min(group.terms.len().saturating_sub(1));
            }
        }
        self.groups.retain(|group| !group.terms.is_empty());
        self.active = self.active.min(self.groups.len().saturating_sub(1));
        if self.groups.is_empty() {
            self.visible = false;
        }
    }
}

impl App {
    /// Let the panel start shells in `dir`. What they write, and how a copy
    /// to the clipboard went, come back through `post`.
    pub fn attach_terminal(&mut self, dir: PathBuf, post: Arc<dyn Fn(Event) + Send + Sync>) {
        self.panel.start = Some(dir);
        self.panel.post = Some(post);
        self.start_kept();
    }

    /// Hang up every shell, all at once, and wait until each has gone.
    pub fn shutdown_terminals(&mut self) {
        let terms = std::mem::take(&mut self.panel.terms);
        let mut closing = std::mem::take(&mut self.panel.closing);
        closing.extend(terms.into_iter().filter_map(|term| term.pty).map(Pty::close_in_background));
        for handle in closing {
            let _ = handle.join();
        }
        self.panel.groups.clear();
    }

    // ── geometry ────────────────────────────────────────────────────────────

    /// Rows the panel takes from an area `height` rows tall, status line
    /// included: none while it is hidden, or when the panes would be left
    /// too little.
    pub(super) fn panel_rows(&self, height: u16) -> u16 {
        if !self.panel.shown() {
            return 0;
        }
        let room = height.saturating_sub(1);
        let most = room.saturating_sub(MIN_ABOVE);
        if most < MIN_HEIGHT {
            return 0;
        }
        self.panel.height.unwrap_or(room / 3).clamp(MIN_HEIGHT, most)
    }

    /// Where the panel goes in `area`: under the panes, beside the sidebar.
    pub(super) fn panel_area_in(&self, area: Rect) -> Option<Rect> {
        let rows = self.panel_rows(area.height);
        if rows == 0 {
            return None;
        }
        let beside = self.sidebar_area().map_or(0, |sidebar| sidebar.width).min(area.width);
        let bottom = area.bottom().saturating_sub(1);
        Some(Rect::new(area.x + beside, bottom - rows, area.width - beside, rows))
    }

    fn panel_area(&self) -> Option<Rect> {
        self.panel_area_in(self.viewport)
    }

    /// Each shown terminal's screen, in `panel`.
    fn screens_in(&self, panel: Rect) -> Vec<(Id, Rect)> {
        let Some(group) = self.panel.groups.get(self.panel.active) else { return Vec::new() };
        let body = Rect { y: panel.y + 1, height: panel.height.saturating_sub(1), ..panel };
        let count = u16::try_from(group.terms.len()).unwrap_or(u16::MAX).max(1);
        // One column between each two for the rule.
        let each = body.width.saturating_sub(count - 1) / count;
        let mut x = body.x;
        let mut screens = Vec::with_capacity(group.terms.len());
        for (index, id) in group.terms.iter().enumerate() {
            let last = index + 1 == group.terms.len();
            let width = if last { body.right().saturating_sub(x) } else { each };
            screens.push((*id, Rect { x, width, ..body }));
            x = x.saturating_add(width + 1);
        }
        screens
    }

    /// Where the header's buttons go, right to left from its end.
    fn header_buttons(header: Rect) -> Vec<(TermButton, Rect)> {
        let mut right = header.right();
        let mut buttons = Vec::new();
        for button in TermButton::ALL.into_iter().rev() {
            if right < header.x + 3 {
                break;
            }
            right -= 3;
            buttons.push((button, Rect::new(right, header.y, 3, 1)));
        }
        buttons.reverse();
        buttons
    }

    /// The part of the header the tabs have, and what they say.
    fn header_tabs(&self, header: Rect) -> (Rect, Vec<Tab>) {
        let buttons = Self::header_buttons(header);
        let end = buttons.first().map_or(header.right(), |(_, area)| area.x);
        let hint = u16::try_from(nun_ui::text_width(&self.panel_hint()) + 2).unwrap_or(0);
        let width = end.saturating_sub(header.x);
        // The hint gives way to the tabs on a narrow panel.
        let width = if width > hint + 20 { width - hint } else { width };
        let tabs = self
            .panel
            .groups
            .iter()
            .map(|group| {
                let name = group
                    .focused()
                    .and_then(|id| self.panel.term(id))
                    .map_or_else(String::new, |term| {
                        term.emulator.title().unwrap_or(&term.name).to_string()
                    });
                let label = match group.terms.len() {
                    1 => name,
                    count => format!("{name} {} {count}", self.palette.glyph(Glyph::TerminalSplit)),
                };
                Tab { label, modified: false }
            })
            .collect();
        (Rect { width, ..header }, tabs)
    }

    /// What the header says about the key between the editor and the
    /// terminal, which is the one key the terminal does not get.
    fn panel_hint(&self) -> String {
        let key = self.single_key_for(Command::ToggleTerminal);
        match (key, self.focus == Focus::Terminal) {
            (Some(key), true) => format!("{key} to the editor"),
            (Some(key), false) => format!("{key} to the terminal"),
            (None, _) => String::new(),
        }
    }

    /// The first key bound on its own to `command`, as the status line says
    /// it. Only a single key can be taken from a terminal: a chord's first
    /// key is a key some program wants.
    fn single_key_for(&self, command: Command) -> Option<String> {
        self.keymap
            .sequences_for(&command)
            .into_iter()
            .find(|keys| keys.len() == 1)
            .map(|keys| nun_input::Sequence(keys).to_string())
    }

    /// Lay out the panel's hit regions, and fit each shown terminal to the
    /// room it has.
    pub(super) fn layout_panel(&mut self, hits: &mut nun_input::HitMap<Target>) {
        let Some(panel) = self.panel_area() else { return };
        let header = Rect { height: 1, ..panel };
        hits.push(super::cells(header), Target::TermHeader, false);
        let (strip, tabs) = self.header_tabs(header);
        self.panel.strip_scroll =
            self.panel.strip_scroll.min(TabStrip::total_width(&tabs).saturating_sub(strip.width));
        for (index, area) in
            TabStrip::layout(&tabs, strip, self.panel.strip_scroll).into_iter().enumerate()
        {
            if area.width == 0 {
                continue;
            }
            hits.push(super::cells(area), Target::TermTab(index), true);
            if let Some(close) = TabStrip::close_area(area) {
                hits.push(super::cells(close), Target::TermTabClose(index), true);
            }
        }
        for (button, area) in Self::header_buttons(header) {
            hits.push(super::cells(area), Target::TermButton(button), true);
        }
        for (id, area) in self.screens_in(panel) {
            // A hover target so the pointer is followed over it: the link
            // under it is underlined, and a program that asked for motion
            // is told of it.
            hits.push(super::cells(area), Target::TermScreen(id), true);
            let size = Size::new(area.width, area.height);
            if let Some(term) = self.panel.term_mut(id) {
                term.emulator.resize(size);
                if let Some(pty) = term.pty.as_mut() {
                    // A pty that refuses a size keeps the one it had; the
                    // program is no worse off than before.
                    let _ = pty.resize(size);
                }
            }
        }
    }

    // ── drawing ─────────────────────────────────────────────────────────────

    /// Draw the panel into `area`, the whole of what is being drawn.
    pub(super) fn render_panel(&self, area: Rect, cells: &mut Cells) {
        let Some(panel) = self.panel_area_in(area) else { return };
        let header = Rect { height: 1, ..panel };
        self.render_header(header, cells);
        let focused = (self.focus == Focus::Terminal).then(|| self.panel.focused()).flatten();
        let screens = self.screens_in(panel);
        for (id, rect) in &screens {
            let Some(term) = self.panel.term(*id) else { continue };
            let link = match self.hovered_link() {
                Some((over, span)) if over == *id => Some(span),
                _ => None,
            };
            TerminalView::new(&term.emulator, &self.palette)
                .focused(focused == Some(*id))
                .link(link)
                .render(*rect, cells);
            if rect.right() < panel.right() {
                let style = self.palette.fg(Role::Line);
                for y in rect.top()..rect.bottom() {
                    cells[(rect.right(), y)]
                        .set_symbol(self.palette.glyph(Glyph::RuleVertical))
                        .set_style(style);
                }
            }
        }
    }

    fn render_header(&self, header: Rect, cells: &mut Cells) {
        // The header is the panel's edge and says whether it has the
        // keyboard: the accent's rule while it does.
        let focused = self.focus == Focus::Terminal;
        let style = if focused || self.panel.drag == Some(Drag::Resize) {
            self.palette.on(Role::Raised, Role::Accent)
        } else {
            self.palette.on(Role::Raised, Role::Dim)
        };
        for x in header.left()..header.right() {
            cells[(x, header.y)]
                .set_symbol(self.palette.glyph(Glyph::RuleHorizontal))
                .set_style(style);
        }
        let (strip, tabs) = self.header_tabs(header);
        let hovered = match self.hover.current() {
            Some(Target::TermTab(index) | Target::TermTabClose(index)) => Some(index),
            _ => None,
        };
        let used =
            TabStrip::total_width(&tabs).saturating_sub(self.panel.strip_scroll).min(strip.width);
        TabStrip::new(&tabs, &self.palette, self.panel.active)
            .hovered(hovered)
            .scrolled_by(self.panel.strip_scroll)
            .render(Rect { width: used, ..strip }, cells);

        let hint = self.panel_hint();
        let buttons = Self::header_buttons(header);
        let end = buttons.first().map_or(header.right(), |(_, area)| area.x);
        let width = u16::try_from(nun_ui::text_width(&hint)).unwrap_or(u16::MAX);
        if let Some(x) = end.checked_sub(width + 1)
            && x > strip.x + used
        {
            super::write_at(cells, header, x, &hint, style);
        }
        for (button, area) in buttons {
            let style = if self.hover.current() == Some(Target::TermButton(button)) {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Raised, Role::Text)
            };
            super::write_at(cells, area, area.x, &self.button_glyph(button.glyph()), style);
        }
    }

    // ── keeping up with the programs ────────────────────────────────────────

    /// The colours a program may ask the panel about.
    fn answers(&self) -> Answers {
        let rgb = |role| {
            let rgb = self.palette.ramp().get(role);
            (rgb.r, rgb.g, rgb.b)
        };
        Answers { foreground: rgb(Role::Text), background: rgb(Role::Ground) }
    }

    /// A program wrote something, or ended.
    pub(super) fn terminal_report(&mut self, report: Report) -> Outcome {
        match report {
            Report::Output { id, bytes } => {
                let answers = self.answers();
                let Some(term) = self.panel.term_mut(id) else { return Outcome::Continue };
                let effects = term.emulator.feed(&bytes, answers);
                if let Some(pty) = &term.pty {
                    pty.consumed(bytes.len());
                }
                self.carry_out_effects(id, effects);
                if self.panel.shown() { Outcome::Redraw } else { Outcome::Continue }
            }
            Report::Exited { id } => {
                // A shell that has gone takes its terminal with it, as it
                // would take its window.
                let was = self.panel.focused() == Some(id);
                self.panel.remove(id);
                if self.panel.groups.is_empty() && self.focus == Focus::Terminal {
                    self.focus = Focus::Editor;
                }
                if was {
                    self.panel.had_focus = None;
                }
                Outcome::Redraw
            }
        }
    }

    /// Do what a program's output asked of the terminal around it.
    fn carry_out_effects(&mut self, id: Id, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Reply(bytes) => {
                    if let Some(pty) = self.panel.term(id).and_then(|term| term.pty.as_ref()) {
                        pty.write(bytes);
                    }
                }
                Effect::Copy(text) => self.copy(text),
                Effect::Bell => {}
            }
        }
    }

    /// When a program's synchronized update should be drawn unfinished.
    pub(super) fn panel_deadline(&self) -> Option<Instant> {
        self.panel.terms.iter().filter_map(|term| term.emulator.sync_deadline()).min()
    }

    /// Draw the updates whose programs never said they had finished.
    pub(super) fn panel_tick(&mut self, now: Instant) -> Outcome {
        let answers = self.answers();
        let mut outcome = Outcome::Continue;
        let due: Vec<Id> = self
            .panel
            .terms
            .iter()
            .filter(|term| term.emulator.sync_deadline().is_some_and(|deadline| deadline <= now))
            .map(|term| term.id)
            .collect();
        for id in due {
            if let Some(term) = self.panel.term_mut(id) {
                let effects = term.emulator.end_sync(answers);
                self.carry_out_effects(id, effects);
                outcome = Outcome::Redraw;
            }
        }
        outcome
    }

    /// Tell the programs that asked when the keyboard comes and goes.
    pub(super) fn panel_focus_follow(&mut self) {
        let now = (self.focus == Focus::Terminal).then(|| self.panel.focused()).flatten();
        let before = std::mem::replace(&mut self.panel.had_focus, now);
        if before == now {
            return;
        }
        for (id, focused) in [(before, false), (now, true)] {
            let Some(term) = id.and_then(|id| self.panel.term(id)) else { continue };
            if let (Some(pty), Some(bytes)) =
                (&term.pty, input::focus(focused, term.emulator.modes()))
            {
                pty.write(bytes);
            }
        }
    }

    // ── commands ────────────────────────────────────────────────────────────

    /// Start a shell, in a tab of its own or beside the one being used.
    fn spawn_terminal(&mut self, beside: bool) -> Outcome {
        let cwd = self
            .panel
            .start
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let id = match self.start_shell(cwd) {
            Ok(id) => id,
            Err(message) => {
                self.message = Some(message);
                return Outcome::Redraw;
            }
        };
        if let Some(group) = self.panel.groups.get_mut(self.panel.active).filter(|_| beside) {
            group.focus = (group.focus + 1).min(group.terms.len());
            group.terms.insert(group.focus, id);
        } else {
            self.panel.groups.push(Group { terms: vec![id], focus: 0 });
            self.panel.active = self.panel.groups.len() - 1;
        }
        self.panel.visible = true;
        self.focus = Focus::Terminal;
        Outcome::Redraw
    }

    /// Start a shell in `cwd` and keep track of it, in no tab yet. `Err`
    /// says why it could not be started.
    fn start_shell(&mut self, cwd: PathBuf) -> Result<Id, String> {
        let Some(post) = self.panel.post.clone() else {
            return Err("The terminal is not available here.".into());
        };
        // A first guess at the size; the layout fits it before the shell
        // has drawn anything worth keeping.
        let size = Size::new(self.viewport.width.max(2), (self.viewport.height / 3).max(2));
        let spec = match &self.panel.program {
            Some(command_line) => {
                let args: Vec<&str> = command_line[1..].iter().map(String::as_str).collect();
                Spec::program(&command_line[0], &args, cwd.clone(), size)
            }
            None => Spec::shell(cwd.clone(), size),
        };
        let name = spec
            .args
            .iter()
            .rev()
            .find(|arg| !arg.starts_with('-'))
            .and_then(|program| Path::new(program).file_name())
            .map_or_else(|| "shell".to_string(), |name| name.to_string_lossy().into_owned());
        let id = self.panel.next_id;
        let report = Arc::new(move |report| post(Event::Term(report)));
        let pty = match Pty::spawn(id, &spec, report) {
            Ok(pty) => pty,
            Err(error) => return Err(format!("Could not start a shell: {error}")),
        };
        self.panel.next_id += 1;
        let emulator = Emulator::new(size, nun_term::emulator::SCROLLBACK);
        self.panel.terms.push(Terminal { id, pty: Some(pty), emulator, name, cwd });
        Ok(id)
    }

    /// Run one of the panel's commands.
    pub(super) fn terminal_command(&mut self, command: Command) -> Outcome {
        match command {
            Command::ToggleTerminal if self.focus == Focus::Terminal => {
                self.focus = Focus::Editor;
                Outcome::Redraw
            }
            Command::ToggleTerminal | Command::NextTerminal if self.panel.groups.is_empty() => {
                self.spawn_terminal(false)
            }
            Command::ToggleTerminal => {
                self.panel.visible = true;
                self.focus = Focus::Terminal;
                Outcome::Redraw
            }
            Command::NewTerminal => self.spawn_terminal(false),
            Command::SplitTerminal => self.spawn_terminal(true),
            Command::NextTerminal => {
                let panel = &mut self.panel;
                if let Some(group) = panel.groups.get_mut(panel.active) {
                    if group.focus + 1 < group.terms.len() {
                        group.focus += 1;
                    } else {
                        group.focus = 0;
                        panel.active = (panel.active + 1) % panel.groups.len();
                    }
                }
                panel.visible = true;
                self.focus = Focus::Terminal;
                Outcome::Redraw
            }
            Command::CloseTerminal => {
                let Some(id) = self.panel.focused().filter(|_| self.panel.shown()) else {
                    self.message = Some("No terminal to close.".into());
                    return Outcome::Redraw;
                };
                self.panel.remove(id);
                if self.panel.groups.is_empty() {
                    self.focus = Focus::Editor;
                }
                Outcome::Redraw
            }
            Command::HideTerminal => {
                self.panel.visible = false;
                if self.focus == Focus::Terminal {
                    self.focus = Focus::Editor;
                }
                Outcome::Redraw
            }
            _ => Outcome::Continue,
        }
    }

    // ── the keyboard ────────────────────────────────────────────────────────

    /// A key pressed while a terminal has the keyboard. `None` when it is
    /// not the terminal's: the key back to the editor, or one no program can
    /// be sent, which the editor's bindings then get.
    pub(super) fn terminal_key(&mut self, event: &KeyEvent) -> Option<Outcome> {
        if self.focus != Focus::Terminal {
            return None;
        }
        let Some(id) = self.panel.focused().filter(|_| self.panel.shown()) else {
            self.focus = Focus::Editor;
            return None;
        };
        if let Some(key) = super::to_key(event)
            && self.keymap.get(&[key]) == Some(&Command::ToggleTerminal)
        {
            return None;
        }
        let (key, held) = term_key(event)?;
        let modes = self.panel.term(id)?.emulator.modes();
        let bytes = input::key(key, held, modes)?;
        self.acknowledge();
        let term = self.panel.term_mut(id)?;
        term.emulator.scroll_to_bottom();
        term.emulator.deselect();
        if let Some(pty) = &term.pty {
            pty.write(bytes);
        }
        Some(Outcome::Redraw)
    }

    /// Text pasted while a terminal has the keyboard.
    pub(super) fn terminal_paste(&mut self, text: &str) -> Outcome {
        let Some(term) = self.panel.focused().and_then(|id| self.panel.term_mut(id)) else {
            return Outcome::Continue;
        };
        term.emulator.scroll_to_bottom();
        if let Some(pty) = &term.pty {
            pty.write(input::paste(text, term.emulator.modes()));
        }
        Outcome::Redraw
    }

    // ── the mouse ───────────────────────────────────────────────────────────

    /// The pointer did something the panel may want. `None` when it is not
    /// the panel's to handle.
    pub(super) fn panel_pointer(
        &mut self,
        mouse: MouseEvent,
        target: Option<Target>,
        now: Instant,
    ) -> Option<Outcome> {
        let before = self.hovered_link();
        self.panel.pointer = match target {
            Some(Target::TermScreen(id)) => {
                self.screen_cell(id, mouse.column, mouse.row).map(|(row, col)| (id, row, col))
            }
            _ => None,
        };
        // Only the link under the pointer is drawn differently for it.
        let moved = if before == self.hovered_link() { Outcome::Continue } else { Outcome::Redraw };
        if let Some(drag) = self.panel.drag {
            return Some(self.panel_drag(drag, mouse).and(moved));
        }
        if self.menu.is_some() || self.prompt.is_some() || self.finder.is_some() {
            return None;
        }
        let outcome = match target? {
            Target::TermScreen(id) => self.screen_pointer(id, mouse, now),
            target if !target.in_panel() => return None,
            target => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => self.header_press(target),
                MouseEventKind::Down(MouseButton::Middle) => match target {
                    Target::TermTab(index) | Target::TermTabClose(index) => {
                        self.close_tab_group(index)
                    }
                    _ => Outcome::Continue,
                },
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let down = mouse.kind == MouseEventKind::ScrollDown;
                    self.panel.strip_scroll = if down {
                        self.panel.strip_scroll.saturating_add(4)
                    } else {
                        self.panel.strip_scroll.saturating_sub(4)
                    };
                    Outcome::Redraw
                }
                _ => Outcome::Continue,
            },
        };
        Some(outcome.and(moved))
    }

    /// The row and column of terminal `id` under the pointer at `(x, y)`,
    /// kept inside its screen.
    fn screen_cell(&self, id: Id, x: u16, y: u16) -> Option<(u16, u16)> {
        let (_, area) =
            self.screens_in(self.panel_area()?).into_iter().find(|(at, _)| *at == id)?;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let col = x.clamp(area.x, area.right() - 1) - area.x;
        let row = y.clamp(area.y, area.bottom() - 1) - area.y;
        Some((row, col))
    }

    /// A press on the header, one of its tabs, or one of its buttons.
    fn header_press(&mut self, target: Target) -> Outcome {
        self.acknowledge();
        match target {
            Target::TermHeader => {
                self.panel.drag = Some(Drag::Resize);
                Outcome::Redraw
            }
            Target::TermTab(index) => {
                self.panel.active = index.min(self.panel.groups.len().saturating_sub(1));
                self.focus = Focus::Terminal;
                Outcome::Redraw
            }
            Target::TermTabClose(index) => self.close_tab_group(index),
            Target::TermButton(TermButton::New) => self.run(Command::NewTerminal),
            Target::TermButton(TermButton::Split) => self.run(Command::SplitTerminal),
            Target::TermButton(TermButton::Close) => self.run(Command::CloseTerminal),
            Target::TermButton(TermButton::Hide) => self.run(Command::HideTerminal),
            _ => Outcome::Continue,
        }
    }

    /// Close every terminal in tab `index`.
    fn close_tab_group(&mut self, index: usize) -> Outcome {
        let Some(group) = self.panel.groups.get(index).cloned() else { return Outcome::Continue };
        for id in group.terms {
            self.panel.remove(id);
        }
        if self.panel.groups.is_empty() && self.focus == Focus::Terminal {
            self.focus = Focus::Editor;
        }
        Outcome::Redraw
    }

    /// The pointer did something over terminal `id`'s screen.
    fn screen_pointer(&mut self, id: Id, mouse: MouseEvent, now: Instant) -> Outcome {
        let Some((row, col)) = self.screen_cell(id, mouse.column, mouse.row) else {
            return Outcome::Continue;
        };
        let held = pointer_mods(mouse.modifiers);
        let Some(modes) = self.panel.term(id).map(|term| term.emulator.modes()) else {
            return Outcome::Continue;
        };
        // Shift keeps the mouse for selecting, whatever the program wants.
        let program = modes.mouse() && !mouse.modifiers.contains(KeyModifiers::SHIFT);
        let pointer = match mouse.kind {
            MouseEventKind::Down(button) => Some(input::Pointer::Press(term_button(button))),
            MouseEventKind::ScrollUp => Some(input::Pointer::WheelUp),
            MouseEventKind::ScrollDown => Some(input::Pointer::WheelDown),
            MouseEventKind::Moved => Some(input::Pointer::Move),
            _ => None,
        };
        if program && let Some(pointer) = pointer {
            if let MouseEventKind::Down(button) = mouse.kind {
                self.give_terminal_focus(id);
                self.panel.drag = Some(Drag::Program { id, button });
            }
            self.send_pointer(id, pointer, row, col, held);
            return Outcome::Redraw;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                self.wheel_terminal(id, up)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.acknowledge();
                self.chords.cancel();
                self.give_terminal_focus(id);
                let clicks = self.clicks.press(mouse.column, mouse.row, now);
                let pick = match clicks {
                    1 => Pick::Chars,
                    2 => Pick::Words,
                    _ => Pick::Lines,
                };
                if let Some(term) = self.panel.term_mut(id) {
                    term.emulator.select(row, col, pick, false);
                }
                self.panel.drag = Some(Drag::Select { id, clicks, moved: false });
                Outcome::Redraw
            }
            MouseEventKind::Down(_) => {
                self.give_terminal_focus(id);
                Outcome::Redraw
            }
            _ => Outcome::Continue,
        }
    }

    /// Give terminal `id` the keyboard.
    fn give_terminal_focus(&mut self, id: Id) {
        if let Some(group) = self.panel.groups.get_mut(self.panel.active)
            && let Some(at) = group.terms.iter().position(|term| *term == id)
        {
            group.focus = at;
        }
        self.focus = Focus::Terminal;
    }

    fn send_pointer(&self, id: Id, pointer: input::Pointer, row: u16, col: u16, held: input::Mods) {
        let Some(term) = self.panel.term(id) else { return };
        if let (Some(pty), Some(bytes)) =
            (&term.pty, input::pointer(pointer, col, row, held, term.emulator.modes()))
        {
            pty.write(bytes);
        }
    }

    /// The wheel turned over a terminal whose program did not ask for it:
    /// the arrows for a full-screen program that reads them, and the
    /// scrollback for anything else.
    fn wheel_terminal(&mut self, id: Id, up: bool) -> Outcome {
        let Some(term) = self.panel.term_mut(id) else { return Outcome::Continue };
        let lines = usize::try_from(WHEEL_LINES).unwrap_or(1);
        if let Some(arrows) = input::wheel_as_arrows(up, lines, term.emulator.modes()) {
            if let Some(pty) = &term.pty {
                pty.write(arrows);
            }
            return Outcome::Continue;
        }
        term.emulator.scroll(if up { WHEEL_LINES } else { -WHEEL_LINES });
        Outcome::Redraw
    }

    /// The pointer moved or a button came up while the panel has a drag.
    fn panel_drag(&mut self, drag: Drag, mouse: MouseEvent) -> Outcome {
        let released = matches!(mouse.kind, MouseEventKind::Up(_));
        match drag {
            Drag::Resize => {
                if released {
                    self.panel.drag = None;
                } else if let Some(panel) = self.panel_area() {
                    let bottom = panel.bottom();
                    self.panel.height = Some(bottom.saturating_sub(mouse.row).max(MIN_HEIGHT));
                }
                Outcome::Redraw
            }
            Drag::Program { id, button } => {
                let Some((row, col)) = self.screen_cell(id, mouse.column, mouse.row) else {
                    self.panel.drag = None;
                    return Outcome::Continue;
                };
                let held = pointer_mods(mouse.modifiers);
                let button = term_button(button);
                let pointer = if released {
                    self.panel.drag = None;
                    input::Pointer::Release(button)
                } else {
                    input::Pointer::Drag(button)
                };
                self.send_pointer(id, pointer, row, col, held);
                Outcome::Continue
            }
            Drag::Select { id, clicks, moved } => {
                let Some((row, col)) = self.screen_cell(id, mouse.column, mouse.row) else {
                    self.panel.drag = None;
                    return Outcome::Continue;
                };
                if released {
                    self.panel.drag = None;
                    return self.selection_done(id, row, col, clicks == 1 && !moved);
                }
                if matches!(mouse.kind, MouseEventKind::Drag(_)) {
                    self.panel.drag = Some(Drag::Select { id, clicks, moved: true });
                    if let Some(term) = self.panel.term_mut(id) {
                        term.emulator.extend(row, col, true);
                    }
                    return Outcome::Redraw;
                }
                Outcome::Continue
            }
        }
    }

    /// The button came up at the end of a selection: a click on a link
    /// follows it, and anything selected is copied.
    fn selection_done(&mut self, id: Id, row: u16, col: u16, click: bool) -> Outcome {
        if click
            && let Some(link) = self.panel.term(id).and_then(|term| Self::link_at(term, row, col))
        {
            if let Some(term) = self.panel.term_mut(id) {
                term.emulator.deselect();
            }
            return self.follow_terminal_link(link);
        }
        let Some(text) = self.panel.term(id).and_then(|term| term.emulator.selected_text()) else {
            return Outcome::Redraw;
        };
        self.copy(text);
        Outcome::Redraw
    }

    // ── links ───────────────────────────────────────────────────────────────

    /// What can be followed at a cell: an OSC 8 link the program made, or a
    /// path or address in the text.
    fn link_at(term: &Terminal, row: u16, col: u16) -> Option<TermLink> {
        if let Some((uri, ..)) = term.emulator.link_at(row, col) {
            return Some(TermLink::Uri(uri));
        }
        let (text, columns) = term.emulator.row_text(row);
        let index = columns.iter().rposition(|start| *start <= col)?;
        let found = links::at(&text, index)?;
        Some(match found.target {
            links::Target::Url(url) => TermLink::Uri(url),
            links::Target::File { path, line, column } => {
                let base = term.emulator.cwd().unwrap_or(&term.cwd);
                TermLink::File { path: resolve(&path, base), line, column }
            }
        })
    }

    /// The link under the pointer: which terminal, and where it is in it.
    fn hovered_link(&self) -> Option<(Id, (u16, u16, u16))> {
        let (id, row, col) = self.panel.pointer?;
        Some((id, Self::link_span(self.panel.term(id)?, row, col)?))
    }

    /// The columns of the link at a cell, to underline.
    fn link_span(term: &Terminal, row: u16, col: u16) -> Option<(u16, u16, u16)> {
        if let Some((_, start, end)) = term.emulator.link_at(row, col) {
            return Some((row, start, end));
        }
        let (text, columns) = term.emulator.row_text(row);
        let index = columns.iter().rposition(|start| *start <= col)?;
        let found = links::at(&text, index)?;
        let start = *columns.get(found.start)?;
        let end = columns.get(found.end).copied().unwrap_or(term.emulator.size().cols);
        Some((row, start, end))
    }

    fn follow_terminal_link(&mut self, link: TermLink) -> Outcome {
        match link {
            TermLink::Uri(uri) => self.follow_link(&uri),
            TermLink::File { path, line, column } => {
                if !path.is_file() {
                    self.message = Some(format!("{} is not a file here.", path.display()));
                    return Outcome::Redraw;
                }
                let at = lsp::Position {
                    line: line.unwrap_or(1).saturating_sub(1),
                    character: column.unwrap_or(1).saturating_sub(1),
                };
                let place = Place {
                    path,
                    range: lsp::Range { start: at, end: at },
                    encoding: Encoding::Utf32,
                };
                self.pick_place(&place, Open::Here)
            }
        }
    }

    // ── the clipboard ───────────────────────────────────────────────────────

    /// Copy `text`, on the clipboard's thread, which says how that went
    /// once it knows: nothing is said to have been copied before it has
    /// been, and a copy sent through the terminal is said to have been sent.
    /// See [`crate::clipboard`] for where it goes.
    #[cfg(not(test))]
    fn copy(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        let place = self.clipboard_place(|name| std::env::var_os(name).is_some());
        let setting = self.clipboard_setting();
        if self.panel.copier.is_none() {
            let Some(post) = self.panel.post.clone() else { return };
            self.panel.copier = clipboard::worker(post);
        }
        let sent = self
            .panel
            .copier
            .as_ref()
            .is_some_and(|copier| copier.send(clipboard::Job { text, setting, place }).is_ok());
        if !sent {
            self.message = Some("Could not copy: the clipboard's thread would not start.".into());
        }
    }

    #[cfg(test)]
    fn copy(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        let chars = text.chars().count();
        let place = self.clipboard_place(|_| false);
        let outcome = clipboard::copy(&text, self.clipboard_setting(), place, &clipboard::Faked);
        self.panel.copied.push(text);
        self.copied(chars, outcome);
    }

    /// How a copy of `chars` characters went.
    pub(super) fn copied(&mut self, chars: usize, outcome: CopyOutcome) -> Outcome {
        let count = match chars {
            1 => "1 character".to_string(),
            chars => format!("{chars} characters"),
        };
        self.message = Some(match outcome {
            CopyOutcome::Taken => format!("Copied {count}."),
            CopyOutcome::Sent(to) => format!("Sent {count} to {to}."),
            CopyOutcome::Escape { bytes, to } => {
                self.panel.escapes.push(bytes);
                format!("Sent {count} to {to}.")
            }
            CopyOutcome::Failed(problem) => format!("Could not copy: {problem}."),
        });
        Outcome::Redraw
    }

    /// Escapes for the main loop to write to the terminal nun is drawn on,
    /// between frames: copies through OSC 52.
    pub fn take_escapes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.panel.escapes)
    }
}

/// Something in a terminal that can be followed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TermLink {
    /// A URL: the web, or a file by its `file:` address.
    Uri(String),
    /// A file named in the output, perhaps with a place in it.
    File { path: PathBuf, line: Option<u32>, column: Option<u32> },
}

/// A path from a program's output, made absolute against the directory the
/// program was in, and `~` expanded as the shell would have.
fn resolve(path: &str, base: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let (Some(rest), Some(home)) = (path.strip_prefix("~/"), home) {
        return home.join(rest);
    }
    base.join(path)
}

/// A keystroke as a terminal key, if it is one a program can be sent.
fn term_key(event: &KeyEvent) -> Option<(input::Key, input::Mods)> {
    use input::Key;
    let key = match event.code {
        KeyCode::Char(ch) => Key::Char(ch),
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Esc => Key::Esc,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Insert => Key::Insert,
        KeyCode::Delete => Key::Delete,
        KeyCode::F(n) => Key::F(n),
        _ => return None,
    };
    // Cmd never reaches a program in a terminal; it is the editor's.
    if event.modifiers.contains(KeyModifiers::SUPER) {
        return None;
    }
    Some((key, pointer_mods(event.modifiers)))
}

/// The modifiers held, as a terminal reports them.
fn pointer_mods(modifiers: KeyModifiers) -> input::Mods {
    [
        (KeyModifiers::SHIFT, input::Mods::SHIFT),
        (KeyModifiers::ALT, input::Mods::ALT),
        (KeyModifiers::CONTROL, input::Mods::CTRL),
    ]
    .into_iter()
    .filter(|(flag, _)| modifiers.contains(*flag))
    .fold(input::Mods::NONE, |mods, (_, ours)| mods.union(ours))
}

const fn term_button(button: MouseButton) -> input::Button {
    match button {
        MouseButton::Left => input::Button::Left,
        MouseButton::Middle => input::Button::Middle,
        MouseButton::Right => input::Button::Right,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    use nun_core::Buffer;
    use nun_theme::{Probe, derive};
    use nun_ui::Palette;

    use super::*;

    /// Long enough for a loaded machine running the whole suite at once.
    const PATIENCE: Duration = Duration::from_secs(10);

    struct Rig {
        app: App,
        events: Receiver<Event>,
    }

    /// An editor 80 by 24 whose terminals run `script` under `sh`.
    fn rig(script: &str) -> Rig {
        let mut app = App::new(
            Buffer::from_text("one\ntwo\n"),
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Basic),
        );
        app.set_viewport(Rect::new(0, 0, 80, 24));
        let (sender, events) = mpsc::channel();
        app.attach_terminal(
            std::env::temp_dir(),
            Arc::new(move |event| {
                let _ = sender.send(event);
            }),
        );
        app.panel.program = Some(vec!["/bin/sh".into(), "-c".into(), script.into()]);
        Rig { app, events }
    }

    impl Rig {
        /// Hand the editor what the programs report until `done` holds.
        fn until(&mut self, what: &str, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + PATIENCE;
            while !done(&self.app) {
                let left = deadline.saturating_duration_since(Instant::now());
                let Ok(event) = self.events.recv_timeout(left) else {
                    panic!("gave up waiting for {what}; the screen says {:?}", self.screen());
                };
                self.app.handle(event);
            }
        }

        /// Until the terminal with the keyboard shows `text`.
        fn until_shown(&mut self, text: &str) {
            let wanted = text.to_string();
            self.until(text, move |app| screen_of(app).contains(&wanted));
        }

        fn screen(&self) -> String {
            screen_of(&self.app)
        }

        fn press(&mut self, code: KeyCode) -> Outcome {
            self.app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        }

        fn type_text(&mut self, text: &str) {
            for ch in text.chars() {
                self.press(KeyCode::Char(ch));
            }
        }

        fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) {
            self.app.handle(Event::Mouse(MouseEvent { kind, column, row, modifiers }));
        }

        /// Where terminal `index` of the tab showing is on screen.
        fn screen_area(&self, index: usize) -> Rect {
            let panel = self.app.panel_area().expect("the panel is showing");
            self.app.screens_in(panel)[index].1
        }
    }

    fn screen_of(app: &App) -> String {
        app.panel
            .focused()
            .and_then(|id| app.panel.term(id))
            .map_or_else(String::new, |term| term.emulator.contents())
    }

    #[test]
    fn the_toggle_key_opens_a_shell_types_into_it_and_comes_back() {
        let mut rig = rig("printf 'ready\\n'; read line; echo \"got <$line>\"; sleep 30");
        assert_eq!(rig.press(KeyCode::F(6)), Outcome::Redraw);
        assert_eq!(rig.app.focus, Focus::Terminal);
        assert!(rig.app.panel_area().is_some());
        rig.until_shown("ready");
        // Keys the editor would take for itself go to the program instead.
        rig.type_text("hi");
        rig.app.handle(Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)));
        rig.press(KeyCode::Enter);
        rig.until_shown("got <hi");
        assert!(rig.app.finder.is_none(), "Ctrl+P went to the program, not the palette");
        assert_eq!(
            rig.app.doc().buffer.text().to_string(),
            "one\ntwo\n",
            "nothing reached the document"
        );

        rig.press(KeyCode::F(6));
        assert_eq!(rig.app.focus, Focus::Editor);
        rig.type_text("x");
        assert_eq!(rig.app.doc().buffer.text().to_string(), "xone\ntwo\n");
        assert!(rig.app.panel_area().is_some(), "the panel stays where it was");
    }

    #[test]
    fn the_panel_takes_its_rows_from_the_panes_and_its_header_says_the_key_back() {
        let mut rig = rig("sleep 30");
        let before = rig.app.panes_area();
        rig.press(KeyCode::F(6));
        let panel = rig.app.panel_area().unwrap();
        let above = rig.app.panes_area();
        assert_eq!(above.bottom(), panel.y, "the panes end where the panel starts");
        assert_eq!(panel.bottom(), 23, "the status line stays at the bottom");
        assert!(above.height < before.height);

        let mut cells = Cells::empty(rig.app.viewport);
        rig.app.render(rig.app.viewport, &mut cells);
        let header: String = (0..80).map(|x| cells[(x, panel.y)].symbol().to_string()).collect();
        assert!(header.contains("F6 to the editor"), "{header:?}");
    }

    #[test]
    fn a_click_on_a_file_position_in_the_output_opens_it_there() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();
        let script = "echo 'error: here'; echo '  --> main.rs:2:9'; sleep 30";
        let mut rig = rig(script);
        rig.app.panel.start = Some(dir.path().to_path_buf());
        rig.press(KeyCode::F(6));
        rig.until_shown("main.rs:2:9");
        let area = rig.screen_area(0);
        let (x, y) = (area.x + 8, area.y + 1);
        rig.mouse(MouseEventKind::Moved, x, y, KeyModifiers::NONE);
        assert!(rig.app.panel.pointer.is_some());
        rig.mouse(MouseEventKind::Down(MouseButton::Left), x, y, KeyModifiers::NONE);
        rig.mouse(MouseEventKind::Up(MouseButton::Left), x, y, KeyModifiers::NONE);

        assert_eq!(rig.app.focus, Focus::Editor, "{:?}", rig.app.message);
        assert_eq!(rig.app.doc().buffer.path().and_then(Path::file_name), file.file_name());
        let buffer = &rig.app.doc().buffer;
        let head = buffer.selections().primary().head;
        assert_eq!((buffer.line_of(head), buffer.column_of(head)), (1, 8));
    }

    #[test]
    fn dragging_across_the_output_selects_and_copies_it() {
        let mut rig = rig("echo 'alpha beta gamma'; sleep 30");
        rig.press(KeyCode::F(6));
        rig.until_shown("gamma");
        let area = rig.screen_area(0);
        rig.mouse(MouseEventKind::Down(MouseButton::Left), area.x + 6, area.y, KeyModifiers::NONE);
        rig.mouse(MouseEventKind::Drag(MouseButton::Left), area.x + 9, area.y, KeyModifiers::NONE);
        rig.mouse(MouseEventKind::Up(MouseButton::Left), area.x + 9, area.y, KeyModifiers::NONE);
        assert_eq!(rig.app.panel.copied, ["beta"]);
        assert_eq!(rig.app.message.as_deref(), Some("Copied 4 characters."));
    }

    #[test]
    fn a_copy_through_the_terminal_waits_for_the_main_loop_and_is_said_to_be_sent() {
        let mut rig = rig("true");
        let mut loaded = nun_config::Loaded::defaults();
        loaded.config.clipboard = nun_config::Clipboard::Osc52;
        let startup = crate::terminal::Startup {
            palette: Probe::builtin_dark(),
            kitty_keyboard: Some(true),
            underlines: nun_ui::UnderlineProbe::new(),
            attributes: vec![62, 22, 52],
        };
        rig.app.attach_settings(loaded, startup, crate::commands::KeySet::Basic, None);
        rig.app.copy("beta".into());
        assert_eq!(rig.app.take_escapes(), [crate::clipboard::osc52("beta")]);
        assert!(rig.app.take_escapes().is_empty(), "written once");
        assert_eq!(rig.app.message.as_deref(), Some("Sent 4 characters to the terminal to copy."));
    }

    #[test]
    fn a_program_that_asks_for_the_mouse_gets_it_and_shift_takes_it_back() {
        // SGR mouse reports, and nothing echoed but what cat shows of them.
        let script = "printf '\\033[?1000h\\033[?1006hon\\n'; stty -icanon -echo; cat -v";
        let mut rig = rig(script);
        rig.press(KeyCode::F(6));
        rig.until_shown("on");
        rig.until("the mouse mode", |app| {
            app.panel.term(app.panel.focused().unwrap()).unwrap().emulator.modes().mouse()
        });
        let area = rig.screen_area(0);
        rig.mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 4,
            area.y + 2,
            KeyModifiers::NONE,
        );
        rig.mouse(
            MouseEventKind::Up(MouseButton::Left),
            area.x + 4,
            area.y + 2,
            KeyModifiers::NONE,
        );
        rig.until_shown("^[[<0;5;3M^[[<0;5;3m");
        assert!(rig.app.panel.copied.is_empty());

        rig.mouse(MouseEventKind::Down(MouseButton::Left), area.x, area.y, KeyModifiers::SHIFT);
        rig.mouse(MouseEventKind::Drag(MouseButton::Left), area.x + 1, area.y, KeyModifiers::SHIFT);
        rig.mouse(MouseEventKind::Up(MouseButton::Left), area.x + 1, area.y, KeyModifiers::SHIFT);
        assert_eq!(rig.app.panel.copied, ["on"]);
    }

    #[test]
    fn dragging_the_header_resizes_the_panel_and_tells_the_program() {
        let script = "trap 'stty size' WINCH; echo ready; while :; do sleep 0.05; done";
        let mut rig = rig(script);
        rig.press(KeyCode::F(6));
        rig.until_shown("ready");
        let panel = rig.app.panel_area().unwrap();
        let rows = rig.screen_area(0).height;
        rig.mouse(MouseEventKind::Down(MouseButton::Left), 40, panel.y, KeyModifiers::NONE);
        rig.mouse(MouseEventKind::Drag(MouseButton::Left), 40, panel.y - 5, KeyModifiers::NONE);
        rig.mouse(MouseEventKind::Up(MouseButton::Left), 40, panel.y - 5, KeyModifiers::NONE);
        let taller = rig.screen_area(0).height;
        assert_eq!(taller, rows + 5);
        rig.until_shown(&format!("{taller} 80"));
    }

    #[test]
    fn split_puts_a_second_terminal_beside_the_first_and_next_goes_between() {
        let mut rig = rig("echo started; sleep 30");
        rig.press(KeyCode::F(6));
        let first = rig.app.panel.focused();
        rig.app.run(Command::SplitTerminal);
        let second = rig.app.panel.focused();
        assert_ne!(first, second);
        assert_eq!(rig.app.panel.groups.len(), 1, "one tab, split");
        let (left, right) = (rig.screen_area(0), rig.screen_area(1));
        assert_eq!(left.y, right.y);
        assert!(left.right() < right.x, "a rule between them");
        rig.app.run(Command::NextTerminal);
        assert_eq!(rig.app.panel.focused(), first);

        rig.app.run(Command::NewTerminal);
        assert_eq!(rig.app.panel.groups.len(), 2, "a tab of its own");
        rig.app.run(Command::CloseTerminal);
        assert_eq!(rig.app.panel.groups.len(), 1);
        assert_eq!(rig.app.panel.active, 0);
    }

    #[test]
    fn a_shell_that_exits_takes_its_terminal_and_the_keyboard_goes_back() {
        let mut rig = rig("echo bye");
        rig.press(KeyCode::F(6));
        rig.until("the shell to exit", |app| app.panel.groups.is_empty());
        assert_eq!(rig.app.focus, Focus::Editor);
        assert!(rig.app.panel_area().is_none());
    }

    #[test]
    fn hiding_leaves_the_shell_running_and_showing_brings_it_back() {
        let mut rig = rig("sleep 30");
        rig.press(KeyCode::F(6));
        let id = rig.app.panel.focused();
        rig.app.run(Command::HideTerminal);
        assert!(rig.app.panel_area().is_none());
        assert_eq!(rig.app.focus, Focus::Editor);
        rig.press(KeyCode::F(6));
        assert_eq!(rig.app.panel.focused(), id, "the same shell, not a new one");
        assert_eq!(rig.app.focus, Focus::Terminal);
    }

    #[test]
    fn shutting_down_ends_every_shell() {
        let mut rig = rig("trap '' HUP; sleep 30");
        rig.press(KeyCode::F(6));
        rig.app.run(Command::NewTerminal);
        let pids: Vec<u32> =
            rig.app.panel.terms.iter().filter_map(|term| term.pty.as_ref()).map(Pty::pid).collect();
        assert_eq!(pids.len(), 2);
        let started = Instant::now();
        rig.app.shutdown_terminals();
        assert!(started.elapsed() < Duration::from_secs(2), "hung up together, not in turn");
        for pid in pids {
            let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
            assert!(
                rustix::process::test_kill_process(pid).is_err(),
                "{pid:?} outlived the editor"
            );
        }
    }

    #[test]
    fn paths_are_relative_to_where_the_program_is() {
        let base = Path::new("/work/project");
        assert_eq!(resolve("src/main.rs", base), Path::new("/work/project/src/main.rs"));
        assert_eq!(resolve("/etc/hosts", base), Path::new("/etc/hosts"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(resolve("~/notes.md", base), Path::new(&home).join("notes.md"));
        }
    }
}
