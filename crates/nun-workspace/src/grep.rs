//! Searching the project's text, off the thread that draws.
//!
//! A project search is unbounded in a way a directory listing is not: the
//! answer to "where is `fn render`" in a large repository is thousands of
//! lines from tens of thousands of files, and the user has usually typed
//! another character before the first one arrives. So this does two things
//! the file tree's worker does not need to.
//!
//! It **streams**. Hits go out in small batches as they are found, so the
//! panel fills from the top while the walk is still running, rather than
//! staying blank and then appearing all at once.
//!
//! It **cancels**. Every search carries a generation, and a newer generation
//! makes an older one stop — between files, and between the lines of a file.
//! A cancelled search says so and sends nothing more, so the panel can throw
//! away what it has and start again without waiting for a walk it no longer
//! cares about.
//!
//! It owns a thread of its own rather than sharing [`crate::jobs::Jobs`],
//! because a search of a large repository would otherwise sit in front of the
//! directory listings the tree is waiting on, and the tree would freeze for
//! as long as the search took.
//!
//! The matching and the walking are ripgrep's own crates, in process. Shelling
//! out to `rg` would mean parsing its output, inheriting a child process that
//! outlives a cancelled search, and depending on something being installed.
//!
//! A matching line is one [`Hit`], because the panel shows lines and a line
//! matching six times is one row to click. But the hit carries all six ranges,
//! because a project-wide replace is built on this and a replace that changed
//! the first match and left the rest is the kind of bug found after the commit.

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;

/// Most characters of one line a hit carries.
///
/// A minified bundle is one line a megabyte long, and neither the channel nor
/// the panel wants it. The window slides to keep the match itself inside, so a
/// hit on such a line is still worth showing.
pub const MOST_CHARS: usize = 1_000;

/// How many hits gather before a batch goes out.
const BATCH_HITS: usize = 50;

/// How long hits gather before a batch goes out, however few there are.
///
/// Short enough that a search matching one line per second still looks alive,
/// long enough that a search matching everything does not flood the channel
/// with one message per hit.
const BATCH_EVERY: Duration = Duration::from_millis(30);

/// How a query treats case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Case {
    /// `Foo` matches `Foo` and nothing else.
    Sensitive,
    /// `foo` matches `Foo`, `FOO` and `foo`.
    Insensitive,
    /// Insensitive until the user types a capital, which reads as meaning it.
    #[default]
    Smart,
}

/// What to search for, and how.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Options {
    /// What the user typed. An empty query finds nothing, rather than every
    /// line of the project.
    pub query: String,
    /// Whether `query` is a regular expression rather than literal text.
    pub regex: bool,
    /// How to treat case.
    pub case: Case,
    /// Whether a match must stand alone rather than inside a longer word.
    pub whole_word: bool,
    /// Whether to search files the ignore rules would normally hide — the
    /// ignore files, the hidden entries, and what a parent directory excludes.
    /// `.git` itself stays out either way.
    pub include_ignored: bool,
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Which file, relative to the root that was searched.
    pub path: PathBuf,
    /// Which line, counting from one, as an editor does.
    pub line: u32,
    /// Where the *first* match on the line starts, counting characters from
    /// one — not bytes, so it is a column a cursor can be put at. It is a
    /// position in the real line, not in `text`, and it does not move when
    /// `text` is windowed.
    pub column: u32,
    /// The line itself, with its terminator removed and capped at
    /// [`MOST_CHARS`] characters around the first match.
    pub text: String,
    /// Every match on the line, in order, as character offsets into `text`.
    /// Never empty.
    ///
    /// On a line long enough for `text` to be a window onto it, this is a
    /// subset: a match outside the window is dropped rather than given a range
    /// that does not index `text`. So anything that *rewrites* the line — a
    /// project-wide replace — must find the matches in the real line again,
    /// and must not take this for the whole set.
    pub matched: Vec<Range<u32>>,
}

/// News from a search in progress.
///
/// Every search produces exactly one terminal message — [`Found::Done`] or
/// [`Found::Failed`] — so a caller can always tell when a generation is
/// finished with, including one that was superseded before it began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Some more hits, in the order they were found.
    Hits {
        /// Which search asked.
        generation: u64,
        /// The hits themselves.
        hits: Vec<Hit>,
    },
    /// The search is over.
    Done {
        /// Which search asked.
        generation: u64,
        /// How many *lines* matched in all — rows for the panel, not
        /// individual matches. A line matching three times counts once, the
        /// same way it is one [`Hit`].
        hits: usize,
        /// How many files had at least one of them.
        files: usize,
        /// Whether a newer search stopped this one part way.
        cancelled: bool,
    },
    /// The search never ran, and this is why, as a sentence — an unparseable
    /// regular expression, usually, which is what a half-typed one is.
    Failed {
        /// Which search asked.
        generation: u64,
        /// What to show the user.
        error: String,
    },
}

