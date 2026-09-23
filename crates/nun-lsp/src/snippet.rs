//! Snippets, as completion items carry them: `for ${1:item} in ${2:iter} {\n\t$0\n}`.
//!
//! [`parse`] turns the protocol's snippet syntax into the text to insert and
//! where its tab-stops fall in it, as char offsets into that text. Nothing
//! here touches a buffer: the editor inserts the text and puts the carets on
//! the stops.
//!
//! What the grammar allows, and what nun makes of it:
//!
//! - `$1`, `${1}` — a tab-stop. The same number twice is one stop in two
//!   places, and both are selected together, so typing edits both.
//! - `${1:default}` — a placeholder: a stop whose text starts out as
//!   `default`, selected when the stop is reached. Placeholders nest, and a
//!   bare `$1` elsewhere mirrors the placeholder's text.
//! - `${1|one,two|}` — a choice. nun has no chooser for it: the first option
//!   is inserted as the stop's placeholder.
//! - `$0` — where the caret ends up. Without one, it ends up after the text.
//! - `$NAME`, `${NAME}`, `${NAME:default}` — a variable, resolved by the
//!   caller. One the caller does not know becomes its default, or nothing.
//! - `${1/regex/format/}`, `${NAME/regex/format/}` — a transform. nun does not
//!   run the regex: a transformed variable is its plain value, and a
//!   transformed stop is its plain text, not a stop of its own.
//!
//! Anything that does not parse is text. A server that sends `$` on its own,
//! or an unclosed `${`, gets exactly what it sent.

use std::collections::BTreeMap;
use std::ops::Range;

/// A snippet ready to insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    /// The text, with every placeholder, choice and variable filled in.
    pub text: String,
    /// The stops in the order Tab visits them: 1, 2, … then 0 last. Never
    /// empty — the final stop is always there.
    pub stops: Vec<Stop>,
}

/// One tab-stop, in every place it appears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    /// Its number; 0 is the final stop.
    pub index: u32,
    /// Char ranges of [`Snippet::text`] it covers, in order. Empty ranges
    /// are plain carets; the rest are placeholder text to select.
    pub ranges: Vec<Range<usize>>,
}

impl Snippet {
    /// Whether it has stops to visit before the final one. A snippet with
    /// none is inserted and the caret put at its end, and that is all.
    #[must_use]
    pub fn has_stops(&self) -> bool {
        self.stops.len() > 1
    }

    /// The final stop: where the caret goes when the snippet is done.
    #[must_use]
    pub fn last(&self) -> &Stop {
        // Never empty: `parse` and `plain` both end with the final stop.
        &self.stops[self.stops.len() - 1]
    }

    /// Text that is not a snippet at all, as one: the text, and the caret
    /// after it.
    #[must_use]
    pub fn plain(text: &str) -> Self {
        let end = text.chars().count();
        Self {
            text: text.to_string(),
            stops: vec![Stop { index: 0, ranges: std::iter::once(end..end).collect() }],
        }
    }
}

/// One piece of a parsed snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Text(String),
    /// A stop. `content` is its placeholder, when it has one; `mirror` marks a
    /// transformed stop, which shows the stop's text but is not a stop.
    Stop {
        index: u32,
        content: Option<Vec<Node>>,
        mirror: bool,
    },
    Variable {
        name: String,
        default: Option<Vec<Node>>,
    },
}

/// Parse `source`, resolving variables with `variable`.
///
/// Never fails: whatever does not parse as a snippet construct is taken as
/// text.
#[must_use]
pub fn parse(source: &str, variable: &dyn Fn(&str) -> Option<String>) -> Snippet {
    let chars: Vec<char> = source.chars().collect();
    let mut parser = Parser { chars: &chars, at: 0 };
    let nodes = parser.any(false);

    // The first placeholder of each stop is what its bare mentions mirror.
    let mut defaults = BTreeMap::new();
    collect_defaults(&nodes, &mut defaults);

    let mut out = Render { text: String::new(), len: 0, stops: BTreeMap::new(), variable };
    out.nodes(&nodes, &defaults, 0);

    let Render { text, len, mut stops, .. } = out;
    let last = stops.remove(&0).unwrap_or_else(|| std::iter::once(len..len).collect());
    let mut ordered: Vec<Stop> =
        stops.into_iter().map(|(index, ranges)| Stop { index, ranges }).collect();
    ordered.push(Stop { index: 0, ranges: last });
    Snippet { text, stops: ordered }
}

