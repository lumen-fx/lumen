//! Decomposable plugin groups, modelled on bevy's `PluginGroup` /
//! `PluginGroupBuilder`.
//!
//! [`LumenDefaultPlugins`] is the whole batteries-included stack - markup
//! loading, taffy layout, the winit window, cosmic text, input, the hover /
//! press / drag / scroll primitives, asset loading, the optional Rhai script
//! host, and the MCP introspection server. It carries the app's markup / CSS /
//! window configuration and is the value passed to [`App::add_plugins`]:
//!
//! ```no_run
//! use lumenui::prelude::*;
//!
//! # fn demo() -> lumenui::Result<()> {
//! lumenui::App::new()
//!     .add_plugins(
//!         // Dev-hot-reload-by-default: reads from disk + watches in `cargo
//!         // run`, `include_str!`-embeds in `cargo run --release`.
//!         LumenDefaultPlugins.with_source(lumen_source!("examples/main.lmn", "examples/main.css")),
//!     )
//!     .run()
//! # }
//! ```
//!
//! [`with_source`](LumenDefaultPlugins::with_source) + [`lumen_source!`] is the
//! recommended path; [`with_markup`](LumenPluginsBuilder::with_markup) /
//! [`with_css`](LumenPluginsBuilder::with_css) stay as an always-embed escape
//! hatch and [`with_dir`](LumenPluginsBuilder::with_dir) as the explicit
//! disk-load path. See [`Source`] for the dev-vs-release model.
//!
//! ## Subtracting and composing
//!
//! Constituent plugins are addressable by marker type so the stack can be
//! trimmed - `.build().disable::<ScriptPlugin>()` yields a pure-Rust app with no
//! script host. See [`LumenPluginsBuilder::disable`] for exactly which
//! subtractions are load-bearing on the windowed boot path versus advisory.
//!
//! For your *own* plugins, [`PluginGroup`] + [`PluginGroupBuilder`] give the
//! full bevy-style mechanism: implement [`PluginGroup::build`], hand the built
//! group to [`App::add_plugins`], and `disable` / `set` / `add_after` compose
//! real [`Plugin`]s installed in order.
//!
//! [`App`]: crate::App
//! [`App::add_plugins`]: crate::App::add_plugins

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::lumen_core;

use crate::app::App;
use lumen_core::app::{App as EcsApp, Plugin};
use std::any::TypeId;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// --- Object-safe plugin wrapper ----------------------------------------------

/// Object-safe view of a [`Plugin`] so heterogeneous plugins can live in a
/// group's `Vec`. Blanket-implemented for every [`Plugin`]; you never write
/// this by hand.
pub trait BoxedPlugin: Send + 'static {
    /// Install the plugin, consuming its box.
    fn install(self: Box<Self>, app: &mut EcsApp);
    /// The concrete plugin type id, used for `disable` / `set` lookups.
    fn plugin_type_id(&self) -> TypeId;
    /// The plugin's [`Plugin::name`], for diagnostics.
    fn plugin_name(&self) -> &'static str;
}

impl<P: Plugin + Send + 'static> BoxedPlugin for P {
    fn install(self: Box<Self>, app: &mut EcsApp) {
        app.add_plugin(*self);
    }
    fn plugin_type_id(&self) -> TypeId {
        TypeId::of::<P>()
    }
    fn plugin_name(&self) -> &'static str {
        Plugin::name(self)
    }
}

// --- Generic plugin group (user-facing, bevy parity) -------------------------

/// A named, ordered collection of [`Plugin`]s that install together.
///
/// Implement it for your own bundle, then pass `MyPlugins.build()` to
/// [`App::add_plugins`](crate::App::add_plugins):
///
/// ```
/// use lumenui::prelude::*;
/// use lumenui::plugins::{PluginGroup, PluginGroupBuilder};
///
/// # struct Physics; impl lumenui::ecs_app::Plugin for Physics { fn build(self, _: &mut lumenui::ecs_app::App) {} }
/// # struct Audio; impl lumenui::ecs_app::Plugin for Audio { fn build(self, _: &mut lumenui::ecs_app::App) {} }
/// struct GamePlugins;
/// impl PluginGroup for GamePlugins {
///     fn build(self) -> PluginGroupBuilder {
///         PluginGroupBuilder::new("GamePlugins")
///             .add(Physics)
///             .add(Audio)
///     }
/// }
/// ```
pub trait PluginGroup: Sized {
    /// Enumerate the group's plugins into a [`PluginGroupBuilder`].
    fn build(self) -> PluginGroupBuilder;
}

