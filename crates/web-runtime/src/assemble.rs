//! Building the app a page runs.
//!
//! This is the browser's composition point, the counterpart of what
//! `lumen-runtime` does for a window. It installs the parts of Lumen that
//! have no platform in them (widget behaviour, focus, the reconcilers, the
//! two-way bindings) and leaves out the parts the browser already is: layout,
//! paint, accessibility, the font stack, and text editing. The last one is
//! easy to miss. A `<input>` in a page is edited by the browser, which owns
//! the caret, the selection and the IME; Lumen's rope-backed editor would be
//! a second one writing over the same text.
//!
//! The ordering edges below are the ones the desktop registers, and they are
//! not decoration: every dirty-gated binding reader has to run after the
//! pushes that mark a key dirty, or the key's one-tick window closes
//! unobserved and a bound label freezes at its spawn value.

use bevy_ecs::prelude::*;
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
use lumen_script::ScriptSet;

/// An app with everything installed that runs the same on every platform.
///
/// The scene is not in it yet: the script host goes in first so its
/// `on_start` has run before anything is spawned, which is the order the
/// desktop uses too.
pub fn portable_app() -> App {
    let mut app = App::new();
    // The page lays out and paints; nothing here extracts a scene.
    app.extract_fns.clear();
    app.world.init_resource::<PropertyStore>();
    app.world.init_resource::<ArraySignals>();

    app.add_plugin(lumen_input::InputPlugin);
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

/// Apply the state the page was rendered from, before the first tick.
///
/// Only signals nothing has written yet: the same rule the spawner applies
/// to a `signal_seed`, and for the same reason. A script that published a
/// signal with a value of its own, or a widget default already in the store,
/// is the state the page believes, and the seed says what the markup shows.
/// They should agree; where they do not, the live one wins and the mismatch
/// shows up as a repaint rather than as a lost write.
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
