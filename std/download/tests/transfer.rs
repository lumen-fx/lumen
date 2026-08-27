//! The transfer itself, against the crate's own loopback server.
//!
//! What these prove, once per concern:
//!
//! - a checksum is read in every spelling the module accepts, and refused in
//!   the ones it does not;
//! - the destination only ever holds a complete, verified file, and the temp
//!   file the bytes landed in is gone whichever way the transfer ended;
//! - a body the server declared no size for reports no total;
//! - `max_bytes` stops a body that is too large, before and during the read.

use std::path::{Path, PathBuf};

use lumen_download::testkit::{BODY, OTHER_BODY, TestServer};
use lumen_download::transfer::{self, Checksum, Limits, Transferred};

/// A fresh directory to download into.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen-download-transfer-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The sha256 of `bytes`, spelled the way a checksum is written.
fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    transfer::hex(Sha256::digest(bytes).as_slice())
}

/// Every `.part-` file left in `dir`.
fn temps(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("listing")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".part-"))
        .collect();
    names.sort();
    names
}

/// One progress report: how much had arrived, and the size the server
/// declared.
type Report = (u64, Option<u64>);

/// What one download answered: its outcome, and every progress report it made.
type Downloaded = (Result<Transferred, String>, Vec<Report>);

/// Download one URL, counting the progress reports it made.
fn fetch(url: &str, dest: &Path, checksum: &Checksum, limits: &Limits) -> Downloaded {
    let mut seen = Vec::new();
    let outcome = transfer::to_file(url, dest, checksum, limits, &mut |received, total| {
        seen.push((received, total));
    });
    (outcome, seen)
}

/// A checksum reads the same written three ways, and every other spelling is
/// refused rather than guessed at.
#[test]
fn a_checksum_is_read_in_the_spellings_the_module_accepts() {
    let hex = digest(BODY);
    let expected = transfer::parse_checksum(&hex).expect("a bare digest is sha256");

    assert_eq!(
        transfer::parse_checksum(&format!("sha256:{hex}")).expect("prefixed"),
        expected,
        "the prefixed spelling names the same digest"
    );
    assert_eq!(
        transfer::parse_checksum(&format!("SHA256:{}", hex.to_uppercase())).expect("uppercase"),
        expected,
        "neither the prefix nor the digits are case sensitive"
    );
    assert_eq!(
        transfer::parse_checksum("   ").expect("blank"),
        Checksum::None,
        "an empty checksum asks for no check"
    );

    for bad in [
        "sha512:0000",
        "md5:d41d8cd98f00b204e9800998ecf8427e",
        &hex[..63],
        &format!("{hex}0"),
        "not a checksum at all",
    ] {
        let err = transfer::parse_checksum(bad).expect_err("refused");
        assert!(
            err.starts_with("unsupported checksum format"),
            "the refusal names the format problem: {err}"
        );
    }
}

