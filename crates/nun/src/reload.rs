//! Reading the configuration again whenever one of its files changes.
//!
//! Everything here happens on a thread of its own: watching, reading and
//! parsing. What it finds goes back to the editor through the event channel
//! as [`nun_config::News`], and the main thread swaps it in, so a slow disk
//! or a large `.editorconfig` never holds up a frame.
//!
//! Directories are watched rather than files, because editors save by
//! writing a new file and renaming it over the old one, and a watch on the
//! old one sees nothing. The directories are the one the person's
//! `nun.toml` is in, the project root and the one its `.nun.toml` is in, and
//! each directory an `.editorconfig` bearing on an open file was found in,
//! plus the open files' own directories inside the project, where a new one
//! could appear. An `.editorconfig` created above the project while nun runs
//! is not noticed until nun starts again: watching every directory up to `/`
//! would cost far more than it is worth.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use nun_config::{Decision, EditorConfig, Files, News, Sources, TrustStore};

/// Something for the worker to do.
enum Message {
    /// Keep `.editorconfig` news coming for this file.
    Follow(PathBuf),
    /// Remember a decision about the project in `dir`, then load again.
    Decide { dir: PathBuf, decision: Decision, fingerprint: String },
    /// Something in this directory changed.
    Changed(PathBuf),
}

/// The handle on the worker, which runs for as long as the editor does.
pub struct Reloader {
    messages: Sender<Message>,
}

impl std::fmt::Debug for Reloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reloader").finish_non_exhaustive()
    }
}

impl Reloader {
    /// Start watching the layers that govern `root`, from `files` as they
    /// were read at startup, posting news to `post`.
    ///
    /// # Errors
    ///
    /// If the native watcher or the thread cannot be started.
    pub fn start(
        root: PathBuf,
        files: Files,
        post: Box<dyn Fn(News) + Send + 'static>,
    ) -> std::io::Result<Self> {
        let (messages, inbox) = mpsc::channel();
        let changes = messages.clone();
        let watcher = nun_workspace::Watcher::new(Box::new(move |change| {
            let _ = changes.send(Message::Changed(change.dir));
        }))
        .map_err(std::io::Error::other)?;
        let worker = Worker {
            root,
            files,
            trust: trust_store(),
            watcher,
            watched: BTreeSet::new(),
            followed: BTreeMap::new(),
            cache: BTreeMap::new(),
            post,
        };
        thread::Builder::new().name("nun-config".into()).spawn(move || worker.run(&inbox))?;
        Ok(Self { messages })
    }

    /// Keep `.editorconfig` news coming for `path`, starting with what bears
    /// on it now.
    pub fn follow(&self, path: &Path) {
        let _ = self.messages.send(Message::Follow(path.to_path_buf()));
    }

    /// Remember a decision about the project file in `dir`, and apply it.
    pub fn decide(&self, dir: PathBuf, decision: Decision, fingerprint: String) {
        let _ = self.messages.send(Message::Decide { dir, decision, fingerprint });
    }
}

/// The decisions remembered on this machine.
pub fn trust_store() -> TrustStore {
    TrustStore::default_path().map_or_else(TrustStore::in_memory, TrustStore::load)
}

struct Worker {
    root: PathBuf,
    /// The files as last read: what a broken file falls back to.
    files: Files,
    trust: TrustStore,
    watcher: nun_workspace::Watcher,
    watched: BTreeSet<PathBuf>,
    /// Every file followed, and the `.editorconfig` files last said for it.
    followed: BTreeMap<PathBuf, Vec<EditorConfig>>,
    /// Each directory's `.editorconfig`, as last read; `None` for none.
    cache: BTreeMap<PathBuf, Option<EditorConfig>>,
    post: Box<dyn Fn(News) + Send>,
}

impl Worker {
    fn run(mut self, inbox: &Receiver<Message>) {
        self.watch_layers();
        for message in inbox {
            match message {
                Message::Follow(path) => {
                    let configs = self.editorconfigs(&path);
                    self.followed.insert(path.clone(), configs.clone());
                    (self.post)(News::EditorConfig { path, configs });
                }
                Message::Decide { dir, decision, fingerprint } => {
                    if let Err(error) = self.trust.decide(dir, decision, fingerprint) {
                        (self.post)(News::NotRemembered(error.to_string()));
                    }
                    self.reload(true);
                }
                Message::Changed(dir) => {
                    // Another nun may have decided about a project meanwhile.
                    self.trust = trust_store();
                    self.reload(false);
                    self.editorconfig_changed(&dir);
                }
            }
        }
    }

    /// Watch the directories the settings files are, or would be, in.
    fn watch_layers(&mut self) {
        let sources = Sources::discover(&self.root);
        let dirs = [
            sources.user.as_deref().and_then(Path::parent).map(Path::to_path_buf),
            sources.project.as_deref().and_then(Path::parent).map(Path::to_path_buf),
            Some(self.root.clone()),
        ];
        for dir in dirs.into_iter().flatten() {
            self.watch(dir);
        }
    }

    fn watch(&mut self, dir: PathBuf) {
        if dir.is_dir() && self.watched.insert(dir.clone()) {
            self.watcher.watch(dir);
        }
    }

    /// Read the settings files again, and say so if anything changed —
    /// or always, after a decision, so the prompt is answered either way.
    fn reload(&mut self, always: bool) {
        let sources = Sources::discover(&self.root);
        let files = Files::read(&sources, &self.files);
        let changed = files != self.files;
        self.files = files;
        self.watch_layers();
        if changed || always {
            (self.post)(News::Settings(Box::new(nun_config::resolve(&self.files, &self.trust))));
        }
    }

    /// The `.editorconfig` files bearing on `path`, read through the cache,
    /// watching every directory one was found in.
    fn editorconfigs(&mut self, path: &Path) -> Vec<EditorConfig> {
        let mut found_in = Vec::new();
        let cache = &mut self.cache;
        let configs = nun_config::editorconfig::find(path, |dir| {
            let config = cache.entry(dir.to_path_buf()).or_insert_with(|| EditorConfig::read(dir));
            if config.is_some() {
                found_in.push(dir.to_path_buf());
            }
            config.clone()
        });
        // Inside the project, every directory on the way, so a new file is
        // seen; outside it, only where one already is.
        let inside = path.ancestors().skip(1).take_while(|dir| dir.starts_with(&self.root));
        let inside: Vec<PathBuf> = inside.map(Path::to_path_buf).collect();
        for dir in found_in.into_iter().chain(inside) {
            self.watch(dir);
        }
        configs
    }

    /// Something changed in `dir`: read its `.editorconfig` again, and tell
    /// every followed file whose files are now different.
    fn editorconfig_changed(&mut self, dir: &Path) {
        let fresh = EditorConfig::read(dir);
        if self.cache.get(dir) == Some(&fresh) {
            return;
        }
        self.cache.insert(dir.to_path_buf(), fresh);
        let paths: Vec<PathBuf> =
            self.followed.keys().filter(|path| path.starts_with(dir)).cloned().collect();
        for path in paths {
            let configs = self.editorconfigs(&path);
            if self.followed.get(&path) != Some(&configs) {
                self.followed.insert(path.clone(), configs.clone());
                (self.post)(News::EditorConfig { path, configs });
            }
        }
    }
}
