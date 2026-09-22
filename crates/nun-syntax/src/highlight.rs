//! Parsing a document and turning its tree into highlight runs.
//!
//! Two things matter here beyond getting the colours right.
//!
//! **Parsing is incremental.** The previous tree and the edit that changed it
//! go in, so a keystroke in a ten-thousand-line file costs the nodes around
//! the caret rather than the file.
//!
//! **Nothing here can take the editor down.** A grammar is C code: it can
//! panic and it can take unreasonably long on pathological input. Parsing runs
//! under a deadline and inside `catch_unwind`, and a language that breaks
//! either rule is switched off for that document rather than being allowed to
//! stop the session.

use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use ropey::Rope;
use tree_sitter::{InputEdit, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

use crate::language::{self, Language};

/// How long a single parse may take before it is abandoned.
///
/// Generous next to a frame — a first parse of a large file is allowed to be
/// slower than a keystroke — but bounded, because a grammar that has gone
/// quadratic must not hold the worker forever.
pub const PARSE_BUDGET: Duration = Duration::from_millis(250);

/// How far either side of the visible window highlights are computed, so
/// scrolling a little does not need a new answer.
///
/// A screenful of dense code is a couple of kilobytes, so this is a few
/// screens either way. Much more and the query — not the parse — becomes the
/// slow part: every capture in the margin is found and resolved for nothing.
const MARGIN_BYTES: usize = 4 * 1024;

/// A run of characters that share one capture.
///
/// Ranges are char offsets, never bytes: everything above this crate counts in
/// chars, and converting once here is cheaper than converting on every cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// First char.
    pub start: u32,
    /// One past the last char.
    pub end: u32,
    /// The capture name, such as `function.method` or `string`.
    pub capture: &'static str,
}

/// What went wrong with a language, when something did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trouble {
    /// The grammar took longer than [`PARSE_BUDGET`].
    TooSlow,
    /// The grammar panicked.
    Panicked,
}

impl std::fmt::Display for Trouble {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooSlow => f.write_str("took too long"),
            Self::Panicked => f.write_str("crashed"),
        }
    }
}

/// One document's parse state.
pub struct Document {
    language: &'static Language,
    parser: Parser,
    tree: Option<Tree>,
    /// Whether the tree still describes the text: an edited tree is told where
    /// the change was, but it is not a parse until it has been through the
    /// parser again.
    stale: bool,
    text: Rope,
    /// Set once the grammar has misbehaved; nothing is parsed after that.
    trouble: Option<Trouble>,
    /// How long a parse may take. Settable so the giving-up path can be
    /// exercised without having to find input that really does hang.
    budget: Duration,
}

impl std::fmt::Debug for Document {
    /// By hand because a parser, a tree and a rope printed in full are pages
    /// of noise; what anyone debugging wants is which language it is and
    /// whether it parsed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("language", &self.language.name)
            .field("parsed", &self.tree.is_some())
            .field("stale", &self.stale)
            .field("chars", &self.text.len_chars())
            .field("trouble", &self.trouble)
            .finish_non_exhaustive()
    }
}

/// An edit, in char offsets, as nun-core counts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEdit {
    /// Where the change starts.
    pub start: u32,
    /// Where it ended before.
    pub old_end: u32,
    /// Where it ends now.
    pub new_end: u32,
}

impl Document {
    /// A document in `language`, holding `text`.
    #[must_use]
    pub fn new(language: &'static Language, text: Rope) -> Self {
        let mut parser = Parser::new();
        let trouble = parser.set_language(&language.grammar).err().map(|_| Trouble::Panicked);
        Self { language, parser, tree: None, stale: true, text, trouble, budget: PARSE_BUDGET }
    }