struct GroupEntry {
    type_id: TypeId,
    name: &'static str,
    plugin: Box<dyn BoxedPlugin>,
    enabled: bool,
}

/// Ordered, editable list of plugins produced by [`PluginGroup::build`].
///
/// Mirrors bevy's `PluginGroupBuilder`: [`add`](Self::add) appends,
/// [`disable`](Self::disable) / [`enable`](Self::enable) toggle an entry by
/// type, [`set`](Self::set) swaps an entry's implementation in place, and
/// [`add_before`](Self::add_before) / [`add_after`](Self::add_after) insert
/// relative to an existing entry.
pub struct PluginGroupBuilder {
    group_name: &'static str,
    entries: Vec<GroupEntry>,
}

impl PluginGroupBuilder {
    /// Start an empty builder tagged with a group name (used in diagnostics).
    pub fn new(group_name: &'static str) -> Self {
        Self {
            group_name,
            entries: Vec::new(),
        }
    }

    fn index_of<P: 'static>(&self) -> Option<usize> {
        let tid = TypeId::of::<P>();
        self.entries.iter().position(|e| e.type_id == tid)
    }

    fn entry_of<P: Plugin + Send + 'static>(plugin: P) -> GroupEntry {
        GroupEntry {
            type_id: TypeId::of::<P>(),
            name: Plugin::name(&plugin),
            plugin: Box::new(plugin),
            enabled: true,
        }
    }

    /// Append `plugin` to the end of the group.
    #[must_use]
    #[allow(clippy::should_implement_trait)] // bevy-parity method name
    pub fn add<P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        self.entries.push(Self::entry_of(plugin));
        self
    }

    /// Insert `plugin` immediately before the existing entry of type `Target`.
    /// Falls back to appending when `Target` is absent.
    #[must_use]
    pub fn add_before<Target: 'static, P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        let entry = Self::entry_of(plugin);
        match self.index_of::<Target>() {
            Some(i) => self.entries.insert(i, entry),
            None => self.entries.push(entry),
        }
        self
    }

    /// Insert `plugin` immediately after the existing entry of type `Target`.
    /// Falls back to appending when `Target` is absent.
    #[must_use]
    pub fn add_after<Target: 'static, P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        let entry = Self::entry_of(plugin);
        match self.index_of::<Target>() {
            Some(i) => self.entries.insert(i + 1, entry),
            None => self.entries.push(entry),
        }
        self
    }

    /// Replace the entry of type `P` with a fresh instance, keeping its
    /// position. Appends when `P` is absent.
    #[must_use]
    pub fn set<P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        match self.index_of::<P>() {
            Some(i) => self.entries[i] = Self::entry_of(plugin),
            None => self.entries.push(Self::entry_of(plugin)),
        }
        self
    }

    /// Disable the entry of type `P`; it stays in the list (preserving order
    /// for later `add_before` / `add_after`) but is skipped at install.
    #[must_use]
    pub fn disable<P: 'static>(mut self) -> Self {
        if let Some(i) = self.index_of::<P>() {
            self.entries[i].enabled = false;
        }
        self
    }

    /// Re-enable a previously [`disable`](Self::disable)d entry of type `P`.
    #[must_use]
    pub fn enable<P: 'static>(mut self) -> Self {
        if let Some(i) = self.index_of::<P>() {
            self.entries[i].enabled = true;
        }
        self
    }

    /// The group's name.
    pub fn group_name(&self) -> &'static str {
        self.group_name
    }

    /// Names of the plugins that would install, in order.
    pub fn enabled_names(&self) -> impl Iterator<Item = &'static str> {
        self.entries.iter().filter(|e| e.enabled).map(|e| e.name)
    }

    /// Install every enabled plugin into `app`, in order.
    pub fn finish(self, app: &mut EcsApp) {
        for entry in self.entries {
            if entry.enabled {
                entry.plugin.install(app);
            }
        }
    }
}

