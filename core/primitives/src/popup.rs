//! Popovers: elements shown in the top layer while their open signal holds,
//! placed against an anchor and kept inside the window.
//!
//! A `popover` element, a `<dropdown>`'s list and a `<menu>` are all one
//! thing here, a [`Popover`]: the element carries [`TopLayer`](lumen_core::render_world::TopLayer), so it is laid
//! out against its window and no ancestor clips or scrolls it, and while it is
//! open [`place_popovers`] holds its position in [`StyleOverrides`]. Three
//! more systems act on open popovers:
//!
//! - [`dismiss_popups_on_outside_press`] closes an `auto` popover when a
//!   primary press lands outside both its subtree and whatever opened it.
//! - [`open_popovers_from_targets`] runs a click on a `popovertarget`
//!   element: it shows, hides or toggles the popover with that id and makes
//!   the element its anchor.
//! - [`report_popover_toggles`] raises [`PopoverToggled`] when a popover
//!   opens or closes, which the DOM layer delivers as `beforetoggle` and
//!   `toggle`.
//!
//! Where a popover goes ([`place_against`]): on the side of its anchor that
//! CSS `position-area` names (below and start-aligned by default), on the
//! other side when it does not fit there and `position-try-fallbacks`
//! allows the flip, clamped to the window's edges, and no taller than the
//! room on the side it took. A popover with no anchor is centred in the
//! window; a `<menu>` opened at a point (`open_menu_at`, a context menu) is
//! placed against that point.

use bevy_ecs::message::{Message, MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::components::{Length, LumenId, Position, Style};
use lumen_core::prelude::*;
use lumen_core::render_world::NotPaintRoot;
use lumen_core::signals::signal_is_truthy;
use lumen_core::style_overrides::{Live, StyleOverrides, StyleOwner, StyleValues};
use lumen_core::window::Viewports;

use crate::tabs::DropdownButton;

/// How a popover closes on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverMode {
    /// `popover="auto"`: a press outside it or Escape closes it.
    #[default]
    Auto,
    /// `popover="manual"`: it stays open until something closes it.
    Manual,
}

/// An element shown in the top layer while its open signal holds.
///
/// While it is open, [`place_popovers`] holds its `position`, `inset` (and,
/// when it has to, `max-height` and `width`) in [`StyleOverrides`]; on close
/// it lets go and the element is back to what its styles say.
#[derive(Component, Clone, Debug)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    require(StyleOverrides, lumen_core::render_world::TopLayer)
)]
// A page's browser lays out and paints its popovers: no top layer to keep.
#[cfg_attr(target_arch = "wasm32", require(StyleOverrides))]
pub struct Popover {
    /// The signal that holds while the popover is open.
    pub open_signal: String,
    /// How it closes on its own.
    pub mode: PopoverMode,
    /// `anchor="id"`: the element it is placed against.
    pub anchor: Option<String>,
    /// Whether it takes its anchor's width (a `<dropdown>`'s list).
    pub match_anchor_width: bool,
    /// The element that opened it last (a `popovertarget` button), its
    /// anchor when [`anchor`](Self::anchor) names none.
    pub invoker: Option<Entity>,
    /// The built panel of a `<dropdown>` or `<menu>`: focus moves into it
    /// when it opens and the arrow keys walk its rows (see
    /// [`crate::popup_nav`]).
    pub keyboard: bool,
}

impl Popover {
    /// An `auto` popover whose open state lives in `open_signal`.
    pub fn new(open_signal: impl Into<String>) -> Self {
        Self {
            open_signal: open_signal.into(),
            mode: PopoverMode::Auto,
            anchor: None,
            match_anchor_width: false,
            invoker: None,
            keyboard: false,
        }
    }
}

impl StyleOwner for Popover {
    const RANK: u8 = 20;
}

/// What a click on a [`PopoverTarget`] does to its popover.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverAction {
    /// Open it when closed, close it when open.
    #[default]
    Toggle,
    /// Open it.
    Show,
    /// Close it.
    Hide,
}

/// `popovertarget="id"`: a click on this element acts on the popover with
/// that id and makes this element its anchor.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct PopoverTarget {
    /// The popover's id.
    pub target: String,
    /// What a click does.
    pub action: PopoverAction,
}

/// The side of its anchor a popover opens on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AreaSide {
    /// Above.
    Top,
    /// Below.
    #[default]
    Bottom,
    /// To the left.
    Left,
    /// To the right.
    Right,
}

