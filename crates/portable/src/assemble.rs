//! Building the app, without the platform it runs on.
//!
//! This is the composition point for everything that is not a window, the
//! counterpart of what `lumen-runtime` does for one. It installs what the
//! platform this app lands on does not already do (the reconcilers, the
//! two-way bindings, key routing, and the widget behaviour a browser has no
//! native control for) and leaves out layout, paint, accessibility, the font
//! stack, and everything a browser drives itself.
//!
//! That last part is most of the desktop's input layer. A browser hit-tests,
//! moves focus, edits fields, owns the caret, the selection and the IME, and
//! toggles, steps and drags its own form controls; Lumen's versions of those
//! would be a second hand on the same control, and the DOM backend already
//! writes what the browser did straight into the world. What stays is the
//! routing a forwarded key needs, radio groups (the browser unchecks the
//! sibling without saying so), tab strips and their panels, progress
//! bindings and validation, none of which a page gets for free.
//!
//! The ordering edges below are the ones the desktop registers, and they are
//! not decoration: every dirty-gated binding reader has to run after the
//! pushes that mark a key dirty, or the key's one-tick window closes
//! unobserved and a bound label freezes at its spawn value.

use std::sync::Arc;

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::{Schedules, SingleThreadedExecutor};
use lumen_core::app::Tick;
use lumen_core::components::{InlineStyle, LumenAttributes, LumenClasses, LumenTag, TextContent};
use lumen_core::prelude::Children;
use lumen_core::prelude::{App, TickStage};
use lumen_core::property_store::{PropertyKey, PropertyStore, commit_external_properties};
use lumen_core::signals::{
    ArraySignals, apply_checked_bindings, apply_disabled_bindings, apply_scroll_bindings,
    apply_text_bindings, apply_value_bindings, push_scroll_to_signal, push_slider_to_signal,
    push_textinput_to_signal, push_toggle_to_signal,
};
use lumen_html::contract::{NodePath, NodeSeed, Seed};
use lumen_html::paths::walk_nodes;
use lumen_i18n::{I18n, I18nError, Lang, LanguageIdentifier, SharedI18n};
use lumen_primitives::{ProgressPlugin, RadioPlugin, TabsPlugin, ValidationPlugin};
use lumen_scene::spawn;
use lumen_scene::spawn::ForMarker;
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
    app.world.init_resource::<PropertyStore>();
    app.world.init_resource::<ArraySignals>();
    // The scene applier below reads the script command stream whether or not
    // a host is installed. An app written in a language no host in this build
    // answers for still ticks; it just has nothing writing to the stream. A
    // host installed later finds this already registered.
    register_script_commands(&mut app.world);

    install_http(&mut app);

    // Key routing only. The pointer, text-editing, IME and file-drop half of
    // the input layer is a native window's, and the clipboard it would
    // install is the one non-send resource this app cannot carry.
    app.add_plugin(lumen_input::KeyDispatchPlugin);
    // The tree a script reads and the mutations it issues, which is how a
    // fragment reaches the world: `mount()` inserts a node the DOM applier
    // built, and the applier is where a fragment key becomes a subtree. The
    // desktop installs the same three systems.
    lumen_scene::dom::install_dom(&mut app);
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

/// The locale every other one falls back to. The desktop's fallback chain
/// ends here too, and it is the locale an app's source strings are in.
const FALLBACK_LOCALE: &str = "en-US";

