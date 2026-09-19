//! Finding files by name, and scoring what the user typed against them.
//!
//! Both halves live on the worker: walking a large repository takes as long
//! as it takes, and scoring a hundred thousand paths on every keystroke is
//! not something a frame should wait for.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Most files one project is listed as.
///
/// A repository larger than this is one where the palette's own listing is
/// not the answer anyway, and the cap keeps a runaway walk — a symlinked
/// mount, a home directory opened by accident — from eating memory.
pub const MOST_FILES: usize = 200_000;

/// One match: which candidate, how well it scored, and where it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Its position in the list that was searched.
    pub index: usize,
    /// Higher is better.
    pub score: u32,
    /// Char offsets that matched, for highlighting.
    pub matched: Vec<u32>,
}

/// Every file under `root` that the ignore rules keep, relative to it.
///
/// Sorted shortest-path-first, so a search with no query at all offers the
/// files nearest the top of the project.
#[must_use]
pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkBuilder::new(root)
        .hidden(true)
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .take(MOST_FILES)
        .collect();
    files.sort_by_key(|path| (path.components().count(), path.as_os_str().len()));
    files
}

/// Score `query` against `candidates`, best first.
///
/// An empty query keeps the candidates in the order they came in, which is
/// what makes an empty palette useful rather than blank.
#[must_use]
pub fn search(candidates: &[String], query: &str, limit: usize) -> Vec<Match> {
    if query.is_empty() {
        return candidates
            .iter()
            .enumerate()
            .take(limit)
            .map(|(index, _)| Match { index, score: 0, matched: Vec::new() })
            .collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

    let mut buffer = Vec::new();
    let mut found: Vec<Match> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            buffer.clear();
            let text = Utf32Str::new(candidate, &mut buffer);
            let mut positions = Vec::new();
            let score = pattern.indices(text, &mut matcher, &mut positions)?;
            positions.sort_unstable();
            positions.dedup();
            Some(Match { index, score, matched: positions })
        })
        .collect();

    // Best score first. Two candidates often score the same — `config.rs`
    // and `a/b/config.rs` do — and then the shorter one is the one meant;
    // after that the order they came in, which is where frecency has already
    // had its say.
    found.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| candidates[a.index].len().cmp(&candidates[b.index].len()))
            .then(a.index.cmp(&b.index))
    });
    found.truncate(limit);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn an_empty_query_keeps_the_order_it_was_given() {
        let candidates = strings(&["b.rs", "a.rs", "c.rs"]);
        let found = search(&candidates, "", 10);
        assert_eq!(found.iter().map(|m| m.index).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn a_query_finds_what_contains_its_letters_in_order() {
        let candidates = strings(&["src/main.rs", "src/app/tabs.rs", "README.md"]);
        let found = search(&candidates, "tabs", 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].index, 1);
        assert!(!found[0].matched.is_empty(), "it says where it matched");
    }

    #[test]
    fn the_matched_characters_are_the_ones_that_matched() {
        let candidates = strings(&["alpha"]);
        let found = search(&candidates, "aph", 10);
        let matched: Vec<usize> = found[0].matched.iter().map(|index| *index as usize).collect();
        let letters: String =
            matched.iter().map(|index| "alpha".chars().nth(*index).unwrap()).collect();
        assert_eq!(letters, "aph");
    }

    #[test]
    fn a_closer_match_scores_higher() {
        let candidates = strings(&["a/b/config.rs", "config.rs"]);
        let found = search(&candidates, "config", 10);
        assert_eq!(found[0].index, 1, "the shorter path wins: {found:?}");
    }

    #[test]
    fn nothing_matching_finds_nothing() {
        assert!(search(&strings(&["a.rs"]), "zzz", 10).is_empty());
    }

    #[test]
    fn the_limit_is_respected() {
        let candidates: Vec<String> = (0..100).map(|index| format!("file{index}.rs")).collect();
        assert_eq!(search(&candidates, "file", 10).len(), 10);
        assert_eq!(search(&candidates, "", 5).len(), 5);
    }

    #[test]
    fn non_ascii_queries_match_non_ascii_names() {
        let candidates = strings(&["日本語.rs", "café.rs"]);
        assert_eq!(search(&candidates, "語", 10)[0].index, 0);
        assert_eq!(search(&candidates, "caf", 10)[0].index, 1);
    }

    #[test]
    fn listing_a_project_leaves_out_what_the_ignore_rules_do() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".gitignore"), "target\n").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "").unwrap();
        fs::write(dir.path().join("README.md"), "").unwrap();
        fs::write(dir.path().join("target/big.o"), "").unwrap();

        let files = list_files(dir.path());
        let names: Vec<String> = files.iter().map(|path| path.display().to_string()).collect();
        assert!(names.contains(&"README.md".to_string()), "{names:?}");
        assert!(names.contains(&"src/main.rs".to_string()), "{names:?}");
        assert!(!names.iter().any(|name| name.contains("target")), "{names:?}");
        assert!(!names.iter().any(|name| name.contains(".git")), "{names:?}");
    }

    #[test]
    fn the_listing_offers_the_top_of_the_project_first() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        fs::write(dir.path().join("a/b/c/deep.rs"), "").unwrap();
        fs::write(dir.path().join("top.rs"), "").unwrap();

        let files = list_files(dir.path());
        assert_eq!(files[0], Path::new("top.rs"));
    }

    #[test]
    fn searching_a_hundred_thousand_paths_is_quick() {
        let candidates: Vec<String> =
            (0..100_000).map(|index| format!("src/module{index}/file{index}.rs")).collect();
        let start = std::time::Instant::now();
        let found = search(&candidates, "mod99file99", 50);
        let took = start.elapsed();
        assert!(!found.is_empty());
        assert!(took < std::time::Duration::from_secs(2), "{took:?}");
    }
}
