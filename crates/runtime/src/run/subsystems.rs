//! Per-subsystem register units for [`build_app`](super::app_build::build_app).
//!
//! Each `register_*` fn groups the `add_plugin` / `add_systems` /
//! `insert_resource` wiring for one subsystem so `build_app` reads as a
//! sequence of subsystem installs instead of a 700-line flat block. Two
//! payoffs:
//!
//! 1. **Startup / RSS gating (today).** [`SubsystemUsage`] is computed once
//!    from a single bounded source scan + `lumen.toml` + run-mode flags, and
//!    the gated units (MCP, global hotkeys, file dialogs) are skipped when the
//!    app provably does not use them - so a pure-UI app binds no MCP port and
//!    grabs no X11 hotkey manager.
//! 2. **Compile-time tree-shaking (future).** With the wiring already carved
//!    per subsystem, dropping one from a build becomes a one-line `cfg` /
//!    manifest gate on its `register_*` call - no untangling first.
//!
//! CONSERVATIVE GATING CONTRACT: a unit is skipped only when there is a
//! *reliable* signal the subsystem is unused. When in doubt (AOT artifact
//! whose source we cannot read, an embedder Rust hook we cannot scan, a read
//! error), the unit stays registered - a false positive merely wastes a little
//! idle work, whereas a false negative would silently drop a subsystem the app
//! depends on. Units with no reliable "unused" signal are left default-on and
//! carry a `TODO(tree-shake)` note naming the signal that would let them gate.

use super::*;

/// Static per-subsystem usage signals, resolved once up front and used to
/// gate the register units below.
///
/// Each field answers "should this subsystem be initialised for this app?".
/// Detection is a bounded static scan of the app's `.lmn` / `.rhai` / `.lua` /
/// `.cdl` / `.css` sources (plus any in-memory markup) for per-subsystem usage
/// markers, with `lumen.toml` overrides taking precedence where they exist.
/// Every signal errs toward ON (see the module contract).
pub(crate) struct SubsystemUsage {
    /// Install the global-hotkey OS manager (`OsHotkeyRegistry`) + the
    /// per-tick `poll_hotkeys` drain. The manager opens an X11 connection on
    /// Linux, so skipping it for a hotkey-free app is a real idle win.
    pub(crate) hotkey: bool,
    /// Install `AsyncTokioPlugin` so file dialogs resolve on the shared tokio
    /// runtime instead of blocking the tick. The runtime spawns worker
    /// threads, so a dialog-free app skips it.
    pub(crate) file_dialog: bool,
}

impl SubsystemUsage {
    /// Resolve every subsystem signal from one bounded source scan.
    ///
    /// `has_app_hooks` is `true` when the embedder supplied `RunOptions`
    /// `app_hooks` (Rust SDK closures). Their code is not scannable here, so
    /// it downgrades a gated subsystem's "unused" verdict to on - an
    /// SDK app may drive that subsystem from Rust.
    pub(crate) fn detect(opts: &RunOptions, dir: &Path, has_app_hooks: bool) -> Self {
        // A precompiled artifact carries no readable source at this point.
        let no_source = opts.artifact.is_some();

        // Single bounded read of the app's source into one haystack, reused by
        // every marker check below. Skipped for an artifact (nothing to read).
        let mut hay = opts.markup.clone().unwrap_or_default();
        if !no_source {
            let mut budget: usize = 128;
            scan_sources(dir, &mut hay, &mut budget, 0);
        }

        // Hotkey: previously always-on, now gated (a strict subset removal).
        // Gate on the `register_hotkey` script builtin (also matches
        // `unregister_hotkey`, which contains it, and the candela
        // `lumen::register_hotkey` form). Conservative fallbacks force it ON:
        // an artifact (opaque source) or any embedder hook (opaque Rust that
        // may register a hotkey).
        let hotkey = no_source || has_app_hooks || hay.contains("register_hotkey");

        // File dialogs: gate on the dialog builtins, with the same
        // conservative fallbacks as the hotkey gate above.
        let file_dialog = no_source || has_app_hooks || file_dialog_markers_present(&hay);

        Self {
            hotkey,
            file_dialog,
        }
    }
}

/// Bounded read of an app's `.lmn` / `.rhai` / `.lua` / `.cdl` / `.css` source
/// tree into a single haystack. Shared by the runtime startup gate
/// ([`SubsystemUsage::detect`]) and lumenc's compile-time bundle capability
/// inference ([`crate::config::BundleCapabilities::resolve`]) so both apply the
/// SAME conservative marker scan.
pub(crate) fn scan_app_sources(dir: &Path) -> String {
    let mut hay = String::new();
    let mut budget: usize = 128;
    scan_sources(dir, &mut hay, &mut budget, 0);
    hay
}

