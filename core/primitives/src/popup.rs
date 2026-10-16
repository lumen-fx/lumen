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
//! - [`place_menus_at_points`] puts a `<menu>` panel opened with a point
//!   (`open_menu_at`, a context menu) at that point, moved to the other
//!   side of it where it would leave the window.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::components::Style;
use lumen_core::prelude::*;
use lumen_core::render_world::Viewport;
use lumen_core::style_overrides::{Live, StyleFields, StyleOverrides, StyleOwner, StyleValues};

use crate::tabs::DropdownButton;

/// Marker on a `<dropdown>` / `<menu>` floating panel. Carries the
/// open-state signal so the dismissal + edge-flip systems can query the
/// panel by its bound signal.
///
/// While the panel is open, the edge-flip holds its `inset` in
/// [`StyleOverrides`]; on close it lets go and the panel is back where its
/// styles place it, as they stand then.
#[derive(Component, Clone, Debug)]
#[require(StyleOverrides)]
pub struct PopupPanel {
    /// Open-state signal (`__dropdown_open:*` / `__menu_open:*`).
    pub open_signal: String,
}

impl StyleOwner for PopupPanel {
    const RANK: u8 = 20;
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

/// Fallback gap (px) between a dropdown panel and its trigger, used when
/// the trigger carries no [`PopupGap`] (e.g. no CSS `popup-gap` authored).
const POPUP_GAP: f32 = 4.0;

/// Gap between a popup panel and the control that opens it, in logical
/// pixels. Sits on the trigger entity.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct PopupGap(pub f32);

impl Default for PopupGap {
    fn default() -> Self {
        Self(POPUP_GAP)
    }
}

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
        if press.is_primary() {
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
///
/// The placement is held in [`StyleOverrides`] while the panel is open, so
/// a restyle meanwhile keeps it, and let go of on close.
pub fn flip_open_dropdown_panels(
    viewport: Option<Res<Viewport>>,
    store: Res<PropertyStore>,
    headers: Query<(&DropdownButton, &Transform, Option<&PopupGap>)>,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
    mut panels: Query<(Entity, &PopupPanel, &mut Style, &mut StyleOverrides)>,
) {
    let Some(viewport) = viewport else {
        return;
    };
    let vp = viewport.size;
    for (panel_e, panel, mut style, mut overrides) in &mut panels {
        let open = store.get_global_bool(&panel.open_signal) == Some(true);
        let placed = overrides.held_by::<PopupPanel>().is_some();
        if !open {
            // Back to where its styles place it, once, on close.
            if placed {
                overrides.release::<PopupPanel>().write_style(&mut style);
            }
            continue;
        }
        // Anchor to the dropdown header bound to the same open signal.
        let Some((_, header_t, gap)) = headers
            .iter()
            .find(|(h, _, _)| h.open_signal == panel.open_signal)
        else {
            continue;
        };
        // Where the styles place it; only the vertical edges move.
        let resting = overrides
            .below::<PopupPanel>(
                StyleFields::INSET,
                &Live {
                    style: Some(&style),
                    ..Live::default()
                },
            )
            .inset
            .unwrap_or(style.inset);
        let place = |top: f32| StyleValues {
            inset: Some(lumen_core::components::Edges {
                top,
                bottom: f32::NAN,
                ..resting
            }),
            ..StyleValues::default()
        };
        // Release the panel to content height so layout can measure it,
        // then position precisely once it has a size.
        if !placed {
            let live = Live {
                style: Some(&style),
                ..Live::default()
            };
            let out = overrides.hold::<PopupPanel>(place(resting.top), &live);
            out.write_style(&mut style);
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
            gap.copied().unwrap_or_default().0,
            PopupSide::Below,
        );
        let local_top = origin.y - cb_t.absolute.y;
        if !edge_eq(style.inset.top, local_top) {
            let live = Live {
                style: Some(&style),
                ..Live::default()
            };
            let out = overrides.hold::<PopupPanel>(place(local_top), &live);
            out.write_style(&mut style);
        }
    }
}

/// Prefix of the reserved signal a menu's open state lives in.
const MENU_OPEN_PREFIX: &str = "__menu_open:";

/// The reserved signal holding the window point a menu was opened at, as
/// `"x y"`, beside its `__menu_open:<id>` open state. `open_menu_at` writes
/// it; `open_menu` empties it, which opens the menu where its styles put it.
fn menu_at_signal(menu_id: &str) -> String {
    format!("__menu_at:{menu_id}")
}

/// The point a menu was opened at, when it was opened at one.
fn menu_point(store: &PropertyStore, menu_id: &str) -> Option<Vec2> {
    let raw = store.get_global_str(&menu_at_signal(menu_id))?;
    let mut parts = raw.split_whitespace().map(str::parse::<f32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(x)), Some(Ok(y))) if x.is_finite() && y.is_finite() => Some(Vec2::new(x, y)),
        _ => None,
    }
}

