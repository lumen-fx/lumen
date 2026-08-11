//! High-level application builder mirroring the `lumenc run` boot path.

use crate::error::{Error, Result};
use crate::events::{EventCtx, EventKind, HandlerEntry, install_rust_handlers};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_runtime::RunOptions;
use std::path::PathBuf;

/// Callback applied to the fully-built ECS app before the event loop.
type ConfigureHook = Box<dyn FnOnce(&mut lumen_core::app::App) + Send + 'static>;

/// Rhai engine extension callback (parity with `lumen_app_expose`).
type RhaiExtension = Box<dyn FnOnce(&mut rhai::Engine) + Send + 'static>;

/// Entry point for building a Lumen application in Rust.
///
/// This is a facade over [`AppBuilder`]; start with [`App::builder`]:
///
/// ```no_run
/// use lumenui::simple::App;
///
/// fn main() -> lumenui::Result<()> {
///     App::builder()
///         .markup("<root><label id=\"l\" bind-text=\"msg\" text=\"hi\" /></root>")
///         .title("Quickstart")
///         .property("msg", "hello")
///         .on_click("l", |ctx| ctx.set("msg", "clicked"))
///         .run()
/// }
/// ```
///
/// Not to be confused with the ECS-first [`crate::App`], the primary entry
/// point that adds plugin groups and real systems. Reach the low-level ECS
/// app from here through [`AppBuilder::configure`].
pub struct App;

impl App {
    /// Start building an application. See [`AppBuilder`] for the
    /// available knobs.
    pub fn builder() -> AppBuilder {
        AppBuilder::default()
    }
}

/// Builder for a Lumen application.
///
/// Collects markup/CSS sources, window options, initial signal values,
/// and native Rust event handlers, then [`AppBuilder::run`] boots the
/// exact plugin stack `lumenc run` uses (taffy layout, winit window,
/// cosmic text, input, primitives, assets, optional Rhai script host,
/// optional MCP introspection server) and enters the event loop.
#[derive(Default)]
pub struct AppBuilder {
    dir: Option<PathBuf>,
    markup: Option<String>,
    css: Option<String>,
    title: Option<String>,
    size: Option<(u32, u32)>,
    hot_reload: Option<bool>,
    seeds: Vec<(String, PropertyValue)>,
    handlers: Vec<HandlerEntry>,
    rhai_extensions: Vec<RhaiExtension>,
    configure: Vec<ConfigureHook>,
}

impl AppBuilder {
    /// Use this in-memory markup string as the app's UI. The idiomatic
    /// call is `.markup(include_str!("main.lmn"))` so the markup ships
    /// inside the binary. Disables hot reload (there is no file to
    /// watch). Either this or [`Self::dir`] must be provided.
    pub fn markup(mut self, src: impl Into<String>) -> Self {
        self.markup = Some(src.into());
        self
    }

    /// Use this in-memory stylesheet. Typically
    /// `.css(include_str!("main.css"))`. Optional - markup-only apps and
    /// `<root skin="default">` both work without it.
    pub fn css(mut self, src: impl Into<String>) -> Self {
        self.css = Some(src.into());
        self
    }

