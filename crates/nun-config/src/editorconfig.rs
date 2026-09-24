//! `.editorconfig`: the whitespace a project asks of every editor.
//!
//! The format is from <https://spec.editorconfig.org>. Files are read from the
//! file's own directory upwards, stopping after one that says `root = true`;
//! within a file, later sections win over earlier ones, and a closer file wins
//! over a farther one. Properties nun does not know are left alone — other
//! tools read the same file — but a known property with a value nun cannot use
//! is reported, with its line.
//!
//! An `.editorconfig` needs no trust: it can only say how whitespace is written,
//! and it is the same file every other editor on the machine already obeys.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::Problem;

/// The file name looked for in each directory.
pub const NAME: &str = ".editorconfig";

/// One `.editorconfig`, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorConfig {
    /// Where it is.
    pub path: PathBuf,
    /// Whether it says `root = true`: nothing above it is read.
    pub root: bool,
    /// Its sections, in the order they are written.
    pub sections: Vec<Section>,
    /// Lines that could not be read.
    pub problems: Vec<Problem>,
}

/// One `[glob]` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The glob, as written.
    pub glob: String,
    glob_compiled: Vec<Token>,
    /// Its properties, in order: name, value, line. Names are lowercased, and
    /// so are the values of the properties the spec defines.
    pub properties: Vec<(String, String, usize)>,
}

/// A property that applies to a file, and where it was said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// The value, lowercased where the spec says values are case-insensitive.
    pub value: String,
    /// The `.editorconfig` it is from.
    pub path: PathBuf,
    /// The line it is on.
    pub line: usize,
    /// The section it is in.
    pub glob: String,
}

/// The properties the spec defines, whose values are case-insensitive.
const KNOWN: &[&str] = &[
    "indent_style",
    "indent_size",
    "tab_width",
    "end_of_line",
    "charset",
    "trim_trailing_whitespace",
    "insert_final_newline",
    "max_line_length",
    "root",
];

impl EditorConfig {
    /// Read `text` as the `.editorconfig` at `path`.
    ///
    /// Lines that are neither a section, a property, a comment nor blank are
    /// reported and skipped; everything else still applies.
    #[must_use]
    pub fn parse(path: &Path, text: &str) -> Self {
        let mut config = Self {
            path: path.to_path_buf(),
            root: false,
            sections: Vec::new(),
            problems: Vec::new(),
        };
        let dir = path.parent().unwrap_or_else(|| Path::new("/"));
        // A byte-order mark is not whitespace to `trim`, and would otherwise
        // stick to the first line.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            if let Some(glob) = trimmed.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
                config.sections.push(Section {
                    glob: glob.to_string(),
                    glob_compiled: compile(dir, glob),
                    properties: Vec::new(),
                });
                continue;
            }
            let Some((name, value)) = trimmed.split_once('=') else {
                config.problems.push(Problem {
                    path: path.to_path_buf(),
                    line: Some(line),
                    message: format!("`{trimmed}` is neither a [section] nor a name = value"),
                });
                continue;
            };
            let name = name.trim().to_lowercase();
            let value = value.trim();
            let value = if KNOWN.contains(&name.as_str()) {
                value.to_lowercase()
            } else {
                value.to_string()
            };
            match config.sections.last_mut() {
                Some(section) => section.properties.push((name, value, line)),
                // Before any section, only `root` means anything.
                None if name == "root" => config.root = value == "true",
                None => {}
            }
        }
        config
    }

    /// Read the `.editorconfig` in `dir`, if there is one.
    #[must_use]
    pub fn read(dir: &Path) -> Option<Self> {
        let path = dir.join(NAME);
        let text = std::fs::read_to_string(&path).ok()?;
        Some(Self::parse(&path, &text))
    }
}

/// The `.editorconfig` files that bear on `file`, nearest first: one per
/// directory from the file's own upwards, stopping after the first that says
/// `root = true`. `read` gives the file in a directory, if there is one, so the
/// caller can cache them.
pub fn find(file: &Path, mut read: impl FnMut(&Path) -> Option<EditorConfig>) -> Vec<EditorConfig> {
    let mut found = Vec::new();
    for dir in file.ancestors().skip(1) {
        if let Some(config) = read(dir) {
            let root = config.root;
            found.push(config);
            if root {
                break;
            }
        }
    }
    found
}

