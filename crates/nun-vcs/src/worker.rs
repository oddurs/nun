//! Git, off the thread that draws.
//!
//! Two threads, because the work comes in two sizes. Hunks are per document,
//! small, and wanted within a keystroke or two; a status walks the whole
//! working tree and on a large repository takes as long as it takes. Sharing
//! one thread would put every gutter update behind the walk. Both report
//! through the same callback, which hands the answer to the editor's event
//! channel.
//!
//! The editor debounces — it sends the text once typing pauses — and each
//! thread takes everything waiting at once and keeps only the newest of what
//! supersedes, so a burst of requests is one piece of work.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use gix::bstr::BString;
use ropey::Rope;

use crate::diff::{Base, Diff, Hunk};
use crate::repo::{Against, MOST_BYTES, Repo, is_binary};
use crate::status::Status;

/// Which document a message is about.
pub type DocId = u32;

/// Something for git to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Start following a document: find its repository, read its staged
    /// version, and diff.
    Open {
        /// Which document.
        id: DocId,
        /// Which version of its text this is.
        version: u64,
        /// Where it is on disk, which need not exist yet.
        path: PathBuf,
        /// Its text, as the buffer holds it.
        text: Rope,
    },
    /// The text changed.
    Update {
        /// Which document.
        id: DocId,
        /// Which version of its text this is.
        version: u64,
        /// The text now.
        text: Rope,
    },
    /// The file moved: saved under another name.
    Moved {
        /// Which document.
        id: DocId,
        /// Where it is now.
        path: PathBuf,
    },
    /// Stop following a document.
    Close(DocId),
    /// The index or `HEAD` may have changed underneath: a commit or an add in
    /// a terminal, a checkout. Diff every document again, and answer for
    /// those whose hunks changed.
    Refresh,
    /// Work out the status of the working tree `root` is in.
    Status(PathBuf),
    /// Diff a document against an old version of its choosing — for a diff
    /// view, which may want `HEAD` rather than the index.
    Compare {
        /// Which document.
        id: DocId,
        /// Which request this is, echoed back.
        serial: u64,
        /// What to compare against.
        against: Against,
    },
    /// Write one hunk into the index, leaving the working tree alone.
    Stage {
        /// Which document.
        id: DocId,
        /// The version the hunk was taken from. If the text has moved on
        /// since, the hunk describes lines that are no longer there, and
        /// nothing is staged.
        version: u64,
        /// The hunk, as [`Reply::Hunks`] gave it.
        hunk: Hunk,
    },
    /// A marker that comes back once everything before it on the hunk
    /// thread is done.
    Echo(u64),
}

/// What git has to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// A document's hunks against its staged version, as of a version of its
    /// text. `None` when there is nothing to compare with: no repository, an
    /// untracked or binary file, one too large to diff.
    Hunks {
        /// Which document.
        id: DocId,
        /// Which version of the text they describe.
        version: u64,
        /// The hunks, and the base they are from.
        diff: Option<Arc<Diff>>,
    },
    /// The status of a working tree, or `None` when the folder is in no
    /// repository — which is not a failure.
    Status {
        /// The folder it was asked for.
        root: PathBuf,
        /// What has changed in it.
        status: Result<Option<Arc<Status>>, String>,
    },
    /// The answer to [`Request::Compare`].
    Compared {
        /// Which document.
        id: DocId,
        /// Which version of the text it describes.
        version: u64,
        /// The serial it was asked with.
        serial: u64,
        /// What it was compared against.
        against: Against,
        /// The hunks, or `None` as for [`Reply::Hunks`].
        diff: Option<Arc<Diff>>,
    },
    /// The answer to [`Request::Stage`]. A success is followed by fresh
    /// [`Reply::Hunks`] for every document in that repository.
    Staged {
        /// Which document.
        id: DocId,
        /// Whether it was staged, and why not.
        result: Result<(), String>,
    },
    /// The marker from [`Request::Echo`].
    Echo(u64),
}

/// Somewhere to post replies, shared by both threads.
type Report = Arc<dyn Fn(Reply) + Send + Sync + 'static>;

/// Git's threads.
///
/// Dropping it stops them once the work they have is done.
#[derive(Debug)]
pub struct Vcs {
    hunks: Sender<Request>,
    status: Sender<PathBuf>,
}

impl Vcs {
    /// Start the threads, reporting through `report`.
    ///
    /// `report` runs on the worker threads, so it should do nothing but hand
    /// the reply on to the editor's event channel.
    #[must_use]
    pub fn new(report: Box<dyn Fn(Reply) + Send + Sync + 'static>) -> Self {
        let report: Report = Arc::from(report);

        let (hunks, receiver) = mpsc::channel::<Request>();
        let posted = Arc::clone(&report);
        thread::spawn(move || hunk_thread(&receiver, &*posted));

        let (status, receiver) = mpsc::channel::<PathBuf>();
        thread::spawn(move || status_thread(&receiver, &*report));

        Self { hunks, status }
    }

