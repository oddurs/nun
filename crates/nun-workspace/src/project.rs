//! What to call the project: its GitHub repository when it is a checkout of
//! one, and its folder otherwise.
//!
//! The repository is found by reading git's own files rather than by running
//! `git`: a process per workspace is a lot to pay for one label, and `git` may
//! not be installed at all. That means reading only what the label needs — the
//! nearest `.git`, the `gitdir:` file a worktree or submodule leaves in its
//! place, and the `[remote]` sections of the config it leads to.
//!
//! Two things git itself would do are deliberately not done: `url.<base>.insteadOf`
//! rewrites are not applied, and `[include]` and `[includeIf]` directives are not
//! followed. Both are rare for a remote's URL, and following includes would
//! mean reading files anywhere on disk to draw a header. A remote that only
//! becomes a GitHub URL through either shows the folder name instead.

use std::fs;
use std::path::{Path, PathBuf};

/// What to call the project rooted at `root`: `owner/repo` when the nearest
/// repository at or above it has a GitHub remote, and the folder's name
/// otherwise.
///
/// Reads files, so it belongs on a worker — see [`crate::Job::ProjectName`].
#[must_use]
pub fn project_name(root: &Path) -> String {
    github_repo(root).unwrap_or_else(|| folder_name(root))
}

/// The name of the folder `root` is, as it is on disk, or the whole path when
/// it has no final component (`/`).
#[must_use]
pub fn folder_name(root: &Path) -> String {
    root.file_name()
        .map_or_else(|| root.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// `owner/repo` for the nearest repository at or above `start`, when one of
/// its remotes is on GitHub.
#[must_use]
pub fn github_repo(start: &Path) -> Option<String> {
    let config = fs::read_to_string(config_path(start)?).ok()?;
    pick_github(&remotes(&config))
}

/// The config file of the nearest repository at or above `start`.
///
/// `.git` is usually a directory. In a linked worktree or a submodule it is a
/// file reading `gitdir: <path>` instead, and for a worktree the directory it
/// names holds a `commondir` file pointing at the repository the worktree
/// shares its config with.
#[must_use]
pub fn config_path(start: &Path) -> Option<PathBuf> {
    let dot_git = start.ancestors().map(|dir| dir.join(".git")).find(|path| path.exists())?;
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = fs::read_to_string(&dot_git).ok()?;
        let target = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?.trim();
        // Relative to the folder the `.git` file is in.
        dot_git.parent()?.join(target)
    };
    let common = match fs::read_to_string(git_dir.join("commondir")) {
        // Relative to the git dir it is in, or absolute.
        Ok(text) => git_dir.join(text.trim()),
        Err(_) => git_dir,
    };
    Some(common.join("config"))
}

/// The remote to name the project after: `origin` when it is on GitHub,
/// otherwise the first remote that is.
#[must_use]
pub fn pick_github(remotes: &[(String, String)]) -> Option<String> {
    let origin = remotes.iter().find(|(name, _)| name == "origin");
    origin
        .and_then(|(_, url)| github_slug(url))
        .or_else(|| remotes.iter().find_map(|(_, url)| github_slug(url)))
}

/// Each `[remote "name"]` section in a git config, with its first `url`, in
/// the order they appear.
///
/// A small reader for the part of the format a remote uses: section names are
/// matched without regard to case and subsection names with it, `#` and `;`
/// start a comment outside quotes, and a value may be quoted. The deprecated
/// `[remote.name]` spelling is read too.
#[must_use]
pub fn remotes(config: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    // The remote whose section we are in, and whether it has its url yet.
    let mut current: Option<String> = None;
    for line in config.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            current = remote_section(header);
            continue;
        }
        let Some(name) = &current else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        if !key.trim().eq_ignore_ascii_case("url") {
            continue;
        }
        let value = unquote(value);
        if !value.is_empty() && !found.iter().any(|(seen, _)| seen == name) {
            found.push((name.clone(), value));
        }
    }
    found
}

/// The remote a section header names, given everything after its `[`.
fn remote_section(header: &str) -> Option<String> {
    let header = header.split_once(']')?.0.trim();
    if let Some((section, sub)) = header.split_once(char::is_whitespace) {
        let sub = sub.trim().strip_prefix('"')?.strip_suffix('"')?;
        return section
            .eq_ignore_ascii_case("remote")
            .then(|| sub.replace("\\\"", "\"").replace("\\\\", "\\"));
    }
    let (section, sub) = header.split_once('.')?;
    (section.eq_ignore_ascii_case("remote") && !sub.is_empty()).then(|| sub.to_lowercase())
}