/// Bounded recursive read of the app's `.lmn` / `.rhai` / `.lua` / `.cdl` /
/// `.css` source files into `hay` for marker scanning. Depth- and
/// file-count-capped so a huge asset tree can't turn detection into a slow
/// directory crawl.
fn scan_sources(dir: &Path, hay: &mut String, budget: &mut usize, depth: u8) {
    if depth > 4 || *budget == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        if *budget == 0 {
            break;
        }
        let p = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            scan_sources(&p, hay, budget, depth + 1);
        } else if matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("lmn" | "rhai" | "lua" | "cdl" | "css")
        ) && let Ok(s) = std::fs::read_to_string(&p)
        {
            hay.push('\n');
            hay.push_str(&s);
            *budget -= 1;
        }
    }
}

/// True when `hay` (concatenated markup + script source) calls one of the
/// file-dialog builtins. `pick_file` is a prefix of `pick_files` and
/// `pick_file_filtered`, so these three markers cover the whole family.
pub(crate) fn file_dialog_markers_present(hay: &str) -> bool {
    ["pick_file", "save_file", "pick_folder"]
        .iter()
        .any(|marker| hay.contains(marker))
}

// -------------------------------------------------------------------------
// Core visual stack - always registered. Every visual app needs it; there is
// no gate here by design (see the module contract's "never gate the core").
// -------------------------------------------------------------------------

/// Text shaping. Installs the shaper the layout engine, the editing
/// systems, and the caret pass all measure with, and hands back a second
/// shaper for the renderer.
///
/// Both come from one backend so the shaped glyphs a frame paints are the
/// ones layout measured. The render-side shaper shares the layout
/// shaper's already-scanned font database rather than walking every
/// system font directory again, which a cold start used to pay twice.
///
/// An embedder that wants a different backend replaces the
/// [`ShaperService`] from an app hook; the layout engine reads whatever
/// is installed.
pub(crate) fn register_text(app: &mut App) -> Box<dyn TextShaper> {
    let layout_shaper = CosmicShaper::new();
    let render_shaper = CosmicShaper::new_sharing_db(&layout_shaper);
    app.world.insert_non_send(ShaperService::new(layout_shaper));
    Box::new(render_shaper)
}

/// Layout, text-editing, input, and the primitive interaction/visual plugins
/// (scroll / press / drag / dnd / hover / cursor / controls / form controls /
/// tooltip / tabs / transitions / validation / assets). The always-on stack.
pub(crate) fn register_core(app: &mut App) {
    app.add_plugin(TaffyLayoutPlugin);
    app.add_plugin(InputPlugin::default());
    // Accessibility: the world-side half, which walks the tree once per
    // tick in `TickStage::A11ySync` and leaves an update for whatever
    // platform bridge is listening. It runs headless too, so an app under
    // test reports the same tree it would to a screen reader.
    app.add_plugin(lumen_a11y_accesskit::A11yPlugin);
    // W2 Qt-polish (text-editing core): attaches TextBuffer / TextCursor /
    // UndoStack to every `<input>` / `<textarea>`, applies the pointer ->
    // caret / drag-select / double-click requests lumen-input produces
    // (same tick - the plugin orders its mutator after the shared
    // `TextEditSet::Producers` label), mirrors the buffer back into
    // TextContent/TextInput for the renderer + bindings, and drives the
    // caret blink phase.
    app.add_plugin(lumen_text::TextEditPlugin);
    // Caret-keep-visible: measure the caret x/y against the field box and
    // maintain the per-input scroll offset the extractor subtracts.
    // LayoutSync stage, after `sync_layout`, so the `Transform` from this
    // tick's layout pass is final before the offset is derived.
    app.add_systems(
        TickStage::LayoutSync,
        scroll_caret_into_view.after(lumen_layout_taffy::sync_layout),
    );
    app.add_plugin(ScrollPlugin);
    app.add_plugin(PressPlugin::default());
    app.add_plugin(DragPlugin::default());
    // In-app + file drag-and-drop: registers DropAccepted / DragStarted
    // and wires the drag-gesture -> drop-target hit-test pipeline.
    // TODO(tree-shake): a reliable "no drag/drop" signal is hard (any
    // draggable/droppable element or file-drop handler counts), so this stays
    // in the always-on core rather than gating on a false-negative-prone scan.
    app.add_plugin(lumen_os_dnd::DndPlugin);
    app.add_plugin(HoverTintPlugin);
    app.add_plugin(lumen_primitives::StateStylePlugin);
    // Wave 3: cursor-shape selection (I-beam / pointer / grab). The
    // window backend polls `CursorRequest` each frame; headless runs
    // simply never read it.
    app.add_plugin(lumen_primitives::CursorPlugin);
    app.add_plugin(ControlsPlugin);
    // W5 form controls: checkbox visuals + tri-state, radio groups
    // (selection, roving tabindex, arrow nav), progress fill/sweep.
    app.add_plugin(CheckboxPlugin);
    app.add_plugin(RadioPlugin);
    app.add_plugin(ProgressPlugin);
    app.add_plugin(TooltipPlugin);
    app.add_plugin(TabsPlugin);
    app.add_plugin(TransitionPlugin);
    app.add_plugin(ValidationPlugin);
    app.add_plugin(AssetsPlugin);
}

