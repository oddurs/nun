//! Writing whole files whose new contents were worked out somewhere else, and
//! only while they still hold what that was worked out from.
//!
//! A rename from a language server edits files nobody has open. The editor
//! reads them here, works out what each becomes on its own thread — it has the
//! server's position encoding, which this crate does not — shows that, and
//! sends back what was read beside what to write. Nothing is written over a
//! file that no longer holds what was read: the preview described that text,
//! and a file that has moved on since is one the preview says nothing about.
//!
//! **Every file is checked before any is written.** A file that changed makes
//! the whole run stop before it starts, so the ordinary way for this to go
//! wrong — someone saved one of the files meanwhile — writes nothing at all.
//!
//! **What can still go wrong halfway is said exactly.** The disk can refuse a
//! write, and a file can change in the moment between the check and its own
//! write. Either stops the run where it is: the files before it are written,
//! it is not, and nothing after it is tried. [`Written`] says which is which
//! for every file, so the caller can say precisely what happened rather than
//! "something failed".
//!
//! Taking a rewrite back is the same operation the other way round: what was
//! written becomes what is expected, and what was read becomes what to write.
//! So an undo gets the same check for free, and refuses to put the old text
//! over a file somebody has edited since.

use std::fs;
use std::path::{Path, PathBuf};

/// One file to write: what it must still hold, and what to write in its
/// place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    /// The file.
    pub path: PathBuf,
    /// What it held when its new contents were worked out, byte for byte.
    pub expect: String,
    /// What to write.
    pub text: String,
}

/// What became of one file in a [`rewrite`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written {
    /// It holds the new text now.
    Written,
    /// It was left alone because it no longer holds what was expected.
    Changed,
    /// It could not be read or written, and this is why, as a sentence. A
    /// write that failed has been undone as far as it could be, and the
    /// sentence says whether that worked.
    Failed(String),
    /// It was not tried: something before it stopped the run.
    NotReached,
}

/// Read each of `paths` as text.
///
/// A file that is not UTF-8 is an error rather than something decoded with
/// replacement characters: its text is going to be edited and written back,
/// and a lossy decode would write the damage back with it.
#[must_use]
pub fn read_texts(paths: &[PathBuf]) -> Vec<(PathBuf, Result<String, String>)> {
    paths.iter().map(|path| (path.clone(), read_text(path))).collect()
}

fn read_text(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", path.display()))
}

/// Write every file in `files`, in order, provided each still holds what it
/// is expected to.
///
/// All of them are checked first, and one that fails the check — changed, or
/// unreadable — means none is written. Past the check, the first file that
/// cannot be written, or that changed in the moment since it was checked,
/// stops the run: the ones before it stay written and nothing after it is
/// tried. Each file comes back beside what became of it, in the order given.
///
/// Files are written in place rather than through a temporary renamed over
/// them, which keeps their permissions, their inode and anything linked to
/// them. A write that fails partway has the expected text put back.
#[must_use]
pub fn rewrite(files: &[Rewrite]) -> Vec<(PathBuf, Written)> {
    let checked: Vec<Written> = files.iter().map(check).collect();
    if checked.iter().any(|written| *written != Written::NotReached) {
        return files.iter().map(|file| file.path.clone()).zip(checked).collect();
    }

    let mut outcomes = Vec::with_capacity(files.len());
    let mut stopped = false;
    for file in files {
        let outcome = if stopped { Written::NotReached } else { write(file) };
        stopped |= outcome != Written::Written;
        outcomes.push((file.path.clone(), outcome));
    }
    outcomes
}

/// Whether a file still holds what it should. `NotReached` means it passed,
/// since a file that passed has, so far, had nothing done to it.
fn check(file: &Rewrite) -> Written {
    match fs::read(&file.path) {
        Ok(bytes) if bytes == file.expect.as_bytes() => Written::NotReached,
        Ok(_) => Written::Changed,
        Err(error) => Written::Failed(format!("{}: {error}", file.path.display())),
    }
}

