//! Launches a pure-markup Lumen app via `lumenc run <dir>`.
//!
//! Expects:
//!
//! ```text
//! <dir>/
//!   main.lmn     # required: markup (with optional inline <script>)
//!   main.css     # optional: stylesheet
//! ```
//!
//! - Wires the default plugin stack: taffy layout, winit window, cosmic text, input, hover/press/drag/scroll primitives, optional Rhai script host, optional MCP server.
//! - Runs winit's event loop and returns when the window closes or a fatal error occurs.
//! - Hot reload: a `notify` file watcher (inotify / FSEvents / ReadDirectoryChangesW) covers `main.lmn`, `main.css`, and every included / imported source; an fs event wakes the loop for one tick and the `hot_reload` system re-checks mtimes. On change it despawns the spawned root, re-parses, re-applies CSS, re-spawns, and reloads the Rhai script against a fresh `Scope`. Parse errors keep the previous tree intact and log to stderr. `LUMEN_HOT_RELOAD_POLL=1` (or watcher init failure) falls back to the legacy 300 ms mtime poll.
//! - Script commands: `SetText` updates the matching `LumenId`'s [`TextContent`]; other variants (`Print`, `AddClicks`, `SetString`) no-op here.

use bevy_ecs::component::Mutable;
use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_assets::AssetsPlugin;
use lumen_core::prelude::*;
use lumen_input::InputPlugin;
use lumen_layout_taffy::TaffyLayoutPlugin;
#[cfg(feature = "mcp")]
use lumen_mcp::LumenMcpPlugin;
use lumen_os_filedialog::{FileDialogKind, FileDialogRequest, FileDialogService};
use lumen_os_hotkey::HotkeyRegistry as OsHotkeyRegistry;
use lumen_os_notify::NotificationService;
use lumen_os_tray::{TrayConfig as OsTrayConfig, TrayService as OsTrayService};
use lumen_primitives::{
    CheckboxPlugin, ControlsPlugin, DragPlugin, HoverTintPlugin, PressPlugin, ProgressPlugin,
    RadioPlugin, ScrollPlugin, TabsPlugin, TooltipPlugin, TransitionPlugin, ValidationPlugin,
};
use lumen_script::ScriptCommand;
use lumen_script::ScriptHost;
#[cfg(feature = "host-candela")]
use lumen_script_candela::{CandelaHost, ScriptCandelaPlugin};
#[cfg(feature = "host-lua")]
use lumen_script_lua::{LuaHost, ScriptLuaPlugin};
use lumen_script_rhai::{RhaiHost, ScriptRhaiPlugin};
// The host-generic script systems live in `lumen-script` and are
// re-exported by both host crates. Importing them from the runtime crate
// means a single generic `::<H>` path resolves for whichever `ScriptHost`
// the `[script] engine` key selects (Rhai or Lua).
// `ScriptSet` is how the host-neutral half orders against them: with several
// hosts installed, an edge naming one host's system leaves the others outside
// the one-tick dirty window.
use lumen_script::{ScriptCommandEvent, ScriptSet, fire_on_ready, reload_script};
use lumen_text_cosmic::CosmicShaper;
use lumen_window_winit::{WinitOptions, run};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
// `Duration` / `Instant` are only used by the (gated) hot-reload poll throttle.
#[cfg(feature = "runtime-parse")]
use std::time::{Duration, Instant};

use crate::source_parser::SourceParser;
use lumen_ir::layout_ir::{Attributes, Element};

/// A single `rhai::Engine` extension callback; factored into an alias to
/// keep clippy's `type_complexity` lint quiet.
type RhaiExtension = Box<dyn FnOnce(&mut rhai::Engine) + Send + 'static>;

/// An embedder callback invoked on the fully-built [`App`] right before
/// the window event loop starts. Every default plugin and system is
/// already registered at that point, so a hook can insert resources and
/// add systems ordered against the default stack's public systems (for
/// example `.before(lumen_core::signals::apply_text_bindings)`).
///
/// This is the native-Rust counterpart of [`RunOptions::rhai_extensions`]:
/// the Rust SDK (`sdk/rust`, crate `lumen`) uses it to wire Rust-closure
/// event handlers and whole `bevy_ecs` systems into the tick without lumenc
/// knowing about them.
///
/// Not `Send`: hooks are drained and invoked in [`build_app`] on the calling
/// thread (see the loop near the end of that fn), never moved across threads.
/// The bound is intentionally omitted so the SDK can defer `IntoScheduleConfigs`
/// values - `system.chain()` / `system.run_if(..)` box into `!Send`
/// `ScheduleConfigs`, which a `Send` hook could not capture.
pub type AppHook = Box<dyn FnOnce(&mut App) + 'static>;

