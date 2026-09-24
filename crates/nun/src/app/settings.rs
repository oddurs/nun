//! The configuration, live: swapping a new one in, whitespace per file, and
//! asking about a project's own settings.
//!
//! What a new configuration changes, setting by setting:
//!
//! - **theme and glyphs** — the palette is derived again from the probe taken
//!   at startup and drawn from the next frame.
//! - **keys** — the keymap is built again, over the key set the terminal
//!   negotiated at startup.
//! - **ui** — the pointer and hover settings and `undercurl` apply at once.
//!   `mouse`, `alternate_screen` and `keyboard_enhancement` are how the
//!   terminal was entered, so they say they will apply at the next start.
//! - **lsp** — a language whose server changed has its documents moved to
//!   the new one, and a server nothing asks for any more is stopped; the rest
//!   keep running. `format_on_save` applies to the next save.
//! - **editor** — each open file's whitespace is worked out again, with its
//!   `.editorconfig`, and the next save follows it.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use nun_config::{
    Charset, Decision, EditorConfig, EndOfLine, IndentStyle, Loaded, News, Whitespace,
};
use nun_core::{Edit, LineEnding};
use nun_ui::{Palette, Underlines};

use super::panes::DocId;
use super::prompt::{Answer, Prompt, Purpose};
use super::{App, Outcome};
use crate::commands::KeySet;
use crate::reload::Reloader;
use crate::terminal::Startup;

/// The settings, and what they were worked out from.
#[derive(Debug, Default)]
pub struct Live {
    /// The configuration in force, once one is attached.
    loaded: Option<Loaded>,
    /// What the terminal said at startup, which the theme is derived from.
    startup: Option<Startup>,
    /// The key set the terminal was entered with.
    set: Option<KeySet>,
    /// The worker watching the files.
    reloader: Option<Reloader>,
    /// Each followed file's `.editorconfig` files, nearest first.
    editorconfigs: HashMap<PathBuf, Vec<EditorConfig>>,
    /// Each document's whitespace.
    whitespace: HashMap<DocId, Whitespace>,
    /// The line ending and byte-order mark each document had when it was
    /// read, to go back to when nothing asks for another any more.
    own: HashMap<DocId, (LineEnding, bool)>,
    /// The fingerprint of the project file last asked about, so one decision
    /// is asked for once however often the file is read.
    asked: Option<String>,
    /// Every problem with the settings already said, so a reload says only
    /// what is new.
    said: BTreeSet<String>,
    /// The same for the `.editorconfig` files, which are said per file.
    said_editorconfig: BTreeSet<String>,
    /// Underlines to switch the screen to, for the main loop.
    underlines: Option<Underlines>,
}

impl App {
    /// Take the configuration the editor was started with, what the terminal
    /// said about itself, and the worker that will say when it changes.
    /// Problems in it are taken as already said: the caller said them.
    pub fn attach_settings(
        &mut self,
        loaded: Loaded,
        startup: Startup,
        set: KeySet,
        reloader: Option<Reloader>,
    ) {
        let said = crate::all_problems(&loaded, &startup, set);
        self.settings = Live {
            said: said.into_iter().collect(),
            loaded: Some(loaded),
            startup: Some(startup),
            set: Some(set),
            reloader,
            ..Live::default()
        };
        let ids: Vec<DocId> = self.docs.iter().map(|document| document.id).collect();
        for id in ids {
            self.settings_follow(id);
        }
        self.ask_about_project();
    }

    /// Underlines the screen should switch to, once.
    pub fn take_underlines(&mut self) -> Option<Underlines> {
        self.settings.underlines.take()
    }

    /// A document was opened, reloaded or renamed: work out its whitespace,
    /// and have its `.editorconfig` followed.
    pub(super) fn settings_follow(&mut self, id: DocId) {
        let Some(path) = self.doc_by(id).and_then(|doc| doc.buffer.path().map(PathBuf::from))
        else {
            return;
        };
        if let Some(reloader) = &self.settings.reloader {
            reloader.follow(&path);
        }
        if let Some(doc) = self.doc_by(id) {
            let own = (doc.buffer.line_ending(), doc.buffer.has_bom());
            self.settings.own.insert(id, own);
        }
        self.apply_whitespace(id);
    }

