//! Floating-panel behavior shared by `<dropdown>` and `<menu>`:
//! outside-click dismissal and viewport edge-flip anchoring.
//!
//! The markup parser wraps each panel in an `<if eq="true" mode="hide">`
//! keyed on a synthetic open signal (`__dropdown_open:*` /
//! `__menu_open:*`) and tags the panel entity with [`PopupPanel`]. Two
//! tiny systems then act on any open panel:
//!
//! - [`dismiss_popups_on_outside_press`] closes a panel when a primary
//!   press lands outside both the panel's subtree and its trigger.
//! - [`flip_open_dropdown_panels`] positions a dropdown panel just below
//!   its trigger, flipping above when the panel would overflow the
//!   viewport bottom - the same edge-flip rule
//!   [`crate::tooltip`] applies, factored into [`anchored_popup_origin`].

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::components::Style;
use lumen_core::prelude::*;
use lumen_core::render_world::Viewport;

use crate::tabs::DropdownButton;

/// Marker on a `<dropdown>` / `<menu>` floating panel. Carries the
/// open-state signal so the dismissal + edge-flip systems can query the
/// panel by its bound signal, plus the panel's authored vertical inset
/// so the flip system can restore it when the panel closes.
#[derive(Component, Clone, Debug)]
pub struct PopupPanel {
    /// Open-state signal (`__dropdown_open:*` / `__menu_open:*`).
    pub open_signal: String,
    /// Authored `inset.top` (px) - the resting placement below the
    /// trigger, restored on close.
    pub default_top: f32,
    /// Authored `inset.bottom` (px), restored on close.
    pub default_bottom: f32,
    /// Whether the flip system has overwritten the authored inset for
    /// the current open session (so it restores exactly once on close).
    pub positioned: bool,
}

/// Which side of the anchor a popup prefers on its primary axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupSide {
    /// Below the anchor; flips above near the viewport bottom
    /// (dropdowns, menus).
    Below,
    /// Right of the anchor; flips left near the viewport right edge
    /// (tooltips, submenus).
    Right,
}

/// Viewport-space top-left for a `popup`-sized box anchored to a trigger
/// rect (`anchor_pos` / `anchor_size`), preferring `side` and flipping
/// to the opposite side of the viewport when the preferred placement
/// would overflow. `gap` px separate the popup from the anchor on the
/// primary axis; the cross axis aligns to the anchor's leading edge and
/// also flips inward when it would overflow.
///
/// Generalises the edge-flip `tooltip::spawn_tooltip_popups` performs so
/// dropdown and menu panels can share one anchoring rule (Qt's
/// `QToolTip` / Adwaita popover contract).
pub fn anchored_popup_origin(
    anchor_pos: Vec2,
    anchor_size: Vec2,
    popup: Vec2,
    viewport: Vec2,
    gap: f32,
    side: PopupSide,
) -> Vec2 {
    match side {
        PopupSide::Below => {
            let below = anchor_pos.y + anchor_size.y + gap;
            let y = if below + popup.y <= viewport.y {
                below
            } else {
                (anchor_pos.y - gap - popup.y).max(0.0)
            };
            let x = if anchor_pos.x + popup.x <= viewport.x {
                anchor_pos.x
            } else {
                (anchor_pos.x + anchor_size.x - popup.x).max(0.0)
            };
            Vec2::new(x, y)
        }
        PopupSide::Right => {
            let right = anchor_pos.x + anchor_size.x + gap;
            let x = if right + popup.x <= viewport.x {
                right
            } else {
                (anchor_pos.x - gap - popup.x).max(0.0)
            };
            let y = if anchor_pos.y + popup.y <= viewport.y {
                anchor_pos.y
            } else {
                (anchor_pos.y + anchor_size.y - popup.y).max(0.0)
            };
            Vec2::new(x, y)
        }
    }
}

/// Gap (px) between a dropdown panel and its trigger.
const POPUP_GAP: f32 = 4.0;

