//! A loader registered by an app reaches the same pipeline the built-in
//! ones do: the registry routes an extension it has never heard of to the
//! new loader, the decode runs on the worker pool, and the payload lands on
//! the waiting entity.

use bevy_ecs::prelude::*;
use bevy_ecs::system::RunSystemOnce;
use lumen_assets::{
    AssetKind, AssetLoader, AssetServer, ImageSource, LoadContext, LoadErrorKind, LoadedAsset,
    LoadedSvg, SvgData, drain_completed_decodes, spawn_pending_decodes,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Claims `.blip`, a format this crate knows nothing about: a four-byte
/// magic followed by a body the loader turns into a vector payload of its
/// own choosing. The fixed intrinsic size is the tell that this loader ran
/// rather than a built-in one.
struct BlipLoader;

/// Intrinsic size every `.blip` decodes to, chosen so it cannot be mistaken
/// for a size any real file would report.
const BLIP_INTRINSIC: glam::Vec2 = glam::Vec2::new(16.0, 16.0);

impl AssetLoader for BlipLoader {
    fn extensions(&self) -> &[&str] {
        &["blip"]
    }

    fn kind(&self) -> AssetKind {
        AssetKind::Svg
    }

    fn load(&self, ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind> {
        let bytes = ctx.read_bytes()?;
        if !bytes.starts_with(b"BLIP") {
            let path = ctx.path();
            return Err(LoadErrorKind::DecodeFailed(format!("{path:?}: not a blip")));
        }
        let data = SvgData {
            intrinsic: BLIP_INTRINSIC,
            scene: vello::Scene::new(),
            // The body past the magic is what this format costs; the header
            // is not part of the payload.
            source_bytes: bytes.len() - 4,
        };
        Ok(LoadedAsset::Svg(LoadedSvg(data.into())))
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-assets-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn custom_loader_serves_an_extension_the_crate_does_not_know() {
    let dir = temp_dir("custom-loader");
    let path = dir.join("tone.blip");
    std::fs::write(&path, b"BLIPpayload").expect("write asset");

    let mut world = World::new();
    let mut server = AssetServer::default();
    server.register_loader(BlipLoader);
    assert_eq!(
        server.loaders().kind_for(&path),
        Some(AssetKind::Svg),
        "the registry routes .blip to the registered loader"
    );
    world.insert_resource(server);

    let entity = world.spawn(ImageSource(path.clone())).id();
    world
        .run_system_once(spawn_pending_decodes)
        .expect("enqueue the load");

    let deadline = Instant::now() + Duration::from_secs(10);
    let loaded = loop {
        world
            .run_system_once(drain_completed_decodes)
            .expect("drain completed loads");
        if let Some(svg) = world.get::<LoadedSvg>(entity) {
            break (svg.intrinsic, svg.source_bytes);
        }
        assert!(
            Instant::now() < deadline,
            "the custom loader never produced a payload"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        loaded,
        (BLIP_INTRINSIC, b"payload".len()),
        "the loader's own payload comes through"
    );

    // Second entity on the same path is served from the cache, so the
    // custom payload is cached like a built-in one.
    let second = world.spawn(ImageSource(path.clone())).id();
    world
        .run_system_once(spawn_pending_decodes)
        .expect("serve from cache");
    assert!(
        world.get::<LoadedSvg>(second).is_some(),
        "a cached custom asset is attached synchronously"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_registered_loader_replaces_a_built_in_extension() {
    let mut server = AssetServer::default();
    assert_eq!(
        server.loaders().kind_for(Path::new("photo.png")),
        Some(AssetKind::Image)
    );

    struct PngAsSvg;
    impl AssetLoader for PngAsSvg {
        fn extensions(&self) -> &[&str] {
            &["png"]
        }
        fn kind(&self) -> AssetKind {
            AssetKind::Svg
        }
        fn load(&self, _ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind> {
            Err(LoadErrorKind::Unsupported)
        }
    }

    server.register_loader(PngAsSvg);
    assert_eq!(
        server.loaders().kind_for(Path::new("photo.png")),
        Some(AssetKind::Svg),
        "the later registration wins the extension"
    );
    assert_eq!(
        server.loaders().kind_for(Path::new("icon.svg")),
        Some(AssetKind::Svg),
        "an extension the new loader did not name keeps its own loader"
    );
    assert_eq!(
        server.loaders().kind_for(Path::new("mystery.dat")),
        Some(AssetKind::Image),
        "and the fallback that catches unclaimed extensions is untouched"
    );
}
