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
//! **What was previewed has to still be there.** The caller names each line by
//! its number *and* by the text the search recorded for it, and a line is
//! rewritten only if it is still the line that text came from. Asking instead
//! whether the line still matches the query would be asking the question the
//! search has already answered: it passes by construction in the case worth
//! fearing, where a checkout or a format-on-save leaves line 42 matching while
//! making it a different line 42. The file's modification time is checked too,
//! but only as a cheap early-out — it cannot be leant on, because it is
//! measured in whole seconds on filesystems people really use. A file failing
//! either check is left alone and said so in the [`Report`] rather than being
//! rewritten quietly.

use std::cell::RefCell;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use grep_matcher::{Captures as _, Matcher as _};
use grep_regex::{RegexCaptures, RegexMatcher};

use crate::grep::{Hit, Options, build_matcher};

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
    /// A line that was chosen is no longer there, or is no longer the line
    /// that was previewed.
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
/// `lines` are the chosen lines with what the search recorded of each, sorted
/// by line number and without repeats.
///
/// Two checks stand between the preview and the write, and either one leaves
/// the file untouched.
///
/// The first is `searched_at`: a file whose modification time is newer than
/// the moment the search *started* has been written to since, so nothing on
/// screen for it can be trusted. Taking the search's *start* rather than its
/// end errs towards skipping a file written while the walk was still running,
/// which is the safe direction. It is a cheap early-out that needs no read,
/// but it cannot be relied on alone: `searched_at` has nanosecond resolution
/// and a file's modification time has one second on HFS+ and many network
/// mounts, two on FAT, so a write lands unseen whenever it falls in the rest
/// of the clock second the search started in. Widening the comparison to cover
/// that would skip any file edited in the two seconds before a search, which
/// is a thing people do constantly and would be reported to them as a lie.
///
/// The second closes that window, and is the one that means something: every
/// chosen line must still be *the line that was previewed* — see
/// [`Recorded::still_there`]. Not "does it still match the query", which is
/// the same question the search already answered and passes by construction in
/// exactly the case worth fearing, where a checkout or a format-on-save leaves
/// line 42 matching while making it a different line 42.
///
/// A file failing either check fails as a whole. Applying half of a preview
/// would be worse than applying none of it.
pub(crate) fn plan(
    path: &Path,
    replacer: &Replacer,
    lines: &[Recorded],
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
    for recorded in lines {
        let Some(index) = (recorded.line() as usize).checked_sub(1) else {
            return Plan::Leave(Outcome::Skipped(Skipped::Moved));
        };
        let Some(&(content, _)) = split.get(index) else {
            return Plan::Leave(Outcome::Skipped(Skipped::Moved));
        };
        if !recorded.still_there(content) {
            return Plan::Leave(Outcome::Skipped(Skipped::Moved));
        }
        // Redundant beside the check above — identical text matches the same
        // query — and kept anyway, because it is the invariant rather than an
        // inference from one: nothing is written over a line this query does
        // not match, whatever else has gone wrong upstream.
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

/// One line the person chose, and what the search recorded of it.
///
/// Build it from the hit the panel is showing, with [`Recorded::of`]. That is
/// deliberately the only convenient way to get one: whether a hit carries a
/// whole line or a window onto it is something the *search* knows and nothing
/// downstream can recover from the text, since a window is not always
/// [`MOST_CHARS`](crate::MOST_CHARS) characters long.
///
/// Comparing a window against a whole line is the mistake this type exists to
/// make unavailable. Done by equality it would turn every hit on a long line
/// into [`Skipped::Moved`] and report that the file had changed when it had
/// not; done by substring it would accept `cat` as the preview of
/// `if (cat) { return cat; }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// The whole line, because the search read all of it.
    Whole {
        /// Which line, counting from one, as [`Hit::line`](crate::Hit::line).
        line: u32,
        /// The line, with its terminator removed.
        text: String,
    },
    /// A window onto a line too long for a hit to carry whole.
    Window {
        /// Which line, counting from one.
        line: u32,
        /// The part of it the search recorded.
        text: String,
    },
}

impl Recorded {
    /// What `hit` recorded of the line it is on.
    #[must_use]
    pub fn of(hit: &Hit) -> Self {
        let (line, text) = (hit.line, hit.text.clone());
        if hit.whole { Self::Whole { line, text } } else { Self::Window { line, text } }
    }

    /// Which line this was recorded from, counting from one.
    #[must_use]
    pub const fn line(&self) -> u32 {
        match self {
            Self::Whole { line, .. } | Self::Window { line, .. } => *line,
        }
    }

    /// Whether `current` is still the line this was recorded from.
    ///
    /// A whole line has to be the same text. Nothing weaker will do: accepting
    /// a line that merely contains it would accept `cat` as the preview of
    /// `if (cat) { return cat; }`, a line the person never saw.
    ///
    /// A window only has to still be somewhere in the line, because being
    /// identical was never on offer — the search read a thousand characters of
    /// a line that may be a megabyte long, and that is all anyone ever had.
    /// Two alternatives were weighed and are worse.
    ///
    /// Falling back to the timestamp and "does it still match the query" is
    /// the check this one replaced, for being unable to answer the question
    /// being asked. Reaching for it on precisely the lines that are hardest to
    /// verify is where it is least defensible, not most.
    ///
    /// Requiring the window at the *same character offset* is tempting and
    /// buys nothing. It would reject any edit earlier in the line while
    /// forfeiting nothing real, because a windowed line has matches outside
    /// the window that are rewritten without ever having been previewed — that
    /// is inherent to the line being too long to show, and no offset test
    /// changes it. It would also mean carrying the offset, giving the panel
    /// one more thing to get right.
    ///
    /// What is left unguarded is a thousand characters still present verbatim
    /// in a long line that changed around them. No check can rule that out:
    /// the search never saw the rest of that line, so there is nothing else to
    /// compare against.
    #[must_use]
    pub fn still_there(&self, current: &str) -> bool {
        match self {
            Self::Whole { text, .. } => current == text,
            Self::Window { text, .. } => current.contains(text.as_str()),
        }
    }
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

/// One file's chosen lines, sorted by line number, without repeats, and with
/// a path named twice folded into one entry.
///
/// The panel builds its list from rows the user clicked, so it is under no
/// obligation to hand them over in any particular order — and a file
/// rewritten twice in one pass would find its own output on the second go and
/// skip it, which would be a confusing way to learn that.
///
/// A line number given twice keeps the first record offered for it. The two
/// can only differ if the panel is holding two hits for one line, which the
/// search does not produce.
pub(crate) fn tidy(chosen: &[(PathBuf, Vec<Recorded>)]) -> Vec<(PathBuf, Vec<Recorded>)> {
    let mut tidied: Vec<(PathBuf, Vec<Recorded>)> = Vec::with_capacity(chosen.len());
    for (path, lines) in chosen {
        match tidied.iter_mut().find(|(seen, _)| seen == path) {
            Some((_, seen)) => seen.extend_from_slice(lines),
            None => tidied.push((path.clone(), lines.clone())),
        }
    }
    for (_, lines) in &mut tidied {
        lines.sort_by_key(Recorded::line);
        lines.dedup_by_key(|recorded| recorded.line());
    }
    tidied
}

/// `n` characters that repeat nothing, so a window of them appears in a line
/// exactly once.
///
/// Shared with [`crate::ops`]'s tests. Filler of one repeated character will
/// not do for testing a window: a thousand `x` are still found in a line of
/// two thousand with three of them changed, so the test would pass without
/// testing anything.
#[cfg(test)]
pub(crate) fn filler(n: usize) -> String {
    let mut text = String::new();
    for number in 0.. {
        if text.chars().count() >= n {
            break;
        }
        text.push_str(&number.to_string());
        text.push(' ');
    }
    text.chars().take(n).collect()
}

/// `body` with the character at `at` made into one it contains nowhere else,
/// so any window spanning it is gone rather than found again somewhere.
#[cfg(test)]
pub(crate) fn tweak(body: &str, at: usize) -> String {
    let mut chars: Vec<char> = body.chars().collect();
    chars[at] = 'Z';
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grep::{Case, MOST_CHARS};

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

    fn at(line: u32, text: &str) -> Recorded {
        Recorded::Whole { line, text: text.to_owned() }
    }

    fn whole(text: &str) -> Recorded {
        Recorded::Whole { line: 1, text: text.to_owned() }
    }

    fn windowed(text: &str) -> Recorded {
        Recorded::Window { line: 1, text: text.to_owned() }
    }

    #[test]
    fn chosen_lines_are_sorted_and_a_path_named_twice_is_folded_in() {
        let chosen = vec![
            (PathBuf::from("a.rs"), vec![at(7, "g"), at(3, "c"), at(3, "c")]),
            (PathBuf::from("b.rs"), vec![at(1, "a")]),
            (PathBuf::from("a.rs"), vec![at(5, "e")]),
        ];
        assert_eq!(
            tidy(&chosen),
            [
                (PathBuf::from("a.rs"), vec![at(3, "c"), at(5, "e"), at(7, "g")]),
                (PathBuf::from("b.rs"), vec![at(1, "a")]),
            ]
        );
    }

    #[test]
    fn a_whole_line_has_to_be_exactly_what_was_previewed() {
        assert!(whole("let cat = 1;").still_there("let cat = 1;"));
        assert!(!whole("let cat = 1;").still_there("let cat = 2;"));
        assert!(
            !whole("cat").still_there("if (cat) { return cat; }"),
            "a line that merely contains it is a line nobody previewed"
        );
        assert!(!whole("if (cat) { return cat; }").still_there("cat"), "nor the other way");
        assert!(whole("").still_there(""), "an empty line is a line");
        assert!(!whole("").still_there("cat"), "but it is not every line");
    }

    #[test]
    fn a_window_only_has_to_still_be_in_the_line() {
        // Being identical was never on offer for these: the search read a
        // thousand characters of a line that may be far longer.
        let body = format!("{}cat{}", filler(2_000), filler(2_000));
        let window: String = body.chars().skip(1_500).take(MOST_CHARS).collect();
        assert!(window.contains("cat"), "a real window holds the match");

        assert!(windowed(&window).still_there(&body), "still in the line");
        assert!(
            windowed(&window).still_there(&format!("{body} // appended")),
            "an edit elsewhere in the line does not move it"
        );
        assert!(!windowed(&window).still_there(&tweak(&body, 1_800)), "changed through it");
    }

    #[test]
    fn a_window_is_never_compared_against_a_line_as_though_it_were_whole() {
        // The mistake the type exists to prevent, from both directions. By
        // equality a long line would always be reported as changed; by
        // substring a short one would accept a line it is only part of.
        let body = format!("{}cat{}", filler(2_000), filler(2_000));
        let window: String = body.chars().skip(1_500).take(MOST_CHARS).collect();

        assert!(windowed(&window).still_there(&body));
        assert!(!whole(&window).still_there(&body), "as a whole line it would be wrong");
        assert!(whole(&body).still_there(&body), "and the whole line is right");

        // A window can be shorter than the cap — one taken near the end of a
        // line — so its length proves nothing about which it is. This is why
        // the search has to say, and why `Recorded::of` is the way to build
        // one.
        let short: String = body.chars().skip(3_900).collect();
        assert!(short.chars().count() < MOST_CHARS, "{}", short.chars().count());
        assert!(windowed(&short).still_there(&body), "still a window of it");
        assert!(!whole(&short).still_there(&body));
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
