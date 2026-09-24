//! How one file's whitespace is written: the layers, file by file.
//!
//! The `[editor]` settings are the one place the four layers interleave per
//! file. Each key is resolved on its own, and each layer overrides the one
//! before: nun's default, the person's `nun.toml`, the `.editorconfig`
//! sections that match the file, then a trusted project's `.nun.toml`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::editorconfig::{self, EditorConfig, Property};
use crate::schema::Value;
use crate::{Layer, Loaded, Origin, Problem};

/// What Tab types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentStyle {
    /// A tab character.
    Tab,
    /// Spaces, to the next indent stop.
    Space,
}

/// The line ending a file is saved with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndOfLine {
    /// `\n`.
    Lf,
    /// `\r\n`.
    Crlf,
}

/// How a file is encoded when it is saved. nun only writes UTF-8; the choice
/// is whether it begins with a byte-order mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// UTF-8, no mark.
    Utf8,
    /// UTF-8 behind a byte-order mark.
    Utf8Bom,
}

/// The whitespace settings for one file, and where each came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Whitespace {
    /// What Tab types; `None` types a tab, as nun always has.
    pub indent_style: Option<IndentStyle>,
    /// Columns one indent takes when indenting with spaces.
    pub indent_size: usize,
    /// Columns a tab advances to.
    pub tab_width: usize,
    /// The line ending to save with; `None` keeps the file's own.
    pub end_of_line: Option<EndOfLine>,
    /// Whether to save with a byte-order mark; `None` keeps the file's own.
    pub charset: Option<Charset>,
    /// Strip spaces and tabs from line ends on save.
    pub trim_trailing_whitespace: bool,
    /// End the file with a line break on save.
    pub insert_final_newline: bool,
    /// Where each setting that is not a default came from, by key.
    pub origins: BTreeMap<String, Origin>,
    /// What in the `.editorconfig` files could not be used for this file.
    pub problems: Vec<Problem>,
}

/// The keys this covers, in the order they are shown.
pub const KEYS: &[&str] = &[
    "editor.indent_style",
    "editor.indent_size",
    "editor.tab_width",
    "editor.end_of_line",
    "editor.charset",
    "editor.trim_trailing_whitespace",
    "editor.insert_final_newline",
];

/// A value one layer gives a key, and whether it is the one used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Where it is said.
    pub origin: Origin,
    /// What it says, as the file writes it.
    pub value: String,
    /// Why it is not used, when that is not only because a later layer said
    /// something else.
    pub ignored: Option<String>,
}

impl Whitespace {
    /// The whitespace for `file`, given the settings and the `.editorconfig`
    /// files that bear on it, nearest first.
    #[must_use]
    pub fn resolve(loaded: &Loaded, file: &Path, configs: &[EditorConfig]) -> Self {
        let chains = chains(loaded, file, configs);
        let used = |key: &str| {
            chains.get(key).and_then(|steps| steps.iter().rev().find(|step| step.ignored.is_none()))
        };
        let text = |key: &str| used(key).map(|step| step.value.as_str());
        let number = |key: &str| text(key).and_then(|value| value.parse::<usize>().ok());
        let flag = |key: &str| text(key) == Some("true");

        let tab_width = number("editor.tab_width").unwrap_or(crate::Config::default().tab_width);
        let indent_size = number("editor.indent_size").unwrap_or(tab_width);
        let mut origins = BTreeMap::new();
        for key in KEYS {
            if let Some(step) = used(key)
                && step.origin != Origin::Default
            {
                origins.insert((*key).to_string(), step.origin.clone());
            }
        }
        let problems = configs
            .iter()
            .flat_map(|config| config.problems.clone())
            .chain(chains.values().flatten().filter_map(|step| {
                let (Origin::File { layer: Layer::EditorConfig, path, line }, Some(why)) =
                    (&step.origin, &step.ignored)
                else {
                    return None;
                };
                Some(Problem { path: path.clone(), line: Some(*line), message: why.clone() })
            }))
            .collect();

        Self {
            indent_style: match text("editor.indent_style") {
                Some("tab") => Some(IndentStyle::Tab),
                Some("space") => Some(IndentStyle::Space),
                _ => None,
            },
            indent_size,
            tab_width,
            end_of_line: match text("editor.end_of_line") {
                Some("lf") => Some(EndOfLine::Lf),
                Some("crlf") => Some(EndOfLine::Crlf),
                _ => None,
            },
            charset: match text("editor.charset") {
                Some("utf-8") => Some(Charset::Utf8),
                Some("utf-8-bom") => Some(Charset::Utf8Bom),
                _ => None,
            },
            trim_trailing_whitespace: flag("editor.trim_trailing_whitespace"),
            insert_final_newline: flag("editor.insert_final_newline"),
            origins,
            problems,
        }
    }

