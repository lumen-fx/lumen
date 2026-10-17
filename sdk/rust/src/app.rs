//! The ECS-first [`App`] - Lumen's bevy-shaped entry point.

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::{bevy_ecs, lumen_core, lumen_runtime, lumenc};

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::{Error, Result};
use crate::events::{EventCtx, EventKind, HandlerEntry, install_rust_handlers};
use crate::frame::Frame;
use crate::plugins::{AppPlugins, BootConfig, WindowMode};
use crate::signal::Signals;
use bevy_ecs::message::{Message, MessageReader};
use bevy_ecs::prelude::{Res, ResMut, Resource};
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::ScheduleSystem;
use lumen_core::app::{App as EcsApp, Plugin};
use lumen_core::input::CloseRequest;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_core::render_world::{FrameDirty, SurfaceCapture};
use lumen_core::tick::TickStage;
use lumen_runtime::app_kind::HeadlessHandoff;
use lumen_runtime::{HeadlessOptions, PluginInstaller, RunOptions};

type Deferred = Box<dyn FnOnce(&mut EcsApp) + 'static>;

/// The lumenc [`RunOptions`] plus the seeds and deferred installers that
/// [`App`] threads into it as an app hook.
type RunSetup = (RunOptions, Vec<(PropertyKey, PropertyValue)>, Vec<Deferred>);

/// A Lumen application, assembled the way a bevy app is: add plugin groups, add
/// systems, then [`run`](Self::run).
///
/// ```no_run
/// # extern crate lumenui_engine as lumenui;
/// use lumenui::prelude::*;
///
/// fn bump(mut clicks: MessageReader<ClickEvent>, mut signals: Signals) {
///     if clicks.read().next().is_some() {
///         let n = signals.get_or::<i64>("count", 0) + 1;
///         signals.set("count", n);
///     }
/// }
///
/// # fn demo() -> lumenui::Result<()> {
/// lumenui::App::new()
///     .add_plugins(LumenDefaultPlugins.with_markup("<root/>"))
///     .add_systems(TickStage::Systems, bump.after(ClickSet::Producers))
///     .run()
/// # }
/// ```
///
/// User systems are *real* `bevy_ecs` systems: take `Query` / `Res` / `ResMut` /
/// `MessageReader` / [`Commands`](bevy_ecs::system::Commands) / [`Signals`](crate::Signals),
/// and they are scheduled into the live [`TickStage`] schedule next to the
/// framework's own systems. Nothing is stringly-typed. A system that reads
/// clicks orders itself after
/// [`ClickSet::Producers`](lumen_core::input::ClickSet::Producers), as `bump`
/// does: without the edge it may run before the click is written and see it a
/// tick late, on a tick that paints nothing.
#[derive(Default)]
pub struct App {
    boot: BootConfig,
    seeds: Vec<(PropertyKey, PropertyValue)>,
    plugins: Vec<PluginInstaller>,
    deferred: Vec<Deferred>,
    handlers: Vec<HandlerEntry>,
}

impl App {
    /// A fresh app with no plugins, systems, or UI source yet.
    pub fn new() -> Self {
        Self::default()
    }

    // -- Assembly ------------------------------------------------------------

    /// Add a plugin group - [`LumenDefaultPlugins`](crate::plugins::LumenDefaultPlugins),
    /// a configured builder, a user [`PluginGroupBuilder`](crate::plugins::PluginGroupBuilder),
    /// or a tuple of those. See [`AppPlugins`].
    #[must_use]
    pub fn add_plugins(mut self, plugins: impl AppPlugins) -> Self {
        plugins.apply(&mut self);
        self
    }

