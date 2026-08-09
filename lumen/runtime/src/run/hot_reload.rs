use super::*;

// Read by always-compiled runtime systems (`apply_script_commands` for
// asset-dir + root-entity access, `reapply_styles_on_root_class_change`),
// so the struct stays compiled unconditionally; only its *insertion* (the
// hot-reload watcher) is gated behind `runtime-parse`. In a parser-free
// build the resource is simply never inserted and those systems see `None`.
#[derive(Resource)]
// Only `dir` / `root` are read in a parser-free build; the watch-path fields
// exist for the (gated) hot-reload poll.
#[cfg_attr(not(feature = "runtime-parse"), allow(dead_code))]
pub(crate) struct HotReloadState {
    pub(crate) dir: PathBuf,
    pub(crate) html_path: PathBuf,
    pub(crate) css_path: PathBuf,
    pub(crate) html_mtime: Option<SystemTime>,
    pub(crate) css_mtime: Option<SystemTime>,
    /// Resolved paths of every external `.rhai` script the markup
    /// referenced via `<script src="...">`.
    pub(crate) script_paths: Vec<PathBuf>,
    /// mtime of each `script_paths` entry, in the same order. `None`
    /// means the file didn't exist last time we checked.
    pub(crate) script_mtimes: Vec<Option<SystemTime>>,
    /// Resolved paths of every `<include>`d `.lmn` file (transitive), so a
    /// hot edit to an included fragment re-triggers a reload.
    pub(crate) include_paths: Vec<PathBuf>,
    /// mtime of each `include_paths` entry, in the same order.
    pub(crate) include_mtimes: Vec<Option<SystemTime>>,
    /// Resolved paths of every `@import`ed `.css` file (transitive).
    pub(crate) css_import_paths: Vec<PathBuf>,
    /// mtime of each `css_import_paths` entry, in the same order.
    pub(crate) css_import_mtimes: Vec<Option<SystemTime>>,
    /// Path + mtime of every `locale/*.ftl` catalogue. Stored as whole
    /// pairs rather than two parallel lists because the set itself can
    /// change: adding a translation file is a change, not just editing one.
    pub(crate) locale_stamps: Vec<(PathBuf, Option<SystemTime>)>,
    /// Extra `[asset_roots]` from `lumen.toml`, absolutized once at
    /// startup so hot-reload can keep using them.
    pub(crate) asset_roots: Vec<PathBuf>,
    /// Fallback `<root skin>` name from `lumen.toml`. Used only when the
    /// markup doesn't carry an explicit `skin="..."`.
    pub(crate) skin_override: Option<String>,
    pub(crate) root: Entity,
}
// HotkeyRegistry + register/unregister/poll moved to lumen-os-hotkey
// (W6.5). The runtime now installs `lumen_os_hotkey::HotkeyRegistry`
// via `OsHotkeyRegistry::new()` + adds `lumen_os_hotkey::poll_hotkeys`
// as the polling system, which surfaces both Pressed and Released
// events (the audit's `run.rs:931-933` "release dropped" bug).

/// Minimum wall-clock gap between hot-reload disk polls (fallback mode
/// only). The watcher stat()s the html + css + every script file, so
/// running it every tick (potentially 120+ Hz) is pure syscall churn; a
/// source edit taking effect up to ~300 ms late is imperceptible.
///
/// Hot reload re-parses source on change, so the whole machinery is gated
/// to `runtime-parse` builds.
#[cfg(feature = "runtime-parse")]
const HOT_RELOAD_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Wall-clock time of the last hot-reload disk poll. Throttles [`hot_reload`]
/// in [`HotReloadDriver::Poll`] mode.
#[cfg(feature = "runtime-parse")]
#[derive(Resource)]
struct HotReloadThrottle(Instant);

/// Cross-thread signal between the `notify` watcher callback and the
/// [`hot_reload`] system. The callback (notify's own thread) raises
/// `changed` and fires the wired [`lumen_core::app::EventLoopWaker`] so a
/// parked event loop wakes for exactly one tick; `hot_reload` consumes the
/// flag and runs its mtime diff (which filters spurious events - the diff
/// deciding "no source actually changed" makes an over-eager event cost
/// one no-op tick, never a reload).
#[cfg(feature = "runtime-parse")]
#[derive(Default)]
pub(crate) struct HotReloadFlag {
    changed: std::sync::atomic::AtomicBool,
    waker: std::sync::OnceLock<lumen_core::app::EventLoopWaker>,
}

