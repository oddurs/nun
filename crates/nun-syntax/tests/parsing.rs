//! Parsing, highlighting, injections, and staying out of the way when a
//! grammar misbehaves.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use nun_syntax::{Document, Reply, Request, Span, TextEdit, Worker, of_name, of_path};
use ropey::Rope;

fn rust(text: &str) -> Document {
    Document::new(of_name("rust").unwrap(), Rope::from_str(text))
}

/// The capture covering the first occurrence of `needle`.
fn capture_of(spans: &[Span], text: &str, needle: &str) -> Option<&'static str> {
    let byte = text.find(needle)?;
    let at = u32::try_from(text[..byte].chars().count()).ok()?;
    spans.iter().find(|span| span.start <= at && span.end > at).map(|span| span.capture)
}

fn all(document: &mut Document) -> Vec<Span> {
    document.highlights(0..u32::MAX).expect("the language is not disabled")
}

#[test]
fn a_rust_file_comes_back_with_the_captures_you_would_expect() {
    let text = "fn greet(name: &str) -> String {\n    // hello\n    format!(\"hi {name}\")\n}\n";
    let mut document = rust(text);
    let spans = all(&mut document);

    assert!(capture_of(&spans, text, "fn ").unwrap().starts_with("keyword"), "fn");
    assert!(capture_of(&spans, text, "// hello").unwrap().starts_with("comment"), "comment");
    assert!(capture_of(&spans, text, "\"hi").unwrap().starts_with("string"), "string");
    assert!(capture_of(&spans, text, "greet").unwrap().starts_with("function"), "name");
}

#[test]
fn the_runs_do_not_overlap_and_stay_in_order() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/highlight.rs"))
        .expect("this crate's own source");
    let mut document = rust(&text);
    let spans = all(&mut document);

    assert!(spans.len() > 100, "a real file has plenty to highlight");
    for pair in spans.windows(2) {
        assert!(pair[0].end <= pair[1].start, "{:?} overlaps {:?}", pair[0], pair[1]);
        assert!(pair[0].start < pair[0].end, "empty run: {:?}", pair[0]);
    }
}

#[test]
fn the_innermost_capture_wins() {
    let text = "fn main() { println!(\"x\"); }\n";
    let mut document = rust(text);
    let spans = all(&mut document);
    let at_name = capture_of(&spans, text, "println").unwrap();
    assert!(at_name.contains("macro") || at_name.contains("function"), "{at_name}");
}

#[test]
fn sql_inside_a_rust_string_is_highlighted_as_sql() {
    let text = "fn q() { sqlx::query!(\"SELECT name FROM users WHERE id = 1\"); }\n";
    let mut document = rust(text);
    let spans = all(&mut document);

    let select = capture_of(&spans, text, "SELECT");
    assert!(
        select.is_some_and(|capture| capture.starts_with("keyword")),
        "SELECT should be a SQL keyword, was {select:?}"
    );
}

#[test]
fn css_inside_html_is_highlighted_as_css() {
    let text = "<html><style>body { color: red; }</style></html>\n";
    let mut document = Document::new(of_name("html").unwrap(), Rope::from_str(text));
    let spans = all(&mut document);

    assert!(capture_of(&spans, text, "color").is_some(), "the CSS is not highlighted");
    assert!(capture_of(&spans, text, "body {").is_some(), "the selector is not highlighted");
}

#[test]
fn a_file_in_a_language_nun_does_not_have_is_simply_not_highlighted() {
    assert!(of_path(std::path::Path::new("notes.txt")).is_none());
}

#[test]
fn typing_into_a_large_file_reparses_within_a_frame() {
    let mut text = String::new();
    for index in 0..10_000 {
        let _ = writeln!(text, "fn function{index}(x: u32) -> u32 {{ x + {index} }}");
    }
    let mut document = rust(&text);
    let first = Instant::now();
    let _ = all(&mut document);
    let cold = first.elapsed();

    let at = u32::try_from(text.len() / 2).unwrap();
    let mut edited = text.clone();
    edited.insert(at as usize, 'z');
    document.update(
        Rope::from_str(&edited),
        Some(TextEdit { start: at, old_end: at, new_end: at + 1 }),
    );

    let start = Instant::now();
    let spans = document.highlights(at.saturating_sub(2000)..at + 2000).unwrap();
    let warm = start.elapsed();

    assert!(!spans.is_empty());
    assert!(warm < Duration::from_millis(16), "reparse took {warm:?} (cold parse {cold:?})");
}