/// Reactive bindings + reconcilers + dialog lifecycle + the in-app error
/// overlay. Always registered: these drive `bind=`, `<for>` / `<if>`,
/// `<dialog>`, and hot-reload parse-error surfacing - core to every app.
pub(crate) fn register_reactive(app: &mut App) {
    // Reactive bindings: <element bind="text:foo"> reads
    // PropertyStore[Global("foo")] into TextContent every tick. Wave-D made
    // PropertyStore the canonical typed store; the legacy Signals resource
    // stays installed as a back-compat shim so embedders that still hold
    // `Res<Signals>` references keep observing writes via the per-tick
    // `mirror_property_store_globals_to_signals` back-mirror.
    #[allow(deprecated)]
    app.world.init_resource::<lumen_core::signals::Signals>();
    app.world
        .init_resource::<lumen_core::signals::ArraySignals>();
    // External signal channel: any thread (C ABI, background sampler,
    // tokio task) can push mutations into PropertyStore / ArraySignals via
    // `lumen_core::signals::push_external_signal` etc. Wave-D routes scalar
    // writes through `push_external_property` directly; the drain system
    // below only handles the residual ArraySignals payloads. Both drains run
    // once per tick and are cheap when their channel is empty.
    lumen_core::signals::init_external_signals();
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::drain_external_signals,
    );
    // What the reconcilers must do themselves rather than leave to the
    // presentation layer. A host that windows long lists or cascades CSS on
    // its own replaces this before the app runs.
    app.world.init_resource::<crate::spawn::ScenePolicy>();
    app.add_systems(TickStage::Systems, crate::spawn::reconcile_for_blocks);
    app.add_systems(TickStage::Systems, crate::spawn::reconcile_if_blocks);
    // A mounted subtree takes its place in the document, not at the end
    // of it: `DocumentOrder` is restated from the hierarchy once the
    // reconcilers have flushed their spawns, so Tab reaches an `<if>`
    // body where it sits in the markup no matter what follows it. The
    // `after` edges are what put the sync point in front of this system,
    // so the walk sees the entities the reconcilers just queued.
    app.add_systems(
        TickStage::Systems,
        crate::spawn::renumber_document_order
            .after(crate::spawn::reconcile_for_blocks)
            .after(crate::spawn::reconcile_if_blocks),
    );
    // W5 dialog contract (Qt QDialog):
    // - Enter-anywhere activates the default button. Ordered after the
    //   focused-key fanout (same-tick keystroke) and before the script
    //   click dispatch so the synthesized ClickEvent reaches `on_click`
    //   handlers on this very tick.
    // - Default-button clicks (pointer path) mark the pending accept
    //   after both click producers have run.
    // - The lifecycle system (initial focus / restore / exactly-once
    //   accepted-or-rejected) settles after the accept markers.
    // `activate_dialog_default_on_enter` is registered by
    // `register_script_systems::<H>` so its
    // `.before(dispatch_clicks_and_doubles::<H>)` edge anchors the host the
    // `[script] engine` key actually selected.
    app.add_systems(
        TickStage::Systems,
        crate::spawn::mark_dialog_accept_on_default_click
            .after(lumen_input::dispatch_clicks)
            .after(crate::spawn::activate_dialog_default_on_enter),
    );
    app.add_systems(
        TickStage::Systems,
        crate::spawn::manage_dialog_lifecycle
            .after(crate::spawn::reconcile_if_blocks)
            .after(crate::spawn::mark_dialog_accept_on_default_click),
    );
    // Esc closes every visible <dialog> by writing "" to its open
    // signal. Runs in Input stage so the next reconcile_if_blocks tick
    // observes the new signal state - and strictly after the Wave-3
    // press cancel, so an Escape consumed by an in-flight press leaves
    // the dialog open.
    app.add_systems(
        TickStage::Input,
        crate::spawn::close_dialogs_on_escape.after(lumen_input::cancel_press_on_escape),
    );
    // In-app error overlay: hot-reload parse failures land in
    // `ErrorBanner`, the reconciler spawns / updates / despawns the
    // banner entity, and Esc dismisses.
    app.world.insert_resource(ErrorBanner::default());
    app.add_systems(TickStage::Systems, reconcile_error_banner);
    app.add_systems(TickStage::Input, dismiss_error_banner_on_escape);
}

