//! The files an edit creates, moves and deletes, worked out before anything
//! is done.
//!
//! A `WorkspaceEdit` is a list of steps in order — edit this file, move that
//! one, edit it again under its new name — and each step describes the files
//! as the steps before it left them. Carried out literally, that order would
//! mean checking a file's text only after the move in front of it, halfway
//! through writing the project. So the list is sorted out first, with nothing
//! touched:
//!
//! - every text edit is traced back through the operations before it to the
//!   file it lands in as the project stands now — a file that is already
//!   there, or one an operation creates, or nothing, which is refused;
//! - edits to files already there are made where those files are now, before
//!   any operation, by the ordinary guarded rewrite, which checks every file
//!   before it writes any;
//! - edits to a file an operation creates become what it is created holding;
//! - then the operations run, in the order the server listed them.
//!
//! That comes to the same files in the same places as doing the steps one by
//! one: an edit does not care where its file sits, and a created file holding
//! its text from the start is the same file as one created empty and then
//! written.
//!
//! **What an operation may do.** Nothing is overwritten, ever, whatever the
//! server's options say: a create or a move onto something that is there is
//! refused whole, unless the server said to leave it be when it is there, in
//! which case it is left out. A delete of something that is not there is
//! refused unless the server said to leave that be too, and a folder is only
//! deleted if the server said to delete what is in it. Anything left out
//! that a later step still names is refused, because that step was worked
//! out on the files as they would have been.

use std::path::{Path, PathBuf};

use nun_lsp::types::{ResourceOp, TextEdit};
use nun_workspace::Present;

use super::FileEdits;

/// A file operation as a server asked for it, with its options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Wanted {
    Create { path: PathBuf, overwrite: bool, ignore: bool },
    Rename { from: PathBuf, to: PathBuf, overwrite: bool, ignore: bool },
    Delete { path: PathBuf, recursive: bool, ignore: bool },
}

impl Wanted {
    /// The operation a server sent, with its paths as `local` makes them, or
    /// `None` when one of them is not a file.
    pub(super) fn of(op: &ResourceOp, local: &impl Fn(PathBuf) -> PathBuf) -> Option<Self> {
        let path = |uri| nun_lsp::uri::to_path(uri).map(local);
        Some(match op {
            ResourceOp::Create(create) => {
                let options = create.options.as_ref();
                Self::Create {
                    path: path(&create.uri)?,
                    overwrite: options.and_then(|o| o.overwrite).unwrap_or(false),
                    ignore: options.and_then(|o| o.ignore_if_exists).unwrap_or(false),
                }
            }
            ResourceOp::Rename(rename) => {
                let options = rename.options.as_ref();
                Self::Rename {
                    from: path(&rename.old_uri)?,
                    to: path(&rename.new_uri)?,
                    overwrite: options.and_then(|o| o.overwrite).unwrap_or(false),
                    ignore: options.and_then(|o| o.ignore_if_exists).unwrap_or(false),
                }
            }
            ResourceOp::Delete(delete) => {
                let options = delete.options.as_ref();
                Self::Delete {
                    path: path(&delete.uri)?,
                    recursive: options.and_then(|o| o.recursive).unwrap_or(false),
                    ignore: options.and_then(|o| o.ignore_if_not_exists).unwrap_or(false),
                }
            }
        })
    }

    /// Every path it names.
    pub(super) fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Create { path, .. } | Self::Delete { path, .. } => vec![path],
            Self::Rename { from, to, .. } => vec![from, to],
        }
    }
}

/// One step of an edit, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Step {
    Edit(FileEdits),
    Op(Wanted),
}

/// Where a path named at some step is, as the project stands before any.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    /// A file that is, or is not, already there.
    There(PathBuf),
    /// The file the operation at this index creates.
    Created(usize),
    /// Nothing: the operation at this index moved or deleted what was there.
    Gone(usize),
}

