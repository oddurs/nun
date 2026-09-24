//! Whether a project's `.nun.toml` is trusted.
//!
//! A config file from a freshly cloned repository must not silently choose a
//! formatter or point a language server at an arbitrary binary. So a project
//! file is inert until the person says otherwise, and what they say is
//! remembered per directory, in the state directory, with a fingerprint of
//! what they agreed to.
//!
//! **Which edits ask again.** The fingerprint covers only the settings whose
//! [`Scope`](crate::schema::Scope) needs trust — the ones that run a program
//! or rewrite files — so editing the project's `tab_width` applies at once,
//! and changing its language server's command asks first. While it asks, the
//! harmless settings go on applying: the directory was trusted, and they are
//! as harmless as they were. The fingerprint is SHA-256 over those settings
//! written out in a fixed order, so it changes with what they say rather than
//! with comments or layout, and a file cannot be written to match another's.
//!
//! An `Ignore` is remembered the same way, so an ignored project stays quiet
//! until its risky settings change, when it asks again.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::file::File;

/// What the person said about a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Apply its settings.
    Trust,
    /// Leave them inert, and stop asking.
    Ignore,
}

impl Decision {
    const fn word(self) -> &'static str {
        match self {
            Self::Trust => "trust",
            Self::Ignore => "ignore",
        }
    }
}

/// Where a project file stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Never decided on: inert, and to be asked about.
    Unknown,
    /// Trusted, as it is now.
    Trusted,
    /// Trusted once, and its risky settings have changed since: those are
    /// withheld until it is trusted again, and the rest apply.
    Changed,
    /// Ignored, as it is now: inert, and quiet.
    Ignored,
}

/// The fingerprint of what in `file` needs trust.
#[must_use]
pub fn fingerprint(file: &File) -> String {
    let mut text = String::new();
    for (key, entry) in &file.entries {
        if crate::schema::find(key).is_some_and(|setting| setting.scope.needs_trust()) {
            let _ = writeln!(text, "{key}={}", entry.value);
        }
    }
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().fold(String::with_capacity(64), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// Every decision made, by canonical directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustStore {
    /// Where it is kept; `None` keeps it in memory only.
    path: Option<PathBuf>,
    records: BTreeMap<PathBuf, (Decision, String)>,
}

impl TrustStore {
    /// Where decisions are kept: `$XDG_STATE_HOME/nun/trust`, or
    /// `~/.local/state/nun/trust`.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })?;
        Some(base.join("nun").join("trust"))
    }

    /// The decisions kept at `path`. A missing or damaged file is no
    /// decisions, or fewer: the worst a damaged line costs is being asked
    /// again.
    #[must_use]
    pub fn load(path: PathBuf) -> Self {
        let records = fs::read_to_string(&path).map(|text| parse(&text)).unwrap_or_default();
        Self { path: Some(path), records }
    }

    /// Decisions held in memory only.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Where the project file in `dir`, with `fingerprint`, stands.
    #[must_use]
    pub fn trust(&self, dir: &Path, fingerprint: &str) -> Trust {
        match self.records.get(dir) {
            Some((Decision::Trust, known)) if known == fingerprint => Trust::Trusted,
            Some((Decision::Trust, _)) => Trust::Changed,
            Some((Decision::Ignore, known)) if known == fingerprint => Trust::Ignored,
            None | Some((Decision::Ignore, _)) => Trust::Unknown,
        }
    }

    /// Remember a decision in memory.
    pub fn remember(&mut self, dir: PathBuf, decision: Decision, fingerprint: String) {
        self.records.insert(dir, (decision, fingerprint));
    }

    /// Remember a decision, and write it down.
    ///
    /// What is on disk is read again first and this one decision laid over
    /// it, so two nuns deciding about two projects do not undo each other.
    ///
    /// # Errors
    ///
    /// If the state directory or the file cannot be written.
    pub fn decide(
        &mut self,
        dir: PathBuf,
        decision: Decision,
        fingerprint: String,
    ) -> io::Result<()> {
        if let Some(path) = &self.path {
            let mut records = fs::read_to_string(path).map(|text| parse(&text)).unwrap_or_default();
            records.insert(dir.clone(), (decision, fingerprint.clone()));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut text = String::from(
                "# Project settings nun has been told to trust or ignore. Delete a line to be asked again.\n",
            );
            for (dir, (decision, fingerprint)) in &records {
                let _ = writeln!(text, "{}\t{fingerprint}\t{}", decision.word(), dir.display());
            }
            let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
            fs::write(&temporary, text)?;
            fs::rename(&temporary, path)?;
            self.records = records;
        } else {
            self.remember(dir, decision, fingerprint);
        }
        Ok(())
    }
}

fn parse(text: &str) -> BTreeMap<PathBuf, (Decision, String)> {
    let mut records = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(word), Some(fingerprint), Some(dir)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let decision = match word {
            "trust" => Decision::Trust,
            "ignore" => Decision::Ignore,
            _ => continue,
        };
        records.insert(PathBuf::from(dir), (decision, fingerprint.to_string()));
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Layer;

    fn file(text: &str) -> File {
        File::parse(Path::new("/p/.nun.toml"), Layer::Project, text, None)
    }

    #[test]
    fn only_risky_settings_change_the_fingerprint() {
        let base = file("[editor]\ntab_width = 2\n[lsp.rust]\ncommand = \"ra\"\n");
        let harmless =
            file("# a comment\n[editor]\ntab_width = 3\n\n[lsp.rust]\ncommand = \"ra\"\n");
        let risky = file("[editor]\ntab_width = 2\n[lsp.rust]\ncommand = \"./evil\"\n");
        let added =
            file("[editor]\ntab_width = 2\n[lsp.rust]\ncommand = \"ra\"\nformat_on_save = true\n");
        assert_eq!(fingerprint(&base), fingerprint(&harmless));
        assert_ne!(fingerprint(&base), fingerprint(&risky));
        assert_ne!(fingerprint(&base), fingerprint(&added));
        assert_eq!(fingerprint(&base).len(), 64);
    }

    #[test]
    fn decisions_are_per_directory_and_per_fingerprint() {
        let mut store = TrustStore::in_memory();
        let dir = Path::new("/p");
        assert_eq!(store.trust(dir, "a"), Trust::Unknown);
        store.remember(dir.into(), Decision::Trust, "a".into());
        assert_eq!(store.trust(dir, "a"), Trust::Trusted);
        assert_eq!(store.trust(dir, "b"), Trust::Changed);
        assert_eq!(store.trust(Path::new("/q"), "a"), Trust::Unknown);
        store.remember(dir.into(), Decision::Ignore, "a".into());
        assert_eq!(store.trust(dir, "a"), Trust::Ignored);
        assert_eq!(store.trust(dir, "b"), Trust::Unknown, "a changed file asks again");
    }

    #[test]
    fn decisions_survive_a_restart_and_do_not_undo_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/nun/trust");
        let mut one = TrustStore::load(path.clone());
        let mut two = TrustStore::load(path.clone());
        one.decide("/p".into(), Decision::Trust, "a".into()).unwrap();
        two.decide("/q".into(), Decision::Ignore, "b".into()).unwrap();

        let again = TrustStore::load(path.clone());
        assert_eq!(again.trust(Path::new("/p"), "a"), Trust::Trusted);
        assert_eq!(again.trust(Path::new("/q"), "b"), Trust::Ignored);

        std::fs::write(&path, "garbage\ntrust\tc\t/r\n").unwrap();
        assert_eq!(TrustStore::load(path).trust(Path::new("/r"), "c"), Trust::Trusted);
    }
}
