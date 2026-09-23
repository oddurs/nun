//! Completion, the parts that are about the protocol rather than the screen.
//!
//! A server answers once; the person keeps typing. So the list is filtered
//! and sorted here, on every keystroke, against what has been typed since —
//! [`filter`] — and nothing waits for the server to catch up. When the text
//! is accepted, [`insertion`] says what the item wants inserted and where,
//! and [`Asked`] reads the server's positions, which describe the text as it
//! was when the question was asked, against the text as it is now.

use lsp_types::{
    CompletionItem, CompletionResponse, CompletionTextEdit, InsertTextFormat, InsertTextMode,
    Position, TextEdit,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ropey::Rope;

use crate::position::Encoding;

/// Most items one answer is kept to. A server asked with nothing typed can
/// send the whole of a standard library; scoring that on every keystroke is
/// the one way filtering could get slow enough to notice.
pub const MOST_ITEMS: usize = 5_000;

/// The items of an answer, and whether the server said typing more should
/// ask again.
#[must_use]
pub fn items_of(response: Option<CompletionResponse>) -> (Vec<CompletionItem>, bool) {
    let (mut items, incomplete) = match response {
        None => (Vec::new(), false),
        Some(CompletionResponse::Array(items)) => (items, false),
        Some(CompletionResponse::List(list)) => (list.items, list.is_incomplete),
    };
    // Cut down, the list is no longer all there is.
    let incomplete = incomplete || items.len() > MOST_ITEMS;
    items.truncate(MOST_ITEMS);
    (items, incomplete)
}

/// The text an item is matched against.
fn filter_text(item: &CompletionItem) -> &str {
    item.filter_text.as_deref().unwrap_or(&item.label)
}

/// The key an item sorts by when scores tie.
fn sort_text(item: &CompletionItem) -> &str {
    item.sort_text.as_deref().unwrap_or(&item.label)
}

/// One item that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// Its position in the list.
    pub index: usize,
    /// Higher is better.
    pub score: u32,
    /// Char offsets of its label that matched, for highlighting.
    pub matched: Vec<u32>,
}

/// The items that match `query`, best first.
///
/// With nothing typed, every item in the order the server sorted them. Ties
/// go to the server's order too, which is where its idea of relevance —
/// locals before globals, fields before methods — has its say.
#[must_use]
pub fn filter(items: &[CompletionItem], query: &str) -> Vec<Shown> {
    if query.is_empty() {
        let mut shown: Vec<Shown> =
            (0..items.len()).map(|index| Shown { index, score: 0, matched: Vec::new() }).collect();
        shown.sort_by(|a, b| sort_text(&items[a.index]).cmp(sort_text(&items[b.index])));
        return shown;
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    // Char by char, not the grapheme-by-grapheme `Utf32Str::new` does: what
    // is typed is chars, and a decomposed `é` typed has to match one written.
    let mut chars: Vec<char> = Vec::new();
    let mut indices = Vec::new();
    let mut shown: Vec<Shown> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            chars.clear();
            chars.extend(filter_text(item).chars());
            let score = pattern.score(Utf32Str::Unicode(&chars), &mut matcher)?;
            Some(Shown { index, score, matched: Vec::new() })
        })
        .collect();
    shown.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| sort_text(&items[a.index]).cmp(sort_text(&items[b.index])))
            .then(a.index.cmp(&b.index))
    });

    // Where it matched is only worth working out for what is drawn, and only
    // against the label, which is what is drawn: the filter text can differ.
    // Char offsets, as the widget counts them.
    for found in shown.iter_mut().take(MOST_HIGHLIGHTED) {
        chars.clear();
        chars.extend(items[found.index].label.chars());
        indices.clear();
        if pattern.indices(Utf32Str::Unicode(&chars), &mut matcher, &mut indices).is_some() {
            indices.sort_unstable();
            indices.dedup();
            found.matched.clone_from(&indices);
        }
    }
    shown
}

/// How many of the best matches say where they matched. More than a popup
/// shows at once, and a scroll that goes further gets plain labels.
const MOST_HIGHLIGHTED: usize = 200;

/// Which of an item's two ranges to use, when it has both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Replace from the start of the word to the caret, and keep whatever
    /// follows the caret. The default, as in most editors.
    #[default]
    Insert,
    /// Replace the whole word the caret is in.
    Replace,
}

/// What accepting an item does to the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    /// The range the text replaces, as the server gave it; `None` when the
    /// item left that to the editor, which replaces the word before the
    /// caret.
    pub range: Option<lsp_types::Range>,
    /// The text, with line endings as the buffer keeps them.
    pub text: String,
    /// Whether `text` is snippet syntax rather than plain text.
    pub snippet: bool,
    /// Whether lines after the first take the indentation of the line the
    /// text goes into.
    pub indent: bool,
    /// Edits elsewhere — an import, most often — with line endings as the
    /// buffer keeps them.
    pub additional: Vec<TextEdit>,
}