    /// News from the worker.
    pub(super) fn config_news(&mut self, news: News) -> Outcome {
        match news {
            News::Settings(loaded) => self.apply_settings(&loaded),
            News::EditorConfig { path, configs } => {
                self.settings.editorconfigs.insert(path.clone(), configs);
                let ids: Vec<DocId> = self
                    .docs
                    .iter()
                    .filter(|doc| doc.buffer.path() == Some(path.as_path()))
                    .map(|doc| doc.id)
                    .collect();
                for id in ids {
                    self.apply_whitespace(id);
                }
            }
            News::NotRemembered(why) => {
                self.warn(format!("Could not remember the decision about this project: {why}"));
            }
        }
        Outcome::Redraw
    }

    /// Swap in a new configuration, and apply whatever it changed.
    fn apply_settings(&mut self, loaded: &Loaded) {
        let old = self.settings.loaded.replace(loaded.clone());
        if let (Some(startup), Some(set)) = (self.settings.startup.clone(), self.settings.set) {
            let (ramp, _) = crate::build_ramp(&startup.palette, loaded);
            self.palette = Palette::new(ramp).with_glyphs(crate::glyphs(loaded).glyphs);
            let (keymap, _) = crate::commands::keymap(set, &loaded.config.keys);
            self.keymap = keymap;
            let underlines = crate::underlines(loaded, &startup.underlines);
            if old
                .as_ref()
                .is_none_or(|old| crate::underlines(old, &startup.underlines) != underlines)
            {
                self.settings.underlines = Some(underlines);
            }
            self.say_new_problems(&crate::all_problems(loaded, &startup, set));
        }
        crate::pointer_settings(self, &loaded.config);
        self.set_format_on_save(crate::format_on_save(loaded));
        self.reconfigure_servers(loaded);
        if let Some(old) = &old {
            for key in ["ui.mouse", "ui.alternate_screen", "ui.keyboard_enhancement"] {
                if old.config.get(key) != loaded.config.get(key) {
                    self.warn(format!("{key} changes how nun enters the terminal; it applies next time nun starts."));
                }
            }
        }
        let ids: Vec<DocId> = self.docs.iter().map(|document| document.id).collect();
        for id in ids {
            self.apply_whitespace(id);
        }
        self.ask_about_project();
    }

    /// Move documents whose language's server changed to the new one.
    fn reconfigure_servers(&mut self, loaded: &Loaded) {
        let Some(lsp) = self.lsp.as_mut() else { return };
        let (servers, _) = crate::language_servers(loaded);
        let changed = lsp.reconfigure(servers);
        if changed.is_empty() {
            return;
        }
        let ids: Vec<DocId> = self
            .docs
            .iter()
            .filter(|doc| {
                let language = doc.buffer.path().and_then(nun_lsp::language_of);
                language.is_some_and(|language| changed.iter().any(|name| name == language.config))
            })
            .map(|doc| doc.id)
            .collect();
        for id in ids {
            self.lsp_open(id);
        }
    }

    /// Say the first problem not said before, and how many more there are.
    fn say_new_problems(&mut self, problems: &[String]) {
        let fresh: Vec<&String> =
            problems.iter().filter(|problem| !self.settings.said.contains(*problem)).collect();
        if let Some(first) = fresh.first() {
            let more = match fresh.len() - 1 {
                0 => String::new(),
                more => format!(" ({more} more: `nun config` lists them)"),
            };
            self.warn(format!("Config: {first}{more}"));
        }
        self.settings.said = problems.iter().cloned().collect();
    }

    /// Work out a document's whitespace again, and set its buffer to it.
    fn apply_whitespace(&mut self, id: DocId) {
        let Some(loaded) = self.settings.loaded.as_ref() else { return };
        let Some(document) = self.docs.iter_mut().find(|doc| doc.id == id) else { return };
        let Some(path) = document.buffer.path().map(PathBuf::from) else {
            document.buffer.set_tab_width(loaded.config.tab_width);
            return;
        };
        let configs = self.settings.editorconfigs.get(&path).map_or(&[][..], Vec::as_slice);
        let whitespace = Whitespace::resolve(loaded, &path, configs);
        let buffer = &mut document.buffer;
        let (own_ending, own_bom) =
            self.settings.own.get(&id).copied().unwrap_or((buffer.line_ending(), buffer.has_bom()));
        buffer.set_tab_width(whitespace.tab_width);
        buffer.set_line_ending(match whitespace.end_of_line {
            Some(EndOfLine::Lf) => LineEnding::Lf,
            Some(EndOfLine::Crlf) => LineEnding::Crlf,
            None => own_ending,
        });
        buffer.set_bom(match whitespace.charset {
            Some(Charset::Utf8) => false,
            Some(Charset::Utf8Bom) => true,
            None => own_bom,
        });
        let problems: Vec<String> =
            whitespace.problems.iter().map(nun_config::Problem::brief).collect();
        let said = &mut self.settings.said_editorconfig;
        let fresh = problems.iter().find(|problem| !said.contains(*problem)).cloned();
        said.extend(problems);
        self.settings.whitespace.insert(id, whitespace);
        if let Some(problem) = fresh {
            self.warn(format!("EditorConfig: {problem}"));
        }
    }