/// Place each open `<menu>` panel that was opened at a point with its
/// top-left corner on that point, the way a context menu opens at the
/// pointer. Where the panel would cross the window's right or bottom edge it
/// opens to the left of or above the point instead, as a native context
/// menu does.
///
/// The panel is `position: absolute` inside its `<if>` wrapper, so the
/// window point is converted to `top` / `left` against that containing
/// block, and `right` / `bottom` go to `auto` so the panel keeps its own
/// size. The placement is held in [`StyleOverrides`] while the menu is open
/// at a point and let go of when it closes or reopens without one.
///
/// Where the panel lands depends on a layout that has not run yet: its own
/// size, and where its containing block sits once a just-shown menu takes
/// part in layout again. So a tick that moves the panel asks for another,
/// which places it against the layout this one produces, and the placement
/// settles in the tick that finds nothing to move.
pub fn place_menus_at_points(
    viewport: Option<Res<Viewport>>,
    anim: Option<Res<lumen_core::render_world::AnimationsActive>>,
    store: Res<PropertyStore>,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
    mut panels: Query<(Entity, &PopupPanel, &mut Style, &mut StyleOverrides)>,
) {
    let vp = viewport
        .map(|v| v.size)
        .unwrap_or(Vec2::splat(f32::INFINITY));
    for (panel_e, panel, mut style, mut overrides) in &mut panels {
        let Some(menu_id) = panel.open_signal.strip_prefix(MENU_OPEN_PREFIX) else {
            continue;
        };
        let placed = overrides.held_by::<PopupPanel>().is_some();
        let point = (store.get_global_bool(&panel.open_signal) == Some(true))
            .then(|| menu_point(&store, menu_id))
            .flatten();
        let Some(point) = point else {
            if placed {
                overrides.release::<PopupPanel>().write_style(&mut style);
            }
            continue;
        };
        let Some(cb) = parents
            .get(panel_e)
            .ok()
            .and_then(|c| transforms.get(c.parent()).ok())
        else {
            continue;
        };
        let size = transforms
            .get(panel_e)
            .map(|t| t.size)
            .unwrap_or(Vec2::ZERO);
        let origin = anchored_popup_origin(point, Vec2::ZERO, size, vp, 0.0, PopupSide::Below);
        let local = origin - cb.absolute;
        let want = lumen_core::components::Edges {
            top: local.y,
            right: f32::NAN,
            bottom: f32::NAN,
            left: local.x,
            ..Default::default()
        };
        let current = style.inset;
        let unchanged = placed
            && edge_eq(current.top, want.top)
            && edge_eq(current.left, want.left)
            && edge_eq(current.right, want.right)
            && edge_eq(current.bottom, want.bottom);
        if unchanged {
            continue;
        }
        let live = Live {
            style: Some(&style),
            ..Live::default()
        };
        let out = overrides.hold::<PopupPanel>(
            StyleValues {
                inset: Some(want),
                ..StyleValues::default()
            },
            &live,
        );
        out.write_style(&mut style);
        if let Some(anim) = &anim {
            anim.request();
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
                local: None,
                pointer: Default::default(),
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
    /// system once and return the panel's resulting `inset.top`. No
    /// [`PopupGap`] on the header - the no-CSS-authored path.
    fn run_flip(header_y: f32) -> f32 {
        run_flip_with_gap(header_y, None)
    }

    /// The component-based counterpart of [`run_flip`]: same shape, but
    /// `gap` optionally inserts a [`PopupGap`] on the trigger header, to
    /// prove a CSS-supplied gap changes the panel's anchored position.
    fn run_flip_with_gap(header_y: f32, gap: Option<f32>) -> f32 {
        let mut world = World::new();
        world.insert_resource(Viewport {
            size: Vec2::new(500.0, 500.0),
            ..Viewport::default()
        });
        world.insert_resource(PropertyStore::default());
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__dropdown_open:fruit", true);

        let mut header = world.spawn((
            DropdownButton::new("__dropdown_open:fruit"),
            Transform::new(Vec2::new(10.0, header_y), Vec2::new(100.0, 30.0)),
        ));
        if let Some(gap) = gap {
            header.insert(PopupGap(gap));
        }
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

    /// No [`PopupGap`] on the trigger must keep today's 4 px gap - the
    /// explicit no-component counterpart of
    /// [`custom_popup_gap_moves_the_panel`].
    #[test]
    fn no_popup_gap_keeps_the_default_gap() {
        let top = run_flip_with_gap(10.0, None);
        assert!((top - 4.0).abs() < 0.5, "no PopupGap -> today's 4 px gap");
    }

    /// A CSS-supplied [`PopupGap`] on the trigger changes the panel's
    /// anchored offset.
    #[test]
    fn custom_popup_gap_moves_the_panel() {
        let top = run_flip_with_gap(10.0, Some(20.0));
        assert!(
            (top - 20.0).abs() < 0.5,
            "a CSS-supplied popup-gap moves the panel off the 4 px default (got {top})"
        );
    }

    #[test]
    fn dropdown_panel_flips_above_near_viewport_bottom() {
        // Header near the bottom: 460 + 30 + 4 + 80 = 574 > 500 -> flip.
        // Panel is anchored above, so its local top goes negative.
        let top = run_flip(460.0);
        assert!(top < 0.0, "panel flips above the trigger, got top={top}");
    }

    /// A menu opened at a point inside a containing block at (40, 20) in a
    /// 500 px window, with the panel measured at 100x80. Returns the world and
    /// the panel.
    fn menu_at_world(point: &str) -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(Viewport {
            size: Vec2::new(500.0, 500.0),
            ..Viewport::default()
        });
        world.insert_resource(PropertyStore::default());
        {
            let mut store = world.resource_mut::<PropertyStore>();
            store.set_global_bool("__menu_open:edit", true);
            store.set_global_str("__menu_at:edit", point);
        }
        let cb = world
            .spawn(Transform::new(Vec2::new(40.0, 20.0), Vec2::ZERO))
            .id();
        let panel = world
            .spawn((
                PopupPanel {
                    open_signal: "__menu_open:edit".to_string(),
                },
                Style::default(),
                Transform::new(Vec2::new(40.0, 20.0), Vec2::new(100.0, 80.0)),
                ChildOf(cb),
            ))
            .id();
        (world, panel)
    }

    fn inset(world: &World, panel: Entity) -> (f32, f32) {
        let inset = world.get::<Style>(panel).unwrap().inset;
        (inset.left, inset.top)
    }

    /// A menu opened at a point puts its top-left corner on the point,
    /// converted into its containing block's coordinates.
    #[test]
    fn menu_opened_at_a_point_sits_on_the_point() {
        let (mut world, panel) = menu_at_world("120 70");
        world.run_system_once(place_menus_at_points).unwrap();
        assert_eq!(inset(&world, panel), (80.0, 50.0));
        let style = world.get::<Style>(panel).unwrap();
        assert!(
            style.inset.right.is_nan() && style.inset.bottom.is_nan(),
            "the far edges stay auto so the panel keeps its own size"
        );
    }

    /// Near the window's right and bottom edges the menu opens to the left
    /// of and above the point instead, the way a native context menu does.
    #[test]
    fn menu_at_a_point_near_the_edges_opens_inward() {
        let (mut world, panel) = menu_at_world("480 470");
        world.run_system_once(place_menus_at_points).unwrap();
        // 480 - 100 = 380 and 470 - 80 = 390 in the window, less the
        // containing block's (40, 20).
        assert_eq!(inset(&world, panel), (340.0, 370.0));
    }

    /// Closing the menu, or reopening it without a point, gives the panel
    /// back the place its styles put it.
    #[test]
    fn closing_a_menu_opened_at_a_point_lets_go_of_the_placement() {
        let (mut world, panel) = menu_at_world("120 70");
        let resting = world.get::<Style>(panel).unwrap().inset;
        world.run_system_once(place_menus_at_points).unwrap();
        assert_ne!(world.get::<Style>(panel).unwrap().inset, resting);

        world
            .resource_mut::<PropertyStore>()
            .set_global_str("__menu_at:edit", "");
        world.run_system_once(place_menus_at_points).unwrap();
        assert_eq!(world.get::<Style>(panel).unwrap().inset, resting);

        world
            .resource_mut::<PropertyStore>()
            .set_global_str("__menu_at:edit", "120 70");
        world.run_system_once(place_menus_at_points).unwrap();
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__menu_open:edit", false);
        world.run_system_once(place_menus_at_points).unwrap();
        assert_eq!(world.get::<Style>(panel).unwrap().inset, resting);
    }

    /// A just-shown menu's containing block is laid out a tick later than the
    /// placement is first written, so a tick that moves the panel asks for
    /// another, and the next one places it against the fresh layout. A tick
    /// with nothing to move asks for none, so an open menu costs no frames.
    #[test]
    fn menu_placement_follows_the_next_layout_and_then_settles() {
        use lumen_core::render_world::AnimationsActive;
        let (mut world, panel) = menu_at_world("120 70");
        world.init_resource::<AnimationsActive>();
        world.run_system_once(place_menus_at_points).unwrap();
        assert!(
            world.resource::<AnimationsActive>().get(),
            "moved: tick again"
        );

        // Layout ran: the containing block now sits lower in the window.
        let cb = world.get::<ChildOf>(panel).unwrap().parent();
        world.get_mut::<Transform>(cb).unwrap().absolute = Vec2::new(0.0, 300.0);
        world.resource::<AnimationsActive>().clear();
        world.run_system_once(place_menus_at_points).unwrap();
        assert_eq!(inset(&world, panel), (120.0, -230.0));
        assert!(world.resource::<AnimationsActive>().get());

        world.resource::<AnimationsActive>().clear();
        world.run_system_once(place_menus_at_points).unwrap();
        assert!(
            !world.resource::<AnimationsActive>().get(),
            "settled: no more ticks"
        );
    }
}