#[cfg(feature = "runtime-parse")]
impl HotReloadFlag {
    /// Raise the changed flag and wake the parked loop (if a waker has
    /// been wired). Called from the notify callback thread.
    fn raise(&self) {
        self.changed
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(w) = self.waker.get() {
            w.wake();
        }
    }

    /// Consume the flag. Called once per tick by [`hot_reload`].
    fn take_changed(&self) -> bool {
        self.changed
            .swap(false, std::sync::atomic::Ordering::AcqRel)
    }
}

/// How source changes are detected. Inserted alongside [`HotReloadState`].
#[cfg(feature = "runtime-parse")]
#[derive(Resource, Clone)]
pub(crate) enum HotReloadDriver {
    /// `notify` file watcher: the loop parks indefinitely at idle and is
    /// woken by real fs events - zero poll ticks.
    Watch {
        /// Shared changed-flag + waker slot.
        flag: std::sync::Arc<HotReloadFlag>,
        /// Keeps the OS watcher registration alive for the app lifetime.
        _watcher: std::sync::Arc<std::sync::Mutex<notify::RecommendedWatcher>>,
    },
    /// mtime polling at [`HOT_RELOAD_POLL_INTERVAL`]. Fallback when
    /// `LUMEN_HOT_RELOAD_POLL` is set or watcher init fails; the headless
    /// loop keeps its periodic wake slices in this mode so the poll runs.
    Poll,
}

/// Build a `notify` watcher covering every hot-reload source file. Watches
/// the (deduplicated) parent directories non-recursively - editors save via
/// temp-file + rename, so watching the files themselves would silently drop
/// the registration on the first save.
#[cfg(feature = "runtime-parse")]
pub(crate) fn spawn_hot_reload_watcher(
    watch_dirs: &std::collections::HashSet<PathBuf>,
    flag: std::sync::Arc<HotReloadFlag>,
) -> Result<notify::RecommendedWatcher, notify::Error> {
    use notify::Watcher;
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            // Raise on errors too - a missed rescan beats a silently dead
            // watcher; the mtime diff filters false positives.
            let _ = res;
            flag.raise();
        })?;
    for d in watch_dirs {
        watcher.watch(d, notify::RecursiveMode::NonRecursive)?;
    }
    Ok(watcher)
}

