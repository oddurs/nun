//! Parsing, off the thread that draws.
//!
//! The editor sends the text and the edit that changed it and carries on
//! drawing; highlights arrive later as a message and are applied to the next
//! frame. Everything that could take an unknown amount of time — a first parse
//! of a large file, a grammar gone quadratic, a query over a big window — is
//! on this side of the channel.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::thread;

use ropey::Rope;

use crate::highlight::{Document, Span, TextEdit};
use crate::language::Language;

/// Which document a message is about.
pub type DocId = u32;

/// Something for the worker to do.
#[derive(Debug)]
pub enum Request {
    /// Start following a document.
    Open {
        /// Which document.
        id: DocId,
        /// Its language.
        language: &'static Language,
        /// Its text.
        text: Rope,
    },
    /// The text changed.
    Update {
        /// Which document.
        id: DocId,
        /// Which version of it this is.
        version: u64,
        /// The text now.
        text: Rope,
        /// The single edit that made it, when there was exactly one.
        edit: Option<TextEdit>,
        /// The char range worth highlighting: what is on screen.
        window: std::ops::Range<u32>,
    },
    /// The same text, a different part of it on screen.
    Window {
        /// Which document.
        id: DocId,
        /// Which version of it is being looked at.
        version: u64,
        /// The char range worth highlighting.
        window: std::ops::Range<u32>,
    },
    /// What does this document declare?
    Symbols {
        /// Which document.
        id: DocId,
        /// Which version of it is being asked about.
        version: u64,
    },
    /// Grow each of these char ranges to the node enclosing it.
    Grow {
        /// Which document.
        id: DocId,
        /// Which version of it the ranges are in.
        version: u64,
        /// Which grow this is, echoed back so an answer is matched to the
        /// question it answers rather than to whichever one is outstanding.
        serial: u64,
        /// The ranges, one per selection.
        ranges: Vec<std::ops::Range<u32>>,
    },
    /// Stop following a document.
    Close(DocId),
    /// A marker that comes back once everything before it is done.
    Echo(u64),
}

/// What the worker has to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Highlight runs for a document, as of a version.
    Highlights {
        /// Which document.
        id: DocId,
        /// Which version they were computed from.
        version: u64,
        /// The char range they cover.
        window: std::ops::Range<u32>,
        /// The runs, in order, not overlapping.
        spans: Vec<Span>,
    },
    /// A language was switched off for a document, and why.
    Disabled {
        /// Which document.
        id: DocId,
        /// The language that misbehaved.
        language: &'static str,
        /// What it did.
        why: String,
    },
    /// What a document declares, in the order it declares it.
    Symbols {
        /// Which document.
        id: DocId,
        /// Which version they were read from.
        version: u64,
        /// The outline.
        symbols: Vec<crate::Symbol>,
    },
    /// Where a document can be folded, as of a version. Sent after every
    /// update, since any edit can move, make or remove a region.
    Folds {
        /// Which document.
        id: DocId,
        /// Which version they were read from.
        version: u64,
        /// The regions, in order of their header lines.
        folds: Vec<crate::FoldRange>,
    },
    /// The ranges from [`Request::Grow`], grown, in the same order.
    Grown {
        /// Which document.
        id: DocId,
        /// Which version they were grown in.
        version: u64,
        /// The serial of the [`Request::Grow`] this answers.
        serial: u64,
        /// The grown ranges, or `None` when the language is switched off.
        ranges: Option<Vec<std::ops::Range<u32>>>,
    },
    /// The marker from [`Request::Echo`].
    Echo(u64),
}

/// A thread parsing documents.
///
/// Dropping it stops the thread once the work it has is done.
#[derive(Debug)]
pub struct Worker {
    sender: Sender<Request>,
}