/// Every property that applies to `file`, from `configs` nearest first, with
/// where each was said. `unset` removes a property, as the spec says.
#[must_use]
pub fn properties(file: &Path, configs: &[EditorConfig]) -> BTreeMap<String, Property> {
    let text: Vec<char> = slashed(file).chars().collect();
    let mut out = BTreeMap::new();
    // Farthest first, so nearer files and later sections overwrite.
    for config in configs.iter().rev() {
        for section in &config.sections {
            if !matches(&section.glob_compiled, &text) {
                continue;
            }
            for (name, value, line) in &section.properties {
                out.insert(
                    name.clone(),
                    Property {
                        value: value.clone(),
                        path: config.path.clone(),
                        line: *line,
                        glob: section.glob.clone(),
                    },
                );
            }
        }
    }
    out.retain(|_, property| property.value != "unset");
    out
}

/// A path with `/` between its parts, whatever the platform uses.
fn slashed(path: &Path) -> String {
    let text = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' { text.into_owned() } else { text.replace('\\', "/") }
}

// ── globs ───────────────────────────────────────────────────────────────────

/// One piece of a compiled glob.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// This character.
    Char(char),
    /// `?`: any one character but `/`.
    One,
    /// `*`: any run of characters without a `/`.
    Star,
    /// `**`: any run of characters at all.
    StarStar,
    /// `**/`: nothing, or any run of whole directories.
    Dirs,
    /// `[...]` or `[!...]`: one character in, or not in, the ranges.
    Class { negated: bool, ranges: Vec<(char, char)> },
    /// `{a,b,c}`: any one of these.
    Either(Vec<Vec<Token>>),
    /// `{n1..n2}`: a whole number in this range.
    Number(i64, i64),
}

/// Compile the glob of a section in the `.editorconfig` in `dir`.
///
/// A glob with no `/` in it matches a file of that name anywhere below `dir`;
/// one with a `/` is relative to `dir`, and a leading `/` changes nothing.
fn compile(dir: &Path, glob: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> =
        slashed(dir).trim_end_matches('/').chars().map(Token::Char).collect();
    tokens.push(Token::Char('/'));
    if !glob.contains('/') {
        tokens.push(Token::Dirs);
    }
    let glob = glob.strip_prefix('/').unwrap_or(glob);
    let chars: Vec<char> = glob.chars().collect();
    tokens.extend(parse(&chars));
    tokens
}

/// Parse a glob, or part of one, into tokens.
fn parse(chars: &[char]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        match chars[at] {
            '\\' if at + 1 < chars.len() => {
                tokens.push(Token::Char(chars[at + 1]));
                at += 2;
            }
            '?' => {
                tokens.push(Token::One);
                at += 1;
            }
            '*' if chars.get(at + 1) == Some(&'*') => {
                if chars.get(at + 2) == Some(&'/') {
                    tokens.push(Token::Dirs);
                    at += 3;
                } else {
                    tokens.push(Token::StarStar);
                    at += 2;
                }
            }
            '*' => {
                tokens.push(Token::Star);
                at += 1;
            }
            '[' => {
                if let Some((token, used)) = class(&chars[at..]) {
                    tokens.push(token);
                    at += used;
                } else {
                    tokens.push(Token::Char('['));
                    at += 1;
                }
            }
            '{' => {
                if let Some(end) = closing_brace(&chars[at..]) {
                    tokens.extend(braces(&chars[at + 1..at + end]));
                    at += end + 1;
                } else {
                    tokens.push(Token::Char('{'));
                    at += 1;
                }
            }
            ch => {
                tokens.push(Token::Char(ch));
                at += 1;
            }
        }
    }
    tokens
}