/// How a popover lines up with its anchor along the side it opens on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AreaAlign {
    /// Start edges together.
    #[default]
    Start,
    /// Centred.
    Center,
    /// End edges together.
    End,
}

/// Where a popover goes against its anchor: CSS `position-area` and
/// `position-try-fallbacks`, with `popup-gap` between the two.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct AnchorPlacement {
    /// The side of the anchor.
    pub side: AreaSide,
    /// The alignment along that side.
    pub align: AreaAlign,
    /// May move to the other side of the anchor on the block (vertical)
    /// axis when it does not fit.
    pub flip_block: bool,
    /// May move to the other side on the inline (horizontal) axis.
    pub flip_inline: bool,
}

impl Default for AnchorPlacement {
    /// Below the anchor, start edges together, both flips allowed: the way a
    /// dropdown's list opens.
    fn default() -> Self {
        Self {
            side: AreaSide::Bottom,
            align: AreaAlign::Start,
            flip_block: true,
            flip_inline: true,
        }
    }
}

/// A popover opened or closed.
///
/// Raised by [`report_popover_toggles`] the tick its open signal changes;
/// the DOM layer delivers it to the popover as `beforetoggle` and then
/// `toggle`.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopoverToggled {
    /// The popover.
    pub entity: Entity,
    /// Whether it is open now.
    pub open: bool,
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

/// Whether the popover's open signal holds, by the rule an `<if>` uses.
fn is_open(store: &PropertyStore, signal: &str) -> bool {
    store
        .get_global_str(signal)
        .as_deref()
        .is_some_and(signal_is_truthy)
}

/// Close any open `auto` [`Popover`] when a primary press lands outside both
/// its own subtree and what opened it: a `<dropdown>` header bound to the
/// same signal, a `popovertarget` element naming it, or the element that
/// last opened it.
///
/// Reads [`PointerPressed`] and the entity under the cursor ([`Hovered`],
/// maintained by `lumen_input::hit_test`). A press over empty space
/// dismisses every open `auto` popover. A press on the opener counts as
/// inside, so the opener's own toggle (which fires on release) stays the
/// one that closes it and the popover does not close and reopen.
pub fn dismiss_popups_on_outside_press(
    mut presses: MessageReader<PointerPressed>,
    hovered: Query<Entity, With<Hovered>>,
    popovers: Query<(Entity, &Popover, Option<&LumenId>)>,
    openers: (Query<&DropdownButton>, Query<&PopoverTarget>),
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
    for (popover_e, popover, id) in &popovers {
        if popover.mode != PopoverMode::Auto || !is_open(&store, &popover.open_signal) {
            continue;
        }
        let inside = target
            .is_some_and(|t| press_hits_popover(t, popover_e, popover, id, &openers, &parents));
        if !inside {
            store.set_global_bool(&popover.open_signal, false);
        }
    }
}

/// Walk `target`'s ancestor chain; the press counts as inside the popover
/// when it reaches the popover (its subtree) or what opens it.
fn press_hits_popover(
    target: Entity,
    popover_e: Entity,
    popover: &Popover,
    id: Option<&LumenId>,
    (headers, targets): &(Query<&DropdownButton>, Query<&PopoverTarget>),
    parents: &Query<&ChildOf>,
) -> bool {
    let mut cur = Some(target);
    while let Some(e) = cur {
        if e == popover_e || popover.invoker == Some(e) {
            return true;
        }
        if headers
            .get(e)
            .is_ok_and(|h| h.open_signal == popover.open_signal)
        {
            return true;
        }
        if let (Ok(t), Some(id)) = (targets.get(e), id)
            && t.target == id.0
        {
            return true;
        }
        cur = parents.get(e).ok().map(|c| c.parent());
    }
    false
}