/// Trace `path`, as it is after `ops`, back to before them.
fn origin(ops: &[Wanted], path: &Path) -> Origin {
    let mut path = path.to_path_buf();
    for (at, op) in ops.iter().enumerate().rev() {
        match op {
            Wanted::Rename { from, to, .. } => {
                if let Ok(rest) = path.strip_prefix(to) {
                    path = if rest.as_os_str().is_empty() { from.clone() } else { from.join(rest) };
                } else if path.starts_with(from) {
                    return Origin::Gone(at);
                }
            }
            Wanted::Create { path: made, .. } => {
                if path == *made {
                    return Origin::Created(at);
                }
                if path.starts_with(made) {
                    return Origin::Gone(at);
                }
            }
            Wanted::Delete { path: gone, .. } => {
                if path.starts_with(gone) {
                    return Origin::Gone(at);
                }
            }
        }
    }
    Origin::There(path)
}

/// An edit's steps sorted out, as the module docs describe.
#[derive(Debug, Default)]
pub(super) struct Sorted {
    /// Edits to files already there, by where they are now.
    pub(super) edits: Vec<FileEdits>,
    /// The operations, in order.
    pub(super) ops: Vec<Wanted>,
    /// Edits to files an operation creates: which operation, and the edits.
    pub(super) created: Vec<(usize, Vec<TextEdit>)>,
    /// Where it matters what is there now, for the operations to be checked.
    pub(super) probe: Vec<PathBuf>,
    /// Every path a step names, beside how many operations come before it.
    named: Vec<(usize, PathBuf)>,
}

/// Sort an edit's steps out, or say why it cannot be done as a whole.
/// `label` names a path in what is said.
pub(super) fn sort(steps: Vec<Step>, label: &impl Fn(&Path) -> String) -> Result<Sorted, String> {
    let mut sorted = Sorted::default();
    for step in steps {
        match step {
            Step::Edit(file) => {
                sorted.named.push((sorted.ops.len(), file.path.clone()));
                match origin(&sorted.ops, &file.path) {
                    Origin::There(path) => {
                        if sorted.edits.iter().any(|edited| edited.path == path) {
                            return Err(twice(&path, label));
                        }
                        sorted.edits.push(FileEdits { path, ..file });
                    }
                    Origin::Created(at) => {
                        if sorted.created.iter().any(|(made, _)| *made == at) {
                            return Err(twice(&file.path, label));
                        }
                        sorted.created.push((at, file.edits));
                    }
                    Origin::Gone(_) => {
                        return Err(format!(
                            "the server edits {} after moving or deleting it",
                            label(&file.path)
                        ));
                    }
                }
            }
            Step::Op(op) => {
                for path in op.paths() {
                    sorted.named.push((sorted.ops.len(), path.to_path_buf()));
                }
                // What is there already decides whether it can happen; a
                // path an earlier operation settles needs no looking at.
                let wanted: Vec<&Path> = match &op {
                    Wanted::Rename { from, .. }
                        if matches!(origin(&sorted.ops, from), Origin::Gone(_)) =>
                    {
                        return Err(format!(
                            "the server moves {} after moving or deleting it",
                            label(from)
                        ));
                    }
                    Wanted::Delete { path, ignore: false, .. }
                        if matches!(origin(&sorted.ops, path), Origin::Gone(_)) =>
                    {
                        return Err(format!(
                            "the server deletes {} after moving or deleting it",
                            label(path)
                        ));
                    }
                    op => op.paths(),
                };
                for path in wanted {
                    if let Origin::There(path) = origin(&sorted.ops, path)
                        && !sorted.probe.contains(&path)
                    {
                        sorted.probe.push(path);
                    }
                }
                sorted.ops.push(op);
            }
        }
    }
    Ok(sorted)
}

fn twice(path: &Path, label: &impl Fn(&Path) -> String) -> String {
    // Two lists for one file are applied one after the other, the second in
    // the coordinates the first leaves. Nothing sends that in practice, and
    // getting it subtly wrong would be worse than refusing it plainly.
    format!("the server edits {} twice over", label(path))
}

/// What becomes of one operation, now that what is there is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decided {
    /// Create the file at the path.
    Create(PathBuf),
    /// Move what is at the first path to the second.
    Move(PathBuf, PathBuf),
    /// Put what is at the path in the trash.
    Delete(PathBuf),
    /// Nothing: the server said to leave it be.
    Skip,
}

