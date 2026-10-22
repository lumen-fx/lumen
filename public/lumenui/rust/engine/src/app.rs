//! [`App`]: a Lumen app whose code is Rust.

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::{bevy_ecs, lumen_core, lumen_runtime, lumen_script, lumenc};

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::{Error, Result};
use crate::events::{
    Event, EventKind, Handler, HandlerEntry, HandlerSet, ReadyHandler, install_handlers,
};
use crate::frame::Frame;
use crate::plugins::AppPlugins;
use crate::signal::Signals;
use crate::ui::{CompiledUi, Ui};
use bevy_ecs::message::{Message, MessageReader};
use bevy_ecs::prelude::{Res, ResMut, Resource};
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::ScheduleSystem;
use lumen_core::app::{App as EcsApp, Plugin};
use lumen_core::input::CloseRequest;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_core::render_world::{FrameDirty, SurfaceCapture};
use lumen_core::tick::{AppSet, TickStage};
use lumen_runtime::app_kind::HeadlessHandoff;
use lumen_runtime::{HeadlessOptions, PluginInstaller, RunOptions};
use lumen_script::ScriptFn;

type Deferred = Box<dyn FnOnce(&mut EcsApp) + 'static>;

/// The runtime's options plus the seeds and deferred installers that [`App`]
/// threads into it as an app hook.
type RunSetup = (RunOptions, Vec<(PropertyKey, PropertyValue)>, Vec<Deferred>);

/// Where the app's UI comes from.
enum UiInput {
    /// Compiled from what `lumen.toml` names, by the build script or by
    /// [`CompiledUi::compile`].
    Compiled(CompiledUi),
    /// Markup handed over as text, parsed when the app starts.
    Markup(String),
}

/// A Lumen app whose code is Rust.
///
/// The UI is the app's markup and stylesheets, named by `lumen.toml` and
/// compiled when the crate builds; [`lumenui::app!()`](crate::app) starts an
/// app from it. Code reaches the UI the way a script does, through handlers
/// on element ids, signals by name and the element tree:
///
/// ```no_run
/// # extern crate lumenui_engine as lumenui;
/// use lumenui::prelude::*;
///
/// # fn demo() -> lumenui::Result<()> {
/// lumenui::App::from_markup(r#"<root><button id="bump" text="+1"/><label bind-text="clicks"/></root>"#)
///     .signal("clicks", 0i64)
///     .on_click("bump", |ui, _| ui.update("clicks", |n: i64| n + 1))
///     .run()
/// # }
/// ```
///
/// The same app takes `bevy_ecs` systems where they fit better, such as a
/// paint loop over every pointer move: [`add_systems`](Self::add_systems)
/// puts a system in the app's slot of the tick, after the input and before
/// the screen is updated, so it orders itself against nothing.
pub struct App {
    ui: UiInput,
    app_dir: Option<PathBuf>,
    title: Option<String>,
    size: Option<(u32, u32)>,
    seeds: Vec<(PropertyKey, PropertyValue)>,
    plugins: Vec<PluginInstaller>,
    deferred: Vec<Deferred>,
    handlers: Vec<HandlerEntry>,
    ready: Vec<ReadyHandler>,
    native_fns: Vec<ScriptFn>,
}

impl App {
    /// An app over a compiled UI, normally
    /// [`lumenui::ui!()`](crate::ui): what [`lumenui::app!()`](crate::app)
    /// expands to.
    pub fn new(ui: CompiledUi) -> Self {
        Self::with_input(UiInput::Compiled(ui))
    }

    /// An app over markup given as text, parsed when the app starts. For a
    /// test or an example that has no build script; an app ships the UI its
    /// build compiled ([`lumenui::app!()`](crate::app)).
    pub fn from_markup(markup: impl Into<String>) -> Self {
        Self::with_input(UiInput::Markup(markup.into()))
    }