/// Run this tick's clicks on `popovertarget` elements: show, hide or toggle
/// the popover with that id, and make the clicked element its anchor. The
/// click is the innermost element's, so the walk goes up to the nearest
/// element that names a popover, as a click on a button's icon clicks the
/// button.
pub fn open_popovers_from_targets(
    mut clicks: MessageReader<ClickEvent>,
    targets: Query<&PopoverTarget>,
    parents: Query<&ChildOf>,
    mut popovers: Query<(&mut Popover, &LumenId)>,
    mut store: ResMut<PropertyStore>,
) {
    for click in clicks.read() {
        let mut cur = Some(click.entity);
        let found = loop {
            let Some(e) = cur else { break None };
            if let Ok(t) = targets.get(e) {
                break Some((e, t));
            }
            cur = parents.get(e).ok().map(|c| c.parent());
        };
        let Some((invoker, target)) = found else {
            continue;
        };
        let Some((mut popover, _)) = popovers.iter_mut().find(|(_, id)| id.0 == target.target)
        else {
            lumen_core::warn_line!("popovertarget `{}`: no popover has that id", target.target);
            continue;
        };
        let open = is_open(&store, &popover.open_signal);
        let next = match target.action {
            PopoverAction::Toggle => !open,
            PopoverAction::Show => true,
            PopoverAction::Hide => false,
        };
        if next {
            popover.invoker = Some(invoker);
        }
        if next != open {
            store.set_global_bool(&popover.open_signal, next);
        }
    }
}

/// Raise a [`PopoverToggled`] for each popover whose open state changed
/// since the last tick it was looked at.
pub fn report_popover_toggles(
    store: Res<PropertyStore>,
    mut seen: Local<std::collections::HashMap<Entity, bool>>,
    popovers: Query<(Entity, &Popover)>,
    mut out: MessageWriter<PopoverToggled>,
) {
    seen.retain(|e, _| popovers.contains(*e));
    for (entity, popover) in &popovers {
        let open = is_open(&store, &popover.open_signal);
        let before = seen.insert(entity, open).unwrap_or(false);
        if before != open {
            out.write(PopoverToggled { entity, open });
        }
    }
}

/// Where a popover goes, from [`place_against`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Its top-left corner in window coordinates.
    pub origin: Vec2,
    /// The height it is capped at, when its content is taller than the
    /// room on the side it took.
    pub max_height: Option<f32>,
}

/// Place a `size` box against the anchor box at `anchor_pos` / `anchor_size`
/// in a `viewport`-sized window, `gap` away from it:
///
/// 1. on `placement.side`, unless the box does not fit there, the flip on
///    that axis is allowed, and the other side has more room;
/// 2. lined up with the anchor by `placement.align`, switched to the other
///    end when it would leave the window and the cross-axis flip is
///    allowed;
/// 3. no taller than the room on the side it took (top or bottom), and no
///    taller than the window beside the anchor (left or right);
/// 4. clamped inside the window.
///
/// A box already capped by a previous placement measures at its cap, so a
/// box that fills its room exactly keeps that cap rather than letting go and
/// growing back.
pub fn place_against(
    anchor_pos: Vec2,
    anchor_size: Vec2,
    size: Vec2,
    viewport: Vec2,
    gap: f32,
    placement: AnchorPlacement,
) -> Placement {
    let vertical = matches!(placement.side, AreaSide::Top | AreaSide::Bottom);
    // Work on (main, cross) axes: main is the axis the side is on.
    let (a_main, a_cross) = if vertical {
        ((anchor_pos.y, anchor_size.y), (anchor_pos.x, anchor_size.x))
    } else {
        ((anchor_pos.x, anchor_size.x), (anchor_pos.y, anchor_size.y))
    };
    let (s_main, s_cross) = if vertical {
        (size.y, size.x)
    } else {
        (size.x, size.y)
    };
    let (v_main, v_cross) = if vertical {
        (viewport.y, viewport.x)
    } else {
        (viewport.x, viewport.y)
    };
    let (flip_main, flip_cross) = if vertical {
        (placement.flip_block, placement.flip_inline)
    } else {
        (placement.flip_inline, placement.flip_block)
    };
    let after = matches!(placement.side, AreaSide::Bottom | AreaSide::Right);
    let room_after = (v_main - (a_main.0 + a_main.1 + gap)).max(0.0);
    let room_before = (a_main.0 - gap).max(0.0);
    let (room_pref, room_other) = if after {
        (room_after, room_before)
    } else {
        (room_before, room_after)
    };
    let take_after = if s_main <= room_pref || !flip_main || room_pref >= room_other {
        after
    } else {
        !after
    };
    let room = if take_after { room_after } else { room_before };
    // Capped on the main axis only when it is the height.
    let capped = vertical && s_main >= room - 0.5 && s_main > 0.0;
    let main_len = if capped { room } else { s_main };
    let main = if take_after {
        a_main.0 + a_main.1 + gap
    } else {
        a_main.0 - gap - main_len
    };
    let align_at = |align: AreaAlign| match align {
        AreaAlign::Start => a_cross.0,
        AreaAlign::Center => a_cross.0 + (a_cross.1 - s_cross) / 2.0,
        AreaAlign::End => a_cross.0 + a_cross.1 - s_cross,
    };
    let mut cross = align_at(placement.align);
    let leaves = |c: f32| c < 0.0 || c + s_cross > v_cross;
    if leaves(cross) && flip_cross {
        let other = match placement.align {
            AreaAlign::Start => AreaAlign::End,
            AreaAlign::End => AreaAlign::Start,
            AreaAlign::Center => AreaAlign::Center,
        };
        let flipped = align_at(other);
        if !leaves(flipped) {
            cross = flipped;
        }
    }
    let cross = cross.min(v_cross - s_cross).max(0.0);
    let main = main.min(v_main - main_len).max(0.0);
    // Beside the anchor, the height is the cross axis: no taller than the
    // window.
    let side_cap = (!vertical && s_cross >= v_cross - 0.5 && s_cross > 0.0).then_some(v_cross);
    let origin = if vertical {
        Vec2::new(cross, main)
    } else {
        Vec2::new(main, cross)
    };
    Placement {
        origin,
        max_height: if capped { Some(room) } else { side_cap },
    }
}