/// Command-bus drain, the FFI typed-read mirror, and the `set_color_scheme`
/// `Command::Typed` handler. Always registered: the command queue is the
/// canonical mutation seam and the drains no-op cheaply on empty queues.
pub(crate) fn register_commands(app: &mut App) {
    // Drain `Command::SetProperty` + `Command::Typed` entries on every
    // tick. W4.6 routes `set_color_scheme(name)` through `Command::Typed`
    // - registering this drain here means the matching handler installed
    // below actually runs. The drain is otherwise unused by lumenc today
    // and the drain implementation no-ops on empty queues, so the cost
    // is one atomic try_recv miss per tick.
    app.add_systems(
        TickStage::CommandDrain,
        lumen_core::command::apply_property_commands,
    );
    // FFI typed-read mirror: copy PropertyStore typed scalars into a
    // process-wide Mutex<HashMap> at tick end so cross-thread FFI
    // accessors (lumen_signal_get_int64 / _float64 / _bool / _color)
    // see writes from any source - ECS, script, or other FFI calls.
    // The mirror runs in A11ySync (the last main-world stage) so it
    // sees every write committed earlier in the tick.
    //
    // Wave-D: `drain_external_properties` is now registered globally inside
    // `App::new()` so we don't duplicate it here (bevy errors on
    // `.after(...)` against a SystemTypeSet that has more than one
    // registration). The init below stays idempotent.
    lumen_core::property_store::init_external_properties();
    // Ordered before `clear_property_store_dirty` (also A11ySync) so the
    // mirror still sees this tick's dirty keys - it now updates only the
    // dirtied cells instead of rebuilding the whole map every tick.
    app.add_systems(
        TickStage::A11ySync,
        lumen_core::property_store::mirror_property_store_to_typed_cache
            .before(lumen_core::property_store::clear_property_store_dirty),
    );
    // Register a `Command::Typed` handler for the `set_color_scheme` script
    // built-in. The runtime registers that built-in host-neutrally
    // (`crate::run::builtin_script_fns`), and its body pushes
    // [`ColorSchemeIntent`] payloads through
    // [`lumen_core::command::CommandQueue`]; this handler applies them to
    // [`StyleManager::set_scheme`] inside [`TickStage::CommandDrain`].
    // candela reaches the same [`StyleManager`] through its own prelude, via
    // `ScriptCommand::SetColorScheme`.
    //
    // Risk register section "set_root_class based theme demos break" calls out
    // that the legacy migration path was `set_root_class("theme-dark")`;
    // the new path is `set_color_scheme("force-dark")` (or `"default"` for
    // OS-follow). `set_root_class` keeps working - it just sets classes -
    // but theme-token CSS now hangs off `StyleManager::effective_dark`.
    app.register_command::<ColorSchemeIntent, _>(|world, payload| {
        world
            .resource_mut::<lumen_core::components::StyleManager>()
            .set_scheme(payload.0);
    });
}

