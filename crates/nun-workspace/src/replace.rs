//! Rewriting what a search found, one line at a time.
//!
//! A project-wide replace is the most destructive thing an editor does, and it
//! is one click away from the panel that found the hits. So the whole design
//! here is about being able to say afterwards exactly what was written and
//! exactly what was not.
//!
//! **The preview and the write use one engine.** [`Replacer`] turns a line into
//! what it would become, and the same call answers the panel's `before → after`
//! row and builds the file that is written. A preview that was computed a
//! different way from the write would be a preview of nothing.
//!
//! **Capture groups come from the matcher, not from here.** In regex mode the
//! replacement is interpolated by `grep-matcher` against the same captures the
//! search matched with, so `$1`, `${name}` and `$$` mean what they mean in
//! ripgrep and there is no second escaping rule to keep true. In literal mode
//! nothing is interpolated at all: a `$1` in the replacement is a dollar, a
//! one, and nothing else.
//!
//! **Only the chosen lines change.** The caller names line numbers, and a line
//! that matches but was not named is copied out byte for byte. Every match on a
//! line that *was* named is replaced, not the first, which is why
//! [`Hit::matched`](crate::Hit::matched) is a list.
//!
//! **What was previewed has to still be there.** Two checks stand between a
//! preview and a write — the file's modification time against when the search
//! started, and every chosen line still being there and still matching — and a
//! file that fails either is left alone and said so in the [`Report`] rather
//! than being rewritten quietly.

use std::cell::RefCell;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use grep_matcher::{Captures as _, Matcher as _};
use grep_regex::{RegexCaptures, RegexMatcher};

use crate::grep::{Options, build_matcher};

/// A replacement, compiled once and applied to many lines.
///
/// The panel asks for a preview of every visible row on every frame, so the
/// matcher is built here rather than per call, and the scratch captures are
/// kept and reused.
#[derive(Debug)]
pub struct Replacer {
    matcher: RegexMatcher,
    /// The replacement as bytes, because that is what interpolation writes
    /// against.
    replacement: Vec<u8>,
    /// Whether the replacement has capture references to expand. Literal
    /// queries never do, and a regex replacement with no `$` in it has nothing
    /// to expand either, so both skip the captures entirely.
    interpolates: bool,
    /// Scratch captures, reused rather than allocated per line.
    captures: RefCell<RegexCaptures>,
}

impl Replacer {
    /// Compile `options` and hold `replacement` ready to apply.
    ///
    /// Whether the replacement interpolates depends on `options.regex`, and it
    /// is the one difference worth being sure of: in regex mode `$1`,
    /// `${name}` and `$$` are expanded against the captures of each match,
    /// exactly as ripgrep expands them; in literal mode the replacement is
    /// literal text and every character of it, `$` included, means itself.
    ///
    /// # Errors
    ///
    /// The query as a sentence to show, when it is not a regular expression
    /// that compiles — which is what a half-typed one is — or when it is
    /// empty, since an empty query would otherwise match between every pair of
    /// characters in the project.
    pub fn new(options: &Options, replacement: &str) -> Result<Self, String> {
        if options.query.is_empty() {
            return Err("there is nothing to replace".to_owned());
        }
        let matcher = build_matcher(options)?;
        let captures = RefCell::new(matcher.new_captures().map_err(|error| error.to_string())?);
        Ok(Self {
            matcher,
            replacement: replacement.as_bytes().to_vec(),
            interpolates: options.regex && replacement.contains('$'),
            captures,
        })
    }

