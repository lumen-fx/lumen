//! App builder and [`Plugin`] trait. Wraps [`bevy_ecs::World`] and [`Schedule`] directly without depending on `bevy_app`.
//!
//! ## Two-world architecture
//!
//! [`App`] holds a **main world** for app/UI state and a **render world** for per-frame extracted draw data and GPU resources.
//! Cross-world flow is documented in [`crate::render_world`].
//!
//! ## Tick
//!
//! Each call to [`App::tick`] runs:
//!
//! 1. [`Tick`](crate::tick::Tick) `advance()`, bumping the frame counter and `dt`.
//! 2. The main schedule, ordered `Input -> CommandDrain -> Systems -> LayoutSync -> A11ySync`.
//! 3. [`clear_extracted`] on the render world to remove transient `Extracted*` entities.
//! 4. Every registered [`ExtractFn`] against `(&main, &mut render)`.
//! 5. The render schedule on the render world, ordered `Prepare -> Render`.

use crate::command::{Command, CommandQueue, CommandReceiver, CommandRegistry};
use crate::input::{
    ClickEvent, CloseRequest, DoubleClickEvent, DragEndEvent, DragMoveEvent, DragStartEvent,
    FileDropped, FileHoverCancelled, FileHovered, FilePicked, FocusTracker, FocusedKey,
    HotkeyFired, ImeEvent, ImeRequest, KeyPressed, KeyReleased, LongPressEvent, MenuClicked,
    ModifiersState, MouseWheel, PendingFileDrops, PointerLeft, PointerMoved, PointerPressed,
    PointerReleased, PointerState, ShowContextMenu, TextInputCommitted, TrayClicked,
};
use crate::node_ir::{PreviousScene, RetainedScene, transform_extracted_to_nodes};
use crate::property_store::PropertyStore;
use crate::render_world::{
    ExtractFn, ExtractSchedule, ExtractSet, FrameDamage, FrameDirty, HiddenExtracts, Render,
    RenderStage, Viewport, clear_extracted, cull_hidden, cull_offscreen, extract_borders,
    extract_clips, extract_rects, extract_scrollbars, extract_shadows, extract_text,
    roll_up_frame_dirty, stash_hidden_entities,
};
use crate::tick::TickStage;
use bevy_ecs::message::{Message, MessageRegistry};
use bevy_ecs::prelude::*;
use bevy_ecs::schedule::ScheduleLabel;
use bevy_ecs::system::ScheduleSystem;
use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Schedule label for the main tick.
#[derive(ScheduleLabel, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Tick;

/// Cross-thread handle used to wake a parked platform event loop after
/// something is pushed onto a resource the tick loop doesn't otherwise
/// observe until the next OS event - e.g. `lumen-mcp`'s `SimulateQueue`
/// filling from the MCP server thread while `lumen-window-winit`'s winit
/// loop sits parked in `about_to_wait`. Without a wakeup, injected input
/// is invisible until an unrelated OS event (mouse move, resize, ...)
/// happens to tick the app.
///
/// Backends that run a real OS event loop insert this as a main-world
/// resource once they have a way to interrupt their own park/wait call
/// (`lumen-window-winit::run` does it via a `winit::event_loop::EventLoopProxy`).
/// Headless/test contexts simply never insert it, so callers must treat
/// its absence as "no loop to wake" and no-op.
#[derive(Clone, Resource)]
pub struct EventLoopWaker(pub std::sync::Arc<dyn Fn() + Send + Sync>);

impl EventLoopWaker {
    /// Invoke the wakeup callback.
    pub fn wake(&self) {
        (self.0)()
    }
}

impl std::fmt::Debug for EventLoopWaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EventLoopWaker").finish()
    }
}

/// Process-start reference instant for startup instrumentation.
///
/// Set once by the binary entry point ([`lumenc`'s `main`]) as early as
/// reachable; read by the windowed backend to time exec->first-frame for
/// the `LUMEN_BOOT_TRACE` startup marker (the same measurement the
/// headless boot-trace prints). Absent in embedders that never call
/// [`mark_process_start`], in which case first-frame timing is simply not
/// reported and the marker carries no `startup_ms:`.
static PROCESS_START: OnceLock<std::time::Instant> = OnceLock::new();

