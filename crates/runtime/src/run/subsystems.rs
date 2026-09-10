//! The core stack [`build_app`](super::app_build::build_app) installs, and
//! the environment it installs the optional subsystems with.
//!
//! Each `register_*` fn groups the `add_plugin` / `add_systems` /
//! `insert_resource` wiring for one part of the core (text, layout and
//! input, the reactive bindings, the command bus, styling), so `build_app`
//! reads as a sequence of installs. The core is never gated: every visual
//! app needs all of it.
//!
//! Everything optional (OS integration, the introspection server, the HTTP
//! client, the devtools overlay) is a `lumen-capability` entry its own crate
//! registers, installed by `build_app` at the phase it asked for and never
//! named here. A subsystem that should stay idle for an app that does not
//! use it decides that itself, from the [`CapabilityEnv`] built below.
//!
//! CONSERVATIVE GATING CONTRACT: a subsystem skips itself only on a
//! *reliable* signal that it is unused. When in doubt (an artifact whose
//! source cannot be read, an embedder Rust hook the scan cannot see, a read
//! error), it installs: a false positive wastes a little idle work, whereas
//! a false negative would silently drop a subsystem the app depends on.
//! The environment carries that doubt as opacity, under which every source
//! query answers yes.

use lumen_capability::CapabilityEnv;

use super::*;

/// The environment the optional subsystems are installed with, for the app
/// at `dir`: its config, the run mode, and a bounded read of its sources.
///
/// A precompiled artifact carries no readable source at this point, so the
/// environment is opaque for one. In-memory markup counts as source.
pub(crate) fn capability_env(
    opts: &RunOptions,
    dir: &Path,
    cfg: &crate::config::LumenToml,
) -> CapabilityEnv {
    let no_source = opts.artifact.is_some() || opts.artifact_bytes.is_some();
    // Single bounded read of the app's source into one haystack, reused by
    // every query a subsystem makes. Skipped for an artifact (nothing to read).
    let mut hay = opts.markup.clone().unwrap_or_default();
    if !no_source {
        let mut budget: usize = 128;
        scan_sources(dir, &mut hay, &mut budget, 0);
    }
    CapabilityEnv::new(dir, cfg.raw.clone(), hay, no_source).headless(opts.bounded)
}

/// Bounded read of an app's `.lmn` / `.rhai` / `.lua` / `.cdl` / `.css` source
/// tree into a single haystack. Shared by [`capability_env`] and lumenc's
/// compile-time bundle capability inference
/// ([`crate::config::BundleCapabilities::resolve`]) so both apply the same
/// conservative marker scan.
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
    // The clipboard host. Part of the core rather than optional because the
    // text fields above paste and copy through it. `NonSend` (`arboard` is
    // `!Send` on Linux and Wayland) and long-lived on purpose: on X11 the
    // process that wrote the selection has to stay alive to serve it, so a
    // per-call handle would lose the text the moment the call returned. A
    // backend that refuses (headless CI, no compositor) leaves the resource
    // absent and the clipboard builtins no-op with a warning.
    match ClipboardHost::try_new() {
        Some(host) => app.world.insert_non_send(host),
        None => eprintln!("lumenc: no clipboard backend; clipboard builtins are inert"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumen_subsys_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A bare UI app (no hotkey builtin, readable source) answers a use
    /// query no, which is the skip path for a subsystem that gates on one;
    /// an app that calls the builtin answers yes.
    #[test]
    fn a_source_scan_answers_a_use_query_from_the_markup() {
        let dir = temp_dir("usage");
        let cfg = crate::config::LumenToml::default();
        let bare = RunOptions::new(&dir)
            .with_markup("<root><button id=\"inc\">+</button></root>".to_string());
        let env = capability_env(&bare, &dir, &cfg);
        assert!(!env.sources_mention(&["register_hotkey"]));
        assert!(!env.sources_mention(lumen_script::FILE_DIALOG_BUILTINS));

        let hotkey_app = RunOptions::new(&dir).with_markup(
            "<root><script>fn f(){ register_hotkey(\"Ctrl+S\",\"save\"); }</script></root>"
                .to_string(),
        );
        let env = capability_env(&hotkey_app, &dir, &cfg);
        assert!(env.sources_mention(&["register_hotkey"]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An artifact has no readable source, so the environment is opaque and
    /// every use query answers yes: a subsystem the app might drive is
    /// installed rather than dropped in silence.
    #[test]
    fn an_artifact_makes_every_use_query_answer_yes() {
        let dir = temp_dir("artifact");
        let cfg = crate::config::LumenToml::default();
        let compiled = RunOptions::new(&dir).with_artifact_bytes(Vec::new());
        let env = capability_env(&compiled, &dir, &cfg);
        assert!(env.sources_mention(&["register_hotkey"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The run mode reaches the environment: a bounded run is headless.
    #[test]
    fn a_bounded_run_is_headless() {
        let dir = temp_dir("bounded");
        let cfg = crate::config::LumenToml::default();
        let mut opts = RunOptions::new(&dir).with_markup("<root/>".to_string());
        opts.bounded = true;
        assert!(capability_env(&opts, &dir, &cfg).headless);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