    /// What `line` becomes when the replacement is applied to every match in
    /// it.
    ///
    /// A line that does not match comes back unchanged, so this is also the
    /// `after` side of a preview for a row the user is about to exclude.
    ///
    /// What goes in is what comes out: given a windowed
    /// [`Hit::text`](crate::Hit::text) this previews the window, while the
    /// write rewrites every match in the whole line. On a line long enough for
    /// that to differ there is nothing on screen to preview anyway.
    #[must_use]
    pub fn line(&self, line: &str) -> String {
        // The lossy step is unreachable for anything a person types: the only
        // way a rewrite is not text is a `(?-u)` pattern whose match cuts a
        // character in half. It is here so a preview of that still renders,
        // and it is deliberately *not* how a file is written — [`plan`] takes
        // the strict path and refuses the file instead.
        String::from_utf8_lossy(&self.rewrite(line)).into_owned()
    }

    /// Whether `line` matches at all.
    #[must_use]
    pub fn matches(&self, line: &str) -> bool {
        self.matcher.is_match(line.as_bytes()).unwrap_or(false)
    }

    /// The same rewrite as [`Replacer::line`], refusing to guess when the
    /// result would not be text.
    fn text(&self, line: &str) -> Option<String> {
        String::from_utf8(self.rewrite(line)).ok()
    }

    fn rewrite(&self, line: &str) -> Vec<u8> {
        let haystack = line.as_bytes();
        let mut out = Vec::with_capacity(haystack.len());
        // Both arms are infallible — `RegexMatcher::Error` is `NoError` — so
        // there is nothing to report and nothing to fall back to.
        if self.interpolates {
            let mut captures = self.captures.borrow_mut();
            let _ = self.matcher.replace_with_captures(
                haystack,
                &mut captures,
                &mut out,
                |captures, dst| {
                    captures.interpolate(
                        |name| self.matcher.capture_index(name),
                        haystack,
                        &self.replacement,
                        dst,
                    );
                    true
                },
            );
        } else {
            let _ = self.matcher.replace(haystack, &mut out, |_, dst| {
                dst.extend_from_slice(&self.replacement);
                true
            });
        }
        out
    }
}

/// What `line` becomes when `replacement` is applied to every match in it.
///
/// `None` when the options do not compile — the panel shows the error the
/// search already reported rather than a preview.
///
/// In regex mode the replacement interpolates capture groups: `$1`, `${name}`,
/// and `$$` for a literal dollar. In literal mode it does not, and a `$1` in
/// the replacement is three characters of replacement text.
///
/// This compiles the query every time it is called. A caller previewing many
/// lines — the panel, every frame — should hold a [`Replacer`] instead.
#[must_use]
pub fn preview(options: &Options, replacement: &str, line: &str) -> Option<String> {
    Replacer::new(options, replacement).ok().map(|replacer| replacer.line(line))
}

/// What a replace did to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It was rewritten, and this many of its lines are different for it. Zero
    /// when every chosen line came out the same as it went in, in which case
    /// nothing was written.
    Changed(usize),
    /// It was left exactly as it was, for this reason.
    Skipped(Skipped),
    /// It could not be acted on at all, and this is why, as a sentence.
    Failed(String),
}

/// Why a file was left as it was rather than rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skipped {
    /// Something wrote to it after the search ran, so what was previewed is
    /// not what is there now.
    Written,
    /// A line that was chosen is no longer there, or no longer matches.
    Moved,
    /// It is not UTF-8 text, or the replacement would not leave it as text.
    /// Either way, writing it back would mangle it.
    NotText,
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Written => f.write_str("written to since the search"),
            Self::Moved => f.write_str("its lines have moved"),
            Self::NotText => f.write_str("not text"),
        }
    }
}

/// What a whole replace came to.
///
/// The paths are relative to the root that was searched, so they are the same
/// paths the panel showed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Every file that was asked for, in the order it was asked for, and what
    /// happened to it.
    pub files: Vec<(PathBuf, Outcome)>,
    /// How many lines changed across all of them.
    pub lines: usize,
}

impl Report {
    /// How many files were rewritten.
    #[must_use]
    pub fn changed(&self) -> usize {
        self.count(|outcome| matches!(outcome, Outcome::Changed(lines) if *lines > 0))
    }