/// A worker searching the project's text and reporting back.
///
/// Dropping it cancels whatever is in flight and stops the worker.
#[derive(Debug)]
pub struct Grep {
    sender: Sender<Request>,
    /// The newest generation asked for. The worker compares against it rather
    /// than draining a channel, so a search already inside a file notices.
    current: Arc<AtomicU64>,
}

/// One search, on its way to the worker.
#[derive(Debug)]
struct Request {
    root: PathBuf,
    options: Options,
    generation: u64,
}

impl Grep {
    /// Start a worker that reports through `report`.
    ///
    /// `report` is called on the worker's thread, so it should do nothing but
    /// hand the message on — post it to the editor's event channel and let the
    /// main thread act on it.
    #[must_use]
    pub fn new(report: Box<dyn Fn(Found) + Send + 'static>) -> Self {
        let (sender, receiver) = mpsc::channel::<Request>();
        let current = Arc::new(AtomicU64::new(0));
        let theirs = Arc::clone(&current);
        thread::spawn(move || {
            while let Ok(request) = receiver.recv() {
                run(&request, &theirs, report.as_ref());
            }
        });
        Self { sender, current }
    }

    /// Search `root` for `options`, cancelling anything older.
    ///
    /// `generation` identifies the search and must increase: it is how a late
    /// answer to a query the user has typed past is recognised, and how the
    /// search in flight is told to stop. A generation that is not higher than
    /// the current one is stale on arrival and comes straight back as a
    /// cancelled [`Found::Done`].
    pub fn search(&self, root: impl Into<PathBuf>, options: Options, generation: u64) {
        self.current.fetch_max(generation, Ordering::SeqCst);
        // A worker that has gone — only at shutdown — drops the request
        // rather than failing a keystroke.
        let _ = self.sender.send(Request { root: root.into(), options, generation });
    }

