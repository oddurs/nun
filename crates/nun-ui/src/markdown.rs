//! Markdown, as a card shows it.
//!
//! Language servers describe symbols in Markdown, and dumped raw it is a
//! wall of backticks and asterisks. This turns it into the card's own
//! material — paragraphs of styled runs — covering what hover text actually
//! uses: headings, emphasis, inline code, fenced code, lists, quotes, links
//! and paragraphs. Anything else comes through as its text.
//!
//! Parsing is `pulldown-cmark`'s. The text is untrusted and `CommonMark` is
//! full of corners — nested emphasis, reference links, entities, escapes —
//! where a small hand-written parser either gets it wrong or goes quadratic
//! on the wrong input. It is linear, fuzzed, and built here with no default
//! features, so it brings one small crate with it.
//!
//! Fenced code is highlighted with the same grammars and the same roles as
//! the editor, so a signature in a card reads the way it does in the file.
//! That happens here, as the card is built, rather than on the parser's
//! thread: what a server puts in a hover is a signature and perhaps an
//! example, a few hundred bytes that parse in microseconds, and a round trip
//! to the worker would cost more than it saves. A card's code is capped in
//! size and in time spent, together rather than block by block, so
//! a server that sends a whole file, or a thousand small blocks, still cannot
//! hold up the editor — past either, code is shown plain.

use std::time::{Duration, Instant};

use nun_theme::Role;
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
use ropey::Rope;

use crate::glyph::{Glyph, Glyphs};
use crate::popover::{Paragraph, Run};

/// The most code, in bytes, one card highlights. Past this it is shown
/// plain, because a hover that long is not a signature.
const MOST_HIGHLIGHTED: usize = 16 * 1024;

/// How long highlighting one card's code may take before the rest is shown
/// plain.
const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(20);

/// How much highlighting one card gets, across all of its code blocks.
#[derive(Debug, Clone, Copy)]
pub struct CodeBudget {
    /// Bytes of code still to be highlighted.
    bytes: usize,
    /// Time still to be spent highlighting.
    time: Duration,
}

impl CodeBudget {
    /// A card's worth.
    #[must_use]
    pub const fn new() -> Self {
        Self { bytes: MOST_HIGHLIGHTED, time: HIGHLIGHT_BUDGET }
    }

    /// Take the bytes highlighting `code` needs, and say how long there is
    /// left to do it in. `None` when there is not enough of either.
    fn take(&mut self, code: &str) -> Option<Duration> {
        if self.time.is_zero() {
            return None;
        }
        self.bytes = self.bytes.checked_sub(code.len())?;
        Some(self.time)
    }

    /// Highlighting took `spent`.
    fn spend(&mut self, spent: Duration) {
        self.time = self.time.saturating_sub(spent);
    }
}

impl Default for CodeBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// How inline code is drawn: apart from prose, and not like a link.
const INLINE_CODE: Role = Role::Type;

/// A card's worth of text: its paragraphs, and where its links go.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Markdown {
    /// The paragraphs. A run's `link` indexes into `links`.
    pub body: Vec<Paragraph>,
    /// Where each link goes, as written.
    pub links: Vec<String>,
}

impl Markdown {
    /// `text` read as Markdown. A code block that does not say its language
    /// is taken to be in `language`, the language of the file the text is
    /// about, when there is one. Code is highlighted out of `budget`, and
    /// bullets, quote bars and task boxes are drawn with `glyphs`.
    #[must_use]
    pub fn parse(
        text: &str,
        language: Option<&str>,
        budget: &mut CodeBudget,
        glyphs: &Glyphs,
    ) -> Self {
        let mut reader = Reader::new(language, budget, glyphs);
        for event in Parser::new_ext(text, Options::empty()) {
            reader.event(event);
        }
        reader.flush();
        reader.out.trim();
        reader.out
    }

    /// `text` as it is, a paragraph per line.
    #[must_use]
    pub fn plain(text: &str) -> Self {
        let mut out = Self {
            body: text.lines().map(|line| vec![Run::new(line, Role::Text)]).collect(),
            links: Vec::new(),
        };
        out.trim();
        out
    }

    /// `code` in `language`, highlighted out of `budget`.
    #[must_use]
    pub fn code(language: &str, code: &str, budget: &mut CodeBudget) -> Self {
        let body = vec![highlighted(Some(language), code, budget)];
        let mut out = Self { body, links: Vec::new() };
        out.trim();
        out
    }