    /// Give this document a different parse budget.
    #[must_use]
    pub const fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Which language this is.
    #[must_use]
    pub const fn language(&self) -> &'static Language {
        self.language
    }

    /// Why this document is not being highlighted, if it is not.
    #[must_use]
    pub const fn trouble(&self) -> Option<&Trouble> {
        self.trouble.as_ref()
    }

    /// Take a new version of the text.
    ///
    /// `edit` describes the one change that made it, when there was exactly
    /// one; anything else — a multi-caret revision, an undo, a file reloaded
    /// from disk — parses afresh. Tracking several edits through one reparse
    /// would mean replaying them in the coordinates each left behind, and
    /// getting that wrong silently corrupts the tree.
    pub fn update(&mut self, text: Rope, edit: Option<TextEdit>) {
        if let Some(edit) = edit
            && let Some(tree) = self.tree.as_mut()
            && let Some(input) = input_edit(&self.text, &text, edit)
        {
            // The tree is told where the change was, and goes back through the
            // parser as the starting point for the next parse — which is what
            // makes the reparse cost the change rather than the file.
            tree.edit(&input);
        } else {
            self.tree = None;
        }
        self.stale = true;
        self.text = text;
    }

    /// Parse, and give back the highlight runs covering `range` (char offsets).
    ///
    /// Returns `None` when the document's language is switched off.
    pub fn highlights(&mut self, range: std::ops::Range<u32>) -> Option<Vec<Span>> {
        if self.trouble.is_some() {
            return None;
        }
        if let Err(trouble) = self.parse() {
            self.trouble = Some(trouble);
            self.tree = None;
            return None;
        }
        let tree = self.tree.clone()?;

        let start = self.text.char_to_byte(range.start.min(len_chars(&self.text)) as usize);
        let end = self.text.char_to_byte(range.end.min(len_chars(&self.text)) as usize);
        let window = start.saturating_sub(MARGIN_BYTES)..end.saturating_add(MARGIN_BYTES);

        let mut spans = Vec::new();
        collect(&mut spans, self.language, tree.root_node(), &self.text, &window, 0);
        // Injected languages are parsed against their own slice of the text,
        // so their spans arrive already offset into it.
        if let Some(injections) = self.language.injections.as_ref() {
            self.inject(&mut spans, injections, &tree, &window);
        }
        Some(flatten(spans, &self.text))
    }

    /// Parse, and give back what the file declares, in the order it declares
    /// it.
    ///
    /// Returns `None` when the document's language is switched off. A language
    /// nun has no tags query for gives an empty outline rather than `None`:
    /// there is nothing wrong, there is just nothing to list.
    pub fn symbols(&mut self) -> Option<Vec<crate::Symbol>> {
        if self.trouble.is_some() {
            return None;
        }
        if let Err(trouble) = self.parse() {
            self.trouble = Some(trouble);
            self.tree = None;
            return None;
        }
        let tree = self.tree.clone()?;
        Some(crate::symbols::of_tree(self.language, &tree, &self.text))
    }

    /// Grow each char range to the smallest named node that covers strictly
    /// more of the text, which is what "select the enclosing thing" means.
    ///
    /// An empty range — a caret — grows to the smallest named node around it:
    /// the identifier it is in before the call that identifier is in. A range
    /// that already covers the whole file stays as it is. Nodes the grammar
    /// leaves unnamed are stepped over, because they are punctuation and
    /// keywords: selecting a lone `(` is never what anyone meant.
    ///
    /// Returns `None` when the document's language is switched off.
    pub fn grow(&mut self, ranges: &[std::ops::Range<u32>]) -> Option<Vec<std::ops::Range<u32>>> {
        if self.trouble.is_some() {
            return None;
        }
        if let Err(trouble) = self.parse() {
            self.trouble = Some(trouble);
            self.tree = None;
            return None;
        }
        let tree = self.tree.clone()?;
        let root = tree.root_node();
        let text = &self.text;
        let chars = len_chars(text);
        let grown = ranges
            .iter()
            .map(|range| {
                let (from, to) = (range.start.min(chars), range.end.min(chars));
                let (start, end) =
                    (text.char_to_byte(from as usize), text.char_to_byte(to as usize));
                let mut node = root.named_descendant_for_byte_range(start, end).unwrap_or(root);
                // Up until the node covers more than the range does. An empty
                // range is covered strictly by anything that is not empty.
                while !(node.is_named()
                    && node.start_byte() <= start
                    && node.end_byte() >= end
                    && node.end_byte() - node.start_byte() > end - start)
                {
                    let Some(parent) = node.parent() else { return from..to };
                    node = parent;
                }
                let char_of = |byte: usize| {
                    u32::try_from(text.byte_to_char(byte.min(text.len_bytes()))).unwrap_or(chars)
                };
                char_of(node.start_byte())..char_of(node.end_byte())
            })
            .collect();
        Some(grown)
    }

    /// The regions of the file that can be folded away, as line ranges: the
    /// header line that stays in view, and the last line hidden under it.
    ///
    /// A region is a named node that spans more than one line. Several can
    /// start on one line, and the one taken is what the line *opens*: the
    /// node starting furthest to the right, which is the `{` at the end of
    /// `if x {` rather than the whole `if … else` statement around it, and
    /// the argument list of `call(` rather than the statement holding the
    /// call. A region that would hide the header of the next one (`} else {`)
    /// stops a line short, so folding the `if` leaves the `else` in view. The
    /// whole file is never a region.
    ///
    /// Returns `None` when the document's language is switched off.
    pub fn folds(&mut self) -> Option<Vec<FoldRange>> {
        if self.trouble.is_some() {
            return None;
        }
        if let Err(trouble) = self.parse() {
            self.trouble = Some(trouble);
            self.tree = None;
            return None;
        }
        let tree = self.tree.clone()?;

        // For each line, the region it opens: the start column and last line
        // of the node starting furthest right on it, the longest on a tie.
        let mut starts: std::collections::BTreeMap<usize, (usize, usize)> =
            std::collections::BTreeMap::new();
        let mut cursor = tree.walk();
        let mut descend = cursor.goto_first_child();
        while descend || cursor.goto_next_sibling() || climb(&mut cursor) {
            let node = cursor.node();
            let (first, last) = (node.start_position().row, last_row(node));
            let spans = last > first;
            if spans && node.is_named() && !node.is_error() && !opens_on_a_child(node) {
                let last = self.stop_before_a_clause(node, first, last);
                if last > first {
                    let opens = (node.start_position().column, last);
                    let kept = starts.entry(first).or_insert(opens);
                    *kept = (*kept).max(opens);
                }
            }
            // A node on one line has nothing below it that spans lines.
            descend = spans && cursor.goto_first_child();
        }

        let headers: Vec<usize> = starts.keys().copied().collect();
        let folds = starts
            .iter()
            .filter_map(|(&header, &(_, last))| {
                let last = if headers.binary_search(&last).is_ok() {
                    last.saturating_sub(1)
                } else {
                    last
                };
                (last > header).then(|| FoldRange {
                    header: u32::try_from(header).unwrap_or(u32::MAX),
                    last: u32::try_from(last).unwrap_or(u32::MAX),
                })
            })
            .collect();
        Some(folds)
    }

    /// Where a region opened by `node` on line `first` should end, at the
    /// latest `last`: before the first clause that starts back at the header
    /// line's own indentation after an indented body.
    ///
    /// That is how a language without braces says a construct has moved on
    /// — Python's `else:`, `elif:`, `except:` and `finally:` are children of
    /// the `if` or `try` they belong to, and without this folding the `if`
    /// would fold its `else` away with it.
    fn stop_before_a_clause(&self, node: tree_sitter::Node, first: usize, last: usize) -> usize {
        let header = self.text.line(first);
        let indent = header.chars().take_while(|ch| matches!(ch, ' ' | '\t')).count();
        // Only once an indented body has been seen: a TOML table's pairs sit
        // at its header's own indentation and are its contents, not clauses.
        let mut cursor = node.walk();
        let mut body = false;
        for child in node.named_children(&mut cursor) {
            let start = child.start_position();
            if start.row <= first {
                continue;
            }
            if start.column > indent {
                body = true;
            } else if body {
                return last.min(start.row - 1);
            }
        }
        last
    }

    /// Parse the current text, reusing the previous tree where there is one.
    fn parse(&mut self) -> Result<(), Trouble> {
        if !self.stale && self.tree.is_some() {
            return Ok(());
        }
        let deadline = Instant::now() + self.budget;
        let text = &self.text;
        let parser = &mut self.parser;
        let old = self.tree.as_ref();

        // A grammar is C code reached through FFI: it can panic, and it can
        // run away on input it was not built for. Both are contained here.
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            let mut over_budget = |_: &tree_sitter::ParseState| {
                if Instant::now() > deadline {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let options = tree_sitter::ParseOptions::new().progress_callback(&mut over_budget);
            parser.parse_with_options(
                &mut |byte, _| {
                    let (chunk, chunk_byte, _, _) = text.chunk_at_byte(byte.min(text.len_bytes()));
                    chunk.get(byte - chunk_byte..).unwrap_or("").as_bytes()
                },
                old,
                Some(options),
            )
        }))
        .map_err(|_| Trouble::Panicked)?;

        match parsed {
            Some(tree) => {
                self.tree = Some(tree);
                self.stale = false;
                Ok(())
            }
            // `parse_with_options` gives nothing back when the progress
            // callback stopped it, which here means the deadline passed.
            None => Err(Trouble::TooSlow),
        }
    }

    /// Parse each embedded language and add its spans.
    fn inject(
        &self,
        spans: &mut Vec<Raw>,
        injections: &Query,
        tree: &Tree,
        window: &std::ops::Range<usize>,
    ) {
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(window.clone());
        let mut matches = cursor.matches(injections, tree.root_node(), RopeText(&self.text));

        while let Some(found) = matches.next() {
            let mut language = None;
            let mut content = None;
            for capture in found.captures() {
                match injections.capture_names()[capture.index as usize] {
                    "injection.language" => {
                        language =
                            Some(self.text.byte_slice(capture.node.byte_range()).to_string());
                    }
                    "injection.content" => content = Some(capture.node),
                    _ => {}
                }
            }
            // A query may name the language in a property instead of a
            // capture: `(#set! injection.language "css")`.
            let named = injections
                .property_settings(found.pattern_index)
                .iter()
                .find(|property| &*property.key == "injection.language")
                .and_then(|property| property.value.as_deref());

            let named = named.map(ToString::to_string);
            let Some(inner) = language.or(named).as_deref().and_then(language::of_name) else {
                continue;
            };
            let Some(content) = content else { continue };
            // One level deep. Embedded code embedding further code is rare
            // enough that the cost of the recursion is not worth its risk.
            let text = self.text.byte_slice(content.byte_range()).to_string();
            let mut parser = Parser::new();
            if parser.set_language(&inner.grammar).is_err() {
                continue;
            }
            let Ok(Some(subtree)) = catch_unwind(AssertUnwindSafe(|| parser.parse(&text, None)))
            else {
                continue;
            };
            let offset = content.start_byte();
            let inner_window = 0..text.len();
            let inner_text = Rope::from_str(&text);
            collect(spans, inner, subtree.root_node(), &inner_text, &inner_window, offset);
        }
    }
}

