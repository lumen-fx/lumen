//! Behaviour of the in-window menu bar a platform with no native one draws.
//!
//! The bar's buttons are [`DropdownButton`]s with no options, bound to a
//! menu's `__menu_open:<id>` signal: a click opens and closes the menu, and
//! the popup machinery anchors it below the button. What a menu bar does on
//! top of that lives here:
//!
//! - [`switch_menubar_menus_on_hover`]: while one of the bar's menus is
//!   open, moving the pointer onto another of its buttons opens that menu in
//!   its place, as every desktop menu bar does.
//! - [`dispatch_menu_accelerators`]: an item's `accel` works as a shortcut
//!   while the window has focus, the way the OS runs it for a native bar,
//!   and raises the same [`MenuClicked`] a click on the item does.
//!
//! The bar floats at the top of the root, out of the root's own layout, and
//! [`crate::ReservedEdge::Top`] on it keeps its height clear above the app's
//! content.

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::chord::{Chord, ReservedChords};
use lumen_core::input::{KeyPressed, MenuClicked};
use lumen_core::prelude::*;
use lumen_input::FocusKeeps;

use crate::tabs::DropdownButton;

/// Prefix of the reserved signal a menu's open state lives in.
const MENU_OPEN_PREFIX: &str = "__menu_open:";

/// Whether `button` opens a menu rather than a dropdown's option list. Only
/// a menu bar builds those.
fn opens_a_menu(button: &DropdownButton) -> bool {
    button.open_signal.starts_with(MENU_OPEN_PREFIX)
}

/// While a menu bar menu is open, open the menu of the bar button the
/// pointer has just moved onto in its place. The button counts as moved
/// onto when the pointer reaches it or any part of it, its label included.
pub fn switch_menubar_menus_on_hover(
    mut store: ResMut<PropertyStore>,
    buttons: Query<&DropdownButton>,
    newly_hovered: Query<Entity, Added<HoverWithin>>,
) {
    for entity in &newly_hovered {
        let Ok(target) = buttons.get(entity) else {
            continue;
        };
        if !opens_a_menu(target) || store.get_global_bool(&target.open_signal) == Some(true) {
            continue;
        }
        let open: Vec<String> = buttons
            .iter()
            .filter(|b| opens_a_menu(b) && store.get_global_bool(&b.open_signal) == Some(true))
            .map(|b| b.open_signal.clone())
            .collect();
        if open.is_empty() {
            continue;
        }
        for signal in open {
            store.set_global_bool(&signal, false);
        }
        store.set_global_bool(&target.open_signal, true);
    }
}

/// The name the menu bar's shortcuts go under in [`ReservedChords`].
pub const OWNER: &str = "menubar";

/// The shortcuts the in-window menu bar's items declare, each with the id
/// of the item it activates.
#[derive(Resource, Debug, Clone, Default)]
pub struct MenuAccelerators(Vec<(Chord, String)>);

impl MenuAccelerators {
    /// Collect `(item id, accel)` pairs. An accelerator that does not parse
    /// is left out and reported once.
    pub fn from_entries<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut out = Vec::new();
        for (id, accel) in entries {
            match Chord::parse(accel) {
                Some(a) => out.push((a, id.to_string())),
                None => lumen_core::warn_line!(
                    "menubar: item `{id}` has an accel `{accel}` this platform cannot read"
                ),
            }
        }
        Self(out)
    }

    /// How many shortcuts there are.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The shortcuts, for [`ReservedChords`] under [`OWNER`].
    pub fn chords(&self) -> Vec<Chord> {
        self.0.iter().map(|(c, _)| c.clone()).collect()
    }
}