impl Worker {
    /// Start a worker reporting through `report`.
    ///
    /// `report` runs on the worker's thread, so it should do nothing but hand
    /// the message on to the editor's event channel.
    #[must_use]
    pub fn new(report: Box<dyn Fn(Reply) + Send + 'static>) -> Self {
        let (sender, receiver) = mpsc::channel::<Request>();
        thread::spawn(move || {
            let mut documents: HashMap<DocId, Document> = HashMap::new();
            while let Ok(first) = receiver.recv() {
                // Everything waiting is taken at once, so that typing faster
                // than the parser can keep up coalesces into one parse of the
                // latest text rather than a queue of stale ones.
                let mut batch = vec![first];
                batch.extend(receiver.try_iter());
                for request in coalesce(batch) {
                    for reply in handle(&mut documents, request) {
                        report(reply);
                    }
                }
            }
        });
        Self { sender }
    }

    /// Ask for something. A worker that has gone — only at shutdown — drops it.
    pub fn send(&self, request: Request) {
        let _ = self.sender.send(request);
    }
}

/// Fold requests that a later one in the same batch supersedes into it.
///
/// Only the newest text and the newest window for a document are worth
/// parsing — an earlier one would be parsed only to be thrown away. Folding
/// them together is not the same as dropping the earlier ones, though, and the
/// difference is the whole reason this is not a filter:
///
/// An `Update` carries the edit that produced its text, in the coordinates of
/// the text *before* it. Two updates in one batch therefore describe two
/// different starting points, and the second one's edit is meaningless to a
/// parser that never saw the first one's text. The survivor keeps the newest
/// text and forgets the edit: that costs one parse from scratch, where
/// believing the edit would leave the tree quietly wrong for the rest of the
/// session.
///
/// A `Window` carries no text at all, only a version and a range. Letting one
/// replace an `Update` would throw the new text away while still labelling the
/// answer with the new version — so the editor would accept a reply computed
/// from text it had already moved past. It merges into the `Update` instead.
fn coalesce(batch: Vec<Request>) -> Vec<Request> {
    let mut out: Vec<Request> = Vec::with_capacity(batch.len());
    // Where a document's surviving request sits in `out`, while it can still
    // take another one. An `Open` or a `Close` ends that: the requests either
    // side of one are about different text, whatever the id says.
    let mut absorbing: HashMap<DocId, usize> = HashMap::new();
    for request in batch {
        match request {
            Request::Update { id, .. } | Request::Window { id, .. } => {
                if let Some(&at) = absorbing.get(&id) {
                    absorb(&mut out[at], request);
                } else {
                    absorbing.insert(id, out.len());
                    out.push(request);
                }
            }
            // None of these is folded, and none may be folded past: each marks
            // a point the document's text must stand still at. The requests
            // either side of an open or a close are about different text,
            // whatever the id says.
            //
            // An outline or a grow folded backwards would move in front of an
            // `Update` in the same batch and read the text that update was
            // about to replace — the same hazard the `Window` rule above exists
            // to avoid. And a later `Update` folded into one before it would
            // jump the queue the other way, so the question is answered from
            // text newer than the version it names.
            Request::Open { id, .. }
            | Request::Close(id)
            | Request::Symbols { id, .. }
            | Request::Grow { id, .. } => {
                absorbing.remove(&id);
                out.push(request);
            }
            // An echo keeps its place because its place is the whole point of
            // it; it reads no text, so folding across it is harmless.
            Request::Echo(_) => out.push(request),
        }
    }
    out
}

/// Fold `later` into `survivor`, which is about the same document.
fn absorb(survivor: &mut Request, later: Request) {
    match later {
        // New text replaces whatever was there. Its edit survives only if what
        // it replaces carried none of its own, since two edits in sequence
        // describe two different texts and the parser has seen neither.
        Request::Update { id, version, text, edit, window } => {
            let edit = match survivor {
                Request::Update { .. } => None,
                _ => edit,
            };
            *survivor = Request::Update { id, version, text, edit, window };
        }
        // No new text: only the version being looked at, and the part of it.
        Request::Window { version: newer, window: moved, .. } => match survivor {
            Request::Update { version, window, .. } | Request::Window { version, window, .. } => {
                *version = newer;
                *window = moved;
            }
            _ => {}
        },
        // Nothing else is ever absorbed; `coalesce` sends only the two above.
        _ => {}
    }
}