// --- Constituent markers for the default stack -------------------------------

/// Identifies one slice of [`LumenDefaultPlugins`], for `disable::<T>()`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum LumenPluginId {
    /// Taffy flexbox layout.
    Layout,
    /// The winit window + GPU surface (`RenderPlugin` is its render half).
    Window,
    /// The wgpu/vello renderer.
    Render,
    /// Pointer / keyboard / focus input dispatch.
    Input,
    /// Cosmic-text shaping.
    Text,
    /// Hover / press / drag / scroll / controls / tabs / tooltip primitives.
    Primitives,
    /// Image + font asset loading.
    Assets,
    /// The Rhai script host for inline / external `<script>`.
    Script,
    /// The MCP introspection server.
    Mcp,
}

/// A constituent of [`LumenDefaultPlugins`] addressable by type in
/// `disable::<T>()`.
pub trait ConstituentPlugin: 'static {
    /// The slice this marker names.
    const ID: LumenPluginId;
}

macro_rules! constituent {
    ($(#[$m:meta])* $name:ident => $id:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $name;
        impl ConstituentPlugin for $name {
            const ID: LumenPluginId = LumenPluginId::$id;
        }
    };
}

constituent!(/// Taffy layout slice of the default stack.
    LayoutPlugin => Layout);
constituent!(/// Winit window slice of the default stack.
    WindowPlugin => Window);
constituent!(/// wgpu/vello renderer slice of the default stack.
    RenderPlugin => Render);
constituent!(/// Input-dispatch slice of the default stack.
    InputPlugin => Input);
constituent!(/// Cosmic-text slice of the default stack.
    TextPlugin => Text);
constituent!(/// Interaction-primitives slice of the default stack.
    PrimitivesPlugin => Primitives);
constituent!(/// Asset-loading slice of the default stack.
    AssetsPlugin => Assets);
constituent!(/// Rhai script-host slice of the default stack.
    ScriptPlugin => Script);
constituent!(/// MCP introspection-server slice of the default stack.
    McpPlugin => Mcp);

// --- Boot configuration threaded into the lumenc pipeline --------------------

/// How the app's frame loop is driven.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WindowMode {
    /// Open a winit window and block in its event loop.
    #[default]
    Windowed,
    /// No window / GPU; tick the schedule N times (tests, CI).
    Headless,
}

/// Everything [`App::run`](crate::App::run) needs to hand to the lumenc boot
/// pipeline. Accumulated from [`LumenDefaultPlugins`] / [`LumenPluginsBuilder`].
#[derive(Default)]
pub(crate) struct BootConfig {
    pub(crate) markup: Option<String>,
    pub(crate) css: Option<String>,
    pub(crate) dir: Option<PathBuf>,
    pub(crate) title: Option<String>,
    pub(crate) size: Option<(u32, u32)>,
    pub(crate) hot_reload: Option<bool>,
    pub(crate) mode: WindowMode,
    /// Set when [`ScriptPlugin`] is disabled: `<script>` blocks are stripped
    /// from markup before it reaches the (script-installing) lumenc pipeline.
    pub(crate) strip_script: bool,
    pub(crate) disabled: HashSet<LumenPluginId>,
}

// --- UI source: dev-hot-reload-by-default vs release-embed -------------------