/// Record the process-start instant. Idempotent - only the first call
/// wins, so calling it as the first statement of `main` captures the
/// earliest reachable moment. A no-op if already set.
pub fn mark_process_start() {
    let _ = PROCESS_START.set(std::time::Instant::now());
}

/// The process-start instant recorded by [`mark_process_start`], if any.
pub fn process_start() -> Option<std::time::Instant> {
    PROCESS_START.get().copied()
}

/// Plugin trait registered via [`App::add_plugin`].
///
/// `build` consumes `self` so the implementor can move non-clone payloads (text shapers, async runtimes, sockets) into the world.
pub trait Plugin: Sized {
    /// Returns the plugin's name for diagnostics; defaults to the type name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Returns the list of plugin names this plugin depends on. Default: empty.
    ///
    /// [`App::add_plugin`] checks each name against the already-installed set and returns [`AppError::PluginCycle`]
    /// (or [`AppError::MissingDependency`]) before the build runs, catching "X registered Y's resource before Y was
    /// installed today" at insertion time. Wave 1 wires the actual topological sort over a deferred plugin queue.
    fn depends_on(&self) -> &'static [&'static str] {
        &[]
    }

    /// Registers systems and resources on `app`, consuming the plugin.
    fn build(self, app: &mut App);

    /// Optional teardown invoked on [`App::drop`]. Default no-op.
    ///
    /// async-tokio joins workers here; render backends release GPU resources.
    /// Reserved for wave 2 wiring - foundation only defines the hook.
    fn cleanup(&mut self, _app: &mut App) {}
}

/// Type-erased plugin metadata recorded by [`App::add_plugin`].
///
/// Carries the plugin's `name()` and `depends_on()` so `is_plugin_added` / topological queries can answer without re-
/// running the build closure.
pub trait PluginMetadata: Send + Sync {
    /// Plugin name (matches the corresponding [`Plugin::name`] return).
    fn name(&self) -> &'static str;
    /// Declared dependencies (matches [`Plugin::depends_on`]).
    fn depends_on(&self) -> &'static [&'static str];
    /// Concrete plugin type id, for [`App::is_plugin_added`].
    fn type_id(&self) -> TypeId;
}

struct PluginInfo {
    name: &'static str,
    deps: &'static [&'static str],
    type_id: TypeId,
}

impl PluginMetadata for PluginInfo {
    fn name(&self) -> &'static str {
        self.name
    }
    fn depends_on(&self) -> &'static [&'static str] {
        self.deps
    }
    fn type_id(&self) -> TypeId {
        self.type_id
    }
}

/// Errors returned by builder methods that can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// A plugin's `depends_on()` listed a plugin that has not been added yet.
    MissingDependency {
        /// Plugin attempting to install.
        plugin: &'static str,
        /// Missing dependency name.
        missing: &'static str,
    },
    /// Topological sort detected a cycle among installed plugins.
    PluginCycle {
        /// One plugin participating in the detected cycle.
        plugin: &'static str,
    },
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::MissingDependency { plugin, missing } => {
                write!(
                    f,
                    "plugin `{plugin}` depends on `{missing}`, which has not been added"
                )
            }
            AppError::PluginCycle { plugin } => {
                write!(f, "plugin dependency cycle detected at `{plugin}`")
            }
        }
    }
}

impl std::error::Error for AppError {}