    fn with_input(ui: UiInput) -> Self {
        Self {
            ui,
            app_dir: None,
            title: None,
            size: None,
            seeds: Vec::new(),
            plugins: Vec::new(),
            deferred: Vec::new(),
            handlers: Vec::new(),
            ready: Vec::new(),
            native_fns: Vec::new(),
        }
    }

    // -- Where the app lives ------------------------------------------------

    /// Read `lumen.toml` and the assets from `dir`.
    ///
    /// Without it they come from the executable's directory when that holds
    /// a `lumen.toml` (a packaged app), else from the directory the UI was
    /// compiled from, else the working directory. A test sets it to run the
    /// app over a `lumen.toml` of its own.
    #[must_use]
    pub fn app_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.app_dir = Some(dir.into());
        self
    }

    /// The window title, over `[window] title` in `lumen.toml`. For a title
    /// the app works out when it starts; a fixed one belongs in `lumen.toml`.
    #[must_use]
    pub fn window_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The window size in logical pixels, over `[window] size` in
    /// `lumen.toml`. For a size the app works out when it starts (the size
    /// it had when it last closed); a fixed one belongs in `lumen.toml`.
    #[must_use]
    pub fn window_size(mut self, width: u32, height: u32) -> Self {
        self.size = Some((width, height));
        self
    }

    // -- State ---------------------------------------------------------------

    /// Seed the signal `name` before the first tick, so markup bound to it
    /// shows `value` on the first frame.
    #[must_use]
    pub fn signal(mut self, name: impl Into<String>, value: impl Into<PropertyValue>) -> Self {
        self.seeds
            .push((PropertyKey::global(name.into()), value.into()));
        self
    }

    /// Insert a resource: the app's own state, which handlers reach with
    /// [`Ui::resource`] / [`Ui::resource_mut`] and systems with
    /// `Res` / `ResMut`.
    #[must_use]
    pub fn insert_resource<R: Resource>(mut self, resource: R) -> Self {
        self.push_deferred(move |ecs| {
            ecs.world.insert_resource(resource);
        });
        self
    }

    /// A signal recomputed every tick from others: `output` takes what `f`
    /// returns, and markup bound to it shows the fresh value on the tick
    /// its inputs change. An unchanged result is not a write.
    ///
    /// ```
    /// # extern crate lumenui_engine as lumenui;
    /// use lumenui::prelude::*;
    ///
    /// # let app =
    /// lumenui::App::from_markup("<root/>")
    ///     .signal("count", 0i64)
    ///     .add_computed("label", |s| format!("clicks: {}", s.get_or::<i64>("count", 0)));
    /// ```
    #[must_use]
    pub fn add_computed<T, F>(self, output: impl Into<String>, f: F) -> Self
    where
        T: Into<PropertyValue> + 'static,
        F: Fn(&Signals) -> T + Send + Sync + 'static,
    {
        let output = output.into();
        self.add_systems(TickStage::Systems, move |mut signals: Signals| {
            let value = f(&signals);
            signals.set(&output, value);
        })
    }

    // -- Handlers ------------------------------------------------------------

    /// Run `handler` when the element with `id`, or anything inside it, is
    /// clicked.
    ///
    /// The handler gets a [`Ui`] for the app and the [`Event`]. A click
    /// goes to the nearest element at or above the target that has a click
    /// handler, so a click on a button's icon runs the button's.
    #[must_use]
    pub fn on_click<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.on(EventKind::Click, id, handler)
    }

    /// Run `handler` for a click that no [`on_click`](Self::on_click)
    /// handler takes. [`Event::id`] names the nearest element with an id.
    #[must_use]
    pub fn on_any_click<F>(self, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.on_any(EventKind::Click, handler)
    }

    /// Run `handler` on a double-click on the element with `id`. That tick
    /// the plain click does not fire.
    #[must_use]
    pub fn on_double_click<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.on(EventKind::DoubleClick, id, handler)
    }

    /// Run `handler` on a long press on the element with `id`.
    #[must_use]
    pub fn on_long_press<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.on(EventKind::LongPress, id, handler)
    }

    /// Run `handler` when a context menu is asked for on the element with
    /// `id`. Open one with [`Ui::open_menu_at`] at [`Event::position`].
    #[must_use]
    pub fn on_context_menu<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.on(EventKind::ContextMenu, id, handler)
    }

    /// Run `handler` when the control with `id` toggles; [`Event::checked`]
    /// is its new state. A bound signal already holds it.
    #[must_use]
    pub fn on_toggle<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.on(EventKind::Toggle, id, handler)
    }

    /// Run `handler` when the slider with `id` moves; [`Event::value`] is
    /// its new value.
    #[must_use]
    pub fn on_slider<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.on(EventKind::Slider, id, handler)
    }

    /// Run `handler` when the `<menuitem>` with `id` is chosen.
    #[must_use]
    pub fn on_menu<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.on(EventKind::Menu, id, handler)
    }

    /// Run `handler` for an event of `kind` on the element with `id`. The
    /// `on_*` methods above are this for one kind each.
    #[must_use]
    pub fn on<F>(mut self, kind: EventKind, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.handlers
            .push((kind, Some(id.into()), Box::new(handler) as Handler));
        self
    }

    /// Run `handler` for an event of `kind` that no handler on an id takes.
    #[must_use]
    pub fn on_any<F>(mut self, kind: EventKind, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static,
    {
        self.handlers
            .push((kind, None, Box::new(handler) as Handler));
        self
    }

    /// Run `handler` once, on the first tick the element tree can be
    /// queried: where an app fills its first lists and reads what the
    /// markup laid out.
    #[must_use]
    pub fn on_ready<F>(mut self, handler: F) -> Self
    where
        F: FnMut(&mut Ui<'_>) + Send + Sync + 'static,
    {
        self.ready.push(Box::new(handler));
        self
    }

    // -- Scripts -------------------------------------------------------------

    /// Expose a Rust function to the app's scripts under `name`, in every
    /// language the app runs. Arguments and the result cross as
    /// [`ScriptValue`](crate::ScriptValue); `arity` is the argument count.
    /// [`script_fn`](Self::script_fn) declares parameter types, a namespace
    /// and a doc line.
    #[must_use]
    pub fn native_fn<F>(self, name: impl Into<String>, arity: usize, f: F) -> Self
    where
        F: Fn(&[crate::ScriptValue]) -> crate::ScriptValue + Send + Sync + 'static,
    {
        self.script_fn(ScriptFn::value(name, arity, f))
    }

    /// Expose a fully described [`ScriptFn`] to the app's scripts.
    #[must_use]
    pub fn script_fn(mut self, f: ScriptFn) -> Self {
        self.native_fns.push(f);
        self
    }

    // -- ECS -----------------------------------------------------------------

    /// Add a plugin group: a [`PluginGroupBuilder`](crate::plugins::PluginGroupBuilder)
    /// or a tuple of them. See [`AppPlugins`].
    #[must_use]
    pub fn add_plugins(mut self, plugins: impl AppPlugins) -> Self {
        plugins.apply(&mut self);
        self
    }

    /// Add a [`Plugin`], installed before the markup is parsed and the
    /// script hosts load.
    ///
    /// That is what lets a widget plugin (the one `#[derive(Widget)]`
    /// generates) register its tag in time for the markup that uses it, and
    /// a plugin register script functions candela binds when the program
    /// compiles. A plugin builds before the app's own
    /// [`insert_resource`](Self::insert_resource) /
    /// [`add_systems`](Self::add_systems) run, so one that reads such a
    /// resource reads it from a system.
    #[must_use]
    pub fn add_plugin<P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        self.plugins.push(Box::new(move |ecs: &mut EcsApp| {
            ecs.add_plugin(plugin);
        }));
        self
    }

    /// Add `bevy_ecs` systems (one, a tuple, a `.chain()`) to a
    /// [`TickStage`].
    ///
    /// In [`TickStage::Systems`] they go in the app's slot of the tick,
    /// [`AppSet`]: after the tick's input has become element events, every
    /// control has pushed its value into its signal and the app's handlers
    /// have run, and before the signals are written back into the tree. A
    /// system there reads this tick's events and what the handlers changed,
    /// and writes what shows on this tick's frame, with no ordering of its
    /// own; `.chain()` / `.after` between the app's own systems still apply.
    #[must_use]
    pub fn add_systems<M: 'static>(
        self,
        stage: TickStage,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M> + 'static,
    ) -> Self {
        let mut this = self;
        this.push_deferred(move |ecs| {
            if stage == TickStage::Systems {
                ecs.add_systems(stage, systems.in_set(AppSet).after(HandlerSet));
            } else {
                ecs.add_systems(stage, systems);
            }
        });
        this
    }

    /// Register a [`Message`] type of the app's own, for systems to read
    /// and write. Lumen's own messages are registered already.
    #[must_use]
    pub fn add_message<M: Message>(mut self) -> Self {
        self.push_deferred(|ecs| {
            ecs.add_message::<M>();
        });
        self
    }

    pub(crate) fn push_deferred(&mut self, f: impl FnOnce(&mut EcsApp) + 'static) {
        self.deferred.push(Box::new(f));
    }

    // -- Running -------------------------------------------------------------

    /// Open the window and run until it closes.
    ///
    /// Started by `lumenc run <app> --headless`, the app takes
    /// [`run_headless_rendered`](Self::run_headless_rendered) instead, with
    /// the `--ticks`, `--dpr` and `--size` given there: lumenc hands them to
    /// this process through [`HeadlessHandoff`].
    pub fn run(self) -> Result<()> {
        if let Some(handoff) = HeadlessHandoff::from_env().map_err(Error::Setup)? {
            return self.run_handed_off(handoff);
        }
        let (opts, seeds, deferred) = self.into_run_options()?;
        lumen_runtime::run_app(with_hooks(opts, seeds, deferred)).map_err(Error::Run)
    }

    /// Build the app without a window and tick it `ticks` times: the cheap
    /// test entry point, with the same markup, handlers and systems as
    /// [`run`](Self::run) and nothing painted.
    pub fn run_headless(self, ticks: u32) -> Result<()> {
        let (opts, seeds, deferred) = self.into_run_options()?;
        lumen_runtime::run_app_headless(with_hooks(opts, seeds, deferred), ticks)
            .map_err(Error::Run)
    }

    /// Run the whole pipeline with no window: layout, rendering on the app's
    /// `[render] backend` into an offscreen target, scripts, and the
    /// introspection server when `[mcp] simulate` is on. The same mode as
    /// `lumenc run <app> --headless`.
    ///
    /// `options.ticks` bounds the run; `None` runs until SIGINT / SIGTERM.
    /// The viewport size is the window size
    /// ([`window_size`](Self::window_size), else `[window] size` in
    /// `lumen.toml`).
    pub fn run_headless_rendered(self, options: HeadlessOptions) -> Result<()> {
        let (opts, seeds, deferred) = self.into_run_options()?;
        lumen_runtime::run_app_headless_rendered(with_hooks(opts, seeds, deferred), options)
            .map_err(Error::Run)
    }

    /// Run like [`run_headless_rendered`](Self::run_headless_rendered) and
    /// return the last frame it painted, after `options.ticks` ticks (or
    /// after SIGINT / SIGTERM when that is `None`). How an app screenshots
    /// itself or compares a frame with a golden image in its own tests.
    ///
    /// ```no_run
    /// # extern crate lumenui_engine as lumenui;
    /// use lumenui::prelude::*;
    ///
    /// # fn demo() -> lumenui::Result<()> {
    /// let frame = lumenui::App::from_markup("<root/>")
    ///     .window_size(320, 240)
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
            self.size = Some(size);
        }
        self.run_headless_rendered(HeadlessOptions {
            dpr: handoff.dpr.unwrap_or(1.0),
            ticks: handoff.ticks,
        })
    }

    /// A bare ECS app with only the seeds, plugins, systems and handlers
    /// added here: no markup and no engine stack. For unit tests over the
    /// app's own logic; tick it by hand.
    pub fn build_bare(self) -> EcsApp {
        let mut ecs = EcsApp::new();
        {
            let mut store = ecs.world.resource_mut::<PropertyStore>();
            for (key, value) in self.seeds {
                store.set(key, value);
            }
        }
        // The lists a `<for>` renders, which a full app's stack brings.
        ecs.world
            .init_resource::<lumen_core::signals::ArraySignals>();
        // Plugins first, matching a real boot: there they run before the
        // script hosts load, which is ahead of everything added through
        // `deferred`.
        for f in self.plugins {
            f(&mut ecs);
        }
        for f in self.deferred {
            f(&mut ecs);
        }
        install_handlers(&mut ecs, self.handlers, self.ready);
        ecs
    }

    fn into_run_options(mut self) -> Result<RunSetup> {
        // The handlers install through the same app hook as everything else,
        // after the systems the app added.
        if !self.handlers.is_empty() || !self.ready.is_empty() {
            let handlers = std::mem::take(&mut self.handlers);
            let ready = std::mem::take(&mut self.ready);
            self.deferred.push(Box::new(move |ecs| {
                install_handlers(ecs, handlers, ready);
            }));
        }
        let exe = std::env::current_exe().ok();
        let (mut opts, watch) = match self.ui {
            UiInput::Compiled(ui) => {
                let dir = match self.app_dir {
                    Some(dir) => dir,
                    None => app_dir_for(exe.as_deref(), ui.built_in.as_deref())
                        .map_or_else(current_dir, Ok)?,
                };
                // Reload on an edit only while the app reads the directory it
                // was compiled from; a packaged copy has no sources to watch.
                let watch = ui.watch && ui.built_in.as_deref() == Some(dir.as_path());
                let mut opts = RunOptions::new(dir);
                opts.artifact_bytes = Some(ui.bytes.into_owned());
                (opts, watch)
            }
            UiInput::Markup(markup) => {
                let dir = match self.app_dir {
                    Some(dir) => dir,
                    None => app_dir_for(exe.as_deref(), None).map_or_else(current_dir, Ok)?,
                };
                let mut opts = RunOptions::new(dir);
                opts.markup = Some(markup);
                (opts, false)
            }
        };
        opts.hot_reload = watch;
        opts.watch_sources = watch;
        opts.plugins = self.plugins;
        opts.native_fns = self.native_fns;
        opts.title = self.title;
        if let Some(size) = self.size {
            opts.size = size;
        }
        // The compiler-plugin chain resolves here, with the rest of the
        // options, so every run entry point carries it.
        let opts = lumenc::with_default_compiler_plugins(opts).map_err(Error::Run)?;
        Ok((opts, self.seeds, self.deferred))
    }
}

