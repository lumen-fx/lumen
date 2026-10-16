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
//! - [`reserve_menubar_space`]: the bar floats at the top of the root, out
//!   of the root's own layout, so `justify` and `align` on the root still
//!   place the app's content the way they would without a bar; the root
//!   keeps the bar's height clear above that content.

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::components::{Length, Style};
use lumen_core::input::{KeyPressed, MenuClicked};
use lumen_core::prelude::*;
use lumen_core::style_overrides::{Live, StyleFields, StyleOverrides, StyleOwner, StyleValues};

use crate::tabs::DropdownButton;

/// Marker the runtime puts on the in-window menu bar it mounts as the first
/// child of the root.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct InWindowMenuBar;

impl StyleOwner for InWindowMenuBar {
    const RANK: u8 = 10;
}

/// Keep the in-window menu bar's height clear at the top of the root.
///
/// The bar is absolutely positioned at the root's top edge, so it takes no
/// part in how the root lays out its content. The root's top padding is
/// held at what its styles say plus the bar's height, which is what moves
/// that content below the bar. The height is the bar's own `height` when it
/// has a fixed one (the user-agent sheet gives it one, so the first frame
/// already has room), else the height layout measured, in which case a tick
/// that changes the reservation asks for another to lay out against it.
pub fn reserve_menubar_space(
    anim: Option<Res<lumen_core::render_world::AnimationsActive>>,
    bars: Query<(&ChildOf, &Style, Option<&Transform>), With<InWindowMenuBar>>,
    mut roots: Query<(&mut Style, &mut StyleOverrides), Without<InWindowMenuBar>>,
) {
    for (child_of, bar_style, bar_box) in &bars {
        let height = match bar_style.height {
            Length::Px(h) => h,
            _ => bar_box.map(|t| t.size.y).unwrap_or(0.0),
        };
        let Ok((mut style, mut overrides)) = roots.get_mut(child_of.parent()) else {
            continue;
        };
        let live = Live {
            style: Some(&style),
            ..Live::default()
        };
        let resting = overrides
            .below::<InWindowMenuBar>(StyleFields::PADDING, &live)
            .padding
            .unwrap_or(style.padding);
        let mut want = resting;
        want.top += height;
        let held = overrides
            .held_by::<InWindowMenuBar>()
            .and_then(|v| v.padding);
        if held.is_some_and(|p| (p.top - want.top).abs() < 0.5) {
            continue;
        }
        let out = overrides.hold::<InWindowMenuBar>(
            StyleValues {
                padding: Some(want),
                ..StyleValues::default()
            },
            &live,
        );
        out.write_style(&mut style);
        if !matches!(bar_style.height, Length::Px(_))
            && let Some(anim) = &anim
        {
            anim.request();
        }
    }
}

/// Prefix of the reserved signal a menu's open state lives in.
const MENU_OPEN_PREFIX: &str = "__menu_open:";

/// Whether `button` opens a menu rather than a dropdown's option list. Only
/// a menu bar builds those.
fn opens_a_menu(button: &DropdownButton) -> bool {
    button.open_signal.starts_with(MENU_OPEN_PREFIX)
}

