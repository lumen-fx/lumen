//! # Lumen Rust SDK
//!
//! An ECS-first surface over the [Lumen](https://github.com/lumen-fx/lumen) UI
//! framework. Lumen runs on `bevy_ecs`; this crate exposes that power in
//! bevy's own shape - plugin groups, real systems, typed resources - instead of
//! hiding it behind builders and stringly-keyed callbacks.
//!
//! ## Quickstart
//!
//! ```no_run
//! use lumenui::prelude::*;
//!
//! fn main() -> lumenui::Result<()> {
//!     lumenui::App::new()
//!         .add_plugins(
//!             // Hot-reloads from disk in `cargo run`; embedded in
//!             // `cargo run --release`. Same line, both behaviours.
//!             LumenDefaultPlugins
//!                 .with_source(lumen_source!("examples/main.lmn", "examples/main.css")),
//!         )
//!         .add_systems(TickStage::Systems, (bump_counter, update_label).chain())
//!         .run()
//! }
//!
//! /// Count this tick's clicks into the `count` signal.
//! fn bump_counter(mut clicks: MessageReader<ClickEvent>, mut signals: Signals) {
//!     let n = clicks.read().count();
//!     if n > 0 {
//!         let total = signals.get_or::<i64>("count", 0) + n as i64;
//!         signals.set("count", total);
//!     }
//! }
//!
//! /// Push the count straight into the label's `TextContent` - a real ECS
//! /// query, no binding indirection.
//! fn update_label(signals: Signals, mut labels: Query<(&LumenId, &mut TextContent)>) {
//!     let count = signals.get_or::<i64>("count", 0);
//!     for (id, mut text) in &mut labels {
//!         if id.0 == "counter-label" {
//!             text.0 = format!("clicks: {count}");
//!         }
//!     }
//! }
//! ```
//!
//! Everything a user writes here is a *real* `bevy_ecs` system: it takes
//! [`Query`], [`Res`], [`ResMut`], [`MessageReader`], [`Commands`], or the typed
//! [`Signals`] param, and it is scheduled into the live [`TickStage`] schedule
//! beside the framework's own systems.
//!
//! ## The four pillars
//!
//! 1. **ECS-first assembly** - [`App::new`] -> [`add_plugins`](App::add_plugins) ->
//!    [`add_systems`](App::add_systems) -> [`run`](App::run). [`App`] wraps
//!    lumen-core's ECS app and the lumenc boot pipeline.
//! 2. **Decomposable plugin groups** - [`LumenDefaultPlugins`] is the full
//!    stack; trim it with `.build().disable::<ScriptPlugin>()` or compose your
//!    own with [`PluginGroup`] / [`PluginGroupBuilder`]. See [`plugins`].
//! 3. **Typed signals** - [`Signals`] gives `signals.get::<i64>("count")`; the
//!    [`signals!`] macro mints typed [`Property`] handle structs.
//! 4. **Event ergonomics** - two levels. For a full system, run conditions
//!    [`on_click`] / [`on_toggle`] / [`on_change`] gate it bevy-style:
//!    `bump.run_if(on_click("bump"))`. For the common "click an id -> write a
//!    signal" case, register a closure directly:
//!    [`App::on_click`](App::on_click) hands it an [`EventCtx`] with typed
//!    `get` / `set`, no `MessageReader` boilerplate. Derived signals come from
//!    [`App::add_computed`](App::add_computed).
//!
//! ## Dev hot reload vs release embed
//!
//! Point the UI at [`lumen_source!`] and the build profile decides how it
//! loads - you write one line:
//!
//! ```no_run
//! # use lumenui::prelude::*;
//! # fn demo() -> lumenui::Result<()> { lumenui::App::new().add_plugins(
//! LumenDefaultPlugins.with_source(lumen_source!("examples/main.lmn", "examples/main.css"))
//! # ).run() }
//! ```
//!
//! * `cargo run` (debug) - the markup / CSS stay on disk. The runtime reads
//!   them from the app directory at startup and installs its `notify`-based
//!   hot-reload watcher, so editing `main.lmn` / `main.css` updates the running
//!   window live. No restart, no rebuild.
//! * `cargo run --release` - the same files are `include_str!`-baked into the
//!   binary. Startup skips disk I/O and runs no watcher: the ship-time AOT
//!   optimisation.
//!
//! The macro selects the branch on `cfg(debug_assertions)`; the source line is
//! identical either way. The disk/hot-reload path follows the app-dir
//! convention - name the entry markup `main.lmn` and the stylesheet `main.css`
//! and keep them in one directory (`lumenc run <dir>` uses the same layout).
//!
//! Escape hatches remain. The explicit
//! [`with_markup`](plugins::LumenPluginsBuilder::with_markup) /
//! [`with_css`](plugins::LumenPluginsBuilder::with_css) pair always embeds (any
//! profile), and [`with_dir`](plugins::LumenPluginsBuilder::with_dir) always
//! disk-loads and watches. Frameworks split the same way - SwiftUI / Flutter
//! reload from the source tree in debug and freeze into the bundle for release;
//! Dioxus / Leptos watch the asset dir under a dev server and `include_str!` for
//! a shipped binary - the difference here is that the *runtime* already owns the
//! watcher, so the SDK only chooses disk vs embed.
//!
//! ## Sugar layer
//!
//! Prefer a terse builder for a small app? [`mod@simple`] keeps the v1
//! `App::builder().on_click(..)` surface, delegating to the same core.
//!
//! ## Custom widgets
//!
//! The prelude re-exports [`Widget`](widget::Widget) and its derive; install
//! widget or backend plugins with [`App::add_plugin`] /
//! [`App::add_plugins`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod conditions;
pub mod dom;
mod error;
mod events;
pub mod plugins;
pub mod signal;
mod simple_impl;

