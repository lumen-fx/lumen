use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_script_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut commands: Commands,
    ids: Query<(Entity, &LumenId)>,
    mut texts: Query<&mut TextContent>,
    mut inputs: Query<&mut lumen_core::components::TextInput>,
    mut store: ResMut<lumen_core::property_store::PropertyStore>,
    mut array_signals: ResMut<lumen_core::signals::ArraySignals>,
    mut style_manager: ResMut<lumen_core::components::StyleManager>,
    mut picked: MessageWriter<lumen_core::input::FilePicked>,
    mut hotkeys: Option<NonSendMut<OsHotkeyRegistry>>,
    file_dialog: Res<FileDialogService>,
    notifier: Res<NotificationService>,
    mut tray: NonSendMut<OsTrayService>,
    hot: Option<Res<HotReloadState>>,
    // Async file-dialog fast path (Part B tree-shaking): these are only read
    // when an embedder installed `lumen-async-tokio`'s resources. Compiled out
    // of a build without the `async` feature; a trimmed bundle then always
    // takes the blocking `file_dialog.open(..)` path below.
    #[cfg(feature = "async")] tokio_rt: Option<Res<lumen_async_tokio::TokioRuntime>>,
    #[cfg(feature = "async")] async_queue: Option<Res<lumen_async_tokio::AsyncCommandQueue>>,
) {
    // Asset paths from script (`set_src`) get the same dir-relative
    // resolution as parser-time paths, so authors can write
    // `set_src("hero-icon", "icons/sun.png")` regardless of cwd.
    let dir: PathBuf = hot
        .as_ref()
        .map(|h| h.dir.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::Print(s) => eprintln!("[script] {s}"),
            ScriptCommand::SetText { target_id, text } => {
                for (e, id) in &ids {
                    if id.0 == *target_id {
                        if let Ok(mut tc) = texts.get_mut(e) {
                            tc.0 = text.clone();
                        }
                        // Replacing text from script must clamp the
                        // caret, or it points past the new buffer end
                        // and next keypress crashes the insert_str.
                        if let Ok(mut input) = inputs.get_mut(e) {
                            input.cursor = text.len();
                        }
                    }
                }
            }
            ScriptCommand::SetSrc { target_id, path } => {
                let p = Path::new(path);
                let resolved = if p.is_relative() {
                    dir.join(p)
                } else {
                    p.to_path_buf()
                };
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
            ScriptCommand::SetSignal { name, value } => {
                store.set_global_str(name, value.as_str());
            }
            ScriptCommand::RegisterHotkey { name, accelerator } => {
                if let Some(reg) = hotkeys.as_mut() {
                    reg.register(name, accelerator);
                }
            }
            ScriptCommand::UnregisterHotkey { name } => {
                if let Some(reg) = hotkeys.as_mut() {
                    reg.unregister(name);
                }
            }
            ScriptCommand::SetClasses { target_id, classes } => {
                let new_classes: Vec<String> =
                    classes.split_whitespace().map(|s| s.to_string()).collect();
                if target_id == "<root>" {
                    if let Some(state) = &hot {
                        commands
                            .entity(state.root)
                            .insert(lumen_core::components::LumenClasses::from(new_classes));
                    }
                } else {
                    for (e, id) in &ids {
                        if id.0 == *target_id {
                            commands
                                .entity(e)
                                .insert(lumen_core::components::LumenClasses::from(
                                    new_classes.clone(),
                                ));
                        }
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
            ScriptCommand::SetArray { name, items } => {
                array_signals.set(name, items.clone());
            }
            ScriptCommand::Notify { title, body } => {
                // Fire-and-forget - the notifier returns once the
                // daemon accepts the spec; the actual popup lives on
                // the OS side. Errors log through the service so a
                // missing libnotify (CI / headless) doesn't kill the
                // app. Now lives in lumen-os-notify (W6.5).
                notifier.send_simple(title, body);
            }
            ScriptCommand::OpenFileDialog {
                kind,
                tag,
                filters,
                default_name,
            } => {
                let os_kind = match kind {
                    lumen_script::FileDialogKind::Open => FileDialogKind::Open,
                    lumen_script::FileDialogKind::OpenMulti => FileDialogKind::OpenMulti,
                    lumen_script::FileDialogKind::Save => FileDialogKind::Save,
                    lumen_script::FileDialogKind::PickFolder => FileDialogKind::PickFolder,
                };
                let req = FileDialogRequest {
                    kind: os_kind,
                    tag: tag.clone(),
                    filters: filters
                        .iter()
                        .map(|(label, exts)| (label.clone(), exts.clone()).into())
                        .collect(),
                    default_name: default_name.clone(),
                };
                // Prefer the async path when TokioRuntime + AsyncCommandQueue
                // are available (W6.4): rfd::AsyncFileDialog::pick_file()
                // runs on the shared tokio runtime; the result lands as a
                // FileDialogResultCommand -> FilePicked via the typed-command
                // drain registered by FileDialogPlugin. Falls back to the
                // pollster::block_on path when those resources are missing
                // (e.g. headless / no-runtime embedders). The async path is
                // compiled in only with the `async` feature (Part B).
                #[cfg(feature = "async")]
                if let (Some(rt), Some(queue)) = (&tokio_rt, &async_queue) {
                    file_dialog.open_single_with(rt, queue, req);
                } else {
                    file_dialog.open(&req, &mut picked);
                }
                #[cfg(not(feature = "async"))]
                file_dialog.open(&req, &mut picked);
            }
            ScriptCommand::CopyImageToClipboard { path } => {
                handle_copy_image_to_clipboard(path, &dir);
            }
            ScriptCommand::SaveClipboardImage { path } => {
                handle_save_clipboard_image(path, &dir);
            }
            ScriptCommand::RegisterTrayIcon {
                id,
                icon_path,
                tooltip,
            } => {
                let cfg = OsTrayConfig {
                    id: id.clone(),
                    icon_path: PathBuf::from(icon_path),
                    tooltip: tooltip.clone(),
                    menu: None,
                    template: false,
                };
                tray.register(&cfg, &dir);
            }
            ScriptCommand::UnregisterTrayIcon { id } => {
                tray.unregister(id);
            }
            ScriptCommand::SetProperty { key, value } => {
                // Typed PropertyStore write deferred to the next tick.
                // Hosts that want the immediate cross-thread path use
                // `lumen_core::property_store::push_external_property`
                // directly (the Rhai typed-builtin path). This branch
                // exists for hosts that prefer a tick-coalesced apply.
                lumen_core::property_store::push_external_property(key.clone(), value.clone());
            }
            ScriptCommand::AddClicks(_)
            | ScriptCommand::SetString { .. }
            | ScriptCommand::SetTimer { .. }
            | ScriptCommand::CancelTimer { .. }
            | ScriptCommand::Fetch { .. }
            // `Http` is handled off-thread by script-runtime's
            // `drain_fetch_commands` (same seam as `Fetch`); no-op here.
            | ScriptCommand::Http { .. }
            // Audio transport is applied by `apply_audio_commands` against
            // the `AudioService` + `AssetServer`; no-op here.
            | ScriptCommand::AudioPlay { .. }
            | ScriptCommand::AudioPause
            | ScriptCommand::AudioResume
            | ScriptCommand::AudioStop
            | ScriptCommand::AudioSeek { .. }
            | ScriptCommand::AudioVolume { .. }
            // Dynamic DOM mutation + window setters are applied by
            // `dom_commands::apply_dom_commands` (an exclusive system that
            // needs `&mut World` for spawn / despawn / reparent); no-op here.
            | ScriptCommand::SetAttr { .. }
            | ScriptCommand::RemoveAttr { .. }
            | ScriptCommand::SetNodeText { .. }
            | ScriptCommand::ClassAdd { .. }
            | ScriptCommand::ClassRemove { .. }
            | ScriptCommand::ClassToggle { .. }
            | ScriptCommand::SetStyleProp { .. }
            | ScriptCommand::RemoveStyleProp { .. }
            | ScriptCommand::Spawn { .. }
            | ScriptCommand::Insert { .. }
            | ScriptCommand::ReplaceWith { .. }
            | ScriptCommand::RemoveNode { .. }
            | ScriptCommand::CloneNode { .. }
            | ScriptCommand::SetInnerMarkup { .. }
            | ScriptCommand::BindEvent { .. }
            | ScriptCommand::UnbindEvent { .. }
            | ScriptCommand::WindowSetTitle { .. }
            | ScriptCommand::WindowSetSize { .. } => {}
        }
    }
}
// File dialog wiring moved to lumen-os-filedialog (W6.4). Tray
// register / unregister / poll moved to lumen-os-tray (W6.5). The
// runtime installs `FileDialogService` / `NotificationService` as
// resources and `TrayService` as a non-send resource.

/// Decodes the PNG at `path` (resolved relative to the app dir when relative) to RGBA8 and copies it to the system clipboard.
/// Errors log to stderr; the clipboard backend is not always available (e.g. headless CI).
fn handle_copy_image_to_clipboard(path: &str, dir: &Path) {
    let p = Path::new(path);
    let resolved = if p.is_relative() {
        dir.join(p)
    } else {
        p.to_path_buf()
    };
    let img = match image::open(&resolved) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            eprintln!("lumenc: copy_image '{}': {e}", resolved.display());
            return;
        }
    };
    let (w, h) = (img.width(), img.height());
    let rgba = img.into_raw();
    let Some(clip) = lumen_os_clipboard::ClipboardHost::try_new() else {
        eprintln!("lumenc: copy_image: no clipboard backend");
        return;
    };
    if !clip.set_rgba8_image(w, h, rgba) {
        eprintln!("lumenc: copy_image: clipboard backend rejected image");
    }
}

/// Pulls the current clipboard image (when present) and writes it as PNG to `path` (resolved relative to the app dir when relative).
fn handle_save_clipboard_image(path: &str, dir: &Path) {
    let Some(clip) = lumen_os_clipboard::ClipboardHost::try_new() else {
        eprintln!("lumenc: save_clipboard_image: no clipboard backend");
        return;
    };
    let Some((w, h, rgba)) = clip.get_rgba8_image() else {
        eprintln!("lumenc: save_clipboard_image: clipboard has no image");
        return;
    };
    let p = Path::new(path);
    let resolved = if p.is_relative() {
        dir.join(p)
    } else {
        p.to_path_buf()
    };
    let Some(img) = image::RgbaImage::from_raw(w, h, rgba) else {
        eprintln!("lumenc: save_clipboard_image: bad rgba dims {w}x{h}");
        return;
    };
    if let Err(e) = img.save(&resolved) {
        eprintln!("lumenc: save_clipboard_image '{}': {e}", resolved.display());
    }
}

// run_file_dialog moved to lumen-os-filedialog (W6.4) - see
// `FileDialogService::open`. The runtime requests it via
// `Res<FileDialogService>` and translates `lumen_script::
// FileDialogKind` -> `lumen_os_filedialog::FileDialogKind` at the
// `ScriptCommand::OpenFileDialog` call site.