    /// Add a single [`Plugin`], installed on the ECS app before the markup is
    /// parsed and before the script hosts load.
    ///
    /// That phase is what lets a widget plugin (the one `#[derive(Widget)]`
    /// generates, say) register its tag in time for the markup that uses it,
    /// and what lets a plugin register script functions
    /// ([`ScriptFnAppExt::add_script_fn`](lumen_script::ScriptFnAppExt::add_script_fn)):
    /// candela binds its host declarations when the program compiles, so a
    /// registration after the load has nothing to bind to. The consequence to
    /// know: a plugin builds before this builder's own
    /// [`insert_resource`](Self::insert_resource) /
    /// [`add_systems`](Self::add_systems) run, so a plugin that reads a
    /// resource the builder inserts must do it from a system rather than from
    /// `build`.
    #[must_use]
    pub fn add_plugin<P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        self.plugins.push(Box::new(move |ecs: &mut EcsApp| {
            ecs.add_plugin(plugin);
        }));
        self
    }

    /// Add one or more systems (a single system or a tuple / `.chain()` of them)
    /// to a [`TickStage`]. Accepts the full `bevy_ecs` configuration surface -
    /// `.chain()`, `.after(..)`, `.run_if(..)`.
    ///
    /// ```
    /// # extern crate lumenui_engine as lumenui;
    /// use lumenui::prelude::*;
    /// fn a() {}
    /// fn b() {}
    /// # let app =
    /// lumenui::App::new().add_systems(TickStage::Systems, (a, b).chain());
    /// ```
    #[must_use]
    pub fn add_systems<M: 'static>(
        mut self,
        stage: TickStage,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M> + 'static,
    ) -> Self {
        self.push_deferred(move |ecs| {
            ecs.add_systems(stage, systems);
        });
        self
    }

    /// Insert a resource, available to every system.
    #[must_use]
    pub fn insert_resource<R: Resource>(mut self, resource: R) -> Self {
        self.push_deferred(move |ecs| {
            ecs.world.insert_resource(resource);
        });
        self
    }

    /// Register a custom [`Message`] type so systems can `MessageReader` /
    /// `MessageWriter` it. Lumen's own input messages are pre-registered.
    #[must_use]
    pub fn add_message<M: Message>(mut self) -> Self {
        self.push_deferred(|ecs| {
            ecs.add_message::<M>();
        });
        self
    }

    /// Seed a global signal before the first tick. `bind-*` markup observes it
    /// on the first frame.
    #[must_use]
    pub fn insert_signal(
        mut self,
        name: impl Into<String>,
        value: impl Into<PropertyValue>,
    ) -> Self {
        self.seeds
            .push((PropertyKey::global(name.into()), value.into()));
        self
    }

    // -- Terse native event handlers -----------------------------------------
    //
    // A closure-per-element surface over the same [`RustHandlers`] machinery
    // the [`crate::simple::AppBuilder`] uses, lifted onto the ECS-first `App`
    // so the common "click this id -> write that signal" case needs no
    // hand-written `MessageReader<ClickEvent>` + id-filter system. Reach for a
    // full [`add_systems`](Self::add_systems) system when you need queries,
    // resources, or the event stream; reach for these when you just want to
    // mutate signals in response to a named element.

    /// Register a click handler for the element with `id="..."`. The closure
    /// gets an [`EventCtx`] for typed signal access; writes it makes are
    /// reflected by `bind-*` markup on the same tick. A per-id handler
    /// overrides [`on_any_click`](Self::on_any_click) for its id.
    ///
    /// ```
    /// # extern crate lumenui_engine as lumenui;
    /// use lumenui::prelude::*;
    ///
    /// # let app =
    /// lumenui::App::new().on_click("bump", |ctx| {
    ///     let n = ctx.get_or::<i64>("count", 0) + 1;
    ///     ctx.set("count", n);
    /// });
    /// ```
    #[must_use]
    pub fn on_click<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::Click, Some(id.into()), handler)
    }

    /// Register a fallback click handler that fires for every element without
    /// a dedicated [`on_click`](Self::on_click). Inspect
    /// [`EventCtx::target`] to see which element was clicked.
    #[must_use]
    pub fn on_any_click<F>(self, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::Click, None, handler)
    }

    /// Register a double-click handler for the element with `id="..."`. On a
    /// double-click tick the plain click for the same element is suppressed
    /// (one double, not two clicks plus a double).
    #[must_use]
    pub fn on_double_click<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::DoubleClick, Some(id.into()), handler)
    }

    /// Register a long-press handler for the element with `id="..."`.
    #[must_use]
    pub fn on_long_press<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::LongPress, Some(id.into()), handler)
    }

    /// Register a handler for an arbitrary `(kind, id)` pair; a `None` id is
    /// the wildcard slot. The named `on_*` methods are sugar over this.
    #[must_use]
    pub fn on<F>(mut self, kind: EventKind, id: Option<String>, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.handlers.push((kind, id, Box::new(handler)));
        self
    }

    /// Register a *computed* signal: `output` is recomputed every tick from
    /// `f`, which reads other signals through the borrowed [`Signals`]. The
    /// recompute is ordered before the reactive binding readers, so a
    /// `bind-text="output"` label reflects the fresh value on the same tick
    /// its inputs change.
    ///
    /// This is the native-Rust analogue of the script `derive(...)` builtin
    /// and the Python SDK's `@computed`. Writes are change-gated by the
    /// [`PropertyStore`] (an unchanged recompute pushes nothing onto the
    /// dirty queue), so an idle computed costs one closure call per tick and
    /// never spams observers.
    ///
    /// ```
    /// # extern crate lumenui_engine as lumenui;
    /// use lumenui::prelude::*;
    ///
    /// # let app =
    /// lumenui::App::new()
    ///     .insert_signal("count", 0i64)
    ///     .add_computed("label", |s| format!("clicks: {}", s.get_or::<i64>("count", 0)));
    /// ```
    #[must_use]
    pub fn add_computed<T, F>(self, output: impl Into<String>, f: F) -> Self
    where
        T: Into<PropertyValue> + 'static,
        F: Fn(&Signals) -> T + Send + Sync + 'static,
    {
        let output = output.into();
        self.add_systems(
            TickStage::Systems,
            (move |mut signals: Signals| {
                let value = f(&signals);
                signals.set(&output, value);
            })
            .before(lumen_core::signals::apply_text_bindings)
            .before(lumen_core::signals::apply_checked_bindings)
            .before(lumen_core::signals::apply_value_bindings),
        )
    }

    // -- Boot plumbing used by the plugins module ----------------------------

    pub(crate) fn merge_boot(&mut self, boot: BootConfig) {
        let BootConfig {
            markup,
            css,
            dir,
            built_dir,
            title,
            size,
            hot_reload,
            mode,
            strip_script,
            disabled,
        } = boot;
        if markup.is_some() {
            self.boot.markup = markup;
        }
        if built_dir.is_some() {
            self.boot.built_dir = built_dir;
        }
        if css.is_some() {
            self.boot.css = css;
        }
        if dir.is_some() {
            self.boot.dir = dir;
        }
        if title.is_some() {
            self.boot.title = title;
        }
        if size.is_some() {
            self.boot.size = size;
        }
        if hot_reload.is_some() {
            self.boot.hot_reload = hot_reload;
        }
        if mode != WindowMode::default() {
            self.boot.mode = mode;
        }
        self.boot.strip_script |= strip_script;
        self.boot.disabled.extend(disabled);
    }

    pub(crate) fn push_deferred(&mut self, f: impl FnOnce(&mut EcsApp) + 'static) {
        self.deferred.push(Box::new(f));
    }

    pub(crate) fn push_plugin(&mut self, f: impl FnOnce(&mut EcsApp) + Send + 'static) {
        self.plugins.push(Box::new(f));
    }

    // -- Running -------------------------------------------------------------

    /// Boot the app and enter the winit event loop. Blocks until the window
    /// closes. Requires a UI source - normally
    /// [`with_source`](crate::plugins::LumenDefaultPlugins::with_source) +
    /// [`lumen_source!`](crate::lumen_source) (disk + hot reload in debug,
    /// embedded in release), or the explicit
    /// [`with_markup`](crate::plugins::LumenDefaultPlugins::with_markup) /
    /// `with_dir` paths.
    ///
    /// Started by `lumenc run <app> --headless`, the app takes
    /// [`run_headless_rendered`](Self::run_headless_rendered) instead, with
    /// the `--ticks`, `--dpr` and `--size` given there: lumenc hands them to
    /// this process through [`HeadlessHandoff`].
    pub fn run(self) -> Result<()> {
        if let Some(handoff) = HeadlessHandoff::from_env().map_err(Error::Setup)? {
            return self.run_handed_off(handoff);
        }
        let headless = self.boot.mode == WindowMode::Headless;
        let (opts, seeds, deferred) = self.into_run_options()?;
        if headless {
            // Windowing disabled: fall back to a bounded headless drive so
            // `run()` still terminates instead of blocking on a loop that
            // never opens a window.
            return lumen_runtime::run_app_headless(with_hooks(opts, seeds, deferred), 1)
                .map_err(Error::Run);
        }
        lumen_runtime::run_app(with_hooks(opts, seeds, deferred)).map_err(Error::Run)
    }

    /// Build the full stack without a window and tick it `ticks` times. The CI /
    /// test entry point: same markup, systems, and reactive bindings as
    /// [`run`](Self::run), no GPU. Requires a UI source.
    pub fn run_headless(self, ticks: u32) -> Result<()> {
        let (opts, seeds, deferred) = self.into_run_options()?;
        lumen_runtime::run_app_headless(with_hooks(opts, seeds, deferred), ticks)
            .map_err(Error::Run)
    }

    /// Run the whole pipeline with no window: layout, rendering on the app's
    /// `[render] backend` into an offscreen target, scripts, and the
    /// introspection server when `[mcp] simulate` is on. The same mode as
    /// `lumenc run <app> --headless`; [`run_headless`](Self::run_headless)
    /// is the cheaper one that ticks the app and paints nothing.
    ///
    /// `options.ticks` bounds the run; `None` runs until SIGINT / SIGTERM.
    /// The viewport size is the window size
    /// ([`with_size`](crate::plugins::LumenPluginsBuilder::with_size), else
    /// `[window] size` in `lumen.toml`).
    pub fn run_headless_rendered(self, options: HeadlessOptions) -> Result<()> {
        let (opts, seeds, deferred) = self.into_run_options()?;
        lumen_runtime::run_app_headless_rendered(with_hooks(opts, seeds, deferred), options)
            .map_err(Error::Run)
    }

    /// Run like [`run_headless_rendered`](Self::run_headless_rendered) and
    /// return the last frame it painted: the one the closing tick draws,
    /// after `options.ticks` ticks (or after SIGINT / SIGTERM when that is
    /// `None`). This is how an app screenshots itself or compares a frame
    /// with a golden image in its own tests.
    ///
    /// ```no_run
    /// # extern crate lumenui_engine as lumenui;
    /// use lumenui::prelude::*;
    ///
    /// # fn demo() -> lumenui::Result<()> {
    /// let frame = lumenui::App::new()
    ///     .add_plugins(LumenDefaultPlugins.with_markup("<root/>").with_size(320, 240))
    ///     .screenshot(HeadlessOptions { dpr: 1.0, ticks: Some(3) })?;
    /// frame.save_png("shot.png")?;
    /// # Ok(()) }
    /// ```
    pub fn screenshot(mut self, options: HeadlessOptions) -> Result<Frame> {
        let slot: Arc<Mutex<Option<SurfaceCapture>>> = Arc::default();
        let shared = Arc::clone(&slot);
        self.push_deferred(move |ecs| {
            // The introspection server brings one when it is on; otherwise
            // nothing in the stack answers a capture, so put one in both
            // worlds: the request is made in the main one and answered by
            // the renderer in the render one.
            let capture = match ecs.world.get_resource::<SurfaceCapture>() {
                Some(capture) => capture.clone(),
                None => {
                    let capture = SurfaceCapture::default();
                    ecs.world.insert_resource(capture.clone());
                    ecs.render_world.insert_resource(capture.clone());
                    capture
                }
            };
            *shared.lock().unwrap_or_else(PoisonError::into_inner) = Some(capture);
            ecs.add_systems(TickStage::Systems, capture_on_close);
        });
        self.run_headless_rendered(options)?;
        let capture = slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| Error::Setup("the app never started, so nothing was painted".into()))?;
        capture
            .read()
            .map(Frame::from)
            .ok_or_else(|| Error::Setup("the renderer painted no frame for the screenshot".into()))
    }

    /// `run` under a [`HeadlessHandoff`]: the options lumenc was given, with
    /// the size it names in place of the window's.
    fn run_handed_off(mut self, handoff: HeadlessHandoff) -> Result<()> {
        if let Some(size) = handoff.size {
            self.boot.size = Some(size);
        }
        self.run_headless_rendered(HeadlessOptions {
            dpr: handoff.dpr.unwrap_or(1.0),
            ticks: handoff.ticks,
        })
    }

    /// Build a *bare* ECS [`App`](EcsApp) carrying only the seeds, user
    /// systems, and user plugins added here - no markup, no backend stack.
    ///
    /// For unit tests over pure-Rust logic and for verifying plugin-group
    /// composition: install exactly the slices you want and `tick()` by hand.
    pub fn build_bare(self) -> EcsApp {
        let mut ecs = EcsApp::new();
        {
            let mut store = ecs.world.resource_mut::<PropertyStore>();
            for (key, value) in self.seeds {
                store.set(key, value);
            }
        }
        // Plugins first, matching the windowed path: on a real boot they run
        // before the script hosts load, which is ahead of everything added
        // through `deferred`.
        for f in self.plugins {
            f(&mut ecs);
        }
        for f in self.deferred {
            f(&mut ecs);
        }
        // Install native handlers last so their collect -> dispatch systems
        // sit after any user systems added through `deferred`.
        install_rust_handlers(&mut ecs, self.handlers);
        ecs
    }

    fn into_run_options(mut self) -> Result<RunSetup> {
        // Fold the terse `on_click` / `on` handlers into a single deferred
        // installer so they run through the same pipeline as the builder
        // surface, after every other deferred system is registered.
        if !self.handlers.is_empty() {
            let handlers = std::mem::take(&mut self.handlers);
            self.deferred.push(Box::new(move |ecs| {
                install_rust_handlers(ecs, handlers);
            }));
        }
        let in_memory = self.boot.markup.is_some();
        let dir = match self.boot.dir {
            Some(d) => d,
            None if in_memory => match embedded_app_dir(
                std::env::current_exe().ok().as_deref(),
                self.boot.built_dir.as_deref(),
            ) {
                Some(dir) => dir,
                None => std::env::current_dir()
                    .map_err(|e| Error::Setup(format!("resolve current dir: {e}")))?,
            },
            None => {
                return Err(Error::Setup(
                    "no UI source: add \
                     LumenDefaultPlugins.with_source(lumen_source!(\"main.lmn\")) \
                     (hot-reload in debug, embedded in release), or the explicit \
                     .with_markup(include_str!(\"main.lmn\")) / .with_dir(<app dir>)"
                        .into(),
                ));
            }
        };

        let mut opts = RunOptions::new(dir);
        opts.plugins = self.plugins;
        opts.title = self.boot.title;
        if let Some(size) = self.boot.size {
            opts.size = size;
        }
        opts.markup = match (self.boot.markup, self.boot.strip_script) {
            (Some(src), true) => Some(strip_script_blocks(&src)),
            (other, _) => other,
        };
        opts.css = self.boot.css;
        opts.hot_reload = self.boot.hot_reload.unwrap_or(!in_memory);

        // The compiler-plugin chain resolves here, with the other option
        // preparation, so every run entry point below - windowed, headless
        // mode, bounded ticks - carries it without touching the options
        // again.
        let opts = lumenc::with_default_compiler_plugins(opts).map_err(Error::Run)?;
        Ok((opts, self.seeds, self.deferred))
    }
}