fn handle(documents: &mut HashMap<DocId, Document>, request: Request) -> Vec<Reply> {
    match request {
        Request::Open { id, language, text } => {
            documents.insert(id, Document::new(language, text));
            Vec::new()
        }
        Request::Update { id, version, text, edit, window } => {
            let Some(document) = documents.get_mut(&id) else { return Vec::new() };
            document.update(text, edit);
            let mut replies = highlights(id, version, window, document);
            // Read from the parse the highlights just made, so it costs a walk
            // of the tree and not another parse.
            if let Some(folds) = document.folds() {
                replies.push(Reply::Folds { id, version, folds });
            }
            replies
        }
        Request::Window { id, version, window } => {
            let Some(document) = documents.get_mut(&id) else { return Vec::new() };
            highlights(id, version, window, document)
        }
        Request::Symbols { id, version } => {
            let Some(document) = documents.get_mut(&id) else { return Vec::new() };
            // A grammar that timed out or panicked still owes an answer:
            // without the first the palette waits for ever, and without the
            // second the document loses its colours with nothing said.
            let Some(symbols) = document.symbols() else {
                let mut replies = vec![Reply::Symbols { id, version, symbols: Vec::new() }];
                if let Some(trouble) = document.trouble() {
                    replies.push(Reply::Disabled {
                        id,
                        language: document.language().name,
                        why: trouble.to_string(),
                    });
                }
                return replies;
            };
            vec![Reply::Symbols { id, version, symbols }]
        }
        Request::Grow { id, version, serial, ranges } => {
            // Always answered, even for a document that is not being followed:
            // the editor is waiting to know what to select.
            let ranges = documents.get_mut(&id).and_then(|document| document.grow(&ranges));
            vec![Reply::Grown { id, version, serial, ranges }]
        }
        Request::Close(id) => {
            documents.remove(&id);
            Vec::new()
        }
        Request::Echo(marker) => vec![Reply::Echo(marker)],
    }
}