/// Close any open [`PopupPanel`] when a primary press lands outside both
/// the panel's own subtree and (for dropdowns) its trigger header.
///
/// Reads [`PointerPressed`] and the entity currently under the cursor
/// ([`Hovered`], maintained by `lumen_input::hit_test`). A press over
/// empty space clears the cursor target, so it dismisses every open
/// panel. Pressing the dropdown header is treated as *inside* so the
/// header's own toggle (`tabs::dispatch_dropdown_clicks`, which fires on
/// release) stays authoritative and the panel doesn't close-then-reopen.
pub fn dismiss_popups_on_outside_press(
    mut presses: MessageReader<PointerPressed>,
    hovered: Query<Entity, With<Hovered>>,
    panels: Query<(Entity, &PopupPanel)>,
    headers: Query<&DropdownButton>,
    parents: Query<&ChildOf>,
    mut store: ResMut<PropertyStore>,
) {
    let mut primary_press = false;
    for press in presses.read() {
        if matches!(press.button, PointerButton::Primary) {
            primary_press = true;
        }
    }
    if !primary_press {
        return;
    }
    // At most one entity is hovered; `None` means the cursor is over
    // empty space (a genuine outside click).
    let target = hovered.iter().next();
    for (panel_e, panel) in &panels {
        if store.get_global_bool(&panel.open_signal) != Some(true) {
            continue;
        }
        let inside = target
            .map(|t| press_hits_popup(t, panel_e, &panel.open_signal, &headers, &parents))
            .unwrap_or(false);
        if !inside {
            store.set_global_bool(&panel.open_signal, false);
        }
    }
}

/// Walk `target`'s ancestor chain; the press counts as inside the popup
/// when it reaches the panel entity (its subtree) or a dropdown header
/// bound to the same open signal (the trigger).
fn press_hits_popup(
    target: Entity,
    panel_e: Entity,
    open_signal: &str,
    headers: &Query<&DropdownButton>,
    parents: &Query<&ChildOf>,
) -> bool {
    let mut cur = Some(target);
    while let Some(e) = cur {
        if e == panel_e {
            return true;
        }
        if headers
            .get(e)
            .map(|h| h.open_signal == open_signal)
            .unwrap_or(false)
        {
            return true;
        }
        cur = parents.get(e).ok().map(|c| c.parent());
    }
    false
}

/// Anchor each open dropdown panel below its trigger header, flipping it
/// above when the panel's bottom edge would fall past the viewport
/// bottom. Runs after the first layout pass has measured the panel
/// (mirrors the tooltip's spawn-then-measure ordering).
///
/// The panel is `position: absolute` inside its `<if>` wrapper (its
/// containing block), so the viewport-space origin from
/// [`anchored_popup_origin`] is converted to a local `inset.top` against
/// the containing block; `inset.bottom` is released to `auto` (`NaN`) so
/// the panel sizes to its option list in both orientations. Menu panels
/// have no queryable trigger and are left untouched here - their
/// dismissal still works via [`dismiss_popups_on_outside_press`].
pub fn flip_open_dropdown_panels(
    viewport: Option<Res<Viewport>>,
    store: Res<PropertyStore>,
    headers: Query<(&DropdownButton, &Transform)>,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
    mut panels: Query<(Entity, &mut PopupPanel, &mut Style)>,
) {
    let Some(viewport) = viewport else {
        return;
    };
    let vp = viewport.size;
    for (panel_e, mut panel, mut style) in &mut panels {
        let open = store.get_global_bool(&panel.open_signal) == Some(true);
        if !open {
            // Restore the authored resting inset once, on close.
            if panel.positioned {
                style.inset.top = panel.default_top;
                style.inset.bottom = panel.default_bottom;
                panel.positioned = false;
            }
            continue;
        }
        // Anchor to the dropdown header bound to the same open signal.
        let Some((_, header_t)) = headers
            .iter()
            .find(|(h, _)| h.open_signal == panel.open_signal)
        else {
            continue;
        };
        // Release the panel to content height so layout can measure it,
        // then position precisely once it has a size.
        if !panel.positioned {
            style.inset.top = panel.default_top;
            style.inset.bottom = f32::NAN;
            panel.positioned = true;
        }
        let Ok(panel_t) = transforms.get(panel_e) else {
            continue;
        };
        if panel_t.size.y <= 0.0 {
            continue;
        }
        let Ok(cb) = parents.get(panel_e) else {
            continue;
        };
        let Ok(cb_t) = transforms.get(cb.parent()) else {
            continue;
        };
        let origin = anchored_popup_origin(
            header_t.absolute,
            header_t.size,
            panel_t.size,
            vp,
            POPUP_GAP,
            PopupSide::Below,
        );
        let local_top = origin.y - cb_t.absolute.y;
        if !edge_eq(style.inset.top, local_top) {
            style.inset.top = local_top;
            style.inset.bottom = f32::NAN;
        }
    }
}