/// The app directory of an app whose markup is embedded, where the files the
/// markup and stylesheet name by relative path and `lumen.toml` are read
/// from: the directory holding `exe` when it holds a `lumen.toml` too, which
/// is the layout `lumenc package` writes, else the directory the sources were
/// embedded from when it is still there. `None` leaves the working directory.
fn embedded_app_dir(exe: Option<&Path>, built: Option<&Path>) -> Option<PathBuf> {
    exe.and_then(Path::parent)
        .filter(|dir| dir.join("lumen.toml").is_file())
        .or(built.filter(|dir| dir.is_dir()))
        .map(Path::to_path_buf)
}

/// Ask for the frame the closing tick paints. The runtime writes the close
/// before that tick, and a requested capture makes the renderer paint the
/// frame however little changed; the dirty flag makes the tick reach the
/// renderer at all.
fn capture_on_close(
    mut closes: MessageReader<CloseRequest>,
    capture: Res<SurfaceCapture>,
    mut dirty: ResMut<FrameDirty>,
) {
    if closes.read().count() > 0 {
        capture.request();
        dirty.dirty = true;
    }
}

/// Attach the seed + deferred-system installation as a lumenc app hook.
fn with_hooks(
    opts: RunOptions,
    seeds: Vec<(PropertyKey, PropertyValue)>,
    deferred: Vec<Deferred>,
) -> RunOptions {
    // Inject the compiler's front-end so the runtime can parse markup / CSS
    // from source (dev source-load + hot reload); `lumen-runtime` links none.
    let opts = if opts.parser.is_none() {
        opts.with_parser(lumenc::default_parser())
    } else {
        opts
    };
    opts.with_app_hook(move |ecs: &mut EcsApp| {
        if !seeds.is_empty() {
            let mut store = ecs.world.resource_mut::<PropertyStore>();
            for (key, value) in seeds {
                store.set(key, value);
            }
        }
        for f in deferred {
            f(ecs);
        }
    })
}

