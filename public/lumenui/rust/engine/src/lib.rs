//! # Lumen Rust SDK
//!
//! Write a [Lumen](https://github.com/lumen-fx/lumen) app's code in Rust. The
//! app is the same as any Lumen app: `lumen.toml` describes it, its markup and
//! stylesheets live in `src/`, and its assets sit beside it. The build
//! compiles the UI, and the Rust code drives it the way a script does:
//! handlers on element ids, signals by name, and the element tree.
//!
//! ## An app
//!
//! ```text
//! counter/
//!   Cargo.toml        lumenui in [dependencies], lumenui-build in [build-dependencies]
//!   build.rs          fn main() { lumenui_build::compile(); }
//!   lumen.toml        [app] entry = "main.lmn"
//!   src/main.lmn      <button id="bump" text="+1"/> <label bind-text="clicks"/>
//!   src/main.rs
//! ```
//!
//! ```ignore
//! use lumenui::prelude::*;
//!
//! fn main() -> lumenui::Result<()> {
//!     lumenui::app!()
//!         .signal("clicks", 0i64)
//!         .on_click("bump", |ui, _| ui.update("clicks", |n: i64| n + 1))
//!         .on_click("reset", |ui, _| ui.set("clicks", 0i64))
//!         .run()
//! }
//! ```
//!
//! [`app!()`](app) starts an [`App`] from the UI the build script compiled.
//! A handler gets a [`Ui`] (signals, `<for>` lists, the element tree, menus,
//! the app's own resources) and the [`Event`]. An event goes to the nearest
//! element at or above its target that has a handler, so a click on a
//! button's icon runs the button's handler; [`Event::row`] says which `<for>`
//! row it was in.
//!
//! ## How the UI builds
//!
//! The build script compiles everything `lumen.toml` names (the entry, its
//! includes and imports, templates, pages, scripts, translations) into one
//! artifact, and the executable carries it. A markup or style error fails
//! `cargo build`. Debug and release builds start from that artifact alike; a
//! debug build also watches `src/` and reloads the UI on an edit while the
//! window stays open. Assets (images, fonts) and `lumen.toml` stay beside the
//! executable.
//!
//! ## Systems where they fit
//!
//! The same [`App`] takes `bevy_ecs` systems for the work that suits them,
//! such as a paint loop over every pointer move.
//! [`App::add_systems`] puts a system in [`TickStage::Systems`] after the
//! tick's input and before the screen is updated, so it needs no ordering
//! against the engine:
//!
//! ```no_run
//! # extern crate lumenui_engine as lumenui;
//! use lumenui::prelude::*;
//!
//! #[derive(Resource, Default)]
//! struct Stroke(Vec<Vec2>);
//!
//! fn paint(mut moves: MessageReader<PointerMoved>, mut stroke: ResMut<Stroke>) {
//!     stroke.0.extend(moves.read().map(|m| m.position));
//! }
//!
//! # fn demo() -> lumenui::Result<()> {
//! lumenui::App::from_markup("<root/>")
//!     .insert_resource(Stroke::default())
//!     .add_systems(TickStage::Systems, paint)
//!     .run()
//! # }
//! ```
//!
//! [`Signals`] is the system parameter for signals, and the [`dom`] handles
//! work from a system as from a handler.
//!
//! ## Custom widgets
//!
//! The prelude re-exports [`Widget`](lumen_widget::Widget) and its derive;
//! install the plugin the derive generates with [`App::add_plugin`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// The SDK under the name `#[derive(Widget)]` reaches it by from inside this
// package (its examples and tests name it so already).
extern crate self as lumenui_engine;

pub mod app;
pub mod clipboard;
pub mod conditions;
pub mod dialog;
pub mod dom;
mod error;
mod events;
mod frame;
pub mod plugins;
pub mod signal;
mod ui;