/// Options for `lumenc run`.
pub struct RunOptions {
    /// Path to the app directory (must contain `main.lmn`).
    pub dir: PathBuf,
    /// Window title. Defaults to the directory name.
    pub title: Option<String>,
    /// Window size in logical pixels.
    pub size: (u32, u32),
    /// Background color when no CSS sets one.
    pub clear: Color,
    /// Watch source files and reload on change. On by default.
    pub hot_reload: bool,
    /// Native Rhai extensions installed before script compile. Each
    /// closure receives the inner `rhai::Engine` and can `register_fn`
    /// app-specific builtins backed by Rust crates / FFI. Lumen ships
    /// only UI primitives; OS-level integrations live in the embedding
    /// binary (see `apps/sysmon` for a worked example using
    /// `sysinfo`).
    pub rhai_extensions: Vec<RhaiExtension>,
    /// In-memory markup source. When `Some`, the runtime parses this
    /// string instead of reading `<dir>/main.lmn` from disk, and hot
    /// reload is disabled (there is no file to watch). Set by the Rust
    /// SDK's `include_str!`-based embedding path; `dir` is still used
    /// to resolve relative asset paths and `lumen.toml`.
    pub markup: Option<String>,
    /// In-memory stylesheet source. When `Some`, used instead of
    /// `<dir>/main.css`. Independent of [`Self::markup`]; `None` falls
    /// back to the on-disk lookup.
    pub css: Option<String>,
    /// Callbacks invoked on the fully-built [`App`] just before the
    /// event loop starts. See [`AppHook`].
    pub app_hooks: Vec<AppHook>,
    /// Load a precompiled AOT artifact (`lumenc build`) instead of parsing
    /// `<dir>/main.lmn` + `main.css` from source. When `Some`, the parser is
    /// bypassed entirely (and hot reload is disabled); `dir` is still used to
    /// resolve `lumen.toml`. Required for a runtime built without the
    /// `runtime-parse` feature.
    pub artifact: Option<PathBuf>,
    /// Load a precompiled AOT artifact from in-memory bytes instead of a file
    /// path. The link-not-embed launcher path: the compiler produces LMNA
    /// bytes in-process and hands them across the C-ABI
    /// (`lumen_app_new_from_lmna`) so the runtime never touches the parser or a
    /// source file. When `Some`, it wins over [`Self::artifact`] and
    /// [`Self::parser`]; `dir` is still used to resolve relative asset paths.
    pub artifact_bytes: Option<Vec<u8>>,
    /// True for a headless / bounded automation run (`--headless`, and the
    /// FFI/test `run_app_headless` contract). Suppresses the long-lived
    /// interactive daemons that only make sense for a windowed session: the
    /// MCP introspection server (unless `[mcp] simulate = true`, which
    /// automation drivers set) and the hot-reload file watcher. Off by
    /// default so an ordinary `lumenc run` keeps both. See [`build_app`].
    pub bounded: bool,
    /// Injected markup/CSS front-end used for dev source-load and hot-reload
    /// re-parse. `lumen-runtime` links no parser itself (it stays in the
    /// compiler); the CLI / SDK / FFI dev paths populate this with a
    /// [`SourceParser`] impl (`lumenc`'s `LumencParser`). `None` is valid for
    /// the precompiled-artifact path ([`Self::artifact`]) and for a runtime
    /// that only ever loads AOT artifacts; a from-source run with no parser
    /// fails with [`RunError::ParserDisabled`].
    pub parser: Option<Box<dyn SourceParser>>,
}

impl RunOptions {
    /// Built-in default window size - kept as a constant so the runtime
    /// can detect that the caller didn't override it and let
    /// `lumen.toml`'s `[window] size` win instead.
    pub const DEFAULT_SIZE: (u32, u32) = (960, 720);