    /// Tab, in the document being edited: a tab at every selection, or
    /// spaces to each one's next indent stop, counted from where it starts.
    pub(super) fn type_tab(&mut self) {
        let id = self.doc().id;
        let size = match self.settings.whitespace.get(&id) {
            Some(whitespace) if whitespace.indent_style == Some(IndentStyle::Space) => {
                whitespace.indent_size.max(1)
            }
            _ => {
                self.doc_mut().buffer.insert("\t");
                return;
            }
        };
        let buffer = &mut self.doc_mut().buffer;
        let edits: Vec<Edit> = buffer
            .selections()
            .ranges()
            .iter()
            .map(|range| {
                let column = buffer.column_of(range.from());
                Edit::replace(range.from(), range.to(), " ".repeat(size - column % size))
            })
            .collect();
        buffer.edit(edits);
    }

    /// Before a document is written: trim trailing whitespace and end it with
    /// a line break, where its settings ask for either. One undoable edit.
    pub(super) fn tidy_before_save(&mut self, id: DocId) {
        let Some(whitespace) = self.settings.whitespace.get(&id) else { return };
        let (trim, final_newline) =
            (whitespace.trim_trailing_whitespace, whitespace.insert_final_newline);
        let Some(document) = self.docs.iter_mut().find(|doc| doc.id == id) else { return };
        let edits = tidy_edits(&document.buffer, trim, final_newline);
        if !edits.is_empty() {
            document.buffer.edit(edits);
            document.buffer.commit_undo_group();
        }
    }

    /// Ask about the project's settings file, if it is waiting on a decision
    /// not already asked for, and nothing else is being asked.
    fn ask_about_project(&mut self) {
        let Some(project) =
            self.settings.loaded.as_ref().and_then(|loaded| loaded.project.as_ref())
        else {
            return;
        };
        if !project.asks()
            || self.prompt.is_some()
            || self.settings.asked.as_ref() == Some(&project.fingerprint)
        {
            return;
        }
        self.settings.asked = Some(project.fingerprint.clone());
        self.prompt = Some(trust_prompt(project));
    }

    /// Ask again about the project's settings file, whatever was decided.
    pub(super) fn review_project(&mut self) -> Outcome {
        let project = self.settings.loaded.as_ref().and_then(|loaded| loaded.project.as_ref());
        match project {
            Some(project) => self.prompt = Some(trust_prompt(project)),
            None => {
                self.message =
                    Some(format!("This project has no {} to trust.", nun_config::PROJECT_FILE));
            }
        }
        Outcome::Redraw
    }

    /// The question about the project was answered.
    pub(super) fn trust_answered(&mut self, answer: Answer) -> Outcome {
        let Some(project) = self.settings.loaded.as_ref().and_then(|loaded| loaded.project.clone())
        else {
            return Outcome::Redraw;
        };
        let decision = match answer {
            Answer::Accept => Decision::Trust,
            Answer::Discard => Decision::Ignore,
            Answer::Confirm => {
                // Read it before deciding; the question is there again in
                // the palette, as "Review the project's settings".
                self.open_file(&project.file.path);
                return Outcome::Redraw;
            }
            Answer::Cancel => return Outcome::Redraw,
        };
        if let Some(reloader) = &self.settings.reloader {
            reloader.decide(project.dir, decision, project.fingerprint);
        } else {
            // No worker, as in the tests: decide in memory and resolve here.
            {
                let mut store = nun_config::TrustStore::in_memory();
                store.remember(project.dir.clone(), decision, project.fingerprint.clone());
                let files = nun_config::Files {
                    user: self.settings.loaded.as_ref().and_then(|loaded| loaded.user.clone()),
                    project: Some(project.file),
                };
                self.apply_settings(&nun_config::resolve(&files, &store));
            }
        }
        self.message = Some(match decision {
            Decision::Trust => "Trusted this project's settings.".to_string(),
            Decision::Ignore => "Ignoring this project's settings.".to_string(),
        });
        Outcome::Redraw
    }
}

