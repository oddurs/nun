//! A question in the status line, answered by click or by key.
//!
//! Naming a new file, confirming a rename, deciding what to do with unsaved
//! changes: each is a sentence, sometimes a text field, and a row of buttons.
//! The buttons are real hit targets, so every prompt can be answered with the
//! mouse alone; Enter picks the first button and Esc the last.
//!
//! While a prompt is up, keys go to it. That is the same capture a text field
//! in any dialog has, and it ends the moment the prompt is answered — there is
//! no state to be in, only a question on screen.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{App, Outcome};

/// What answering the prompt goes on to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Purpose {
    /// Create a file with the typed name in this directory.
    NewFile(PathBuf),
    /// Create a folder with the typed name in this directory.
    NewFolder(PathBuf),
    /// Rename this entry to the typed name.
    Rename(PathBuf),
    /// Rename the symbol the rename in progress is about to the typed name.
    RenameSymbol,
    /// Unsaved changes are about to be left; then close this tab of this pane.
    UnsavedThenClose(usize, usize),
}

/// How the prompt was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Answer {
    /// Do it: create, rename, or save and carry on.
    Confirm,
    /// Carry on without saving.
    Discard,
    /// Never mind.
    Cancel,
}

/// A question on screen.
#[derive(Debug, Clone)]
pub(super) struct Prompt {
    pub(super) purpose: Purpose,
    pub(super) message: String,
    /// The text being typed, when the prompt asks for a name.
    pub(super) field: Option<String>,
    pub(super) buttons: Vec<(&'static str, Answer)>,
    /// A char index in the document being edited to draw the prompt beside,
    /// rather than in the status line: a new name is asked for at the name.
    pub(super) anchor: Option<usize>,
}

impl Prompt {
    /// Ask for a name, with `initial` already typed.
    pub(super) fn name(purpose: Purpose, message: String, initial: &str) -> Self {
        let confirm = match purpose {
            Purpose::Rename(_) | Purpose::RenameSymbol => "Rename",
            _ => "Create",
        };
        Self {
            purpose,
            message,
            field: Some(initial.to_string()),
            buttons: vec![(confirm, Answer::Confirm), ("Cancel", Answer::Cancel)],
            anchor: None,
        }
    }

    /// The same prompt, drawn beside char `anchor` of the document being
    /// edited when that is on screen.
    pub(super) const fn at(mut self, anchor: Option<usize>) -> Self {
        self.anchor = anchor;
        self
    }

    /// Ask what to do with unsaved changes before going on.
    pub(super) fn unsaved(purpose: Purpose, name: &str) -> Self {
        Self {
            purpose,
            message: format!("{name} has unsaved changes."),
            field: None,
            buttons: vec![
                ("Save", Answer::Confirm),
                ("Don't save", Answer::Discard),
                ("Cancel", Answer::Cancel),
            ],
            anchor: None,
        }
    }

    /// The text before the buttons, field included.
    pub(super) fn text(&self) -> String {
        match &self.field {
            Some(field) => format!(" {} {field}", self.message),
            None => format!(" {}", self.message),
        }
    }

    /// Where each button is drawn in the status line `area`, right-aligned.
    pub(super) fn button_areas(&self, area: Rect) -> Vec<Rect> {
        let mut right = area.right().saturating_sub(1);
        let mut areas = Vec::with_capacity(self.buttons.len());
        for (label, _) in self.buttons.iter().rev() {
            let width = u16::try_from(label.width() + 2).unwrap_or(u16::MAX);
            let Some(x) = right.checked_sub(width) else { break };
            if x < area.x {
                break;
            }
            areas.push(Rect::new(x, area.y, width, 1));
            right = x.saturating_sub(1);
        }
        areas.reverse();
        // Buttons that did not fit are left out from the left, so the last
        // (Cancel) always survives on a narrow terminal.
        let skipped = self.buttons.len() - areas.len();
        let mut all = vec![Rect::default(); skipped];
        all.extend(areas);
        all
    }
}

impl App {
    /// Where the prompt is drawn: beside its anchor, when it has one on
    /// screen, and in the status line otherwise.
    ///
    /// Beside means the row under the anchor's line, or the one over it when
    /// the line is the last on screen, starting at the anchor's column and
    /// pulled left as far as it has to be to fit. It is as wide as its text
    /// and buttons with room to type, and never wider than the text.
    pub(super) fn prompt_area(&self, status: Rect) -> Rect {
        self.prompt
            .as_ref()
            .and_then(|prompt| Some((prompt, prompt.anchor?)))
            .and_then(|(prompt, at)| self.beside(prompt, at))
            .unwrap_or(status)
    }