/// Style-invalidation cache, style version tracking, the live combined
/// stylesheet, the theme/media re-resolver systems, and the per-cache memory
/// budget. Always registered: every app carries CSS + a memory budget.
pub(crate) fn register_styles(
    app: &mut App,
    ir: &lumen_ir::layout_ir::LayoutIR,
    cfg: &crate::config::LumenToml,
) {
    // Install the `MemoryBudget` resource with defaults overridden by
    // `lumen.toml [perf]`. `enforce_budget` runs each tick and evicts cache
    // entries until each cache is under its cap.
    let mut budget = lumen_core::components::MemoryBudget::default();
    if let Some(v) = cfg.perf.images_mb {
        budget.images_mb = v;
    }
    if let Some(v) = cfg.perf.shape_entries {
        budget.shape_entries = v;
    }
    if let Some(v) = cfg.perf.scene_fragments {
        budget.scene_fragments = v;
    }
    app.world.insert_resource(budget);
    // Runs in A11ySync (the last main-world stage) so eviction reflects
    // the steady-state of the just-ticked frame.
    app.add_systems(TickStage::A11ySync, enforce_budget);

    // Compute the union of class names referenced by any skin + user CSS
    // selector. `reapply_styles_on_root_class_change` consults the set to
    // skip respawns when no changed class can match a selector.
    //
    // Derived directly from the already-combined (skin + user) stylesheet on
    // the IR - no re-read from disk and no re-parse. This works identically
    // for the parse-from-source and the artifact-load paths, and keeps the
    // cache off the source parser entirely.
    let inval = match &ir.combined_stylesheet {
        Some(sheet) => StyleInvalidationCache::from_stylesheet(sheet),
        None => StyleInvalidationCache::default(),
    };
    app.world.insert_resource(inval);
    // W4.7: monotonic counter bumped on each runtime class / palette /
    // media-feature flip; downstream cascade consumers re-resolve only
    // entities flagged by `StyleInvalidationCache`. Starts at 0 so a
    // first-tick bump cleanly signals "stale".
    app.world.insert_resource(StyleVersion::default());
    // The combined (skin + user) stylesheet, kept live so the theme /
    // media re-resolver can re-run the cascade without a disk read.
    if let Some(sheet) = ir.combined_stylesheet.clone() {
        app.world.insert_resource(RuntimeStylesheet(sheet));
    }
    // Tracks the last `StyleVersion` the in-place re-resolver actually
    // consumed, so `reapply_computed_styles` re-walks only after a bump.
    app.world
        .insert_resource(AppliedStyleVersion(StyleVersion::default().0));
    // Order in `TickStage::Systems`:
    //   1. `detect_media_change`   - theme / viewport-breakpoint flip -> bump
    //   2. `reapply_styles_on_root_class_change` - root class flip -> bump
    //   3. `apply_dom_commands` - script spawns / class edits -> bump
    //   4. `reconcile_if_blocks` - a newly mounted body -> bump
    //   5. `reapply_computed_styles` - consume the bump, re-resolve entities
    //
    // The `apply_dom_commands` edge is what keeps a scripted DOM edit
    // single-frame. Every spawn, reparent, class edit and inline-style
    // write bumps `StyleVersion` at the end of that system, and
    // `reapply_computed_styles` is the only thing that turns the bump into
    // real components: a fresh `spawn("label")` carries no cascaded
    // `TextStyle`, `Visuals` or box `Style` until it runs. Without the
    // edge the consumer can be scheduled ahead of the producer, so a
    // script that rebuilds a subtree paints one frame of unstyled,
    // wrongly-measured nodes before the cascade lands on the next tick -
    // the whole pane visibly flashes on every edit that rebuilds it.
    //
    // The `reconcile_if_blocks` edge does the same job for the elements a
    // `<if>` gate mounts. Those carry the attributes the load-time cascade
    // resolved, so without a re-resolve a page reached by navigation comes
    // up in whatever color scheme the app booted with rather than the one
    // now in force.
    app.add_systems(TickStage::Systems, detect_media_change);
    app.add_systems(
        TickStage::Systems,
        reapply_styles_on_root_class_change.after(detect_media_change),
    );
    app.add_systems(
        TickStage::Systems,
        reapply_computed_styles
            .after(reapply_styles_on_root_class_change)
            .after(lumen_scene::dom::apply_dom_commands)
            .after(crate::spawn::reconcile_if_blocks),
    );
}

// -------------------------------------------------------------------------
// OS-integration subsystems.
// -------------------------------------------------------------------------

/// Global-hotkey OS manager + the per-tick drain. GATED on
/// [`SubsystemUsage::hotkey`] (the `register_hotkey` script marker): the
/// manager opens an X11 connection on Linux, so a hotkey-free app skips it
/// entirely. `None` (init failure / displayless host) silently disables
/// support; `register_hotkey` then no-ops with a warning.
pub(crate) fn register_os_hotkey(app: &mut App) {
    // S30 global hotkeys - install the manager as a non-send
    // resource (some platforms own a main-thread channel) and poll
    // the event receiver each tick.
    if let Some(reg) = OsHotkeyRegistry::new() {
        app.world.insert_non_send(reg);
        // Both `HotkeyPressed` and `HotkeyReleased` are registered by
        // `App::new`, beside every other input message, so `poll_hotkeys`
        // can write either one without a local registration here.
        app.add_systems(TickStage::Systems, lumen_os_hotkey::poll_hotkeys);
    }
}

/// File-dialog host resource, plus the executor the dialogs resolve on.
///
/// The service itself is DEFAULT-ON: its constructor is a single `AtomicU64`
/// (no thread, no device), so an idle app pays nothing and a false negative
/// would silently swallow an embedder's `pick_file(...)`.
///
/// `AsyncTokioPlugin` is GATED on [`SubsystemUsage::file_dialog`] because it
/// builds a multi-threaded tokio runtime. It has to be installed for dialogs
/// to work at all on macOS: `NSOpenPanel` only resolves while the main run
/// loop is pumping, so a dialog run inline deadlocks there and reports a
/// cancel. Installing an executor here rather than leaving it to embedders
/// is what makes `pick_file` reach the user on every platform. The dialog
/// crate itself names no backend; it reads whichever `SpawnService` this
/// installs.
pub(crate) fn register_os_filedialog(app: &mut App, file_dialog_used: bool) {
    app.world.insert_resource(FileDialogService::new());
    // A resolved dialog comes back as a typed command from whichever thread
    // ran it. Without a handler for that payload the command drain discards
    // it and the script's `on_file_picked` never fires, so this registration
    // is what closes the loop between `pick_file(...)` and the callback.
    app.register_command::<FileDialogResultCommand, _>(|world, payload| {
        world.write_message(FilePicked::from(*payload));
    });
    #[cfg(feature = "async")]
    if file_dialog_used {
        app.add_plugin(lumen_async_tokio::AsyncTokioPlugin);
    }
    #[cfg(not(feature = "async"))]
    let _ = file_dialog_used;
}

