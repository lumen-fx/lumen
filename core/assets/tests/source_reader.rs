//! The `SourceReader` seam: raw bytes through the source chain - bundles,
//! then registered sources, then the filesystem - from any thread, with no
//! decoder or cache involved.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lumen_assets::{AssetServer, AssetSource, LumenBundle, SourceReader};

fn temp_path(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "lumen-source-reader-{tag}-{}-{seq}",
        std::process::id()
    ))
}

/// A test source answering from a fixed path -> bytes map.
struct MapSource(HashMap<PathBuf, Vec<u8>>);

impl MapSource {
    fn one(path: impl Into<PathBuf>, bytes: &[u8]) -> Self {
        Self(HashMap::from([(path.into(), bytes.to_vec())]))
    }
}

impl AssetSource for MapSource {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        self.0.get(path).cloned()
    }
}

/// An app dir with one packed asset, returned as (dir, opened bundle).
fn packed_dir() -> (PathBuf, LumenBundle) {
    let dir = temp_path("app");
    std::fs::create_dir_all(dir.join("tracks")).unwrap();
    std::fs::write(dir.join("tracks").join("tone.bin"), b"bundled-bytes").unwrap();
    let lpak = temp_path("out.lpak");
    LumenBundle::pack_dir(&dir, &lpak).expect("pack");
    let bundle = LumenBundle::open(&lpak).expect("open");
    let _ = std::fs::remove_file(&lpak);
    (dir, bundle)
}

#[test]
fn the_reader_serves_bundle_uris_and_rooted_paths() {
    let (dir, bundle) = packed_dir();
    let mut server = AssetServer::default();
    server.register_bundle(bundle);
    server.set_bundle_root(&dir);
    // The loose file changes after packing: the bundled entry must still
    // win, which is what lets an app ship the archive alone.
    std::fs::write(dir.join("tracks").join("tone.bin"), b"disk-bytes").unwrap();

    let reader = server.source_reader();
    let via_uri = reader
        .read(Path::new("lumen://app/tracks/tone.bin"))
        .expect("uri resolves");
    assert_eq!(via_uri, b"bundled-bytes");
    let via_path = reader
        .read(&dir.join("tracks").join("tone.bin"))
        .expect("rooted path resolves");
    assert_eq!(via_path, b"bundled-bytes");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn registered_sources_answer_in_order_after_the_bundles() {
    let (dir, bundle) = packed_dir();
    let key = dir.join("tracks").join("tone.bin");
    let mut server = AssetServer::default();
    server.register_bundle(bundle);
    server.set_bundle_root(&dir);
    // Both extra sources also claim the bundled key, plus one of their own.
    server.register_source(MapSource(HashMap::from([
        (key.clone(), b"first-source".to_vec()),
        (PathBuf::from("shared"), b"from-first".to_vec()),
    ])));
    server.register_source(MapSource(HashMap::from([
        (PathBuf::from("shared"), b"from-second".to_vec()),
        (PathBuf::from("only-second"), b"second-owns".to_vec()),
    ])));

    let reader = server.source_reader();
    // Bundles answer before any registered source.
    assert_eq!(reader.read(&key).expect("bundle wins"), b"bundled-bytes");
    // Registration order decides between sources claiming one path.
    assert_eq!(
        reader.read(Path::new("shared")).expect("first source wins"),
        b"from-first"
    );
    // A later source still answers for what only it holds.
    assert_eq!(
        reader
            .read(Path::new("only-second"))
            .expect("second source"),
        b"second-owns"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_filesystem_answers_when_no_source_claims() {
    let file = temp_path("loose.bin");
    std::fs::write(&file, b"loose-bytes").unwrap();
    let server = AssetServer::default();

    let reader = server.source_reader();
    assert_eq!(reader.read(&file).expect("fs fallback"), b"loose-bytes");
    let missing = reader.read(&temp_path("missing.bin"));
    assert_eq!(
        missing.expect_err("nothing holds it").kind(),
        std::io::ErrorKind::NotFound
    );

    let _ = std::fs::remove_file(&file);
}

/// The reader is a snapshot, safe to move to another thread; the documented
/// cost is that a source registered after the snapshot is invisible to it.
#[test]
fn the_reader_is_a_thread_safe_snapshot() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SourceReader>();

    let (dir, bundle) = packed_dir();
    let mut server = AssetServer::default();
    server.register_bundle(bundle);
    let stale = server.source_reader();
    server.register_source(MapSource::one("late", b"late-bytes"));

    let joined = std::thread::spawn(move || {
        let bundled = stale
            .read(Path::new("lumen://app/tracks/tone.bin"))
            .expect("bundled entry reads off-thread");
        let late = stale.read(Path::new("late"));
        (bundled, late.is_err())
    })
    .join()
    .expect("reader thread");
    assert_eq!(joined.0, b"bundled-bytes");
    assert!(
        joined.1,
        "a source registered after the snapshot is not seen"
    );

    // A fresh snapshot sees it.
    assert_eq!(
        server
            .source_reader()
            .read(Path::new("late"))
            .expect("fresh snapshot"),
        b"late-bytes"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