#[test]
fn an_edit_that_cannot_be_described_reparses_from_scratch_rather_than_lying() {
    let mut document = rust("fn a() {}\n");
    let _ = all(&mut document);

    document.update(
        Rope::from_str("fn b() {}\n"),
        Some(TextEdit { start: 99, old_end: 0, new_end: 5 }),
    );
    let spans = all(&mut document);
    assert!(capture_of(&spans, "fn b() {}\n", "b").unwrap().starts_with("function"));
}

#[test]
fn highlights_follow_the_text_through_a_run_of_edits() {
    let mut text = String::from("fn a() {}\n");
    let mut document = rust(&text);
    let _ = all(&mut document);

    for letter in "bcdef".chars() {
        let at = u32::try_from(text.len()).unwrap();
        let _ = writeln!(text, "fn {letter}() {{}}");
        let new_end = u32::try_from(text.len()).unwrap();
        document.update(Rope::from_str(&text), Some(TextEdit { start: at, old_end: at, new_end }));
    }

    let spans = all(&mut document);
    for letter in "abcdef".chars() {
        let name = format!("fn {letter}");
        let at = u32::try_from(text.find(&name).unwrap()).unwrap() + 3;
        let capture = spans.iter().find(|span| span.start <= at && span.end > at);
        assert!(
            capture.is_some_and(|span| span.capture.starts_with("function")),
            "fn {letter}: {capture:?}"
        );
    }
}

fn worker() -> (Worker, std::sync::mpsc::Receiver<Reply>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = Worker::new(Box::new(move |reply| {
        let _ = sender.send(reply);
    }));
    (worker, receiver)
}