/// The question about a project file: what it would change, and the buttons.
///
/// Enter views the file rather than trusting it, so a keystroke meant for
/// the text as the question appears cannot trust anything.
fn trust_prompt(project: &nun_config::Project) -> Prompt {
    // What runs a program or rewrites files first: on a narrow line, that is
    // what must not be cut off.
    let mut changes = project.would_change();
    changes.sort_by_key(|change| {
        let key = change.split(' ').next().unwrap_or_default();
        !nun_config::schema::find(key).is_some_and(|setting| setting.scope.needs_trust())
    });
    let what = if changes.is_empty() {
        "sets nothing yet".to_string()
    } else {
        format!("would set {}", changes.join(", "))
    };
    let lead = match project.trust {
        nun_config::Trust::Changed => "changed since you trusted it:",
        _ => "is not trusted:",
    };
    Prompt {
        purpose: Purpose::TrustProject,
        message: format!("{} {lead} it {what}.", nun_config::PROJECT_FILE),
        field: None,
        buttons: vec![
            ("View", Answer::Confirm),
            ("Trust", Answer::Accept),
            ("Ignore", Answer::Discard),
            ("Not now", Answer::Cancel),
        ],
        anchor: None,
    }
}

/// The edits that trim trailing spaces and tabs, and end the text with a
/// line break, as asked.
fn tidy_edits(buffer: &nun_core::Buffer, trim: bool, final_newline: bool) -> Vec<Edit> {
    let mut edits = Vec::new();
    let text = buffer.text();
    if trim {
        for line in 0..buffer.len_lines() {
            let end = buffer.line_end(line);
            let start = buffer.line_start(line);
            let mut from = end;
            while from > start && matches!(text.char(from - 1), ' ' | '\t') {
                from -= 1;
            }
            if from < end {
                edits.push(Edit::delete(from, end));
            }
        }
    }
    let len = buffer.len_chars();
    if final_newline && len > 0 && text.char(len - 1) != '\n' {
        edits.push(Edit::insert(len, "\n"));
    }
    edits
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use nun_config::{Files, Layer, TrustStore};
    use nun_theme::{Probe, Rgb, Role, derive};
    use nun_ui::{Event, UnderlineProbe};
    use ratatui::layout::Rect;

    use super::*;

    fn startup() -> Startup {
        Startup {
            palette: Probe::builtin_dark(),
            kitty_keyboard: Some(true),
            underlines: UnderlineProbe::new(),
        }
    }

    fn user(text: &str) -> nun_config::File {
        nun_config::File::parse(Path::new("/home/nun.toml"), Layer::User, text, None)
    }

    fn project(dir: &Path, text: &str) -> nun_config::File {
        nun_config::File::parse(&dir.join(".nun.toml"), Layer::Project, text, None)
    }

    /// An editor on `name` in a folder of its own, with `files` in force.
    fn editor(name: &str, text: &str, files: &Files) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        let (buffer, _) = nun_core::Buffer::load(&path).unwrap();
        let mut app = App::new(
            buffer,
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 100, 8));
        let loaded = nun_config::resolve(files, &TrustStore::in_memory());
        app.attach_settings(loaded, startup(), KeySet::Full, None);
        (app, dir)
    }

    fn settings(files: &Files) -> Event {
        Event::Config(News::Settings(Box::new(nun_config::resolve(
            files,
            &TrustStore::in_memory(),
        ))))
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn click(app: &mut App, column: u16, row: u16) {
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }

    #[test]
    fn saving_a_layer_reapplies_it_live_theme_and_keys_included() {
        let (mut app, _dir) = editor("a.txt", "x", &Files::default());
        let before = app.palette.ramp().get(Role::Accent);
        let files = Files {
            user: Some(user(
                "[editor]\ntab_width = 2\n[theme.roles]\naccent = \"#123456\"\n[keys]\n\"ctrl+k t\" = \"file.save\"\n",
            )),
            project: None,
        };
        assert_eq!(app.handle(settings(&files)), Outcome::Redraw);
        assert_eq!(app.doc().buffer.tab_width(), 2);
        assert_ne!(before, Rgb::new(0x12, 0x34, 0x56));
        assert_eq!(app.palette.ramp().get(Role::Accent), Rgb::new(0x12, 0x34, 0x56));
        let bound = app.keymap.sequences_for(&crate::commands::Command::Save);
        assert!(bound.iter().any(|keys| nun_input::Sequence(keys).to_string() == "Ctrl+K T"));
    }

    #[test]
    fn a_new_problem_is_said_once_with_its_line() {
        let (mut app, _dir) = editor("a.txt", "x", &Files::default());
        let broken = Files { user: Some(user("[ui]\nmouse = 3\n")), project: None };
        app.handle(settings(&broken));
        let said = app.shown_message().unwrap_or_default().to_string();
        assert!(said.contains("Config: nun.toml:2: ui.mouse"), "{said}");
        press(&mut app, KeyCode::Esc);
        app.handle(settings(&broken));
        assert_eq!(app.shown_message(), None, "said already");
    }

    #[test]
    fn a_setting_that_needs_a_restart_says_so() {
        let (mut app, _dir) = editor("a.txt", "x", &Files::default());
        app.handle(settings(&Files { user: Some(user("[ui]\nmouse = false\n")), project: None }));
        let said = app.shown_message().unwrap_or_default().to_string();
        assert!(said.contains("next time nun starts"), "{said}");
    }

    #[test]
    fn undercurl_goes_to_the_screen_once() {
        let (mut app, _dir) = editor("a.txt", "x", &Files::default());
        assert_eq!(app.take_underlines(), None);
        app.handle(settings(&Files {
            user: Some(user("[ui]\nundercurl = \"on\"\n")),
            project: None,
        }));
        assert_eq!(app.take_underlines(), Some(Underlines::FULL));
        assert_eq!(app.take_underlines(), None);
    }

    #[test]
    fn an_untrusted_project_is_asked_about_and_trusted_with_a_click() {
        let dir = tempfile::tempdir().unwrap();
        let files = Files {
            user: None,
            project: Some(project(
                dir.path(),
                "[editor]\nindent_style = \"space\"\nindent_size = 2\n",
            )),
        };
        let (mut app, _file_dir) = editor("a.rs", "", &files);
        let prompt = app.prompt.as_ref().expect("asked on first sight");
        assert!(prompt.message.contains("indent_size = 2"), "{}", prompt.message);

        press(&mut app, KeyCode::Char('x'));
        assert!(app.prompt.is_some(), "a stray key answers nothing");
        assert_eq!(app.doc().buffer.text().to_string(), "", "and types nothing");

        // Inert until then: Tab types a tab.
        let status = Rect::new(0, 7, 100, 1);
        let trust = app.prompt.as_ref().unwrap().button_areas(status)[1];
        click(&mut app, trust.x + 1, trust.y);
        assert!(app.prompt.is_none());
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.doc().buffer.text().to_string(), "  ", "trusted: two spaces");
    }

    #[test]
    fn a_project_is_ignored_from_the_keyboard_and_asked_once() {
        let dir = tempfile::tempdir().unwrap();
        let files =
            Files { user: None, project: Some(project(dir.path(), "[editor]\ntab_width = 2\n")) };
        let (mut app, _file_dir) = editor("a.rs", "", &files);
        press(&mut app, KeyCode::Char('i'));
        assert!(app.prompt.is_none());
        assert_eq!(app.doc().buffer.tab_width(), 4, "ignored");
        app.handle(settings(&files));
        assert!(app.prompt.is_none(), "not asked again for the same file");
        app.run(crate::commands::Command::ReviewProjectSettings);
        assert!(app.prompt.is_some(), "until asked for");
    }

    #[test]
    fn editorconfig_sets_the_indent_and_the_save_tidies() {
        let (mut app, dir) = editor("a.py", "x = 1   ", &Files::default());
        let path = app.doc().buffer.path().unwrap().to_path_buf();
        let configs = vec![EditorConfig::parse(
            &dir.path().join(".editorconfig"),
            "[*.py]\nindent_style = space\nindent_size = 4\ntrim_trailing_whitespace = true\ninsert_final_newline = true\nend_of_line = crlf\n",
        )];
        app.handle(Event::Config(News::EditorConfig { path: path.clone(), configs }));
        assert_eq!(app.doc().buffer.line_ending(), LineEnding::Crlf);

        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Tab);
        assert!(app.doc().buffer.text().to_string().ends_with("    "));
        app.run(crate::commands::Command::Save);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x = 1\r\n");
    }

    #[test]
    fn spaces_go_to_the_next_indent_stop() {
        let (mut app, dir) = editor("a.py", "ab", &Files::default());
        let path = app.doc().buffer.path().unwrap().to_path_buf();
        let configs = vec![EditorConfig::parse(
            &dir.path().join(".editorconfig"),
            "[*]\nindent_style = space\nindent_size = 4\n",
        )];
        app.handle(Event::Config(News::EditorConfig { path, configs }));
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.doc().buffer.text().to_string(), "ab  ");
    }

    #[test]
    fn tidying_trims_line_ends_and_adds_the_last_break() {
        let buffer = nun_core::Buffer::from_text("a  \n\tb\t\nc \u{3000}  ");
        let mut tidied = nun_core::Buffer::from_text(&buffer.text().to_string());
        tidied.edit(tidy_edits(&buffer, true, true));
        assert_eq!(tidied.text().to_string(), "a\n\tb\nc \u{3000}\n", "only spaces and tabs");

        let clean = nun_core::Buffer::from_text("a\nb\n");
        assert!(tidy_edits(&clean, true, true).is_empty());
        assert!(tidy_edits(&nun_core::Buffer::from_text(""), true, true).is_empty());
        assert!(tidy_edits(&buffer, false, false).is_empty());
    }

    #[test]
    fn the_prompt_says_what_the_project_would_change() {
        let file = nun_config::File::parse(
            Path::new("/p/.nun.toml"),
            nun_config::Layer::Project,
            "[lsp.rust]\ncommand = \"./x\"\n",
            None,
        );
        let files = nun_config::Files { user: None, project: Some(file) };
        let loaded = nun_config::resolve(&files, &nun_config::TrustStore::in_memory());
        let prompt = trust_prompt(loaded.project.as_ref().unwrap());
        assert!(prompt.message.contains("lsp.rust.command = \"./x\""), "{}", prompt.message);
        assert_eq!(prompt.buttons[0], ("View", Answer::Confirm), "Enter only views it");
    }

    fn spaces(app: &mut App, dir: &Path, size: usize) {
        let path = app.doc().buffer.path().unwrap().to_path_buf();
        let text = format!("[*]\nindent_style = space\nindent_size = {size}\n");
        let configs = vec![EditorConfig::parse(&dir.join(".editorconfig"), &text)];
        app.handle(Event::Config(News::EditorConfig { path, configs }));
    }

    #[test]
    fn each_caret_goes_to_its_own_stop_by_display_width() {
        let (mut app, dir) = editor("a.txt", "a\nabc\n中\n", &Files::default());
        spaces(&mut app, dir.path(), 4);
        let buffer = &mut app.doc_mut().buffer;
        buffer.set_selections(nun_core::Selections::new(
            vec![nun_core::Range::caret(1), nun_core::Range::caret(5), nun_core::Range::caret(7)],
            0,
        ));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.doc().buffer.text().to_string(), "a   \nabc \n中  \n");
    }

    #[test]
    fn tab_over_a_selection_counts_from_its_start() {
        let (mut app, dir) = editor("a.txt", "abcdef", &Files::default());
        spaces(&mut app, dir.path(), 4);
        let buffer = &mut app.doc_mut().buffer;
        buffer.set_selections(nun_core::Selections::new(vec![nun_core::Range::new(1, 3)], 0));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.doc().buffer.text().to_string(), "a   def");
    }

    #[test]
    fn taking_a_line_ending_away_gives_the_file_its_own_back() {
        let (mut app, dir) = editor("a.txt", "a\r\nb\r\n", &Files::default());
        let path = app.doc().buffer.path().unwrap().to_path_buf();
        let set =
            vec![EditorConfig::parse(&dir.path().join(".editorconfig"), "[*]\nend_of_line = lf\n")];
        app.handle(Event::Config(News::EditorConfig { path: path.clone(), configs: set }));
        assert_eq!(app.doc().buffer.line_ending(), LineEnding::Lf);
        app.handle(Event::Config(News::EditorConfig { path, configs: Vec::new() }));
        assert_eq!(app.doc().buffer.line_ending(), LineEnding::Crlf);
    }
}
