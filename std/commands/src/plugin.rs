//! The plugin: its configuration, the elements it adopts, and the one system
//! that hears keys and menu clicks, runs commands, and keeps menus, the
//! reserved chords and the palette in step with the registry.

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::{Message, MessageReader};
use bevy_ecs::prelude::*;
use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::chord::{Chord, ReservedChords};
use lumen_module::lumen_core::components::{Disabled, LumenAttributes, LumenId, LumenTag};
use lumen_module::lumen_core::input::{
    ClickEvent, Key, KeyCode, KeyPressed, MenuClicked, Modifiers, NamedKey,
};
use lumen_module::lumen_core::prelude::TickStage;
use lumen_module::lumen_input::FocusKeeps;
use lumen_module::lumen_primitives::MenuItem;
use lumen_module::lumen_script::{CustomEvent, ScriptFnAppExt, ScriptSet};

use crate::model::{Command, PALETTE, PaletteAsk, Shared, chords};
use crate::palette::{self, Palette, PaletteInput};
use crate::persist::{self, Persist};

/// The module's name, as an app declares it.
pub const NAME: &str = "lumen-commands";

/// The web half's descriptor, which declares this module's functions.
pub const DESCRIPTOR: &str = include_str!("../web/lumen-addon.toml");

/// The default look, at user-agent origin.
const UA_CSS: &str = include_str!("../commands.css");

/// The default strings, under the app's own catalogue.
const MESSAGES_EN: &str = include_str!("../locale/en-US.ftl");

/// The tag a command is declared with.
const COMMAND: &str = "command";

/// The palette's tag.
const PALETTE_TAG: &str = "command-palette";

/// The default chord of the palette's command.
const PALETTE_KEYS: &str = "CommandOrControl+Shift+P";

/// The module's message `key` in the app's language, else `fallback`.
pub(crate) fn text(key: &str, fallback: &str) -> String {
    lumen_module::lumen_core::i18n::try_translate(key).unwrap_or_else(|| fallback.to_string())
}

/// Commands: `<command>` and `<command-palette>` elements, the `commands`
/// script namespace, and the user's key bindings kept between runs.
///
/// Ships as the bundled `lumen-commands` runtime module (an app declares
/// `lumen-commands = { bundled = true }` under `[dependencies]`), and works
/// the same added as an ordinary plugin.
#[derive(Clone, Debug, Default)]
pub struct CommandsPlugin {
    /// Whether the user's bindings are kept between runs.
    pub persist: Persist,
}

impl CommandsPlugin {
    /// Build from the module's `config` table: `persist` (`"auto"` or
    /// `"none"`).
    #[must_use]
    pub fn new(config: ModuleConfig) -> Self {
        let mut plugin = Self::default();
        if config.str("persist") == Some("none") {
            plugin.persist = Persist::None;
        }
        plugin
    }
}

/// A command ran: from its chord, a menu item, the palette or a call. Written
/// beside the `command` event, for a Rust system that prefers messages.
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub struct CommandRun {
    /// The command's id.
    pub id: String,
}

/// A `<command>` element, and the id it declares.
#[derive(Component, Clone, Debug)]
pub struct CommandElement {
    /// The command's id.
    pub id: String,
}

/// Elements of the module's that appeared or went since the last tick.
#[derive(Resource, Default)]
pub(crate) struct Arrivals {
    new: Vec<Entity>,
    gone: Vec<String>,
}

/// Whether the kept bindings were read.
#[derive(Resource, Default)]
pub(crate) struct Loaded(bool);

/// Register message `T` unless something already did.
fn message<T: Message>(world: &mut World) {
    if !world.contains_resource::<bevy_ecs::message::Messages<T>>() {
        bevy_ecs::message::MessageRegistry::register_message::<T>(world);
    }
}

impl Plugin for CommandsPlugin {
    fn build(self, app: &mut App) {
        for tag in crate::TAGS {
            lumen_module::lumen_widget::register_widget_tag_owned(tag);
        }
        let shared = Shared::default();
        let _ = shared.lock().register(Command {
            id: PALETTE.to_string(),
            label: String::new(),
            label_key: Some("lumen-commands-palette".to_string()),
            defaults: chords(PALETTE_KEYS).unwrap_or_default(),
            enabled: true,
            from_markup: false,
        });
        app.world.insert_resource(shared.clone());
        app.world.insert_resource(self.persist);
        app.world.init_resource::<Arrivals>();
        app.world.init_resource::<Loaded>();
        app.world.init_resource::<Palette>();
        app.world.init_resource::<ReservedChords>();
        app.add_ua_stylesheet(NAME, UA_CSS);
        app.add_default_messages("en-US", MESSAGES_EN);
        message::<CommandRun>(&mut app.world);
        message::<CustomEvent>(&mut app.world);
        message::<KeyPressed>(&mut app.world);
        message::<MenuClicked>(&mut app.world);
        message::<ClickEvent>(&mut app.world);
        install_observers(&mut app.world);
        match crate::api::script_fns(&shared) {
            Ok(fns) => {
                app.add_script_fns(fns);
            }
            Err(e) => lumen_module::lumen_core::warn_line!("{NAME}: {e}"),
        }
        // One node: it reads what happened, and when anything did, queues
        // the module's world-wide work. Before the script handlers, so an
        // `on_ready` finds the commands the markup declared, and a command
        // run by a key raises its event on the tick the key came.
        app.add_systems(
            TickStage::Systems,
            gather.before(ScriptSet::Ready).before(ScriptSet::Dispatch),
        );
    }
}