#[test]
fn the_worker_answers_with_highlights_for_the_version_it_was_given() {
    let (worker, replies) = worker();
    let text = "fn main() {}\n";
    worker.send(Request::Open {
        id: 1,
        language: of_name("rust").unwrap(),
        text: Rope::from_str(text),
    });
    worker.send(Request::Update {
        id: 1,
        version: 7,
        text: Rope::from_str(text),
        edit: None,
        window: 0..u32::try_from(text.len()).unwrap(),
    });

    match replies.recv_timeout(Duration::from_secs(10)).unwrap() {
        Reply::Highlights { id, version, spans, .. } => {
            assert_eq!((id, version), (1, 7));
            assert!(!spans.is_empty());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn typing_faster_than_the_parser_answers_only_the_latest_text() {
    let (worker, replies) = worker();
    let mut text = String::from("fn main() {}\n");
    worker.send(Request::Open {
        id: 1,
        language: of_name("rust").unwrap(),
        text: Rope::from_str(&text),
    });

    for version in 1..=20u64 {
        text.push_str("fn more() {}\n");
        worker.send(Request::Update {
            id: 1,
            version,
            text: Rope::from_str(&text),
            edit: None,
            window: 0..u32::try_from(text.len()).unwrap(),
        });
    }
    worker.send(Request::Echo(1));

    let mut answers = Vec::new();
    loop {
        match replies.recv_timeout(Duration::from_secs(10)).unwrap() {
            Reply::Highlights { version, .. } => answers.push(version),
            Reply::Echo(1) => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(answers.contains(&20), "the latest text is always answered: {answers:?}");
    assert!(answers.len() < 20, "superseded versions are not parsed: {answers:?}");
}

#[test]
fn closing_a_document_stops_it_being_parsed() {
    let (worker, replies) = worker();
    worker.send(Request::Open {
        id: 1,
        language: of_name("rust").unwrap(),
        text: Rope::from_str("fn a() {}\n"),
    });
    worker.send(Request::Close(1));
    worker.send(Request::Window { id: 1, version: 1, window: 0..10 });
    worker.send(Request::Echo(2));

    assert_eq!(replies.recv_timeout(Duration::from_secs(10)).unwrap(), Reply::Echo(2));
}

// ── a grammar that misbehaves ───────────────────────────────────────────────

#[test]
fn a_parse_that_runs_over_its_budget_is_abandoned_and_the_language_switched_off() {
    let mut text = String::new();
    for index in 0..20_000 {
        let _ = writeln!(text, "fn function{index}() {{ let x = {index}; }}");
    }
    // No budget at all: the first progress check gives up.
    let mut document =
        Document::new(of_name("rust").unwrap(), Rope::from_str(&text)).with_budget(Duration::ZERO);

    assert!(document.highlights(0..u32::MAX).is_none(), "it should have given up");
    assert_eq!(document.trouble(), Some(&nun_syntax::Trouble::TooSlow));

    // And it stays off rather than being tried again on every keystroke.
    document.update(Rope::from_str("fn a() {}\n"), None);
    assert!(document.highlights(0..u32::MAX).is_none());
}

#[test]
fn a_document_whose_language_is_off_still_holds_its_text() {
    let mut text = String::new();
    for index in 0..20_000 {
        let _ = writeln!(text, "fn function{index}() {{ let x = {index}; }}");
    }
    let mut document =
        Document::new(of_name("rust").unwrap(), Rope::from_str(&text)).with_budget(Duration::ZERO);
    let _ = document.highlights(0..u32::MAX);
    assert!(document.trouble().is_some());

    // The editor goes on editing it; only the colours are gone.
    let at = u32::try_from(text.len()).unwrap();
    text.push_str("fn more() {}\n");
    document.update(
        Rope::from_str(&text),
        Some(TextEdit { start: at, old_end: at, new_end: u32::try_from(text.len()).unwrap() }),
    );
    assert!(document.highlights(0..u32::MAX).is_none(), "still off, not retried each keystroke");
}

// ── growing a selection ─────────────────────────────────────────────────────

/// Grow `needle`'s first occurrence (or a caret before it, when `caret`) once
/// per step, and give back the text each step selected.
fn growing(text: &str, needle: &str, caret: bool, steps: usize) -> Vec<String> {
    let mut document = rust(text);
    let byte = text.find(needle).expect("the needle is in the text");
    let from = u32::try_from(text[..byte].chars().count()).unwrap();
    let to = if caret { from } else { from + u32::try_from(needle.chars().count()).unwrap() };
    let mut range = from..to;
    let mut seen = Vec::new();
    for _ in 0..steps {
        range = document.grow(std::slice::from_ref(&range)).expect("rust is on")[0].clone();
        seen.push(
            text.chars()
                .skip(range.start as usize)
                .take((range.end - range.start) as usize)
                .collect(),
        );
    }
    seen
}

#[test]
fn a_caret_grows_to_its_word_first_and_then_outwards() {
    let text = "fn main() {\n    let total = price * count;\n}\n";
    let steps = growing(text, "price", true, 4);
    assert_eq!(steps[0], "price", "the identifier under the caret");
    assert_eq!(steps[1], "price * count", "then the expression it is in");
    assert_eq!(steps[2], "let total = price * count;", "then the statement");
    assert!(steps[3].starts_with('{'), "then the block: {:?}", steps[3]);
}

#[test]
fn growing_never_stops_on_punctuation() {
    let text = "fn f() { call(a, b); }\n";
    let steps = growing(text, "(a", true, 1);
    assert_eq!(steps[0], "(a, b)", "the argument list, not a lone parenthesis");
}

#[test]
fn a_selection_that_is_already_a_node_grows_past_itself() {
    let text = "fn f() { call(a, b); }\n";
    let steps = growing(text, "call(a, b)", false, 1);
    assert_eq!(steps[0], "call(a, b);", "the statement, not the same call again");
}

#[test]
fn the_whole_file_is_as_far_as_it_goes() {
    let text = "fn f() {}\n";
    let mut document = rust(text);
    let all = 0..u32::try_from(text.chars().count()).unwrap();
    let grown = document.grow(std::slice::from_ref(&all)).unwrap();
    assert_eq!(grown, [all], "nothing encloses the file");
}

#[test]
fn several_ranges_grow_independently() {
    let text = "fn f() { one(); }\nfn g() { two(); }\n";
    let mut document = rust(text);
    let at = |needle: &str| u32::try_from(text.find(needle).unwrap()).unwrap();
    let grown = document.grow(&[at("one")..at("one"), at("two")..at("two")]).unwrap();
    assert_eq!(grown, [at("one")..at("one") + 3, at("two")..at("two") + 3]);
}

#[test]
fn growing_counts_in_chars_not_bytes() {
    // Everything before the caret is multi-byte, so a byte offset taken for a
    // char offset would land in the middle of the comment.
    let text = "// größe 日本\nfn f() { value; }\n";
    let steps = growing(text, "value", true, 1);
    assert_eq!(steps[0], "value");
}
