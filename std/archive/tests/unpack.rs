//! The unpacker on its own: what names a format, what an archive is allowed
//! to write, and what a broken one reports.
//!
//! Every archive here is written by the crate's own fixture writers at the
//! moment the test runs, hostile ones included, so nothing that could escape
//! a destination is ever checked into the repository.

use std::path::{Path, PathBuf};

use lumen_archive::testkit;
use lumen_archive::unpack::{self, Format};

/// One of the fixture writers: it takes the path to save the archive at.
type FixtureWriter = fn(&Path) -> std::io::Result<()>;

/// A fresh directory of this test's own, emptied first so a rerun starts
/// clean.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen-archive-unpack-{}-{case}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// The leading bytes of a file, as much of them as [`unpack::detect`] reads.
fn head(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("read the archive");
    let take = usize::try_from(unpack::MAGIC_LEN).expect("the magic length fits a usize");
    bytes.into_iter().take(take).collect()
}

/// What one destination holds, as sorted relative paths.
fn tree(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap_or(&path);
                found.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    found.sort();
    found
}

/// The magic bytes decide, so an archive saved under a name that disagrees
/// with its contents still unpacks as what it is. The extension answers only
/// when the bytes say nothing.
#[test]
fn the_bytes_name_the_format_before_the_extension() {
    let dir = scratch("detect");
    let zip_named_tar = dir.join("mislabelled.tar");
    let targz_named_zip = dir.join("mislabelled.zip");
    let tar_named_zip = dir.join("plain.zip");
    testkit::normal_zip(&zip_named_tar).expect("zip fixture");
    testkit::normal_tar_gz(&targz_named_zip).expect("tar.gz fixture");
    testkit::normal_tar(&tar_named_zip).expect("tar fixture");

    assert_eq!(
        unpack::detect(&head(&zip_named_tar), &zip_named_tar),
        Some(Format::Zip)
    );
    assert_eq!(
        unpack::detect(&head(&targz_named_zip), &targz_named_zip),
        Some(Format::TarGz)
    );
    assert_eq!(
        unpack::detect(&head(&tar_named_zip), &tar_named_zip),
        Some(Format::Tar)
    );

    // Nothing recognisable in the bytes: the name is all there is to go on.
    for (name, expected) in [
        ("empty.zip", Some(Format::Zip)),
        ("empty.tar.gz", Some(Format::TarGz)),
        ("empty.tgz", Some(Format::TarGz)),
        ("empty.tar", Some(Format::Tar)),
        ("EMPTY.TAR.GZ", Some(Format::TarGz)),
        ("notes.txt", None),
        ("no-extension", None),
    ] {
        assert_eq!(
            unpack::detect(b"not an archive", Path::new(name)),
            expected,
            "{name}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A zip, a tar.gz, and a plain tar carrying the same three files all unpack
/// to the same tree, and the count is the files written.
#[test]
fn every_container_unpacks_to_the_same_tree() {
    let dir = scratch("formats");
    let cases: [(&str, FixtureWriter); 3] = [
        ("bundle.zip", testkit::normal_zip),
        ("bundle.tar.gz", testkit::normal_tar_gz),
        ("bundle.tar", testkit::normal_tar),
    ];
    for (name, write) in cases {
        let src = dir.join(name);
        let dest = dir.join(format!("out-{name}"));
        write(&src).expect("fixture");

        let unpacked = unpack::extract(&src, &dest).expect("the archive unpacks");
        assert_eq!(unpacked.files, testkit::MEMBERS.len(), "{name}");
        assert_eq!(unpacked.links_skipped, 0, "{name}");
        assert_eq!(
            tree(&dest),
            vec![
                "nested/deep/leaf.txt".to_string(),
                "nested/other.txt".to_string(),
                "top.txt".to_string(),
            ],
            "{name}"
        );
        for (member, body) in testkit::MEMBERS {
            assert_eq!(
                std::fs::read_to_string(dest.join(member)).ok(),
                Some(body.to_string()),
                "{name}: {member}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// An entry climbing out with `..` ends the extraction, names itself in the
/// error, and writes nothing outside the destination.
#[test]
fn an_entry_that_climbs_out_stops_the_extraction() {
    let dir = scratch("escape");
    let src = dir.join("hostile.zip");
    let dest = dir.join("out");
    testkit::escaping_zip(&src).expect("fixture");

    let error = unpack::extract(&src, &dest).expect_err("the extraction is refused");
    assert!(
        error.contains(testkit::ESCAPING_ENTRY),
        "the error names the entry: {error}"
    );
    assert!(
        !dir.join("escape.txt").exists(),
        "nothing was written beside the destination"
    );
    assert_eq!(
        tree(&dest),
        vec!["top.txt".to_string()],
        "the entries before the refused one are all that landed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An entry spelling an absolute path ends the extraction the same way, and
/// the path it named is never created.
#[test]
fn an_absolute_entry_stops_the_extraction() {
    let dir = scratch("absolute");
    let src = dir.join("hostile.tar.gz");
    let dest = dir.join("out");
    testkit::absolute_tar_gz(&src).expect("fixture");
    let _ = std::fs::remove_file(testkit::ABSOLUTE_ENTRY);

    let error = unpack::extract(&src, &dest).expect_err("the extraction is refused");
    assert!(
        error.contains(testkit::ABSOLUTE_ENTRY),
        "the error names the entry: {error}"
    );
    assert!(
        !Path::new(testkit::ABSOLUTE_ENTRY).exists(),
        "the absolute path the entry named was not written"
    );
    assert!(tree(&dest).is_empty(), "nothing landed in the destination");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A symbolic link is counted and passed over; the files around it still
/// land, and the count of files written leaves it out.
#[cfg(unix)]
#[test]
fn a_link_entry_is_skipped_rather_than_written() {
    let dir = scratch("symlink");
    let src = dir.join("linked.zip");
    let dest = dir.join("out");
    testkit::symlink_zip(&src).expect("fixture");

    let unpacked = unpack::extract(&src, &dest).expect("the archive unpacks");
    assert_eq!(unpacked.files, 1);
    assert_eq!(unpacked.links_skipped, 1);
    assert_eq!(tree(&dest), vec!["real.txt".to_string()]);
    assert!(
        !dest.join(testkit::LINK_ENTRY).exists(),
        "the link was not created in any form"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Unpacking over a destination that already holds those files replaces them.
#[test]
fn an_existing_file_is_overwritten() {
    let dir = scratch("overwrite");
    let src = dir.join("bundle.zip");
    let dest = dir.join("out");
    testkit::normal_zip(&src).expect("fixture");
    std::fs::create_dir_all(dest.join("nested/deep")).expect("destination");
    std::fs::write(
        dest.join("top.txt"),
        "stale contents, longer than the new one",
    )
    .expect("stale file");

    let unpacked = unpack::extract(&src, &dest).expect("the archive unpacks");
    assert_eq!(unpacked.files, testkit::MEMBERS.len());
    assert_eq!(
        std::fs::read_to_string(dest.join("top.txt")).ok(),
        Some("top".to_string()),
        "the file was replaced, not appended to"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An archive that stops in the middle reports the failure rather than
/// pretending it finished.
#[test]
fn a_truncated_archive_reports_an_error() {
    let dir = scratch("truncated");
    let src = dir.join("half.zip");
    let dest = dir.join("out");
    testkit::truncated_zip(&src).expect("fixture");

    let error = unpack::extract(&src, &dest).expect_err("a half archive cannot unpack");
    assert!(
        error.contains("cannot read the archive"),
        "the error says the archive is unreadable: {error}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A file that is no archive at all is refused before anything is created.
#[test]
fn a_file_that_is_no_archive_is_refused() {
    let dir = scratch("unknown");
    let src = dir.join("notes.txt");
    let dest = dir.join("out");
    std::fs::write(&src, "just some text").expect("file");

    let error = unpack::extract(&src, &dest).expect_err("the file is refused");
    assert!(
        error.contains("not a zip, tar, or tar.gz archive"),
        "the error names what was expected: {error}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An archive that is not there reports it, rather than answering as if it
/// had unpacked nothing.
#[test]
fn a_missing_archive_reports_the_open_failure() {
    let dir = scratch("missing");
    let error = unpack::extract(&dir.join("never-downloaded.zip"), &dir.join("out"))
        .expect_err("a missing archive cannot unpack");
    assert!(error.contains("cannot open"), "{error}");

    let _ = std::fs::remove_dir_all(&dir);
}
