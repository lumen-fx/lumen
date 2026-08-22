//! The ECS-first [`App`] - Lumen's bevy-shaped entry point.

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::{bevy_ecs, lumen_core, lumen_runtime, lumenc};

use crate::error::{Error, Result};
use crate::events::{EventCtx, EventKind, HandlerEntry, install_rust_handlers};
use crate::plugins::{AppPlugins, BootConfig, WindowMode};
use crate::signal::Signals;
use bevy_ecs::message::Message;
use bevy_ecs::prelude::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::ScheduleSystem;
use lumen_core::app::{App as EcsApp, Plugin};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_core::tick::TickStage;
use lumen_runtime::{PluginInstaller, RunOptions};

type Deferred = Box<dyn FnOnce(&mut EcsApp) + 'static>;

/// The lumenc [`RunOptions`] plus the seeds and deferred installers that
/// [`App`] threads into it as an app hook.
type RunSetup = (RunOptions, Vec<(PropertyKey, PropertyValue)>, Vec<Deferred>);

/// A Lumen application, assembled the way a bevy app is: add plugin groups, add
/// systems, then [`run`](Self::run).
///
/// ```no_run
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
///     .add_systems(TickStage::Systems, bump)
///     .run()
/// # }
/// ```
///
/// User systems are *real* `bevy_ecs` systems: take `Query` / `Res` / `ResMut` /
/// `MessageReader` / [`Commands`](bevy_ecs::system::Commands) / [`Signals`](crate::Signals),
/// and they are scheduled into the live [`TickStage`] schedule next to the
/// framework's own systems. Nothing is stringly-typed.
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

    /// Add a single [`Plugin`], installed on the built ECS app before the
    /// script hosts load.
    ///
    /// That phase is what lets a plugin register script functions
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
    pub fn run(self) -> Result<()> {
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
            None if in_memory => std::env::current_dir()
                .map_err(|e| Error::Setup(format!("resolve current dir: {e}")))?,
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
/// pipeline never installs the Rhai host. Case-insensitive on the tag name;
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
    use crate::plugins::{LumenDefaultPlugins, Source};

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

    /// The `lumen_source!` macro picks the variant by build profile: disk (+
    /// watcher) in debug, embedded in release. Same source line, both builds.
    #[test]
    fn lumen_source_macro_selects_by_profile() {
        let src = crate::lumen_source!("examples/main.lmn", "examples/main.css");
        if cfg!(debug_assertions) {
            assert!(matches!(src, Source::Disk { .. }));
        } else {
            assert!(matches!(src, Source::Embedded { css: Some(_), .. }));
        }
    }

    #[test]
    fn strip_script_removes_blocks() {
        let src = "<root><script>let x = 1;</script><label/></root>";
        assert_eq!(strip_script_blocks(src), "<root><label/></root>");
    }

    #[test]
    fn strip_script_handles_self_closing_and_attrs() {
        let src = "<root><script src=\"a.rhai\" /><label/></root>";
        assert_eq!(strip_script_blocks(src), "<root><label/></root>");
    }

    #[test]
    fn strip_script_leaves_plain_markup() {
        let src = "<root><label text=\"hi\"/></root>";
        assert_eq!(strip_script_blocks(src), src);
    }
}
