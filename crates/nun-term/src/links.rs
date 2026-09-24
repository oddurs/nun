//! What in a line of output can be followed.
//!
//! Compilers, test runners and linters name places as `path:line:column`, and
//! that is what this looks for first: `src/main.rs:42:8`, `./lib.rs:3`, a
//! bare `Cargo.toml`, or a path inside quotes, as Python's tracebacks write
//! it (`File "app.py", line 12`). URLs are found too. Nothing here touches the
//! disk: whether a path names a file is for whoever follows it to find out,
//! relative to wherever the terminal is.
//!
//! Positions are in characters of the line as given, not bytes and not
//! columns; the emulator says which column each character is in.

/// Where a link goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A file, and perhaps a place in it. Line and column count from one, as
    /// the tools that print them do.
    File {
        /// The path as written.
        path: String,
        /// The line.
        line: Option<u32>,
        /// The column.
        column: Option<u32>,
    },
    /// A web address.
    Url(String),
}

/// A link found in a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The first character of it.
    pub start: usize,
    /// One past its last character.
    pub end: usize,
    /// Where it goes.
    pub target: Target,
}

/// The link covering character `at` of `line`, if one does.
#[must_use]
pub fn at(line: &str, at: usize) -> Option<Found> {
    find(line).into_iter().find(|found| found.start <= at && at < found.end)
}

/// Every link in `line`, left to right.
#[must_use]
pub fn find(line: &str) -> Vec<Found> {
    let chars: Vec<char> = line.chars().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '"' || ch == '\'' || ch == '`' {
            if let Some((link, next)) = quoted(&chars, index) {
                found.push(link);
                index = next;
                continue;
            }
            index += 1;
            continue;
        }
        if !is_word(ch) {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && is_word(chars[index]) {
            index += 1;
        }
        let word: String = chars[start..index].iter().collect();
        if let Some(link) = url(&chars, start).or_else(|| path(&word, start, &chars[index..])) {
            index = index.max(link.end);
            found.push(link);
        }
    }
    found
}

/// Characters a path or a position can be made of. Spaces are not among
/// them: a path with a space in it is found only in quotes.
fn is_word(ch: char) -> bool {
    ch.is_alphanumeric()
        || matches!(ch, '/' | '.' | '_' | '-' | '~' | '+' | '@' | ':' | '#' | '%' | '=' | '\\')
}

/// A URL starting at `start`, which runs until whitespace, a quote or an
/// angle bracket, less any punctuation that ends the sentence around it.
fn url(chars: &[char], start: usize) -> Option<Found> {
    let rest: String = chars[start..].iter().take(8).collect();
    if !["http://", "https://", "file://"].iter().any(|scheme| rest.starts_with(scheme)) {
        return None;
    }
    let mut end = start;
    while end < chars.len()
        && !chars[end].is_whitespace()
        && !matches!(chars[end], '"' | '\'' | '<' | '>' | '`')
    {
        end += 1;
    }
    // A closing bracket belongs to the URL only if it opened one.
    loop {
        let text: String = chars[start..end].iter().collect();
        let last = chars[end - 1];
        let unbalanced = |open: char, close: char| {
            last == close && text.matches(open).count() < text.matches(close).count()
        };
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
        {
            end -= 1;
        } else {
            break;
        }
    }
    let text: String = chars[start..end].iter().collect();
    (text.len() > 8).then_some(Found { start, end, target: Target::Url(text) })
}

/// A path at `start`, spelled `word`, perhaps with a line and column after
/// it — as `:12:5`, or as `(12,5)` in `after` the way some compilers write
/// it.
fn path(word: &str, start: usize, after: &[char]) -> Option<Found> {
    // Trailing punctuation is the sentence's, not the path's.
    let word = word.trim_end_matches(['.', ':', ',']);
    let mut parts = word.split(':');
    let path = parts.next()?;
    let numbers: Vec<&str> = parts.collect();
    let mut line = None;
    let mut column = None;
    let mut taken = path.chars().count();
    for (index, number) in numbers.iter().enumerate().take(2) {
        let Ok(value) = number.parse::<u32>() else { break };
        if index == 0 {
            line = Some(value);
        } else {
            column = Some(value);
        }
        taken += 1 + number.chars().count();
    }
    if !looks_like_a_path(path) {
        return None;
    }
    let mut end = start + taken;
    if line.is_none()
        && word.chars().count() == taken
        && let Some((l, c, used)) = parenthesised(after)
    {
        line = Some(l);
        column = c;
        end += used;
    }
    Some(Found { start, end, target: Target::File { path: path.to_string(), line, column } })
}

/// `(12,5)` or `(12)` at the start of `after`, and how many characters it
/// takes.
fn parenthesised(after: &[char]) -> Option<(u32, Option<u32>, usize)> {
    if after.first() != Some(&'(') {
        return None;
    }
    let close = after.iter().position(|ch| *ch == ')')?;
    let inside: String = after[1..close].iter().collect();
    let mut numbers = inside.split(',').map(str::trim);
    let line = numbers.next()?.parse().ok()?;
    let column = match numbers.next() {
        Some(column) => Some(column.parse().ok()?),
        None => None,
    };
    numbers.next().is_none().then_some((line, column, close + 1))
}