/// Where the app's UI (markup + optional CSS) comes from, and *when* it is
/// read.
///
/// This is the value the [`lumen_source!`](crate::lumen_source) macro produces
/// and that [`with_source`](LumenDefaultPlugins::with_source) consumes. It
/// encodes Lumen's dev-vs-ship split directly:
///
/// * **[`Disk`](Self::Disk)** - the *debug* default. The UI stays on disk; the
///   runtime reads `main.lmn` / `main.css` from `dir` at startup and installs
///   its `notify`-based hot-reload watcher, so `cargo run` picks up on-disk
///   edits live. This is the same on-disk app-dir path `lumenc run <dir>` and
///   [`with_dir`](LumenDefaultPlugins::with_dir) already use - no new watcher.
/// * **[`Embedded`](Self::Embedded)** - the *release* default (and the explicit
///   always-embed escape hatch). The markup / CSS were baked into the binary at
///   compile time (`include_str!`); startup skips disk I/O and no watcher runs.
///
/// The point of the split is that AOT-embedding is the **ship** optimisation
/// (fast startup, no source on disk) while **dev** wants disk + hot reload -
/// and [`lumen_source!`](crate::lumen_source) picks the right one per
/// `cfg(debug_assertions)` so the same source line does both.
///
/// This mirrors how other toolkits handle it: SwiftUI / Flutter reload widgets
/// from the source tree in debug and freeze them into the app bundle for
/// release; Dioxus / Leptos watch the crate's asset dir under `dx serve` but
/// `include_str!`-embed for a shipped binary. Lumen's twist is that the
/// *runtime* already owns the watcher, so the SDK only has to choose disk vs
/// embed.
#[derive(Clone, Debug)]
pub enum Source {
    /// Debug default: read `main.lmn` / `main.css` from this directory at
    /// runtime and hot-reload them via the runtime watcher.
    ///
    /// Follows the app-dir convention (`lumenc run <dir>`): the entry markup is
    /// `main.lmn` and the stylesheet, if any, is `main.css`. The macro derives
    /// this directory from the markup file's compile-time path.
    Disk {
        /// Directory containing `main.lmn` (+ optional `main.css`).
        dir: PathBuf,
    },
    /// Release default (and explicit always-embed): UI baked in at compile
    /// time.
    ///
    // TODO(aot): once `lumenc build` lands (bincode `LMNA` = LayoutIR + script,
    // behind the `runtime-parse` feature), the release payload should carry the
    // *compiled* artifact instead of raw source text - add a
    // `Compiled { lmna: Vec<u8> }` variant (or an `lmna: Option<Vec<u8>>` field)
    // and have the boot path hand it to the parser-free runtime. Embedding the
    // raw source string here is the interim payload and keeps this seam small.
    Embedded {
        /// Markup baked in via `include_str!`.
        markup: String,
        /// Optional stylesheet baked in via `include_str!`.
        css: Option<String>,
    },
}

impl Source {
    /// Build a [`Disk`](Self::Disk) source from a markup *file* path, using its
    /// parent directory as the app dir. Used by
    /// [`lumen_source!`](crate::lumen_source) in debug builds; the file itself
    /// is not read here - the runtime loads `main.lmn` / `main.css` from the
    /// derived directory and watches them.
    pub fn disk(markup_file: impl AsRef<Path>) -> Self {
        let path = markup_file.as_ref();
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Source::Disk { dir }
    }

    /// Build an [`Embedded`](Self::Embedded) source from compile-time markup and
    /// optional CSS. Used by [`lumen_source!`](crate::lumen_source) in release
    /// builds, and directly as the explicit always-embed escape hatch.
    pub fn embedded(markup: impl Into<String>, css: Option<impl Into<String>>) -> Self {
        Source::Embedded {
            markup: markup.into(),
            css: css.map(Into::into),
        }
    }
}