/// Lumen application holding the main world, render world, their schedules, and the extract-fn list.
pub struct App {
    /// Main world carrying app/UI state, layout, and scripts.
    pub world: World,
    /// Render world carrying per-frame extracted draw data and GPU resources.
    pub render_world: World,
    /// Extract fns invoked in registration order each tick, after the main schedule and before the render schedule.
    /// Plugins may push, replace, or swap entries (for example, [`crate::render_world::extract_rects`] is swapped out by scroll / mask / transform plugins that need to alter [`crate::render_world::ExtractedRect`] origins).
    pub extract_fns: Vec<ExtractFn>,
    /// Worker-thread budget for the `bevy_ecs` multithreaded executor.
    ///
    /// - Defaults to [`LUMEN_DEFAULT_THREADS`] (4).
    /// - Plugins raise it monotonically via [`Self::request_threads_at_least`].
    /// - The `LUMEN_THREADS` environment variable overrides any plugin request.
    /// - Read at the first [`Self::tick`]; the task pool is initialised once via `ComputeTaskPool::get_or_init` and subsequent updates have no effect.
    pub desired_threads: usize,
    /// Plugin name -> metadata. Populated by [`Self::add_plugin`] in installation order.
    pub installed_plugins: HashMap<&'static str, Box<dyn PluginMetadata>>,
    /// Concrete plugin type ids that have been added. Consulted by [`Self::is_plugin_added`].
    installed_plugin_types: HashSet<TypeId>,
}

/// Upper bound on the default worker count for the bevy_ecs task pool when
/// no plugin or env var raises it. One worker per main-stage band
/// (input / systems / layout / render). The effective default is
/// [`default_thread_budget`] = `min(available_parallelism, LUMEN_DEFAULT_THREADS)`,
/// so a 24-core box does not spawn a 24-wide pool for a UI that never
/// saturates four workers.
pub const LUMEN_DEFAULT_THREADS: usize = 4;

/// Effective default worker budget: [`LUMEN_DEFAULT_THREADS`] capped to the
/// machine's available parallelism. Falls back to 1 when parallelism is
/// unknown. The `LUMEN_THREADS` env var (read in [`ensure_task_pool`]) and
/// `lumen.toml [runtime] threads` still override this.
pub fn default_thread_budget() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    LUMEN_DEFAULT_THREADS.min(cores).max(1)
}

/// Marks whether the global `bevy_tasks` compute pool has been initialised and short-circuits the env-var lookup on subsequent calls.
static TASK_POOL_INITIALISED: OnceLock<usize> = OnceLock::new();