#[cfg(feature = "runtime-parse")]
pub(crate) fn hot_reload<H: ScriptHost + Resource<Mutability = Mutable>>(world: &mut World) {
    // Decide whether the (comparatively expensive) mtime sweep runs this
    // tick. Watch mode: only when the notify callback raised the flag.
    // Poll mode: at most every `HOT_RELOAD_POLL_INTERVAL`.
    let watch_flag = match world.get_resource::<HotReloadDriver>() {
        Some(HotReloadDriver::Watch { flag, .. }) => Some(std::sync::Arc::clone(flag)),
        _ => None,
    };
    let due = match watch_flag {
        Some(flag) => {
            // Wire the loop waker in lazily - the resource appears after
            // plugin build (headless: pre-loop; windowed: run()) and this
            // system runs every tick, so the first tick wires it.
            if flag.waker.get().is_none()
                && let Some(w) = world.get_resource::<lumen_core::app::EventLoopWaker>()
            {
                let _ = flag.waker.set(w.clone());
            }
            flag.take_changed()
        }
        None => {
            let now = Instant::now();
            let due = match world.get_resource::<HotReloadThrottle>() {
                Some(t) => now.duration_since(t.0) >= HOT_RELOAD_POLL_INTERVAL,
                None => true,
            };
            if due {
                world.insert_resource(HotReloadThrottle(now));
            }
            due
        }
    };
    if !due {
        return;
    }

    // The markup/CSS front-end is injected (it lives in the compiler, not the
    // runtime). Without it there is nothing to re-parse, so a hot-reload tick
    // is a no-op. `build_app` inserts `RuntimeParser` alongside `HotReloadState`
    // whenever a parser was supplied.
    let Some(parser) = world
        .get_resource::<crate::source_parser::RuntimeParser>()
        .map(|p| p.0.clone())
    else {
        return;
    };
    let Some(state) = world.get_resource::<HotReloadState>() else {
        return;
    };
    let html_now = mtime(&state.html_path);
    let css_now = mtime(&state.css_path);
    let script_now: Vec<Option<SystemTime>> = state.script_paths.iter().map(|p| mtime(p)).collect();
    let include_now: Vec<Option<SystemTime>> =
        state.include_paths.iter().map(|p| mtime(p)).collect();
    let css_import_now: Vec<Option<SystemTime>> =
        state.css_import_paths.iter().map(|p| mtime(p)).collect();
    let locale_now = locale_stamps(&state.dir);
    if html_now == state.html_mtime
        && css_now == state.css_mtime
        && script_now == state.script_mtimes
        && include_now == state.include_mtimes
        && css_import_now == state.css_import_mtimes
        && locale_now == state.locale_stamps
    {
        return;
    }
    let dir = state.dir.clone();
    let html_path = state.html_path.clone();
    let css_path = state.css_path.clone();
    let asset_roots = state.asset_roots.clone();
    let skin_override = state.skin_override.clone();
    let old_root = state.root;

    // File-based pages: reuse the boot-time plan (page set + entry) so a hot
    // edit re-assembles the multi-page tree. Editing any page file reloads;
    // adding/removing page files needs a restart (the plan is fixed at boot).
    let page_plan = world.get_resource::<crate::pages::PagePlan>().cloned();

    // Re-apply against the live OS theme / viewport so a hot edit lands
    // with the same context the running window is showing.
    let media = media_context_from_world(world);
    let LoadResult {
        ir,
        html_mtime,
        css_mtime,
        script_paths,
        script_mtimes,
        include_paths,
        include_mtimes,
        css_import_paths,
        css_import_mtimes,
    } = match load_ir(
        &*parser,
        &html_path,
        &css_path,
        &dir,
        &asset_roots,
        skin_override.as_deref(),
        &media,
        SourceOverrides {
            plan: page_plan.as_ref(),
            ..SourceOverrides::default()
        },
    ) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("hot-reload: {e}");
            eprintln!("lumenc {msg}");
            if let Some(mut banner) = world.get_resource_mut::<ErrorBanner>() {
                banner.0 = Some(msg);
            }
            if let Some(mut s) = world.get_resource_mut::<HotReloadState>() {
                s.html_mtime = html_now;
                s.css_mtime = css_now;
                s.script_mtimes = script_now;
                s.include_mtimes = include_now;
                s.css_import_mtimes = css_import_now;
                s.locale_stamps = locale_now;
            }
            return;
        }
    };

    // Re-read the translation catalogues before the respawn below, so
    // `translatable="key"` resolves against the edited strings.
    reload_catalogues(world, &dir);

    // Stable-identity state preservation: snapshot per-LumenId state
    // BEFORE we despawn the old tree, then re-apply after spawn so
    // input cursors, toggle / slider values, and scroll offsets
    // survive a markup edit. Resources (Signals, ArraySignals,
    // FocusTracker, ...) survive automatically - they're world-level,
    // not entity-level.
    let preserved = snapshot_stateful_components(world);
    world.entity_mut(old_root).despawn();
    // Refresh the live stylesheet so the theme / media re-resolver
    // cascades against the just-edited CSS.
    if let Some(sheet) = ir.combined_stylesheet.clone() {
        world.insert_resource(RuntimeStylesheet(sheet));
    }
    use crate::spawn::SpawnIntoWorld;
    let new_root = ir.spawn_into(world);
    restore_stateful_components(world, &preserved);

    {
        // `ScriptHost::replace` preserves the engine, scope, and the
        // signal mirror, so signal values survive across edit-save
        // cycles. Handlers and derivations re-register from the new
        // body; compile-first + full rollback on eval failure keeps the
        // live host usable on the old source. No-op when no script host
        // is installed.
        let combined = combined_script_source(&ir, &dir).unwrap_or_default();
        if !combined.trim().is_empty()
            && let Some(Err(e)) = reload_script::<H>(world, &combined, "<inline>")
        {
            eprintln!("lumenc hot-reload: script load failed: {e}");
        }
    }

    if let Some(mut s) = world.get_resource_mut::<HotReloadState>() {
        s.root = new_root;
        s.html_mtime = html_mtime;
        s.css_mtime = css_mtime;
        s.script_paths = script_paths;
        s.script_mtimes = script_mtimes;
        s.include_paths = include_paths;
        s.include_mtimes = include_mtimes;
        s.css_import_paths = css_import_paths;
        s.css_import_mtimes = css_import_mtimes;
        s.locale_stamps = locale_now;
    }
    if let Some(mut banner) = world.get_resource_mut::<ErrorBanner>() {
        banner.0 = None;
    }
    eprintln!("lumenc: reloaded {}", html_path.display());
}

