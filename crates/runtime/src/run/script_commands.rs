use super::*;

/// Apply the script commands whose effect is on the app's own state: an
/// asset path resolved against the app dir, the cascade's color scheme, the
/// clipboard the core's text fields already own.
///
/// The commands whose whole effect is on the scene belong to
/// [`lumen_scene::script_commands::apply_scene_script_commands`], which the
/// browser and the server register too, and the ones an optional subsystem
/// answers (a hotkey, a tray icon, a dialog, a notification, the clipboard)
/// are read by that subsystem's own applier, installed with it. Every
/// applier reads the same stream through a cursor of its own and ignores
/// what the others own.
pub(crate) fn apply_script_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut commands: Commands,
    ids: Query<(Entity, &LumenId)>,
    mut style_manager: ResMut<lumen_core::components::StyleManager>,
    clipboard: Option<NonSend<ClipboardHost>>,
    mut clipboard_out: MessageWriter<lumen_core::input::ClipboardRead>,
) {
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::SetSrc { target_id, path } => {
                // Asset paths from script (`set_src`) get the same
                // dir-relative resolution as parser-time paths, so authors
                // can write `set_src("hero-icon", "icons/sun.png")`
                // regardless of cwd.
                let resolved = resolve_asset_src(path);
                for (e, id) in &ids {
                    if id.0 == *target_id {
                        let mut ent = commands.entity(e);
                        // Strip stale results so the asset pipeline
                        // re-decodes from scratch. Enqueued is the
                        // marker that prevents duplicate decode jobs;
                        // dropping it forces a fresh enqueue next tick.
                        ent.remove::<lumen_assets::LoadedImage>();
                        ent.remove::<lumen_assets::LoadedSvg>();
                        ent.remove::<lumen_assets::ImageLoadFailed>();
                        ent.remove::<lumen_assets::Enqueued>();
                        ent.insert(lumen_assets::ImageSource(resolved.clone()));
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
            ScriptCommand::ClipboardWrite { text } => match clipboard.as_ref() {
                Some(clip) => {
                    if !clip.write_text(text) {
                        eprintln!("lumenc: clipboard_write: backend rejected the text");
                    }
                }
                None => eprintln!("lumenc: clipboard_write: no clipboard backend"),
            },
            ScriptCommand::ClipboardRead { tag } => {
                // Answer every request, even with no backend, so a script
                // waiting on `on_clipboard(tag, text)` is never left hanging.
                let text = clipboard
                    .as_ref()
                    .map(|clip| clip.read_text())
                    .unwrap_or_default();
                clipboard_out.write(lumen_core::input::ClipboardRead {
                    tag: tag.clone(),
                    text,
                });
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

/// Resolve a `set_src` path the way every asset path resolves: a
/// `lumen://app/...` URI passes through verbatim (the bundle source claims
/// the scheme itself; joining it against the app dir would mangle it), and
/// everything else resolves app-relative. The app directory comes from the
/// published process-global cache ([`lumen_core::app_paths`]), which the
/// runtime fills for every run; reading the hot-reload state here would fall
/// back to the process cwd in packaged, artifact and headless runs, where no
/// watcher exists. The same rule the audio module's `audio_play` applies.
fn resolve_asset_src(path: &str) -> PathBuf {
    if path.starts_with("lumen://") {
        PathBuf::from(path)
    } else {
        lumen_core::app_paths::resolve(path)
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_asset_src;
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
            resolve_asset_src("icons/sun.png"),
            dir.join("icons/sun.png")
        );
        let absolute = dir.join("elsewhere.png");
        assert_eq!(
            resolve_asset_src(absolute.to_str().expect("utf8 path")),
            absolute
        );
        assert_eq!(
            resolve_asset_src("lumen://app/icons/sun.png"),
            PathBuf::from("lumen://app/icons/sun.png"),
            "a bundle URI must reach the source chain unresolved"
        );
        assert!(!resolve_asset_src("lumen://app/x").starts_with(Path::new(&dir)));
    }
}