/// Whether `text` could name a file: it has a directory in it, or an
/// extension, and is not a number, a version, or punctuation.
fn looks_like_a_path(text: &str) -> bool {
    if text.is_empty() || text.contains("://") || text.chars().all(|ch| !ch.is_alphabetic()) {
        return false;
    }
    let name = text.rsplit(['/', '\\']).next().unwrap_or(text);
    let extension = name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && !ext.is_empty()
            && ext.chars().all(char::is_alphanumeric)
            && ext.chars().any(char::is_alphabetic)
    });
    let directory = text.contains('/')
        && !text.starts_with("//")
        && text.trim_matches('/').contains(char::is_alphanumeric);
    extension || directory
}

/// A path in quotes starting at `open`, and where scanning should carry on.
/// A space is allowed inside; a line number follows as Python writes it.
fn quoted(chars: &[char], open: usize) -> Option<(Found, usize)> {
    let quote = chars[open];
    let close = open + 1 + chars[open + 1..].iter().position(|ch| *ch == quote)?;
    let inside: String = chars[open + 1..close].iter().collect();
    let found = path(&inside, open + 1, &chars[close + 1..])?;
    let Target::File { path: name, mut line, column } = found.target else { return None };
    // A quoted path is the whole of what is quoted, or it is not one.
    if found.end != close && line.is_none() {
        return None;
    }
    let rest: String = chars[close + 1..].iter().take(24).collect();
    let mut end = found.end;
    if line.is_none()
        && let Some(after) = rest.strip_prefix(", line ")
    {
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(number) = digits.parse() {
            line = Some(number);
            end = close + 1 + ", line ".len() + digits.len();
        }
    }
    let end = end.max(close + 1);
    Some((Found { start: open + 1, end, target: Target::File { path: name, line, column } }, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, line: Option<u32>, column: Option<u32>) -> Target {
        Target::File { path: path.into(), line, column }
    }

    fn targets(line: &str) -> Vec<Target> {
        find(line).into_iter().map(|found| found.target).collect()
    }

    #[test]
    fn a_path_with_a_line_and_column() {
        let line = "  --> src/main.rs:42:8";
        let found = find(line);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].target, file("src/main.rs", Some(42), Some(8)));
        assert_eq!(found[0].start, 6);
        assert_eq!(found[0].end, line.chars().count());
    }

    #[test]
    fn a_path_with_a_line_only() {
        assert_eq!(targets("error at lib.rs:7: oops"), [file("lib.rs", Some(7), None)]);
    }

    #[test]
    fn with_and_without_a_leading_dot_slash() {
        assert_eq!(targets("./src/lib.rs:3:1"), [file("./src/lib.rs", Some(3), Some(1))]);
        assert_eq!(targets("src/lib.rs:3:1"), [file("src/lib.rs", Some(3), Some(1))]);
        assert_eq!(targets("../up/there.txt"), [file("../up/there.txt", None, None)]);
    }

    #[test]
    fn inside_quotes() {
        assert_eq!(
            targets(r#"  File "src/app.py", line 12, in main"#),
            [file("src/app.py", Some(12), None)]
        );
        assert_eq!(targets("see 'notes/to do.md' now"), [file("notes/to do.md", None, None)]);
        assert_eq!(
            targets(r#"cannot open "Cargo.toml:4:2""#),
            [file("Cargo.toml", Some(4), Some(2))]
        );
        assert_eq!(targets(r#"say "hello there" twice"#), [], "quoted prose is not a path");
    }

    #[test]
    fn the_end_of_a_sentence_is_not_part_of_the_path() {
        assert_eq!(targets("Wrote out/report.html."), [file("out/report.html", None, None)]);
        assert_eq!(targets("see main.rs:10."), [file("main.rs", Some(10), None)]);
    }

    #[test]
    fn a_position_in_parentheses() {
        assert_eq!(
            targets("src/app.ts(12,5): error TS2322"),
            [file("src/app.ts", Some(12), Some(5))]
        );
    }

    #[test]
    fn numbers_versions_and_prose_are_not_paths() {
        assert_eq!(targets("took 3.14 seconds, 12:30:01"), []);
        assert_eq!(targets("error: aborting due to 2 previous errors"), []);
        assert_eq!(targets("version 1.2.3"), []);
        assert_eq!(targets("a / b"), []);
    }

    #[test]
    fn urls_and_their_brackets() {
        assert_eq!(
            targets("see https://example.com/a?b=c."),
            [Target::Url("https://example.com/a?b=c".into())]
        );
        assert_eq!(
            targets("(https://en.wikipedia.org/wiki/Rust_(programming_language))"),
            [Target::Url("https://en.wikipedia.org/wiki/Rust_(programming_language)".into())]
        );
    }

    #[test]
    fn wide_and_combined_characters_count_as_one_each() {
        let line = "日本 src/é.rs:1";
        let found = find(line);
        assert_eq!(found[0].target, file("src/é.rs", Some(1), None));
        assert_eq!(found[0].start, 3);
        assert_eq!(found[0].end, line.chars().count());
    }

    #[test]
    fn at_finds_the_link_under_a_character() {
        let line = "a.rs:1 and b.rs:2";
        assert_eq!(at(line, 0).map(|f| f.target), Some(file("a.rs", Some(1), None)));
        assert_eq!(at(line, 6), None, "the space between");
        assert_eq!(at(line, 16).map(|f| f.target), Some(file("b.rs", Some(2), None)));
    }

    proptest::proptest! {
        #[test]
        fn finding_never_panics_and_spans_are_in_bounds(line in "\\PC{0,60}") {
            let count = line.chars().count();
            for found in find(&line) {
                proptest::prop_assert!(found.start < found.end && found.end <= count, "{found:?}");
            }
        }
    }
}