/// Notification host resource + the per-tick action-button drain. DEFAULT-ON.
///
/// The `[app] id` from `lumen.toml` becomes the notification app id: Windows
/// keys toasts off the AppUserModelID and macOS off the bundle id, so without
/// it a notification is attributed to whatever binary happens to be running.
///
/// TODO(tree-shake): gate on the `notify` script builtin. Left default-on:
/// `NotificationService::new()` opens no thread and no connection, so the
/// idle cost is nil and a false negative would silently swallow an
/// embedder's `notify(...)`.
pub(crate) fn register_os_notify(app: &mut App, cfg: &crate::config::LumenToml) {
    let service = match cfg.app.id.as_deref().filter(|s| !s.is_empty()) {
        Some(id) => NotificationService::new().with_app_id(id),
        None => NotificationService::new(),
    };
    app.world.insert_resource(service);
    app.add_systems(
        TickStage::Systems,
        lumen_os_notify::poll_notification_actions,
    );
}

/// System-tray host resource + the per-tick click drain. DEFAULT-ON.
///
/// TODO(tree-shake): gate on the `tray_icon` script builtin / a `<tray>`
/// markup marker. Left default-on: the service is `Default` (registration is
/// what actually creates the OS icon, lazily), and `poll_tray_events` is a
/// cheap per-tick queue check.
pub(crate) fn register_os_tray(app: &mut App) {
    app.world.insert_non_send(OsTrayService::new());
    // Every target has a `poll_tray_events`: `tray-icon` backs macOS and
    // Windows, `ksni` backs Linux, and anything else gets an inert stub.
    app.add_systems(TickStage::Systems, lumen_os_tray::poll_tray_events);
}

/// Clipboard, launcher, and sleep-inhibit hosts. DEFAULT-ON.
///
/// The clipboard handle is `NonSend` (`arboard` is `!Send` on Linux and
/// Wayland) and long-lived on purpose: on X11 the process that wrote the
/// selection has to stay alive to serve it, so a per-call handle would lose
/// the text the moment the call returned. A backend that refuses (headless
/// CI, no compositor) leaves the resource absent and the clipboard builtins
/// no-op with a warning.
///
/// The launcher and the inhibit holder are both idle until called: the
/// launcher is stateless, and the holder only talks to the platform once a
/// script asks to keep the machine awake.
pub(crate) fn register_os_misc(app: &mut App, cfg: &crate::config::LumenToml) {
    match ClipboardHost::try_new() {
        Some(host) => app.world.insert_non_send(host),
        None => eprintln!("lumenc: no clipboard backend; clipboard builtins are inert"),
    }
    app.world.insert_resource(Launcher::new());
    let app_name = cfg.app.id.clone().unwrap_or_else(|| "lumen".to_string());
    app.world
        .insert_non_send(InhibitHolder::new().with_app_name(app_name));
}

/// Recent-files, autostart, and single-instance hosts. DEFAULT-ON.
///
/// `LifecycleService`, `RecentFilesService`, and `AutostartService` are all
/// cheap to construct - a couple of cloned `PathBuf`s, no thread, no
/// connection - so, like the launcher and the inhibit holder above, they are
/// left in unconditionally rather than gated on a script marker.
///
/// The app id comes off [`lumen_core::app_paths::app_id`], published by
/// `build_app` before this runs, rather than off `cfg` directly: it is the
/// same id `read_file` / `data_dir` resolve against, so the recent-files
/// store lands in the identical `<data-dir>/lumen/<id>` directory a script's
/// own `data_dir()` call would see.
///
/// Single-instance locking itself does not happen here: binding a socket is
/// not cheap-and-side-effect-free the way these constructors are, and doing
/// it for every headless / test run of `build_app` would make CI runs and
/// SDK embedders fight each other over one socket. See [`super::run_app`]'s
/// single-instance gate, which runs before `build_app` and, on the primary
/// path, hands its already-bound `LifecycleService` in through an
/// `app_hooks` closure so the poll system below drains the SAME inbox the
/// gate's listener thread feeds - not a second, freshly constructed one
/// whose inbox nothing writes to. A `single_instance`-free app (the common
/// case) gets the default, never-bound `LifecycleService` this function
/// inserts; the poll system is a no-op for it.
pub(crate) fn register_os_lifecycle(app: &mut App) {
    let id = lumen_os_lifecycle::AppId::from(lumen_core::app_paths::app_id());
    let lifecycle = lumen_os_lifecycle::LifecycleService::new();
    let recent = lumen_os_lifecycle::RecentFilesService::new(lifecycle.data_dir(&id));
    // The autostart entry has to point somewhere; a `lumenc run` dev session
    // launches through the `lumenc` binary itself, which is the best answer
    // available without a packaged app's own launcher path.
    let exe = std::env::current_exe().unwrap_or_default();
    let autostart = lumen_os_lifecycle::AutostartService::new(id, exe);
    app.world.insert_resource(lifecycle);
    app.world.insert_resource(recent);
    app.world.insert_resource(autostart);
    app.add_systems(TickStage::Systems, lumen_os_lifecycle::poll_second_instance);
}