    /// Construct with sensible defaults.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            title: None,
            size: Self::DEFAULT_SIZE,
            // Single source of truth for the fallback: see
            // `lumen_window_winit::DEFAULT_CLEAR`. `build_app` overrides this
            // with the resolved `--lumen-window-bg` custom property when the
            // app or its active skin defines one.
            clear: lumen_window_winit::DEFAULT_CLEAR,
            hot_reload: true,
            rhai_extensions: Vec::new(),
            markup: None,
            css: None,
            app_hooks: Vec::new(),
            artifact: None,
            artifact_bytes: None,
            bounded: false,
            parser: None,
        }
    }

    /// Builder: inject the markup/CSS front-end used for dev source-load and
    /// hot-reload re-parse. See [`Self::parser`].
    pub fn with_parser(mut self, parser: Box<dyn SourceParser>) -> Self {
        self.parser = Some(parser);
        self
    }

    /// Builder: load a precompiled AOT [`lumen_ir::artifact`] instead of parsing
    /// source. See [`Self::artifact`].
    pub fn with_artifact(mut self, path: impl Into<PathBuf>) -> Self {
        self.artifact = Some(path.into());
        self
    }

    /// Builder: load a precompiled AOT [`lumen_ir::artifact`] from in-memory
    /// bytes instead of a file path. See [`Self::artifact_bytes`].
    pub fn with_artifact_bytes(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.artifact_bytes = Some(bytes.into());
        self
    }

    /// Builder: install a callback that registers native Rhai
    /// builtins from the embedding binary. See [`RunOptions`].
    pub fn with_rhai_extension<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut rhai::Engine) + Send + 'static,
    {
        self.rhai_extensions.push(Box::new(f));
        self
    }

    /// Builder: parse this in-memory markup string instead of reading
    /// `<dir>/main.lmn` from disk. Disables hot reload. See
    /// [`Self::markup`].
    pub fn with_markup(mut self, src: impl Into<String>) -> Self {
        self.markup = Some(src.into());
        self
    }

    /// Builder: apply this in-memory stylesheet instead of reading
    /// `<dir>/main.css` from disk. See [`Self::css`].
    pub fn with_css(mut self, src: impl Into<String>) -> Self {
        self.css = Some(src.into());
        self
    }

    /// Builder: install a callback invoked on the fully-built [`App`]
    /// right before the event loop starts. See [`AppHook`].
    pub fn with_app_hook<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut App) + 'static,
    {
        self.app_hooks.push(Box::new(f));
        self
    }
}

/// Convenience entry point for embedding apps. Wraps [`run_app`] with
/// a single closure that registers native Rhai functions on top of
/// Lumen's defaults - the minimal-boilerplate path for shipping a
/// custom Lumen app that links one extra Rust crate (sysinfo, hyper,
/// rusqlite, anything).
///
/// Note: the bare `lumen_runtime::run_with` links no markup parser - a
/// from-source run needs one injected via [`RunOptions::with_parser`]. The
/// compiler (`lumenc::run_with`) and the SDKs wire the default parser for you.
///
/// ```no_run
/// fn main() -> Result<(), Box<dyn std::error::Error>> {
///     lumen_runtime::run_with(env!("CARGO_MANIFEST_DIR"), |engine| {
///         engine.register_fn("now_ms", || 42_i64);
///     })?;
///     Ok(())
/// }
/// ```
pub fn run_with<F>(dir: impl Into<PathBuf>, extend: F) -> Result<(), RunError>
where
    F: FnOnce(&mut rhai::Engine) + Send + 'static,
{
    run_app(RunOptions::new(dir).with_rhai_extension(extend))
}

/// Errors raised while preparing the app.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// Required input file missing or unreadable.
    #[error("read {0}: {1}")]
    Read(PathBuf, std::io::Error),
    /// Markup failed to parse.
    #[error("parse main.lmn: {0}")]
    ParseHtml(String),
    /// CSS failed to parse.
    #[error("parse main.css: {0}")]
    ParseCss(String),
    /// CSS application failed.
    #[error("apply CSS: {0}")]
    ApplyCss(String),
    /// winit returned an error.
    #[error("window: {0}")]
    Window(String),
    /// Headless mode failed to initialise (offscreen GPU context or
    /// signal-handler install). Raised only by
    /// [`crate::run_headless::run_app_headless_rendered`].
    #[error("headless: {0}")]
    Headless(String),
    /// The app's script failed to compile. Raised by [`check_app`],
    /// which compiles the combined `<script>` source with the exact
    /// engine settings `lumenc run` loads with - a script that would
    /// die at load fails the check instead of false-passing.
    #[error("script: {0}")]
    Script(String),
    /// `lumen.toml` is invalid (parse / read error).
    #[error("lumen.toml: {0}")]
    Config(#[from] crate::config::ConfigError),
    /// A `locale/*.ftl` catalogue could not be read or parsed, or its
    /// filename is not a BCP-47 tag.
    #[error("i18n: {0}")]
    I18n(String),
    /// A precompiled AOT artifact failed to read / decode.
    #[error("artifact: {0}")]
    Artifact(String),
    /// The runtime was built without the `runtime-parse` feature (parser
    /// removed) and asked to run an app from source rather than from a
    /// precompiled artifact. Pass `--artifact <file>` (built via
    /// `lumenc build`) or rebuild with `--features runtime-parse`.
    #[error(
        "this runtime was built without the markup parser (runtime-parse); \
         run from a precompiled artifact (lumenc build) or rebuild with \
         --features runtime-parse"
    )]
    ParserDisabled,
}