impl Sorted {
    /// Decide each operation from what `probed` says is there, or say why
    /// the edit cannot be done as a whole.
    pub(super) fn decide(
        &self,
        probed: &[(PathBuf, Present)],
        label: &impl Fn(&Path) -> String,
    ) -> Result<Vec<Decided>, String> {
        let mut decided = Vec::with_capacity(self.ops.len());
        for (at, op) in self.ops.iter().enumerate() {
            let before = &self.ops[..at];
            let present = |path: &Path| match origin(before, path) {
                Origin::There(path) => probed
                    .iter()
                    .find(|(probed, _)| *probed == path)
                    .map_or(Present::Missing, |(_, present)| *present),
                Origin::Created(_) => Present::File,
                Origin::Gone(_) => Present::Missing,
            };
            let taken = |path: &Path, overwrite: bool, ignore: bool| -> Result<bool, String> {
                if present(path) == Present::Missing {
                    Ok(false)
                } else if overwrite {
                    Err(format!(
                        "the server would put a file over {}, and nun does not overwrite files \
                         as part of an edit",
                        label(path)
                    ))
                } else if ignore {
                    Ok(true)
                } else {
                    Err(format!("the server would make {}, which is already there", label(path)))
                }
            };
            let this = match op {
                Wanted::Create { path, overwrite, ignore } => {
                    if taken(path, *overwrite, *ignore)? {
                        Decided::Skip
                    } else {
                        Decided::Create(path.clone())
                    }
                }
                Wanted::Rename { from, to, overwrite, ignore } => {
                    if present(from) == Present::Missing {
                        return Err(format!(
                            "the server moves {}, which is not there",
                            label(from)
                        ));
                    }
                    if from == to || taken(to, *overwrite, *ignore)? {
                        Decided::Skip
                    } else {
                        Decided::Move(from.clone(), to.clone())
                    }
                }
                Wanted::Delete { path, recursive, ignore } => match present(path) {
                    Present::Missing if *ignore => Decided::Skip,
                    Present::Missing => {
                        return Err(format!(
                            "the server deletes {}, which is not there",
                            label(path)
                        ));
                    }
                    Present::Dir if !recursive => {
                        return Err(format!(
                            "the server deletes the folder {} without saying to delete what \
                             is in it",
                            label(path)
                        ));
                    }
                    Present::File | Present::Dir => Decided::Delete(path.clone()),
                },
            };
            if this == Decided::Skip {
                self.nothing_later_names(at, label)?;
            }
            decided.push(this);
        }
        Ok(decided)
    }