    /// How many files were left as they were because they are no longer what
    /// was searched.
    #[must_use]
    pub fn skipped(&self) -> usize {
        self.count(|outcome| matches!(outcome, Outcome::Skipped(_)))
    }

    /// How many files could not be acted on at all.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.count(|outcome| matches!(outcome, Outcome::Failed(_)))
    }

    fn count(&self, which: impl Fn(&Outcome) -> bool) -> usize {
        self.files.iter().filter(|(_, outcome)| which(outcome)).count()
    }
}

/// A sentence for the status line: `Changed 42 lines in 7 files, skipped 1`.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Changed {} {} in {} {}",
            self.lines,
            plural(self.lines, "line"),
            self.changed(),
            plural(self.changed(), "file")
        )?;
        if self.skipped() > 0 {
            write!(f, ", skipped {}", self.skipped())?;
        }
        if self.failed() > 0 {
            write!(f, ", {} failed", self.failed())?;
        }
        Ok(())
    }
}

/// `line` or `lines`, without a format string per call site.
pub(crate) fn plural(count: usize, word: &str) -> String {
    if count == 1 { word.to_owned() } else { format!("{word}s") }
}

/// What reading a file says should happen to it.
pub(crate) enum Plan {
    /// Write this text; this many of its lines are different for it.
    Write(String, usize),
    /// Do not write it, for this reason.
    Leave(Outcome),
}

/// Work out what `path` should become, without writing anything.
///
/// `lines` are line numbers counting from one, sorted and without repeats.
///
/// Two checks stand between the preview and the write, and either one leaves
/// the file untouched.
///
/// The first is `searched_at`: a file whose modification time is newer than
/// the moment the search *started* has been written to since, so nothing on
/// screen for it can be trusted, whichever lines still happen to match. This
/// is the check that catches the dangerous case — a `git checkout` or a
/// format-on-save that rewrites a file into something where line 42 still
/// matches but is a different line 42 — and it is the reason a timestamp is
/// worth carrying through the job. Taking the search's *start* rather than its
/// end errs towards skipping a file that was written while the walk was still
/// running, which is the safe direction.
///
/// The second is that every chosen line must still be there and still match.
/// That is weaker on its own — it cannot see an edit that left the line
/// matching — but it is what makes a bug in line numbering harmless rather
/// than silent: nothing is ever written over a line the query does not match.
///
/// A file failing either check fails as a whole. Applying half of a preview
/// would be worse than applying none of it.
pub(crate) fn plan(
    path: &Path,
    replacer: &Replacer,
    lines: &[u32],
    searched_at: SystemTime,
) -> Plan {
    let modified = match fs::metadata(path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => modified,
        Err(error) => return Plan::Leave(Outcome::Failed(format!("{}: {error}", path.display()))),
    };
    if modified > searched_at {
        return Plan::Leave(Outcome::Skipped(Skipped::Written));
    }

    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return Plan::Leave(Outcome::Failed(format!("{}: {error}", path.display()))),
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Plan::Leave(Outcome::Skipped(Skipped::NotText));
    };

    let split = split(text);
    let mut rewritten: Vec<(usize, String)> = Vec::new();
    for &number in lines {
        let Some(index) = (number as usize).checked_sub(1) else {
            return Plan::Leave(Outcome::Skipped(Skipped::Moved));
        };
        let Some(&(content, _)) = split.get(index) else {
            return Plan::Leave(Outcome::Skipped(Skipped::Moved));
        };
        if !replacer.matches(content) {
            return Plan::Leave(Outcome::Skipped(Skipped::Moved));
        }
        let Some(after) = replacer.text(content) else {
            return Plan::Leave(Outcome::Skipped(Skipped::NotText));
        };
        if after != content {
            rewritten.push((index, after));
        }
    }
    if rewritten.is_empty() {
        // Every chosen line matched and came out identical — replacing `foo`
        // with `foo`. There is nothing to write and nothing to undo.
        return Plan::Leave(Outcome::Changed(0));
    }

    let changed = rewritten.len();
    let mut out = String::with_capacity(text.len());
    let mut next = 0;
    for (index, (content, terminator)) in split.iter().enumerate() {
        if rewritten.get(next).is_some_and(|&(at, _)| at == index) {
            out.push_str(&rewritten[next].1);
            next += 1;
        } else {
            out.push_str(content);
        }
        // Each line keeps the ending it had, so a CRLF file stays CRLF and a
        // file with no final newline does not grow one.
        out.push_str(terminator);
    }
    Plan::Write(out, changed)
}