fn current_dir() -> Result<PathBuf> {
    std::env::current_dir().map_err(|e| Error::Setup(format!("resolve current dir: {e}")))
}

/// Where an app reads `lumen.toml` and its assets from: the directory
/// holding `exe` when it holds a `lumen.toml` too, which is the layout
/// `lumenc package` writes, else the directory its UI was compiled from when
/// that is still there. `None` leaves the working directory.
fn app_dir_for(exe: Option<&Path>, built: Option<&Path>) -> Option<PathBuf> {
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

/// Attach the seed and deferred-system installation as an app hook, and the
/// compiler's front-end, which a debug build reloads with and
/// `set_inner_markup` parses with.
fn with_hooks(
    opts: RunOptions,
    seeds: Vec<(PropertyKey, PropertyValue)>,
    deferred: Vec<Deferred>,
) -> RunOptions {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glam::Vec2;
    use bevy_ecs::entity::Entity;
    use lumen_core::components::{LumenId, TextContent, Transform};
    use lumen_core::input::{
        ClickEvent, Key, KeyPressed, Modifiers, NamedKey, PointerButton, PointerMoved,
        PointerPressed, PointerReleased, PointerState,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Markup given as text is parsed at startup and never reloads.
    #[test]
    fn markup_is_parsed_at_startup_and_does_not_reload() {
        let (opts, ..) = App::from_markup("<root/>")
            .into_run_options()
            .expect("into_run_options");
        assert_eq!(opts.markup.as_deref(), Some("<root/>"));
        assert!(opts.artifact_bytes.is_none());
        assert!(!opts.hot_reload);
    }

    /// A compiled UI boots from its bytes; a debug build run from the
    /// directory it was compiled from watches that directory.
    #[test]
    fn a_compiled_ui_boots_from_bytes_and_reloads_where_it_was_built() {
        let dir = std::env::temp_dir().join(format!("lumenui-compiled-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ui = CompiledUi {
            bytes: std::borrow::Cow::Borrowed(b"LMNA"),
            built_in: Some(dir.clone()),
            watch: true,
        };
        let (opts, ..) = App::new(ui.clone())
            .into_run_options()
            .expect("into_run_options");
        assert_eq!(opts.artifact_bytes.as_deref(), Some(&b"LMNA"[..]));
        assert_eq!(opts.dir, dir);
        assert!(opts.hot_reload && opts.watch_sources);

        // Read from somewhere else, there is nothing of its own to watch.
        let (opts, ..) = App::new(ui)
            .app_dir(std::env::temp_dir())
            .into_run_options()
            .expect("into_run_options");
        assert!(!opts.hot_reload && !opts.watch_sources);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #525: an app reads the rest of itself from the executable's directory
    /// when it is a packaged app's, else from the directory it was compiled
    /// from, and only then from the working directory.
    #[test]
    fn an_app_finds_its_app_dir() {
        let root = std::env::temp_dir().join(format!("lumenui-app-dir-{}", std::process::id()));
        let packaged = root.join("packaged");
        let built = root.join("built");
        std::fs::create_dir_all(&packaged).unwrap();
        std::fs::create_dir_all(&built).unwrap();
        std::fs::write(packaged.join("lumen.toml"), "").unwrap();
        let exe = packaged.join("app");
        let bare_exe = built.join("target").join("app");

        assert_eq!(
            app_dir_for(Some(&exe), Some(&built)),
            Some(packaged.clone()),
            "a packaged app reads beside its executable"
        );
        assert_eq!(
            app_dir_for(Some(&bare_exe), Some(&built)),
            Some(built.clone()),
            "a build run from its target directory reads where it was built"
        );
        assert_eq!(
            app_dir_for(Some(&bare_exe), Some(&root.join("gone"))),
            None,
            "a build moved off the machine it was built on falls back"
        );
        let _ = std::fs::remove_dir_all(&root);
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
        let app = app.app_dir(&dir);
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

    /// A press and release over `at` on one tick, the way a simulated click
    /// arrives; the window's present then clears the dirty flag.
    fn click_at_and_tick(ecs: &mut EcsApp, at: Vec2) -> bool {
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

    fn click_and_tick(ecs: &mut EcsApp, id: &str) -> bool {
        let at = center_of(ecs, id);
        click_at_and_tick(ecs, at)
    }

    fn click_and_tick_entity(ecs: &mut EcsApp, entity: Entity) -> bool {
        let t = *ecs.world.get::<Transform>(entity).expect("laid out");
        click_at_and_tick(ecs, t.absolute + t.size * 0.5)
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

    /// A click handler's signal write shows on the tick the click lands, and
    /// that tick raises a frame: a headless run parks once a tick paints
    /// nothing, so a write that only surfaced a tick later would stay off
    /// screen until something else woke the app (#514).
    #[test]
    fn click_handler_write_paints_on_its_own_tick() {
        let mut ecs = headless_stack(
            App::from_markup(COUNTER)
                .signal("clicks", 0i64)
                .on_click("bump", |ui, _| ui.update("clicks", |n: i64| n + 1)),
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

    /// Every handler kind writes on the tick its event fires and raises that
    /// tick's frame: a double-click, a long press, the `on_any` slot, and a
    /// click synthesized by Enter on the focused button (#514).
    #[test]
    fn every_handler_kind_paints_on_its_own_tick() {
        let bump =
            |name: &'static str| move |ui: &mut Ui<'_>, _: &Event| ui.update(name, |n: i64| n + 1);
        let mut ecs = headless_stack(
            App::from_markup(KINDS)
                .on_any_click(bump("clicks"))
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

    /// A click inside a `<for>` row reaches the list's handler with the
    /// row's key and fields, and a list the handler writes shows on the
    /// same tick.
    #[test]
    fn a_click_in_a_row_names_the_row() {
        const LIST: &str = r#"<root padding="20" gap="10">
  <for id="layers" each="$layers" key="id" gap="4">
    <row height="30px"><button width="200px" height="30px" text="{row.name}"/></row>
  </for>
  <label id="picked" width="200px" height="30px" bind-text="picked" text="-"/>
</root>"#;
        let mut ecs = headless_stack(
            App::from_markup(LIST)
                .on_ready(|ui| {
                    ui.set_list(
                        "layers",
                        [("1", "Ink"), ("2", "Paper")].map(|(id, name)| {
                            crate::Record::new().with("id", id).with("name", name)
                        }),
                    );
                })
                .on_click("layers", |ui, ev| {
                    let row = ev.row().expect("the click is in a row");
                    let picked = format!("{} {}", row.key(), row.field("name").unwrap_or(""));
                    ui.set("picked", picked);
                }),
        );
        for _ in 0..3 {
            ecs.tick();
        }
        let list = entity_by_id(&mut ecs, "layers");
        let rows: Vec<Entity> = ecs
            .world
            .get::<bevy_ecs::hierarchy::Children>(list)
            .map(|c| c.iter().copied().collect())
            .unwrap_or_default();
        assert_eq!(rows.len(), 2, "on_ready filled the list");
        let button = ecs
            .world
            .get::<bevy_ecs::hierarchy::Children>(rows[1])
            .and_then(|c| c.first().copied())
            .expect("the row holds its button");
        click_and_tick_entity(&mut ecs, button);
        assert_eq!(text_of(&mut ecs, "picked"), "2 Paper");
    }

    /// Write `clicks` from a system the schedule runs after the `bind-text`
    /// reader, the order a system added straight to the ECS app may land in.
    fn late_signal_writer(mut clicks: MessageReader<ClickEvent>, mut signals: Signals) {
        let hits = clicks.read().count() as i64;
        if hits > 0 {
            signals.update::<i64>("clicks", |n| n + hits);
        }
    }

    /// Adds [`late_signal_writer`] past the readers, outside the app set.
    struct LateWriter;

    impl Plugin for LateWriter {
        fn build(self, app: &mut EcsApp) {
            app.add_systems(
                TickStage::Systems,
                late_signal_writer
                    .after(lumen_core::input::ClickSet::Producers)
                    .after(lumen_core::signals::apply_text_bindings),
            );
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
            App::from_markup(COUNTER)
                .signal("clicks", 0i64)
                .add_plugin(LateWriter),
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
            App::from_markup(COUNTER).add_systems(TickStage::LayoutSync, late_dom_editor),
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
}
