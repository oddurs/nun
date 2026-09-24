//! One `nun.toml`, read into checked settings.
//!
//! The file is walked by hand rather than deserialised, so that each value is
//! judged on its own: a bad one is reported with its line and left out, and
//! every good one around it still applies. A file that is not TOML at all
//! keeps the settings it had the last time it read, so a half-typed edit
//! costs nothing until it is finished.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use toml::de::{DeTable, DeValue};

use crate::schema::{self, Kind, Value};
use crate::{Layer, Problem};

/// One setting as a file gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What it is set to.
    pub value: Value,
    /// The line it is set on, from 1.
    pub line: usize,
}

/// A configuration file, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Where it is.
    pub path: PathBuf,
    /// Which layer it is.
    pub layer: Layer,
    /// Every good setting in it, by dotted key.
    pub entries: BTreeMap<String, Entry>,
    /// Everything in it that could not be used.
    pub problems: Vec<Problem>,
}

impl File {
    /// Read the file at `path`, or `None` when there is none — which is not a
    /// problem: zero config is the expected case.
    ///
    /// `previous` is what the same file said the last time it was read; its
    /// values stand in for any that are broken now.
    #[must_use]
    pub fn read(path: &Path, layer: Layer, previous: Option<&Self>) -> Option<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Some(Self::parse(path, layer, &text, previous)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                let mut file = Self::empty(path, layer);
                if let Some(previous) = previous {
                    file.entries.clone_from(&previous.entries);
                }
                file.problems.push(Problem {
                    path: path.to_path_buf(),
                    line: None,
                    message: format!("could not be read: {error}"),
                });
                Some(file)
            }
        }
    }

    fn empty(path: &Path, layer: Layer) -> Self {
        Self { path: path.to_path_buf(), layer, entries: BTreeMap::new(), problems: Vec::new() }
    }

    /// Read `text` as the file at `path`.
    #[must_use]
    pub fn parse(path: &Path, layer: Layer, text: &str, previous: Option<&Self>) -> Self {
        let mut file = Self::empty(path, layer);
        let table = match DeTable::parse(text) {
            Ok(table) => table,
            Err(error) => {
                let kept = previous.is_some_and(|previous| !previous.entries.is_empty());
                if let Some(previous) = previous {
                    file.entries.clone_from(&previous.entries);
                }
                let tail =
                    if kept { "; its settings stay as they were until it reads" } else { "" };
                file.problems.push(Problem {
                    path: path.to_path_buf(),
                    line: error.span().map(|span| line_of(text, span.start)),
                    message: format!("{}{tail}", error.message().trim_end()),
                });
                return file;
            }
        };

        let mut broken = Vec::new();
        let mut walk = Walk { text, file: &mut file, broken: &mut broken };
        walk.table(table.get_ref(), &mut Vec::new());

        // A value that is wrong now keeps the one it had, rather than falling
        // back a layer: a typo in `tab_width = 2` should not flip the file to
        // eight columns while it is being fixed.
        if let Some(previous) = previous {
            for (key, at) in broken {
                if let Some(entry) = previous.entries.get(&key) {
                    file.entries.insert(key.clone(), entry.clone());
                    if let Some(problem) = file.problems.get_mut(at) {
                        let _ = write!(problem.message, "; keeping {}", entry.value);
                    }
                }
            }
        }
        // The parser hands tables back sorted by key; a person reads by line.
        file.problems.sort_by_key(|problem| problem.line);
        file
    }

    /// Where a key is set in this file, and to what.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key)
    }
}

/// The line, from 1, that byte `offset` of `text` is on.
pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    let offset = offset.min(text.len());
    // By bytes, so an offset inside a character cannot panic.
    text.as_bytes()[..offset].split(|byte| *byte == b'\n').count()
}

struct Walk<'a> {
    text: &'a str,
    file: &'a mut File,
    /// Keys whose value is wrong, and which problem says so.
    broken: &'a mut Vec<(String, usize)>,
}