/// Check one file once more, then write it.
///
/// The second check narrows the window for a write landing between the check
/// of every file and this one's turn, which on a slow disk and a long list is
/// not nothing. It cannot close it: nothing short of a lock every other
/// program honours could.
fn write(file: &Rewrite) -> Written {
    match check(file) {
        Written::NotReached => {}
        other => return other,
    }
    let Err(error) = fs::write(&file.path, &file.text) else { return Written::Written };
    // The write may have truncated the file before it failed. What it held
    // is known exactly, so it goes back.
    let put_back = match fs::write(&file.path, &file.expect) {
        Ok(()) => "it was put back as it was".to_string(),
        Err(again) => format!("it could not be put back either ({again})"),
    };
    Written::Failed(format!("{}: {error}; {put_back}", file.path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    fn change(path: &Path, expect: &str, text: &str) -> Rewrite {
        Rewrite { path: path.to_path_buf(), expect: expect.into(), text: text.into() }
    }

    #[test]
    fn every_file_is_written_when_each_holds_what_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let a = file(dir.path(), "a.rs", "let cat = 1;\r\n");
        let b = file(dir.path(), "b.rs", "\u{feff}cat()");
        let read = read_texts(&[a.clone(), b.clone()]);
        assert_eq!(read[0].1.as_deref(), Ok("let cat = 1;\r\n"), "bytes, not lines");
        assert_eq!(read[1].1.as_deref(), Ok("\u{feff}cat()"), "the mark is kept");

        let outcomes = rewrite(&[
            change(&a, "let cat = 1;\r\n", "let dog = 1;\r\n"),
            change(&b, "\u{feff}cat()", "\u{feff}dog()"),
        ]);
        assert_eq!(outcomes, [(a.clone(), Written::Written), (b.clone(), Written::Written)]);
        assert_eq!(fs::read_to_string(&a).unwrap(), "let dog = 1;\r\n");
        assert_eq!(fs::read_to_string(&b).unwrap(), "\u{feff}dog()");
    }

    #[test]
    fn one_changed_file_means_none_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let a = file(dir.path(), "a.rs", "cat");
        let b = file(dir.path(), "b.rs", "cat, edited since");
        let outcomes = rewrite(&[change(&a, "cat", "dog"), change(&b, "cat", "dog")]);
        assert_eq!(outcomes, [(a.clone(), Written::NotReached), (b.clone(), Written::Changed)]);
        assert_eq!(fs::read_to_string(&a).unwrap(), "cat", "checked first, so untouched");
        assert_eq!(fs::read_to_string(&b).unwrap(), "cat, edited since");
    }

    #[test]
    fn a_missing_file_fails_the_check_and_stops_everything() {
        let dir = tempfile::tempdir().unwrap();
        let a = file(dir.path(), "a.rs", "cat");
        let gone = dir.path().join("gone.rs");
        let outcomes = rewrite(&[change(&a, "cat", "dog"), change(&gone, "cat", "dog")]);
        assert_eq!(outcomes[0], (a.clone(), Written::NotReached));
        assert!(matches!(&outcomes[1].1, Written::Failed(why) if why.contains("gone.rs")));
        assert_eq!(fs::read_to_string(&a).unwrap(), "cat");
    }

    #[cfg(unix)]
    #[test]
    fn a_write_the_disk_refuses_stops_the_run_and_says_exactly_where() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let a = file(dir.path(), "a.rs", "cat");
        let b = file(dir.path(), "b.rs", "cat");
        let c = file(dir.path(), "c.rs", "cat");
        fs::set_permissions(&b, fs::Permissions::from_mode(0o444)).unwrap();
        // Readable but not writable: it passes the check and fails the write.
        // Root can write it anyway, in which case there is nothing to see.
        if fs::OpenOptions::new().write(true).open(&b).is_ok() {
            return;
        }

        let outcomes = rewrite(&[
            change(&a, "cat", "dog"),
            change(&b, "cat", "dog"),
            change(&c, "cat", "dog"),
        ]);
        assert_eq!(outcomes[0], (a.clone(), Written::Written));
        assert!(matches!(&outcomes[1].1, Written::Failed(why) if why.contains("b.rs")));
        assert_eq!(outcomes[2], (c.clone(), Written::NotReached));
        assert_eq!(fs::read_to_string(&a).unwrap(), "dog");
        assert_eq!(fs::read_to_string(&b).unwrap(), "cat");
        assert_eq!(fs::read_to_string(&c).unwrap(), "cat");
    }

    #[test]
    fn taking_a_rewrite_back_is_the_same_rewrite_the_other_way() {
        let dir = tempfile::tempdir().unwrap();
        let a = file(dir.path(), "a.rs", "cat");
        assert_eq!(rewrite(&[change(&a, "cat", "dog")])[0].1, Written::Written);
        assert_eq!(rewrite(&[change(&a, "dog", "cat")])[0].1, Written::Written);
        assert_eq!(fs::read_to_string(&a).unwrap(), "cat");

        // Edited after the rewrite: taking it back would lose the edit.
        assert_eq!(rewrite(&[change(&a, "cat", "dog")])[0].1, Written::Written);
        fs::write(&a, "dog and more").unwrap();
        assert_eq!(rewrite(&[change(&a, "dog", "cat")])[0].1, Written::Changed);
        assert_eq!(fs::read_to_string(&a).unwrap(), "dog and more");
    }

    #[test]
    fn a_file_that_is_not_text_is_not_read_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        fs::write(&path, [0xff, 0xfe, b'c']).unwrap();
        let read = read_texts(std::slice::from_ref(&path));
        assert!(matches!(&read[0].1, Err(why) if why.contains("not UTF-8")));
    }
}