fn collect_defaults<'n>(nodes: &'n [Node], defaults: &mut BTreeMap<u32, &'n [Node]>) {
    for node in nodes {
        match node {
            Node::Stop { index, content: Some(content), mirror: false } => {
                defaults.entry(*index).or_insert(content.as_slice());
                collect_defaults(content, defaults);
            }
            Node::Variable { default: Some(default), .. } => collect_defaults(default, defaults),
            _ => {}
        }
    }
}

struct Render<'v> {
    text: String,
    /// Chars in `text`.
    len: usize,
    stops: BTreeMap<u32, Vec<Range<usize>>>,
    variable: &'v dyn Fn(&str) -> Option<String>,
}

/// How deep mirrors may nest inside one another. A placeholder that mentions
/// its own stop — `${1:a$1}` — would otherwise mirror itself forever.
const MOST_DEPTH: usize = 8;

impl Render<'_> {
    fn push(&mut self, text: &str) {
        self.text.push_str(text);
        self.len += text.chars().count();
    }

    fn nodes(&mut self, nodes: &[Node], defaults: &BTreeMap<u32, &[Node]>, depth: usize) {
        for node in nodes {
            match node {
                Node::Text(text) => self.push(text),
                Node::Stop { index, content, mirror } => {
                    let start = self.len;
                    let content = content.as_deref().or_else(|| defaults.get(index).copied());
                    if let Some(content) = content
                        && depth < MOST_DEPTH
                    {
                        self.nodes(content, defaults, depth + 1);
                    }
                    if !mirror {
                        self.stops.entry(*index).or_default().push(start..self.len);
                    }
                }
                Node::Variable { name, default } => match (self.variable)(name) {
                    Some(value) => self.push(&value),
                    None => {
                        if let Some(default) = default {
                            self.nodes(default, defaults, depth);
                        }
                    }
                },
            }
        }
    }
}