/// A region that can be folded: its header line, which stays in view, and
/// the last line folding it hides. Lines count from zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FoldRange {
    /// The line that stays in view.
    pub header: u32,
    /// The last line hidden under it.
    pub last: u32,
}

/// The last line `node` really reaches. A node ending at the very start of a
/// line — TOML's tables take their trailing newline with them — ends on the
/// line before.
fn last_row(node: tree_sitter::Node) -> usize {
    let end = node.end_position();
    if end.column == 0 && end.row > node.start_position().row { end.row - 1 } else { end.row }
}

/// Whether `node` starts exactly where its first named child does: a bare
/// container with no delimiter of its own, such as Python's `block`, which
/// begins with its first statement. Such a node does not open a region on
/// that line — its first statement is not a header — and the construct
/// around it, whose header is the line above, folds it instead.
fn opens_on_a_child(node: tree_sitter::Node) -> bool {
    node.named_child(0).is_some_and(|child| child.start_byte() == node.start_byte())
}

/// Step the cursor up to the next unvisited sibling of an ancestor, or report
/// that the walk is over.
fn climb(cursor: &mut tree_sitter::TreeCursor) -> bool {
    while cursor.goto_parent() {
        if cursor.goto_next_sibling() {
            return true;
        }
    }
    false
}

