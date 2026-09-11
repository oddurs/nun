//! Saving touches the filesystem, so it gets its own file.

use std::fs;

use nun_core::{Buffer, SaveError};

#[test]
fn a_clean_file_round_trips_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.txt");
    let original: &[u8] = b"one\r\ntwo\r\nthree";
    fs::write(&path, original).unwrap();

    let (mut buffer, report) = Buffer::load(&path).unwrap();
    assert!(!report.lossy);
    buffer.save().unwrap();

    assert_eq!(fs::read(&path).unwrap(), original, "an untouched file is written back unchanged");
}

#[test]
fn a_bom_survives_a_load_edit_save_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bom.txt");
    fs::write(&path, "\u{feff}hello".as_bytes()).unwrap();

    let (mut buffer, _) = Buffer::load(&path).unwrap();
    buffer.select_all();
    buffer.insert("goodbye");
    buffer.save().unwrap();

    assert_eq!(fs::read(&path).unwrap(), "\u{feff}goodbye".as_bytes());
}

#[test]
fn saving_refuses_to_clobber_a_file_that_changed_underneath() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("racy.txt");
    fs::write(&path, b"original").unwrap();

    let (mut buffer, _) = Buffer::load(&path).unwrap();
    buffer.insert("mine");

    // Someone else writes to it while the buffer is open.
    fs::write(&path, b"theirs, much longer than the original").unwrap();

    match buffer.save() {
        Err(SaveError::ChangedOnDisk { .. }) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), b"theirs, much longer than the original");
}

#[test]
fn saving_again_after_a_refusal_is_possible_once_the_buffer_owns_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retry.txt");
    fs::write(&path, b"original").unwrap();

    let (mut buffer, _) = Buffer::load(&path).unwrap();
    fs::write(&path, b"changed").unwrap();
    assert!(buffer.save().is_err());

    // Re-reading adopts the new stamp, so the next save goes through.
    let (mut buffer, _) = Buffer::load(&path).unwrap();
    buffer.select_all();
    buffer.insert("ours");
    buffer.save().unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"ours");
}

#[test]
fn a_buffer_with_no_path_says_so_rather_than_guessing() {
    let mut buffer = Buffer::from_text("floating");
    assert!(matches!(buffer.save(), Err(SaveError::NoPath)));
}

#[test]
fn saving_writes_through_a_symlink_rather_than_replacing_it() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.txt");
    let link = dir.path().join("link.txt");
    fs::write(&real, b"before").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let (mut buffer, _) = Buffer::load(&link).unwrap();
    buffer.select_all();
    buffer.insert("after");
    buffer.save().unwrap();

    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link survives");
    assert_eq!(fs::read(&real).unwrap(), b"after", "and the target was written");
}

#[test]
fn saving_keeps_the_original_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mode.txt");
    fs::write(&path, b"x").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

    let (mut buffer, _) = Buffer::load(&path).unwrap();
    buffer.insert("y");
    buffer.save().unwrap();

    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640, "a fresh temp file would have taken the umask instead");
}

#[test]
fn an_interrupted_save_leaves_no_stray_temporary_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tidy.txt");
    fs::write(&path, b"x").unwrap();

    let (mut buffer, _) = Buffer::load(&path).unwrap();
    buffer.insert("y");
    buffer.save().unwrap();

    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
}

#[test]
fn saving_clears_the_modified_flag() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dirty.txt");
    fs::write(&path, b"x").unwrap();

    let (mut buffer, _) = Buffer::load(&path).unwrap();
    buffer.insert("y");
    assert!(buffer.is_modified());
    buffer.save().unwrap();
    assert!(!buffer.is_modified());
}