pub use app::App;
pub use conditions::{on_change, on_click, on_toggle};
pub use error::{Error, Result};
pub use events::{Event, EventKind, HandlerSet, Row};
pub use frame::Frame;
/// How [`App::run_headless_rendered`] and [`App::screenshot`] run: the
/// offscreen target's device pixel ratio and an optional tick bound.
pub use lumen_runtime::HeadlessOptions;
pub use signal::Signals;
pub use ui::{CompiledUi, Record, Ui};

/// The app's compiled UI, from what its build script wrote.
///
/// Expands to a [`CompiledUi`] over the artifact `lumenui_build::compile()`
/// put in the crate's `OUT_DIR`. In a debug build it also carries the crate
/// directory, which the app watches to reload the UI on an edit.
///
/// The build script is what makes the artifact; without one the macro fails
/// with a message saying to add it.
#[macro_export]
macro_rules! ui {
    () => {
        $crate::CompiledUi::from_build(
            ::core::include_bytes!(::core::env!(
                "LUMENUI_UI",
                "lumenui::ui!() needs the UI the app's build script compiles: add a build.rs \
                 beside Cargo.toml with `fn main() { lumenui_build::compile(); }` and \
                 `lumenui-build` under [build-dependencies]"
            )),
            ::core::env!("CARGO_MANIFEST_DIR"),
            ::core::cfg!(debug_assertions),
        )
    };
}

/// An [`App`] over the app's compiled UI: `App::new(lumenui::ui!())`.
#[macro_export]
macro_rules! app {
    () => {
        $crate::App::new($crate::ui!())
    };
}

// -- The crates behind the engine ---------------------------------------------

// Every Lumen crate this SDK is written against, taken from the engine rather
// than depended on again. An app gets one copy of each of these, shared with
// the runtime already inside the engine; naming them here is what keeps it
// that way. Submodules reach them as `crate::lumen_core`, `crate::bevy_ecs`,
// and so on.
//
// They are taken through `lumen_engine` except on Windows, where that crate
// does not exist (its linker cannot produce the shared library it also
// builds) and the same items come straight from the engine crate. Either way
// these are the same crates and the same types, and an app links them in.
#[cfg(windows)]
pub use lumen::sdk::{
    bevy_ecs, lumen_core, lumen_os_clipboard, lumen_os_filedialog, lumen_primitives, lumen_runtime,
    lumen_scene, lumen_script, lumen_widget, lumen_widget_macros, lumenc,
};
#[cfg(not(windows))]
pub use lumen_engine::sdk::{
    bevy_ecs, lumen_core, lumen_os_clipboard, lumen_os_filedialog, lumen_primitives, lumen_runtime,
    lumen_scene, lumen_script, lumen_widget, lumen_widget_macros,
};
// The parser/compiler front-end sits outside the shared engine (the `lumenc`
// binary links that library itself); its heavy dependencies still resolve
// into the engine, so this adds the front-end alone.
#[cfg(not(windows))]
pub use lumenc;

/// The math crate (`glam`) the engine's components and messages carry, such as
/// the `Vec2` of a click's position. Name it through here: a dependency of an
/// app's own would be a second copy whose types are not these.
pub use glam;

/// The host-neutral description of a native function exposed to script, and
/// the value type its arguments and result cross as. Build one with
/// [`ScriptFn::from_fn`] from a plain Rust closure, with [`ScriptFn::new`] when
/// it also wants a doc line or a namespace, or with [`App::native_fn`] for the
/// untyped shape.
pub use lumen_script::{
    ScriptField, ScriptFn, ScriptFnCx, ScriptNs, ScriptSig, ScriptStruct, ScriptTy, ScriptType,
    ScriptValue,
};

/// One-stop import: `use lumenui::prelude::*;`.
///
/// Everything an app names in ordinary code: [`App`] and the handler
/// surface ([`Ui`], [`Event`], [`Record`]), the DOM handles, signals, dialogs
/// and the clipboard, the input types events carry, the common components,
/// and the `bevy_ecs` items systems are written with, including the derives
/// an app defines its own data with (`Component`, `Resource`, `Message`,
/// `Bundle`, `Widget`). Import it with a glob: the `bevy_ecs` derives find the
/// ECS crate through the `bevy_ecs` name this module also brings in, so an app
/// needs no dependency on that crate.
pub mod prelude {
    use crate::{lumen_core, lumen_widget, lumen_widget_macros};

