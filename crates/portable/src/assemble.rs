//! Building the app, without the platform it runs on.
//!
//! This is the composition point for everything that is not a window, the
//! counterpart of what `lumen-runtime` does for one. It installs the parts of
//! Lumen that have no platform in them (widget behaviour, focus, the
//! reconcilers, the two-way bindings) and leaves out layout, paint,
//! accessibility, the font stack, and text editing.
//!
//! Text editing is the one that is easy to miss. In a browser, an `<input>`
//! is edited by the browser, which owns the caret, the selection and the IME;
//! Lumen's rope-backed editor would be a second one writing over the same
//! text.
//!
//! The ordering edges below are the ones the desktop registers, and they are
//! not decoration: every dirty-gated binding reader has to run after the
//! pushes that mark a key dirty, or the key's one-tick window closes
//! unobserved and a bound label freezes at its spawn value.

#[cfg(target_arch = "wasm32")]
use std::sync::Arc;

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::{Schedules, SingleThreadedExecutor};
use lumen_core::app::Tick;
use lumen_core::prelude::{App, TickStage};
use lumen_core::property_store::{PropertyKey, PropertyStore, commit_external_properties};
use lumen_core::signals::{
    ArraySignals, apply_checked_bindings, apply_disabled_bindings, apply_scroll_bindings,
    apply_text_bindings, apply_value_bindings, push_scroll_to_signal, push_slider_to_signal,
    push_textinput_to_signal, push_toggle_to_signal,
};
use lumen_html::contract::Seed;
use lumen_primitives::{
    CheckboxPlugin, ControlsPlugin, PressPlugin, ProgressPlugin, RadioPlugin, TabsPlugin,
    ValidationPlugin,
};
use lumen_scene::spawn;
#[cfg(target_arch = "wasm32")]
use lumen_script::FetchRegistry;
use lumen_script::ScriptSet;
use lumen_script::runtime::register_script_commands;
#[cfg(target_arch = "wasm32")]
use lumen_web_http::WebFetchDispatch;

/// An app with everything installed that runs the same on every platform.
///
/// The scene is not in it yet: the script host goes in first so its
/// `on_start` has run before anything is spawned, which is the order the
/// desktop uses too.
pub fn portable_app() -> App {
    let mut app = App::new();
    // Keep the `Tick` schedule on bevy_ecs's single-threaded executor
    // rather than the platform default. Two reasons, both specific to this
    // assembly. First, it excludes layout, paint, and the font stack, the
    // systems the multi-threaded executor is meant to fan out, so there is
    // nothing here for it to parallelize; app logic in this assembly stays
    // serial within a tick, and SSR's parallelism comes from running many
    // requests at once, not from splitting one request's tick across
    // threads. Second, a per-request renderer builds this app on its own
    // worker thread and depends on every system in the tick running there
    // too. A dispatcher that calls out to `HttpDispatch` from a
    // `ComputeTaskPool` worker instead is a value planted and later
    // dropped on the wrong thread. `NonSendMut` cannot express "pin the
    // whole schedule", only individual systems, so the executor is pinned
    // instead.
    if let Some(mut schedules) = app.world.get_resource_mut::<Schedules>() {
        if let Some(schedule) = schedules.get_mut(Tick) {
            schedule.set_executor(SingleThreadedExecutor::new());
        }
    }
    // Whatever this app is put into lays out and paints it; no extract here.
    app.extract_fns.clear();
    app.world.init_resource::<PropertyStore>();
    app.world.init_resource::<ArraySignals>();
    // The scene applier below reads the script command stream whether or not
    // a host is installed. An app written in a language no host in this build
    // answers for still ticks; it just has nothing writing to the stream. A
    // host installed later finds this already registered.
    register_script_commands(&mut app.world);

    install_http(&mut app);

    // No clipboard: it is the one non-send resource the input layer installs,
    // and this app has to run wherever it is put.
    app.add_plugin(lumen_input::InputPlugin { clipboard: false });
    // The tree a script reads and the mutations it issues, which is how a
    // fragment reaches the world: `mount()` inserts a node the DOM applier
    // built, and the applier is where a fragment key becomes a subtree. The
    // desktop installs the same three systems.
    lumen_scene::dom::install_dom(&mut app);
    app.add_plugin(PressPlugin::default());
    app.add_plugin(ControlsPlugin);
    app.add_plugin(CheckboxPlugin);
    app.add_plugin(RadioPlugin);
    app.add_plugin(TabsPlugin);
    app.add_plugin(ProgressPlugin);
    app.add_plugin(ValidationPlugin);

    install_reconcilers(&mut app);
    install_bindings(&mut app);
    app
}