/// What accepting `item` inserts, and where.
#[must_use]
pub fn insertion(item: &CompletionItem, mode: Mode) -> Insertion {
    let (range, text) = match &item.text_edit {
        Some(CompletionTextEdit::Edit(edit)) => (Some(edit.range), edit.new_text.as_str()),
        Some(CompletionTextEdit::InsertAndReplace(edit)) => {
            let range = match mode {
                Mode::Insert => edit.insert,
                Mode::Replace => edit.replace,
            };
            (Some(range), edit.new_text.as_str())
        }
        None => (None, item.insert_text.as_deref().unwrap_or(&item.label)),
    };
    let additional = item
        .additional_text_edits
        .iter()
        .flatten()
        .map(|edit| TextEdit { range: edit.range, new_text: to_lf(&edit.new_text) })
        .collect();
    Insertion {
        range,
        text: to_lf(text),
        snippet: item.insert_text_format == Some(InsertTextFormat::SNIPPET),
        indent: item.insert_text_mode != Some(InsertTextMode::AS_IS),
        additional,
    }
}

/// Text from a server with its line endings as the buffer keeps them. The
/// buffer holds `\n` whatever the file has on disk, and so does the server's
/// copy; a `\r` let in would be a character the server does not know is
/// there.
#[must_use]
pub fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Where a completion was asked for: the text and the caret as they were.
///
/// A server's positions describe the text it was asked about. By the time an
/// item is accepted the person may have typed more of the word, or taken
/// some of it back — nothing else, or the popup would have closed. So a
/// position is read against the text as it was, and anything at or after
/// the caret on the caret's line moves with the caret.
#[derive(Debug, Clone)]
pub struct Asked {
    text: Rope,
    head: usize,
    encoding: Encoding,
}

impl Asked {
    /// Asked about `text` with the caret at `head`, a char index, of a server
    /// counting in `encoding`.
    #[must_use]
    pub const fn new(text: Rope, head: usize, encoding: Encoding) -> Self {
        Self { text, head, encoding }
    }

    /// The caret, as it was.
    #[must_use]
    pub const fn head(&self) -> usize {
        self.head
    }