/// The whole good path: the bytes arrive, the declared size reaches the
/// progress reports, the checksum verifies, and nothing is left over.
#[test]
fn a_verified_download_lands_at_the_destination() {
    let server = TestServer::start();
    let dir = scratch("verified");
    let dest = dir.join("payload.bin");
    let checksum = transfer::parse_checksum(&format!("sha256:{}", digest(BODY))).expect("parsed");

    let (outcome, seen) = fetch(&server.url("/fixed"), &dest, &checksum, &Limits::default());

    let done = outcome.expect("the transfer completed");
    assert_eq!(done.path, dest);
    assert_eq!(done.received, BODY.len() as u64);
    assert_eq!(done.total, Some(BODY.len() as u64));
    assert_eq!(std::fs::read(&dest).expect("the file"), BODY);
    assert!(
        seen.iter()
            .all(|(_, total)| *total == Some(BODY.len() as u64)),
        "a declared Content-Length reaches every progress report: {seen:?}"
    );
    assert!(temps(&dir).is_empty(), "the temp file was renamed away");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A body the server declared no size for still downloads, and reports no
/// total.
#[test]
fn a_body_with_no_declared_size_reports_no_total() {
    let server = TestServer::start();
    let dir = scratch("nolength");
    let dest = dir.join("payload.bin");

    let (outcome, seen) = fetch(
        &server.url("/nolength"),
        &dest,
        &Checksum::None,
        &Limits::default(),
    );

    let done = outcome.expect("the transfer completed");
    assert_eq!(
        done.total, None,
        "nothing was declared, so nothing is known"
    );
    assert_eq!(done.received, BODY.len() as u64);
    assert_eq!(std::fs::read(&dest).expect("the file"), BODY);
    assert!(
        seen.iter().all(|(_, total)| total.is_none()),
        "no progress report invents a size: {seen:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A mismatching checksum fails the transfer, names both digests, and leaves
/// nothing behind: not the destination, not the temp file.
#[test]
fn a_checksum_mismatch_writes_nothing() {
    let server = TestServer::start();
    let dir = scratch("mismatch");
    let dest = dir.join("payload.bin");
    let checksum = transfer::parse_checksum(&digest(BODY)).expect("parsed");

    let (outcome, _) = fetch(
        &server.url("/mismatch"),
        &dest,
        &checksum,
        &Limits::default(),
    );

    let err = outcome.expect_err("a body that hashes to something else is refused");
    assert!(err.contains("checksum mismatch"), "{err}");
    assert!(err.contains(&digest(BODY)), "the expected digest: {err}");
    assert!(
        err.contains(&digest(OTHER_BODY)),
        "the actual digest: {err}"
    );
    assert!(!dest.exists(), "the destination was never written");
    assert!(temps(&dir).is_empty(), "the temp file was removed");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A failed transfer never replaces what was already at the destination.
#[test]
fn a_failure_leaves_the_previous_file_alone() {
    let server = TestServer::start();
    let dir = scratch("keep");
    let dest = dir.join("payload.bin");
    std::fs::write(&dest, b"the version already there").expect("seed");

    for url in ["/missing", "/mismatch"] {
        let checksum = transfer::parse_checksum(&digest(BODY)).expect("parsed");
        let (outcome, _) = fetch(&server.url(url), &dest, &checksum, &Limits::default());
        assert!(outcome.is_err(), "{url} must fail");
        assert_eq!(
            std::fs::read(&dest).expect("the file"),
            b"the version already there",
            "{url} replaced a file it never downloaded"
        );
        assert!(temps(&dir).is_empty(), "{url} left a temp file");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A reply that is not 2xx is a failed download, not a downloaded error page.
#[test]
fn a_missing_file_is_a_failure_and_not_a_body() {
    let server = TestServer::start();
    let dir = scratch("missing");
    let dest = dir.join("payload.bin");

    let (outcome, _) = fetch(
        &server.url("/missing"),
        &dest,
        &Checksum::None,
        &Limits::default(),
    );

    let err = outcome.expect_err("404 is a failure");
    assert!(err.contains("HTTP 404"), "the status is named: {err}");
    assert!(!dest.exists(), "the 404 page was not saved as the file");
    assert!(temps(&dir).is_empty(), "the temp file was removed");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `max_bytes` stops a body that is too large. The server declaring the size
/// up front and leaving it out are separate paths, so both are exercised.
#[test]
fn max_bytes_stops_a_body_that_is_too_large() {
    let server = TestServer::start();
    let dir = scratch("cap");
    let limits = Limits {
        timeout_ms: None,
        max_bytes: Some(8),
    };

    for url in ["/fixed", "/nolength"] {
        let dest = dir.join("payload.bin");
        let (outcome, _) = fetch(&server.url(url), &dest, &Checksum::None, &limits);
        let err = outcome.expect_err("a body past the cap is refused");
        assert!(err.contains("max_bytes"), "the setting to raise: {err}");
        assert!(!dest.exists(), "{url} wrote a truncated file");
        assert!(temps(&dir).is_empty(), "{url} left a temp file");
    }

    // The same body inside a cap that fits it downloads whole.
    let dest = dir.join("payload.bin");
    let limits = Limits {
        timeout_ms: None,
        max_bytes: Some(BODY.len() as u64),
    };
    let (outcome, _) = fetch(&server.url("/fixed"), &dest, &Checksum::None, &limits);
    assert!(outcome.is_ok(), "a body of exactly the cap downloads");
    assert_eq!(std::fs::read(&dest).expect("the file"), BODY);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A destination under directories that do not exist yet is created on the
/// way, so a script names where it wants the file rather than building the
/// tree first.
#[test]
fn a_destination_under_a_missing_directory_is_created() {
    let server = TestServer::start();
    let dir = scratch("nested");
    let dest = dir.join("cache/images/payload.bin");

    let (outcome, _) = fetch(
        &server.url("/fixed"),
        &dest,
        &Checksum::None,
        &Limits::default(),
    );

    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(std::fs::read(&dest).expect("the file"), BODY);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A slow body reports progress more than once, and the running count only
/// ever grows.
#[test]
fn progress_climbs_while_a_slow_body_arrives() {
    let server = TestServer::start();
    let dir = scratch("drip");
    let dest = dir.join("payload.bin");

    let (outcome, seen) = fetch(
        &server.url("/drip"),
        &dest,
        &Checksum::None,
        &Limits::default(),
    );

    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(
        seen.len() > 1,
        "a body arriving in pieces reports more than once: {seen:?}"
    );
    assert!(
        seen.windows(2).all(|w| w[0].0 < w[1].0),
        "the running count only grows: {seen:?}"
    );
    assert_eq!(seen.last().map(|(n, _)| *n), Some(BODY.len() as u64));

    let _ = std::fs::remove_dir_all(&dir);
}