/// Prefix of the reserved signal a menu's open state lives in.
const MENU_OPEN_PREFIX: &str = "__menu_open:";

/// The reserved signal holding the window point a menu was opened at, as
/// `"x y"`, beside its `__menu_open:<id>` open state. `open_menu_at` writes
/// it; `open_menu` empties it.
fn menu_at_signal(menu_id: &str) -> String {
    format!("__menu_at:{menu_id}")
}

/// The point a menu was opened at, when it was opened at one.
fn menu_point(store: &PropertyStore, open_signal: &str) -> Option<Vec2> {
    let menu_id = open_signal.strip_prefix(MENU_OPEN_PREFIX)?;
    let raw = store.get_global_str(&menu_at_signal(menu_id))?;
    let mut parts = raw.split_whitespace().map(str::parse::<f32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(x)), Some(Ok(y))) if x.is_finite() && y.is_finite() => Some(Vec2::new(x, y)),
        _ => None,
    }
}

/// What [`place_popovers`] reads to find and measure an anchor.
#[derive(bevy_ecs::system::SystemParam)]
pub struct AnchorLookup<'w, 's> {
    transforms: Query<'w, 's, &'static Transform>,
    gaps: Query<'w, 's, &'static PopupGap>,
    headers: Query<'w, 's, (Entity, &'static DropdownButton)>,
    ids: Query<'w, 's, (Entity, &'static LumenId)>,
    /// Parents up to the nearest paint root, for the scroll offsets above an
    /// anchor.
    parents: Query<'w, 's, &'static ChildOf, NotPaintRoot>,
    tree: Query<'w, 's, &'static ChildOf>,
    scrolls: Query<'w, 's, &'static ScrollOffset>,
}

impl AnchorLookup<'_, '_> {
    /// Where `e` is drawn in its window: its layout box moved by the scroll
    /// offsets of its ancestors.
    fn painted(&self, e: Entity) -> Option<(Vec2, Vec2)> {
        let t = self.transforms.get(e).ok()?;
        let scroll = lumen_input::ancestor_scroll(e, &self.parents, &self.scrolls);
        Some((t.absolute - scroll, t.size))
    }

    /// The element with the id `id`.
    fn by_id(&self, id: &str) -> Option<Entity> {
        self.ids.iter().find(|(_, l)| l.0 == id).map(|(e, _)| e)
    }

    /// The element `popover` is placed against: its `anchor`, else the
    /// element that opened it, else the `<dropdown>` header or menu-bar
    /// button bound to its signal.
    fn anchor_of(&self, popover: &Popover) -> Option<Entity> {
        if let Some(id) = &popover.anchor {
            return self.by_id(id);
        }
        if let Some(e) = popover.invoker.filter(|e| self.transforms.contains(*e)) {
            return Some(e);
        }
        self.headers
            .iter()
            .find(|(_, h)| h.open_signal == popover.open_signal)
            .map(|(e, _)| e)
    }
}

/// Place every open [`Popover`] against its anchor (see [`place_against`])
/// and hold the placement in [`StyleOverrides`] while it stays open; let go
/// of it on close.
///
/// A popover is laid out against its window ([`TopLayer`](lumen_core::render_world::TopLayer)), so the held
/// `inset` is in window coordinates. Its anchor, in order: `anchor="id"`,
/// the element that opened it, the `<dropdown>` header or menu-bar button
/// bound to its signal. A `<menu>` opened at a point goes against that
/// point; a `<menu>` with none of these opens below where it is written; any
/// other popover with no anchor is centred in the window.
///
/// Where it lands depends on a layout that has not run yet: its own size,
/// and where its anchor sits once a just-shown popover takes part in layout.
/// So a tick that moves a popover asks for another, which places it against
/// the layout this one produces, and the placement settles in the tick that
/// finds nothing to move. An open popover follows its anchor when the anchor
/// scrolls or moves.
pub fn place_popovers(
    viewports: Viewports,
    anim: Option<Res<lumen_core::render_world::AnimationsActive>>,
    store: Res<PropertyStore>,
    anchors: AnchorLookup,
    mut popovers: Query<(
        Entity,
        &Popover,
        Option<&AnchorPlacement>,
        &mut Style,
        &mut StyleOverrides,
    )>,
) {
    for (popover_e, popover, placement, mut style, mut overrides) in &mut popovers {
        let held = overrides.held_by::<Popover>().is_some();
        if !is_open(&store, &popover.open_signal) {
            if held {
                overrides.release::<Popover>().write_style(&mut style);
            }
            continue;
        }
        let Some(vp) = viewports.of(popover_e).map(|v| v.size) else {
            continue;
        };
        let size = anchors
            .transforms
            .get(popover_e)
            .map(|t| t.size)
            .unwrap_or(Vec2::ZERO);
        let own_gap = anchors.gaps.get(popover_e).ok().map(|g| g.0);
        let mut placement = placement.copied().unwrap_or_default();
        // Against a point, a box, its own spot, or nothing.
        let anchor = anchors
            .anchor_of(popover)
            .and_then(|a| anchors.painted(a).map(|rect| (a, rect)));
        let (anchor_pos, anchor_size, gap) =
            if let Some(point) = menu_point(&store, &popover.open_signal) {
                // A context menu opens with its corner on the pointer.
                (point, Vec2::ZERO, 0.0)
            } else if let Some((a, (pos, size))) = anchor {
                let gap = own_gap
                    .or_else(|| anchors.gaps.get(a).ok().map(|g| g.0))
                    .unwrap_or(POPUP_GAP);
                (pos, size, gap)
            } else if popover.open_signal.starts_with(MENU_OPEN_PREFIX) {
                // A `<menu>` opened by id alone opens below where it is written.
                let Some((pos, _)) = anchors
                    .tree
                    .get(popover_e)
                    .ok()
                    .and_then(|p| anchors.painted(p.parent()))
                else {
                    continue;
                };
                (pos, Vec2::ZERO, 0.0)
            } else {
                // Centred, as a browser shows a popover with no anchor.
                placement.align = AreaAlign::Center;
                let centre = vp / 2.0;
                (centre - Vec2::new(0.0, size.y / 2.0), Vec2::ZERO, 0.0)
            };
        let place = place_against(anchor_pos, anchor_size, size, vp, gap, placement);
        let want = StyleValues {
            position: Some(Position::Absolute),
            inset: Some(lumen_core::components::Edges {
                top: place.origin.y,
                left: place.origin.x,
                ..lumen_core::components::Edges::auto()
            }),
            max_height: place.max_height.map(Length::Px),
            width: popover
                .match_anchor_width
                .then_some(Length::Px(anchor_size.x)),
            ..StyleValues::default()
        };
        let current = overrides.held_by::<Popover>();
        if held && current.is_some_and(|c| same_placement(c, &want)) {
            continue;
        }
        // A cap or a width it no longer needs goes back to the styles.
        if held {
            overrides.release::<Popover>().write_style(&mut style);
        }
        let live = Live {
            style: Some(&style),
            ..Live::default()
        };
        let out = overrides.hold::<Popover>(want, &live);
        out.write_style(&mut style);
        if let Some(anim) = &anim {
            anim.request();
        }
    }
}

/// Whether two held placements put a popover in the same place, treating
/// `NaN` (`auto`) as equal to itself and sub-pixel moves as no move.
fn same_placement(a: &StyleValues, b: &StyleValues) -> bool {
    let edges = |x: Option<lumen_core::components::Edges>,
                 y: Option<lumen_core::components::Edges>| match (x, y) {
        (Some(x), Some(y)) => {
            edge_eq(x.top, y.top)
                && edge_eq(x.left, y.left)
                && edge_eq(x.right, y.right)
                && edge_eq(x.bottom, y.bottom)
        }
        (None, None) => true,
        _ => false,
    };
    let len = |x: Option<Length>, y: Option<Length>| match (x, y) {
        (Some(Length::Px(x)), Some(Length::Px(y))) => edge_eq(x, y),
        (x, y) => x == y,
    };
    a.position == b.position
        && edges(a.inset, b.inset)
        && len(a.max_height, b.max_height)
        && len(a.width, b.width)
}

/// Inset comparison that treats `NaN` (`auto`) as equal to itself so the
/// placement doesn't rewrite (and needlessly re-lay-out) an unchanged
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
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;
    use bevy_ecs::world::World;

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

    fn place(anchor: (f32, f32, f32, f32), size: (f32, f32), p: AnchorPlacement) -> Placement {
        place_against(
            Vec2::new(anchor.0, anchor.1),
            Vec2::new(anchor.2, anchor.3),
            Vec2::new(size.0, size.1),
            Vec2::new(500.0, 500.0),
            4.0,
            p,
        )
    }

    #[test]
    fn a_popover_opens_below_its_anchor_with_the_start_edges_together() {
        let p = place(
            (10.0, 10.0, 100.0, 30.0),
            (150.0, 80.0),
            AnchorPlacement::default(),
        );
        assert_eq!(p.origin, Vec2::new(10.0, 44.0));
        assert_eq!(p.max_height, None);
    }

    #[test]
    fn near_the_bottom_it_flips_above() {
        let p = place(
            (10.0, 460.0, 100.0, 30.0),
            (100.0, 80.0),
            AnchorPlacement::default(),
        );
        assert_eq!(p.origin.y, 460.0 - 4.0 - 80.0);
        assert_eq!(p.max_height, None);
    }

    #[test]
    fn without_the_flip_it_stays_below_and_shrinks_to_the_room() {
        let no_flip = AnchorPlacement {
            flip_block: false,
            ..AnchorPlacement::default()
        };
        let p = place((10.0, 400.0, 100.0, 30.0), (100.0, 200.0), no_flip);
        assert_eq!(p.origin.y, 434.0);
        assert_eq!(p.max_height, Some(500.0 - 434.0));
    }

    #[test]
    fn too_tall_for_either_side_it_takes_the_roomier_one_and_caps_its_height() {
        // 150 px of room below, 296 above: a 400 px list goes above, 296 tall.
        let p = place(
            (10.0, 300.0, 100.0, 46.0),
            (100.0, 400.0),
            AnchorPlacement::default(),
        );
        assert_eq!(p.max_height, Some(296.0));
        assert_eq!(p.origin.y, 0.0);
        // Measured at its cap the next tick, it stays where it is.
        let again = place(
            (10.0, 300.0, 100.0, 46.0),
            (100.0, 296.0),
            AnchorPlacement::default(),
        );
        assert_eq!(again, p);
    }

    #[test]
    fn near_the_right_edge_it_lines_up_end_edges_then_clamps() {
        let p = place(
            (450.0, 10.0, 40.0, 20.0),
            (200.0, 50.0),
            AnchorPlacement::default(),
        );
        // End edges together: 450 + 40 - 200.
        assert_eq!(p.origin.x, 290.0);
        // Wider than the window allows on either alignment: clamped.
        let wide = place(
            (10.0, 10.0, 40.0, 20.0),
            (600.0, 50.0),
            AnchorPlacement::default(),
        );
        assert_eq!(wide.origin.x, 0.0);
    }

    #[test]
    fn centred_and_beside_the_anchor() {
        let centre = AnchorPlacement {
            align: AreaAlign::Center,
            ..AnchorPlacement::default()
        };
        assert_eq!(
            place((100.0, 10.0, 100.0, 20.0), (60.0, 30.0), centre)
                .origin
                .x,
            120.0
        );
        let right = AnchorPlacement {
            side: AreaSide::Right,
            ..AnchorPlacement::default()
        };
        assert_eq!(
            place((100.0, 10.0, 100.0, 20.0), (60.0, 30.0), right).origin,
            Vec2::new(204.0, 10.0)
        );
    }

    #[test]
    fn edge_eq_treats_nan_as_equal() {
        assert!(edge_eq(f32::NAN, f32::NAN));
        assert!(!edge_eq(f32::NAN, 4.0));
        assert!(edge_eq(4.0, 4.2));
        assert!(!edge_eq(4.0, 40.0));
    }

    fn open_panel_world() -> (World, Entity) {
        let mut world = World::new();
        world.init_resource::<Messages<PointerPressed>>();
        world.insert_resource(PropertyStore::default());
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__dropdown_open:fruit", true);
        let panel = world.spawn(Popover::new("__dropdown_open:fruit")).id();
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

    fn is_fruit_open(world: &World) -> bool {
        world
            .resource::<PropertyStore>()
            .get_global_bool("__dropdown_open:fruit")
            == Some(true)
    }

    #[test]
    fn outside_press_closes_panel() {
        let (mut world, _panel) = open_panel_world();
        world.spawn(Hovered);
        press_primary(&mut world);
        world
            .run_system_once(dismiss_popups_on_outside_press)
            .unwrap();
        assert!(!is_fruit_open(&world), "press outside the panel closes it");
    }

    #[test]
    fn press_inside_panel_keeps_it_open() {
        let (mut world, panel) = open_panel_world();
        world.entity_mut(panel).insert(Hovered);
        press_primary(&mut world);
        world
            .run_system_once(dismiss_popups_on_outside_press)
            .unwrap();
        assert!(
            is_fruit_open(&world),
            "press inside the panel leaves it open"
        );
    }

    #[test]
    fn press_on_trigger_keeps_it_open() {
        let (mut world, _panel) = open_panel_world();
        world.spawn((DropdownButton::new("__dropdown_open:fruit"), Hovered));
        press_primary(&mut world);
        world
            .run_system_once(dismiss_popups_on_outside_press)
            .unwrap();
        assert!(is_fruit_open(&world), "press on the trigger leaves it open");
    }

    #[test]
    fn a_manual_popover_ignores_an_outside_press() {
        let (mut world, panel) = open_panel_world();
        world.get_mut::<Popover>(panel).unwrap().mode = PopoverMode::Manual;
        world.spawn(Hovered);
        press_primary(&mut world);
        world
            .run_system_once(dismiss_popups_on_outside_press)
            .unwrap();
        assert!(is_fruit_open(&world));
    }

    /// A 500 px window with a dropdown header at `header_y` and its 80 px
    /// panel open; returns the world and the panel.
    fn dropdown_world(header_y: f32, gap: Option<f32>) -> (World, Entity) {
        let mut world = World::new();
        lumen_core::window::spawn_primary_window(
            &mut world,
            lumen_core::window::WindowViewport {
                size: Vec2::new(500.0, 500.0),
                ..lumen_core::window::WindowViewport::default()
            },
        );
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
        let mut popover = Popover::new("__dropdown_open:fruit");
        popover.match_anchor_width = true;
        let panel = world
            .spawn((
                popover,
                Style::default(),
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 80.0)),
            ))
            .id();
        (world, panel)
    }

