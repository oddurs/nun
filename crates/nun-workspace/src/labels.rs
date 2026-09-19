//! What to call each open file on its tab.
//!
//! A file name is what people recognise, so a tab shows just that — until two
//! tabs would say the same thing. Then each grows by folders, one at a time,
//! until they differ. Only the tabs that clash grow: opening a second
//! `mod.rs` must not turn every other tab into a path.

use std::ffi::OsStr;
use std::path::Path;

/// What an unsaved, never-written buffer is called.
pub const UNNAMED: &str = "untitled";

/// A label for each path, in the same order, disambiguated against each other.
///
/// Files with the same name grow leftwards by one folder at a time until they
/// differ: `src/mod.rs` and `tests/mod.rs` become `src/mod.rs` and
/// `tests/mod.rs`, while a lone `main.rs` stays `main.rs`. Two paths that are
/// the same all the way to the root are left identical, because they are the
/// same file.
#[must_use]
pub fn tab_labels(paths: &[Option<&Path>]) -> Vec<String> {
    let mut labels: Vec<String> = paths.iter().map(|path| name_of(*path)).collect();
    // Parents, nearest first, for growing a label that clashes.
    let ancestors: Vec<Vec<&OsStr>> = paths
        .iter()
        .map(|path| {
            path.map(|path| {
                let mut names: Vec<&OsStr> =
                    path.parent().map(|parent| parent.iter().collect()).unwrap_or_default();
                names.reverse();
                names
            })
            .unwrap_or_default()
        })
        .collect();

    for depth in 0..ancestors.iter().map(Vec::len).max().unwrap_or(0) {
        let clashing = clashes(&labels);
        if clashing.is_empty() {
            break;
        }
        for index in clashing {
            // A path with no folder left to add has nothing to grow by.
            if let Some(name) = ancestors[index].get(depth) {
                labels[index] = format!("{}/{}", name.to_string_lossy(), labels[index]);
            }
        }
    }
    labels
}

/// Indices of labels that more than one tab shares.
fn clashes(labels: &[String]) -> Vec<usize> {
    let mut clashing = Vec::new();
    for (index, label) in labels.iter().enumerate() {
        if labels.iter().enumerate().any(|(other, same)| other != index && same == label) {
            clashing.push(index);
        }
    }
    clashing
}

fn name_of(path: Option<&Path>) -> String {
    path.and_then(Path::file_name)
        .map_or_else(|| UNNAMED.to_string(), |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn labels(paths: &[&str]) -> Vec<String> {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let refs: Vec<Option<&Path>> = paths.iter().map(|path| Some(path.as_path())).collect();
        tab_labels(&refs)
    }

    #[test]
    fn a_tab_is_the_file_name() {
        assert_eq!(labels(&["src/main.rs", "README.md"]), ["main.rs", "README.md"]);
    }

    #[test]
    fn two_of_the_same_name_grow_by_their_folder() {
        assert_eq!(labels(&["src/mod.rs", "tests/mod.rs"]), ["src/mod.rs", "tests/mod.rs"]);
    }

    #[test]
    fn only_the_tabs_that_clash_grow() {
        assert_eq!(
            labels(&["src/mod.rs", "tests/mod.rs", "build.rs"]),
            ["src/mod.rs", "tests/mod.rs", "build.rs"]
        );
    }

    #[test]
    fn they_grow_until_they_differ_not_merely_by_one_folder() {
        assert_eq!(
            labels(&["a/deep/mod.rs", "b/deep/mod.rs"]),
            ["a/deep/mod.rs", "b/deep/mod.rs"],
            "one folder is not enough: both would say deep/mod.rs"
        );
    }

    #[test]
    fn a_tab_that_already_differs_stops_growing() {
        assert_eq!(
            labels(&["one/x/mod.rs", "two/mod.rs", "three/mod.rs"]),
            ["x/mod.rs", "two/mod.rs", "three/mod.rs"]
        );
    }

    #[test]
    fn three_of_a_kind_all_grow() {
        let grown = labels(&["a/mod.rs", "b/mod.rs", "c/mod.rs"]);
        assert_eq!(grown, ["a/mod.rs", "b/mod.rs", "c/mod.rs"]);
    }

    #[test]
    fn a_buffer_with_no_path_is_untitled() {
        assert_eq!(tab_labels(&[None]), [UNNAMED]);
        assert_eq!(tab_labels(&[None, None]), [UNNAMED, UNNAMED], "and they stay that way");
    }

    #[test]
    fn the_same_file_twice_keeps_one_name() {
        assert_eq!(labels(&["src/main.rs", "src/main.rs"]), ["src/main.rs", "src/main.rs"]);
    }

    #[test]
    fn non_ascii_names_survive() {
        assert_eq!(labels(&["日本/語.rs", "中文/語.rs"]), ["日本/語.rs", "中文/語.rs"]);
    }

    #[test]
    fn a_file_at_the_root_has_nothing_to_grow_by() {
        let grown = labels(&["/mod.rs", "src/mod.rs"]);
        assert_eq!(grown[1], "src/mod.rs");
        assert!(grown[0].ends_with("mod.rs"), "{grown:?}");
    }
}