impl Walk<'_> {
    fn problem(&mut self, line: usize, message: String) -> usize {
        self.file.problems.push(Problem {
            path: self.file.path.clone(),
            line: Some(line),
            message,
        });
        self.file.problems.len() - 1
    }

    fn table(&mut self, table: &DeTable<'_>, path: &mut Vec<String>) {
        for (key, value) in table {
            path.push(key.get_ref().to_string());
            let line = line_of(self.text, key.span().start);
            self.value(value.get_ref(), line, path);
            path.pop();
        }
    }

    fn value(&mut self, value: &DeValue<'_>, line: usize, path: &mut Vec<String>) {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        let key = parts.join(".");
        if let DeValue::Table(inner) = value
            && schema::is_section(&parts)
        {
            self.table(inner, path);
            return;
        }
        let Some(setting) = schema::lookup(&parts) else {
            let hint = schema::nearest(&parts)
                .map_or_else(String::new, |near| format!("; did you mean `{near}`?"));
            self.problem(line, format!("no setting called `{key}`{hint}"));
            return;
        };
        if self.file.layer == Layer::Project && !setting.scope.project_may_set() {
            self.problem(
                line,
                format!("`{key}` is a personal setting; only your own nun.toml can change it"),
            );
            return;
        }
        match check(setting.kind, value) {
            Ok(checked) => {
                if self.file.entries.contains_key(&key) {
                    self.problem(line, format!("{key} is set twice; the second is used"));
                }
                self.file.entries.insert(key, Entry { value: checked, line });
            }
            Err(why) => {
                let at = self.problem(line, format!("{key} {why}"));
                self.broken.push((key, at));
            }
        }
    }
}