    /// Ask for something. Threads that have gone — only at shutdown — drop
    /// it.
    pub fn send(&self, request: Request) {
        let _ = match request {
            Request::Status(root) => self.status.send(root).map_err(drop),
            other => self.hunks.send(other).map_err(drop),
        };
    }
}

fn status_thread(receiver: &Receiver<PathBuf>, report: &(dyn Fn(Reply) + Send + Sync)) {
    // Opened once per folder and kept: discovery reads config files, and
    // the same folder is asked about on every save and every change on disk.
    let mut repos: HashMap<PathBuf, Option<Repo>> = HashMap::new();
    while let Ok(first) = receiver.recv() {
        // Only the newest of each folder is worth walking for.
        let mut roots = vec![first];
        for root in receiver.try_iter() {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        for root in roots {
            let repo = repos.entry(root.clone()).or_default();
            // Asked again each time while there is none: `git init` can
            // happen while the editor is open.
            if repo.is_none() {
                *repo = Repo::discover(&root);
            }
            let status = match repo {
                None => Ok(None),
                Some(repo) => repo
                    .status()
                    .map(|mut status| {
                        status.spelled_from(&root);
                        Some(Arc::new(status))
                    })
                    .map_err(|e| e.0),
            };
            report(Reply::Status { root, status });
        }
    }
}

/// A document the hunk thread is following.
#[derive(Debug)]
struct Tracked {
    path: PathBuf,
    version: u64,
    text: Rope,
    /// Which of the thread's repositories it is in, and where in it.
    place: Option<(usize, BString)>,
    /// The staged version it was last diffed against, by id, so an unchanged
    /// index costs a lookup rather than a read and a decode.
    base: Option<(gix::ObjectId, Arc<Base>)>,
    /// The hunks last reported, so a refresh that changes nothing says
    /// nothing.
    reported: Option<Arc<Diff>>,
}

/// What the hunk thread holds.
#[derive(Debug, Default)]
struct Hunks {
    docs: HashMap<DocId, Tracked>,
    /// Every repository a document has been found in. Never shrinks: there
    /// are only ever a handful, and one a closed document was in is likely to
    /// be the one the next is in.
    repos: Vec<Repo>,
}

fn hunk_thread(receiver: &Receiver<Request>, report: &(dyn Fn(Reply) + Send + Sync)) {
    let mut state = Hunks::default();
    while let Ok(first) = receiver.recv() {
        let mut batch = vec![first];
        batch.extend(receiver.try_iter());
        for request in coalesce(batch) {
            state.handle(request, report);
        }
    }
}

/// Drop the requests a later one in the same batch makes pointless: an
/// update followed by another update of the same document, and a refresh
/// followed by another refresh. Everything else keeps its place, because
/// order matters — an update must not overtake the open it follows.
fn coalesce(batch: Vec<Request>) -> Vec<Request> {
    let superseded = |index: usize, request: &Request| {
        batch[index + 1..].iter().any(|later| match (request, later) {
            (Request::Update { id, .. }, Request::Update { id: other, .. }) => id == other,
            (Request::Refresh, Request::Refresh) => true,
            _ => false,
        })
    };
    let keep: Vec<bool> =
        batch.iter().enumerate().map(|(index, request)| !superseded(index, request)).collect();
    batch.into_iter().zip(keep).filter_map(|(request, keep)| keep.then_some(request)).collect()
}

impl Hunks {
    fn handle(&mut self, request: Request, report: &(dyn Fn(Reply) + Send + Sync)) {
        match request {
            Request::Open { id, version, path, text } => {
                let place = self.place(&path);
                let doc = Tracked { path, version, text, place, base: None, reported: None };
                self.docs.insert(id, doc);
                self.rediff(id, true, report);
            }
            Request::Update { id, version, text } => {
                if let Some(doc) = self.docs.get_mut(&id) {
                    doc.version = version;
                    doc.text = text;
                    self.rediff(id, true, report);
                }
            }
            Request::Moved { id, path } => {
                let place = self.place(&path);
                if let Some(doc) = self.docs.get_mut(&id) {
                    doc.path = path;
                    doc.place = place;
                    doc.base = None;
                    self.rediff(id, true, report);
                }
            }
            Request::Close(id) => {
                self.docs.remove(&id);
            }
            Request::Refresh => {
                let ids: Vec<DocId> = self.docs.keys().copied().collect();
                for id in ids {
                    // A file in no repository may be in one now.
                    if let Some(path) = self
                        .docs
                        .get(&id)
                        .filter(|doc| doc.place.is_none())
                        .map(|doc| doc.path.clone())
                    {
                        let place = self.place(&path);
                        if let Some(doc) = self.docs.get_mut(&id) {
                            doc.place = place;
                        }
                    }
                    self.rediff(id, false, report);
                }
            }
            Request::Status(_) => {}
            Request::Compare { id, serial, against } => {
                if let Some(doc) = self.docs.get(&id) {
                    let diff = self.compare(doc, against);
                    let version = doc.version;
                    report(Reply::Compared { id, version, serial, against, diff });
                }
            }
            Request::Stage { id, version, hunk } => {
                let result = self.stage(id, version, &hunk);
                let staged = result.is_ok();
                report(Reply::Staged { id, result });
                if staged {
                    // The index changed for every document in that
                    // repository, not only this one.
                    let ids: Vec<DocId> = self.docs.keys().copied().collect();
                    for id in ids {
                        self.rediff(id, false, report);
                    }
                }
            }
            Request::Echo(marker) => report(Reply::Echo(marker)),
        }
    }

    /// Which repository `path` is in, opening it if it is a new one.
    fn place(&mut self, path: &std::path::Path) -> Option<(usize, BString)> {
        if let Some(found) = self
            .repos
            .iter()
            .enumerate()
            .filter_map(|(index, repo)| Some((index, repo.relative(path)?)))
            // The deepest working tree wins: a file in a submodule is the
            // submodule's, though the superproject's tree contains it too.
            .max_by_key(|(index, _)| self.repos[*index].workdir().components().count())
        {
            // Only trusted if discovery would say the same. It would not for
            // a file in a nested repository not yet opened, so check that
            // no `.git` sits between the file and the one found.
            if !nested_repo_between(path, self.repos[found.0].workdir()) {
                return Some(found);
            }
        }
        let repo = Repo::discover(path)?;
        let rel = repo.relative(path)?;
        let same = self.repos.iter().position(|known| {
            known.workdir() == repo.workdir() && known.common_dir() == repo.common_dir()
        });
        let index = same.unwrap_or_else(|| {
            self.repos.push(repo);
            self.repos.len() - 1
        });
        Some((index, rel))
    }

    /// Diff a document against its staged version again, and say so if the
    /// answer is news — always when `always`, as for an edit the editor is
    /// waiting to hear about.
    fn rediff(&mut self, id: DocId, always: bool, report: &(dyn Fn(Reply) + Send + Sync)) {
        let Some(doc) = self.docs.get_mut(&id) else { return };
        let base = doc.place.as_ref().and_then(|(repo, rel)| {
            let repo = &self.repos[*repo];
            let blob_id = repo.index_id(rel.as_ref()).ok()??;
            match &doc.base {
                Some((known, base)) if *known == blob_id => Some(Arc::clone(base)),
                _ => {
                    let blob = repo.index_blob(rel.as_ref()).ok()??;
                    let base = Arc::new(usable(&blob.data)?);
                    doc.base = Some((blob.id, Arc::clone(&base)));
                    Some(base)
                }
            }
        });
        if base.is_none() {
            doc.base = None;
        }
        let diff = base.and_then(|base| diff_text(base, &doc.text)).map(Arc::new);
        let news = always
            || match (&diff, &doc.reported) {
                (Some(new), Some(old)) => new != old,
                (None, None) => false,
                _ => true,
            };
        doc.reported.clone_from(&diff);
        if news {
            report(Reply::Hunks { id, version: doc.version, diff });
        }
    }

    fn compare(&self, doc: &Tracked, against: Against) -> Option<Arc<Diff>> {
        let (repo, rel) = doc.place.as_ref()?;
        let blob = self.repos[*repo].blob_against(rel.as_ref(), against).ok()??;
        diff_text(Arc::new(usable(&blob.data)?), &doc.text).map(Arc::new)
    }

    fn stage(&self, id: DocId, version: u64, hunk: &Hunk) -> Result<(), String> {
        let doc = self.docs.get(&id).ok_or("That file is no longer open.")?;
        if doc.version != version {
            return Err("The file changed since that hunk was shown; try again.".into());
        }
        let diff = doc.reported.as_ref().ok_or("There is nothing to stage in this file.")?;
        if !diff.hunks().contains(hunk) {
            return Err("That hunk is not one of this file's changes any more.".into());
        }
        if diff.base().is_lossy() {
            return Err("The staged version is not valid UTF-8, so nun will not rewrite it.".into());
        }
        let (repo, rel) = doc.place.as_ref().ok_or("This file is not in a repository.")?;
        let staged = diff.staged(hunk, &doc.text.to_string());
        self.repos[*repo].stage(rel.as_ref(), &diff.base().encode(&staged)).map_err(|e| e.0)
    }
}

/// The base a blob gives, unless it is not worth diffing: binary, or huge.
fn usable(data: &[u8]) -> Option<Base> {
    (data.len() <= MOST_BYTES && !is_binary(data)).then(|| Base::decode(data))
}

fn diff_text(base: Arc<Base>, text: &Rope) -> Option<Diff> {
    (text.len_bytes() <= MOST_BYTES).then(|| Diff::of_rope(base, text))
}

/// Whether a folder between `path` and `workdir` holds a `.git` of its own —
/// a submodule or a nested repository, which `path` belongs to instead.
fn nested_repo_between(path: &std::path::Path, workdir: &std::path::Path) -> bool {
    let Some(rel) = crate::repo::relative_to(path, workdir, None) else { return false };
    let mut dir = workdir.to_path_buf();
    let mut components = rel.components().peekable();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            break;
        }
        dir.push(component);
        if dir.join(".git").exists() {
            return true;
        }
    }
    false
}
