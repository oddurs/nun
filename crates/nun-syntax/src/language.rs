//! The languages nun knows, and how it recognises them.
//!
//! Grammars are compiled in rather than loaded at runtime. A plugin runtime is
//! out of scope for v0.1, and compiling them in means a grammar cannot be
//! half-installed, cannot disagree with the query files beside it, and cannot
//! be a reason the editor will not start.

use std::sync::OnceLock;

use tree_sitter::{Language as Grammar, Query};

/// One language: its grammar, and the queries nun runs against it.
#[derive(Debug)]
pub struct Language {
    /// What it is called, as the palette and the status line say it.
    pub name: &'static str,
    /// The grammar itself.
    pub grammar: Grammar,
    /// Which nodes get which capture names.
    pub highlights: Query,
    /// Where other languages are embedded, if anywhere.
    pub injections: Option<Query>,
    /// What counts as a definition worth listing in an outline, if the
    /// grammar has an opinion.
    pub symbols: Option<Query>,
}

impl Language {
    /// Every capture name this language's highlight query can produce, in
    /// capture-index order.
    #[must_use]
    pub fn capture_names(&self) -> &[&str] {
        self.highlights.capture_names()
    }
}

/// The language for a file name, by extension.
///
/// By name rather than by content: a file being edited may be empty, or may
/// not be what its first line claims yet.
#[must_use]
pub fn of_path(path: &std::path::Path) -> Option<&'static Language> {
    let name = path.file_name()?.to_str()?;
    let extension = path.extension().and_then(|extension| extension.to_str()).unwrap_or("");
    let wanted = match (name, extension) {
        (_, "rs") => "rust",
        (_, "json") => "json",
        // Cargo.lock is TOML whatever it is called.
        ("Cargo.lock", _) | (_, "toml") => "toml",
        (_, "js" | "mjs" | "cjs" | "jsx") => "javascript",
        (_, "py" | "pyi") => "python",
        (_, "html" | "htm") => "html",
        (_, "css") => "css",
        (_, "sql") => "sql",
        _ => return None,
    };
    of_name(wanted)
}

/// The language called `name`, if nun has it.
#[must_use]
pub fn of_name(name: &str) -> Option<&'static Language> {
    // Injections name languages in their own vocabulary, so the aliases live
    // here rather than in each query.
    let name = match name {
        "rs" => "rust",
        "jsx" => "javascript",
        "py" => "python",
        "postgresql" | "mysql" | "sqlite" => "sql",
        other => other,
    };
    all().iter().find(|language| language.name == name)
}

/// Every language, built once.
#[must_use]
pub fn all() -> &'static [Language] {
    static ALL: OnceLock<Vec<Language>> = OnceLock::new();
    ALL.get_or_init(|| {
        [
            build(
                "rust",
                &tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                Some(tree_sitter_rust::INJECTIONS_QUERY),
                Some(tree_sitter_rust::TAGS_QUERY),
            ),
            build(
                "json",
                &tree_sitter_json::LANGUAGE.into(),
                tree_sitter_json::HIGHLIGHTS_QUERY,
                None,
                None,
            ),
            build(
                "toml",
                &tree_sitter_toml_ng::LANGUAGE.into(),
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
                None,
                None,
            ),
            build(
                "javascript",
                &tree_sitter_javascript::LANGUAGE.into(),
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                Some(tree_sitter_javascript::INJECTIONS_QUERY),
                Some(tree_sitter_javascript::TAGS_QUERY),
            ),
            build(
                "python",
                &tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::HIGHLIGHTS_QUERY,
                None,
                Some(tree_sitter_python::TAGS_QUERY),
            ),
            build(
                "html",
                &tree_sitter_html::LANGUAGE.into(),
                tree_sitter_html::HIGHLIGHTS_QUERY,
                Some(tree_sitter_html::INJECTIONS_QUERY),
                None,
            ),
            build(
                "css",
                &tree_sitter_css::LANGUAGE.into(),
                tree_sitter_css::HIGHLIGHTS_QUERY,
                None,
                None,
            ),
            build(
                "sql",
                &tree_sitter_sequel::LANGUAGE.into(),
                tree_sitter_sequel::HIGHLIGHTS_QUERY,
                None,
                None,
            ),
        ]
        .into_iter()
        .flatten()
        .collect()
    })
}