    /// Where a setting came from.
    #[must_use]
    pub fn origin(&self, key: &str) -> Origin {
        self.origins.get(key).cloned().unwrap_or(Origin::Default)
    }
}

/// Every layer's say on every whitespace key for `file`, in order.
#[must_use]
pub fn chains(
    loaded: &Loaded,
    file: &Path,
    configs: &[EditorConfig],
) -> BTreeMap<String, Vec<Step>> {
    let found = editorconfig::properties(file, configs);
    let from_editorconfig = translate(&found);
    let mut chains = BTreeMap::new();
    for key in KEYS {
        let mut steps = Vec::new();
        if *key == "editor.tab_width" {
            steps.push(Step {
                origin: Origin::Default,
                value: crate::Config::default().tab_width.to_string(),
                ignored: None,
            });
        }
        steps.extend(loaded.file_steps(key, Layer::User));
        steps.extend(from_editorconfig.get(*key).cloned());
        steps.extend(loaded.file_steps(key, Layer::Project));
        chains.insert((*key).to_string(), steps);
    }
    chains
}

/// What the `.editorconfig` properties say, as `[editor]` keys.
fn translate(found: &BTreeMap<String, Property>) -> BTreeMap<&'static str, Step> {
    let step = |property: &Property, value: &str, ignored: Option<String>| Step {
        origin: Origin::File {
            layer: Layer::EditorConfig,
            path: property.path.clone(),
            line: property.line,
        },
        value: value.to_string(),
        ignored,
    };
    let bad = |name: &str, property: &Property, expected: &str| {
        Some(format!("{name} = {} is not {expected}", property.value))
    };
    let mut out = BTreeMap::new();

    if let Some(property) = found.get("indent_style") {
        let ignored = match property.value.as_str() {
            "tab" | "space" => None,
            _ => bad("indent_style", property, "tab or space"),
        };
        out.insert("editor.indent_style", step(property, &property.value, ignored));
    }
    if let Some(property) = found.get("tab_width") {
        let ignored = width(&property.value)
            .is_none()
            .then(|| format!("tab_width = {} is not a whole number from 1 to 16", property.value));
        out.insert("editor.tab_width", step(property, &property.value, ignored));
    }
    if let Some(property) = found.get("indent_size") {
        match (property.value.as_str(), width(&property.value)) {
            // The tab width, whichever layer ends up saying what that is.
            ("tab", _) => {}
            (_, Some(size)) => {
                out.insert("editor.indent_size", step(property, &property.value, None));
                // The spec: tab_width defaults to indent_size.
                if !found.contains_key("tab_width") {
                    out.insert("editor.tab_width", step(property, &size.to_string(), None));
                }
            }
            _ => {
                let ignored = bad("indent_size", property, "tab or a whole number from 1 to 16");
                out.insert("editor.indent_size", step(property, &property.value, ignored));
            }
        }
    }
    if let Some(property) = found.get("end_of_line") {
        let ignored = match property.value.as_str() {
            "lf" | "crlf" => None,
            "cr" => Some("end_of_line = cr: nun cannot write CR-only line endings".to_string()),
            _ => bad("end_of_line", property, "lf, crlf or cr"),
        };
        out.insert("editor.end_of_line", step(property, &property.value, ignored));
    }
    if let Some(property) = found.get("charset") {
        let ignored = match property.value.as_str() {
            "utf-8" | "utf-8-bom" => None,
            "latin1" | "utf-16be" | "utf-16le" => {
                Some(format!("charset = {}: nun reads and writes UTF-8 only", property.value))
            }
            _ => bad("charset", property, "a charset EditorConfig names"),
        };
        out.insert("editor.charset", step(property, &property.value, ignored));
    }
    for name in ["trim_trailing_whitespace", "insert_final_newline"] {
        if let Some(property) = found.get(name) {
            let ignored = match property.value.as_str() {
                "true" | "false" => None,
                _ => bad(name, property, "true or false"),
            };
            let key = if name == "trim_trailing_whitespace" {
                "editor.trim_trailing_whitespace"
            } else {
                "editor.insert_final_newline"
            };
            out.insert(key, step(property, &property.value, ignored));
        }
    }
    out
}

fn width(value: &str) -> Option<usize> {
    value.parse::<usize>().ok().filter(|width| (1..=16).contains(width))
}

