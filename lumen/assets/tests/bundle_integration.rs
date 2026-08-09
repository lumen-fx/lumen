//! W6.8 `.lpak` bundle + `lumen://` URI scheme integration.
//!
//! Builds a bundle, registers it on an `AssetServer`, and verifies
//! that `resolve_uri` returns the bundled bytes.

use lumen_assets::{AssetServer, LumenBundle};
use std::io::Write;
use std::path::PathBuf;

fn tempfile_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    };
    p.push(format!("lumen-assets-bundle-{tag}-{nanos}"));
    p
}

fn write_test_dir() -> (PathBuf, PathBuf) {
    let dir = tempfile_path("src");
    std::fs::create_dir_all(dir.join("icons")).unwrap();
    std::fs::write(dir.join("main.lmn"), b"<root></root>").unwrap();
    // Smallest valid 1x1 PNG (89 bytes). Verified bytes-for-bytes
    // works against image::load_from_memory.
    let png_1x1: [u8; 67] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let mut f = std::fs::File::create(dir.join("icons").join("dot.png")).unwrap();
    f.write_all(&png_1x1).unwrap();
    let out = tempfile_path("out.lpak");
    let count = LumenBundle::pack_dir(&dir, &out).expect("pack");
    assert!(count >= 2);
    (dir, out)
}

#[test]
fn asset_server_resolves_lumen_uri_via_bundle() {
    let (src, lpak) = write_test_dir();
    let bundle = LumenBundle::open(&lpak).expect("open lpak");
    let mut server = AssetServer::default();
    server.register_bundle(bundle);

    // Bundled markup roundtrip.
    let bytes = server
        .resolve_uri("lumen://app/main.lmn")
        .expect("resolve markup");
    assert_eq!(bytes, b"<root></root>");

    // Bundled icon roundtrip.
    let icon = server
        .resolve_uri("lumen://app/icons/dot.png")
        .expect("resolve icon");
    assert_eq!(icon.len(), 67);

    // Non-existent path under the same scheme returns None.
    assert!(server.resolve_uri("lumen://app/missing.png").is_none());

    // Foreign schemes return None.
    assert!(server.resolve_uri("file:///etc/passwd").is_none());
    assert!(server.resolve_uri("icons/dot.png").is_none());

    // Cleanup so the next run starts clean.
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_file(&lpak);
}

#[test]
fn bundle_round_trips_through_open() {
    let (src, lpak) = write_test_dir();
    let bundle = LumenBundle::open(&lpak).expect("open lpak");
    assert!(bundle.contains("main.lmn"));
    assert!(bundle.contains("icons/dot.png"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_file(&lpak);
}