/// Install the app's translations for `locale`, from catalogues that were
/// fetched rather than read off a disk.
///
/// `catalogues` pairs a BCP-47 tag with that locale's Fluent source. It is
/// the same registry the desktop builds, reaching the same two readers: the
/// world resource markup resolves a `translatable` key through as it spawns,
/// and the process-wide hook every script host's `t()` calls. A page that
/// arrived already translated still needs both, because a row the app builds
/// after the page opens was never written into the document.
///
/// No formatter is installed: nothing in this assembly links one, so a
/// `format` spec leaves its text as it stands.
///
/// # Errors
///
/// A tag is not BCP-47, or a catalogue is not Fluent. Nothing is installed
/// then, and the app reads in the language its source strings are written in.
pub fn install_i18n(
    world: &mut World,
    locale: &str,
    catalogues: &[(String, String)],
) -> Result<(), I18nError> {
    let current: LanguageIdentifier = Lang::try_from(locale)?.into();
    let fallback: LanguageIdentifier = Lang::try_from(FALLBACK_LOCALE)?.into();
    let mut i18n = I18n::new(current, vec![fallback]);
    for (tag, source) in catalogues {
        let tag: LanguageIdentifier = Lang::try_from(tag.as_str())?.into();
        i18n.load_ftl(tag, source)?;
    }
    let shared = SharedI18n::new(i18n);
    let for_markup = shared.clone();
    world.insert_resource(lumen_core::i18n::AppI18n::new(
        Arc::new(move |key| for_markup.try_t(key)),
        Arc::new(|_, _| None),
    ));
    let for_scripts = shared.clone();
    lumen_core::i18n::set_translator(move |key| for_scripts.try_t(key));
    world.insert_resource(shared);
    Ok(())
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

/// Apply what the page says the app wrote onto single nodes, after the scene
/// is spawned.
///
/// Not part of [`apply_seed`], and it cannot be: a node seed names nodes, and
/// there are none until the tree is there. It is applied for the same reason
/// the signal seed is, one step later. Without it the browser spawns each
/// entity with the class list the markup declares, the projection sees that
/// as a change on the first tick, and the page loses the styling it was
/// rendered with until the app happens to set it again.
///
/// The walk is the one the document names nodes by, so a path here reaches
/// the entity the emitter wrote that path onto.
pub fn apply_node_seed(world: &mut World, root: Entity, seed: &Seed) {
    if seed.nodes.is_empty() {
        return;
    }
    let mut writes: Vec<(Entity, &NodeSeed)> = Vec::new();
    let scene: &World = world;
    if let Some(node) = seed.nodes.get(&NodePath::root().to_string()) {
        writes.push((root, node));
    }
    walk_nodes(
        root,
        (),
        |entity| {
            scene
                .get::<Children>(entity)
                .map(|kids| &**kids)
                .unwrap_or(&[])
        },
        |entity| scene.get::<ForMarker>(entity).is_some(),
        |entity| scene.get::<LumenTag>(entity).is_some(),
        |visit| {
            if let Some(node) = seed.nodes.get(&visit.path.to_string()) {
                writes.push((visit.entity, node));
            }
            Some(())
        },
    );
    for (entity, node) in writes {
        let mut entity = world.entity_mut(entity);
        if let Some(classes) = &node.classes {
            entity.insert(LumenClasses::from(classes.clone()));
        }
        if !node.attrs.is_empty() {
            let mut attrs = entity.take::<LumenAttributes>().unwrap_or_default();
            for (name, value) in &node.attrs {
                attrs.set(name, value.clone());
            }
            entity.insert(attrs);
        }
        if !node.style.is_empty() {
            let mut style = entity.take::<InlineStyle>().unwrap_or_default();
            for (property, value) in &node.style {
                style.set(property, value.clone());
            }
            entity.insert(style);
        }
        if let Some(text) = &node.text {
            entity.insert(TextContent(text.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The translator slot is process-global, so the cases that install one
    /// run one at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const GERMAN: &str = "greeting = Hallo\n";

    #[test]
    fn an_installed_catalogue_answers_both_readers() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut world = World::new();
        install_i18n(
            &mut world,
            "de-DE",
            &[("de-DE".to_string(), GERMAN.to_string())],
        )
        .expect("a valid tag and a valid catalogue");

        // Scripts, through the process-wide hook.
        assert_eq!(lumen_core::i18n::translate("greeting"), "Hallo");
        assert_eq!(lumen_core::i18n::translate("nothing-here"), "nothing-here");
        // Markup, through the app's own resource.
        let app = world
            .get_resource::<lumen_core::i18n::AppI18n>()
            .expect("the app's half of the seam");
        assert_eq!(app.try_translate("greeting").as_deref(), Some("Hallo"));
        assert_eq!(app.try_translate("nothing-here"), None);
        // Nothing formats: no formatter is linked into this assembly.
        assert_eq!(app.format("number", "1234.5"), None);
        lumen_core::i18n::clear_translator();
    }

    #[test]
    fn a_catalogue_that_will_not_parse_is_reported() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut world = World::new();
        assert!(install_i18n(&mut world, "not a tag", &[]).is_err());
        assert!(
            install_i18n(
                &mut world,
                "de-DE",
                &[("de-DE".to_string(), "= no key\n".to_string())],
            )
            .is_err()
        );
    }
}