    /// Run an on-disk app directory (containing `main.lmn`, optional
    /// `main.css` / `lumen.toml`) - the native equivalent of
    /// `lumenc run <dir>` and of the C ABI's `lumen_app_new(dir)`.
    ///
    /// When combined with [`Self::markup`], the directory is still used
    /// to resolve relative asset paths and `lumen.toml`, but the markup
    /// string wins over `<dir>/main.lmn`.
    pub fn dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    /// Window title. Defaults to `lumen.toml [window] title`, then the
    /// app directory name.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Initial window size in physical pixels. Defaults to 960 x 720
    /// (overridable by `lumen.toml [window] size`).
    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.size = Some((width, height));
        self
    }

    /// Explicitly enable or disable hot reload. Defaults to `true` for
    /// [`Self::dir`]-based apps and is always off for in-memory
    /// [`Self::markup`] (nothing on disk to watch).
    pub fn hot_reload(mut self, enabled: bool) -> Self {
        self.hot_reload = Some(enabled);
        self
    }

    /// Seed a global signal before the first tick. Accepts anything
    /// convertible into a [`PropertyValue`] (`i64`, `f64`, `bool`,
    /// `&str`, `String`, [`lumen_core::components::Color`], ...), stored
    /// typed - no stringification. `bind-*` markup observes the value on
    /// the first frame.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<PropertyValue>) -> Self {
        self.seeds.push((name.into(), value.into()));
        self
    }

    /// Register a click handler for the element with `id="..."`. The
    /// closure receives an [`EventCtx`] for typed signal access; writes
    /// are reflected by bound markup on the same tick. A per-id handler
    /// overrides [`Self::on_any_click`] for its id.
    pub fn on_click<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::Click, Some(id.into()), handler)
    }

    /// Register a fallback click handler that fires for every element
    /// without a dedicated [`Self::on_click`] registration. Use
    /// [`EventCtx::target`] to inspect which element was clicked.
    pub fn on_any_click<F>(self, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::Click, None, handler)
    }

    /// Register a double-click handler for the element with `id="..."`.
    /// On a double-click tick the plain click for the same element is
    /// suppressed (one double, not two clicks plus a double).
    pub fn on_double_click<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::DoubleClick, Some(id.into()), handler)
    }

    /// Fallback double-click handler (see [`Self::on_any_click`]).
    pub fn on_any_double_click<F>(self, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::DoubleClick, None, handler)
    }

    /// Register a long-press handler for the element with `id="..."`.
    pub fn on_long_press<F>(self, id: impl Into<String>, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::LongPress, Some(id.into()), handler)
    }

    /// Fallback long-press handler (see [`Self::on_any_click`]).
    pub fn on_any_long_press<F>(self, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.on(EventKind::LongPress, None, handler)
    }

    /// Register a handler for an arbitrary `(kind, id)` pair; `None` id
    /// is the wildcard slot. The named `on_*` methods are sugar over
    /// this.
    pub fn on<F>(mut self, kind: EventKind, id: Option<String>, handler: F) -> Self
    where
        F: FnMut(&mut EventCtx<'_>) + Send + Sync + 'static,
    {
        self.handlers.push((kind, id, Box::new(handler)));
        self
    }

    /// Install native functions into the Rhai script engine, for apps
    /// mixing `<script>` markup with Rust. The native equivalent of the
    /// C ABI's `lumen_app_expose`:
    /// `engine.register_fn("now_ms", || 42_i64)`.
    pub fn rhai_extension<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut rhai::Engine) + Send + 'static,
    {
        self.rhai_extensions.push(Box::new(f));
        self
    }

    /// Full-power escape hatch: run a closure against the built ECS
    /// [`lumen_core::app::App`] (add [`lumen_core::app::Plugin`]s,
    /// register systems, insert resources) after the default stack is
    /// wired and before the event loop starts.
    pub fn configure<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut lumen_core::app::App) + Send + 'static,
    {
        self.configure.push(Box::new(f));
        self
    }

    /// Boot the app and enter the window event loop. Blocks until the
    /// window closes; returns any setup or runtime error.
    pub fn run(self) -> Result<()> {
        let in_memory = self.markup.is_some();
        let dir = match self.dir {
            Some(d) => d,
            None if in_memory => std::env::current_dir()
                .map_err(|e| Error::Setup(format!("resolve current dir: {e}")))?,
            None => {
                return Err(Error::Setup(
                    "no UI source: call .markup(include_str!(\"main.lmn\")) or .dir(<app dir>)"
                        .into(),
                ));
            }
        };

        let mut opts = RunOptions::new(dir);
        opts.title = self.title;
        if let Some(size) = self.size {
            opts.size = size;
        }
        opts.markup = self.markup;
        opts.css = self.css;
        opts.hot_reload = self.hot_reload.unwrap_or(!in_memory);
        for ext in self.rhai_extensions {
            opts.rhai_extensions.push(ext);
        }

        let seeds = self.seeds;
        let handlers = self.handlers;
        let configure = self.configure;
        opts = opts.with_app_hook(move |app| {
            // Seed initial signal values before the first tick so
            // `bind-*` markup renders them on the first frame.
            if !seeds.is_empty() {
                let mut store = app.world.resource_mut::<PropertyStore>();
                for (name, value) in seeds {
                    store.set(PropertyKey::global(name.as_str()), value);
                }
            }
            // Native handler dispatch. The shared installer schedules the
            // collect -> dispatch pipeline before the reactive binding
            // readers so a handler's signal write is reflected by bound
            // markup on the tick the event fired - the Rust mirror of the
            // script path's same-tick commit.
            install_rust_handlers(app, handlers);
            for f in configure {
                f(app);
            }
        });

        // Inject the compiler's front-end so the runtime can parse the app's
        // markup / CSS from source (it links no parser itself).
        opts = opts.with_parser(lumenc::default_parser());
        lumen_runtime::run_app(opts).map_err(Error::Run)
    }
}