/// A config value with its quotes, escapes and trailing comment dealt with.
fn unquote(raw: &str) -> String {
    let mut value = String::new();
    let mut quoted = false;
    let mut chars = raw.trim().chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => quoted = !quoted,
            '\\' => match chars.next() {
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some(other) => value.push(other),
                None => {}
            },
            '#' | ';' if !quoted => break,
            _ => value.push(ch),
        }
    }
    value.trim().to_string()
}

/// `owner/repo` when `url` is a GitHub remote, in any of the forms git
/// accepts for one: `https://`, `http://`, `ssh://` (with or without a port),
/// `git://`, and the scp-like `git@github.com:owner/repo`, each with or
/// without a user, a `www.`, a trailing `.git` and a trailing slash.
#[must_use]
pub fn github_slug(url: &str) -> Option<String> {
    let url = url.trim();
    let (authority, path) = if let Some((scheme, rest)) = url.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git") {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        // A port, if there is one, follows the host.
        let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        (host.split_once(':').map_or(host, |(host, _)| host), path)
    } else {
        // The scp-like form: a colon before any slash, and no scheme.
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') {
            return None;
        }
        (authority.rsplit_once('@').map_or(authority, |(_, host)| host), path)
    };
    let host = authority.to_ascii_lowercase();
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    let (owner, repo) = path.split_once('/')?;
    let valid = |part: &str| !part.is_empty() && !part.contains(['/', '?', '#', ' ']);
    (valid(owner) && valid(repo)).then(|| format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_github_url_form_names_the_repository() {
        for url in [
            "https://github.com/oddurs/nun",
            "https://github.com/oddurs/nun.git",
            "https://github.com/oddurs/nun/",
            "https://github.com/oddurs/nun.git/",
            "http://github.com/oddurs/nun",
            "git@github.com:oddurs/nun",
            "git@github.com:oddurs/nun.git",
            "git@github.com:oddurs/nun/",
            "ssh://git@github.com/oddurs/nun",
            "ssh://git@github.com/oddurs/nun.git",
            "ssh://git@github.com:22/oddurs/nun.git",
            "git://github.com/oddurs/nun",
            "git://github.com/oddurs/nun.git",
            "https://www.github.com/oddurs/nun",
            "git@www.github.com:oddurs/nun.git",
            "https://user@github.com/oddurs/nun",
            "https://user:token@github.com/oddurs/nun.git",
            "HTTPS://GitHub.com/oddurs/nun",
            "  https://github.com/oddurs/nun  ",
        ] {
            assert_eq!(github_slug(url).as_deref(), Some("oddurs/nun"), "{url}");
        }
    }

    #[test]
    fn a_repository_name_keeps_its_case() {
        assert_eq!(
            github_slug("git@github.com:Rust-Lang/Rust.git").as_deref(),
            Some("Rust-Lang/Rust")
        );
    }

    #[test]
    fn anything_not_on_github_is_not_named() {
        for url in [
            "https://gitlab.com/oddurs/nun.git",
            "git@gitlab.com:oddurs/nun.git",
            "https://github.example.com/oddurs/nun",
            "https://notgithub.com/oddurs/nun",
            "/srv/git/nun.git",
            "../nun",
            "file:///srv/git/nun.git",
            "file://github.com/oddurs/nun",
            "github.com/oddurs/nun",
            "https://github.com/oddurs",
            "https://github.com/oddurs/nun/tree/main",
            "https://github.com/",
            "",
        ] {
            assert_eq!(github_slug(url), None, "{url}");
        }
    }

    #[test]
    fn remotes_are_read_from_their_sections() {
        let config = "\
[core]
\trepositoryformatversion = 0
\tbare = false
# a comment, with [remote \"fake\"] in it
; another
[remote \"upstream\"]
\turl = git@gitlab.com:someone/nun.git
\tfetch = +refs/heads/*:refs/remotes/upstream/*
[Remote   \"my fork\"]  # trailing comment
  URL=\"https://github.com/someone/nun\" ; quoted, with a comment
[remote \"origin\"]
\turl = https://github.com/oddurs/nun.git # the real one
\turl = https://github.com/elsewhere/nun.git
[branch \"main\"]
\tremote = origin
\turl = https://github.com/not/a-remote
[remote.legacy]
\turl = git://github.com/legacy/nun
";
        assert_eq!(
            remotes(config),
            vec![
                ("upstream".to_string(), "git@gitlab.com:someone/nun.git".to_string()),
                ("my fork".to_string(), "https://github.com/someone/nun".to_string()),
                ("origin".to_string(), "https://github.com/oddurs/nun.git".to_string()),
                ("legacy".to_string(), "git://github.com/legacy/nun".to_string()),
            ]
        );
    }

    #[test]
    fn origin_wins_when_it_is_on_github() {
        let config = "\
[remote \"fork\"]\n\turl = git@github.com:someone/nun.git
[remote \"origin\"]\n\turl = git@github.com:oddurs/nun.git
";
        assert_eq!(pick_github(&remotes(config)).as_deref(), Some("oddurs/nun"));
    }

    #[test]
    fn without_a_github_origin_the_first_github_remote_is_used() {
        let config = "\
[remote \"origin\"]\n\turl = git@gitlab.com:oddurs/nun.git
[remote \"mirror\"]\n\turl = /srv/git/nun.git
[remote \"github\"]\n\turl = https://github.com/oddurs/nun
[remote \"other\"]\n\turl = https://github.com/someone/nun
";
        assert_eq!(pick_github(&remotes(config)).as_deref(), Some("oddurs/nun"));
    }

    #[test]
    fn no_github_remote_names_nothing() {
        let config = "[remote \"origin\"]\n\turl = git@gitlab.com:oddurs/nun.git\n";
        assert_eq!(pick_github(&remotes(config)), None);
        assert_eq!(pick_github(&[]), None);
    }

    const ORIGIN: &str = "[remote \"origin\"]\n\turl = git@github.com:oddurs/nun.git\n";

    #[test]
    fn a_checkout_is_named_after_its_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("nun");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), ORIGIN).unwrap();
        assert_eq!(project_name(&root), "oddurs/nun");
    }

    #[test]
    fn a_folder_inside_a_checkout_finds_the_repository_above_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("nun");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("crates/nun-core")).unwrap();
        fs::write(root.join(".git/config"), ORIGIN).unwrap();
        assert_eq!(project_name(&root.join("crates/nun-core")), "oddurs/nun");
    }

    #[test]
    fn a_worktree_reads_the_config_it_shares() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("nun");
        fs::create_dir_all(main.join(".git/worktrees/feature")).unwrap();
        fs::write(main.join(".git/config"), ORIGIN).unwrap();
        fs::write(main.join(".git/worktrees/feature/commondir"), "../..\n").unwrap();

        let tree = dir.path().join("worktrees/feature");
        fs::create_dir_all(&tree).unwrap();
        let gitdir = main.join(".git/worktrees/feature");
        fs::write(tree.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();

        assert_eq!(project_name(&tree), "oddurs/nun");
    }

    #[test]
    fn a_submodule_reads_its_own_config_through_a_relative_gitdir() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer");
        fs::create_dir_all(outer.join(".git/modules/inner")).unwrap();
        fs::write(outer.join(".git/config"), ORIGIN).unwrap();
        fs::write(
            outer.join(".git/modules/inner/config"),
            "[remote \"origin\"]\n\turl = https://github.com/someone/inner\n",
        )
        .unwrap();
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join(".git"), "gitdir: ../.git/modules/inner\n").unwrap();

        assert_eq!(project_name(&inner), "someone/inner");
    }

    #[test]
    fn a_repository_without_a_github_remote_is_named_after_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("My Project");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "[core]\n\tbare = false\n").unwrap();
        assert_eq!(project_name(&root), "My Project");
    }

    #[test]
    fn a_repository_without_a_config_is_named_after_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("nun");
        fs::create_dir_all(root.join(".git")).unwrap();
        assert_eq!(project_name(&root), "nun");
    }

    #[test]
    fn a_folder_outside_any_repository_is_named_as_it_is_on_disk() {
        // Assumes the temporary directory is not itself inside a checkout,
        // which is true of every platform's default.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Notes 日本");
        fs::create_dir_all(&root).unwrap();
        assert_eq!(project_name(&root), "Notes 日本");
        assert_eq!(folder_name(Path::new("/")), "/");
    }
}