fn install_observers(world: &mut World) {
    world.add_observer(
        |ev: On<Add, LumenTag>, tags: Query<&LumenTag>, mut arrivals: ResMut<Arrivals>| {
            if tags
                .get(ev.entity)
                .is_ok_and(|t| matches!(&*t.0, COMMAND | PALETTE_TAG))
            {
                arrivals.new.push(ev.entity);
            }
        },
    );
    world.add_observer(
        |ev: On<Remove, CommandElement>,
         elements: Query<&CommandElement>,
         mut arrivals: ResMut<Arrivals>| {
            if let Ok(c) = elements.get(ev.entity) {
                arrivals.gone.push(c.id.clone());
            }
        },
    );
}

/// One key press, as the module needs it.
struct Press {
    key: Key,
    code: KeyCode,
    modifiers: Modifiers,
    /// The focused element keeps it (a text field typing, a viewport).
    kept: bool,
}

/// A menu item that appeared or whose attributes changed.
type ChangedItems = (
    With<MenuItem>,
    Or<(Added<MenuItem>, Changed<LumenAttributes>)>,
);

/// What [`gather`] reads.
#[derive(bevy_ecs::system::SystemParam)]
struct Heard<'w, 's> {
    keys: MessageReader<'w, 's, KeyPressed>,
    menus: MessageReader<'w, 's, MenuClicked>,
    clicks: MessageReader<'w, 's, ClickEvent>,
    focus: FocusKeeps<'w, 's>,
    arrivals: Res<'w, Arrivals>,
    shared: Res<'w, Shared>,
    loaded: Res<'w, Loaded>,
    edited: Query<'w, 's, Entity, (With<CommandElement>, Changed<LumenAttributes>)>,
    items: Query<'w, 's, Entity, ChangedItems>,
    typed: Query<
        'w,
        's,
        (),
        (
            With<PaletteInput>,
            Changed<lumen_module::lumen_core::components::TextContent>,
        ),
    >,
    palette: Res<'w, Palette>,
}

/// Read what happened this tick and queue the work it calls for. Costs one
/// look at each reader on a tick where nothing did.
fn gather(mut heard: Heard, mut commands: Commands) {
    let presses: Vec<Press> = heard
        .keys
        .read()
        .filter(|k| !k.repeat)
        .map(|k| Press {
            key: k.key.clone(),
            code: k.code,
            modifiers: k.modifiers,
            kept: heard.focus.keeps(&k.key, &k.modifiers),
        })
        .collect();
    let menus: Vec<String> = heard.menus.read().map(|m| m.id.clone()).collect();
    let clicks: Vec<Entity> = if heard.palette.is_open() {
        heard.clicks.read().map(|c| c.entity).collect()
    } else {
        heard.clicks.read().for_each(drop);
        Vec::new()
    };
    let edited: Vec<Entity> = heard.edited.iter().collect();
    let items: Vec<Entity> = heard.items.iter().collect();
    let typed = !heard.typed.is_empty();
    let pending = {
        let core = heard.shared.lock();
        core.changed
            || core.save_due
            || !core.queue.is_empty()
            || core.palette.is_some()
            || !core.rebound.is_empty()
            || core.recording.is_some() && !presses.is_empty()
    };
    let due = !presses.is_empty()
        || !menus.is_empty()
        || !clicks.is_empty()
        || !edited.is_empty()
        || !items.is_empty()
        || typed
        || pending
        || !heard.loaded.0
        || heard.palette.is_open()
        || !heard.arrivals.new.is_empty()
        || !heard.arrivals.gone.is_empty();
    if !due {
        return;
    }
    commands.queue(move |world: &mut World| {
        process(
            world,
            Work {
                presses,
                menus,
                clicks,
                edited,
                items,
                typed,
            },
        );
    });
}

/// This tick's input, for [`process`].
struct Work {
    presses: Vec<Press>,
    menus: Vec<String>,
    clicks: Vec<Entity>,
    edited: Vec<Entity>,
    items: Vec<Entity>,
    typed: bool,
}

