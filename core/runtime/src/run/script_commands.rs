use super::*;

/// Apply the script commands whose effect is on the app's own state: an
/// asset path resolved against the app dir, the cascade's color scheme, and
/// the clipboard images.
///
/// The commands whose whole effect is on the scene belong to
/// [`lumen_scene::script_commands::apply_scene_script_commands`], which the
/// browser and the server register too. Clipboard text is
/// [`lumen_script::clipboard::apply_clipboard_commands`]'s, which the script
/// plugin installs on every platform. The ones an optional subsystem answers
/// (a hotkey, a tray icon, a dialog, a notification) are read by that
/// subsystem's own applier, installed with it. Every
/// applier reads the same stream through a cursor of its own and ignores
/// what the others own.
pub(crate) fn apply_script_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut commands: Commands,
    ids: Query<(Entity, &LumenId)>,
    mut style_manager: ResMut<lumen_core::components::StyleManager>,
    mut assets: Option<ResMut<lumen_assets::AssetServer>>,
) {
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::SetSrc { target_id, path } => {
                // Asset paths from script (`set_src`) get the same
                // dir-relative resolution as parser-time paths, so authors
                // can write `set_src("hero-icon", "icons/sun.png")`
                // regardless of cwd.
                let resolved = lumen_assets::resolve_source_path(path);
                for (e, id) in &ids {
                    if id.0 == *target_id {
                        // A decode still in flight for the old source
                        // carries the entity's current request id; moving
                        // the id on is what makes the drain drop that
                        // result instead of attaching the old picture to
                        // the new source.
                        if let Some(server) = assets.as_mut() {
                            server.bump_request_id(e);
                        }
                        let mut ent = commands.entity(e);
                        // Strip stale results so the asset pipeline
                        // re-decodes from scratch. Enqueued is the
                        // marker that prevents duplicate decode jobs;
                        // dropping it forces a fresh enqueue next tick.
                        //
                        // `try_`, because the query answers for the world as
                        // this system runs and nothing orders it against the
                        // `<if>` reconciler, which despawns a whole page's
                        // tree in the same stage. One handler calling
                        // `set_src` and then `page()` queues these writes
                        // against elements the swap is about to take away,
                        // and a plain `insert` fails the whole command
                        // buffer rather than just itself.
                        ent.try_remove::<lumen_assets::LoadedImage>();
                        ent.try_remove::<lumen_assets::LoadedSvg>();
                        ent.try_remove::<lumen_assets::ImageLoadFailed>();
                        ent.try_remove::<lumen_assets::Enqueued>();
                        ent.try_insert(lumen_assets::ImageSource(resolved.clone()));
                    }
                }
            }
            ScriptCommand::SetColorScheme { name } => {
                match lumen_core::components::ColorScheme::from_name(name) {
                    Some(scheme) => style_manager.set_scheme(scheme),
                    None => tracing::warn!(
                        "set_color_scheme: unknown name {name:?}; expected one of \
                         \"default\"/\"force-light\"/\"force-dark\"/\
                         \"prefer-light\"/\"prefer-dark\""
                    ),
                }
            }
            ScriptCommand::CopyImageToClipboard { path } => copy_image_to_clipboard(path),
            ScriptCommand::SaveClipboardImage { path } => save_clipboard_image(path),
            _ => {}
        }
    }
}

/// Decode the PNG at `path` (app-relative when relative) to RGBA8 and copy
/// it to the system clipboard. Errors log to stderr; the backend is not
/// always available (headless CI, for one).
fn copy_image_to_clipboard(path: &str) {
    let resolved = lumen_core::app_paths::resolve(path);
    let img = match image::open(&resolved) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            eprintln!("lumenc: copy_image '{}': {e}", resolved.display());
            return;
        }
    };
    let (w, h) = (img.width(), img.height());
    let rgba = img.into_raw();
    let Some(clip) = ClipboardHost::try_new() else {
        eprintln!("lumenc: copy_image: no clipboard backend");
        return;
    };
    if !clip.set_rgba8_image(w, h, rgba) {
        eprintln!("lumenc: copy_image: clipboard backend rejected image");
    }
}

/// Pull the current clipboard image, when there is one, and write it as
/// PNG to `path` (app-relative when relative).
fn save_clipboard_image(path: &str) {
    let Some(clip) = ClipboardHost::try_new() else {
        eprintln!("lumenc: save_clipboard_image: no clipboard backend");
        return;
    };
    let Some((w, h, rgba)) = clip.get_rgba8_image() else {
        eprintln!("lumenc: save_clipboard_image: clipboard has no image");
        return;
    };
    let resolved = lumen_core::app_paths::resolve(path);
    let Some(img) = image::RgbaImage::from_raw(w, h, rgba) else {
        eprintln!("lumenc: save_clipboard_image: bad rgba dims {w}x{h}");
        return;
    };
    if let Err(e) = img.save(&resolved) {
        eprintln!("lumenc: save_clipboard_image '{}': {e}", resolved.display());
    }
}

#[cfg(test)]
mod tests {
    use lumen_assets::resolve_source_path;
    use std::path::{Path, PathBuf};

