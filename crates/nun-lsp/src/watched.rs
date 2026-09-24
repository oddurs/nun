//! The files a server has asked to hear about, and what it is told of them.
//!
//! A server that trusts its client to watch the disk registers
//! `workspace/didChangeWatchedFiles` with glob patterns, each either absolute,
//! relative to a folder it names, or bare — `**/*.rs` — which is taken as
//! relative to the server's own root. Each is kept here as a folder and a
//! pattern for what is under it: the folders are what the editor watches, and
//! the patterns choose, from everything that changes in them, what the server
//! hears.
//!
//! A folder is kept in two spellings: the server's, and resolved, through no
//! symbolic link. Changes arrive resolved — the editor's watcher reads resolved
//! paths, and the router resolves the rest — and are matched against the
//! resolved folder, then reported spelled the server's way, so a server that
//! was given a folder by a name through a link hears of its files by that
//! name, as it expects.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobBuilder, GlobMatcher};
use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileChangeType, FileEvent, GlobPattern, OneOf,
    WatchKind,
};
use serde_json::Value;

/// One pattern: files under `base` whose path from it matches `glob`.
#[derive(Debug)]
struct Pattern {
    /// The folder as the server spelled it.
    base: PathBuf,
    /// The folder resolved, as changes are spelled.
    resolved: PathBuf,
    glob: GlobMatcher,
    kind: WatchKind,
    /// Whether `base` is to be watched for it. Not for a pattern naming one
    /// path outright: rust-analyzer names its settings folder that way, and
    /// watching everything beside it — all of `Application Support` — for
    /// one name would cost far more than the name is worth. Such a path is
    /// still told of when it is inside a folder watched for another pattern.
    watch: bool,
}

/// Every pattern a server has registered, by registration.
#[derive(Debug, Default)]
pub(crate) struct Watched {
    registrations: Vec<(String, Vec<Pattern>)>,
}

impl Watched {
    /// Take a registration's options. `root` is the server's own folder,
    /// which a bare pattern is relative to. Whether anything was registered;
    /// a registration under an id already taken replaces it.
    pub(crate) fn register(&mut self, id: &str, options: Option<Value>, root: &Path) -> bool {
        let Some(options) = options.and_then(|options| {
            serde_json::from_value::<DidChangeWatchedFilesRegistrationOptions>(options).ok()
        }) else {
            return false;
        };
        let patterns: Vec<Pattern> = options
            .watchers
            .into_iter()
            .filter_map(|watcher| {
                let kind = watcher.kind.unwrap_or(WatchKind::all());
                let (base, glob, watch) = split(watcher.glob_pattern, root)?;
                let glob = compile(&glob)?;
                let resolved = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
                Some(Pattern { base, resolved, glob, kind, watch })
            })
            .collect();
        self.unregister(id);
        self.registrations.push((id.to_string(), patterns));
        true
    }

    /// Forget a registration. Whether there was one by that id.
    pub(crate) fn unregister(&mut self, id: &str) -> bool {
        let before = self.registrations.len();
        self.registrations.retain(|(known, _)| known != id);
        self.registrations.len() != before
    }

    /// The folders to watch, resolved, each once.
    pub(crate) fn folders(&self) -> Vec<PathBuf> {
        let mut folders: Vec<PathBuf> = self
            .registrations
            .iter()
            .flat_map(|(_, patterns)| patterns)
            .filter(|pattern| pattern.watch)
            .map(|pattern| pattern.resolved.clone())
            .collect();
        folders.sort();
        folders.dedup();
        folders
    }

    /// What to tell the server of `changes`, each a resolved path: those a
    /// pattern wants, spelled the way the server spelled that pattern's
    /// folder, each once.
    pub(crate) fn events(&self, changes: &[(PathBuf, FileChangeType)]) -> Vec<FileEvent> {
        let mut events: Vec<FileEvent> = Vec::new();
        for (path, typ) in changes {
            let wanted =
                self.registrations.iter().flat_map(|(_, patterns)| patterns).find_map(|pattern| {
                    let under = path.strip_prefix(&pattern.resolved).ok()?;
                    (pattern.kind.contains(watch_kind(*typ)) && pattern.glob.is_match(under))
                        .then(|| pattern.base.join(under))
                });
            let Some(uri) = wanted.as_deref().and_then(crate::uri::from_path) else { continue };
            let event = FileEvent { uri, typ: *typ };
            if !events.contains(&event) {
                events.push(event);
            }
        }
        events
    }
}

fn watch_kind(typ: FileChangeType) -> WatchKind {
    match typ {
        FileChangeType::CREATED => WatchKind::Create,
        FileChangeType::DELETED => WatchKind::Delete,
        _ => WatchKind::Change,
    }
}

