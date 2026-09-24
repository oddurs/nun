//! nun-vcs against real repositories, made with the `git` command.
//!
//! Each test skips itself when no `git` is installed, since what is under
//! test is agreeing with git, and there is nothing to agree with.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use nun_vcs::{Against, FileStatus, HunkKind, LineMark, Reply, Repo, Request, Vcs};
use ropey::Rope;
use tempfile::TempDir;

/// Run git in `dir` with nothing from the user's own configuration, and
/// nothing from the environment either: run from a git hook, `GIT_DIR` points
/// at the repository being pushed, and every command here would act on it.
fn git(dir: &Path, args: &[&str]) -> String {
    let mut git = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            git.env_remove(name);
        }
    }
    let out = git
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=nun", "-c", "user.email=nun@example.com"])
        .args(["-c", "init.defaultBranch=main", "-c", "protocol.file.allow=always"])
        .args(["-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn has_git() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|out| out.status.success())
}

/// A repository with `file` committed holding `text`.
fn repo_with(file: &str, text: &str) -> TempDir {
    let dir = TempDir::new().expect("temp dir");
    git(dir.path(), &["init", "-q"]);
    write(&dir.path().join(file), text);
    git(dir.path(), &["add", file]);
    git(dir.path(), &["commit", "-qm", "first"]);
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn staged_text(repo: &Repo, rel: &str) -> String {
    let blob = repo.index_blob(rel.into()).expect("index reads").expect("a staged blob");
    String::from_utf8(blob.data).expect("utf-8")
}

/// A running `Vcs` and where its replies land.
fn vcs() -> (Vcs, Receiver<Reply>) {
    let (send, receive) = mpsc::channel();
    let vcs = Vcs::new(Box::new(move |reply| {
        let _ = send.send(reply);
    }));
    (vcs, receive)
}

fn next(replies: &Receiver<Reply>) -> Reply {
    replies.recv_timeout(Duration::from_secs(20)).expect("a reply")
}

fn open(
    vcs: &Vcs,
    replies: &Receiver<Reply>,
    path: PathBuf,
    text: &str,
) -> Option<Vec<(u32, LineMark)>> {
    vcs.send(Request::Open { id: 1, version: 1, path, text: Rope::from_str(text) });
    match next(replies) {
        Reply::Hunks { id: 1, version: 1, diff } => diff.map(|d| d.marks(0..u32::MAX).collect()),
        other => panic!("expected hunks, got {other:?}"),
    }
}

#[test]
fn hunks_follow_the_buffer_not_the_file_on_disk() {
    if !has_git() {
        return;
    }
    let dir = repo_with("a.txt", "one\ntwo\nthree\n");
    let path = dir.path().join("a.txt");
    let (vcs, replies) = vcs();

    // On disk nothing changed; in the buffer, the second line did.
    let marks = open(&vcs, &replies, path, "one\n2\nthree\n").expect("a diff");
    assert_eq!(marks, vec![(1, LineMark::Modified)]);

    vcs.send(Request::Update {
        id: 1,
        version: 2,
        text: Rope::from_str("one\ntwo\nthree\nfour\n"),
    });
    let Reply::Hunks { version: 2, diff: Some(diff), .. } = next(&replies) else { panic!("hunks") };
    assert_eq!(diff.hunks().len(), 1);
    assert_eq!(diff.hunks()[0].kind(), HunkKind::Added);
    assert_eq!(diff.mark(3), Some(LineMark::Added));
}

#[test]
fn updates_in_a_burst_are_answered_once_for_the_newest() {
    if !has_git() {
        return;
    }
    let dir = repo_with("a.txt", "a\n");
    let (vcs, replies) = vcs();
    open(&vcs, &replies, dir.path().join("a.txt"), "a\n");
    for lines in 2..50_usize {
        vcs.send(Request::Update {
            id: 1,
            version: u64::try_from(lines).expect("small"),
            text: Rope::from_str(&"b\n".repeat(lines)),
        });
    }
    vcs.send(Request::Echo(7));
    let mut last = 0;
    loop {
        match next(&replies) {
            Reply::Hunks { version, .. } => last = version,
            Reply::Echo(7) => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(last, 49);
}

#[test]
fn no_git_is_not_an_error_and_there_are_simply_no_marks() {
    if !has_git() {
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("loose.txt");
    write(&path, "x\n");
    assert!(Repo::discover(&path).is_none());

    let (vcs, replies) = vcs();
    assert_eq!(open(&vcs, &replies, path, "y\n"), None);
    vcs.send(Request::Status(dir.path().to_path_buf()));
    match next(&replies) {
        Reply::Status { status, .. } => assert_eq!(status, Ok(None)),
        other => panic!("expected status, got {other:?}"),
    }
}

#[test]
fn an_untracked_file_has_no_marks() {
    if !has_git() {
        return;
    }
    let dir = repo_with("a.txt", "a\n");
    let path = dir.path().join("new.txt");
    write(&path, "n\n");
    let (vcs, replies) = vcs();
    assert_eq!(open(&vcs, &replies, path, "n\n"), None);
}

#[test]
fn a_linked_worktree_diffs_against_its_own_index() {
    if !has_git() {
        return;
    }
    let dir = repo_with("a.txt", "main\n");
    let linked = dir.path().join("linked");
    git(dir.path(), &["worktree", "add", "-q", "-b", "side", linked.to_str().expect("utf-8")]);
    write(&linked.join("a.txt"), "side\n");
    git(&linked, &["commit", "-qam", "side"]);

    let repo = Repo::discover(&linked.join("a.txt")).expect("the worktree");
    assert_eq!(repo.workdir().canonicalize().ok(), linked.canonicalize().ok());
    assert_eq!(staged_text(&repo, "a.txt"), "side\n");

    let (vcs, replies) = vcs();
    assert_eq!(open(&vcs, &replies, linked.join("a.txt"), "side\n"), Some(vec![]));
}

#[test]
fn a_file_in_a_submodule_belongs_to_the_submodule() {
    if !has_git() {
        return;
    }
    let inner = repo_with("inner.txt", "inner\n");
    let outer = repo_with("outer.txt", "outer\n");
    git(outer.path(), &["submodule", "add", "-q", inner.path().to_str().expect("utf-8"), "sub"]);
    git(outer.path(), &["commit", "-qm", "sub"]);

    let path = outer.path().join("sub/inner.txt");
    let repo = Repo::discover(&path).expect("the submodule");
    assert_eq!(repo.relative(&path).map(|rel| rel.to_string()), Some("inner.txt".into()));
    assert_eq!(staged_text(&repo, "inner.txt"), "inner\n");

    // The superproject opened first must not claim the submodule's file.
    let (vcs, replies) = vcs();
    open(&vcs, &replies, outer.path().join("outer.txt"), "outer\n");
    vcs.send(Request::Open { id: 2, version: 1, path, text: Rope::from_str("inner\nmore\n") });
    let Reply::Hunks { id: 2, diff: Some(diff), .. } = next(&replies) else { panic!("hunks") };
    assert_eq!(diff.mark(1), Some(LineMark::Added));

    // A submodule checked out at another commit is a modified path.
    let sub = outer.path().join("sub");
    write(&sub.join("inner.txt"), "moved on\n");
    git(&sub, &["commit", "-qam", "moved"]);
    let status = Repo::discover(outer.path()).expect("outer").status().expect("status");
    assert_eq!(status.of(&sub), Some(FileStatus::Modified));
}

#[test]
fn a_detached_head_is_a_commit_like_any_other() {
    if !has_git() {
        return;
    }
    let dir = repo_with("a.txt", "first\n");
    write(&dir.path().join("a.txt"), "second\n");
    git(dir.path(), &["commit", "-qam", "second"]);
    git(dir.path(), &["checkout", "-q", "--detach", "HEAD~1"]);

    let repo = Repo::discover(dir.path()).expect("repo");
    let head = repo.head_blob("a.txt".into()).expect("reads").expect("a blob");
    assert_eq!(head.data, b"first\n");
    assert!(repo.status().expect("status").is_clean());

    let (vcs, replies) = vcs();
    let marks = open(&vcs, &replies, dir.path().join("a.txt"), "first\nmore\n").expect("a diff");
    assert_eq!(marks, vec![(1, LineMark::Added)]);
}

#[test]
fn a_crlf_file_untouched_has_no_hunks() {
    if !has_git() {
        return;
    }
    let dir = repo_with("w.txt", "one\r\ntwo\r\n");
    let (vcs, replies) = vcs();
    // The buffer holds `\n`, as nun-core loads it.
    assert_eq!(open(&vcs, &replies, dir.path().join("w.txt"), "one\ntwo\n"), Some(vec![]));
}

#[test]
fn staging_a_hunk_writes_the_index_and_leaves_the_file_alone() {
    if !has_git() {
        return;
    }
    let dir = repo_with("a.txt", "a\r\nb\r\nc\r\n");
    let path = dir.path().join("a.txt");
    let on_disk = "A\r\nb\r\nC\r\n";
    write(&path, on_disk);
    let (vcs, replies) = vcs();
    vcs.send(Request::Open {
        id: 1,
        version: 1,
        path: path.clone(),
        text: Rope::from_str("A\nb\nC\n"),
    });
    let Reply::Hunks { diff: Some(diff), .. } = next(&replies) else { panic!("hunks") };
    assert_eq!(diff.hunks().len(), 2);

    vcs.send(Request::Stage { id: 1, version: 1, hunk: diff.hunks()[0].clone() });
    assert_eq!(next(&replies), Reply::Staged { id: 1, result: Ok(()) });
    let Reply::Hunks { diff: Some(after), .. } = next(&replies) else { panic!("fresh hunks") };
    assert_eq!(after.hunks().len(), 1);

    assert_eq!(std::fs::read_to_string(&path).expect("read"), on_disk);
    let repo = Repo::discover(&path).expect("repo");
    assert_eq!(staged_text(&repo, "a.txt"), "A\r\nb\r\nc\r\n");
    // Git agrees about what is staged and what is not.
    assert_eq!(git(dir.path(), &["diff", "--cached", "--numstat"]).trim(), "1\t1\ta.txt");
    assert_eq!(git(dir.path(), &["diff", "--numstat"]).trim(), "1\t1\ta.txt");

    // A hunk from a version the text has moved on from is refused.
    vcs.send(Request::Update { id: 1, version: 2, text: Rope::from_str("A\nb\nC\nd\n") });
    next(&replies);
    vcs.send(Request::Stage { id: 1, version: 1, hunk: after.hunks()[0].clone() });
    assert!(matches!(next(&replies), Reply::Staged { result: Err(_), .. }));
}

#[test]
fn compare_can_diff_against_head_instead_of_the_index() {
    if !has_git() {
        return;
    }
    let dir = repo_with("a.txt", "a\n");
    write(&dir.path().join("a.txt"), "b\n");
    git(dir.path(), &["add", "a.txt"]);
    let (vcs, replies) = vcs();
    // Against the index, nothing; against HEAD, one line.
    assert_eq!(open(&vcs, &replies, dir.path().join("a.txt"), "b\n"), Some(vec![]));
    vcs.send(Request::Compare { id: 1, serial: 9, against: Against::Head });
    let Reply::Compared { serial: 9, diff: Some(diff), .. } = next(&replies) else {
        panic!("compared")
    };
    assert_eq!(diff.mark(0), Some(LineMark::Modified));
}

#[test]
fn a_refresh_after_a_commit_elsewhere_clears_the_marks() {
    if !has_git() {
        return;
    }
    let dir = repo_with("a.txt", "a\n");
    let path = dir.path().join("a.txt");
    let (vcs, replies) = vcs();
    open(&vcs, &replies, path.clone(), "b\n").expect("a diff");
    write(&path, "b\n");
    git(dir.path(), &["commit", "-qam", "b"]);
    vcs.send(Request::Refresh);
    let Reply::Hunks { diff: Some(diff), .. } = next(&replies) else { panic!("hunks") };
    assert!(diff.is_empty());
}

#[test]
fn status_reports_each_kind_of_change() {
    if !has_git() {
        return;
    }
    let dir = repo_with("keep.txt", "k\n");
    write(&dir.path().join("gone.txt"), "g\n");
    write(&dir.path().join("edit.txt"), "e\n");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "more"]);
    std::fs::remove_file(dir.path().join("gone.txt")).expect("rm");
    write(&dir.path().join("edit.txt"), "E\n");
    write(&dir.path().join("dir/new.txt"), "n\n");
    write(&dir.path().join("ignored/big.bin"), "x\n");
    write(&dir.path().join(".gitignore"), "ignored/\n");

    let (vcs, replies) = vcs();
    vcs.send(Request::Status(dir.path().join("dir")));
    let Reply::Status { status: Ok(Some(status)), .. } = next(&replies) else { panic!("status") };
    let root = dir.path();
    assert_eq!(status.of(&root.join("edit.txt")), Some(FileStatus::Modified));
    assert_eq!(status.of(&root.join("gone.txt")), Some(FileStatus::Deleted));
    assert_eq!(status.of(&root.join("dir/new.txt")), Some(FileStatus::Added));
    assert_eq!(status.of(&root.join("dir")), Some(FileStatus::Added));
    assert_eq!(status.of(&root.join("keep.txt")), None);
    assert_eq!(status.of(&root.join("ignored/big.bin")), None);
}

#[test]
fn a_slow_status_does_not_hold_up_hunks() {
    if !has_git() {
        return;
    }
    // Enough untracked files that walking them is real work.
    let dir = repo_with("a.txt", "a\n");
    for index in 0..3000 {
        write(&dir.path().join(format!("many/{}/{index}.txt", index % 30)), "x\n");
    }
    let (vcs, replies) = vcs();
    let asked = Instant::now();
    vcs.send(Request::Status(dir.path().to_path_buf()));
    // Sending never waits on the work.
    assert!(asked.elapsed() < Duration::from_millis(50));
    vcs.send(Request::Open {
        id: 1,
        version: 1,
        path: dir.path().join("a.txt"),
        text: Rope::from_str("b\n"),
    });
    let mut seen_hunks = false;
    for _ in 0..2 {
        match next(&replies) {
            Reply::Hunks { .. } => seen_hunks = true,
            Reply::Status { status, .. } => {
                let status = status.expect("status").expect("a repo");
                assert_eq!(status.files().count(), 3000);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(seen_hunks);
}

/// What a decoy repository holds that a stray command would change.
fn fingerprint(decoy: &Path) -> (String, Vec<u8>, Vec<u8>) {
    let git_dir = decoy.join(".git");
    (
        git(decoy, &["log", "--all", "--format=%H %s"]),
        std::fs::read(git_dir.join("config")).expect("config"),
        std::fs::read(git_dir.join("index")).expect("index"),
    )
}

#[test]
fn git_variables_in_the_environment_redirect_neither_the_tests_nor_the_library() {
    if !has_git() {
        return;
    }
    // Run from a git hook, or with nun as git's editor, the process inherits
    // `GIT_DIR`, `GIT_INDEX_FILE` and friends describing some other
    // repository. Setting them here would be unsafe with other tests running,
    // so this test runs again, alone, in a process that has them, all
    // pointing at a decoy that must come out untouched.
    if let Some(decoy) = std::env::var_os("NUN_VCS_DECOY") {
        let decoy = PathBuf::from(decoy);
        let dir = repo_with("a.txt", "a\n");
        let repo =
            Repo::discover(&dir.path().join("a.txt")).expect("the repository the file is in");
        assert_eq!(repo.workdir().canonicalize().ok(), dir.path().canonicalize().ok());
        assert_eq!(staged_text(&repo, "a.txt"), "a\n");
        write(&dir.path().join("a.txt"), "b\n");
        let status = repo.status().expect("status");
        assert_eq!(status.of(&dir.path().join("a.txt")), Some(FileStatus::Modified));
        assert_eq!(status.of(&decoy.join("decoy.txt")), None);
        return;
    }
    let decoy = repo_with("decoy.txt", "decoy\n");
    let git_dir = decoy.path().join(".git");
    let before = fingerprint(decoy.path());
    let ran = Command::new(std::env::current_exe().expect("the test binary"))
        .args(["git_variables_in_the_environment_redirect_neither_the_tests_nor_the_library"])
        .arg("--exact")
        .env("NUN_VCS_DECOY", decoy.path())
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", decoy.path())
        .env("GIT_INDEX_FILE", git_dir.join("index"))
        .env("GIT_OBJECT_DIRECTORY", git_dir.join("objects"))
        .env("GIT_COMMON_DIR", &git_dir)
        .output()
        .expect("the test runs");
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stdout));
    assert_eq!(fingerprint(decoy.path()), before, "the decoy was changed");
}
