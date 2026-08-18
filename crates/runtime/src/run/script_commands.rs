use super::*;

/// Whether writing `new` over `current` changes the class list. Order
/// matters: `set_class` assigns the whole list, so `"a b"` and `"b a"`
/// are different writes.
fn class_list_differs(
    current: Option<&lumen_core::components::LumenClasses>,
    new: &[String],
) -> bool {
    match current {
        Some(c) => {
            c.0.len() != new.len() || c.0.iter().zip(new).any(|(a, b)| a.as_ref() != b.as_str())
        }
        None => !new.is_empty(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_script_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut commands: Commands,
    // `LumenClasses` rides on this query rather than a second one: the
    // applier is already at bevy's system-parameter ceiling.
    ids: Query<(
        Entity,
        &LumenId,
        Option<&lumen_core::components::LumenClasses>,
    )>,
    mut texts: Query<&mut TextContent>,
    mut inputs: Query<&mut lumen_core::components::TextInput>,
    mut store: ResMut<lumen_core::property_store::PropertyStore>,
    mut array_signals: ResMut<lumen_core::signals::ArraySignals>,
    mut style_manager: ResMut<lumen_core::components::StyleManager>,
    mut hotkeys: Option<NonSendMut<OsHotkeyRegistry>>,
    file_dialog: Res<FileDialogService>,
    mut tray: NonSendMut<OsTrayService>,
    hot: Option<Res<HotReloadState>>,
    // The file dialog runs on the app's executor when one is installed; the
    // resource is absent in a build with no async backend, and the dialog
    // then blocks the tick instead.
    spawn: Option<Res<lumen_core::task::SpawnService>>,
    command_queue: Res<lumen_core::command::CommandQueue>,
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
                for (e, id, _) in &ids {
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
                for (e, id, _) in &ids {
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
                    // A non-root element has no equivalent of the root-class
                    // watcher (`reapply_styles_on_root_class_change`), so
                    // rewriting its class list only restyles if this bumps
                    // `StyleVersion` itself. Without the bump the cascade
                    // re-resolver never re-walks the entity, the new class's
                    // rules never land, and a `transition` on the swapped
                    // property never runs - the `Node::set_class` path in
                    // `dom_commands` bumps, and this global form must match it.
                    let mut changed = false;
                    for (e, id, current) in &ids {
                        if id.0 == *target_id {
                            changed |= class_list_differs(current, &new_classes);
                            commands
                                .entity(e)
                                .insert(lumen_core::components::LumenClasses::from(
                                    new_classes.clone(),
                                ));
                        }
                    }
                    if changed {
                        // Queued rather than taken as a `ResMut` param for the
                        // same reason as above, and it lands with the class
                        // write at the next sync point.
                        commands.queue(|world: &mut World| {
                            if let Some(mut v) =
                                world.get_resource_mut::<crate::run::restyle::StyleVersion>()
                            {
                                v.0 = v.0.wrapping_add(1);
                            }
                        });
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
                // `rfd::AsyncFileDialog::pick_file()` runs on the installed
                // executor when there is one, and inline when there is not.
                // Either way the result lands as a FileDialogResultCommand ->
                // FilePicked through the typed-command drain the
                // FileDialogPlugin registers.
                file_dialog.open_single_with(
                    spawn.as_ref().map(|s| s.as_spawn()),
                    &command_queue,
                    req,
                );
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
                menu,
                template,
            } => {
                let cfg = OsTrayConfig {
                    id: id.clone(),
                    icon_path: PathBuf::from(icon_path),
                    tooltip: tooltip.clone(),
                    menu: (!menu.is_empty()).then(|| OsTrayMenu::parse(menu)),
                    template: *template,
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
            // Notifications, clipboard, launcher, and sleep inhibit are
            // applied by `apply_os_script_commands` below, which holds
            // those hosts; no-op here.
            | ScriptCommand::Notify { .. }
            | ScriptCommand::NotifyEx { .. }
            | ScriptCommand::ClipboardWrite { .. }
            | ScriptCommand::ClipboardRead { .. }
            | ScriptCommand::OpenUrl { .. }
            | ScriptCommand::OpenPath { .. }
            | ScriptCommand::RevealPath { .. }
            | ScriptCommand::KeepAwake { .. }
            | ScriptCommand::AllowSleep { .. }
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
            | ScriptCommand::SpawnFragment { .. }
            | ScriptCommand::BindEvent { .. }
            | ScriptCommand::UnbindEvent { .. }
            | ScriptCommand::WindowSetTitle { .. }
            // The response belongs to a render that was asked for over HTTP,
            // which a window is not; `lumen-ssr` applies these.
            | ScriptCommand::SetResponseStatus { .. }
            | ScriptCommand::SetResponseHeader { .. }
            | ScriptCommand::Redirect { .. }
            | ScriptCommand::WindowSetSize { .. } => {}
        }
    }
}

/// Apply the OS-host script commands: notifications, clipboard text,
/// the URL / file launcher, and sleep inhibits.
///
/// A second applier beside [`apply_script_commands`] rather than more
/// arms in it: that system already sits at the system-parameter limit.
/// Both read the same `ScriptCommandEvent` stream through their own
/// cursor, so each sees every command and each ignores what the other
/// owns.
///
/// The clipboard host is absent on a machine whose backend refused, and
/// the two clipboard commands warn rather than fail the tick.
pub(crate) fn apply_os_script_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    notifier: Res<NotificationService>,
    launcher: Res<Launcher>,
    clipboard: Option<NonSend<ClipboardHost>>,
    mut inhibits: NonSendMut<InhibitHolder>,
    mut clipboard_out: MessageWriter<lumen_core::input::ClipboardRead>,
    hot: Option<Res<HotReloadState>>,
) {
    let dir: PathBuf = hot
        .as_ref()
        .map(|h| h.dir.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::Notify { title, body } => {
                // Fire-and-forget: the call returns once the daemon accepts
                // the spec and the popup lives on the OS side. A missing
                // libnotify logs through the service rather than killing
                // the app.
                notifier.send_simple(title, body);
            }
            ScriptCommand::NotifyEx {
                id,
                title,
                body,
                options,
                actions,
            } => {
                let options = lumen_os_notify::parse_options(options);
                notifier.send(&lumen_os_notify::Notification {
                    id: id.clone(),
                    title: title.clone(),
                    body: body.clone(),
                    icon: options.icon,
                    urgency: options.urgency,
                    actions: lumen_os_notify::parse_actions(actions),
                });
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
            ScriptCommand::OpenUrl { url } => {
                report_launch(&launcher.open_url(url), "open_url", url);
            }
            ScriptCommand::OpenPath { path } => {
                let resolved = resolve_app_path(path, &dir);
                report_launch(
                    &launcher.open_path(&resolved),
                    "open_path",
                    &resolved.display().to_string(),
                );
            }
            ScriptCommand::RevealPath { path } => {
                let resolved = resolve_app_path(path, &dir);
                report_launch(
                    &launcher.reveal_in_file_manager(&resolved),
                    "reveal_path",
                    &resolved.display().to_string(),
                );
            }
            ScriptCommand::KeepAwake { name, reason } => {
                inhibits.start(
                    name,
                    reason,
                    lumen_os_power::InhibitKinds::DISPLAY
                        .union(lumen_os_power::InhibitKinds::SUSPEND),
                );
            }
            ScriptCommand::AllowSleep { name } => inhibits.stop(name),
            // Everything else is applied by `apply_script_commands`, by
            // the audio applier, or by the exclusive DOM applier. Listed
            // rather than caught by `_` so a new command has to be placed
            // deliberately instead of silently going nowhere.
            ScriptCommand::Print(_)
            | ScriptCommand::AddClicks(_)
            | ScriptCommand::SetString { .. }
            | ScriptCommand::SetText { .. }
            | ScriptCommand::SetSrc { .. }
            | ScriptCommand::SetTimer { .. }
            | ScriptCommand::CancelTimer { .. }
            | ScriptCommand::Fetch { .. }
            | ScriptCommand::Http { .. }
            | ScriptCommand::SetSignal { .. }
            | ScriptCommand::SetProperty { .. }
            | ScriptCommand::SetArray { .. }
            | ScriptCommand::CopyImageToClipboard { .. }
            | ScriptCommand::SaveClipboardImage { .. }
            | ScriptCommand::RegisterTrayIcon { .. }
            | ScriptCommand::UnregisterTrayIcon { .. }
            | ScriptCommand::SetClasses { .. }
            | ScriptCommand::SetColorScheme { .. }
            | ScriptCommand::OpenFileDialog { .. }
            | ScriptCommand::RegisterHotkey { .. }
            | ScriptCommand::UnregisterHotkey { .. }
            | ScriptCommand::AudioPlay { .. }
            | ScriptCommand::AudioPause
            | ScriptCommand::AudioResume
            | ScriptCommand::AudioStop
            | ScriptCommand::AudioSeek { .. }
            | ScriptCommand::AudioVolume { .. }
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
            | ScriptCommand::SpawnFragment { .. }
            | ScriptCommand::BindEvent { .. }
            | ScriptCommand::UnbindEvent { .. }
            | ScriptCommand::WindowSetTitle { .. }
            | ScriptCommand::SetResponseStatus { .. }
            | ScriptCommand::SetResponseHeader { .. }
            | ScriptCommand::Redirect { .. }
            | ScriptCommand::WindowSetSize { .. } => {}
        }
    }
}

/// Resolve a script-supplied path against the app directory, matching how
/// `set_src` and the tray icon path resolve, so authors can write
/// app-relative paths regardless of cwd.
fn resolve_app_path(path: &str, dir: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_relative() {
        dir.join(p)
    } else {
        p.to_path_buf()
    }
}

/// Log a failed launch. Success is silent: the platform helper exiting
/// zero says the handler started, not that the user saw anything, so
/// there is nothing useful to report.
fn report_launch(result: &lumen_os_launcher::OpenResult, builtin: &str, target: &str) {
    if let lumen_os_launcher::OpenResult::Failed(msg) = result {
        eprintln!("lumenc: {builtin}('{target}'): {msg}");
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