    /// Whether there is nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.body.is_empty()
    }

    /// `other` after this, a blank row between.
    pub fn append(&mut self, other: Self) {
        if other.is_empty() {
            return;
        }
        if !self.is_empty() {
            self.body.push(Vec::new());
        }
        let offset = self.links.len();
        self.links.extend(other.links);
        self.body.extend(other.body.into_iter().map(|paragraph| {
            paragraph
                .into_iter()
                .map(|run| Run { link: run.link.map(|link| link + offset), ..run })
                .collect()
        }));
    }

    /// No blank rows at either end, and never two together.
    fn trim(&mut self) {
        let blank = |paragraph: &Paragraph| paragraph.iter().all(|run| run.text.trim().is_empty());
        let mut body: Vec<Paragraph> = Vec::with_capacity(self.body.len());
        for paragraph in self.body.drain(..) {
            if blank(&paragraph) {
                if body.last().is_some_and(|last| !blank(last)) {
                    body.push(Vec::new());
                }
            } else {
                body.push(paragraph);
            }
        }
        while body.last().is_some_and(blank) {
            body.pop();
        }
        self.body = body;
    }
}

/// The state of a walk through the parser's events.
#[derive(Debug)]
struct Reader<'l, 'b> {
    out: Markdown,
    language: Option<&'l str>,
    budget: &'b mut CodeBudget,
    glyphs: &'b Glyphs,
    /// The paragraph being built.
    line: Paragraph,
    strong: usize,
    emphasis: usize,
    heading: bool,
    link: Option<usize>,
    /// Open lists, innermost last: the next number of an ordered one.
    lists: Vec<Option<u64>>,
    quotes: usize,
    /// A code block being collected: its language, and its text so far.
    code: Option<(Option<String>, String)>,
}