impl Loaded {
    /// What the file at `layer` says about `key`, as a step in its chain.
    pub(crate) fn file_steps(&self, key: &str, layer: Layer) -> Option<Step> {
        let (file, ignored) = match layer {
            Layer::User => (self.user.as_ref()?, None),
            Layer::Project => {
                let project = self.project.as_ref()?;
                (&project.file, project.withheld(key))
            }
            Layer::EditorConfig => return None,
        };
        let entry = file.get(key)?;
        let value = match &entry.value {
            Value::Text(text) => text.clone(),
            value => value.to_string(),
        };
        Some(Step {
            origin: Origin::File { layer, path: file.path.clone(), line: entry.line },
            value,
            ignored,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::trust::{Decision, TrustStore};
    use crate::{Files, file::File};

    fn user(text: &str) -> File {
        File::parse(Path::new("/home/nun.toml"), Layer::User, text, None)
    }

    fn project(text: &str) -> File {
        File::parse(Path::new("/p/.nun.toml"), Layer::Project, text, None)
    }

    fn trusted(files: &Files) -> TrustStore {
        let mut store = TrustStore::in_memory();
        if let Some(file) = &files.project {
            store.remember(PathBuf::from("/p"), Decision::Trust, crate::trust::fingerprint(file));
        }
        store
    }

    fn editorconfig(text: &str) -> Vec<EditorConfig> {
        vec![EditorConfig::parse(Path::new("/p/.editorconfig"), text)]
    }

    #[test]
    fn with_nothing_said_it_is_what_nun_always_did() {
        let loaded = crate::resolve(&Files::default(), &TrustStore::in_memory());
        let whitespace = Whitespace::resolve(&loaded, Path::new("/p/a.rs"), &[]);
        assert_eq!(whitespace.indent_style, None);
        assert_eq!(whitespace.tab_width, 4);
        assert_eq!(whitespace.indent_size, 4);
        assert_eq!(whitespace.end_of_line, None);
        assert!(!whitespace.trim_trailing_whitespace);
        assert!(whitespace.origins.is_empty());
    }

    #[test]
    fn each_layer_overrides_the_last() {
        let files = Files {
            user: Some(user(
                "[editor]\ntab_width = 8\nindent_style = \"tab\"\nend_of_line = \"lf\"\n",
            )),
            project: Some(project("[editor]\nend_of_line = \"crlf\"\n")),
        };
        let loaded = crate::resolve(&files, &trusted(&files));
        let configs = editorconfig("[*.rs]\nindent_style = space\nindent_size = 2\n");
        let whitespace = Whitespace::resolve(&loaded, Path::new("/p/src/a.rs"), &configs);

        assert_eq!(whitespace.indent_style, Some(IndentStyle::Space), "editorconfig over user");
        assert_eq!(whitespace.indent_size, 2);
        assert_eq!(whitespace.tab_width, 2, "tab_width follows indent_size, over the user's 8");
        assert_eq!(whitespace.end_of_line, Some(EndOfLine::Crlf), "project over user");
        assert!(matches!(
            whitespace.origin("editor.indent_style"),
            Origin::File { layer: Layer::EditorConfig, line: 2, .. }
        ));
        assert!(matches!(
            whitespace.origin("editor.end_of_line"),
            Origin::File { layer: Layer::Project, .. }
        ));

        let markdown = Whitespace::resolve(&loaded, Path::new("/p/README.md"), &configs);
        assert_eq!(markdown.indent_style, Some(IndentStyle::Tab), "the section is only for .rs");
        assert_eq!(markdown.tab_width, 8);
    }

    #[test]
    fn an_untrusted_project_says_nothing() {
        let files = Files { user: None, project: Some(project("[editor]\ntab_width = 2\n")) };
        let loaded = crate::resolve(&files, &TrustStore::in_memory());
        let whitespace = Whitespace::resolve(&loaded, Path::new("/p/a.rs"), &[]);
        assert_eq!(whitespace.tab_width, 4);
    }

    #[test]
    fn indent_size_tab_means_the_tab_width() {
        let loaded = crate::resolve(&Files::default(), &TrustStore::in_memory());
        let configs = editorconfig("[*]\nindent_style = tab\nindent_size = tab\ntab_width = 3\n");
        let whitespace = Whitespace::resolve(&loaded, Path::new("/p/a.c"), &configs);
        assert_eq!((whitespace.indent_size, whitespace.tab_width), (3, 3));
    }

    #[test]
    fn what_nun_cannot_do_is_reported_with_its_line_and_ignored() {
        let loaded = crate::resolve(&Files::default(), &TrustStore::in_memory());
        let configs = editorconfig(
            "[*]\nend_of_line = cr\ncharset = latin1\nindent_size = huge\ntrim_trailing_whitespace = true\n",
        );
        let whitespace = Whitespace::resolve(&loaded, Path::new("/p/a.c"), &configs);
        assert_eq!(whitespace.end_of_line, None);
        assert_eq!(whitespace.charset, None);
        assert_eq!(whitespace.indent_size, 4);
        assert!(whitespace.trim_trailing_whitespace, "the rest still applies");
        let lines: Vec<_> = whitespace.problems.iter().map(|problem| problem.line).collect();
        assert_eq!(lines.len(), 3, "{:?}", whitespace.problems);
        assert!(lines.contains(&Some(2)) && lines.contains(&Some(3)) && lines.contains(&Some(4)));
    }
}
