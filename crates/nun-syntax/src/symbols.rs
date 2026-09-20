//! What a file declares, as an outline.
//!
//! Derived from the parse tree rather than from a language server, so jumping
//! around a file works in a project that has no server, has not started one
//! yet, or is written in something nobody has written a server for.
//!
//! Grammars ship a tags query for exactly this — it is what generates the
//! symbol index for code search — and it captures a definition node along
//! with the part of it that is the name. Nesting is not in the query, because
//! a tag is a fact about one node; it comes from the ranges, since a method's
//! definition lies inside its type's.

use ropey::Rope;
use tree_sitter::{QueryCursor, StreamingIterator};

/// One thing a file declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// What it is called.
    pub name: String,
    /// What kind of thing it is — `function`, `method`, `class` and so on,
    /// as the grammar's own tags query names it.
    pub kind: &'static str,
    /// Where its name starts, as a char offset, which is where going to it
    /// puts the caret.
    pub at: u32,
    /// How deeply it nests inside other definitions.
    pub depth: usize,
    /// The definition this one sits inside, as an index into the same list.
    ///
    /// Recorded here because the stack that computes `depth` already knows it,
    /// and working it out again later means scanning backwards for something
    /// shallower — which is per-match work proportional to the file.
    pub parent: Option<usize>,
}

/// Pull the outline out of a parsed tree.
///
/// `offsets` walks the definitions in the order they appear, which is the
/// order an outline reads in — not the order the query happens to match them,
/// which follows the query's patterns rather than the file.
pub(crate) fn of_tree(
    language: &'static crate::Language,
    tree: &tree_sitter::Tree,
    text: &Rope,
) -> Vec<Symbol> {
    let Some(query) = language.symbols.as_ref() else { return Vec::new() };
    let names = query.capture_names();

    let mut found: Vec<Found> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), crate::highlight::RopeText(text));
    while let Some(matched) = matches.next() {
        // A tags pattern captures the definition and, inside it, the name.
        // Without both there is nothing to list or nothing to call it.
        let mut whole: Option<(usize, usize)> = None;
        let mut name: Option<(usize, usize)> = None;
        let mut kind: Option<&'static str> = None;
        for capture in matched.captures() {
            let capture_name = names[capture.index as usize];
            let range = capture.node.byte_range();
            if let Some(rest) = capture_name.strip_prefix("definition.") {
                whole = Some((range.start, range.end));
                kind = Some(rest);
            } else if capture_name == "name" {
                name = Some((range.start, range.end));
            }
        }
        let (Some(whole), Some(name), Some(kind)) = (whole, name, kind) else { continue };
        let text_of = |(start, end): (usize, usize)| {
            let end = end.min(text.len_bytes());
            let start = start.min(end);
            text.byte_slice(start..end).to_string()
        };
        found.push(Found { whole, at: name.0, kind, name: text_of(name) });
    }

    // In the order they appear, so nesting can be read off a stack.
    found.sort_by_key(|one| (one.whole.0, std::cmp::Reverse(one.whole.1)));
    found.dedup_by(|a, b| a.whole == b.whole && a.name == b.name);

    // The enclosing definitions, each with where it landed in the outline.
    let mut open: Vec<((usize, usize), usize)> = Vec::new();
    let mut outline: Vec<Symbol> = Vec::with_capacity(found.len());
    for (index, one) in found.into_iter().enumerate() {
        // Anything that ended before this starts is no longer enclosing.
        while open.last().is_some_and(|(range, _)| range.1 <= one.whole.0) {
            open.pop();
        }
        outline.push(Symbol {
            name: one.name,
            kind: one.kind,
            at: u32::try_from(text.byte_to_char(one.at.min(text.len_bytes()))).unwrap_or(u32::MAX),
            depth: open.len(),
            parent: open.last().map(|(_, at)| *at),
        });
        open.push((one.whole, index));
    }
    outline
}

/// One definition, before its depth is known.
struct Found {
    whole: (usize, usize),
    at: usize,
    kind: &'static str,
    name: String,
}

#[cfg(test)]
mod tests {
    use crate::{Document, of_name};
    use ropey::Rope;