    /// Stop whatever is in flight without starting anything.
    ///
    /// For closing the panel: the search has no one left to report to, and a
    /// large repository would otherwise be walked to the end for nothing.
    pub fn cancel(&self) {
        self.current.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for Grep {
    fn drop(&mut self) {
        // No generation can beat this, so a search inside a file stops at its
        // next line rather than walking the rest of the project into a
        // callback nobody is listening to.
        self.current.store(u64::MAX, Ordering::SeqCst);
    }
}

/// Run one search to its end, or until a newer one supersedes it.
fn run(request: &Request, current: &AtomicU64, report: &dyn Fn(Found)) {
    let generation = request.generation;
    let done = |hits, files, cancelled| {
        report(Found::Done { generation, hits, files, cancelled });
    };

    if current.load(Ordering::SeqCst) != generation {
        done(0, 0, true);
        return;
    }
    if request.options.query.is_empty() {
        done(0, 0, false);
        return;
    }

    let matcher = match build_matcher(&request.options) {
        Ok(matcher) => matcher,
        Err(error) => {
            report(Found::Failed { generation, error });
            return;
        }
    };

    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        // Stop at the first NUL rather than spilling a binary file's bytes
        // into the panel. `quit` abandons the file; `convert` would keep
        // searching it with the NULs replaced, which is not what a code
        // search wants.
        .binary_detection(BinaryDetection::quit(0))
        .build();

    let mut stream = Stream {
        report,
        current,
        generation,
        pending: Vec::new(),
        since: Instant::now(),
        hits: 0,
        files: 0,
    };

    let mut cancelled = false;
    for entry in walk(&request.root, request.options.include_ignored) {
        if !stream.live() {
            cancelled = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(&request.root) else { continue };

        let mut collect =
            Collect { stream: &mut stream, matcher: &matcher, path: relative, any: false };
        // A file that cannot be read — a permission, a broken symlink, a
        // device — is skipped rather than ending the search. One unreadable
        // file is not a reason to stop answering the question.
        let _ = searcher.search_path(&matcher, entry.path(), &mut collect);
        if collect.any {
            stream.files += 1;
        }
    }

    if cancelled || !stream.live() {
        // Whatever is pending belongs to a question nobody is asking any more.
        done(stream.hits, stream.files, true);
    } else {
        stream.flush();
        done(stream.hits, stream.files, false);
    }
}

/// The walk one search makes over `root`.
fn walk(root: &Path, include_ignored: bool) -> ignore::Walk {
    let mut builder = WalkBuilder::new(root);
    builder.standard_filters(!include_ignored).follow_links(false);
    // `.git` stays out even when the ignore rules are off: its packed objects
    // and logs are not project text, and nobody asking for a symbol wants a
    // hundred hits from a reflog.
    builder.filter_entry(|entry| entry.file_name() != ".git");
    builder.build()
}

/// Turn the options into something that can match a line.
///
/// Literal queries go through the same regex engine with `fixed_strings`
/// rather than being escaped by hand, so `.`, `(` and `\` in a query mean
/// themselves without a second escaping rule to keep true.
fn build_matcher(options: &Options) -> Result<RegexMatcher, String> {
    let insensitive = match options.case {
        Case::Sensitive => false,
        Case::Insensitive => true,
        Case::Smart => !shouts(&options.query, options.regex),
    };
    RegexMatcherBuilder::new()
        .fixed_strings(!options.regex)
        .case_insensitive(insensitive)
        .word(options.whole_word)
        .line_terminator(Some(b'\n'))
        .build(&options.query)
        .map_err(|error| error.to_string())
}

/// Whether the query shouts: an uppercase character the user typed on purpose.
///
/// In a regular expression the character after a backslash is not the user
/// shouting — `\W` and `\S` are classes, not capitals — so an escape and what
/// it escapes are skipped together.
fn shouts(query: &str, regex: bool) -> bool {
    let mut chars = query.chars();
    while let Some(c) = chars.next() {
        if regex && c == '\\' {
            chars.next();
            continue;
        }
        if c.is_uppercase() {
            return true;
        }
    }
    false
}

/// The state one search accumulates: what to send, and when it last sent.
struct Stream<'a> {
    report: &'a dyn Fn(Found),
    current: &'a AtomicU64,
    generation: u64,
    pending: Vec<Hit>,
    since: Instant,
    hits: usize,
    files: usize,
}

impl Stream<'_> {
    /// Whether this search is still the one being asked for.
    fn live(&self) -> bool {
        self.current.load(Ordering::SeqCst) == self.generation
    }

    fn push(&mut self, hit: Hit) {
        self.hits += 1;
        self.pending.push(hit);
        if self.pending.len() >= BATCH_HITS || self.since.elapsed() >= BATCH_EVERY {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let hits = std::mem::take(&mut self.pending);
        (self.report)(Found::Hits { generation: self.generation, hits });
        self.since = Instant::now();
    }
}

/// The sink for one file: turns matching lines into hits on the stream.
struct Collect<'s, 'a> {
    stream: &'s mut Stream<'a>,
    matcher: &'s RegexMatcher,
    path: &'s Path,
    /// Whether this file had a hit, so the file count means files with hits.
    any: bool,
}

impl Sink for Collect<'_, '_> {
    type Error = io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        matched: &SinkMatch<'_>,
    ) -> Result<bool, io::Error> {
        // Checked per line as well as per file: one generated file can hold
        // more matching lines than a whole project of hand-written ones.
        if !self.stream.live() {
            return Ok(false);
        }
        if let Some(hit) = to_hit(self.matcher, self.path, matched) {
            self.any = true;
            self.stream.push(hit);
        }
        Ok(true)
    }
}

/// Build the hit for one matching line, or nothing if it cannot be placed.
///
/// A line is one hit however many times it matches, because the panel shows
/// lines — but it carries every match, because a replace has to change all of
/// them and a panel that only wanted the first can ignore the rest.
fn to_hit(matcher: &RegexMatcher, path: &Path, whole: &SinkMatch<'_>) -> Option<Hit> {
    let raw = trim_terminator(whole.bytes());
    let line = u32::try_from(whole.line_number()?).unwrap_or(u32::MAX);
    let spans = spans(matcher, raw)?;
    let &(first, first_ends) = spans.first()?;

    let text = String::from_utf8_lossy(raw);
    let (text, from) = window(&text, first, first_ends);
    let width = text.chars().count();

    // The first match is always carried, clipped to the window if it is itself
    // longer than one, so a hit never arrives with nothing to highlight. The
    // rest are dropped when they fall outside `text` rather than being given a
    // range that does not index it.
    let mut ranges = vec![span(first, first_ends, from, width)];
    ranges.extend(
        spans[1..]
            .iter()
            .filter(|(_, closes)| *closes <= from + width)
            .map(|&(opens, closes)| span(opens, closes, from, width)),
    );

    let column = u32::try_from(first + 1).unwrap_or(u32::MAX);
    Some(Hit { path: path.to_path_buf(), line, column, text, matched: ranges })
}

/// Every match on `raw`, in order, as character offsets into it.
///
/// Counted on the text rather than the bytes, because a column is where a
/// cursor goes and a cursor moves by characters. A file that is not valid
/// UTF-8 has already been made into one that is, by the same replacement the
/// buffer would use.
///
/// The count walks the line once for all the matches together: matches come
/// back in order and do not overlap, so each one only has to count on from
/// where the last one ended. Counting each match from the start of the line
/// would make a generated line with a thousand matches quadratic.
fn spans(matcher: &RegexMatcher, raw: &[u8]) -> Option<Vec<(usize, usize)>> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut cursor = 0;
    let mut counted = 0;
    matcher
        .find_iter(raw, |found| {
            let (start, end) = (found.start().min(raw.len()), found.end().min(raw.len()));
            if start < cursor {
                return true;
            }
            counted += chars(&raw[cursor..start]);
            let opens = counted;
            counted += chars(&raw[start..end]);
            cursor = end;
            spans.push((opens, counted));
            true
        })
        .ok()?;
    Some(spans)
}