/// A `[...]` class at the start of `chars`, and how many characters it took.
/// `None` when it is not closed, or holds a `/`, and so is a literal `[`.
fn class(chars: &[char]) -> Option<(Token, usize)> {
    let mut at = 1;
    let negated = matches!(chars.get(at), Some('!' | '^'));
    if negated {
        at += 1;
    }
    let mut ranges = Vec::new();
    let mut first = true;
    loop {
        let ch = *chars.get(at)?;
        if ch == ']' && !first {
            return Some((Token::Class { negated, ranges }, at + 1));
        }
        if ch == '/' {
            return None;
        }
        first = false;
        let ch = if ch == '\\' {
            at += 1;
            *chars.get(at)?
        } else {
            ch
        };
        if chars.get(at + 1) == Some(&'-') && chars.get(at + 2).is_some_and(|end| *end != ']') {
            ranges.push((ch, chars[at + 2]));
            at += 3;
        } else {
            ranges.push((ch, ch));
            at += 1;
        }
    }
}

/// Where the `}` closing the `{` at the start of `chars` is.
fn closing_brace(chars: &[char]) -> Option<usize> {
    let mut depth = 0;
    let mut at = 0;
    while at < chars.len() {
        match chars[at] {
            '\\' => at += 1,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// What is between a pair of braces: alternatives, a range of numbers, or —
/// with neither — the braces and their contents as they are, as the spec says
/// a `{single}` is matched.
fn braces(inner: &[char]) -> Vec<Token> {
    if let Some(range) = number_range(inner) {
        return vec![range];
    }
    let mut parts = Vec::new();
    let (mut depth, mut start, mut at) = (0, 0, 0);
    while at < inner.len() {
        match inner[at] {
            '\\' => at += 1,
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&inner[start..at]);
                start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    if parts.is_empty() {
        let mut tokens = vec![Token::Char('{')];
        tokens.extend(parse(inner));
        tokens.push(Token::Char('}'));
        return tokens;
    }
    parts.push(&inner[start..]);
    vec![Token::Either(parts.into_iter().map(parse).collect())]
}

fn number_range(inner: &[char]) -> Option<Token> {
    let text: String = inner.iter().collect();
    let (low, high) = text.split_once("..")?;
    let number = |text: &str| {
        let digits = text.strip_prefix('-').unwrap_or(text);
        (!digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit()))
            .then(|| text.parse::<i64>().ok())
            .flatten()
    };
    let (low, high) = (number(low)?, number(high)?);
    Some(Token::Number(low.min(high), low.max(high)))
}

/// Whether the whole of `text` matches `tokens`.
fn matches(tokens: &[Token], text: &[char]) -> bool {
    step(tokens, text, &|rest| rest.is_empty())
}

/// Match `tokens` against the start of `text`, then hand what is left to
/// `then`. Backtracking, which is fine for globs the length of a path.
fn step(tokens: &[Token], text: &[char], then: &dyn Fn(&[char]) -> bool) -> bool {
    let Some((token, rest)) = tokens.split_first() else { return then(text) };
    match token {
        Token::Char(ch) => text.first() == Some(ch) && step(rest, &text[1..], then),
        Token::One => text.first().is_some_and(|ch| *ch != '/') && step(rest, &text[1..], then),
        Token::Star => {
            let most = text.iter().position(|ch| *ch == '/').unwrap_or(text.len());
            (0..=most).any(|taken| step(rest, &text[taken..], then))
        }
        Token::StarStar => (0..=text.len()).any(|taken| step(rest, &text[taken..], then)),
        Token::Dirs => {
            step(rest, text, then)
                || text
                    .iter()
                    .enumerate()
                    .filter(|(_, ch)| **ch == '/')
                    .any(|(at, _)| at > 0 && step(rest, &text[at + 1..], then))
        }
        Token::Class { negated, ranges } => {
            let Some(ch) = text.first() else { return false };
            let inside = ranges.iter().any(|(low, high)| (low..=high).contains(&ch));
            *ch != '/' && inside != *negated && step(rest, &text[1..], then)
        }
        Token::Either(options) => options
            .iter()
            .any(|option| step(option, text, &|after: &[char]| step(rest, after, then))),
        Token::Number(low, high) => {
            let sign = usize::from(text.first() == Some(&'-'));
            let digits = text[sign..].iter().take_while(|ch| ch.is_ascii_digit()).count();
            (1..=digits).any(|taken| {
                let written: String = text[..sign + taken].iter().collect();
                written.parse::<i64>().is_ok_and(|number| (*low..=*high).contains(&number))
                    && step(rest, &text[sign + taken..], then)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `glob`, in an `.editorconfig` in `/p`, applies to `file`.
    fn applies(glob: &str, file: &str) -> bool {
        let tokens = compile(Path::new("/p"), glob);
        matches(&tokens, &file.chars().collect::<Vec<_>>())
    }

    #[test]
    fn a_glob_without_a_slash_matches_the_name_at_any_depth() {
        assert!(applies("*", "/p/a.rs"));
        assert!(applies("*", "/p/src/deep/a.rs"));
        assert!(applies("*.rs", "/p/a.rs"));
        assert!(applies("*.rs", "/p/src/a.rs"));
        assert!(!applies("*.rs", "/p/a.rsx"));
        assert!(!applies("*.rs", "/q/a.rs"), "only below its own directory");
        assert!(applies("Makefile", "/p/sub/Makefile"));
        assert!(!applies("Makefile", "/p/sub/GNUmakefile"));
    }

    #[test]
    fn a_glob_with_a_slash_is_relative_to_its_file() {
        assert!(applies("src/*.rs", "/p/src/a.rs"));
        assert!(!applies("src/*.rs", "/p/x/src/a.rs"));
        assert!(!applies("src/*.rs", "/p/src/deep/a.rs"), "* stops at a slash");
        assert!(applies("/src/*.rs", "/p/src/a.rs"), "a leading slash changes nothing");
        assert!(applies("src/**.rs", "/p/src/deep/a.rs"), "** does not");
        assert!(applies("src/**/a.rs", "/p/src/a.rs"), "**/ can be no directories");
        assert!(applies("src/**/a.rs", "/p/src/x/y/a.rs"));
        assert!(!applies("src/**/a.rs", "/p/src/xa.rs"));
    }

    #[test]
    fn a_question_mark_is_one_character_but_a_slash() {
        assert!(applies("?.rs", "/p/a.rs"));
        assert!(!applies("?.rs", "/p/ab.rs"));
        assert!(!applies("a?b", "/p/a/b"));
        assert!(applies("?.rs", "/p/é.rs"), "one character, not one byte");
    }

    #[test]
    fn classes_match_one_character() {
        assert!(applies("[abc].rs", "/p/b.rs"));
        assert!(!applies("[abc].rs", "/p/d.rs"));
        assert!(applies("[a-c].rs", "/p/b.rs"));
        assert!(applies("[!a-c].rs", "/p/d.rs"));
        assert!(!applies("[!a-c].rs", "/p/a.rs"));
        assert!(applies("[]].rs", "/p/].rs"), "a ] first is literal");
        assert!(applies("[.rs", "/p/[.rs"), "an unclosed [ is literal");
        assert!(applies("a[/]b", "/p/a[/]b"), "a class may not hold a slash");
    }

    #[test]
    fn braces_are_alternatives_ranges_or_literal() {
        assert!(applies("*.{js,ts}", "/p/a.js"));
        assert!(applies("*.{js,ts}", "/p/a.ts"));
        assert!(!applies("*.{js,ts}", "/p/a.rs"));
        assert!(applies("{package.json,.travis.yml}", "/p/package.json"));
        assert!(applies("*.{a,{b,c}}", "/p/x.c"), "nested");
        assert!(applies("*.{,x}", "/p/a."), "an empty alternative");
        assert!(applies("{single}", "/p/{single}"), "one choice is no choice");
        assert!(!applies("{single}", "/p/single"));
        assert!(applies("{a", "/p/{a"), "an unclosed brace is literal");
    }

    #[test]
    fn number_ranges_match_whole_numbers() {
        assert!(applies("file{1..3}", "/p/file2"));
        assert!(!applies("file{1..3}", "/p/file4"));
        assert!(applies("file{1..3}", "/p/file03"), "a leading zero is still 3");
        assert!(applies("f{-2..2}", "/p/f-1"));
        assert!(!applies("f{-2..2}", "/p/f-3"));
        assert!(applies("f{3..1}", "/p/f2"), "either way round");
        assert!(!applies("f{1..3}", "/p/fx"));
        assert!(applies("f{1..3}.txt", "/p/f1.txt"));
        assert!(applies("f{a..b}", "/p/f{a..b}"), "not numbers: literal");
    }

    #[test]
    fn escapes_make_anything_literal() {
        assert!(applies("\\*.rs", "/p/*.rs"));
        assert!(!applies("\\*.rs", "/p/a.rs"));
        assert!(applies("a\\{b,c\\}", "/p/a{b,c}"));
    }

    #[test]
    fn the_directory_is_matched_literally() {
        let tokens = compile(Path::new("/p/[x]"), "*.rs");
        assert!(matches(&tokens, &"/p/[x]/a.rs".chars().collect::<Vec<_>>()));
        assert!(!matches(&tokens, &"/p/x/a.rs".chars().collect::<Vec<_>>()));
    }

    #[test]
    fn a_file_is_read_with_its_lines() {
        let text = "root = true\n\n# comment\n[*]\nindent_style = Space\nindent_size = 2\n\n; also\n[*.md]\ntrim_trailing_whitespace = false\nnonsense line\ncustom = KeepCase\n";
        let config = EditorConfig::parse(Path::new("/p/.editorconfig"), text);
        assert!(config.root);
        assert_eq!(config.sections.len(), 2);
        assert_eq!(config.sections[0].properties[0], ("indent_style".into(), "space".into(), 5));
        assert_eq!(config.sections[1].glob, "*.md");
        assert_eq!(config.sections[1].properties[1].1, "KeepCase", "unknown values keep case");
        assert_eq!(config.problems.len(), 1);
        assert_eq!(config.problems[0].line, Some(11));
    }

    #[test]
    fn a_byte_order_mark_and_crlf_are_read_through() {
        let config = EditorConfig::parse(
            Path::new("/p/.editorconfig"),
            "\u{feff}root = true\r\n[*.rs]\r\nindent_size = 2\r\n",
        );
        assert!(config.root);
        assert_eq!(config.problems, []);
        assert_eq!(config.sections[0].properties[0], ("indent_size".into(), "2".into(), 3));
    }

    #[test]
    fn nearer_files_and_later_sections_win_and_unset_removes() {
        let outer = EditorConfig::parse(
            Path::new("/p/.editorconfig"),
            "root = true\n[*]\nindent_size = 4\nend_of_line = lf\ncharset = utf-8\n",
        );
        let inner = EditorConfig::parse(
            Path::new("/p/sub/.editorconfig"),
            "[*]\nindent_size = 2\n[*.rs]\nindent_size = 3\n[*]\nend_of_line = unset\n",
        );
        let found = properties(Path::new("/p/sub/a.rs"), &[inner, outer]);
        assert_eq!(found["indent_size"].value, "3");
        assert_eq!(found["indent_size"].line, 4);
        assert_eq!(found["indent_size"].glob, "*.rs");
        assert!(!found.contains_key("end_of_line"));
        assert_eq!(found["charset"].path, Path::new("/p/.editorconfig"));
    }

    #[test]
    fn reading_stops_at_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path();
        std::fs::create_dir_all(top.join("a/b")).unwrap();
        std::fs::write(top.join(".editorconfig"), "[*]\nindent_size = 8\n").unwrap();
        std::fs::write(top.join("a/.editorconfig"), "root = true\n[*]\nindent_size = 2\n").unwrap();
        let found = find(&top.join("a/b/c.rs"), EditorConfig::read);
        assert_eq!(found.len(), 1, "the root stops the walk");
        assert_eq!(properties(&top.join("a/b/c.rs"), &found)["indent_size"].value, "2");
    }
}