/// Resolve a UI [`Source`] to the right dev/release loading strategy, picked at
/// compile time by `cfg(debug_assertions)`.
///
/// * **Debug** (`cargo run`): expands to a [`Source::Disk`] over the markup
///   file's directory (resolved from `CARGO_MANIFEST_DIR` at compile time). The
///   UI is read from disk at startup and hot-reloaded live by the runtime
///   watcher - edit `main.lmn` / `main.css` and the running window updates.
/// * **Release** (`cargo run --release`): expands to a [`Source::Embedded`]
///   that `include_str!`-bakes the markup (and CSS, if given) into the binary.
///   No disk read, no watcher.
///
/// The same source line does both - you never branch on the build profile.
///
/// The disk/hot-reload path follows the app-dir convention: name the markup
/// `main.lmn` and the stylesheet `main.css` and keep them in one directory, so
/// the runtime's `main.lmn` / `main.css` lookup resolves. In release the exact
/// named files are embedded.
///
/// ```no_run
/// use lumenui::prelude::*;
///
/// # fn demo() -> lumenui::Result<()> {
/// lumenui::App::new()
///     .add_plugins(
///         LumenDefaultPlugins
///             // hot-reloads in `cargo run`, embedded in `cargo run --release`
///             .with_source(lumen_source!("examples/main.lmn", "examples/main.css")),
///     )
///     .run()
/// # }
/// ```
///
/// Markup-only form (no stylesheet):
///
/// ```no_run
/// # use lumenui::prelude::*;
/// # fn demo() -> lumenui::Result<()> {
/// # lumenui::App::new().add_plugins(
/// LumenDefaultPlugins.with_source(lumen_source!("examples/main.lmn"))
/// # ).run() }
/// ```
#[macro_export]
macro_rules! lumen_source {
    ($markup:literal) => {{
        #[cfg(debug_assertions)]
        let __lumen_src = $crate::plugins::Source::disk(::core::concat!(
            ::core::env!("CARGO_MANIFEST_DIR"),
            "/",
            $markup
        ));
        #[cfg(not(debug_assertions))]
        let __lumen_src = $crate::plugins::Source::embedded(
            ::core::include_str!(::core::concat!(
                ::core::env!("CARGO_MANIFEST_DIR"),
                "/",
                $markup
            )),
            ::core::option::Option::<&str>::None,
        );
        __lumen_src
    }};
    ($markup:literal, $css:literal) => {{
        #[cfg(debug_assertions)]
        let __lumen_src = $crate::plugins::Source::disk(::core::concat!(
            ::core::env!("CARGO_MANIFEST_DIR"),
            "/",
            $markup
        ));
        #[cfg(not(debug_assertions))]
        let __lumen_src = $crate::plugins::Source::embedded(
            ::core::include_str!(::core::concat!(
                ::core::env!("CARGO_MANIFEST_DIR"),
                "/",
                $markup
            )),
            ::core::option::Option::Some(::core::include_str!(::core::concat!(
                ::core::env!("CARGO_MANIFEST_DIR"),
                "/",
                $css
            ))),
        );
        __lumen_src
    }};
}

// --- LumenDefaultPlugins + its builder ---------------------------------------

/// The full Lumen plugin stack. See the [module docs](self).
///
/// Use it as a value - every configuration method consumes it and returns a
/// [`LumenPluginsBuilder`]: `LumenDefaultPlugins.with_markup(..)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LumenDefaultPlugins;

impl LumenDefaultPlugins {
    /// Set the UI [`Source`] - the recommended, dev-vs-release-aware entry
    /// point. Pair it with [`lumen_source!`](crate::lumen_source) for automatic
    /// hot reload in `cargo run` and embedding in `cargo run --release`:
    ///
    /// ```no_run
    /// # use lumenui::prelude::*;
    /// # fn demo() -> lumenui::Result<()> {
    /// # lumenui::App::new().add_plugins(
    /// LumenDefaultPlugins.with_source(lumen_source!("examples/main.lmn", "examples/main.css"))
    /// # ).run() }
    /// ```
    pub fn with_source(self, source: Source) -> LumenPluginsBuilder {
        LumenPluginsBuilder::default().with_source(source)
    }
    /// Embed this markup string as the UI (`include_str!("main.lmn")`).
    ///
    /// The explicit always-embed escape hatch: the contents are frozen into the
    /// binary regardless of build profile, so there is no hot reload. For the
    /// dev-hot-reload-by-default path prefer
    /// [`with_source`](Self::with_source) + [`lumen_source!`](crate::lumen_source).
    pub fn with_markup(self, src: impl Into<String>) -> LumenPluginsBuilder {
        LumenPluginsBuilder::default().with_markup(src)
    }
    /// Embed this stylesheet (`include_str!("main.css")`).
    pub fn with_css(self, src: impl Into<String>) -> LumenPluginsBuilder {
        LumenPluginsBuilder::default().with_css(src)
    }
    /// Load the UI from an on-disk app directory (`lumenc run <dir>` layout).
    pub fn with_dir(self, dir: impl Into<PathBuf>) -> LumenPluginsBuilder {
        LumenPluginsBuilder::default().with_dir(dir)
    }
    /// Set the window title.
    pub fn with_title(self, title: impl Into<String>) -> LumenPluginsBuilder {
        LumenPluginsBuilder::default().with_title(title)
    }
    /// Set the initial window size in physical pixels.
    pub fn with_size(self, width: u32, height: u32) -> LumenPluginsBuilder {
        LumenPluginsBuilder::default().with_size(width, height)
    }
    /// Enter the disable/enable surface with the full stack selected.
    pub fn build(self) -> LumenPluginsBuilder {
        LumenPluginsBuilder::default()
    }
    /// Add an extra [`Plugin`] alongside the default stack.
    #[allow(clippy::should_implement_trait)] // bevy-parity method name
    pub fn add<P: Plugin + Send + 'static>(self, plugin: P) -> LumenPluginsBuilder {
        LumenPluginsBuilder::default().add(plugin)
    }
}

