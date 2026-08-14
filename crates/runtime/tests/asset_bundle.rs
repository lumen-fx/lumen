//! Headless proof that an app can read its assets out of a `.lpak` archive.
//!
//! `lumenc bundle` has always written the archive; nothing read one back.
//! `RunOptions::assets` closes that: the archive is registered on the asset
//! server keyed to the app directory, so an `<image src>` the markup already
//! resolves against that directory resolves out of the archive instead of the
//! filesystem.

use lumen_assets::{ImageLoadFailed, ImageSource, LoadedImage, LumenBundle};
use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

/// A 1x1 red PNG. Small enough to inline, real enough for the decoder.
const RED_DOT_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen_lpak_{name}_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    dir
}

/// An app whose single `<image>` points at `<dir>/icons/dot.png`, the path
/// the compiler bakes into an artifact for `src="icons/dot.png"`.
fn image_app(dir: &std::path::Path) -> Vec<u8> {
    let image = Element {
        tag: "image".to_string(),
        attrs: Attributes {
            id: Some("dot".to_string()),
            src: Some(dir.join("icons/dot.png").to_string_lossy().into_owned()),
            ..Default::default()
        },
        children: Vec::new(),
        interpolations: Vec::new(),
    };
    let ir = LayoutIR {
        root: Element {
            tag: "root".to_string(),
            attrs: Attributes::default(),
            children: vec![image],
            interpolations: Vec::new(),
        },
        ..Default::default()
    };
    artifact::serialize(&CompiledApp {
        ir,
        script_source: String::new(),
        ..Default::default()
    })
    .unwrap()
}

/// Tick until the image entity carries a decode result, or give up. Decoding
/// runs on a worker thread, so the result lands some ticks after the enqueue.
fn tick_until_decoded(app: &mut lumen_core::app::App) -> Option<bevy_ecs::entity::Entity> {
    for _ in 0..200 {
        app.tick();
        let mut q = app
            .world
            .query_filtered::<bevy_ecs::entity::Entity, bevy_ecs::query::With<ImageSource>>();
        let entities: Vec<_> = q.iter(&app.world).collect();
        for e in entities {
            if app.world.get::<LoadedImage>(e).is_some()
                || app.world.get::<ImageLoadFailed>(e).is_some()
            {
                return Some(e);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}

/// The whole point: pack the app, delete the loose asset, and the app still
/// finds its image. Without the archive registered this run fails to decode.
#[test]
fn assets_resolve_from_a_bundle_when_the_file_is_gone() {
    let dir = scratch_dir("hit");
    std::fs::create_dir_all(dir.join("icons")).unwrap();
    std::fs::write(dir.join("icons/dot.png"), RED_DOT_PNG).unwrap();
    let bytes = image_app(&dir);

    let lpak = dir.with_extension("lpak");
    LumenBundle::pack_dir(&dir, &lpak).expect("pack the app directory");
    std::fs::remove_file(dir.join("icons/dot.png")).unwrap();

    let mut opts = RunOptions::new(&dir)
        .with_artifact_bytes(bytes)
        .with_assets(&lpak);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");

    let entity = tick_until_decoded(&mut app).expect("the image never resolved");
    assert!(
        app.world.get::<ImageLoadFailed>(entity).is_none(),
        "the archive entry must decode: {:?}",
        app.world.get::<ImageLoadFailed>(entity).map(|f| &f.detail)
    );
    let loaded = app.world.get::<LoadedImage>(entity).expect("LoadedImage");
    assert_eq!((loaded.width, loaded.height), (1, 1));

    let _ = std::fs::remove_file(&lpak);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same app with the archive left out fails to find the deleted file, so
/// the test above is proving the archive and not a stale cache.
#[test]
fn assets_do_not_resolve_without_the_bundle() {
    let dir = scratch_dir("miss");
    std::fs::create_dir_all(dir.join("icons")).unwrap();
    let bytes = image_app(&dir);

    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");

    let entity = tick_until_decoded(&mut app).expect("the image never reported an outcome");
    assert!(
        app.world.get::<ImageLoadFailed>(entity).is_some(),
        "with no archive and no file there is nothing to decode"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A named archive that cannot be read stops the run rather than silently
/// falling back to the app directory.
#[test]
fn an_unreadable_bundle_fails_the_run() {
    let dir = scratch_dir("bad");
    let lpak = dir.join("broken.lpak");
    std::fs::write(&lpak, b"not an archive").unwrap();
    let bytes = image_app(&dir);

    let mut opts = RunOptions::new(&dir)
        .with_artifact_bytes(bytes)
        .with_assets(&lpak);
    opts.bounded = true;
    let err = build_headless_app(opts).err().expect("build must fail");
    assert!(
        matches!(err, lumen_runtime::RunError::Assets(..)),
        "expected an asset-bundle error, got {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