    // The ECS crate itself, under its own name. The `bevy_ecs` derives an app
    // writes (`Component`, `Resource`, `Message`, ...) expand to paths that
    // start with `bevy_ecs::`, and an app must not depend on that crate itself:
    // a second copy is a second set of types. Glob-importing the prelude puts
    // the engine's copy under that name, which is what the expansion finds.
    pub use crate::bevy_ecs;

    pub use crate::{App, CompiledUi, Error, Frame, HeadlessOptions, Result};
    pub use crate::{Event, EventKind, Record, Row, Ui};
    pub use crate::{on_change, on_click, on_toggle};

    // An app's own plugin bundles.
    pub use crate::plugins::{PluginGroup, PluginGroupBuilder};

    // Typed signals + the handle-struct macro.
    pub use crate::signal::Signals;
    pub use crate::signals;

    // The element tree: node handles and query results.
    pub use crate::dom::{self, Listener, Node, NodeQuery};

    // OS: native file dialogs and the clipboard.
    pub use crate::clipboard::{Clipboard, RgbaImage};
    pub use crate::dialog::FileDialog;

    // ECS app, its plugin trait, and the tick.
    pub use lumen_core::app::{App as EcsApp, Plugin};
    pub use lumen_core::tick::{AppSet, FrameTiming, TickStage, WakeDeadline};

    // Which renderer draws the app, and on what.
    pub use lumen_core::render_backend::RendererInfo;

    // Components: visuals, text, layout, structure, markers.
    pub use lumen_core::components::{
        Color, Disabled, DropHovered, DropTarget, Edges, Fill, FlexAlign, FlexDirection,
        FlexJustify, Length, LumenClasses, LumenId, LumenTag, Opacity, Position, Selected,
        ShadowSpec, SliderValue, Style, TextAlign, TextContent, TextInput, TextStyle, TextWrap,
        Toggleable, Transform, Visible, Visuals,
    };

    // Input: the messages a system reads and the types events carry.
    pub use lumen_core::input::{
        ClickEvent, ContextMenuEvent, DoubleClickEvent, DragEndEvent, DragMoveEvent,
        DragStartEvent, EditCommitted, Focused, HoverWithin, Hovered, Key, KeyPressed, KeyReleased,
        LongPressEvent, MenuClicked, Modifiers, ModifiersState, MouseWheel, NamedKey,
        PointerButton, PointerData, PointerKind, PointerMoved, PointerPressed, PointerReleased,
        PressWithin, Pressed, WindowFocused,
    };

    // Typed signal surface.
    pub use lumen_core::property_store::{Property, PropertyKey, PropertyStore, PropertyValue};

    // Custom-widget authoring.
    pub use lumen_widget::{Attributes, Widget};
    pub use lumen_widget_macros::Widget;

    // The math type positions and sizes are in.
    pub use crate::glam::Vec2;

    // The bevy_ecs vocabulary systems are written in. `IntoScheduleConfigs`
    // brings `.chain()` / `.run_if()` / `.after()` into scope.
    pub use bevy_ecs::hierarchy::{ChildOf, Children};
    pub use bevy_ecs::message::{MessageReader, MessageWriter};
    pub use bevy_ecs::prelude::{
        Bundle, Commands, Component, Entity, IntoScheduleConfigs, Local, Message, Query, Res,
        ResMut, Resource, With, Without,
    };
    pub use bevy_ecs::query::{Added, Changed};
    pub use bevy_ecs::system::{ParamSet, SystemParam};
    // What `Ui::resource_mut` and `Ui::resource_scope` hand back.
    pub use bevy_ecs::world::Mut;
}