    /// The `set_src` resolution rule, mirroring the audio module's: the app
    /// dir comes from the published process-global cache (never the process
    /// cwd, so packaged and headless runs resolve like dev runs), and a
    /// `lumen://app/...` URI passes through verbatim instead of being
    /// mangled by an app-dir join.
    #[test]
    fn set_src_paths_resolve_like_every_asset_path() {
        let dir = std::env::temp_dir().join(format!("lumen-set-src-{}", std::process::id()));
        lumen_core::app_paths::set_app(&dir, "lumen-set-src-test");

        assert_eq!(
            resolve_source_path("icons/sun.png"),
            dir.join("icons/sun.png")
        );
        let absolute = dir.join("elsewhere.png");
        assert_eq!(
            resolve_source_path(absolute.to_str().expect("utf8 path")),
            absolute
        );
        assert_eq!(
            resolve_source_path("lumen://app/icons/sun.png"),
            PathBuf::from("lumen://app/icons/sun.png"),
            "a bundle URI must reach the source chain unresolved"
        );
        assert!(!resolve_source_path("lumen://app/x").starts_with(Path::new(&dir)));
    }

    /// A load the test releases by hand, so a decode can be held in flight
    /// while the source changes under it. Delegates to the built-in image
    /// loader once released.
    struct GatedLoader {
        gates: std::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::mpsc::Receiver<()>>>,
    }

    impl lumen_assets::AssetLoader for GatedLoader {
        fn extensions(&self) -> &[&str] {
            &["png"]
        }

        fn kind(&self) -> lumen_assets::AssetKind {
            lumen_assets::AssetKind::Image
        }

        fn load(
            &self,
            ctx: &lumen_assets::LoadContext<'_>,
        ) -> Result<lumen_assets::LoadedAsset, lumen_assets::LoadErrorKind> {
            let gate = self
                .gates
                .lock()
                .expect("gate map")
                .remove(ctx.path())
                .expect("every test path has a gate");
            gate.recv_timeout(std::time::Duration::from_secs(30))
                .expect("the test releases the gate");
            lumen_assets::AssetLoader::load(&lumen_assets::ImageLoader, ctx)
        }
    }

    /// Runs the drain until `done` holds, failing after a generous bound
    /// rather than hanging when the result never arrives.
    fn drain_until(
        world: &mut bevy_ecs::world::World,
        done: impl Fn(&bevy_ecs::world::World) -> bool,
    ) {
        use bevy_ecs::system::RunSystemOnce;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !done(world) {
            assert!(std::time::Instant::now() < deadline, "decode never landed");
            world
                .run_system_once(lumen_assets::drain_completed_decodes)
                .expect("drain runs");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// `set_src` while the old source is still decoding: the old decode
    /// finishes first, and it must not land on the element that moved on.
    /// A second element still showing the old source gets it, which is what
    /// proves the old result was drained and dropped for the first one
    /// rather than not having arrived yet.
    #[test]
    fn set_src_drops_a_decode_still_in_flight_for_the_old_source() {
        use bevy_ecs::message::Messages;
        use bevy_ecs::system::RunSystemOnce;
        use bevy_ecs::world::World;
        use lumen_assets::{AssetServer, ImageSource, LoadedImage};
        use lumen_core::components::{LumenId, StyleManager};
        use lumen_script::{ScriptCommand, ScriptCommandEvent};

        let dir = std::env::temp_dir().join(format!("lumen-set-src-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let old = dir.join("old.png");
        let new = dir.join("new.png");
        image::RgbaImage::new(1, 1)
            .save(&old)
            .expect("write old.png");
        image::RgbaImage::new(2, 2)
            .save(&new)
            .expect("write new.png");

        let (release_old, old_gate) = std::sync::mpsc::channel();
        let (release_new, new_gate) = std::sync::mpsc::channel();
        let mut server = AssetServer::default();
        server.register_loader(GatedLoader {
            gates: std::sync::Mutex::new(
                [(old.clone(), old_gate), (new.clone(), new_gate)]
                    .into_iter()
                    .collect(),
            ),
        });

        let mut world = World::new();
        world.insert_resource(server);
        world.init_resource::<StyleManager>();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        let hero = world
            .spawn((LumenId("hero".into()), ImageSource(old.clone())))
            .id();
        let sentinel = world.spawn(ImageSource(old.clone())).id();
        world
            .run_system_once(lumen_assets::spawn_pending_decodes)
            .expect("enqueue the old source");

        world.write_message(ScriptCommandEvent(ScriptCommand::SetSrc {
            target_id: "hero".into(),
            path: new.to_str().expect("utf8 path").into(),
        }));
        world
            .run_system_once(super::apply_script_commands)
            .expect("apply set_src");
        world
            .run_system_once(lumen_assets::spawn_pending_decodes)
            .expect("enqueue the new source");

        release_old.send(()).expect("release the old decode");
        drain_until(&mut world, |w| w.get::<LoadedImage>(sentinel).is_some());
        assert!(
            world.get::<LoadedImage>(hero).is_none(),
            "the old source's decode landed on the element set_src moved on"
        );

        release_new.send(()).expect("release the new decode");
        drain_until(&mut world, |w| w.get::<LoadedImage>(hero).is_some());
        assert_eq!(
            world.get::<LoadedImage>(hero).expect("new image").width,
            2,
            "the element shows the source set_src named"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