/// Initialises the global `bevy_tasks` compute pool with `desired` workers (or the `LUMEN_THREADS` env var override).
/// No-ops after the first call; the pool is global and single-init.
fn ensure_task_pool(desired: usize) {
    TASK_POOL_INITIALISED.get_or_init(|| {
        let n = std::env::var("LUMEN_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(desired)
            .max(1);
        bevy_tasks::ComputeTaskPool::get_or_init(|| {
            bevy_tasks::TaskPoolBuilder::new()
                .num_threads(n)
                .thread_name("lumen-worker".into())
                .build()
        });
        n
    });
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Constructs a fresh app with both worlds, both schedules, the command queue, and the default extract fns.
    pub fn new() -> Self {
        let mut world = World::new();

        // Install the command queue resources on the main world.
        let (queue, receiver) = CommandQueue::new();
        world.insert_resource(queue);
        world.insert_resource(receiver);
        // Strongly-typed custom command registry (see `App::register_command`).
        world.insert_resource(CommandRegistry::default());
        // Foundation property store; legacy `Signals` writes mirror here via `mirror_signals_to_property_store`.
        world.insert_resource(PropertyStore::default());
        // Per-tick frame clock; updated by `App::tick` before each main schedule run.
        world.insert_resource(crate::tick::Tick::default());
        // Insert `Viewport` into the main world; the window plugin mirrors writes into the render world on resize.
        world.insert_resource(Viewport::default());
        // Insert `FrameDirty`; defaults to dirty so the first frame paints. The window backend reads and clears it per presented frame.
        world.insert_resource(FrameDirty::default());
        // Per-tick "an animation is still moving" flag. Reset at the top
        // of every tick (below) and re-raised by animation drivers while
        // they still have motion, so the window backend can self-schedule
        // follow-up frames without spinning at idle.
        world.insert_resource(crate::render_world::AnimationsActive::default());
        // Per-extract-phase memo of the hierarchy-derived maps the extract
        // fns would otherwise each rebuild identically (parent map, scroll
        // offsets, opacities, hidden set, clip rects). Populated by the
        // first extractor of a frame and reused by the rest; see
        // [`crate::render_world::ExtractContextCache`].
        world.insert_resource(crate::render_world::ExtractContextCache::default());
        world.insert_resource(PointerState::default());
        world.insert_resource(crate::input::ScrollbarInteraction::default());
        world.insert_resource(ModifiersState::default());
        world.insert_resource(FocusTracker::default());
        world.insert_resource(ImeRequest::default());
        world.insert_resource(PendingFileDrops::default());

        // Register pointer, keyboard, drag, IME, file-drop, and system messages so producers and consumers can use `MessageWriter` and `MessageReader`.
        MessageRegistry::register_message::<PointerMoved>(&mut world);
        MessageRegistry::register_message::<PointerPressed>(&mut world);
        MessageRegistry::register_message::<PointerReleased>(&mut world);
        MessageRegistry::register_message::<PointerLeft>(&mut world);
        MessageRegistry::register_message::<ClickEvent>(&mut world);
        MessageRegistry::register_message::<KeyPressed>(&mut world);
        MessageRegistry::register_message::<KeyReleased>(&mut world);
        MessageRegistry::register_message::<FocusedKey>(&mut world);
        MessageRegistry::register_message::<MouseWheel>(&mut world);
        MessageRegistry::register_message::<LongPressEvent>(&mut world);
        MessageRegistry::register_message::<DoubleClickEvent>(&mut world);
        MessageRegistry::register_message::<DragStartEvent>(&mut world);
        MessageRegistry::register_message::<DragMoveEvent>(&mut world);
        MessageRegistry::register_message::<DragEndEvent>(&mut world);
        MessageRegistry::register_message::<ImeEvent>(&mut world);
        MessageRegistry::register_message::<TextInputCommitted>(&mut world);
        MessageRegistry::register_message::<FileHovered>(&mut world);
        MessageRegistry::register_message::<FileHoverCancelled>(&mut world);
        MessageRegistry::register_message::<FileDropped>(&mut world);
        MessageRegistry::register_message::<FilePicked>(&mut world);
        MessageRegistry::register_message::<HotkeyFired>(&mut world);
        MessageRegistry::register_message::<MenuClicked>(&mut world);
        MessageRegistry::register_message::<crate::input::DialogClosed>(&mut world);
        MessageRegistry::register_message::<TrayClicked>(&mut world);
        MessageRegistry::register_message::<ShowContextMenu>(&mut world);
        // Close-request bus. Registered here (not only by the window
        // backend plugin) so app-level close hooks - the script host's
        // `on_close` dispatcher, the C-ABI `lumen_app_on_close` router,
        // and SDK systems - can rely on the resource existing in every
        // context, including headless runs that never install a window
        // backend. Window backends write `CloseRequest { vetoed: false }`
        // on an OS close request (window button, SIGINT/SIGTERM); a
        // system that wants to keep the window open writes a fresh
        // `CloseRequest { vetoed: true }` on the same tick.
        MessageRegistry::register_message::<CloseRequest>(&mut world);

        // Install the main `Tick` schedule with a fixed five-stage ordering.
        let mut schedule = Schedule::new(Tick);
        schedule.configure_sets(
            (
                TickStage::Input,
                TickStage::CommandDrain,
                TickStage::Systems,
                TickStage::LayoutSync,
                TickStage::A11ySync,
            )
                .chain(),
        );
        world.add_schedule(schedule);

        // Build the render world.
        let mut render_world = World::new();
        render_world.insert_resource(Viewport::default());
        // Per-frame damage list - foundation only installs the resource; wave 1.5 / wave 2 fill and consume it.
        render_world.insert_resource(FrameDamage::default());
        // Insert the persistent main->render entity registry consulted by upserting extract fns.
        // [`clear_extracted`] skips entities present in this map so their identities survive across frames.
        render_world.insert_resource(crate::render_world::RenderEntityMap::default());
        // W2.1 retained Node IR - produced by `transform_extracted_to_nodes` in `RenderStage::Prepare`,
        // consumed by the back-end walker in `RenderStage::Render`.
        render_world.insert_resource(RetainedScene::default());
        render_world.insert_resource(PreviousScene::default());
        // Snapshot of hidden main entities, refreshed each extract phase by
        // `stash_hidden_entities` and consumed by the `cull_hidden` guard.
        render_world.insert_resource(HiddenExtracts::default());

        // Install the render schedule ordered `Prepare -> Render`.
        let mut render_schedule = Schedule::new(Render);
        render_schedule.configure_sets((RenderStage::Prepare, RenderStage::Render).chain());
        render_world.add_schedule(render_schedule);

        // Install the dedicated extract schedule with a single `Extract` set. Wave 2 migrates the legacy
        // `extract_fns` list onto this schedule.
        let mut extract_schedule = Schedule::new(ExtractSchedule);
        extract_schedule.configure_sets((ExtractSet::Extract,));
        render_world.add_schedule(extract_schedule);

        let mut s = Self {
            world,
            render_world,
            extract_fns: vec![
                // Runs first so it primes the shared hierarchy memos and so
                // `HiddenExtracts` is fresh for the `cull_hidden` guard.
                stash_hidden_entities,
                extract_shadows,
                extract_rects,
                extract_borders,
                extract_text,
                extract_clips,
                extract_scrollbars,
            ],
            desired_threads: default_thread_budget(),
            installed_plugins: HashMap::new(),
            installed_plugin_types: HashSet::new(),
        };
        // Cycle message buffers at the start of `Input` each tick.
        s.add_systems(TickStage::Input, bevy_ecs::message::message_update_system);
        // Clear the per-tick `AnimationsActive` flag before any animation
        // driver runs (Input is chained before Systems). Drivers re-raise
        // it while they still have motion; the window backend reads it
        // after the tick to re-arm the redraw for the next frame.
        s.add_systems(
            TickStage::Input,
            crate::render_world::reset_animations_active,
        );
        // Run [`roll_up_frame_dirty`] in `A11ySync` (the last main-world stage before extract) to fold render-relevant `Changed<T>` filters into [`FrameDirty`].
        s.add_systems(TickStage::A11ySync, roll_up_frame_dirty);
        // Wave-D dirty-queue lifecycle. `clear_signal_dirty` keeps the legacy
        // `Signals::dirty` set tidy for embedders that still hold a `Res<Signals>`
        // reference; `clear_property_store_dirty` runs against the canonical
        // typed queue so derivation systems and the theme propagation consumer
        // observe in-tick `set()` calls before the next tick starts with a
        // clean dirty set.
        s.add_systems(TickStage::A11ySync, crate::signals::clear_signal_dirty);
        s.add_systems(
            TickStage::A11ySync,
            crate::property_store::clear_property_store_dirty,
        );
        // Wave-D back-mirror: pre wave-D systems wrote into `Signals` which
        // mirrored forward into `PropertyStore`. Post wave-D internal systems
        // write directly to `PropertyStore`, so we run the mirror in the
        // reverse direction - copy every dirty global `Str` cell into the
        // legacy `Signals` map so embedders that still call
        // `Res<Signals>.get(...)` keep observing the latest value. Registered
        // in `Systems` after the property bus drain so this tick's writes
        // are observable.
        crate::property_store::init_external_properties();
        s.add_systems(
            TickStage::CommandDrain,
            crate::property_store::drain_external_properties,
        );
        s.add_systems(
            TickStage::Systems,
            crate::signals::mirror_property_store_globals_to_signals,
        );
        // Insert [`crate::components::StyleManager`] (the W4.6 rename of the legacy
        // `OsTheme` resource - now exposing the 5-state AdwColorScheme model:
        // Default / ForceLight / ForceDark / PreferLight / PreferDark) and register
        // the W1.6 split: [`crate::signals::style_manager_to_signal`] (producer)
        // writes `"dark"`/`"light"` into `Signals["__theme__"]` from
        // `StyleManager::effective_dark`, the existing
        // [`mirror_signals_to_property_store`] pushes the write into [`PropertyStore`]
        // keyed on `PropertyKey::Global("__theme__")`, and
        // [`apply_theme_signal_to_root_classes`] (consumer) updates root
        // [`crate::components::LumenClasses`] only when the notify queue carries a
        // `__theme__` write. Replaces the legacy [`apply_theme_class_to_root`]
        // mutex-dance system.
        s.world
            .insert_resource(crate::components::StyleManager::default());
        s.add_systems(TickStage::Systems, crate::signals::style_manager_to_signal);
        s.add_systems(
            TickStage::Systems,
            crate::signals::apply_theme_signal_to_root_classes,
        );
        // W5.4 - install the [`DefaultLayoutDirection`] resource (Ltr
        // by default; the i18n plugin overrides it from the detected
        // system locale) and register [`resolve_layout_direction`] in
        // `LayoutSync` so every entity has a fresh [`ResolvedDirection`]
        // before the layout backend reads it.
        s.world
            .insert_resource(crate::components::DefaultLayoutDirection::default());
        s.add_systems(
            TickStage::LayoutSync,
            crate::components::resolve_layout_direction,
        );
        // Register [`cull_offscreen`] in `RenderStage::Prepare` to drop extracted entities outside the viewport before render.
        s.add_render_systems(RenderStage::Prepare, cull_offscreen);
        // Suppress any extracted entity whose main entity is hidden by a
        // `Visible(false)` on itself or an ancestor - the general guarantee
        // that a hidden subtree paints nothing, behind the per-extractor
        // `hidden_entities` filters.
        s.add_render_systems(RenderStage::Prepare, cull_hidden);
        // W2.1 - build the retained Node IR each frame from the flat Extracted* bag.
        // Runs after `cull_offscreen` / `cull_hidden` so culled leaves never reach the tree.
        s.add_render_systems(
            RenderStage::Prepare,
            transform_extracted_to_nodes
                .after(cull_offscreen)
                .after(cull_hidden),
        );
        s
    }

    /// Registers a message type with [`MessageRegistry`] so [`bevy_ecs::message::MessageWriter`] and [`bevy_ecs::message::MessageReader`] can be used for `M`.
    pub fn add_message<M: Message>(&mut self) -> &mut Self {
        MessageRegistry::register_message::<M>(&mut self.world);
        self
    }

    /// Calls `plugin.build(self)`, consuming the plugin.
    ///
    /// Records the plugin's metadata in [`Self::installed_plugins`] so [`Self::is_plugin_added`] and other queries can
    /// answer without re-running the build closure. Logs (but does not panic) when a declared `depends_on` entry has
    /// not been installed yet - wave 1 wires the topological sort that would defer the build instead.
    pub fn add_plugin<P: Plugin + 'static>(&mut self, plugin: P) -> &mut Self {
        let name = plugin.name();
        let deps = plugin.depends_on();
        for dep in deps {
            if !self.installed_plugins.contains_key(*dep) {
                // Foundation only logs - wave 1 will fold this into the topo-sort + AppError::MissingDependency error
                // surface. Today's plugin chains add in correct order already, so missing deps are real bugs.
                eprintln!(
                    "[lumen-core] plugin `{name}` declares dependency on `{dep}` which has not been added yet"
                );
            }
        }
        let type_id = TypeId::of::<P>();
        plugin.build(self);
        self.installed_plugin_types.insert(type_id);
        self.installed_plugins.insert(
            name,
            Box::new(PluginInfo {
                name,
                deps,
                type_id,
            }),
        );
        self
    }

    /// Returns `true` when a plugin of type `P` has been installed via [`Self::add_plugin`].
    pub fn is_plugin_added<P: Plugin + 'static>(&self) -> bool {
        self.installed_plugin_types.contains(&TypeId::of::<P>())
    }

    /// Returns `true` when a plugin with the supplied [`Plugin::name`] has been installed.
    pub fn plugin_added(&self, name: &str) -> bool {
        self.installed_plugins.contains_key(name)
    }

    /// Registers a strongly-typed handler invoked when a [`Command::Typed`] payload of type `T` is drained.
    ///
    /// Replaces the legacy blind `Command::Custom(Box<dyn Any>)` downcast pattern: producers build
    /// `Command::Typed { type_id: TypeId::of::<T>(), payload: Box::new(value) }`, the drain looks up the handler by
    /// type id and invokes it with the typed payload.
    pub fn register_command<T, F>(&mut self, handler: F) -> &mut Self
    where
        T: Any + Send,
        F: Fn(&mut World, Box<T>) + Send + Sync + 'static,
    {
        let mut registry = self.world.resource_mut::<CommandRegistry>();
        registry.register::<T, F>(handler);
        self
    }

    /// Adds main-world systems into the `Tick` schedule under the given [`TickStage`] set.
    pub fn add_systems<M>(
        &mut self,
        stage: TickStage,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        let mut schedules = self.world.resource_mut::<Schedules>();
        let schedule = schedules
            .get_mut(Tick)
            .expect("Tick schedule should be installed by App::new");
        schedule.add_systems(systems.in_set(stage));
        self
    }

    /// Adds render-world systems into the `Render` schedule under the given [`RenderStage`] set.
    pub fn add_render_systems<M>(
        &mut self,
        stage: RenderStage,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        let mut schedules = self.render_world.resource_mut::<Schedules>();
        let schedule = schedules
            .get_mut(Render)
            .expect("Render schedule should be installed by App::new");
        schedule.add_systems(systems.in_set(stage));
        self
    }

    /// Adds render-world systems into the dedicated [`ExtractSchedule`] under the given [`ExtractSet`].
    ///
    /// Foundation only installs the schedule; the legacy [`Self::extract_fns`] list keeps providing cross-world data.
    /// Wave 2 migrates the existing extractors onto this schedule.
    pub fn add_extract_systems<M>(
        &mut self,
        set: ExtractSet,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        let mut schedules = self.render_world.resource_mut::<Schedules>();
        let schedule = schedules
            .get_mut(ExtractSchedule)
            .expect("ExtractSchedule should be installed by App::new");
        schedule.add_systems(systems.in_set(set));
        self
    }

    /// Appends an extract fn to [`Self::extract_fns`].
    pub fn add_extract_fn(&mut self, f: ExtractFn) -> &mut Self {
        self.extract_fns.push(f);
        self
    }

    /// Raises [`Self::desired_threads`] to `n` if it is currently lower (monotonic max across plugins).
    /// Takes effect at the first [`Self::tick`]; the `LUMEN_THREADS` env var overrides the value.
    pub fn request_threads_at_least(&mut self, n: usize) -> &mut Self {
        if n > self.desired_threads {
            self.desired_threads = n;
        }
        self
    }

    /// Runs one tick: advance the [`crate::tick::Tick`] resource, run the main schedule, then (when [`FrameDirty`] is
    /// set) `clear_extracted`, every extract fn, the extract schedule, and the render schedule.
    /// When [`FrameDirty`] is unset, returns after the main schedule; the previous frame's extracted entities remain in
    /// the render world for the backend's next `RedrawRequested`.
    pub fn tick(&mut self) {
        ensure_task_pool(self.desired_threads);
        if let Some(mut tick) = self.world.get_resource_mut::<crate::tick::Tick>() {
            tick.advance();
        }
        self.world.run_schedule(Tick);

        // Rotate the main world's removal/despawn event buffers once per tick.
        // Standalone bevy_ecs (no bevy_app) never rotates `RemovedComponentEvents`
        // on its own; without this every `Hovered`/`Pressed`/`Focused`/`ChildOf`/
        // `Style` removal accumulates forever. Runs EVERY tick (the main world
        // advances regardless of `FrameDirty`) and AFTER `run_schedule(Tick)`, so
        // all main-world `RemovedComponents` readers - `roll_up_frame_dirty`
        // (A11ySync), the taffy free-node sweeps, `sync_removed_direction`,
        // `BindScroll` cleanup - have already observed this tick's removals inside
        // the schedule. bevy's double-buffered `Events` still retains this tick's
        // and last tick's removals after `update()`, so nothing a same-tick reader
        // needed is dropped. Change detection is untouched: schedule systems track
        // their own per-system `last_run`, not `world.last_change_tick`.
        self.world.clear_trackers();

        let dirty = self
            .world
            .get_resource::<FrameDirty>()
            .map(|f| f.dirty)
            .unwrap_or(true);
        if !dirty {
            return;
        }

        clear_extracted(&mut self.render_world);
        // Open the extract phase so the hierarchy-derived maps
        // ([`build_parent_map`] & friends) are computed once by the first
        // extract fn and cloned back by the rest, instead of each of the
        // six extractors rebuilding them. Strictly scoped to this loop:
        // `end_phase` disables reuse before the render schedules run and
        // before the next tick's Systems-stage callers (hover hit-testing)
        // reach the same helpers, so no stale hierarchy can leak out.
        if let Some(mut c) = self
            .world
            .get_resource_mut::<crate::render_world::ExtractContextCache>()
        {
            c.begin_phase();
        }
        // Clone the fn-pointer vec to release the immutable borrow on `self.extract_fns` before re-borrowing `self.world` mutably.
        let fns = self.extract_fns.clone();
        for f in fns {
            f(&mut self.world, &mut self.render_world);
        }
        if let Some(mut c) = self
            .world
            .get_resource_mut::<crate::render_world::ExtractContextCache>()
        {
            c.end_phase();
        }

        // Run extract systems registered via `add_extract_systems`. These read already-extracted render-world state
        // (e.g. `Changed<ExtractedText>` filters) and queue further render-world work. Foundation ships the schedule
        // empty; wave 2 wires migrations.
        self.render_world.run_schedule(ExtractSchedule);

        self.render_world.run_schedule(Render);

        // Rotate the render world's removal buffers. `clear_extracted` despawns
        // the entire transient `Extracted*` set every dirty frame, recording a
        // removal event for every component on each despawned entity; nothing
        // frees these without `update()`. The render world holds no
        // `RemovedComponents` readers, so there is no same-tick observation to
        // preserve here. Only reachable on dirty ticks (the render pass runs only
        // when dirty), which is exactly when the despawn churn happens.
        self.render_world.clear_trackers();
    }

    /// Borrows the [`CommandReceiver`] resource mutably from the main world.
    pub fn commands(&mut self) -> Mut<'_, CommandReceiver> {
        self.world.resource_mut::<CommandReceiver>()
    }

    /// Constructs a typed [`crate::property_store::Property`] handle bound to
    /// the global namespace. The cell is created lazily on the first `set` -
    /// this helper only mints the typed key wrapper, no allocations besides
    /// the shared `Arc<str>` for the name.
    ///
    /// Equivalent to `Property::<T>::new(name)`; lives on [`App`] so app-init
    /// code can read more like `let count = app.property::<i64>("count");`
    /// without a separate `use` for the prelude.
    pub fn property<T>(
        &self,
        name: impl Into<std::sync::Arc<str>>,
    ) -> crate::property_store::Property<T>
    where
        T: TryFrom<crate::property_store::PropertyValue>
            + Into<crate::property_store::PropertyValue>
            + Clone,
    {
        crate::property_store::Property::<T>::new(name)
    }
}

// Suppress unused-import warnings while the foundation's new types are wired downstream by wave 1.
#[allow(dead_code)]
fn _suppress_unused() {
    let _ = TypeId::of::<Command>();
}