    /// The outline of `text`, as `depth:kind name` for readability.
    fn outline(language: &str, text: &str) -> Vec<String> {
        let language = of_name(language).expect("a language nun has");
        let mut document = Document::new(language, Rope::from_str(text));
        document
            .symbols()
            .expect("the language is on")
            .into_iter()
            .map(|symbol| format!("{}:{} {}", symbol.depth, symbol.kind, symbol.name))
            .collect()
    }

    #[test]
    fn a_rust_file_lists_what_it_declares() {
        let found = outline(
            "rust",
            "struct Point { x: i32 }\n\
             \n\
             impl Point {\n\
             \x20   fn new() -> Self { Self { x: 0 } }\n\
             }\n\
             \n\
             fn main() {}\n",
        );
        assert!(found.iter().any(|row| row.ends_with("Point")), "the struct: {found:?}");
        assert!(found.iter().any(|row| row.ends_with("new")), "the method: {found:?}");
        assert!(found.iter().any(|row| row.ends_with("main")), "the function: {found:?}");
    }

    #[test]
    fn a_rust_method_nests_under_the_block_that_implements_it() {
        // Rust's own tags query tags the method but not the impl holding it,
        // so without nun's supplement every method in a file comes out at the
        // top level beside the free functions.
        let found = outline(
            "rust",
            "impl Point {\n    fn new() {}\n}\n\nimpl Display for Point {\n    fn fmt() {}\n}\n\nfn free() {}\n",
        );
        let depth_of = |name: &str| {
            found
                .iter()
                .find(|row| row.ends_with(name))
                .and_then(|row| row.chars().next())
                .expect("listed")
        };
        assert_eq!(depth_of("new"), '1', "{found:?}");
        assert_eq!(depth_of("fmt"), '1', "and so does the trait method: {found:?}");
        assert_eq!(depth_of("free"), '0', "a free function does not: {found:?}");
        assert_eq!(
            found.iter().filter(|row| row.contains(":impl ")).count(),
            2,
            "both blocks are headings of their own: {found:?}"
        );
    }

    #[test]
    fn a_definition_inside_another_is_deeper_than_it() {
        let found = outline(
            "python",
            "class Shape:\n\
             \x20   def area(self):\n\
             \x20       return 0\n\
             \n\
             def free():\n\
             \x20   pass\n",
        );
        let shape = found.iter().find(|row| row.ends_with("Shape")).expect("the class");
        let area = found.iter().find(|row| row.ends_with("area")).expect("the method");
        let free = found.iter().find(|row| row.ends_with("free")).expect("the function");
        assert!(shape.starts_with('0'), "{shape}");
        assert!(area.starts_with('1'), "a method nests inside its class: {area}");
        assert!(free.starts_with('0'), "and a function after it does not: {free}");
    }

    #[test]
    fn the_outline_reads_in_the_order_the_file_is_written() {
        let found = outline("rust", "fn first() {}\nfn second() {}\nfn third() {}\n");
        let names: Vec<&str> = found
            .iter()
            .filter_map(|row| row.rsplit(' ').next())
            .filter(|name| ["first", "second", "third"].contains(name))
            .collect();
        assert_eq!(names, ["first", "second", "third"]);
    }

    #[test]
    fn going_to_a_symbol_lands_on_its_name() {
        let text = "fn alpha() {}\nfn beta() {}\n";
        let language = of_name("rust").unwrap();
        let mut document = Document::new(language, Rope::from_str(text));
        let symbols = document.symbols().unwrap();
        let beta = symbols.iter().find(|symbol| symbol.name == "beta").expect("beta");
        let at = beta.at as usize;
        assert_eq!(&text[at..at + 4], "beta", "the offset is the name, not the line");
    }

    #[test]
    fn a_language_with_no_tags_query_has_an_empty_outline_rather_than_none() {
        // Nothing is wrong with a TOML file; there is just nothing to list.
        let language = of_name("toml").unwrap();
        let mut document = Document::new(language, Rope::from_str("[table]\nkey = 1\n"));
        assert_eq!(document.symbols(), Some(Vec::new()));
    }

    #[test]
    fn a_name_in_another_script_survives_the_trip_through_bytes() {
        let found = outline("python", "def \u{3b1}\u{3b2}():\n    pass\n");
        assert!(found.iter().any(|row| row.ends_with("\u{3b1}\u{3b2}")), "{found:?}");
    }
}
