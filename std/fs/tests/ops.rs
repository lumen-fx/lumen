//! The operations themselves, against directories on disk.
//!
//! These are the rules a script author runs into: what a refusal looks like,
//! where the byte cap falls, and what a listing gives back. The plugin adds
//! path resolution and the warn-and-degrade wrapper on top; both are covered
//! from the app's side in `plugin.rs` and `module.rs`.

use std::path::{Path, PathBuf};

use lumen_fs::ops;

/// A fresh empty directory of this test's own.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-fs-ops-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The names directly inside `dir`, sorted, read without going through the
/// operation under test.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read the scratch dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A listing gives back the entry names, sorted, one level deep.
#[test]
fn a_listing_carries_sorted_names_rather_than_paths() {
    let dir = scratch("list");
    std::fs::write(dir.join("beta.txt"), "b").expect("file");
    std::fs::write(dir.join("alpha.txt"), "a").expect("file");
    std::fs::create_dir(dir.join("nested")).expect("dir");
    std::fs::write(dir.join("nested/deep.txt"), "d").expect("file");

    let names = ops::list(&dir).expect("the directory lists");
    assert_eq!(names, vec!["alpha.txt", "beta.txt", "nested"]);
    assert!(
        !names.iter().any(|n| n.contains('/')),
        "names, not paths: {names:?}"
    );

    let missing = ops::list(&dir.join("no-such-dir"));
    assert!(
        missing.is_err_and(|m| m.contains("no-such-dir")),
        "a missing directory explains itself"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Creating a directory that is already there is success, not a refusal.
#[test]
fn making_a_directory_that_exists_is_success() {
    let dir = scratch("mkdir");
    let nested = dir.join("a/b/c");

    assert_eq!(ops::mkdir(&nested), Ok(true));
    assert!(nested.is_dir());
    assert_eq!(ops::mkdir(&nested), Ok(true), "the second call agrees");
    assert!(ops::is_dir(&nested));
    assert!(ops::exists(&nested));
    assert!(!ops::is_dir(&dir.join("a/b/c/nothing")));

    // A path a directory cannot be made at reports rather than raises.
    std::fs::write(dir.join("file"), "x").expect("file");
    assert!(ops::mkdir(&dir.join("file/under-a-file")).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Removal takes a file or an empty directory. A directory holding anything
/// is refused by name, and a path that is not there is a quiet false.
#[test]
fn removal_never_recurses() {
    let dir = scratch("remove");
    let full = dir.join("full");
    std::fs::create_dir(&full).expect("dir");
    std::fs::write(full.join("keep.txt"), "kept").expect("file");

    let refusal = ops::remove(&full).expect_err("a directory holding a file is refused");
    assert!(
        refusal.contains("full"),
        "the refusal names the path: {refusal}"
    );
    assert!(
        full.join("keep.txt").exists(),
        "the refusal left the contents alone"
    );

    assert_eq!(ops::remove(&full.join("keep.txt")), Ok(true));
    assert_eq!(ops::remove(&full), Ok(true), "the emptied directory goes");
    assert_eq!(
        ops::remove(&dir.join("never-existed")),
        Ok(false),
        "probing for something absent is not a fault"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Copying takes one file, creates the directories under the destination,
/// and refuses a directory source by name.
#[test]
fn copying_takes_one_file_and_refuses_a_directory() {
    let dir = scratch("copy");
    std::fs::write(dir.join("source.txt"), "contents").expect("file");
    let dest = dir.join("into/a/new/place.txt");

    assert_eq!(ops::copy(&dir.join("source.txt"), &dest), Ok(true));
    assert_eq!(
        std::fs::read_to_string(&dest).ok(),
        Some("contents".to_string())
    );

    std::fs::create_dir(dir.join("tree")).expect("dir");
    let refusal = ops::copy(&dir.join("tree"), &dir.join("elsewhere"))
        .expect_err("a directory source is refused");
    assert!(refusal.contains("tree"), "{refusal}");
    assert!(!dir.join("elsewhere").exists());

    let missing = ops::copy(&dir.join("no-such-file"), &dir.join("out.txt"));
    assert!(missing.is_err_and(|m| m.contains("no-such-file")));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A missing file reads as the empty string with nothing to report; a file
/// that is there but cannot be read as text does report.
#[test]
fn reading_text_treats_a_missing_file_as_empty() {
    let dir = scratch("read");
    assert_eq!(
        ops::read(&dir.join("not-saved-yet.json")),
        Ok(String::new())
    );

    std::fs::write(dir.join("saved.txt"), "hello").expect("file");
    assert_eq!(ops::read(&dir.join("saved.txt")), Ok("hello".to_string()));

    let refusal = ops::read(&dir).expect_err("a directory is not text");
    assert!(refusal.starts_with("read("), "{refusal}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A write goes through a temp file and a rename, so a second write over the
/// same path leaves the finished file and nothing beside it.
#[test]
fn a_write_leaves_only_the_finished_file_behind() {
    let dir = scratch("write");
    let target = dir.join("out.txt");

    assert_eq!(ops::write(&target, "first"), Ok(true));
    assert_eq!(ops::write(&target, "second, longer contents"), Ok(true));

    assert_eq!(
        std::fs::read_to_string(&target).ok(),
        Some("second, longer contents".to_string())
    );
    assert_eq!(
        entries(&dir),
        vec!["out.txt"],
        "no temp file was left behind"
    );

    assert!(
        ops::write(&dir.join("no/such/directory/out.txt"), "x").is_err(),
        "a write with nowhere to land reports"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The cap is inclusive: a file exactly its size reads, and one byte more is
/// refused with the size and the cap in the line.
#[test]
fn the_byte_cap_admits_a_file_of_exactly_its_size() {
    let dir = scratch("cap");
    let at_cap = dir.join("at-cap.bin");
    let over_cap = dir.join("over-cap.bin");
    std::fs::write(&at_cap, vec![7u8; 64]).expect("file");
    std::fs::write(&over_cap, vec![7u8; 65]).expect("file");

    assert_eq!(ops::read_bytes(&at_cap, 64), Ok(vec![7u8; 64]));

    let refusal = ops::read_bytes(&over_cap, 64).expect_err("one byte over is refused");
    assert!(refusal.contains("over-cap.bin"), "{refusal}");
    assert!(refusal.contains("65"), "the size is in the line: {refusal}");
    assert!(refusal.contains("64"), "the cap is in the line: {refusal}");

    assert_eq!(
        ops::read_bytes(&dir.join("absent.bin"), 64),
        Ok(Vec::new()),
        "a missing file reads as no bytes"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every element has to be a byte. One that is not refuses the whole write
/// and says which position it was.
#[test]
fn a_value_that_is_not_a_byte_refuses_the_whole_write() {
    let dir = scratch("write-bytes");
    let target = dir.join("bytes.bin");

    assert_eq!(ops::write_bytes(&target, &[0, 127, 255]), Ok(true));
    assert_eq!(std::fs::read(&target).ok(), Some(vec![0u8, 127, 255]));

    let refusal =
        ops::write_bytes(&target, &[1, 2, 256, 4]).expect_err("a value above 255 is not a byte");
    assert!(refusal.contains("element 2"), "{refusal}");
    assert!(refusal.contains("256"), "{refusal}");
    assert_eq!(
        std::fs::read(&target).ok(),
        Some(vec![0u8, 127, 255]),
        "the refused write left the old contents in place"
    );

    let refusal = ops::write_bytes(&target, &[-1]).expect_err("a negative value is not a byte");
    assert!(refusal.contains("element 0"), "{refusal}");

    assert_eq!(ops::write_bytes(&target, &[]), Ok(true));
    assert_eq!(std::fs::read(&target).ok(), Some(Vec::new()));
    assert_eq!(entries(&dir), vec!["bytes.bin"], "no temp file is left");

    let _ = std::fs::remove_dir_all(&dir);
}
