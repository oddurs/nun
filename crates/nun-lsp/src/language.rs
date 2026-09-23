//! Which language a file is, to a language server, and where its project is.
//!
//! Separate from the grammars nun highlights with: a language can have a
//! server and no grammar (TypeScript, Go, C) or a grammar and no server worth
//! starting (TOML, SQL), and the two lists should not have to agree.

use std::path::{Path, PathBuf};

/// A language as the protocol and the configuration name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Language {
    /// The protocol's `languageId`, sent with each document.
    pub id: &'static str,
    /// The `[lsp.<name>]` section that says which server runs it. Several ids
    /// share one — `typescriptreact` is served by the `typescript` server.
    pub config: &'static str,
    /// Files whose presence marks the top of a project in this language,
    /// nearest first.
    markers: &'static [&'static str],
}

const RUST: &[&str] = &["Cargo.toml"];
const PYTHON: &[&str] = &["pyproject.toml", "setup.py", "setup.cfg", "pyrightconfig.json"];
const SCRIPT: &[&str] = &["tsconfig.json", "jsconfig.json", "package.json"];
const GO: &[&str] = &["go.work", "go.mod"];
const C: &[&str] = &["compile_commands.json", "compile_flags.txt", ".clangd"];

const fn language(
    id: &'static str,
    config: &'static str,
    markers: &'static [&'static str],
) -> Language {
    Language { id, config, markers }
}

/// Every language nun can start a server for, by the name of its section.
#[must_use]
pub fn names() -> &'static [&'static str] {
    &["rust", "python", "typescript", "javascript", "go", "c", "cpp"]
}

/// The language of a file, by its name.
#[must_use]
pub fn of_path(path: &Path) -> Option<Language> {
    let extension = path.extension()?.to_str()?;
    Some(match extension {
        "rs" => language("rust", "rust", RUST),
        "py" | "pyi" => language("python", "python", PYTHON),
        "ts" | "mts" | "cts" => language("typescript", "typescript", SCRIPT),
        "tsx" => language("typescriptreact", "typescript", SCRIPT),
        "js" | "mjs" | "cjs" => language("javascript", "javascript", SCRIPT),
        "jsx" => language("javascriptreact", "javascript", SCRIPT),
        "go" => language("go", "go", GO),
        "c" | "h" => language("c", "c", C),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => language("cpp", "cpp", C),
        _ => return None,
    })
}

impl Language {
    /// The folder a server for `file` should be rooted at.
    ///
    /// The nearest folder above it holding one of the language's markers; for
    /// Rust, the outermost `Cargo.toml` above that which declares a
    /// `[workspace]`, so every crate of a workspace shares one server rather
    /// than each starting its own. Failing a marker, the nearest folder under
    /// version control, and failing that, the file's own folder.
    ///
    /// This reads the filesystem, so it runs with the servers, never on the
    /// thread that draws.
    #[must_use]
    pub fn root(&self, file: &Path) -> PathBuf {
        let folder = file.parent().unwrap_or(file);
        let ancestors = || folder.ancestors();
        let nearest = ancestors().find(|dir| self.markers.iter().any(|m| dir.join(m).exists()));
        if let Some(nearest) = nearest {
            if self.config == "rust" {
                let workspace = nearest.ancestors().filter(|dir| declares_workspace(dir)).last();
                return workspace.unwrap_or(nearest).to_path_buf();
            }
            return nearest.to_path_buf();
        }
        ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(folder).to_path_buf()
    }
}

fn declares_workspace(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("Cargo.toml"))
        .is_ok_and(|manifest| manifest.lines().any(|line| line.trim() == "[workspace]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_are_known_by_extension_and_share_sections() {
        assert_eq!(of_path(Path::new("src/main.rs")).map(|l| l.id), Some("rust"));
        let tsx = of_path(Path::new("App.tsx")).expect("known");
        assert_eq!((tsx.id, tsx.config), ("typescriptreact", "typescript"));
        assert_eq!(of_path(Path::new("Makefile")), None);
        assert_eq!(of_path(Path::new("notes.txt")), None);
        for name in names() {
            assert!(
                ["x.rs", "x.py", "x.ts", "x.js", "x.go", "x.c", "x.cpp"]
                    .iter()
                    .any(|file| of_path(Path::new(file)).is_some_and(|l| l.config == *name)),
                "{name} has no file that reaches it"
            );
        }
    }

    #[test]
    fn a_crate_in_a_workspace_is_rooted_at_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path();
        std::fs::write(top.join("Cargo.toml"), "[workspace]\nmembers = [\"crates/*\"]\n").unwrap();
        let member = top.join("crates/one");
        std::fs::create_dir_all(member.join("src")).unwrap();
        std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"one\"\n").unwrap();

        let rust = of_path(Path::new("x.rs")).unwrap();
        assert_eq!(rust.root(&member.join("src/lib.rs")), top);
    }

    #[test]
    fn a_lone_crate_is_rooted_at_its_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let krate = dir.path().join("one");
        std::fs::create_dir_all(krate.join("src")).unwrap();
        std::fs::write(krate.join("Cargo.toml"), "[package]\nname = \"one\"\n").unwrap();
        let rust = of_path(Path::new("x.rs")).unwrap();
        assert_eq!(rust.root(&krate.join("src/main.rs")), krate);
    }

    #[test]
    fn with_no_marker_the_repository_then_the_folder_is_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("deep/er")).unwrap();
        let python = of_path(Path::new("x.py")).unwrap();
        assert_eq!(python.root(&repo.join("deep/er/x.py")), repo);

        let loose = dir.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        // Without a repository anywhere above: assumes the temporary folder
        // is not inside one, which is true of every platform's temp dir.
        assert_eq!(python.root(&loose.join("x.py")), loose);
    }
}