fn process(world: &mut World, work: Work) {
    let shared = world.resource::<Shared>().clone();
    if !world.resource::<Loaded>().0 {
        world.resource_mut::<Loaded>().0 = true;
        if world.get_resource::<Persist>().copied().unwrap_or_default() == Persist::Auto {
            persist::load(&mut shared.lock());
        }
    }
    adopt(world, &shared, &work.edited);
    palette::follow_signal(world);
    for press in &work.presses {
        hear_key(world, &shared, press);
    }
    for id in &work.menus {
        if let Some(command) = command_of_menu_item(world, &shared, id) {
            shared.lock().run(&command);
        }
    }
    for &clicked in &work.clicks {
        if let Some(id) = palette::row_at(world, clicked) {
            palette::close(world);
            shared.lock().run(&id);
        }
    }
    let ask = shared.lock().palette.take();
    if ask == Some(PaletteAsk::Open) {
        palette::open(world, &shared);
    }
    run_queued(world, &shared);
    let rebound = std::mem::take(&mut shared.lock().rebound);
    for id in rebound {
        let keys = shared.lock().keys_text(&id);
        keys_changed(world, &id, &keys);
    }
    let changed = std::mem::take(&mut shared.lock().changed);
    if changed {
        let chords = shared.lock().all_chords();
        world
            .get_resource_or_init::<ReservedChords>()
            .set(NAME, chords);
        sync_menu_items(world, &shared, None);
    } else if !work.items.is_empty() {
        sync_menu_items(world, &shared, Some(&work.items));
    }
    if changed || work.typed {
        palette::refresh(world, &shared);
    }
    let save = {
        let mut core = shared.lock();
        std::mem::take(&mut core.save_due)
    };
    if save && world.get_resource::<Persist>().copied().unwrap_or_default() == Persist::Auto {
        persist::save(&shared.lock());
    }
}

/// Read the commands the markup declared, and follow `<command>` elements
/// that changed or went.
fn adopt(world: &mut World, shared: &Shared, edited: &[Entity]) {
    let (new, gone) = {
        let mut a = world.resource_mut::<Arrivals>();
        (std::mem::take(&mut a.new), std::mem::take(&mut a.gone))
    };
    for id in gone {
        let mut core = shared.lock();
        if core.get(&id).is_some_and(|c| c.from_markup) {
            core.unregister(&id);
        }
    }
    for e in new.into_iter().chain(edited.iter().copied()) {
        let Ok(entity) = world.get_entity(e) else {
            continue;
        };
        let tag = entity.get::<LumenTag>().map(|t| t.0.clone());
        match tag.as_deref() {
            Some(COMMAND) => read_command(world, shared, e),
            Some(PALETTE_TAG) => palette::adopt_host(world, e),
            _ => {}
        }
    }
}

fn read_command(world: &mut World, shared: &Shared, e: Entity) {
    let attrs = world.get::<LumenAttributes>(e).cloned().unwrap_or_default();
    let id = world
        .get::<LumenId>(e)
        .map(|i| i.0.clone())
        .or_else(|| attrs.get("id").map(str::to_string))
        .unwrap_or_default();
    if id.trim().is_empty() {
        lumen_module::lumen_core::warn_line!("{NAME}: a <command> needs an id; it is left out");
        return;
    }
    let keys = attrs.get("keys").unwrap_or("");
    let defaults = match chords(keys) {
        Ok(c) => c,
        Err(e) => {
            lumen_module::lumen_core::warn_line!("{NAME}: <command id=\"{id}\">: {e}");
            Vec::new()
        }
    };
    let enabled = attrs
        .get("enabled")
        .is_none_or(|v| !matches!(v.trim(), "false" | "0" | "no"));
    let command = Command {
        id: id.clone(),
        label: attrs.get("label").unwrap_or(id.as_str()).to_string(),
        label_key: attrs
            .get("translatable")
            .filter(|k| !k.trim().is_empty())
            .map(str::to_string),
        defaults,
        enabled,
        from_markup: true,
    };
    let previous = world.get::<CommandElement>(e).map(|c| c.id.clone());
    let mut core = shared.lock();
    if let Some(old) = previous.filter(|old| *old != id) {
        core.unregister(&old);
    }
    let _ = core.register(command);
    drop(core);
    world.entity_mut(e).insert(CommandElement { id });
}