/// Configured form of [`LumenDefaultPlugins`]: carries the markup / CSS / window
/// settings plus the set of disabled constituents and any extra user plugins.
#[derive(Default)]
pub struct LumenPluginsBuilder {
    boot: BootConfig,
    extra: Vec<Box<dyn BoxedPlugin>>,
}

impl LumenPluginsBuilder {
    /// Set the UI [`Source`] - dev-vs-release-aware. A [`Source::Disk`] (the
    /// debug branch of [`lumen_source!`](crate::lumen_source)) points the boot
    /// at an on-disk app dir so the runtime loads + hot-reloads it; a
    /// [`Source::Embedded`] (the release branch, or the explicit escape hatch)
    /// bakes the markup / CSS in.
    #[must_use]
    pub fn with_source(mut self, source: Source) -> Self {
        match source {
            // Leave markup/css unset so the boot path disk-loads from `dir`
            // and (hot_reload defaulting to on for a dir source) installs the
            // runtime watcher.
            Source::Disk { dir } => {
                self.boot.dir = Some(dir);
                self.boot.markup = None;
                self.boot.css = None;
            }
            Source::Embedded { markup, css } => {
                self.boot.markup = Some(markup);
                if css.is_some() {
                    self.boot.css = css;
                }
            }
        }
        self
    }
    /// Embed this markup string as the UI.
    ///
    /// Always-embed escape hatch: frozen into the binary regardless of profile,
    /// so no hot reload. Prefer [`with_source`](Self::with_source) +
    /// [`lumen_source!`](crate::lumen_source) for dev hot reload.
    #[must_use]
    pub fn with_markup(mut self, src: impl Into<String>) -> Self {
        self.boot.markup = Some(src.into());
        self
    }
    /// Embed this stylesheet.
    #[must_use]
    pub fn with_css(mut self, src: impl Into<String>) -> Self {
        self.boot.css = Some(src.into());
        self
    }
    /// Load the UI from an on-disk app directory.
    #[must_use]
    pub fn with_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.boot.dir = Some(dir.into());
        self
    }
    /// Set the window title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.boot.title = Some(title.into());
        self
    }
    /// Set the initial window size in physical pixels.
    #[must_use]
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.boot.size = Some((width, height));
        self
    }
    /// Force hot reload on or off. Defaults to on for [`with_dir`](Self::with_dir),
    /// off for embedded markup.
    #[must_use]
    pub fn hot_reload(mut self, enabled: bool) -> Self {
        self.boot.hot_reload = Some(enabled);
        self
    }
    /// Add an extra [`Plugin`] alongside the default stack.
    #[must_use]
    #[allow(clippy::should_implement_trait)] // bevy-parity method name
    pub fn add<P: Plugin + Send + 'static>(mut self, plugin: P) -> Self {
        self.extra.push(Box::new(plugin));
        self
    }

    /// Passthrough for bevy-parity chaining (`.build().disable::<..>()`).
    #[must_use]
    pub fn build(self) -> Self {
        self
    }

    /// Disable a constituent of the stack.
    ///
    /// Honoured concretely on the windowed boot path for:
    /// * [`WindowPlugin`] / [`RenderPlugin`] - switches to the headless frame
    ///   loop (no window, no GPU); pair with
    ///   [`App::run_headless`](crate::App::run_headless).
    /// * [`ScriptPlugin`] - strips `<script>` blocks from the markup so the
    ///   Rhai host is never installed (a pure-Rust app).
    ///
    /// The remaining backend markers ([`LayoutPlugin`], [`InputPlugin`],
    /// [`PrimitivesPlugin`], [`AssetsPlugin`], [`TextPlugin`], [`McpPlugin`])
    /// are installed by the shared lumenc pipeline and are recorded but
    /// advisory on the windowed path; they take effect when you build a bare
    /// app with [`App::build_bare`](crate::App::build_bare) and install slices
    /// yourself. All markers, including these, fully compose in a user
    /// [`PluginGroupBuilder`].
    #[must_use]
    pub fn disable<C: ConstituentPlugin>(mut self) -> Self {
        self.boot.disabled.insert(C::ID);
        match C::ID {
            LumenPluginId::Window | LumenPluginId::Render => self.boot.mode = WindowMode::Headless,
            LumenPluginId::Script => self.boot.strip_script = true,
            _ => {}
        }
        self
    }

    /// Re-enable a previously disabled constituent.
    #[must_use]
    pub fn enable<C: ConstituentPlugin>(mut self) -> Self {
        self.boot.disabled.remove(&C::ID);
        match C::ID {
            LumenPluginId::Window | LumenPluginId::Render => self.boot.mode = WindowMode::Windowed,
            LumenPluginId::Script => self.boot.strip_script = false,
            _ => {}
        }
        self
    }

    /// Select the headless frame loop (equivalent to disabling
    /// [`WindowPlugin`]).
    #[must_use]
    pub fn headless(mut self) -> Self {
        self.boot.mode = WindowMode::Headless;
        self.boot.disabled.insert(LumenPluginId::Window);
        self.boot.disabled.insert(LumenPluginId::Render);
        self
    }

    /// Consume into `app`'s boot state.
    pub(crate) fn write_into(self, app: &mut App) {
        app.merge_boot(self.boot);
        for plugin in self.extra {
            app.push_deferred(move |ecs| plugin.install(ecs));
        }
    }
}