    fn beside(&self, prompt: &Prompt, at: usize) -> Option<Rect> {
        let (text, _) = self.areas();
        if text.height < 2 {
            return None;
        }
        // Where the renderer itself put that char, so a wide character or a
        // tab before it cannot push the prompt somewhere else.
        let (x, y) = nun_ui::EditorView::new(&self.doc().buffer, &self.palette)
            .scrolled_to(self.doc().scroll)
            .cell_of(text, at)?;
        let y = if y + 1 < text.bottom() { y + 1 } else { y - 1 };

        let buttons: usize = prompt.buttons.iter().map(|(label, _)| label.width() + 3).sum();
        let typed = prompt.field.as_deref().map_or(0, UnicodeWidthStr::width);
        // Room to type a name longer than the one there, and a column for the
        // caret.
        let room = typed.max(16) - typed + 2;
        let wanted = prompt.text().width() + room + buttons + 1;
        let width = u16::try_from(wanted).unwrap_or(u16::MAX).min(text.width);
        let x = x.min(text.right().saturating_sub(width)).max(text.x);
        Some(Rect::new(x, y, width, 1))
    }

    /// A key while a prompt is up. Always handled: nothing reaches the text
    /// behind a question until it is answered.
    pub(super) fn prompt_key(&mut self, key: &KeyEvent) -> Outcome {
        let Some(prompt) = self.prompt.as_mut() else { return Outcome::Continue };
        let control = key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER);

        match key.code {
            KeyCode::Enter => return self.answer(Answer::Confirm),
            KeyCode::Esc => return self.answer(Answer::Cancel),
            KeyCode::Char(ch) if !control => {
                if let Some(field) = prompt.field.as_mut() {
                    field.push(ch);
                }
            }
            KeyCode::Backspace => {
                if let Some(field) = prompt.field.as_mut()
                    && let Some((at, _)) = field.grapheme_indices(true).next_back()
                {
                    field.truncate(at);
                }
            }
            _ => {}
        }
        Outcome::Redraw
    }

    /// Text pasted while a prompt is up goes into its field, on one line.
    pub(super) fn prompt_paste(&mut self, text: &str) {
        if let Some(field) = self.prompt.as_mut().and_then(|prompt| prompt.field.as_mut()) {
            field.extend(text.chars().filter(|ch| *ch != '\n' && *ch != '\r'));
        }
    }

    /// The prompt was answered.
    pub(super) fn answer(&mut self, answer: Answer) -> Outcome {
        let Some(prompt) = self.prompt.take() else { return Outcome::Continue };
        let name = prompt.field.unwrap_or_default();
        let name = name.trim();

        match (prompt.purpose, answer) {
            (Purpose::RenameSymbol, answer) => {
                return self.rename_named(answer == Answer::Confirm, name);
            }
            (_, Answer::Cancel) => {}
            (Purpose::NewFile(dir), _) => self.create(&dir, name, false),
            (Purpose::NewFolder(dir), _) => self.create(&dir, name, true),
            (Purpose::Rename(path), _) => self.rename(&path, name),
            (Purpose::UnsavedThenClose(pane, index), Answer::Confirm) => {
                // Formatted first if the language asks for that, in which case
                // the tab closes once the save has happened.
                let id = self.doc().id;
                if self.format_then_save(id, true) {
                    return Outcome::Redraw;
                }
                self.message = Some(self.write(id).unwrap_or_else(|failed| failed));
                // A save that failed leaves the changes unsaved; the tab stays
                // open rather than taking them away with it.
                if !self.doc().buffer.is_modified() {
                    self.drop_tab(pane, index);
                }
            }
            (Purpose::UnsavedThenClose(pane, index), Answer::Discard) => {
                self.drop_tab(pane, index);
            }
        }
        Outcome::Redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_are_right_aligned_in_order() {
        let prompt = Prompt::unsaved(Purpose::UnsavedThenClose(0, 0), "a.rs");
        let areas = prompt.button_areas(Rect::new(0, 9, 60, 1));
        assert_eq!(areas.len(), 3);
        assert!(areas[0].x < areas[1].x && areas[1].x < areas[2].x);
        assert_eq!(areas[2].right(), 59, "one column of margin");
    }

    #[test]
    fn on_a_narrow_line_cancel_survives() {
        let prompt = Prompt::unsaved(Purpose::UnsavedThenClose(0, 0), "a.rs");
        let areas = prompt.button_areas(Rect::new(0, 0, 12, 1));
        assert!(areas[0].width == 0, "Save did not fit");
        assert!(areas[2].width > 0, "Cancel always does");
    }
}