/// A span before overlaps are resolved, in bytes.
#[derive(Debug, Clone)]
pub(crate) struct Raw {
    start: usize,
    end: usize,
    capture: &'static str,
    /// How deeply injected: an injected language's spans sit inside the host's.
    depth: u8,
}

/// Run `language`'s highlight query over `node` and collect what it captures.
fn collect(
    spans: &mut Vec<Raw>,
    language: &'static Language,
    node: tree_sitter::Node,
    text: &Rope,
    window: &std::ops::Range<usize>,
    offset: usize,
) {
    let names = language.highlights.capture_names();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(window.clone());
    let mut matches = cursor.matches(&language.highlights, node, RopeText(text));

    let depth = u8::from(offset > 0);
    while let Some(found) = matches.next() {
        for capture in found.captures() {
            let range = capture.node.byte_range();
            spans.push(Raw {
                start: range.start + offset,
                end: range.end + offset,
                capture: names[capture.index as usize],
                depth,
            });
        }
    }
}

/// Resolve overlapping captures into runs that do not overlap, in char
/// offsets.
///
/// Tree-sitter hands back a capture per matching node, and those nest: the
/// whole call expression, then the function name inside it. The innermost —
/// and, where something is injected, the deepest — wins for each character,
/// which is what every other editor shows.
///
/// A sweep rather than a search: the spans are walked once in start order
/// while a small set of the ones covering the current position is kept. A
/// large file produces tens of thousands of captures, and looking each
/// boundary up against all of them turned a parse into half a minute.
fn flatten(mut spans: Vec<Raw>, text: &Rope) -> Vec<Span> {
    spans.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));

    let mut boundaries: Vec<usize> = Vec::with_capacity(spans.len() * 2);
    for span in &spans {
        boundaries.push(span.start);
        boundaries.push(span.end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut runs: Vec<Span> = Vec::new();
    let mut active: Vec<&Raw> = Vec::new();
    let mut next = 0;

    for pair in boundaries.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        while next < spans.len() && spans[next].start <= start {
            active.push(&spans[next]);
            next += 1;
        }
        active.retain(|span| span.end > start);

        // The winner here: deepest first, then narrowest.
        let Some(winner) = active
            .iter()
            .filter(|span| span.end >= end)
            .max_by_key(|span| (span.depth, usize::MAX - (span.end - span.start)))
        else {
            continue;
        };
        let capture = winner.capture;

        let start = u32::try_from(text.byte_to_char(start.min(text.len_bytes()))).unwrap_or(0);
        let end = u32::try_from(text.byte_to_char(end.min(text.len_bytes()))).unwrap_or(0);
        if start >= end {
            continue;
        }
        match runs.last_mut() {
            // Neighbouring stretches of the same capture read as one run.
            Some(last) if last.end == start && last.capture == capture => last.end = end,
            _ => runs.push(Span { start, end, capture }),
        }
    }
    runs
}