/// One match, moved into `text`'s frame and clipped to its width.
fn span(opens: usize, closes: usize, from: usize, width: usize) -> Range<u32> {
    let opens = u32::try_from(opens.saturating_sub(from)).unwrap_or(u32::MAX);
    let closes = u32::try_from(closes.saturating_sub(from).min(width)).unwrap_or(u32::MAX);
    opens..closes.max(opens)
}

/// How many characters some bytes are, once they are text.
fn chars(bytes: &[u8]) -> usize {
    String::from_utf8_lossy(bytes).chars().count()
}

/// At most [`MOST_CHARS`] characters of `line`, keeping the match in view.
///
/// Returns the text and the character offset it starts at, so a caller can
/// turn a position in the line into a position in the text. A match beyond the
/// cap slides the window rather than falling off the end of it, with a quarter
/// of the window kept in front for context.
fn window(line: &str, start: usize, end: usize) -> (String, usize) {
    if line.chars().count() <= MOST_CHARS {
        return (line.to_owned(), 0);
    }
    if end <= MOST_CHARS {
        return (line.chars().take(MOST_CHARS).collect(), 0);
    }
    let from = start.saturating_sub(MOST_CHARS / 4);
    (line.chars().skip(from).take(MOST_CHARS).collect(), from)
}