/// While a menu bar menu is open, open the menu of the bar button the
/// pointer has just moved onto in its place.
pub fn switch_menubar_menus_on_hover(
    mut store: ResMut<PropertyStore>,
    buttons: Query<&DropdownButton>,
    newly_hovered: Query<Entity, Added<Hovered>>,
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

/// One shortcut: the modifiers that must be held and the key.
#[derive(Debug, Clone, PartialEq)]
struct Accelerator {
    modifiers: Modifiers,
    key: Key,
}

impl Accelerator {
    /// Read an accelerator in the form a `<menuitem accel>` takes
    /// (`CommandOrControl+Shift+S`, `Alt+F4`, `Ctrl+O`): modifiers and a key
    /// joined by `+`. `CommandOrControl` is Command on macOS and Control
    /// elsewhere. `None` for a string with no key or an unknown modifier.
    fn parse(accel: &str) -> Option<Self> {
        let mut modifiers = Modifiers::default();
        let mut parts: Vec<&str> = accel.split('+').map(str::trim).collect();
        let key = parts.pop().filter(|k| !k.is_empty())?;
        for part in parts {
            match part.to_ascii_lowercase().as_str() {
                "commandorcontrol" | "cmdorctrl" | "cmdorcontrol" | "commandorctrl" => {
                    if cfg!(target_os = "macos") {
                        modifiers.super_ = true;
                    } else {
                        modifiers.ctrl = true;
                    }
                }
                "control" | "ctrl" => modifiers.ctrl = true,
                "shift" => modifiers.shift = true,
                "alt" | "option" => modifiers.alt = true,
                "super" | "cmd" | "command" | "meta" => modifiers.super_ = true,
                _ => return None,
            }
        }
        Some(Self {
            modifiers,
            key: Key::from(key),
        })
    }

    /// Whether a key press is this shortcut. Letters compare without case,
    /// because Shift is a modifier of the shortcut, not part of its key.
    fn matches(&self, key: &Key, modifiers: &Modifiers) -> bool {
        if *modifiers != self.modifiers {
            return false;
        }
        match (&self.key, key) {
            (Key::Character(want), Key::Character(got)) => want.eq_ignore_ascii_case(got),
            (want, got) => want == got,
        }
    }
}

/// The shortcuts the in-window menu bar's items declare, each with the id
/// of the item it activates.
#[derive(Resource, Debug, Clone, Default)]
pub struct MenuAccelerators(Vec<(Accelerator, String)>);

impl MenuAccelerators {
    /// Collect `(item id, accel)` pairs. An accelerator that does not parse
    /// is left out and reported once.
    pub fn from_entries<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut out = Vec::new();
        for (id, accel) in entries {
            match Accelerator::parse(accel) {
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
}

/// Activate the menu bar item whose shortcut was pressed, as a click on it
/// would. A held key's repeats do not activate it again.
pub fn dispatch_menu_accelerators(
    accelerators: Res<MenuAccelerators>,
    mut keys: MessageReader<KeyPressed>,
    mut clicked: MessageWriter<MenuClicked>,
) {
    if accelerators.is_empty() {
        keys.read().for_each(drop);
        return;
    }
    for key in keys.read() {
        if key.repeat {
            continue;
        }
        if let Some((_, id)) = accelerators
            .0
            .iter()
            .find(|(a, _)| a.matches(&key.key, &key.modifiers))
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
            modifiers,
            repeat: false,
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

    /// `CommandOrControl` is Control off macOS and Command on it.
    #[test]
    fn command_or_control_follows_the_platform() {
        let a = Accelerator::parse("CommandOrControl+N").expect("parses");
        assert_eq!(a.modifiers.ctrl, !cfg!(target_os = "macos"));
        assert_eq!(a.modifiers.super_, cfg!(target_os = "macos"));
        assert!(Accelerator::parse("Hyper+N").is_none());
        assert!(Accelerator::parse("Ctrl+").is_none());
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

        world.entity_mut(edit).insert(Hovered);
        world
            .run_system_once(switch_menubar_menus_on_hover)
            .unwrap();
        assert!(
            !is_open(&world, "__menu_open:menubar-1"),
            "nothing was open"
        );
        world.entity_mut(edit).remove::<Hovered>();

        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__menu_open:menubar-0", true);
        world.entity_mut(combo).insert(Hovered);
        world
            .run_system_once(switch_menubar_menus_on_hover)
            .unwrap();
        assert!(is_open(&world, "__menu_open:menubar-0"));
        assert!(!is_open(&world, "__dropdown_open:fruit"));
        world.entity_mut(combo).remove::<Hovered>();

        world.entity_mut(edit).insert(Hovered);
        world
            .run_system_once(switch_menubar_menus_on_hover)
            .unwrap();
        assert!(!is_open(&world, "__menu_open:menubar-0"));
        assert!(is_open(&world, "__menu_open:menubar-1"));
        let _ = file;
    }

    /// The root keeps the bar's height clear above its content, on top of
    /// the padding its styles give it, and keeps doing so when a restyle
    /// changes that padding.
    #[test]
    fn the_root_keeps_the_bar_height_clear() {
        let mut world = World::new();
        let root = world
            .spawn((
                Style {
                    padding: lumen_core::components::Edges {
                        top: 10.0,
                        left: 4.0,
                        ..Default::default()
                    },
                    ..Style::default()
                },
                StyleOverrides::default(),
            ))
            .id();
        world.spawn((
            InWindowMenuBar,
            Style {
                height: Length::Px(32.0),
                ..Style::default()
            },
            ChildOf(root),
        ));
        world.run_system_once(reserve_menubar_space).unwrap();
        let padding = world.get::<Style>(root).unwrap().padding;
        assert_eq!((padding.top, padding.left), (42.0, 4.0));

        // Running again holds the same value rather than stacking it.
        world.run_system_once(reserve_menubar_space).unwrap();
        assert_eq!(world.get::<Style>(root).unwrap().padding.top, 42.0);
    }
}