/// A pattern as a folder, a glob for what is under it, and whether the
/// folder is worth watching for it.
fn split(pattern: GlobPattern, root: &Path) -> Option<(PathBuf, String, bool)> {
    match pattern {
        GlobPattern::Relative(relative) => {
            let uri = match relative.base_uri {
                OneOf::Left(folder) => folder.uri,
                OneOf::Right(uri) => uri,
            };
            Some((crate::uri::to_path(&uri)?, relative.pattern, true))
        }
        GlobPattern::String(text) if text.starts_with('/') => {
            // The folder is everything before the first part with a wildcard.
            let parts: Vec<&str> = text.split('/').collect();
            let wild = parts.iter().position(|part| part.contains(['*', '?', '[', '{']));
            let literal = wild.unwrap_or(parts.len() - 1);
            let base = parts[..literal].join("/");
            Some((
                PathBuf::from(if base.is_empty() { "/" } else { &base }),
                parts[literal..].join("/"),
                wild.is_some(),
            ))
        }
        GlobPattern::String(text) => Some((root.to_path_buf(), text, true)),
    }
}

/// The protocol's glob: `*` and `?` stay within one part of the path, `**`
/// spans any number of parts, `{a,b}` is either, `[...]` a range.
fn compile(glob: &str) -> Option<GlobMatcher> {
    let glob: Glob = GlobBuilder::new(glob).literal_separator(true).build().ok()?;
    Some(glob.compile_matcher())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn watched(registration: &Value) -> Watched {
        let mut watched = Watched::default();
        assert!(watched.register("w", Some(registration.clone()), Path::new("/project")));
        watched
    }

    fn told(watched: &Watched, changes: &[(&str, FileChangeType)]) -> Vec<String> {
        let changes: Vec<(PathBuf, FileChangeType)> =
            changes.iter().map(|(path, typ)| (PathBuf::from(path), *typ)).collect();
        watched.events(&changes).into_iter().map(|event| event.uri.as_str().to_string()).collect()
    }

    #[test]
    fn a_pattern_chooses_by_name_and_stays_within_its_folder() {
        let watched = watched(&json!({ "watchers": [
            { "globPattern": { "baseUri": "file:///project/crates/a", "pattern": "**/*.rs" } },
            { "globPattern": "/project/**/Cargo.{toml,lock}" },
            { "globPattern": "*.json" },
            { "globPattern": "/home/me/.config/tool" },
            { "globPattern": "/project/Cargo.toml" },
        ]}));
        assert_eq!(
            watched.folders(),
            [PathBuf::from("/project"), PathBuf::from("/project/crates/a")]
        );
        let changed = FileChangeType::CHANGED;
        assert_eq!(
            told(
                &watched,
                &[
                    ("/project/crates/a/src/lib.rs", changed),
                    ("/project/crates/a/lib.rs", changed),
                    ("/project/crates/b/src/lib.rs", changed),
                    ("/project/crates/b/Cargo.toml", changed),
                    ("/project/Cargo.lock", changed),
                    ("/project/x.json", changed),
                    ("/project/sub/x.json", changed),
                    ("/project/crates/a/src/lib.rs", changed),
                    ("/elsewhere/a.rs", changed),
                    ("/home/me/.config/tool", changed),
                ]
            ),
            [
                "file:///project/crates/a/src/lib.rs",
                "file:///project/crates/a/lib.rs",
                "file:///project/crates/b/Cargo.toml",
                "file:///project/Cargo.lock",
                "file:///project/x.json",
                "file:///home/me/.config/tool",
            ]
        );
    }

    #[test]
    fn a_pattern_hears_only_the_kinds_it_asked_for() {
        let watched = watched(&json!({ "watchers": [{ "globPattern": "**/*.rs", "kind": 5 }] }));
        let path = "/project/src/a.rs";
        assert_eq!(told(&watched, &[(path, FileChangeType::CHANGED)]), Vec::<String>::new());
        assert_eq!(told(&watched, &[(path, FileChangeType::CREATED)]).len(), 1);
        assert_eq!(told(&watched, &[(path, FileChangeType::DELETED)]).len(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn a_path_is_reported_the_way_the_server_spelled_its_folder() {
        let temp = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(temp.path()).unwrap().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let base = crate::uri::from_path(&link).unwrap();
        let mut watched = Watched::default();
        let options = json!({ "watchers": [
            { "globPattern": { "baseUri": base.as_str(), "pattern": "**/*.rs" } },
        ]});
        watched.register("w", Some(options), Path::new("/"));
        assert_eq!(
            watched.folders(),
            std::slice::from_ref(&real),
            "the folder is watched resolved"
        );
        let events = watched.events(&[(real.join("src/new.rs"), FileChangeType::CREATED)]);
        assert_eq!(
            events.iter().map(|event| event.uri.clone()).collect::<Vec<_>>(),
            [crate::uri::from_path(&link.join("src/new.rs")).unwrap()],
            "and its files are named through the link, as the server named it"
        );
    }

    #[test]
    fn registering_again_replaces_and_unregistering_forgets() {
        let mut watched = watched(&json!({ "watchers": [{ "globPattern": "**/*.rs" }] }));
        let options = json!({ "watchers": [{ "globPattern": "/other/**" }] });
        watched.register("w", Some(options), Path::new("/project"));
        assert_eq!(watched.folders(), [PathBuf::from("/other")]);
        assert!(!watched.register("x", Some(json!({ "nonsense": 1 })), Path::new("/")));
        assert!(watched.unregister("w"));
        assert!(!watched.unregister("w"));
        assert!(watched.folders().is_empty());
    }
}