    fn inset(world: &World, panel: Entity) -> (f32, f32) {
        let inset = world.get::<Style>(panel).unwrap().inset;
        (inset.left, inset.top)
    }

    #[test]
    fn a_dropdown_list_opens_below_its_header_at_its_width() {
        let (mut world, panel) = dropdown_world(10.0, None);
        world.run_system_once(place_popovers).unwrap();
        assert_eq!(inset(&world, panel), (10.0, 44.0));
        let style = world.get::<Style>(panel).unwrap();
        assert_eq!(style.position, Position::Absolute);
        assert_eq!(style.width, Length::Px(100.0));
    }

    #[test]
    fn a_css_popup_gap_moves_the_list() {
        let (mut world, panel) = dropdown_world(10.0, Some(20.0));
        world.run_system_once(place_popovers).unwrap();
        assert_eq!(inset(&world, panel), (10.0, 60.0));
    }

    #[test]
    fn a_dropdown_list_flips_above_near_the_bottom() {
        let (mut world, panel) = dropdown_world(460.0, None);
        world.run_system_once(place_popovers).unwrap();
        assert_eq!(inset(&world, panel), (10.0, 376.0));
    }

    #[test]
    fn closing_lets_go_of_the_placement() {
        let (mut world, panel) = dropdown_world(10.0, None);
        let resting = world.get::<Style>(panel).unwrap().clone();
        world.run_system_once(place_popovers).unwrap();
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__dropdown_open:fruit", false);
        world.run_system_once(place_popovers).unwrap();
        assert_eq!(*world.get::<Style>(panel).unwrap(), resting);
    }