/// Put the transport the scripts' `fetch()` and `http()` builtins run on into
/// the app, where the platform has one this assembly can name.
///
/// The registry goes in before any host does: the script plugin installs the
/// disabled default only when it finds none, so an install after it is an
/// install that does nothing.
///
/// In a browser the transport is the page's own `fetch`, which is the one
/// platform whose answer this assembly knows without being told: a page has no
/// thread to run a request on, so a build with no dispatcher installed has no
/// working `fetch()` at all. Everywhere else the transport is the embedder's
/// choice, made where the app is composed - the desktop runtime installs
/// `lumen-http-ureq` - so nothing is assumed here.
fn install_http(app: &mut App) {
    #[cfg(target_arch = "wasm32")]
    app.world
        .insert_resource(FetchRegistry::with_dispatch(Arc::new(WebFetchDispatch)));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = app;
}

/// The systems that keep the spawned tree in step with the app's state.
fn install_reconcilers(app: &mut App) {
    app.world.init_resource::<spawn::ScenePolicy>();
    app.add_systems(
        TickStage::Systems,
        (spawn::reconcile_for_blocks, spawn::reconcile_if_blocks),
    );
    app.add_systems(
        TickStage::Input,
        spawn::close_dialogs_on_escape.after(lumen_input::cancel_press_on_escape),
    );
    app.add_systems(
        TickStage::Systems,
        spawn::mark_dialog_accept_on_default_click
            .after(lumen_input::dispatch_clicks)
            .after(spawn::activate_dialog_default_on_enter),
    );
    app.add_systems(
        TickStage::Systems,
        spawn::manage_dialog_lifecycle
            .after(spawn::reconcile_if_blocks)
            .after(spawn::mark_dialog_accept_on_default_click),
    );
    app.add_systems(
        TickStage::Systems,
        spawn::activate_dialog_default_on_enter
            .after(lumen_input::dispatch_focused_keys)
            .before(ScriptSet::Dispatch),
    );
}

/// The two-way `bind-*` systems, with the edges that keep the one-tick dirty
/// window observable.
fn install_bindings(app: &mut App) {
    app.add_systems(
        TickStage::Systems,
        commit_external_properties
            .after(ScriptSet::Dispatch)
            .before(ScriptSet::Derivations),
    );
    // What a script writes reaches the world here. The edges are the desktop's:
    // a derivation and a binding reader are both gated on the one tick a write
    // is dirty for, so applying after either is applying where neither looks.
    app.add_systems(
        TickStage::Systems,
        lumen_scene::script_commands::apply_scene_script_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .before(ScriptSet::Derivations)
            .before(apply_text_bindings)
            .before(apply_checked_bindings)
            .before(apply_value_bindings)
            .before(spawn::reconcile_for_blocks),
    );
    app.add_systems(
        TickStage::Systems,
        (
            push_textinput_to_signal,
            push_toggle_to_signal,
            push_slider_to_signal,
            push_scroll_to_signal,
        )
            .before(ScriptSet::Derivations),
    );
    app.add_systems(
        TickStage::Systems,
        (
            apply_checked_bindings,
            apply_value_bindings,
            apply_disabled_bindings,
            apply_scroll_bindings,
        )
            .after(commit_external_properties)
            .after(ScriptSet::Derivations)
            .after(push_toggle_to_signal)
            .after(push_slider_to_signal)
            .after(push_textinput_to_signal)
            .after(push_scroll_to_signal),
    );
    // Signal to text lands before the keystroke path so a mid-tick keystroke
    // always wins over a signal write.
    app.add_systems(
        TickStage::Systems,
        apply_text_bindings
            .after(commit_external_properties)
            .after(ScriptSet::Derivations)
            .after(push_toggle_to_signal)
            .after(push_slider_to_signal)
            .after(push_textinput_to_signal)
            .before(lumen_input::type_into_focused),
    );
}

/// Apply the state the page was rendered from, before the scene is spawned.
///
/// Only signals nothing has written yet, so a script that published a signal
/// of its own keeps it: that write is the live state, and the seed only says
/// where the page starts.
///
/// Before the scene, because the spawner seeds a signal from the markup
/// beside it: a `bind-text` label's own `text=` is what the app shows until
/// the signal has a value. The page was rendered with the signal's value, so
/// running the spawner first would let the fallback win and leave the
/// document saying one thing and the app another.
pub fn apply_seed(world: &mut World, seed: &Seed) {
    let mut store = world.resource_mut::<PropertyStore>();
    for (name, value) in &seed.globals {
        let key = PropertyKey::global(name.as_str());
        if store.get(&key).is_none() {
            store.set(key, value.into());
        }
    }
    if seed.arrays.is_empty() {
        return;
    }
    let mut arrays = world.resource_mut::<ArraySignals>();
    for (name, rows) in &seed.arrays {
        if arrays.get(name).is_none() {
            arrays.set(
                name,
                rows.iter()
                    .map(|row| row.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .collect(),
            );
        }
    }
}
