//! A loader registered by an app reaches the same pipeline the built-in
//! ones do: the registry routes an extension it has never heard of to the
//! new loader, the decode runs on the worker pool, and the payload lands on
//! the waiting entity.

use bevy_ecs::prelude::*;
use bevy_ecs::system::RunSystemOnce;
use lumen_assets::{
    AssetKind, AssetLoader, AssetServer, AudioData, AudioSource, LoadContext, LoadErrorKind,
    LoadedAsset, LoadedAudio, drain_completed_decodes, spawn_pending_audio_decodes,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Claims `.blip`, a container this crate knows nothing about, and carries
/// its bytes through as an audio track after checking a four-byte header.
struct BlipLoader;

impl AssetLoader for BlipLoader {
    fn extensions(&self) -> &[&str] {
        &["blip"]
    }

    fn kind(&self) -> AssetKind {
        AssetKind::Audio
    }

    fn load(&self, ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind> {
        let bytes = ctx.read_bytes()?;
        if !bytes.starts_with(b"BLIP") {
            let path = ctx.path();
            return Err(LoadErrorKind::DecodeFailed(format!("{path:?}: not a blip")));
        }
        let bytes: Arc<[u8]> = Arc::from(&bytes[4..]);
        Ok(LoadedAsset::Audio(LoadedAudio(AudioData { bytes }.into())))
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
        Some(AssetKind::Audio),
        "the registry routes .blip to the registered loader"
    );
    world.insert_resource(server);

    let entity = world.spawn(AudioSource(path.clone())).id();
    world
        .run_system_once(spawn_pending_audio_decodes)
        .expect("enqueue the load");

    let deadline = Instant::now() + Duration::from_secs(10);
    let loaded = loop {
        world
            .run_system_once(drain_completed_decodes)
            .expect("drain completed loads");
        if let Some(audio) = world.get::<LoadedAudio>(entity) {
            break audio.bytes.to_vec();
        }
        assert!(
            Instant::now() < deadline,
            "the custom loader never produced a payload"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(loaded, b"payload", "the loader's own bytes come through");

    // Second entity on the same path is served from the cache, so the
    // custom payload is cached like a built-in one.
    let second = world.spawn(AudioSource(path.clone())).id();
    world
        .run_system_once(spawn_pending_audio_decodes)
        .expect("serve from cache");
    assert!(
        world.get::<LoadedAudio>(second).is_some(),
        "a cached custom asset is attached synchronously"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_registered_loader_replaces_a_built_in_extension() {
    let mut server = AssetServer::default();
    assert_eq!(
        server.loaders().kind_for(Path::new("icon.svg")),
        Some(AssetKind::Svg)
    );

    struct SvgAsAudio;
    impl AssetLoader for SvgAsAudio {
        fn extensions(&self) -> &[&str] {
            &["svg"]
        }
        fn kind(&self) -> AssetKind {
            AssetKind::Audio
        }
        fn load(&self, _ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind> {
            Err(LoadErrorKind::Unsupported)
        }
    }

    server.register_loader(SvgAsAudio);
    assert_eq!(
        server.loaders().kind_for(Path::new("icon.svg")),
        Some(AssetKind::Audio),
        "the later registration wins the extension"
    );
    assert_eq!(
        server.loaders().kind_for(Path::new("photo.png")),
        Some(AssetKind::Image),
        "other extensions are untouched"
    );
}