pub use app::App;
pub use conditions::{on_change, on_click, on_toggle};
pub use error::{Error, Result};
pub use events::{EventCtx, EventKind};
pub use plugins::{LumenDefaultPlugins, PluginGroup, PluginGroupBuilder, Source};
pub use signal::Signals;

/// The v1 builder surface - a terse facade over the ECS core for small apps.
///
/// ```no_run
/// use lumenui::simple::App;
/// use lumenui::prelude::*;
///
/// # fn demo() -> lumenui::Result<()> {
/// App::builder()
///     .markup("<root><label id=\"l\" bind-text=\"msg\" text=\"hi\"/></root>")
///     .property("msg", "hello")
///     .on_click("l", |ctx| ctx.set("msg", "clicked"))
///     .run()
/// # }
/// ```
pub mod simple {
    pub use crate::events::{EventCtx, EventKind, Handler};
    pub use crate::simple_impl::{App, AppBuilder};
}

// -- The crates behind the engine ---------------------------------------------

// Every Lumen crate this SDK is written against, taken from the engine rather
// than depended on again. An app gets one copy of each of these, shared with
// the runtime already inside the engine; naming them here is what keeps it
// that way. Submodules reach them as `crate::lumen_core`, `crate::bevy_ecs`,
// and so on.
//
// Which crate they are taken *through* decides how an app links the engine.
// `lumen_engine` is the shared library, so naming it puts that library in the
// app's link graph and an app built with `-C prefer-dynamic` leaves the
// runtime outside its own binary. Windows has no such library - its linker
// cannot produce one for a graph this size - so there the same items come
// straight from the engine crate and the runtime is linked in. Either way
// these are the same crates and the same types.
#[cfg(all(windows, feature = "host-rhai"))]
pub use lumen::sdk::rhai;
#[cfg(windows)]
pub use lumen::sdk::{
    bevy_ecs, lumen_core, lumen_runtime, lumen_script, lumen_widget, lumen_widget_macros, lumenc,
};
#[cfg(all(not(windows), feature = "host-rhai"))]
pub use lumen_engine::sdk::rhai;
#[cfg(not(windows))]
pub use lumen_engine::sdk::{
    bevy_ecs, lumen_core, lumen_runtime, lumen_script, lumen_widget, lumen_widget_macros, lumenc,
};