/// Activate the menu bar item whose shortcut was pressed, as a click on it
/// would. A held key's repeats do not activate it again, a key the focused
/// element keeps (a text field's typing and editing chords, every key of an
/// element that takes them all) is left to it, and so is a chord something
/// else in the app answers app-wide ([`ReservedChords`]), such as a command
/// the item runs, so the chord fires once.
pub fn dispatch_menu_accelerators(
    accelerators: Res<MenuAccelerators>,
    focus: FocusKeeps,
    reserved: Option<Res<ReservedChords>>,
    mut keys: MessageReader<KeyPressed>,
    mut clicked: MessageWriter<MenuClicked>,
) {
    if accelerators.is_empty() {
        keys.read().for_each(drop);
        return;
    }
    for key in keys.read() {
        if key.repeat
            || focus.keeps(&key.key, &key.modifiers)
            || reserved
                .as_ref()
                .is_some_and(|r| r.taken_by_other(OWNER, &key.key, key.code, &key.modifiers))
        {
            continue;
        }
        if let Some((_, id)) = accelerators
            .0
            .iter()
            .find(|(a, _)| a.matches(&key.key, key.code, &key.modifiers))
        {
            clicked.write(MenuClicked { id: id.clone() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;

    fn press(world: &mut World, key: &str, modifiers: Modifiers) {
        world.write_message(KeyPressed {
            key: Key::from(key),
            code: lumen_core::input::KeyCode::Unidentified,
            modifiers,
            repeat: false,
            time_stamp: lumen_core::time::Instant::now(),
        });
    }

    fn clicked(world: &mut World) -> Vec<String> {
        let ids = world
            .resource::<Messages<MenuClicked>>()
            .iter_current_update_messages()
            .map(|m| m.id.clone())
            .collect();
        world.resource_mut::<Messages<MenuClicked>>().clear();
        world.resource_mut::<Messages<KeyPressed>>().clear();
        ids
    }

    /// An item's shortcut raises the item's `MenuClicked`; the same key with
    /// other modifiers, or without them, does not.
    #[test]
    fn a_shortcut_activates_its_item() {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        world.init_resource::<Messages<MenuClicked>>();
        world.insert_resource(MenuAccelerators::from_entries([
            ("open", "Control+O"),
            ("save-as", "Ctrl+Shift+S"),
            ("help", "F1"),
        ]));
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::default()
        };

        press(&mut world, "o", ctrl);
        world.run_system_once(dispatch_menu_accelerators).unwrap();
        assert_eq!(clicked(&mut world), vec!["open"]);

        press(&mut world, "S", ctrl_shift);
        world.run_system_once(dispatch_menu_accelerators).unwrap();
        assert_eq!(clicked(&mut world), vec!["save-as"]);

        press(&mut world, "F1", Modifiers::default());
        world.run_system_once(dispatch_menu_accelerators).unwrap();
        assert_eq!(clicked(&mut world), vec!["help"]);

        press(&mut world, "o", Modifiers::default());
        press(&mut world, "s", ctrl);
        world.run_system_once(dispatch_menu_accelerators).unwrap();
        assert!(clicked(&mut world).is_empty());
    }

    /// A focused text field keeps its typing and editing chords: Ctrl+Z
    /// there undoes the edit and does not reach the bar's Undo, while a
    /// chord the field does not use still does.
    #[test]
    fn a_focused_text_field_keeps_its_editing_chords() {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        world.init_resource::<Messages<MenuClicked>>();
        world.insert_resource(MenuAccelerators::from_entries([
            ("undo", "Ctrl+Z"),
            ("save", "Ctrl+S"),
            ("help", "F1"),
        ]));
        let field = world
            .spawn(lumen_core::components::TextInput::default())
            .id();
        world.insert_resource(FocusTracker(Some(field)));
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        press(&mut world, "z", ctrl);
        world.run_system_once(dispatch_menu_accelerators).unwrap();
        assert!(clicked(&mut world).is_empty(), "the field undoes");
        press(&mut world, "s", ctrl);
        world.run_system_once(dispatch_menu_accelerators).unwrap();
        assert_eq!(clicked(&mut world), vec!["save"]);
        press(&mut world, "F1", Modifiers::default());
        world.run_system_once(dispatch_menu_accelerators).unwrap();
        assert_eq!(clicked(&mut world), vec!["help"]);

        // An element that takes every key keeps them all.
        world
            .entity_mut(field)
            .remove::<lumen_core::components::TextInput>()
            .insert(lumen_core::input::TakesEveryKey);
        press(&mut world, "s", ctrl);
        world.run_system_once(dispatch_menu_accelerators).unwrap();
        assert!(clicked(&mut world).is_empty());
    }

    /// With one bar menu open, hovering another bar button opens its menu
    /// instead. With none open, hovering opens nothing, and a dropdown's
    /// header is not a bar button.
    #[test]
    fn hovering_another_bar_button_switches_the_open_menu() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        let file = world
            .spawn(DropdownButton::new("__menu_open:menubar-0"))
            .id();
        let edit = world
            .spawn(DropdownButton::new("__menu_open:menubar-1"))
            .id();
        let combo = world
            .spawn(DropdownButton::new("__dropdown_open:fruit"))
            .id();
        let is_open = |world: &World, s: &str| {
            world.resource::<PropertyStore>().get_global_bool(s) == Some(true)
        };

        world.entity_mut(edit).insert(HoverWithin);
        world
            .run_system_once(switch_menubar_menus_on_hover)
            .unwrap();
        assert!(
            !is_open(&world, "__menu_open:menubar-1"),
            "nothing was open"
        );
        world.entity_mut(edit).remove::<HoverWithin>();

        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__menu_open:menubar-0", true);
        world.entity_mut(combo).insert(HoverWithin);
        world
            .run_system_once(switch_menubar_menus_on_hover)
            .unwrap();
        assert!(is_open(&world, "__menu_open:menubar-0"));
        assert!(!is_open(&world, "__dropdown_open:fruit"));
        world.entity_mut(combo).remove::<HoverWithin>();

        world.entity_mut(edit).insert(HoverWithin);
        world
            .run_system_once(switch_menubar_menus_on_hover)
            .unwrap();
        assert!(!is_open(&world, "__menu_open:menubar-0"));
        assert!(is_open(&world, "__menu_open:menubar-1"));
        let _ = file;
    }
}