    /// A menu opened at a point in a 500 px window, measured at 100x80.
    fn menu_at_world(point: &str) -> (World, Entity) {
        let mut world = World::new();
        lumen_core::window::spawn_primary_window(
            &mut world,
            lumen_core::window::WindowViewport {
                size: Vec2::new(500.0, 500.0),
                ..lumen_core::window::WindowViewport::default()
            },
        );
        world.insert_resource(PropertyStore::default());
        {
            let mut store = world.resource_mut::<PropertyStore>();
            store.set_global_bool("__menu_open:edit", true);
            store.set_global_str("__menu_at:edit", point);
        }
        let panel = world
            .spawn((
                Popover::new("__menu_open:edit"),
                Style::default(),
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 80.0)),
            ))
            .id();
        (world, panel)
    }

    #[test]
    fn a_menu_opened_at_a_point_sits_on_the_point() {
        let (mut world, panel) = menu_at_world("120 70");
        world.run_system_once(place_popovers).unwrap();
        assert_eq!(inset(&world, panel), (120.0, 70.0));
        let style = world.get::<Style>(panel).unwrap();
        assert!(style.inset.right.is_nan() && style.inset.bottom.is_nan());
    }

    #[test]
    fn a_menu_at_a_point_near_the_edges_opens_inward() {
        let (mut world, panel) = menu_at_world("480 470");
        world.run_system_once(place_popovers).unwrap();
        assert_eq!(inset(&world, panel), (380.0, 390.0));
    }

    #[test]
    fn placement_asks_for_another_tick_until_it_settles() {
        use lumen_core::render_world::AnimationsActive;
        let (mut world, _panel) = menu_at_world("120 70");
        world.init_resource::<AnimationsActive>();
        world.run_system_once(place_popovers).unwrap();
        assert!(
            world.resource::<AnimationsActive>().get(),
            "moved: tick again"
        );
        world.resource::<AnimationsActive>().clear();
        world.run_system_once(place_popovers).unwrap();
        assert!(
            !world.resource::<AnimationsActive>().get(),
            "settled: no more ticks"
        );
    }

    #[test]
    fn a_popover_with_no_anchor_is_centred() {
        let mut world = World::new();
        lumen_core::window::spawn_primary_window(
            &mut world,
            lumen_core::window::WindowViewport {
                size: Vec2::new(500.0, 500.0),
                ..lumen_core::window::WindowViewport::default()
            },
        );
        world.insert_resource(PropertyStore::default());
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__popover_open:tip", true);
        let panel = world
            .spawn((
                Popover::new("__popover_open:tip"),
                Style::default(),
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 80.0)),
            ))
            .id();
        world.run_system_once(place_popovers).unwrap();
        assert_eq!(inset(&world, panel), (200.0, 210.0));
    }

    #[test]
    fn a_popovertarget_click_toggles_its_popover_and_anchors_it() {
        let mut world = World::new();
        world.init_resource::<Messages<ClickEvent>>();
        world.insert_resource(PropertyStore::default());
        let popover = world
            .spawn((Popover::new("__popover_open:pick"), LumenId("pick".into())))
            .id();
        let button = world
            .spawn(PopoverTarget {
                target: "pick".into(),
                action: PopoverAction::Toggle,
            })
            .id();
        let icon = world.spawn(ChildOf(button)).id();
        let click = |world: &mut World, on: Entity| {
            world.write_message(ClickEvent {
                entity: on,
                position: Vec2::ZERO,
                button: PointerButton::Primary,
                local: None,
            });
            world.run_system_once(open_popovers_from_targets).unwrap();
            world.resource_mut::<Messages<ClickEvent>>().clear();
        };
        click(&mut world, icon);
        let open = |world: &World| {
            world
                .resource::<PropertyStore>()
                .get_global_bool("__popover_open:pick")
                == Some(true)
        };
        assert!(open(&world));
        assert_eq!(world.get::<Popover>(popover).unwrap().invoker, Some(button));
        click(&mut world, button);
        assert!(!open(&world));
    }
}