// -- Advanced re-exports ------------------------------------------------------

/// The lumen-core ECS app and [`Plugin`](lumen_core::app::Plugin) trait that
/// plugins build against.
pub use lumen_core::app as ecs_app;
pub use lumen_core::{command, components, input, property_store, signals as core_signals, tick};
pub use lumen_runtime as runtime;
pub use lumen_widget as widget;

/// The ECS crate (`bevy_ecs`) used across Lumen, re-exported so custom systems
/// build against the exact workspace version.
pub use bevy_ecs as ecs;

/// Typed reactive property store and typed [`Property`] handle.
pub use lumen_core::property_store::{Property, PropertyStore};

/// The host-neutral description of a native function exposed to script, and
/// the value type its arguments and result cross as. Build one with
/// [`ScriptFn::new`] for a typed signature, or with
/// [`simple::AppBuilder::native_fn`] for the untyped shape.
pub use lumen_script::{ScriptFn, ScriptFnCx, ScriptNs, ScriptSig, ScriptTy, ScriptValue};

/// One-stop import: `use lumenui::prelude::*;`.
///
/// Brings in [`App`], the plugin groups, typed [`Signals`], the run-condition
/// adapters, the common components, the typed signal surface, and the
/// `bevy_ecs` items systems need (`Query`, `Res`, `ResMut`, `Commands`,
/// `MessageReader`, `.chain()` / `.run_if()` via `IntoScheduleConfigs`).
pub mod prelude {
    use crate::{bevy_ecs, lumen_core, lumen_widget, lumen_widget_macros};

    pub use crate::{App, Error, Result};
    pub use crate::{EventCtx, EventKind};
    pub use crate::{on_change, on_click, on_toggle};

    // Plugin surface.
    pub use crate::lumen_source;
    pub use crate::plugins::{
        AppPlugins, AssetsPlugin, InputPlugin, LayoutPlugin, LumenDefaultPlugins, McpPlugin,
        PluginGroup, PluginGroupBuilder, PrimitivesPlugin, RenderPlugin, ScriptPlugin, Source,
        TextPlugin, WindowPlugin,
    };

    // Typed signals + the handle-struct macro.
    pub use crate::signal::Signals;
    pub use crate::signals;

    // Dynamic DOM: node handles, query results, events, and the
    // window / document / history namespaces.
    pub use crate::dom::{self, Event, Listener, Node, NodeQuery};

    // ECS app + schedule labels.
    pub use lumen_core::app::{App as EcsApp, Plugin};
    pub use lumen_core::tick::TickStage;

    // Components: visuals, text, layout, structure, markers.
    pub use lumen_core::components::{
        Color, Disabled, DropHovered, DropTarget, Edges, Fill, FlexAlign, FlexDirection,
        FlexJustify, Length, LumenClasses, LumenId, LumenTag, Opacity, Position, Selected,
        ShadowSpec, SliderValue, Style, TextAlign, TextContent, TextInput, TextStyle, TextWrap,
        Toggleable, Visible, Visuals,
    };

    // Input messages.
    pub use lumen_core::input::{
        ClickEvent, DoubleClickEvent, DragEndEvent, DragMoveEvent, DragStartEvent, Focused,
        Hovered, KeyPressed, KeyReleased, LongPressEvent, Pressed,
    };

    // Typed signal surface.
    pub use lumen_core::property_store::{Property, PropertyKey, PropertyStore, PropertyValue};

    // Custom-widget authoring.
    pub use lumen_widget::{Attributes, Widget};
    pub use lumen_widget_macros::Widget;

    // The bevy_ecs vocabulary systems are written in. `IntoScheduleConfigs`
    // brings `.chain()` / `.run_if()` / `.after()` into scope.
    pub use bevy_ecs::message::{MessageReader, MessageWriter};
    pub use bevy_ecs::prelude::{
        Commands, Entity, IntoScheduleConfigs, Local, Query, Res, ResMut, Resource, With, Without,
    };
    pub use bevy_ecs::query::Changed;
}