    /// `position`, from the server, as a char index into `now`, where the
    /// caret that was at [`Asked::head`] is now at `head`.
    #[must_use]
    pub fn char_index(&self, now: &Rope, head: usize, position: Position) -> usize {
        let old = self.encoding.char_index(&self.text, position);
        let line = self.text.char_to_line(old);
        let column = old - self.text.line_to_char(line);

        let old_line = self.text.char_to_line(self.head);
        let old_column = self.head - self.text.line_to_char(old_line);
        let head = head.min(now.len_chars());
        let new_line = now.char_to_line(head);
        let new_column = head - now.line_to_char(new_line);

        let column = if line == old_line {
            if column >= old_column {
                (column + new_column).saturating_sub(old_column)
            } else {
                column.min(new_column)
            }
        } else {
            column
        };
        let line = line.min(now.len_lines().saturating_sub(1));
        let start = now.line_to_char(line);
        let slice = now.line(line);
        let len = slice.len_chars();
        // Never past the end of the line, into the next one.
        let len = if len > 0 && slice.char(len - 1) == '\n' { len - 1 } else { len };
        start + column.min(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{CompletionList, InsertReplaceEdit, Range};

    fn item(label: &str) -> CompletionItem {
        CompletionItem { label: label.to_string(), ..CompletionItem::default() }
    }

    fn labels(items: &[CompletionItem], shown: &[Shown]) -> Vec<String> {
        shown.iter().map(|found| items[found.index].label.clone()).collect()
    }

    const fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn nothing_typed_keeps_the_servers_order() {
        let mut items = vec![item("zeta"), item("alpha"), item("mid")];
        items[0].sort_text = Some("0".into());
        items[1].sort_text = Some("2".into());
        items[2].sort_text = Some("1".into());
        assert_eq!(labels(&items, &filter(&items, "")), ["zeta", "mid", "alpha"]);
    }

    #[test]
    fn typing_narrows_and_ranks_and_says_where_it_matched() {
        let items = vec![item("print"), item("println"), item("eprintln"), item("format")];
        let shown = filter(&items, "pln");
        assert_eq!(labels(&items, &shown), ["println", "eprintln"]);
        assert_eq!(shown[0].matched, [0, 5, 6]);
        assert!(filter(&items, "xyz").is_empty());
    }

    #[test]
    fn highlights_count_chars_whatever_the_label_holds() {
        let items = vec![item("cafe\u{301}_au_lait"), item("日本e\u{301}_ok")];
        let shown = filter(&items, "au");
        assert_eq!(shown[0].matched, [6, 7], "`au`, past the two chars of `é`");
        let shown = filter(&items, "ok");
        assert_eq!(shown[0].matched, [5, 6]);
    }

    #[test]
    fn the_filter_text_is_matched_but_the_label_is_highlighted() {
        let mut items = vec![item("len()")];
        items[0].filter_text = Some("length".into());
        let shown = filter(&items, "lgth");
        assert_eq!(shown.len(), 1, "matched against the filter text");
        assert!(shown[0].matched.is_empty(), "which is not what is drawn");
    }

    #[test]
    fn a_long_list_is_cut_down_and_marked_incomplete() {
        let list = CompletionResponse::List(CompletionList {
            is_incomplete: false,
            items: (0..MOST_ITEMS + 10).map(|n| item(&n.to_string())).collect(),
        });
        let (items, incomplete) = items_of(Some(list));
        assert_eq!(items.len(), MOST_ITEMS);
        assert!(incomplete);
        assert_eq!(items_of(None), (Vec::new(), false));
    }

    #[test]
    fn insert_text_or_the_label_when_there_is_no_edit() {
        let mut plain = item("println!");
        assert_eq!(insertion(&plain, Mode::Insert).text, "println!");
        plain.insert_text = Some("println!(\"$1\")".into());
        plain.insert_text_format = Some(InsertTextFormat::SNIPPET);
        let insertion = insertion(&plain, Mode::Insert);
        assert_eq!((insertion.range, insertion.snippet), (None, true));
    }

    #[test]
    fn the_mode_picks_the_insert_or_the_replace_range() {
        let mut both = item("println");
        let insert = Range::new(at(0, 0), at(0, 3));
        let replace = Range::new(at(0, 0), at(0, 5));
        both.text_edit = Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
            new_text: "println".into(),
            insert,
            replace,
        }));
        assert_eq!(insertion(&both, Mode::Insert).range, Some(insert));
        assert_eq!(insertion(&both, Mode::Replace).range, Some(replace));
    }

    #[test]
    fn line_endings_from_the_server_become_the_buffers() {
        let mut crlf = item("block");
        crlf.insert_text = Some("{\r\n\tx\r\n}".into());
        crlf.additional_text_edits = Some(vec![TextEdit {
            range: Range::new(at(0, 0), at(0, 0)),
            new_text: "use std::io;\r\n".into(),
        }]);
        let insertion = insertion(&crlf, Mode::Insert);
        assert_eq!(insertion.text, "{\n\tx\n}");
        assert_eq!(insertion.additional[0].new_text, "use std::io;\n");
        assert_eq!(to_lf("a\rb"), "a\nb", "a lone CR is a line ending too");
    }

    #[test]
    fn positions_follow_typing_that_came_after_the_question() {
        // Asked at `pri|`, the server's range covers `pri` and the word's
        // tail after the caret.
        let then = Rope::from_str("let x = pri;\nnext\n");
        let asked = Asked::new(then, 11, Encoding::Utf16);
        // Two more letters typed.
        let now = Rope::from_str("let x = print;\nnext\n");
        assert_eq!(asked.char_index(&now, 13, at(0, 8)), 8, "before the caret: unmoved");
        assert_eq!(asked.char_index(&now, 13, at(0, 11)), 13, "the caret moved with typing");
        assert_eq!(asked.char_index(&now, 13, at(0, 12)), 14, "after it moves too");
        assert_eq!(asked.char_index(&now, 13, at(1, 2)), 17, "another line: as it was");

        // One letter taken back instead.
        let now = Rope::from_str("let x = pr;\nnext\n");
        assert_eq!(asked.char_index(&now, 10, at(0, 11)), 10);
        assert_eq!(asked.char_index(&now, 10, at(0, 8)), 8);
    }

    #[test]
    fn positions_are_counted_in_the_servers_units() {
        // `é` is one UTF-16 unit and two UTF-8 bytes; `😀` is two and four.
        let text = Rope::from_str("😀é.x");
        let utf16 = Asked::new(text.clone(), 4, Encoding::Utf16);
        assert_eq!(utf16.char_index(&text, 4, at(0, 3)), 2);
        let utf8 = Asked::new(text.clone(), 4, Encoding::Utf8);
        assert_eq!(utf8.char_index(&text, 4, at(0, 6)), 2);
    }

    #[test]
    fn a_position_past_the_line_stays_on_it() {
        let text = Rope::from_str("ab\ncd");
        let asked = Asked::new(text.clone(), 2, Encoding::Utf16);
        assert_eq!(asked.char_index(&text, 2, at(0, 40)), 2);
        assert_eq!(asked.char_index(&text, 2, at(9, 0)), 5, "past the last line: the end");
    }
}