/// The lines of `text`, each with whatever ended it.
///
/// A line's ending is `\n`, `\r\n`, or — for a last line the file does not
/// terminate — nothing at all. Putting the two halves back together is what
/// makes a replace keep a file's line endings and its final-newline-or-not
/// instead of quietly normalising a repository.
fn split(text: &str) -> Vec<(&str, &str)> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let (line, tail) = match rest.find('\n') {
            Some(at) => (&rest[..=at], &rest[at + 1..]),
            None => (rest, ""),
        };
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        lines.push((content, &line[content.len()..]));
        rest = tail;
    }
    lines
}

/// One file's chosen lines, sorted, without repeats, and with a path named
/// twice folded into one entry.
///
/// The panel builds its list from rows the user clicked, so it is under no
/// obligation to hand them over in any particular order — and a file
/// rewritten twice in one pass would find its own output on the second go and
/// skip it, which would be a confusing way to learn that.
pub(crate) fn tidy(chosen: &[(PathBuf, Vec<u32>)]) -> Vec<(PathBuf, Vec<u32>)> {
    let mut tidied: Vec<(PathBuf, Vec<u32>)> = Vec::with_capacity(chosen.len());
    for (path, lines) in chosen {
        match tidied.iter_mut().find(|(seen, _)| seen == path) {
            Some((_, seen)) => seen.extend_from_slice(lines),
            None => tidied.push((path.clone(), lines.clone())),
        }
    }
    for (_, lines) in &mut tidied {
        lines.sort_unstable();
        lines.dedup();
    }
    tidied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grep::Case;

    fn literal(query: &str) -> Options {
        Options { query: query.into(), case: Case::Sensitive, ..Options::default() }
    }

    fn regex(query: &str) -> Options {
        Options { regex: true, ..literal(query) }
    }

    #[test]
    fn a_literal_replacement_changes_every_match_on_the_line() {
        let options = literal("cat");
        assert_eq!(
            preview(&options, "dog", "the cat sat on the cat").unwrap(),
            "the dog sat on the dog"
        );
    }

    #[test]
    fn a_line_that_does_not_match_comes_back_as_it_was() {
        assert_eq!(preview(&literal("cat"), "dog", "no animals here").unwrap(), "no animals here");
    }

    #[test]
    fn capture_groups_are_interpolated_in_regex_mode() {
        let options = regex(r"fn (\w+)\(");
        assert_eq!(
            preview(&options, "fn new_$1(", "fn parse(x: u8) {").unwrap(),
            "fn new_parse(x: u8) {"
        );
    }

    #[test]
    fn a_named_capture_group_is_interpolated_too() {
        let options = regex(r"(?P<key>\w+)=(?P<value>\w+)");
        assert_eq!(preview(&options, "${value}=${key}", "a=b and c=d").unwrap(), "b=a and d=c");
    }

    #[test]
    fn a_doubled_dollar_is_one_dollar_in_regex_mode() {
        assert_eq!(preview(&regex("price"), "$$1", "price here").unwrap(), "$1 here");
    }

    #[test]
    fn a_dollar_is_a_dollar_in_literal_mode() {
        // The one difference between the modes, and the one people get wrong:
        // literal text is literal on both sides of the replace.
        let options = literal("price");
        assert_eq!(preview(&options, "$1", "price here").unwrap(), "$1 here");
        assert_eq!(preview(&options, "${name}", "price here").unwrap(), "${name} here");
    }

    #[test]
    fn a_literal_query_does_not_smuggle_a_regex_in() {
        assert_eq!(preview(&literal("a.c"), "x", "a.c abc").unwrap(), "x abc");
    }

    #[test]
    fn an_unparseable_query_has_no_preview() {
        assert!(preview(&regex("fn ("), "x", "fn (").is_none());
        assert!(preview(&literal(""), "x", "anything").is_none());
    }

    #[test]
    fn a_replacement_can_make_a_line_longer_or_empty_it() {
        assert_eq!(preview(&literal("a"), "aaaa", "a-a").unwrap(), "aaaa-aaaa");
        assert_eq!(preview(&literal("gone"), "", "gone").unwrap(), "");
    }

    #[test]
    fn a_preview_counts_characters_not_bytes() {
        let options = literal("日本");
        assert_eq!(preview(&options, "🦀", "x日本y日本").unwrap(), "x🦀y🦀");
    }

    #[test]
    fn a_whole_word_replacement_leaves_longer_words_alone() {
        let options = Options { whole_word: true, ..literal("value") };
        assert_eq!(preview(&options, "x", "value revalued value").unwrap(), "x revalued x");
    }

    #[test]
    fn an_insensitive_query_replaces_every_case() {
        let options = Options { case: Case::Insensitive, ..literal("needle") };
        assert_eq!(preview(&options, "pin", "Needle NEEDLE needle").unwrap(), "pin pin pin");
    }

    #[test]
    fn one_replacer_answers_for_many_lines() {
        let replacer = Replacer::new(&regex(r"(\w+)@(\w+)"), "$2.$1").unwrap();
        assert_eq!(replacer.line("a@b"), "b.a");
        assert_eq!(replacer.line("c@d and e@f"), "d.c and f.e");
        assert!(replacer.matches("a@b"));
        assert!(!replacer.matches("nothing here"));
    }

    #[test]
    fn a_line_keeps_whatever_ended_it() {
        assert_eq!(split("a\nb\r\nc"), [("a", "\n"), ("b", "\r\n"), ("c", "")]);
        assert_eq!(split("a\n"), [("a", "\n")]);
        assert_eq!(split(""), []);
        assert_eq!(split("\n\n"), [("", "\n"), ("", "\n")]);
    }

    #[test]
    fn chosen_lines_are_sorted_and_a_path_named_twice_is_folded_in() {
        let chosen = vec![
            (PathBuf::from("a.rs"), vec![7, 3, 3]),
            (PathBuf::from("b.rs"), vec![1]),
            (PathBuf::from("a.rs"), vec![5]),
        ];
        assert_eq!(
            tidy(&chosen),
            [(PathBuf::from("a.rs"), vec![3, 5, 7]), (PathBuf::from("b.rs"), vec![1])]
        );
    }

    #[test]
    fn a_report_says_what_happened_in_a_sentence() {
        let report = Report {
            files: vec![
                (PathBuf::from("a.rs"), Outcome::Changed(40)),
                (PathBuf::from("b.rs"), Outcome::Changed(2)),
                (PathBuf::from("c.rs"), Outcome::Skipped(Skipped::Written)),
            ],
            lines: 42,
        };
        assert_eq!(report.to_string(), "Changed 42 lines in 2 files, skipped 1");
        assert_eq!(report.changed(), 2);
        assert_eq!(report.skipped(), 1);
        assert_eq!(report.failed(), 0);

        let one = Report { files: vec![(PathBuf::from("a.rs"), Outcome::Changed(1))], lines: 1 };
        assert_eq!(one.to_string(), "Changed 1 line in 1 file");

        let none = Report::default();
        assert_eq!(none.to_string(), "Changed 0 lines in 0 files");
    }
}