// --- AppPlugins: what App::add_plugins accepts -------------------------------

/// Anything installable via [`App::add_plugins`](crate::App::add_plugins):
/// [`LumenDefaultPlugins`], a [`LumenPluginsBuilder`], a user
/// [`PluginGroupBuilder`], or a tuple of the above.
pub trait AppPlugins {
    /// Apply this configuration / plugin set to `app`.
    fn apply(self, app: &mut App);
}

impl AppPlugins for LumenDefaultPlugins {
    fn apply(self, app: &mut App) {
        LumenPluginsBuilder::default().write_into(app);
    }
}

impl AppPlugins for LumenPluginsBuilder {
    fn apply(self, app: &mut App) {
        self.write_into(app);
    }
}

impl AppPlugins for PluginGroupBuilder {
    fn apply(self, app: &mut App) {
        app.push_deferred(move |ecs| self.finish(ecs));
    }
}

macro_rules! tuple_app_plugins {
    ($($T:ident),+) => {
        impl<$($T: AppPlugins),+> AppPlugins for ($($T,)+) {
            #[allow(non_snake_case)]
            fn apply(self, app: &mut App) {
                let ($($T,)+) = self;
                $($T.apply(app);)+
            }
        }
    };
}
tuple_app_plugins!(A);
tuple_app_plugins!(A, B);
tuple_app_plugins!(A, B, C);
tuple_app_plugins!(A, B, C, D);
tuple_app_plugins!(A, B, C, D, E);