/// Answer one key press: a binding being recorded takes it, the open
/// palette takes its keys, and otherwise the first enabled command bound to
/// it runs, unless the focused element keeps it.
fn hear_key(world: &mut World, shared: &Shared, press: &Press) {
    let recording = shared.lock().recording.clone();
    if let Some(id) = recording {
        if is_modifier(&press.key) {
            return;
        }
        let mut core = shared.lock();
        core.recording = None;
        if press.key != Key::Named(NamedKey::Escape) {
            let chord = Chord {
                key: match &press.key {
                    Key::Character(c) if c.chars().count() == 1 => Key::Character(c.to_lowercase()),
                    other => other.clone(),
                },
                modifiers: press.modifiers,
            };
            let _ = core.bind(&id, vec![chord]);
        } else {
            core.rebound.push(id);
        }
        return;
    }
    if palette::is_open(world) && palette::hear_key(world, shared, &press.key) {
        return;
    }
    if press.kept {
        return;
    }
    let mut core = shared.lock();
    let found = core
        .find(&press.key, press.code, &press.modifiers)
        .map(str::to_string);
    if let Some(id) = found {
        core.run(&id);
    }
}

/// A key that is only a modifier, which a recording waits past.
fn is_modifier(key: &Key) -> bool {
    matches!(key, Key::Character(c) if matches!(c.as_str(), "Shift" | "Control" | "Alt" | "Meta" | "Super" | "AltGraph" | "CapsLock"))
}

/// Raise `keyschange` on the document: the chords of `id` changed.
fn keys_changed(world: &mut World, id: &str, keys: &str) {
    if let Some(root) = document_root(world) {
        world.write_message(
            CustomEvent::new(root, "keyschange")
                .with("id", id)
                .with("keys", keys),
        );
    }
}

fn document_root(world: &World) -> Option<Entity> {
    world
        .get_resource::<lumen_module::lumen_scene::spawn::DocumentRoot>()
        .map(|r| r.0)
}

/// The command a click on menu item `id` runs: the `command` of an item with
/// that id, else (an item of the OS menu bar, which is not an element) the
/// command whose id it is.
fn command_of_menu_item(world: &mut World, shared: &Shared, id: &str) -> Option<String> {
    let mut q = world.query_filtered::<(&LumenId, Option<&LumenAttributes>), With<MenuItem>>();
    let mut element = false;
    for (item_id, attrs) in q.iter(world) {
        if item_id.0 == id {
            element = true;
            if let Some(c) = attrs.and_then(|a| a.get("command")) {
                return Some(c.to_string());
            }
        }
    }
    (!element && shared.lock().get(id).is_some()).then(|| id.to_string())
}

/// Run what is queued: raise `command` on the document for each, and open
/// the palette for its own command.
fn run_queued(world: &mut World, shared: &Shared) {
    let queue = std::mem::take(&mut shared.lock().queue);
    if queue.is_empty() {
        return;
    }
    let root = document_root(world);
    for id in queue {
        if id == PALETTE {
            if palette::is_open(world) {
                palette::close(world);
            } else {
                palette::open(world, shared);
            }
        }
        if let Some(root) = root {
            world.write_message(CustomEvent::new(root, COMMAND).with("id", id.as_str()));
        }
        world.write_message(CommandRun { id });
    }
}

/// Give every menu item that names a command (or only `only`) the command's
/// label, chord and enabled state, writing only what differs.
fn sync_menu_items(world: &mut World, shared: &Shared, only: Option<&[Entity]>) {
    let mut q = world.query_filtered::<(Entity, &LumenAttributes), With<MenuItem>>();
    let targets: Vec<(Entity, String)> = q
        .iter(world)
        .filter(|(e, _)| only.is_none_or(|o| o.contains(e)))
        .filter_map(|(e, a)| a.get("command").map(|c| (e, c.to_string())))
        .collect();
    for (e, id) in targets {
        let (label, accel, enabled) = {
            let core = shared.lock();
            let Some(c) = core.get(&id) else { continue };
            (
                c.label(),
                core.keys(&id).first().map(Chord::to_text),
                c.enabled,
            )
        };
        let mut ent = world.entity_mut(e);
        let attrs = ent.get::<LumenAttributes>().cloned().unwrap_or_default();
        if attrs.get("label") != Some(label.as_str()) || attrs.get("accel") != accel.as_deref() {
            let mut next = attrs;
            next.set("label", label);
            match accel {
                Some(a) => next.set("accel", a),
                None => {
                    next.remove("accel");
                }
            }
            ent.insert(next);
        }
        let disabled = ent.contains::<Disabled>();
        if disabled == enabled {
            if enabled {
                ent.remove::<Disabled>();
            } else {
                ent.insert(Disabled);
            }
            ent.insert(lumen_module::lumen_core::components::StyleDirty);
        }
    }
}

/// The parent of `e`.
pub(crate) fn parent(world: &World, e: Entity) -> Option<Entity> {
    world.get::<ChildOf>(e).map(ChildOf::parent)
}