/// Injections nun adds to what a grammar ships.
///
/// Rust's own query injects Rust into macro bodies and nothing else, so the
/// SQL people actually write — in `sqlx::query!` and friends — would be one
/// long string. The content captured is the inside of the literal, not the
/// quotes around it.
const RUST_INJECTIONS: &str = r#"
((macro_invocation
   macro: (scoped_identifier name: (identifier) @_name)
   (token_tree (string_literal (string_content) @injection.content)))
 (#any-of? @_name "query" "query_as" "query_scalar" "query_file" "query_file_as")
 (#set! injection.language "sql"))

((macro_invocation
   macro: (identifier) @_name
   (token_tree (string_literal (string_content) @injection.content)))
 (#any-of? @_name "sql" "query" "query_as" "query_scalar")
 (#set! injection.language "sql"))
"#;

/// Symbols nun adds to what a grammar's tags query finds.
///
/// Rust's own query tags a method inside a `declaration_list` but does not tag
/// the `impl` block holding it, so every method in a file comes out at the top
/// level beside the functions — an outline with no outline in it. Tagging the
/// block gives the methods something to nest under, and gives a type's
/// inherent and trait implementations the separate headings they have in the
/// file.
/// Two patterns that cannot both match, which is the only safe way to have
/// two: `!trait` and `trait:` are exclusive, so no `impl` is ever tagged
/// twice. Tagging one twice would give a definition two headings sharing a
/// range, and the second would read as nesting inside the first — with
/// everything really inside the block pushed a level deeper again.
///
/// Any self type at all, so an implementation for a reference, a slice, a
/// tuple, a path or a `dyn` heads its own methods: `impl Trait for &str` is
/// exactly as much an implementation as `impl Point`.
///
/// The trait comes into the heading because otherwise a type's inherent
/// implementation and its three trait implementations are four rows all
/// reading the same name, and picking between them is guesswork.
const RUST_SYMBOLS: &str = r"
((impl_item !trait type: (_) @name) @definition.impl)
((impl_item trait: (_) @context type: (_) @name) @definition.impl)
";

/// Build one language, or leave it out.
///
/// A grammar whose query does not compile is dropped rather than taken down
/// the process with it: the rest of the editor has nothing to do with it, and
/// a file in that language is still perfectly editable unhighlighted.
fn build(
    name: &'static str,
    grammar: &Grammar,
    highlights: &str,
    injections: Option<&str>,
    symbols: Option<&str>,
) -> Option<Language> {
    let highlights = Query::new(grammar, highlights).ok()?;
    let extra = match name {
        "rust" => RUST_INJECTIONS,
        _ => "",
    };
    let source = format!("{}{extra}", injections.unwrap_or_default());
    let injections =
        (!source.trim().is_empty()).then(|| Query::new(grammar, &source).ok()).flatten();
    // A grammar whose tags query does not compile simply has no outline; it
    // is not a reason to drop the language, which still highlights.
    let more = match name {
        "rust" => RUST_SYMBOLS,
        _ => "",
    };
    let symbols = format!("{}{more}", symbols.unwrap_or_default());
    let symbols =
        (!symbols.trim().is_empty()).then(|| Query::new(grammar, &symbols).ok()).flatten();
    Some(Language { name, grammar: grammar.clone(), highlights, injections, symbols })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn every_grammar_and_query_compiled() {
        let names: Vec<&str> = all().iter().map(|language| language.name).collect();
        assert_eq!(
            names,
            ["rust", "json", "toml", "javascript", "python", "html", "css", "sql"],
            "a language missing here means its query failed to compile"
        );
    }

    #[test]
    fn files_are_recognised_by_their_names() {
        let named = |path: &str| of_path(Path::new(path)).map(|language| language.name);
        assert_eq!(named("src/main.rs"), Some("rust"));
        assert_eq!(named("Cargo.toml"), Some("toml"));
        assert_eq!(named("Cargo.lock"), Some("toml"), "it is TOML whatever it is called");
        assert_eq!(named("app.jsx"), Some("javascript"));
        assert_eq!(named("index.html"), Some("html"));
        assert_eq!(named("schema.sql"), Some("sql"));
        assert_eq!(named("notes.txt"), None);
        assert_eq!(named("LICENSE"), None);
    }

    #[test]
    fn injections_name_languages_in_their_own_vocabulary() {
        assert_eq!(of_name("rs").map(|language| language.name), Some("rust"));
        assert_eq!(of_name("postgresql").map(|language| language.name), Some("sql"));
        assert_eq!(of_name("brainfuck").map(|language| language.name), None);
    }

    #[test]
    fn the_languages_with_embedded_code_can_find_it() {
        assert!(of_name("rust").unwrap().injections.is_some(), "SQL in a string");
        assert!(of_name("html").unwrap().injections.is_some(), "CSS and JS in a page");
    }

    #[test]
    fn every_capture_name_is_available_for_mapping() {
        for language in all() {
            assert!(
                !language.capture_names().is_empty(),
                "{} has no captures, so nothing would ever be coloured",
                language.name
            );
        }
    }
}