/// The line without whatever ended it.
fn trim_terminator(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    /// A grep whose reports land in a channel this thread can read.
    fn grep() -> (Grep, Receiver<Found>) {
        let (sender, receiver) = mpsc::channel();
        let grep = Grep::new(Box::new(move |found| {
            let _ = sender.send(found);
        }));
        (grep, receiver)
    }

    /// Everything one generation reported, up to and including its terminal
    /// message.
    fn drain(receiver: &Receiver<Found>, generation: u64) -> Vec<Found> {
        let mut all = Vec::new();
        loop {
            let found =
                receiver.recv_timeout(Duration::from_secs(30)).expect("the worker answered");
            let last = match &found {
                Found::Done { generation: g, .. } | Found::Failed { generation: g, .. } => {
                    *g == generation
                }
                Found::Hits { .. } => false,
            };
            all.push(found);
            if last {
                return all;
            }
        }
    }

    /// Every hit one generation sent, in order.
    fn hits(found: &[Found], generation: u64) -> Vec<Hit> {
        found
            .iter()
            .filter_map(|one| match one {
                Found::Hits { generation: g, hits } if *g == generation => Some(hits.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// The terminal message, which every search has exactly one of.
    fn finish(found: &[Found], generation: u64) -> Found {
        found
            .iter()
            .find(|one| {
                matches!(one,
                    Found::Done { generation: g, .. } | Found::Failed { generation: g, .. }
                        if *g == generation)
            })
            .expect("a search always finishes")
            .clone()
    }

    /// Search `dir` once and hand back everything it said.
    fn once(dir: &Path, options: Options) -> (Vec<Hit>, Found) {
        let (grep, receiver) = grep();
        grep.search(dir, options, 1);
        let found = drain(&receiver, 1);
        (hits(&found, 1), finish(&found, 1))
    }

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    /// The characters of `text` that `range` names — what a panel would
    /// highlight, and what a replace would overwrite.
    fn cut(text: &str, range: &Range<u32>) -> String {
        text.chars().skip(range.start as usize).take((range.end - range.start) as usize).collect()
    }

    /// Every match on a hit, cut out of its text.
    fn cuts(hit: &Hit) -> Vec<String> {
        hit.matched.iter().map(|range| cut(&hit.text, range)).collect()
    }

    fn literal(query: &str) -> Options {
        Options { query: query.into(), case: Case::Sensitive, ..Options::default() }
    }

    #[test]
    fn a_literal_query_means_itself() {
        let dir = project(&[("a.rs", "let x = 1;\nfn main() {}\n"), ("b.rs", "fn main() {}\n")]);
        let (hits, done) = once(dir.path(), literal("fn main()"));

        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.line == 1 || hit.line == 2));
        assert_eq!(done, Found::Done { generation: 1, hits: 2, files: 2, cancelled: false });
    }

    #[test]
    fn a_literal_query_does_not_smuggle_a_regex_in() {
        let dir = project(&[("a.txt", "a.c\nabc\n")]);
        let (hits, _) = once(dir.path(), literal("a.c"));

        assert_eq!(hits.len(), 1, "the dot is a dot: {hits:?}");
        assert_eq!(hits[0].text, "a.c");
    }

    #[test]
    fn a_regex_query_matches_a_pattern() {
        let dir = project(&[("a.rs", "fn one() {}\nfn two() {}\nlet three = 3;\n")]);
        let options =
            Options { regex: true, case: Case::Sensitive, ..literal(r"^fn \w+\(\) \{\}$") };
        let (hits, _) = once(dir.path(), options);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits.iter().map(|hit| hit.line).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn an_invalid_regex_comes_back_as_a_sentence() {
        let dir = project(&[("a.rs", "fn main() {}\n")]);
        let options = Options { regex: true, ..literal("fn main(") };
        let (hits, done) = once(dir.path(), options);

        assert!(hits.is_empty());
        match done {
            Found::Failed { generation, error } => {
                assert_eq!(generation, 1);
                assert!(!error.is_empty(), "the panel has something to show");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_query_finds_nothing_rather_than_everything() {
        let dir = project(&[("a.rs", "one\ntwo\nthree\n")]);
        let (hits, done) = once(dir.path(), literal(""));

        assert!(hits.is_empty());
        assert_eq!(done, Found::Done { generation: 1, hits: 0, files: 0, cancelled: false });
    }

    #[test]
    fn a_sensitive_query_ignores_the_wrong_case() {
        let dir = project(&[("a.rs", "Needle\nneedle\nNEEDLE\n")]);
        let (hits, _) = once(dir.path(), literal("needle"));

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
    }

    #[test]
    fn an_insensitive_query_takes_every_case() {
        let dir = project(&[("a.rs", "Needle\nneedle\nNEEDLE\n")]);
        let options = Options { case: Case::Insensitive, ..literal("needle") };
        let (hits, _) = once(dir.path(), options);

        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn smart_case_is_insensitive_until_the_user_types_a_capital() {
        let dir = project(&[("a.rs", "Needle\nneedle\nNEEDLE\n")]);

        let lower = Options { case: Case::Smart, ..literal("needle") };
        let (hits, _) = once(dir.path(), lower);
        assert_eq!(hits.len(), 3, "lowercase asks for all of them");

        let upper = Options { case: Case::Smart, ..literal("Needle") };
        let (hits, _) = once(dir.path(), upper);
        assert_eq!(hits.len(), 1, "a capital means it: {hits:?}");
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn smart_case_does_not_hear_a_regex_escape_as_a_capital() {
        assert!(!shouts(r"\wfoo", true));
        assert!(shouts(r"\wFoo", true));
        assert!(shouts(r"\W", false), "outside a regex a backslash escapes nothing");
        assert!(!shouts("foo", true));
    }

    #[test]
    fn a_whole_word_query_will_not_match_inside_a_longer_word() {
        let dir = project(&[("a.rs", "let value = 1;\nlet revalued = 2;\nvalue()\n")]);
        let options = Options { whole_word: true, ..literal("value") };
        let (hits, _) = once(dir.path(), options);

        assert_eq!(hits.iter().map(|hit| hit.line).collect::<Vec<_>>(), vec![1, 3]);
    }

    #[test]
    fn an_ignored_file_is_skipped_until_it_is_asked_for() {
        let dir = project(&[
            (".gitignore", "target/\n"),
            ("target/built.rs", "needle\n"),
            ("src/main.rs", "needle\n"),
        ]);
        // A `.gitignore` only speaks for a repository, so there has to be one
        // — the same rule ripgrep follows, and the same one the file tree's
        // own listing already follows.
        std::fs::create_dir(dir.path().join(".git")).unwrap();

        let (hits, _) = once(dir.path(), literal("needle"));
        assert_eq!(hits.len(), 1, "the ignore rules hold by default: {hits:?}");
        assert!(hits[0].path.starts_with("src"));

        let options = Options { include_ignored: true, ..literal("needle") };
        let (mut hits, _) = once(dir.path(), options);
        hits.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(hits.len(), 2, "the override reaches them: {hits:?}");
    }

    #[test]
    fn the_git_directory_stays_out_even_when_ignored_files_are_asked_for() {
        let dir = project(&[(".git/COMMIT_EDITMSG", "needle\n"), ("a.rs", "needle\n")]);
        let options = Options { include_ignored: true, ..literal("needle") };
        let (hits, _) = once(dir.path(), options);

        assert_eq!(hits.len(), 1, "a reflog is not project text: {hits:?}");
        assert_eq!(hits[0].path, PathBuf::from("a.rs"));
    }

    #[test]
    fn a_hit_reports_a_path_relative_to_the_root() {
        let dir = project(&[("deep/inside/a.rs", "needle\n")]);
        let (hits, _) = once(dir.path(), literal("needle"));

        assert_eq!(hits[0].path, PathBuf::from("deep/inside/a.rs"));
    }

    #[test]
    fn a_column_counts_characters_not_bytes() {
        // Three wide characters, an emoji with a modifier, and a tab, all
        // before the match. Counted as bytes the column would be 20-odd.
        let line = "日本語🇮🇸\tneedle\n";
        let dir = project(&[("a.txt", line)]);
        let (hits, _) = once(dir.path(), literal("needle"));

        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.line, 1);

        let prefix: String = "日本語🇮🇸\t".into();
        let expected = prefix.chars().count();
        assert_eq!(hit.column, u32::try_from(expected).unwrap() + 1);
        assert_eq!(hit.text, "日本語🇮🇸\tneedle");

        assert_eq!(cuts(hit), ["needle"], "matched indexes text by characters");
    }

    #[test]
    fn a_combining_mark_counts_as_the_characters_it_is() {
        // e + combining acute is two characters, and the column says two.
        let dir = project(&[("a.txt", "e\u{301}xneedle\n")]);
        let (hits, _) = once(dir.path(), literal("needle"));

        assert_eq!(hits[0].column, 4, "e, the mark, x, then the match");
    }

    #[test]
    fn a_hit_on_the_first_column_reports_one_not_zero() {
        let dir = project(&[("a.txt", "needle at the start\n")]);
        let (hits, _) = once(dir.path(), literal("needle"));

        assert_eq!(hits[0].column, 1);
        assert_eq!(hits[0].matched, vec![0..6]);
    }

    #[test]
    fn a_crlf_line_does_not_keep_its_terminator() {
        let dir = project(&[("a.txt", "needle here\r\nand again\r\n")]);
        let (hits, _) = once(dir.path(), literal("needle"));

        assert_eq!(hits[0].text, "needle here");
    }

    #[test]
    fn a_very_long_line_is_capped_and_still_places_the_match() {
        let filler = "x".repeat(50_000);
        let dir = project(&[("bundle.js", &format!("{filler}needle{filler}\n"))]);
        let (hits, _) = once(dir.path(), literal("needle"));

        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert!(hit.text.chars().count() <= MOST_CHARS, "{}", hit.text.chars().count());
        assert_eq!(hit.column, 50_001, "the column is where it really is");

        assert_eq!(cuts(hit), ["needle"], "the window slid to keep the match inside");
    }

    #[test]
    fn a_long_line_with_an_early_match_keeps_the_start_of_it() {
        let dir = project(&[("bundle.js", &format!("needle{}\n", "x".repeat(50_000)))]);
        let (hits, _) = once(dir.path(), literal("needle"));

        assert_eq!(hits[0].column, 1);
        assert_eq!(hits[0].matched, vec![0..6]);
        assert_eq!(hits[0].text.chars().count(), MOST_CHARS);
    }

    #[test]
    fn a_line_is_one_hit_however_many_times_it_matches() {
        let dir = project(&[("a.txt", "needle needle needle\n")]);
        let (hits, done) = once(dir.path(), literal("needle"));

        assert_eq!(hits.len(), 1, "one line is one row to click");
        assert_eq!(hits[0].column, 1, "the column is the first match");
        assert_eq!(
            done,
            Found::Done { generation: 1, hits: 1, files: 1, cancelled: false },
            "the count is rows, not matches"
        );
    }

    #[test]
    fn a_hit_carries_every_match_on_its_line() {
        let dir = project(&[("a.txt", "needle in a needle in a needle\n")]);
        let (hits, _) = once(dir.path(), literal("needle"));

        assert_eq!(hits[0].matched, vec![0..6, 12..18, 24..30]);
        assert_eq!(cuts(&hits[0]), ["needle", "needle", "needle"]);
        assert_eq!(hits[0].column, 1);
    }

    #[test]
    fn matches_that_could_overlap_are_taken_left_to_right() {
        // `aa` in `aaaaa` could be read five ways; a replace has to agree
        // with the panel about which, so both take the leftmost, then carry
        // on from its end.
        let dir = project(&[("a.txt", "aaaaa\n")]);
        let (hits, _) = once(dir.path(), literal("aa"));

        assert_eq!(hits[0].matched, vec![0..2, 2..4]);
        assert_eq!(cuts(&hits[0]), ["aa", "aa"]);
    }

    #[test]
    fn an_alternation_reports_each_branch_where_it_matched() {
        let dir = project(&[("a.rs", "let one = two(three);\n")]);
        let options = Options { regex: true, case: Case::Sensitive, ..literal("one|two|three") };
        let (hits, _) = once(dir.path(), options);

        assert_eq!(cuts(&hits[0]), ["one", "two", "three"]);
        assert_eq!(hits[0].column, 5, "still the first one");
    }

    #[test]
    fn a_match_outside_the_window_is_dropped_rather_than_mis_indexed() {
        // Two matches a long way apart on one line: the window is built
        // around the first, so the second cannot be indexed into `text` and
        // must not be reported as if it could.
        let filler = "x".repeat(50_000);
        let dir = project(&[("bundle.js", &format!("needle{filler}needle{filler}\n"))]);
        let (hits, _) = once(dir.path(), literal("needle"));

        let hit = &hits[0];
        assert_eq!(hit.column, 1);
        assert_eq!(hit.matched, vec![0..6], "only the one the window holds");
        assert_eq!(cuts(hit), ["needle"]);
        for range in &hit.matched {
            assert!(
                range.end as usize <= hit.text.chars().count(),
                "every range indexes text: {range:?}"
            );
        }
    }

    #[test]
    fn a_binary_file_is_skipped_rather_than_dumped() {
        let dir = tempfile::tempdir().unwrap();
        let mut binary = b"needle".to_vec();
        binary.extend_from_slice(&[0, 1, 2, 3, 0]);
        binary.extend_from_slice(b"needle\n");
        std::fs::write(dir.path().join("a.bin"), &binary).unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();

        let (hits, done) = once(dir.path(), literal("needle"));
        assert_eq!(hits.len(), 1, "only the text file: {hits:?}");
        assert_eq!(hits[0].path, PathBuf::from("a.txt"));
        assert_eq!(done, Found::Done { generation: 1, hits: 1, files: 1, cancelled: false });
    }

    #[test]
    fn hits_arrive_before_the_search_is_over() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..2_000 {
            std::fs::write(dir.path().join(format!("f{n}.txt")), "needle\n").unwrap();
        }

        let (grep, receiver) = grep();
        grep.search(dir.path(), literal("needle"), 1);

        // The first message is a batch, not the answer: the panel fills while
        // the walk is still running.
        match receiver.recv_timeout(Duration::from_secs(30)).expect("the worker answered") {
            Found::Hits { generation, hits } => {
                assert_eq!(generation, 1);
                assert!(!hits.is_empty());
                assert!(hits.len() < 2_000, "a batch, not the whole search: {}", hits.len());
            }
            other => panic!("{other:?}"),
        }

        let rest = drain(&receiver, 1);
        match finish(&rest, 1) {
            Found::Done { hits, files, cancelled, .. } => {
                assert_eq!(hits, 2_000);
                assert_eq!(files, 2_000);
                assert!(!cancelled);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_newer_search_cancels_the_one_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..4_000 {
            std::fs::write(dir.path().join(format!("f{n}.txt")), "needle\n").unwrap();
        }

        let (grep, receiver) = grep();
        grep.search(dir.path(), literal("needle"), 1);
        grep.search(dir.path(), literal("haystack"), 2);

        let mut first = None;
        let mut second = None;
        let mut after_first_done = 0;
        while second.is_none() {
            match receiver.recv_timeout(Duration::from_secs(30)).expect("the worker answered") {
                Found::Hits { generation: 1, .. } => {
                    assert!(first.is_none(), "a cancelled search sends no further hits");
                }
                Found::Hits { .. } => after_first_done += 1,
                found @ Found::Done { generation: 1, .. } => first = Some(found),
                found @ Found::Done { .. } => second = Some(found),
                other => panic!("{other:?}"),
            }
        }

        match first.expect("the first search finished") {
            Found::Done { hits, cancelled, .. } => {
                assert!(cancelled, "the first search was stopped");
                assert!(hits < 4_000, "it stopped part way: {hits}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(after_first_done, 0, "the second query matches nothing");
        assert_eq!(
            second.unwrap(),
            Found::Done { generation: 2, hits: 0, files: 0, cancelled: false }
        );
    }

    #[test]
    fn a_search_the_user_has_typed_past_is_not_run_at_all() {
        let dir = project(&[("a.txt", "needle\n")]);
        let (grep, receiver) = grep();

        grep.search(dir.path(), literal("needle"), 5);
        // Lower than the generation already asked for: stale on arrival.
        grep.search(dir.path(), literal("needle"), 3);

        let _ = drain(&receiver, 5);
        assert_eq!(
            finish(&drain(&receiver, 3), 3),
            Found::Done { generation: 3, hits: 0, files: 0, cancelled: true }
        );
    }

    #[test]
    fn cancelling_stops_the_search_without_starting_another() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..4_000 {
            std::fs::write(dir.path().join(format!("f{n}.txt")), "needle\n").unwrap();
        }

        let (grep, receiver) = grep();
        grep.search(dir.path(), literal("needle"), 1);
        grep.cancel();

        match finish(&drain(&receiver, 1), 1) {
            Found::Done { cancelled, .. } => assert!(cancelled),
            other => panic!("{other:?}"),
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]

        /// Whatever the line is made of, the two ways a hit says where the
        /// match is have to agree with each other and with the line: the
        /// column counts characters from one, and `matched` cuts the match
        /// out of `text` when `text` is indexed by characters.
        #[test]
        fn a_hit_always_points_at_its_match(
            before in "\\PC{0,800}",
            after in "\\PC{0,800}",
        ) {
            proptest::prop_assume!(!before.contains("needle") && !after.contains("needle"));

            let dir = project(&[("a.txt", &format!("{before}needle{after}\n"))]);
            let (hits, _) = once(dir.path(), literal("needle"));

            proptest::prop_assert_eq!(hits.len(), 1);
            let hit = &hits[0];
            proptest::prop_assert_eq!(hit.line, 1);
            proptest::prop_assert_eq!(
                hit.column as usize,
                before.chars().count() + 1,
                "the column counts characters from one"
            );
            proptest::prop_assert!(hit.text.chars().count() <= MOST_CHARS);
            proptest::prop_assert!(!hit.matched.is_empty(), "a hit always has a match");

            let width = hit.text.chars().count();
            for range in &hit.matched {
                proptest::prop_assert!(range.end as usize <= width, "{:?} indexes text", range);
                proptest::prop_assert_eq!(cut(&hit.text, range), "needle");
            }
        }
    }

    #[test]
    fn a_few_thousand_files_are_searched_in_a_sensible_time() {
        let dir = tempfile::tempdir().unwrap();
        let body = "fn main() {}\n".repeat(40);
        for n in 0..3_000 {
            std::fs::write(dir.path().join(format!("f{n}.rs")), format!("{body}needle\n")).unwrap();
        }

        let started = Instant::now();
        let (hits, done) = once(dir.path(), literal("needle"));
        let took = started.elapsed();

        assert_eq!(hits.len(), 3_000);
        assert!(matches!(done, Found::Done { cancelled: false, .. }));
        // Generous by an order of magnitude: this is a smoke alarm for a walk
        // that has gone quadratic, not a benchmark.
        assert!(took < Duration::from_secs(30), "searching took {took:?}");
    }
}