/// Remove `<script>...</script>` blocks from markup. Used when
/// [`ScriptPlugin`](crate::plugins::ScriptPlugin) is disabled so the lumenc
/// pipeline never installs a script host. Case-insensitive on the tag name;
/// leaves all other markup untouched.
fn strip_script_blocks(src: &str) -> String {
    let bytes = src.as_bytes();
    let lower = src.to_ascii_lowercase();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if lower[i..].starts_with("<script") {
            // Find the end of the opening tag.
            if let Some(open_end) = lower[i..].find('>') {
                let after_open = i + open_end + 1;
                // Self-closing `<script .../>`.
                if bytes[after_open - 2] == b'/' {
                    i = after_open;
                    continue;
                }
                if let Some(close_rel) = lower[after_open..].find("</script>") {
                    i = after_open + close_rel + "</script>".len();
                    continue;
                }
                // Unterminated - drop the rest.
                break;
            }
            break;
        }
        let ch = src[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glam::Vec2;
    use crate::plugins::{LumenDefaultPlugins, Source};
    use bevy_ecs::entity::Entity;
    use lumen_core::components::{LumenId, TextContent, Transform};
    use lumen_core::input::{
        ClickEvent, ClickSet, Key, KeyPressed, Modifiers, NamedKey, PointerButton, PointerMoved,
        PointerPressed, PointerReleased, PointerState,
    };
    use lumen_core::render_world::FrameDirty;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// An embedded source bakes markup / CSS into `RunOptions` and disables
    /// hot reload - the release / always-embed shape.
    #[test]
    fn embedded_source_embeds_and_disables_hot_reload() {
        let app = App::new()
            .add_plugins(LumenDefaultPlugins.with_source(Source::embedded("<root/>", Some("a{}"))));
        let (opts, ..) = app.into_run_options().expect("into_run_options");
        assert_eq!(opts.markup.as_deref(), Some("<root/>"));
        assert_eq!(opts.css.as_deref(), Some("a{}"));
        assert!(!opts.hot_reload, "embedded source must not hot-reload");
    }

    /// A disk source leaves markup unset (the runtime reads `main.lmn` from
    /// `dir`) and turns hot reload on - the debug / hot-reload shape. Proves
    /// the debug path is NOT a compile-time-frozen copy: no markup is embedded,
    /// only a directory the runtime loads at startup.
    #[test]
    fn disk_source_loads_from_dir_and_enables_hot_reload() {
        let app = App::new()
            .add_plugins(LumenDefaultPlugins.with_source(Source::disk("/app/ui/main.lmn")));
        let (opts, ..) = app.into_run_options().expect("into_run_options");
        assert!(opts.markup.is_none(), "disk source must not embed markup");
        assert_eq!(opts.dir, std::path::PathBuf::from("/app/ui"));
        assert!(opts.hot_reload, "disk source must hot-reload");
    }

    /// `Source::disk` derives the app dir from the markup file's parent; a bare
    /// filename falls back to the current directory.
    #[test]
    fn disk_derives_parent_dir() {
        assert!(matches!(
            Source::disk("main.lmn"),
            Source::Disk { dir } if dir == std::path::Path::new(".")
        ));
    }

    /// A markup file under `src/` names the app around it: `lumen.toml` and
    /// the `src/` lookup both start one directory up.
    #[test]
    fn disk_under_src_is_the_app_above_it() {
        assert!(matches!(
            Source::disk("/app/src/main.lmn"),
            Source::Disk { dir } if dir == std::path::Path::new("/app")
        ));
        assert!(matches!(
            Source::disk("src/main.lmn"),
            Source::Disk { dir } if dir == std::path::Path::new(".")
        ));
    }

    /// #525: embedded sources read the rest of the app from the executable's
    /// directory when it is a packaged app's, else from the directory they
    /// were embedded from, and only then from the working directory.
    #[test]
    fn embedded_sources_find_their_app_dir() {
        let root = std::env::temp_dir().join(format!("lumenui-app-dir-{}", std::process::id()));
        let packaged = root.join("packaged");
        let built = root.join("built");
        std::fs::create_dir_all(&packaged).unwrap();
        std::fs::create_dir_all(&built).unwrap();
        std::fs::write(packaged.join("lumen.toml"), "").unwrap();
        let exe = packaged.join("app");
        let bare_exe = built.join("target").join("app");

        assert_eq!(
            embedded_app_dir(Some(&exe), Some(&built)),
            Some(packaged.clone()),
            "a packaged app reads beside its executable"
        );
        assert_eq!(
            embedded_app_dir(Some(&bare_exe), Some(&built)),
            Some(built.clone()),
            "a build run from its target directory reads where it was built"
        );
        assert_eq!(
            embedded_app_dir(Some(&bare_exe), Some(&root.join("gone"))),
            None,
            "a build moved off the machine it was built on falls back"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The release branch of `lumen_source!` records the app dir the markup
    /// came from.
    #[test]
    fn embedded_from_records_the_app_dir() {
        assert!(matches!(
            Source::embedded_from("/app/src/main.lmn", "<root/>", None::<&str>),
            Source::Embedded { dir: Some(dir), .. } if dir == std::path::Path::new("/app")
        ));
    }

    /// The `lumen_source!` macro picks the variant by build profile: disk (+
    /// watcher) in debug, embedded in release. Same source line, both builds.
    #[test]
    fn lumen_source_macro_selects_by_profile() {
        let src = crate::lumen_source!("examples/src/main.lmn", "examples/src/main.css");
        if cfg!(debug_assertions) {
            assert!(matches!(src, Source::Disk { .. }));
        } else {
            assert!(matches!(src, Source::Embedded { css: Some(_), .. }));
        }
    }

    /// A headless app, held alone: the external DOM bus is process-wide, so
    /// a second app ticking on another test thread would drain the commands
    /// this one's systems queue.
    struct Stack {
        ecs: EcsApp,
        _alone: std::sync::MutexGuard<'static, ()>,
    }

    impl std::ops::Deref for Stack {
        type Target = EcsApp;
        fn deref(&self) -> &EcsApp {
            &self.ecs
        }
    }

    impl std::ops::DerefMut for Stack {
        fn deref_mut(&mut self) -> &mut EcsApp {
            &mut self.ecs
        }
    }

    /// The whole headless stack (input, layout, bindings, paint pipeline)
    /// for `app`, ticked until its first frame is out.
    fn headless_stack(app: App) -> Stack {
        static ALONE: Mutex<()> = Mutex::new(());
        let alone = ALONE.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "lumenui_handler_frame_{}_{}",
            std::process::id(),
            {
                static SEQ: AtomicUsize = AtomicUsize::new(0);
                SEQ.fetch_add(1, Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
        let app = app.add_plugins(LumenDefaultPlugins.with_dir(&dir));
        let (opts, seeds, deferred) = app.into_run_options().expect("into_run_options");
        let (mut ecs, _window) =
            lumenc::build_headless_app(with_hooks(opts, seeds, deferred)).expect("headless app");
        for _ in 0..4 {
            ecs.tick();
        }
        let _ = std::fs::remove_dir_all(&dir);
        Stack { ecs, _alone: alone }
    }

    fn entity_by_id(ecs: &mut EcsApp, id: &str) -> Entity {
        let mut q = ecs.world.query::<(Entity, &LumenId)>();
        q.iter(&ecs.world)
            .find(|(_, l)| l.0 == id)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no #{id}"))
    }

    fn text_of(ecs: &mut EcsApp, id: &str) -> String {
        let e = entity_by_id(ecs, id);
        ecs.world
            .get::<TextContent>(e)
            .map(|t| t.0.clone())
            .unwrap_or_default()
    }

    fn center_of(ecs: &mut EcsApp, id: &str) -> Vec2 {
        let e = entity_by_id(ecs, id);
        let t = *ecs.world.get::<Transform>(e).expect("laid out");
        t.absolute + t.size * 0.5
    }

    /// A press and release over `id` on one tick, the way a simulated click
    /// arrives; the window's present then clears the dirty flag.
    fn click_and_tick(ecs: &mut EcsApp, id: &str) -> bool {
        let at = center_of(ecs, id);
        ecs.world.resource_mut::<FrameDirty>().dirty = false;
        ecs.world.resource_mut::<PointerState>().position = Some(at);
        ecs.world.write_message(PointerMoved {
            position: at,
            local: None,
            pointer: Default::default(),
        });
        ecs.world.write_message(PointerPressed {
            position: at,
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        });
        ecs.world.write_message(PointerReleased {
            position: at,
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        });
        ecs.tick();
        ecs.world.resource::<FrameDirty>().dirty
    }

    /// Longer than the default double-click window.
    const DOUBLE_CLICK_GAP: Duration = Duration::from_millis(350);
    /// Longer than the default long-press threshold.
    const LONG_PRESS_HOLD: Duration = Duration::from_millis(550);

    const KINDS: &str = r#"<root padding="20" gap="10">
  <button id="bump" width="120px" height="40px" text="+1" />
  <label id="clicks" width="200px" height="30px" bind-text="clicks" text="0" />
  <label id="doubles" width="200px" height="30px" bind-text="doubles" text="0" />
  <label id="longs" width="200px" height="30px" bind-text="longs" text="0" />
</root>"#;

    const COUNTER: &str = r#"<root padding="20" gap="10">
  <button id="bump" width="120px" height="40px" text="+1" />
  <label id="clicks" width="200px" height="30px" bind-text="clicks" text="0" />
</root>"#;

    /// A Rust click handler's signal write shows on the tick the click
    /// lands, and that tick raises a frame: a headless run parks once a
    /// tick paints nothing, so a write that only surfaces a tick later
    /// stays off screen until something else wakes the app (#514).
    #[test]
    fn click_handler_write_paints_on_its_own_tick() {
        let mut ecs = headless_stack(
            App::new()
                .add_plugins(LumenDefaultPlugins.with_markup(COUNTER))
                .insert_signal("clicks", 0i64)
                .on_click("bump", |ctx| {
                    let n = ctx.get_or::<i64>("clicks", 0) + 1;
                    ctx.set("clicks", n);
                }),
        );
        for expect in 1..=3 {
            // Clicks spaced past the double-click window, which would
            // otherwise turn the second one into a double-click.
            if expect > 1 {
                std::thread::sleep(DOUBLE_CLICK_GAP);
            }
            let dirty = click_and_tick(&mut ecs, "bump");
            assert_eq!(
                text_of(&mut ecs, "clicks"),
                expect.to_string(),
                "click {expect}: the label did not take the handler's write on the click's tick"
            );
            assert!(dirty, "click {expect}: the click's tick raised no frame");
        }
    }

    /// Every closure handler kind writes on the tick its event fires and
    /// raises that tick's frame: a double-click, a long press, the wildcard
    /// `on`, and a click synthesized by Enter on the focused button (#514).
    #[test]
    fn every_handler_kind_paints_on_its_own_tick() {
        let bump = |name: &'static str| {
            move |ctx: &mut EventCtx<'_>| {
                let n = ctx.get_or::<i64>(name, 0) + 1;
                ctx.set(name, n);
            }
        };
        let mut ecs = headless_stack(
            App::new()
                .add_plugins(LumenDefaultPlugins.with_markup(KINDS))
                .on(EventKind::Click, None, bump("clicks"))
                .on_double_click("bump", bump("doubles"))
                .on_long_press("bump", bump("longs")),
        );

        // A click, then a second inside the double-click window.
        assert!(
            click_and_tick(&mut ecs, "bump"),
            "the click raised no frame"
        );
        assert_eq!(text_of(&mut ecs, "clicks"), "1");
        assert!(
            click_and_tick(&mut ecs, "bump"),
            "the double-click raised no frame"
        );
        assert_eq!(text_of(&mut ecs, "doubles"), "1");

        // Enter on the button the clicks focused.
        std::thread::sleep(DOUBLE_CLICK_GAP);
        ecs.world.resource_mut::<FrameDirty>().dirty = false;
        ecs.world.write_message(KeyPressed {
            key: Key::Named(NamedKey::Enter),
            modifiers: Modifiers::default(),
            repeat: false,
            time_stamp: lumen_core::time::Instant::now(),
        });
        ecs.tick();
        assert_eq!(
            text_of(&mut ecs, "clicks"),
            "2",
            "Enter's click wrote a tick late"
        );
        assert!(
            ecs.world.resource::<FrameDirty>().dirty,
            "Enter's click raised no frame"
        );

        // A press held past the long-press threshold, then released.
        let at = center_of(&mut ecs, "bump");
        ecs.world.resource_mut::<PointerState>().primary_down = true;
        ecs.world.write_message(PointerPressed {
            position: at,
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        });
        ecs.tick();
        std::thread::sleep(LONG_PRESS_HOLD);
        ecs.world.resource_mut::<FrameDirty>().dirty = false;
        ecs.tick();
        assert_eq!(
            text_of(&mut ecs, "longs"),
            "1",
            "the long press wrote a tick late"
        );
        assert!(
            ecs.world.resource::<FrameDirty>().dirty,
            "the long press raised no frame"
        );
    }

    /// Write `clicks` from a system the schedule runs after the `bind-text`
    /// reader, the order an unordered user system may land in.
    fn late_signal_writer(mut clicks: MessageReader<ClickEvent>, mut signals: Signals) {
        let hits = clicks.read().count() as i64;
        if hits > 0 {
            signals.update::<i64>("clicks", |n| n + hits);
        }
    }

    /// Edit `#clicks` from a system that runs after the DOM commands of the
    /// tick were collected.
    fn late_dom_editor(mut clicks: MessageReader<ClickEvent>) {
        if clicks.read().count() > 0
            && let Some(label) = crate::dom::Node::get_by_id("clicks")
        {
            label.set_text("edited");
        }
    }

    /// Bring the loop to rest: tick while the last tick left work behind, the
    /// way a driver that parks on events does. Returns how many ticks ran.
    fn settle(ecs: &mut EcsApp) -> usize {
        let mut ran = 0;
        while lumen_core::tick::work_pending(&ecs.world) && ran < 16 {
            ecs.world.resource_mut::<FrameDirty>().dirty = false;
            ecs.tick();
            ran += 1;
        }
        ran
    }

    /// #523: a signal written after the binding readers ran still reaches
    /// the screen without another input: the tick that wrote it leaves work
    /// behind, and the next one applies it and raises a frame.
    #[test]
    fn a_signal_written_after_the_readers_paints_without_input() {
        let mut ecs = headless_stack(
            App::new()
                .add_plugins(LumenDefaultPlugins.with_markup(COUNTER))
                .insert_signal("clicks", 0i64)
                .add_systems(
                    TickStage::Systems,
                    late_signal_writer
                        .after(ClickSet::Producers)
                        .after(lumen_core::signals::apply_text_bindings),
                ),
        );
        settle(&mut ecs);
        click_and_tick(&mut ecs, "bump");
        assert!(
            lumen_core::tick::work_pending(&ecs.world),
            "the tick that wrote the signal left nothing behind"
        );
        ecs.world.resource_mut::<FrameDirty>().dirty = false;
        ecs.tick();
        assert_eq!(text_of(&mut ecs, "clicks"), "1");
        assert!(
            ecs.world.resource::<FrameDirty>().dirty,
            "the tick that applied the write raised no frame"
        );
        assert!(settle(&mut ecs) < 16, "the loop never came to rest");
    }

    /// #523: a DOM edit queued after the tick's DOM commands were collected
    /// lands on the next tick, and that tick runs without another input.
    #[test]
    fn a_dom_edit_after_the_collector_paints_without_input() {
        let mut ecs = headless_stack(
            App::new()
                .add_plugins(LumenDefaultPlugins.with_markup(COUNTER))
                .add_systems(TickStage::LayoutSync, late_dom_editor),
        );
        settle(&mut ecs);
        click_and_tick(&mut ecs, "bump");
        assert!(
            lumen_core::tick::work_pending(&ecs.world),
            "the tick that queued the edit left nothing behind"
        );
        settle(&mut ecs);
        assert_eq!(text_of(&mut ecs, "clicks"), "edited");
    }

    #[test]
    fn strip_script_removes_blocks() {
        let src = "<root><script>let x = 1;</script><label/></root>";
        assert_eq!(strip_script_blocks(src), "<root><label/></root>");
    }

    #[test]
    fn strip_script_handles_self_closing_and_attrs() {
        let src = "<root><script src=\"a.cdl\" /><label/></root>";
        assert_eq!(strip_script_blocks(src), "<root><label/></root>");
    }

    #[test]
    fn strip_script_leaves_plain_markup() {
        let src = "<root><label text=\"hi\"/></root>";
        assert_eq!(strip_script_blocks(src), src);
    }
}