struct Parser<'c> {
    chars: &'c [char],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    /// Everything up to the end, or up to an unescaped `}` when `nested`.
    fn any(&mut self, nested: bool) -> Vec<Node> {
        let mut nodes = Vec::new();
        let mut text = String::new();
        while let Some(ch) = self.peek() {
            match ch {
                '}' if nested => break,
                '\\' if matches!(self.peek_at(1), Some('$' | '}' | '\\')) => {
                    text.push(self.peek_at(1).unwrap_or('\\'));
                    self.at += 2;
                }
                '$' => {
                    let from = self.at;
                    if let Some(node) = self.dollar() {
                        if !text.is_empty() {
                            nodes.push(Node::Text(std::mem::take(&mut text)));
                        }
                        nodes.push(node);
                    } else {
                        // Not a construct after all: the `$` is text.
                        self.at = from + 1;
                        text.push('$');
                    }
                }
                _ => {
                    text.push(ch);
                    self.at += 1;
                }
            }
        }
        if !text.is_empty() {
            nodes.push(Node::Text(text));
        }
        nodes
    }

    /// A construct starting at `$`, or `None`, having consumed whatever it
    /// looked at, when there is none.
    fn dollar(&mut self) -> Option<Node> {
        self.at += 1;
        match self.peek()? {
            ch if ch.is_ascii_digit() => {
                Some(Node::Stop { index: self.int()?, content: None, mirror: false })
            }
            ch if is_name_start(ch) => Some(Node::Variable { name: self.name(), default: None }),
            '{' => {
                self.at += 1;
                self.braced()
            }
            _ => None,
        }
    }

    /// What follows `${`.
    fn braced(&mut self) -> Option<Node> {
        let first = self.peek()?;
        if first.is_ascii_digit() {
            let index = self.int()?;
            return match self.peek()? {
                '}' => {
                    self.at += 1;
                    Some(Node::Stop { index, content: None, mirror: false })
                }
                ':' => {
                    self.at += 1;
                    let content = self.any(true);
                    self.close()?;
                    Some(Node::Stop { index, content: Some(content), mirror: false })
                }
                '|' => {
                    self.at += 1;
                    let first = self.choice()?;
                    Some(Node::Stop {
                        index,
                        content: Some(vec![Node::Text(first)]),
                        mirror: false,
                    })
                }
                '/' => {
                    self.transform()?;
                    Some(Node::Stop { index, content: None, mirror: true })
                }
                _ => None,
            };
        }
        if !is_name_start(first) {
            return None;
        }
        let name = self.name();
        match self.peek()? {
            '}' => {
                self.at += 1;
                Some(Node::Variable { name, default: None })
            }
            ':' => {
                self.at += 1;
                let default = self.any(true);
                self.close()?;
                Some(Node::Variable { name, default: Some(default) })
            }
            '/' => {
                self.transform()?;
                Some(Node::Variable { name, default: None })
            }
            _ => None,
        }
    }

    fn close(&mut self) -> Option<()> {
        (self.peek()? == '}').then(|| self.at += 1)
    }

    fn int(&mut self) -> Option<u32> {
        let start = self.at;
        while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            self.at += 1;
        }
        self.chars[start..self.at].iter().collect::<String>().parse().ok()
    }

    fn name(&mut self) -> String {
        let start = self.at;
        while self.peek().is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
            self.at += 1;
        }
        self.chars[start..self.at].iter().collect()
    }

    /// The options of a choice after `|`, through the closing `|}`; the first
    /// of them.
    fn choice(&mut self) -> Option<String> {
        let mut options = vec![String::new()];
        loop {
            match self.peek()? {
                '\\' if matches!(self.peek_at(1), Some(',' | '|' | '\\' | '$' | '}')) => {
                    options.last_mut()?.push(self.peek_at(1)?);
                    self.at += 2;
                }
                ',' => {
                    options.push(String::new());
                    self.at += 1;
                }
                '|' if self.peek_at(1) == Some('}') => {
                    self.at += 2;
                    return options.into_iter().next();
                }
                ch => {
                    options.last_mut()?.push(ch);
                    self.at += 1;
                }
            }
        }
    }

    /// Skip `/regex/format/options}`, starting at the first `/`.
    fn transform(&mut self) -> Option<()> {
        // The format can hold `${1:/upcase}`, whose slash is not the end.
        let mut slashes = 0;
        let mut depth = 0usize;
        while slashes < 3 {
            match self.peek()? {
                '\\' => self.at += 1,
                '{' => depth += 1,
                '}' if depth > 0 => depth -= 1,
                '/' if depth == 0 => slashes += 1,
                _ => {}
            }
            self.at += 1;
        }
        while self.peek()?.is_ascii_alphabetic() {
            self.at += 1;
        }
        self.close()
    }
}