/// Inset comparison that treats `NaN` (`auto`) as equal to itself so the
/// flip system doesn't rewrite (and needlessly re-lay-out) an unchanged
/// `auto` edge every tick.
fn edge_eq(a: f32, b: f32) -> bool {
    if a.is_nan() && b.is_nan() {
        return true;
    }
    (a - b).abs() < 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_placement_prefers_under_anchor() {
        // Anchor at (10, 10), 100x30. Roomy viewport -> sits below.
        let o = anchored_popup_origin(
            Vec2::new(10.0, 10.0),
            Vec2::new(100.0, 30.0),
            Vec2::new(100.0, 80.0),
            Vec2::new(500.0, 500.0),
            4.0,
            PopupSide::Below,
        );
        assert_eq!(o.x, 10.0, "aligns to anchor left");
        assert_eq!(o.y, 44.0, "10 + 30 + 4 gap");
    }

    #[test]
    fn below_placement_flips_above_near_viewport_bottom() {
        // Anchor near the bottom edge; an 80-tall panel below would
        // overflow, so it flips above the anchor.
        let anchor_y = 460.0;
        let o = anchored_popup_origin(
            Vec2::new(10.0, anchor_y),
            Vec2::new(100.0, 30.0),
            Vec2::new(100.0, 80.0),
            Vec2::new(500.0, 500.0),
            4.0,
            PopupSide::Below,
        );
        // 460 + 30 + 4 + 80 = 574 > 500 -> flip: anchor_y - gap - height.
        assert_eq!(o.y, anchor_y - 4.0 - 80.0);
        assert!(o.y < anchor_y, "panel now sits above the trigger");
    }

    #[test]
    fn right_placement_flips_left_near_viewport_right() {
        let anchor_x = 470.0;
        let o = anchored_popup_origin(
            Vec2::new(anchor_x, 10.0),
            Vec2::new(20.0, 20.0),
            Vec2::new(120.0, 40.0),
            Vec2::new(500.0, 500.0),
            8.0,
            PopupSide::Right,
        );
        // 470 + 20 + 8 + 120 = 618 > 500 -> flip left.
        assert_eq!(o.x, anchor_x - 8.0 - 120.0);
    }

    #[test]
    fn edge_eq_treats_nan_as_equal() {
        assert!(edge_eq(f32::NAN, f32::NAN));
        assert!(!edge_eq(f32::NAN, 4.0));
        assert!(edge_eq(4.0, 4.2));
        assert!(!edge_eq(4.0, 40.0));
    }

    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;
    use bevy_ecs::world::World;

    fn open_panel_world() -> (World, Entity) {
        let mut world = World::new();
        world.init_resource::<Messages<PointerPressed>>();
        world.insert_resource(PropertyStore::default());
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__dropdown_open:fruit", true);
        let panel = world
            .spawn(PopupPanel {
                open_signal: "__dropdown_open:fruit".to_string(),
                default_top: 40.0,
                default_bottom: -1.0,
                positioned: false,
            })
            .id();
        (world, panel)
    }

    fn press_primary(world: &mut World) {
        world
            .resource_mut::<Messages<PointerPressed>>()
            .write(PointerPressed {
                position: Vec2::ZERO,
                button: PointerButton::Primary,
            });
    }

    fn is_open(world: &World) -> bool {
        world
            .resource::<PropertyStore>()
            .get_global_bool("__dropdown_open:fruit")
            == Some(true)
    }

    #[test]
    fn outside_press_closes_panel() {
        let (mut world, _panel) = open_panel_world();
        // Cursor over an unrelated entity - outside the panel + trigger.
        world.spawn(Hovered);
        press_primary(&mut world);
        world
            .run_system_once(dismiss_popups_on_outside_press)
            .unwrap();
        assert!(!is_open(&world), "press outside the panel closes it");
    }

    #[test]
    fn press_inside_panel_keeps_it_open() {
        let (mut world, panel) = open_panel_world();
        // Cursor over the panel entity itself (its subtree).
        world.entity_mut(panel).insert(Hovered);
        press_primary(&mut world);
        world
            .run_system_once(dismiss_popups_on_outside_press)
            .unwrap();
        assert!(is_open(&world), "press inside the panel leaves it open");
    }

    #[test]
    fn press_on_trigger_keeps_it_open() {
        let (mut world, _panel) = open_panel_world();
        // Cursor over the dropdown header (the trigger) - the header's
        // own toggle owns close-on-reclick, so dismissal must stand down.
        world.spawn((DropdownButton::new("__dropdown_open:fruit"), Hovered));
        press_primary(&mut world);
        world
            .run_system_once(dismiss_popups_on_outside_press)
            .unwrap();
        assert!(is_open(&world), "press on the trigger leaves it open");
    }

    /// Build a dropdown (header + containing block + panel) whose header
    /// sits `header_y` px down a 500 px-tall viewport, then run the flip
    /// system once and return the panel's resulting `inset.top`.
    fn run_flip(header_y: f32) -> f32 {
        let mut world = World::new();
        world.insert_resource(Viewport {
            size: Vec2::new(500.0, 500.0),
            ..Viewport::default()
        });
        world.insert_resource(PropertyStore::default());
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__dropdown_open:fruit", true);

        world.spawn((
            DropdownButton::new("__dropdown_open:fruit"),
            Transform::new(Vec2::new(10.0, header_y), Vec2::new(100.0, 30.0)),
        ));
        // Containing block (`<if>` wrapper) sits just below the header.
        let cb = world
            .spawn(Transform::new(
                Vec2::new(10.0, header_y + 30.0),
                Vec2::new(100.0, 0.0),
            ))
            .id();
        let panel = world
            .spawn((
                PopupPanel {
                    open_signal: "__dropdown_open:fruit".to_string(),
                    default_top: 40.0,
                    default_bottom: -1.0,
                    positioned: false,
                },
                Style::default(),
                // Pre-measured 80 px-tall panel (layout would supply this).
                Transform::new(Vec2::new(10.0, header_y + 34.0), Vec2::new(100.0, 80.0)),
                ChildOf(cb),
            ))
            .id();

        world.run_system_once(flip_open_dropdown_panels).unwrap();
        world.get::<Style>(panel).unwrap().inset.top
    }

    #[test]
    fn dropdown_panel_sits_below_when_room() {
        // Header near the top: 10 + 30 + 4 gap = 44 px below viewport
        // top; 44 + 80 = 124 < 500 -> no flip. Local top = 44 - cb.y(40).
        let top = run_flip(10.0);
        assert!(top > 0.0, "panel below the trigger, got top={top}");
        assert!((top - 4.0).abs() < 0.5, "4 px gap below the header");
    }

    #[test]
    fn dropdown_panel_flips_above_near_viewport_bottom() {
        // Header near the bottom: 460 + 30 + 4 + 80 = 574 > 500 -> flip.
        // Panel is anchored above, so its local top goes negative.
        let top = run_flip(460.0);
        assert!(top < 0.0, "panel flips above the trigger, got top={top}");
    }
}