    /// Refuse the edit if a step after operation `at`, which is being left
    /// out, names anything it names: that step expected it to happen.
    fn nothing_later_names(
        &self,
        at: usize,
        label: &impl Fn(&Path) -> String,
    ) -> Result<(), String> {
        let skipped = self.ops[at].paths();
        for (before, path) in &self.named {
            if *before > at && skipped.iter().any(|named| path.starts_with(named)) {
                return Err(format!(
                    "the server goes on to use {} as though it had been made or moved, and it \
                     was left alone because something is already there",
                    label(path)
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    fn label(path: &Path) -> String {
        path.display().to_string()
    }

    fn edit(path: &str) -> Step {
        Step::Edit(FileEdits {
            path: p(path),
            version: None,
            edits: vec![TextEdit { range: nun_lsp::types::Range::default(), new_text: "x".into() }],
        })
    }

    fn rename(from: &str, to: &str) -> Step {
        Step::Op(Wanted::Rename { from: p(from), to: p(to), overwrite: false, ignore: false })
    }

    fn create(path: &str, ignore: bool) -> Step {
        Step::Op(Wanted::Create { path: p(path), overwrite: false, ignore })
    }

    #[test]
    fn an_edit_under_a_new_name_lands_on_the_file_as_it_is_now() {
        let sorted = sort(
            vec![edit("/p/lib.rs"), rename("/p/foo.rs", "/p/bar.rs"), edit("/p/bar.rs")],
            &label,
        )
        .unwrap();
        let paths: Vec<&Path> = sorted.edits.iter().map(|file| file.path.as_path()).collect();
        assert_eq!(paths, [Path::new("/p/lib.rs"), Path::new("/p/foo.rs")]);
        assert_eq!(sorted.probe, [p("/p/foo.rs"), p("/p/bar.rs")]);
    }

    #[test]
    fn a_folder_moved_carries_the_files_in_it() {
        let sorted = sort(vec![rename("/p/foo", "/p/bar"), edit("/p/bar/mod.rs")], &label).unwrap();
        assert_eq!(sorted.edits[0].path, p("/p/foo/mod.rs"));
    }

    #[test]
    fn edits_to_a_created_file_are_what_it_is_created_with() {
        let sorted = sort(vec![create("/p/new.rs", false), edit("/p/new.rs")], &label).unwrap();
        assert!(sorted.edits.is_empty());
        assert_eq!(sorted.created.len(), 1);
        assert_eq!(sorted.created[0].0, 0);
    }

    #[test]
    fn an_edit_to_what_was_moved_away_is_refused() {
        let refused = sort(vec![rename("/p/foo.rs", "/p/bar.rs"), edit("/p/foo.rs")], &label);
        assert!(refused.unwrap_err().contains("after moving or deleting it"));
    }

    #[test]
    fn one_file_edited_under_both_its_names_is_refused() {
        let refused = sort(
            vec![edit("/p/foo.rs"), rename("/p/foo.rs", "/p/bar.rs"), edit("/p/bar.rs")],
            &label,
        );
        assert!(refused.unwrap_err().contains("twice"));
    }

    #[test]
    fn nothing_is_overwritten_whatever_the_options_say() {
        let over = Step::Op(Wanted::Create { path: p("/p/a.rs"), overwrite: true, ignore: true });
        let sorted = sort(vec![over], &label).unwrap();
        let probed = [(p("/p/a.rs"), Present::File)];
        assert!(sorted.decide(&probed, &label).unwrap_err().contains("does not overwrite"));

        let sorted = sort(vec![rename("/p/a.rs", "/p/b.rs")], &label).unwrap();
        let probed = [(p("/p/a.rs"), Present::File), (p("/p/b.rs"), Present::File)];
        assert!(sorted.decide(&probed, &label).unwrap_err().contains("already there"));
    }

    #[test]
    fn a_create_told_to_ignore_what_is_there_is_left_out_unless_something_later_needs_it() {
        let sorted = sort(vec![create("/p/a.rs", true)], &label).unwrap();
        let probed = [(p("/p/a.rs"), Present::File)];
        assert_eq!(sorted.decide(&probed, &label).unwrap(), [Decided::Skip]);

        let sorted = sort(vec![create("/p/a.rs", true), edit("/p/a.rs")], &label).unwrap();
        assert!(sorted.decide(&probed, &label).unwrap_err().contains("already there"));
    }

    #[test]
    fn a_move_of_a_created_file_needs_no_look_at_the_disk() {
        let sorted =
            sort(vec![create("/p/a.rs", false), rename("/p/a.rs", "/p/b.rs")], &label).unwrap();
        assert_eq!(sorted.probe, [p("/p/a.rs"), p("/p/b.rs")]);
        let probed = [(p("/p/a.rs"), Present::Missing), (p("/p/b.rs"), Present::Missing)];
        assert_eq!(
            sorted.decide(&probed, &label).unwrap(),
            [Decided::Create(p("/p/a.rs")), Decided::Move(p("/p/a.rs"), p("/p/b.rs"))]
        );
    }

    #[test]
    fn a_folder_is_deleted_only_when_the_server_says_what_is_in_it_goes_too() {
        let delete =
            |recursive| Step::Op(Wanted::Delete { path: p("/p/old"), recursive, ignore: false });
        let probed = [(p("/p/old"), Present::Dir)];
        let sorted = sort(vec![delete(false)], &label).unwrap();
        assert!(sorted.decide(&probed, &label).is_err());
        let sorted = sort(vec![delete(true)], &label).unwrap();
        assert_eq!(sorted.decide(&probed, &label).unwrap(), [Decided::Delete(p("/p/old"))]);
    }
}