const fn is_name_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    /// Each stop, and the `(start, end)` of every place it is.
    fn stops(snippet: &Snippet) -> Vec<(u32, Vec<(usize, usize)>)> {
        let spans =
            |stop: &Stop| stop.ranges.iter().map(|range| (range.start, range.end)).collect();
        snippet.stops.iter().map(|stop| (stop.index, spans(stop))).collect()
    }

    /// The text each stop's first range covers.
    fn covered(snippet: &Snippet) -> Vec<String> {
        let chars: Vec<char> = snippet.text.chars().collect();
        snippet.stops.iter().map(|stop| chars[stop.ranges[0].clone()].iter().collect()).collect()
    }

    #[test]
    fn plain_text_is_itself_with_the_caret_after_it() {
        let snippet = parse("println", &none);
        assert_eq!(snippet.text, "println");
        assert_eq!(stops(&snippet), vec![(0, vec![(7, 7)])]);
        assert!(!snippet.has_stops());
    }

    #[test]
    fn tab_stops_come_in_number_order_and_zero_comes_last() {
        let snippet = parse("fn $2($1) {\n\t$0\n}", &none);
        assert_eq!(snippet.text, "fn () {\n\t\n}");
        assert_eq!(stops(&snippet), vec![(1, vec![(4, 4)]), (2, vec![(3, 3)]), (0, vec![(9, 9)])]);
        assert!(snippet.has_stops());
    }

    #[test]
    fn placeholders_are_filled_in_and_cover_their_text() {
        let snippet = parse("for ${1:item} in ${2:iter} {$0}", &none);
        assert_eq!(snippet.text, "for item in iter {}");
        assert_eq!(covered(&snippet), vec!["item", "iter", ""]);
    }

    #[test]
    fn placeholders_nest() {
        let snippet = parse("${1:outer ${2:inner} done}", &none);
        assert_eq!(snippet.text, "outer inner done");
        assert_eq!(covered(&snippet), vec!["outer inner done", "inner", ""]);
        assert_eq!(stops(&snippet)[2], (0, vec![(16, 16)]), "no $0: the end");
    }

    #[test]
    fn a_bare_mention_mirrors_the_placeholder() {
        let snippet = parse("let ${1:x} = 1; $1 + $1", &none);
        assert_eq!(snippet.text, "let x = 1; x + x");
        assert_eq!(stops(&snippet)[0], (1, vec![(4, 5), (11, 12), (15, 16)]));
    }

    #[test]
    fn a_choice_falls_back_to_its_first_option() {
        let snippet = parse("${1|public,private\\, really,crate|} fn", &none);
        assert_eq!(snippet.text, "public fn");
        assert_eq!(covered(&snippet)[0], "public");
        let escaped = parse("${1|a\\,b,c|}", &none);
        assert_eq!(escaped.text, "a,b");
    }

    #[test]
    fn variables_are_resolved_or_default_or_empty() {
        let known = |name: &str| (name == "TM_FILENAME").then(|| "main.rs".to_string());
        assert_eq!(parse("// $TM_FILENAME", &known).text, "// main.rs");
        assert_eq!(parse("// ${TM_FILENAME}!", &known).text, "// main.rs!");
        assert_eq!(parse("${UNKNOWN:fallback}", &known).text, "fallback");
        assert_eq!(parse("[$UNKNOWN]", &known).text, "[]");
        assert_eq!(parse("${TM_FILENAME/(.*)/${1:/upcase}/}", &known).text, "main.rs");
    }

    #[test]
    fn a_variable_default_can_hold_a_stop() {
        let snippet = parse("${NOPE:${1:name}}", &none);
        assert_eq!(snippet.text, "name");
        assert_eq!(covered(&snippet)[0], "name");
    }

    #[test]
    fn escapes_are_text() {
        assert_eq!(parse(r"\$1 costs \\ and \}", &none).text, r"$1 costs \ and }");
        assert_eq!(parse(r"a\nb", &none).text, r"a\nb", "an unknown escape is left alone");
    }

    #[test]
    fn what_does_not_parse_is_text() {
        assert_eq!(parse("cost: $", &none).text, "cost: $");
        assert_eq!(parse("${1:unclosed", &none).text, "${1:unclosed");
        assert_eq!(parse("$ {1}", &none).text, "$ {1}");
        assert_eq!(parse("${}", &none).text, "${}");
        assert_eq!(parse("${1|no end", &none).text, "${1|no end");
    }

    #[test]
    fn a_final_stop_with_a_placeholder_selects_it() {
        let snippet = parse("return ${0:value};", &none);
        assert_eq!(snippet.text, "return value;");
        assert_eq!(stops(&snippet), vec![(0, vec![(7, 12)])]);
    }

    #[test]
    fn a_transformed_stop_shows_its_text_but_is_not_visited() {
        let snippet = parse("${1:name} ${1/(.*)/$1/}", &none);
        assert_eq!(snippet.text, "name name");
        assert_eq!(stops(&snippet)[0], (1, vec![(0, 4)]));
    }

    #[test]
    fn a_placeholder_that_mentions_itself_does_not_recurse_forever() {
        let snippet = parse("${1:a$1}", &none);
        assert!(snippet.text.starts_with('a'));
    }

    #[test]
    fn offsets_count_chars_not_bytes() {
        let snippet = parse("日本${1:語}é$0", &none);
        assert_eq!(snippet.text, "日本語é");
        assert_eq!(stops(&snippet), vec![(1, vec![(2, 3)]), (0, vec![(4, 4)])]);
    }

    proptest! {
        #[test]
        fn anything_parses_and_every_range_is_inside_the_text(source in "[a-c${}:|,/\\\\0-2 é]{0,40}") {
            let snippet = parse(&source, &none);
            let len = snippet.text.chars().count();
            prop_assert!(!snippet.stops.is_empty());
            prop_assert_eq!(snippet.last().index, 0);
            for stop in &snippet.stops {
                for range in &stop.ranges {
                    prop_assert!(range.start <= range.end && range.end <= len, "{range:?} of {len}");
                }
            }
        }

        #[test]
        fn text_with_no_specials_is_unchanged(source in "[a-zA-Z0-9 .()é日]{0,40}") {
            prop_assert_eq!(parse(&source, &none).text, source);
        }
    }
}