/// Per-entity state that survives a hot reload. Keyed by `LumenId.0`
/// in the snapshot map so the post-reload pass can find the matching
/// fresh entity by stable id and re-apply the values. Entities with
/// no `id="..."` attribute lose their state - there's no stable name
/// to match on. Authors who want input-cursor / scroll preservation
/// should tag every stateful element with `id="..."`.
#[cfg(feature = "runtime-parse")]
#[derive(Debug, Default, Clone)]
struct PreservedState {
    text_input: Option<lumen_core::components::TextInput>,
    text_content: Option<lumen_core::components::TextContent>,
    toggleable: Option<lumen_core::components::Toggleable>,
    slider_value: Option<lumen_core::components::SliderValue>,
    scroll_offset: Option<lumen_core::input::ScrollOffset>,
    /// Pre-respawn [`Opacity`] value. Drives a `Transition<f32>` from old to new when the entity carries a matching `TransitionSpec`. `None` means the entity had no [`Opacity`] component.
    opacity: Option<f32>,
}

#[cfg(feature = "runtime-parse")]
fn snapshot_stateful_components(
    world: &mut World,
) -> std::collections::HashMap<String, PreservedState> {
    use lumen_core::components::{
        LumenId, Opacity, SliderValue, TextContent, TextInput, Toggleable,
    };
    use lumen_core::input::ScrollOffset;
    #[allow(clippy::type_complexity)]
    let mut q = world.query::<(
        &LumenId,
        Option<&TextInput>,
        Option<&TextContent>,
        Option<&Toggleable>,
        Option<&SliderValue>,
        Option<&ScrollOffset>,
        Option<&Opacity>,
    )>();
    let mut out = std::collections::HashMap::new();
    for (id, ti, tc, tg, sl, so, op) in q.iter(world) {
        out.insert(
            id.0.clone(),
            PreservedState {
                text_input: ti.cloned(),
                text_content: tc.cloned(),
                toggleable: tg.copied(),
                slider_value: sl.copied(),
                scroll_offset: so.copied(),
                opacity: op.map(|o| o.0),
            },
        );
    }
    out
}

#[cfg(feature = "runtime-parse")]
fn restore_stateful_components(
    world: &mut World,
    preserved: &std::collections::HashMap<String, PreservedState>,
) {
    use lumen_core::components::{LumenId, Opacity};
    use lumen_primitives::{
        Easing, OpacityTransition, Transition, TransitionProperty, TransitionSpecs,
    };
    if preserved.is_empty() {
        return;
    }
    #[allow(clippy::type_complexity)]
    let mut targets: Vec<(Entity, PreservedState, Option<f32>, Option<TransitionSpecs>)> =
        Vec::new();
    let mut q = world.query::<(Entity, &LumenId, Option<&Opacity>, Option<&TransitionSpecs>)>();
    for (entity, id, op, specs) in q.iter(world) {
        if let Some(state) = preserved.get(&id.0) {
            targets.push((entity, state.clone(), op.map(|o| o.0), specs.cloned()));
        }
    }
    for (entity, state, new_opacity, specs) in targets {
        let mut ent = world.entity_mut(entity);
        if let Some(ti) = state.text_input {
            ent.insert(ti);
        }
        if let Some(tc) = state.text_content {
            ent.insert(tc);
        }
        if let Some(tg) = state.toggleable {
            ent.insert(tg);
        }
        if let Some(sl) = state.slider_value {
            ent.insert(sl);
        }
        if let Some(so) = state.scroll_offset {
            ent.insert(so);
        }
        // When an opacity transition is declared and the value changed across the respawn, attach a [`Transition<f32>`] driving [`Opacity`] from old to new.
        // Reset the immediate [`Opacity`] to the old value so the animation starts there; the
        // driver in `lumen-primitives::step_opacity_transitions` advances
        // it back up to `new_opacity` over `spec.duration`.
        if let (Some(old), Some(new), Some(specs)) = (state.opacity, new_opacity, specs.as_ref())
            && let Some(spec) = specs.for_property(TransitionProperty::Opacity)
            && (old - new).abs() > f32::EPSILON
        {
            let easing = if matches!(spec.easing, Easing::Linear) {
                Easing::Linear
            } else {
                spec.easing
            };
            let tween = Transition::<f32>::new(old, new, spec.duration, easing);
            ent.insert(Opacity(old));
            ent.insert(OpacityTransition(tween));
        }
    }
}