fn highlights(
    id: DocId,
    version: u64,
    window: std::ops::Range<u32>,
    document: &mut Document,
) -> Vec<Reply> {
    match document.highlights(window.clone()) {
        Some(spans) => vec![Reply::Highlights { id, version, window, spans }],
        None => document.trouble().map_or_else(Vec::new, |trouble| {
            vec![Reply::Disabled {
                id,
                language: document.language().name,
                why: trouble.to_string(),
            }]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(id: DocId, version: u64, text: &str, edit: Option<TextEdit>) -> Request {
        Request::Update { id, version, text: Rope::from_str(text), edit, window: 0..10 }
    }

    fn edit(start: u32, old_end: u32, new_end: u32) -> TextEdit {
        TextEdit { start, old_end, new_end }
    }

    fn window(id: DocId, version: u64, window: std::ops::Range<u32>) -> Request {
        Request::Window { id, version, window }
    }

    #[track_caller]
    fn as_update(request: &Request) -> (u64, String, Option<TextEdit>, std::ops::Range<u32>) {
        match request {
            Request::Update { version, text, edit, window, .. } => {
                (*version, text.to_string(), *edit, window.clone())
            }
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[track_caller]
    fn as_window(request: &Request) -> (DocId, u64, std::ops::Range<u32>) {
        match request {
            Request::Window { id, version, window } => (*id, *version, window.clone()),
            other => panic!("expected a window, got {other:?}"),
        }
    }

    #[test]
    fn a_grammar_in_trouble_still_answers_an_outline_and_says_what_happened() {
        // Silence would leave the editor waiting for an outline for ever, and
        // would switch a document's colours off with nothing said.
        let language = crate::language::of_name("rust").expect("rust is compiled in");
        // Big enough that the parser checks its progress at all — a handful of
        // bytes finishes before the first check, whatever the budget is.
        let mut text = String::new();
        for index in 0..20_000 {
            use std::fmt::Write;
            let _ = writeln!(text, "fn function{index}() {{ let x = {index}; }}");
        }
        let mut documents = HashMap::new();
        documents.insert(
            1,
            Document::new(language, Rope::from_str(&text)).with_budget(std::time::Duration::ZERO),
        );

        let replies = handle(&mut documents, Request::Symbols { id: 1, version: 4 });
        assert!(
            matches!(replies.first(), Some(Reply::Symbols { version: 4, symbols, .. }) if symbols.is_empty()),
            "an empty outline comes back: {replies:?}"
        );
        assert!(
            matches!(replies.get(1), Some(Reply::Disabled { .. })),
            "and the trouble is reported: {replies:?}"
        );
    }

    #[test]
    fn an_outline_is_not_folded_in_front_of_the_text_it_reads() {
        // Folding it backwards would put it before the update in the same
        // batch and read the text that update was about to replace.
        let folded = coalesce(vec![
            Request::Symbols { id: 1, version: 4 },
            update(1, 5, "fn second() {}", None),
            Request::Symbols { id: 1, version: 5 },
        ]);
        let last = folded.last().expect("something survived");
        assert!(
            matches!(last, Request::Symbols { version: 5, .. }),
            "the outline stays behind the update: {folded:?}"
        );
    }

    #[test]
    fn a_grow_is_not_folded_in_front_of_the_text_it_reads() {
        let folded = coalesce(vec![
            update(1, 5, "fn a() {}", None),
            Request::Grow { id: 1, version: 5, serial: 1, ranges: vec![0..0, 3..3] },
            update(1, 6, "fn ab() {}", None),
        ]);
        assert_eq!(folded.len(), 3, "the grow splits the updates: {folded:?}");
        assert!(matches!(folded[1], Request::Grow { version: 5, .. }));
    }

    #[test]
    fn an_update_after_an_outline_is_not_folded_in_front_of_it() {
        let folded = coalesce(vec![
            update(1, 5, "fn a() {}", None),
            Request::Symbols { id: 1, version: 5 },
            update(1, 6, "fn ab() {}", None),
        ]);
        assert_eq!(folded.len(), 3, "the outline reads version 5's text: {folded:?}");
        assert_eq!(as_update(&folded[0]).1, "fn a() {}");
    }

    #[test]
    fn a_grow_for_a_document_nobody_follows_is_still_answered() {
        // The editor waits for the answer before it will grow again.
        let replies = handle(
            &mut HashMap::new(),
            Request::Grow { id: 9, version: 1, serial: 2, ranges: vec![0..0, 3..3] },
        );
        assert_eq!(replies, [Reply::Grown { id: 9, version: 1, serial: 2, ranges: None }]);
    }

    #[test]
    fn one_request_is_left_alone() {
        let folded = coalesce(vec![update(1, 5, "fn main() {}", Some(edit(0, 0, 12)))]);
        assert_eq!(folded.len(), 1);
        let (version, text, edit, _) = as_update(&folded[0]);
        assert_eq!((version, text.as_str()), (5, "fn main() {}"));
        assert_eq!(edit, Some(TextEdit { start: 0, old_end: 0, new_end: 12 }));
    }

    #[test]
    fn two_updates_keep_the_newer_text_and_forget_the_edit() {
        // The second edit is in the first one's coordinates, and the parser
        // has seen neither text. Describing the jump as one edit would be a
        // lie the tree never recovers from.
        let folded = coalesce(vec![
            update(1, 5, "let a = 1;", Some(edit(0, 0, 10))),
            update(1, 6, "let ab = 1;", Some(edit(6, 6, 7))),
        ]);
        assert_eq!(folded.len(), 1);
        let (version, text, edit, _) = as_update(&folded[0]);
        assert_eq!((version, text.as_str()), (6, "let ab = 1;"));
        assert_eq!(edit, None, "a reparse from scratch is the only honest answer");
    }

    #[test]
    fn a_window_does_not_throw_away_the_text_it_follows() {
        // The bug this is here for: a `Window` replacing an `Update` lost the
        // new text but kept the new version, so the reply was computed from
        // text the editor had moved past and was labelled as current.
        let folded =
            coalesce(vec![update(1, 5, "let ab = 1;", Some(edit(6, 6, 7))), window(1, 6, 40..80)]);
        assert_eq!(folded.len(), 1);
        let (version, text, edit, window) = as_update(&folded[0]);
        assert_eq!(text, "let ab = 1;", "the text survives the window moving");
        assert_eq!(
            edit,
            Some(TextEdit { start: 6, old_end: 6, new_end: 7 }),
            "and so does its edit"
        );
        assert_eq!((version, window), (6, 40..80), "but the newer window wins");
    }

    #[test]
    fn a_window_after_two_updates_still_has_no_edit() {
        let folded = coalesce(vec![
            update(1, 5, "one", Some(edit(0, 0, 3))),
            update(1, 6, "two", Some(edit(0, 3, 3))),
            window(1, 7, 0..3),
        ]);
        assert_eq!(folded.len(), 1);
        let (version, text, edit, window) = as_update(&folded[0]);
        assert_eq!((text.as_str(), edit), ("two", None));
        assert_eq!((version, window), (7, 0..3));
    }

    #[test]
    fn windows_alone_fold_into_the_last_of_them() {
        let folded = coalesce(vec![window(1, 5, 0..10), window(1, 6, 20..30)]);
        assert_eq!(folded.len(), 1);
        assert_eq!(as_window(&folded[0]), (1, 6, 20..30));
    }

    #[test]
    fn documents_do_not_fold_into_each_other() {
        let folded = coalesce(vec![
            update(1, 5, "first", None),
            update(2, 5, "second", None),
            update(1, 6, "first again", None),
        ]);
        assert_eq!(folded.len(), 2);
        assert_eq!(as_update(&folded[0]).1, "first again");
        assert_eq!(as_update(&folded[1]).1, "second");
    }

    #[test]
    fn a_close_stops_the_folding() {
        // Text either side of a close is about two different documents that
        // happen to share a number, so the later one must not absorb the
        // earlier — and the close itself has to stay between them.
        let folded = coalesce(vec![
            update(1, 5, "before", None),
            Request::Close(1),
            update(1, 1, "after", Some(edit(0, 0, 5))),
        ]);
        assert_eq!(folded.len(), 3);
        assert_eq!(as_update(&folded[0]).1, "before");
        assert!(matches!(folded[1], Request::Close(1)));
        let (_, text, edit, _) = as_update(&folded[2]);
        assert_eq!(
            (text.as_str(), edit),
            ("after", Some(TextEdit { start: 0, old_end: 0, new_end: 5 }))
        );
    }

    #[test]
    fn an_open_stops_the_folding_too() {
        let language = crate::language::of_name("rust").expect("rust is compiled in");
        let folded = coalesce(vec![
            update(1, 5, "before", None),
            Request::Open { id: 1, language, text: Rope::from_str("after") },
            window(1, 1, 0..5),
        ]);
        assert_eq!(folded.len(), 3);
        assert_eq!(as_update(&folded[0]).1, "before");
        assert!(matches!(folded[1], Request::Open { .. }));
        assert_eq!(as_window(&folded[2]), (1, 1, 0..5), "the window is not folded across the open");
    }

    #[test]
    fn an_echo_keeps_its_place_behind_the_work_it_marks() {
        let folded =
            coalesce(vec![update(1, 5, "one", None), update(1, 6, "two", None), Request::Echo(99)]);
        assert_eq!(folded.len(), 2);
        assert_eq!(as_update(&folded[0]).1, "two");
        assert!(matches!(folded[1], Request::Echo(99)));
    }
}