/// Lets a query read straight from the rope.
///
/// The alternative is handing tree-sitter the whole file as one string on
/// every keystroke, which is a copy of the document per frame for no reason.
pub(crate) struct RopeText<'a>(pub(crate) &'a Rope);

impl<'a> tree_sitter::TextProvider<&'a [u8]> for RopeText<'a> {
    type I = std::iter::Map<ropey::iter::Chunks<'a>, fn(&'a str) -> &'a [u8]>;

    fn text(&mut self, node: tree_sitter::Node) -> Self::I {
        let range = node.byte_range();
        let end = range.end.min(self.0.len_bytes());
        let start = range.start.min(end);
        self.0.byte_slice(start..end).chunks().map(str::as_bytes)
    }
}

/// Turn a char-offset edit into the byte-and-point edit tree-sitter wants.
fn input_edit(before: &Rope, after: &Rope, edit: TextEdit) -> Option<InputEdit> {
    let start = edit.start as usize;
    let old_end = edit.old_end as usize;
    let new_end = edit.new_end as usize;
    if start > len_chars(before) as usize
        || old_end > len_chars(before) as usize
        || new_end > len_chars(after) as usize
        || start > old_end
        || start > new_end
    {
        return None;
    }
    Some(InputEdit {
        start_byte: before.char_to_byte(start),
        old_end_byte: before.char_to_byte(old_end),
        new_end_byte: after.char_to_byte(new_end),
        start_position: point_of(before, start),
        old_end_position: point_of(before, old_end),
        new_end_position: point_of(after, new_end),
    })
}

/// A char offset as a tree-sitter point: row, and byte column within the row.
fn point_of(text: &Rope, char_offset: usize) -> Point {
    let row = text.char_to_line(char_offset);
    let line_start = text.line_to_byte(row);
    Point::new(row, text.char_to_byte(char_offset) - line_start)
}

fn len_chars(text: &Rope) -> u32 {
    u32::try_from(text.len_chars()).unwrap_or(u32::MAX)
}