/// The HTTP client the scripts' `fetch()` / `http()` builtins run on.
///
/// Installed as the `FetchRegistry` the script plugin would otherwise create
/// for itself, so it has to run before the host plugins are added. The plugin
/// leaves an existing registry alone, which is also how an embedder swaps in
/// its own `lumen_script::HttpClient` from an app hook or by inserting the
/// resource first.
///
/// Costs nothing for an app that never fetches: no connection is opened until
/// a request is queued, and each one runs on its own short-lived worker
/// thread, so there is no gate on usage here.
///
/// COMPILE-TIME GATE (Part B tree-shaking): the shipped client lives behind the
/// `http-fetch` cargo feature. Without it no registry is installed here and the
/// builtins answer every request with the "built without `http-fetch`" error.
#[cfg(feature = "http-fetch")]
pub(crate) fn register_http_client(app: &mut App) {
    app.world
        .insert_resource(lumen_script::FetchRegistry::with_client(
            std::sync::Arc::new(lumen_http_ureq::UreqHttpClient),
        ));
}

/// Inert HTTP register unit for a build compiled WITHOUT `http-fetch`: the
/// script plugin falls back to the disabled client, so `fetch()` reports the
/// missing transport instead of silently doing nothing.
#[cfg(not(feature = "http-fetch"))]
pub(crate) fn register_http_client(_app: &mut App) {}

/// MCP introspection server. GATED on the resolved run-mode + `[mcp]` config.
///
/// COMPILE-TIME GATE (Part B tree-shaking): behind the `mcp` cargo feature.
/// MCP is a dev/introspection capability, so lumenc never infers it into a
/// release `--bundle`; a trimmed build drops `lumen-mcp` and this is the inert
/// no-op below (which still prints the "disabled" hint).
#[cfg(feature = "mcp")]
pub(crate) fn register_mcp(app: &mut App, bounded: bool, cfg: &crate::config::LumenToml) {
    // Precedence:
    //   1. `[mcp] port = 0`            -> hard-disabled.
    //   2. `[runtime] mcp = false`     -> disabled; `= true` -> force-enabled
    //                                     even in a headless/bounded run.
    //   3. bounded (headless) run      -> disabled UNLESS `[mcp] simulate`,
    //                                     because the server thread + per-tick
    //                                     snapshot pipeline are pure overhead
    //                                     for a `--ticks N` bench; automation
    //                                     drivers that need input injection
    //                                     set `simulate = true` and keep it.
    //   4. otherwise (interactive)     -> enabled (the default agent workflow).
    // The server thread is what the `SurfaceCapture` screenshot path lives on,
    // so when it is gated off the rendered-headless loop simply finds no
    // `SurfaceCapture`/`McpSnapshotSchedule` resource (both reads are
    // `Option`-guarded there) and skips capture - a plain tick bench needs
    // neither.
    let simulate_enabled = cfg.mcp.simulate.unwrap_or(false);
    // Off by default like `simulate`: `lumen_framework_status` shells out to
    // `git`/`gh` when this is on, and the introspection port has no
    // authentication, so a shipped app leaves subprocess execution off that
    // surface unless a developer opts in.
    let issues_enabled = cfg.mcp.issues.unwrap_or(false);
    let mcp_enabled = match cfg.runtime.mcp {
        Some(v) => v,
        None => !(bounded && !simulate_enabled),
    };
    let mcp_port: Option<u16> = match (mcp_enabled, cfg.mcp.port) {
        (false, _) | (_, Some(0)) => None,
        (true, Some(p)) => {
            app.add_plugin(
                LumenMcpPlugin::with_port(p)
                    .with_simulate_enabled(simulate_enabled)
                    .with_issues_enabled(issues_enabled),
            );
            Some(p)
        }
        (true, None) => {
            app.add_plugin(
                LumenMcpPlugin::default()
                    .with_simulate_enabled(simulate_enabled)
                    .with_issues_enabled(issues_enabled),
            );
            Some(7878)
        }
    };
    print_mcp_help_snippet(mcp_port, simulate_enabled, issues_enabled);

    // Input-simulation automation (benchmarks, UI tests) drives the app
    // through the MCP `lumen.simulate` queue and observes progress through
    // the snapshot frame counter and scroll-corrected rects. The default
    // 1 Hz `McpSnapshotSchedule` throttle makes that observation useless
    // windowed: the frame counter advances ~once a second (so an external
    // driver reconstructs ~1 fps frame intervals even while the window
    // presents at vsync), and re-queried rects go stale between a
    // scroll-into-view nudge and the follow-up read. The rendered headless
    // path already zeroes this interval for exactly the same reason
    // (`run_headless.rs`); mirror it here whenever simulation is enabled so
    // the windowed automation path is measured on the same footing. Passive
    // introspection (simulate disabled) keeps the 1 Hz throttle to avoid the
    // per-frame snapshot sweep on a normal interactive app.
    if mcp_port.is_some() && simulate_enabled {
        for world in [&mut app.world, &mut app.render_world] {
            if let Some(mut sched) = world.get_resource_mut::<lumen_mcp::McpSnapshotSchedule>() {
                sched.interval = std::time::Duration::ZERO;
            }
        }
    }
}