impl<'l, 'b> Reader<'l, 'b> {
    fn new(language: Option<&'l str>, budget: &'b mut CodeBudget, glyphs: &'b Glyphs) -> Self {
        Self {
            out: Markdown::default(),
            language,
            budget,
            glyphs,
            line: Vec::new(),
            strong: 0,
            emphasis: 0,
            heading: false,
            link: None,
            lists: Vec::new(),
            quotes: 0,
            code: None,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        if let Some((_, code)) = self.code.as_mut() {
            match event {
                Event::Text(text) => code.push_str(&text),
                Event::End(TagEnd::CodeBlock) => self.end_code(),
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.text(&text);
            }
            Event::Code(code) => self.line.push(Run::new(code.to_string(), INLINE_CODE)),
            Event::Html(html) | Event::InlineHtml(html) => {
                self.line.push(Run::new(html.trim_end_matches('\n').to_string(), Role::Dim));
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => self.block(),
            Event::TaskListMarker(done) => {
                let glyph = if done { Glyph::CardTaskDone } else { Glyph::CardTaskOpen };
                self.text(&format!("{} ", self.glyphs.get(glyph)));
            }
            Event::FootnoteReference(name) => self.text(&format!("[{name}]")),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph if self.lists.is_empty() => self.block(),
            Tag::Heading { .. } => {
                self.block();
                self.heading = true;
            }
            Tag::BlockQuote(_) => {
                self.block();
                self.quotes += 1;
            }
            Tag::CodeBlock(kind) => {
                self.block();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        // `rust,ignore`, `python {.class}`: the first word is
                        // the language.
                        let word = info
                            .split(|c: char| c == ',' || c == '{' || c.is_whitespace())
                            .next()
                            .unwrap_or_default();
                        (!word.is_empty()).then(|| word.to_string())
                    }
                    CodeBlockKind::Indented => None,
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(first) => {
                if self.lists.is_empty() {
                    self.block();
                } else {
                    self.flush();
                }
                self.lists.push(first);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}. ");
                        *number += 1;
                        marker
                    }
                    _ => format!("{} ", self.glyphs.get(Glyph::CardBullet)),
                };
                self.line.push(Run::new(format!("{}{marker}", "  ".repeat(depth)), Role::Dim));
            }
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Link { link_type, dest_url, .. } | Tag::Image { link_type, dest_url, .. } => {
                let url = if link_type == LinkType::Email {
                    format!("mailto:{dest_url}")
                } else {
                    dest_url.to_string()
                };
                self.link = Some(self.out.links.len());
                self.out.links.push(url);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Item | TagEnd::TableRow => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = false;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quotes = self.quotes.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => self.link = None,
            TagEnd::TableCell => self.text("  "),
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        let role = if self.link.is_some() { Role::Accent } else { Role::Text };
        self.line.push(Run {
            text: text.to_string(),
            role,
            bold: self.strong > 0 || self.heading,
            italic: self.emphasis > 0,
            link: self.link,
        });
    }

    /// The collected code block is complete.
    fn end_code(&mut self) {
        let Some((language, code)) = self.code.take() else { return };
        let language = language.as_deref().or(self.language);
        let code = code.strip_suffix('\n').unwrap_or(&code);
        self.out.body.push(highlighted(language, code, self.budget));
        self.block();
    }

    /// End the paragraph being built, if there is one.
    fn flush(&mut self) {
        if self.line.is_empty() {
            return;
        }
        let mut line = std::mem::take(&mut self.line);
        if self.quotes > 0 {
            let bar = format!("{} ", self.glyphs.get(Glyph::CardQuote));
            line.insert(0, Run::new(bar.repeat(self.quotes), Role::Dim));
        }
        self.out.body.push(line);
    }

    /// A new block starts: end the last, and leave a blank row after it.
    fn block(&mut self) {
        self.flush();
        if self.out.body.last().is_some_and(|last| !last.is_empty()) {
            self.out.body.push(Vec::new());
        }
    }
}

/// `code` as one paragraph, in the roles its grammar gives it when nun has
/// one for `language` and it is small enough; otherwise plain.
fn highlighted(language: Option<&str>, code: &str, budget: &mut CodeBudget) -> Paragraph {
    let plain = || vec![Run::new(code, Role::Text)];
    let Some(language) = language.and_then(nun_syntax::of_name) else { return plain() };
    let Some(left) = budget.take(code) else { return plain() };
    let started = Instant::now();
    let text = Rope::from_str(code);
    let chars = u32::try_from(text.len_chars()).unwrap_or(u32::MAX);
    let mut document = nun_syntax::Document::new(language, text.clone()).with_budget(left);
    let spans = document.highlights(0..chars);
    budget.spend(started.elapsed());
    let Some(spans) = spans else { return plain() };

    let mut runs = Vec::new();
    let mut at = 0;
    let piece = |from: u32, to: u32| text.slice(from as usize..to as usize).to_string();
    for span in spans {
        let (start, end) = (span.start.max(at), span.end.min(chars));
        if start >= end {
            continue;
        }
        if start > at {
            runs.push(Run::new(piece(at, start), Role::Text));
        }
        runs.push(Run::new(piece(start, end), crate::syntax::role_of(span.capture)));
        at = end;
    }
    if at < chars {
        runs.push(Run::new(piece(at, chars), Role::Text));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each paragraph as its text, with `*` around bold, `_` around italic,
    /// `<n:…>` around link `n`, and `@role:` before a run not in body text.
    fn shown(markdown: &Markdown) -> Vec<String> {
        markdown
            .body
            .iter()
            .map(|paragraph| {
                paragraph
                    .iter()
                    .map(|run| {
                        let mut text = run.text.clone();
                        if run.bold {
                            text = format!("*{text}*");
                        }
                        if run.italic {
                            text = format!("_{text}_");
                        }
                        if let Some(link) = run.link {
                            text = format!("<{link}:{text}>");
                        } else if run.role != Role::Text {
                            text = format!("@{:?}:{text}", run.role);
                        }
                        text
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn paragraphs_emphasis_and_inline_code() {
        let md = Markdown::parse(
            "Some **bold** and *soft*\ntext.\n\nUse `len()` here.",
            None,
            &mut CodeBudget::new(),
            &Glyphs::default(),
        );
        assert_eq!(shown(&md), ["Some *bold* and _soft_ text.", "", "Use @Type:len() here."]);
    }

    #[test]
    fn headings_are_bold_and_set_apart() {
        let md = Markdown::parse("# Title\nbody", None, &mut CodeBudget::new(), &Glyphs::default());
        assert_eq!(shown(&md), ["*Title*", "", "body"]);
    }

    #[test]
    fn lists_keep_their_markers_and_nest() {
        let md = Markdown::parse(
            "- one\n- two\n  - inner\n\n1. first\n2. second",
            None,
            &mut CodeBudget::new(),
            &Glyphs::default(),
        );
        assert_eq!(
            shown(&md),
            ["@Dim:• one", "@Dim:• two", "@Dim:  • inner", "", "@Dim:1. first", "@Dim:2. second"]
        );
    }

    #[test]
    fn links_are_numbered_and_their_targets_kept() {
        let md = Markdown::parse(
            "See [Vec](https://doc.rust-lang.org/std/vec/struct.Vec.html) and <a@b.c>.",
            None,
            &mut CodeBudget::new(),
            &Glyphs::default(),
        );
        assert_eq!(shown(&md), ["See <0:Vec> and <1:a@b.c>."]);
        assert_eq!(md.links, ["https://doc.rust-lang.org/std/vec/struct.Vec.html", "mailto:a@b.c"]);
    }

    #[test]
    fn fenced_code_is_highlighted_in_its_own_language() {
        let md = Markdown::parse(
            "```rust\nfn main() {}\n```\n---\nDocs.",
            None,
            &mut CodeBudget::new(),
            &Glyphs::default(),
        );
        let code = &md.body[0];
        assert_eq!(code.iter().map(|run| run.text.as_str()).collect::<String>(), "fn main() {}");
        assert!(code.iter().any(|run| run.text == "fn" && run.role == Role::Keyword), "{code:?}");
        assert!(code.iter().any(|run| run.text == "main" && run.role == Role::Function));
        assert_eq!(shown(&md)[1..], ["", "Docs."]);
    }

    #[test]
    fn a_fence_with_no_language_takes_the_file_s() {
        let md = Markdown::parse(
            "```\nlet x = 1;\n```",
            Some("rust"),
            &mut CodeBudget::new(),
            &Glyphs::default(),
        );
        assert!(md.body[0].iter().any(|run| run.text == "let" && run.role == Role::Keyword));
        let md = Markdown::parse(
            "```\nlet x = 1;\n```",
            None,
            &mut CodeBudget::new(),
            &Glyphs::default(),
        );
        assert_eq!(shown(&md), ["let x = 1;"], "plain, with nothing to go on");
    }

    #[test]
    fn a_language_nun_does_not_know_is_shown_plain() {
        let md = Markdown::parse(
            "```haskell\nmain = pure ()\n```",
            None,
            &mut CodeBudget::new(),
            &Glyphs::default(),
        );
        assert_eq!(shown(&md), ["main = pure ()"]);
    }

    #[test]
    fn code_lines_stay_lines() {
        let md = Markdown::parse(
            "```rust\nstruct A {\n    b: u8,\n}\n```",
            None,
            &mut CodeBudget::new(),
            &Glyphs::default(),
        );
        let text: String = md.body[0].iter().map(|run| run.text.as_str()).collect();
        assert_eq!(text, "struct A {\n    b: u8,\n}");
    }

    #[test]
    fn quotes_are_marked_down_the_side() {
        let md = Markdown::parse("> quoted", None, &mut CodeBudget::new(), &Glyphs::default());
        assert_eq!(shown(&md), ["@Dim:│ quoted"]);
    }

    #[test]
    fn plain_text_is_not_read_as_markdown() {
        let md = Markdown::plain("a *b*\n\n\nc");
        assert_eq!(shown(&md), ["a *b*", "", "c"]);
    }

    #[test]
    fn appending_renumbers_links_and_leaves_a_gap() {
        let mut md = Markdown::parse("[a](x)", None, &mut CodeBudget::new(), &Glyphs::default());
        md.append(Markdown::parse("[b](y)", None, &mut CodeBudget::new(), &Glyphs::default()));
        assert_eq!(shown(&md), ["<0:a>", "", "<1:b>"]);
        assert_eq!(md.links, ["x", "y"]);
    }

    #[test]
    fn nothing_much_is_nothing() {
        assert!(
            Markdown::parse("\n\n   \n", None, &mut CodeBudget::new(), &Glyphs::default())
                .is_empty()
        );
        assert!(Markdown::plain("").is_empty());
    }

    #[test]
    fn pathological_input_is_still_quick() {
        let text = "*a **b ".repeat(5000) + &"[".repeat(5000);
        let started = std::time::Instant::now();
        let md = Markdown::parse(&text, None, &mut CodeBudget::new(), &Glyphs::default());
        assert!(!md.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn many_blocks_share_one_card_s_allowance() {
        let block = format!("```rust\n{}```\n\n", "let x = 1;\n".repeat(MOST_HIGHLIGHTED / 11 / 3));
        let md =
            Markdown::parse(&block.repeat(6), None, &mut CodeBudget::new(), &Glyphs::default());
        let code: Vec<&Paragraph> =
            md.body.iter().filter(|paragraph| !paragraph.is_empty()).collect();
        assert_eq!(code.len(), 6);
        assert!(code[0].len() > 1, "the first is highlighted");
        assert_eq!(code[5].len(), 1, "the last, past the allowance, is plain");
    }

    #[test]
    fn a_huge_code_block_is_shown_plain() {
        let code = "let x = 1;\n".repeat(MOST_HIGHLIGHTED / 8);
        let md = Markdown::code("rust", &code, &mut CodeBudget::new());
        assert_eq!(md.body[0].len(), 1);
    }
}