/// Read `<dir>/main.lmn` + optional `<dir>/main.css`, build a default
/// `App`, spawn the parsed tree, and enter winit's event loop.
pub fn run_app(opts: RunOptions) -> Result<(), RunError> {
    let (app, winit_opts) = build_app(opts)?;
    run(app, winit_opts).map_err(|e| RunError::Window(e.to_string()))
}

/// Build the full app WITHOUT opening a window, then drive `ticks`
/// main-schedule ticks and return. Headless / CI entry point: same
/// plugin stack, scripts, and reactive bindings as [`run_app`], but no
/// windowing, input, or GPU rendering - just [`App::tick`] in a loop.
///
/// `ticks == 0` builds-and-drops (validates the app loads). The winit
/// options (title, size, text shaper) built alongside the app are
/// discarded; headless ticks run the main schedule + extract + an empty
/// render schedule (no GPU renderer plugin is installed off the winit
/// path), which is sufficient to exercise signal round-trips, script
/// execution, and `<for>` / `<if>` reconciliation.
pub fn run_app_headless(mut opts: RunOptions, ticks: u32) -> Result<(), RunError> {
    // Headless / FFI contract: no interactive session, so gate off the MCP
    // server + hot-reload watcher (see [`RunOptions::bounded`]).
    opts.bounded = true;
    let (mut app, _winit_opts) = build_headless_app(opts)?;
    for _ in 0..ticks {
        app.tick();
    }
    Ok(())
}

/// Shared headless plumbing: [`build_app`] plus the window-free half of
/// the winit backend. The windowed path installs `WinitPlugin` inside
/// `run()`; its `build` is window-free (backend messages,
/// `RedrawScheduler`, the `A11yPlugin` resources + `sync_a11y_tree`
/// system, and the XDG color-scheme command handler) - only the event
/// loop and GPU init in `run()` need a display. Installing it here gives
/// every headless schedule the same resource/system set so a11y-sync and
/// any system that reads a WinitPlugin-provided resource don't fail
/// validation. Used by [`run_app_headless`] (no renderer; FFI/test
/// contract), [`crate::run_headless::run_app_headless_rendered`] (full
/// offscreen-GPU mode), and the golden-image screenshot suite
/// (`lumenc/tests/golden.rs`), which installs an offscreen
/// `WgpuRendererPlugin` on top and reads the framebuffer back.
pub fn build_headless_app(opts: RunOptions) -> Result<(App, WinitOptions), RunError> {
    let (mut app, winit_opts) = build_app(opts)?;
    app.add_plugin(lumen_window_winit::WinitPlugin);
    Ok((app, winit_opts))
}

// -- Submodules (mechanical carve of the former monolithic run.rs) ----------
// Each submodule opens with `use super::*;`, inheriting this module's import
// block and (via the private glob re-exports below) every sibling's items, so
// intra-`run` references resolve unchanged.
mod app_build;
#[cfg(feature = "audio")]
mod audio;
mod caret_scroll;
mod check;
mod dom_commands;
mod hot_reload;
mod i18n;
mod loading;
mod restyle;
mod script_commands;
mod script_systems;
// `pub(crate)` so `crate::config`'s bundle capability inference can reuse the
// shared source scan + marker helpers (Part B tree-shaking).
pub(crate) mod subsystems;

// Private glob re-exports: make every submodule item visible inside `run`
// (and, transitively, to each submodule's `use super::*`). Behaviourally this
// reconstructs the flat namespace the single-file module had.
#[cfg(feature = "audio")]
use audio::*;
use caret_scroll::*;
use check::*;
use hot_reload::*;
use i18n::*;
use loading::*;
use restyle::*;
use script_commands::*;
use script_systems::*;
use subsystems::*;

// Public re-exports: preserve every `crate::run::<name>` path that external
// code (lib.rs re-exports, run_headless.rs, build_cli.rs, integration tests)
// depended on when these items lived directly in run.rs.
// `build_app` is public so the full-pipeline integration tests (which live in
// `lumenc`, where the injected parser is the same crate instance) can build an
// app window-free without the extra `WinitPlugin` that `build_headless_app`
// layers on.
pub use app_build::build_app;
pub use check::CheckReport;
#[cfg(feature = "runtime-parse")]
pub use check::{check_app, compile_app};
#[cfg(feature = "runtime-parse")]
pub(crate) use hot_reload::HotReloadDriver;
pub use restyle::{
    ColorSchemeIntent, ErrorBanner, ErrorBannerMarker, StyleVersion,
    dismiss_error_banner_on_escape, reconcile_error_banner,
};