/// Inert MCP register unit for a build compiled WITHOUT the `mcp` feature:
/// `lumen-mcp` is absent, so no introspection server is installed. Still prints
/// the "disabled" hint so tooling sees a consistent line.
#[cfg(not(feature = "mcp"))]
pub(crate) fn register_mcp(_app: &mut App, _bounded: bool, _cfg: &crate::config::LumenToml) {
    print_mcp_help_snippet(None, false, false);
}

/// Print a copy-pasteable MCP setup hint on stdout. Designed for one-shot
/// scan by an AI agent: the port number on the first line, then a JSON
/// fragment ready to drop into a Claude Code `.mcp.json`. Skipped when the
/// MCP server is disabled (`[mcp] port = 0`).
fn print_mcp_help_snippet(port: Option<u16>, simulate_enabled: bool, issues_enabled: bool) {
    let Some(port) = port else {
        // `port` is `None` for any of: `[mcp] port = 0`, `[runtime] mcp =
        // false`, or a headless/bounded run (where the server is gated off
        // unless simulation is enabled).
        println!("lumenc: MCP server disabled");
        return;
    };
    let sim = if simulate_enabled {
        "ON"
    } else {
        "off - set [mcp] simulate = true in lumen.toml to enable input injection"
    };
    let issues = if issues_enabled {
        "ON"
    } else {
        "off - set [mcp] issues = true in lumen.toml to let lumen_framework_status list open issues"
    };
    println!("lumenc: MCP server on 127.0.0.1:{port} (simulate: {sim})");
    println!("        issue lookup: {issues}");
    println!("        try: lumenc snapshot --port {port}");
    println!("        Claude Code config snippet (drop into .mcp.json under \"mcpServers\"):");
    println!("        \"lumen\": {{");
    println!("          \"command\": \"lumen-mcp-server\",");
    println!("          \"args\": [\"--host\", \"127.0.0.1\", \"--port\", \"{port}\"]");
    println!("        }}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare UI app (no hotkey builtin, from-disk source, no hooks) resolves
    /// the gated OS signals OFF - the no-hotkey / no-dialog skip paths. A
    /// hotkey app flips its signal on.
    #[test]
    fn usage_detect_gates_bare_app_off() {
        let dir = std::env::temp_dir().join(format!(
            "lumen_subsys_usage_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let bare = RunOptions::new(&dir)
            .with_markup("<root><button id=\"inc\">+</button></root>".to_string());
        let usage = SubsystemUsage::detect(&bare, &dir, false);
        assert!(!usage.hotkey, "bare UI app must skip hotkey manager");
        assert!(
            !usage.file_dialog,
            "bare UI app must skip the dialog bridge"
        );

        let hotkey_app = RunOptions::new(&dir).with_markup(
            "<root><script>fn f(){ register_hotkey(\"Ctrl+S\",\"save\"); }</script></root>"
                .to_string(),
        );
        let usage = SubsystemUsage::detect(&hotkey_app, &dir, false);
        assert!(usage.hotkey, "hotkey app must init the manager");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Conservative fallback for the gated hotkey unit: an embedder-hook
    /// app (opaque Rust that may register a hotkey) forces hotkey ON even with
    /// no marker.
    #[test]
    fn app_hooks_force_hotkey_on() {
        let dir = std::env::temp_dir().join(format!(
            "lumen_subsys_hooks_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let hooked = RunOptions::new(&dir).with_markup("<root/>".to_string());
        let usage = SubsystemUsage::detect(&hooked, &dir, true);
        assert!(usage.hotkey, "app_hooks must force the hotkey manager ON");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