/// `value` as a `kind`, or why it is not one.
fn check(kind: Kind, value: &DeValue<'_>) -> Result<Value, String> {
    let wrong = || format!("must be {}, not {}", kind.expected(), describe(value));
    match (kind, value) {
        (Kind::Bool, DeValue::Boolean(on)) => Ok(Value::Bool(*on)),
        (Kind::Int { min, max }, DeValue::Integer(number)) => {
            let parsed = i64::from_str_radix(number.as_str(), number.radix()).ok();
            match parsed.and_then(|number| u64::try_from(number).ok()) {
                Some(number) if (min..=max).contains(&number) => Ok(Value::Int(number)),
                _ => Err(format!("must be between {min} and {max}, not {}", number.as_str())),
            }
        }
        (Kind::OneOf(words), DeValue::String(word)) => {
            if words.contains(&word.as_ref()) {
                Ok(Value::Text(word.to_string()))
            } else {
                Err(wrong())
            }
        }
        (Kind::Text, DeValue::String(text)) => Ok(Value::Text(text.to_string())),
        (Kind::Command, DeValue::String(command)) => {
            if command.trim().is_empty() {
                Err("is empty; use `enabled = false` to turn the server off".to_string())
            } else {
                Ok(Value::Text(command.to_string()))
            }
        }
        (Kind::List, DeValue::Array(items)) => items
            .iter()
            .map(|item| item.get_ref().as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .map(Value::List)
            .ok_or_else(wrong),
        _ => Err(wrong()),
    }
}

/// A value, as a message names it.
fn describe(value: &DeValue<'_>) -> String {
    match value {
        DeValue::String(text) => format!("the string {:?}", text.as_ref()),
        DeValue::Integer(number) => format!("the number {}", number.as_str()),
        DeValue::Boolean(on) => format!("{on}"),
        DeValue::Array(_) => "an array".to_string(),
        other => format!("a {}", other.type_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> File {
        File::parse(Path::new("nun.toml"), Layer::User, text, None)
    }

    #[test]
    fn good_values_are_kept_with_their_lines() {
        let file = parse("[editor]\ntab_width = 2\n\n[ui]\nmouse = false\n");
        assert_eq!(file.problems, []);
        assert_eq!(file.get("editor.tab_width"), Some(&Entry { value: Value::Int(2), line: 2 }));
        assert_eq!(file.get("ui.mouse").unwrap().line, 5);
    }

    #[test]
    fn a_bad_value_names_its_line_and_the_rest_still_applies() {
        let file = parse("[editor]\ntab_width = 0\n\n[ui]\nmouse = \"yes\"\nhyperlinks = false\n");
        assert_eq!(file.problems.len(), 2, "{:?}", file.problems);
        assert_eq!(file.problems[0].line, Some(2));
        assert!(
            file.problems[0].message.contains("between 1 and 16, not 0"),
            "{:?}",
            file.problems
        );
        assert_eq!(file.problems[1].line, Some(5));
        assert!(file.problems[1].message.contains("true or false"), "{:?}", file.problems);
        assert_eq!(file.get("ui.hyperlinks").unwrap().value, Value::Bool(false));
        assert!(file.get("editor.tab_width").is_none());
    }

    #[test]
    fn a_bad_value_keeps_the_one_it_had() {
        let before = parse("[editor]\ntab_width = 2\n");
        let after = File::parse(
            Path::new("nun.toml"),
            Layer::User,
            "[editor]\ntab_width = 99\n",
            Some(&before),
        );
        assert_eq!(after.get("editor.tab_width").unwrap().value, Value::Int(2));
        assert!(after.problems[0].message.ends_with("keeping 2"), "{:?}", after.problems);
    }

    #[test]
    fn a_file_that_is_not_toml_keeps_everything_it_had() {
        let before = parse("[editor]\ntab_width = 2\n[ui]\nmouse = false\n");
        let broken = "[editor]\ntab_width = 2\n[ui\nmouse = false\n";
        let after = File::parse(Path::new("nun.toml"), Layer::User, broken, Some(&before));
        assert_eq!(after.entries, before.entries);
        assert_eq!(after.problems.len(), 1);
        assert_eq!(after.problems[0].line, Some(3), "{:?}", after.problems);
        assert!(after.problems[0].message.contains("stay as they were"), "{:?}", after.problems);

        let fresh = parse(broken);
        assert!(fresh.entries.is_empty(), "with nothing before, nothing");
    }

    #[test]
    fn unknown_keys_are_reported_with_a_suggestion() {
        let file = parse("[editor]\ntab_widht = 2\n\n[nonsense]\nx = 1\n");
        assert_eq!(file.problems.len(), 2, "{:?}", file.problems);
        assert!(file.problems[0].message.contains("did you mean `editor.tab_width`?"));
        assert_eq!(file.problems[0].line, Some(2));
        assert!(file.problems[1].message.contains("`nonsense`"), "{:?}", file.problems);
    }

    #[test]
    fn glyphs_are_read_however_they_are_spelled() {
        let file =
            parse("[glyphs]\npreset = \"ascii\"\nfold.open = \"v\"\n[glyphs.tab]\nclose = \"x\"\n");
        assert_eq!(file.problems, []);
        assert_eq!(file.get("glyphs.preset").unwrap().value, Value::Text("ascii".into()));
        assert_eq!(file.get("glyphs.fold.open").unwrap().value, Value::Text("v".into()));
        assert_eq!(file.get("glyphs.tab.close").unwrap().line, 5);

        let twice = parse("[glyphs]\nfold.open = \"v\"\n\"fold.open\" = \"w\"\n");
        assert!(twice.problems[0].message.contains("set twice"), "{:?}", twice.problems);
        assert_eq!(twice.get("glyphs.fold.open").unwrap().value, Value::Text("w".into()));

        let number = parse("[glyphs]\nlightbulb = 3\n");
        assert!(number.problems[0].message.contains("must be a string"), "{:?}", number.problems);
    }

    #[test]
    fn lsp_settings_are_checked() {
        let file = parse(
            "[lsp.rust]\ncommand = \"\"\n[lsp.go]\nargs = [\"serve\", 3]\n[lsp.zig]\ncommand = \"zls\"\n",
        );
        assert_eq!(file.problems.len(), 2, "{:?}", file.problems);
        assert!(file.problems[0].message.contains("enabled = false"), "{:?}", file.problems);
        assert!(file.problems[1].message.contains("list of strings"), "{:?}", file.problems);
        assert_eq!(file.get("lsp.zig.command").unwrap().value, Value::Text("zls".into()));
    }

    #[test]
    fn a_project_cannot_touch_personal_settings() {
        let file = File::parse(
            Path::new(".nun.toml"),
            Layer::Project,
            "[ui]\nmouse = false\n[editor]\ntab_width = 2\n",
            None,
        );
        assert_eq!(file.problems.len(), 1);
        assert!(file.problems[0].message.contains("personal setting"), "{:?}", file.problems);
        assert!(file.get("ui.mouse").is_none());
        assert!(file.get("editor.tab_width").is_some());
    }

    #[test]
    fn lines_count_from_one() {
        assert_eq!(line_of("a\nb\nc", 0), 1);
        assert_eq!(line_of("a\nb\nc", 2), 2);
        assert_eq!(line_of("a\nb\nc", 99), 3);
    }
}
