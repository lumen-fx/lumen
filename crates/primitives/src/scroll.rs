//! Scroll primitive.
//!
//! - Tag a container with [`Scroll`] to declare its axes.
//! - [`accumulate_wheel`] reads [`MouseWheel`] messages and updates [`ScrollOffset`] on the directly-hovered scrollable entity.
//! - The default extract fns consume [`ScrollOffset`] via [`lumen_core::render_world::parent_scroll_offsets`].
//! - Clipping (`overflow: hidden`) is handled in `lumen-layout-taffy`.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::prelude::*;

/// Plugin: registers [`accumulate_wheel`] and [`integrate_scroll`]. The
/// scroll-aware extract path is supplied by lumen-core's default
/// extracts - they query [`ScrollOffset`] directly and translate
/// descendants accordingly.
pub struct ScrollPlugin;

impl Plugin for ScrollPlugin {
    fn build(self, app: &mut App) {
        // W6 T3: every `ScrollOffset` mutator is ordered strictly BEFORE
        // `hit_test`. The two conflict on `ScrollOffset` (writer vs
        // reader), so without an explicit edge the executor serialized
        // them in an arbitrary, per-tick-flippable order - when the last
        // offset mutation of a fling landed AFTER that tick's hit-test,
        // the `Hovered` marker kept reflecting pre-scroll positions under
        // a stationary cursor until some unrelated tick re-ran it (the
        // "stale hover after simulated scroll" bug). With the edge,
        // content moving under the cursor re-resolves hover on the same
        // tick, matching Qt's behaviour (scroll under a stationary
        // cursor re-evaluates the hovered widget).
        //
        // Wheel ROUTING (`accumulate_wheel` walks up from the hovered
        // entity) now deterministically uses the previous tick's hover -
        // the pointer position it was resolved from is unchanged unless
        // a `PointerMoved` arrived this very tick, which the winit
        // backend delivers on its own tick anyway.
        app.add_systems(
            TickStage::Systems,
            accumulate_wheel.before(lumen_input::hit_test),
        );
        app.add_systems(
            TickStage::Systems,
            integrate_scroll
                .after(accumulate_wheel)
                .before(lumen_input::hit_test),
        );
        // Keyboard scrolling for the focused scrollable (or its nearest
        // scrollable ancestor). Same `.before(hit_test)` edge as the
        // wheel path - it mutates `ScrollOffset` too (W6 T3).
        app.add_systems(
            TickStage::Systems,
            scroll_on_keys.before(lumen_input::hit_test),
        );
        // Overlay scrollbars (spec section 16.2/section 16.6): pointer FSM + fade
        // driver, ordered strictly before the hit-test so the
        // `ScrollbarInteraction` resource it consults reflects THIS
        // tick's pointer position (bars sit above content).
        app.add_systems(
            TickStage::Systems,
            crate::scrollbar::update_scrollbars.before(lumen_input::hit_test),
        );
        // W5.3: drain [`A11yScrollIntoViewRequests`] (written by
        // `handle_a11y_action` in `lumen-window-winit`) so screen-reader
        // initiated `Action::ScrollIntoView` calls actually translate to
        // a scroll. Runs in `LayoutSync` so the read of `Transform.size`
        // sees freshly resolved layout; the offset write lands before
        // `clamp_scroll_offsets` in `A11ySync` clips it to the valid
        // range.
        app.add_systems(TickStage::LayoutSync, apply_a11y_scroll_into_view);
        // Clamp runs after layout so child Transforms reflect this
        // tick's content extent. Without this the user can scroll
        // past either edge into empty space.
        app.add_systems(TickStage::A11ySync, clamp_scroll_offsets);
    }
}

/// Upper bound on per-tick integration step. After a suspend / hot
/// reload / debugger pause, `Tick.dt` can balloon into seconds; without
/// a cap the inertial fling launches the offset off the page in a
/// single tick. 100 ms matches the wall-clock budget most browsers use
/// for their `requestAnimationFrame` rate limiters after backgrounding.
pub const MAX_INTEGRATION_DT_MS: u32 = 100;

/// Velocity below which the integrator snaps to zero, in px/s. At
/// 60 Hz a velocity of 3 px/s produces ~0.05 px of motion per frame -
/// imperceptible, but heavy enough to avoid eternal sub-pixel ticks.
pub const VELOCITY_SLEEP_PX_PER_S: f32 = 3.0;

/// Slack (px) when deciding whether a scroller is already pinned at a
/// limit for wheel-routing purposes. Sub-pixel residue from inertia /
/// rubber-band settling must not make an at-limit scroller "consume"
/// the event and stall the bubble to its ancestor.
const WHEEL_LIMIT_EPSILON: f32 = 0.5;

/// Accumulate wheel deltas into the [`ScrollOffset`] of the innermost
/// scrollable ancestor of whatever entity the pointer is hovering
/// **that can still scroll in the event's direction** (spec section 16.5).
///
/// Why bubbling at all: `hit_test` picks the *deepest* candidate under
/// the cursor (so a tile beats its column container), but the `Scroll`
/// component lives on the container, not on the leaf - so we walk the
/// [`ChildOf`] chain upward. Nested-scroll routing (Qt propagation
/// model): the innermost hovered scroll area handles the wheel first;
/// when it is already at its limit on the event's direction (or the
/// event's axes don't intersect its allowed [`ScrollAxis`]), it does
/// **not** consume the event and the next scrollable ancestor gets it.
/// An inner list pinned at its bottom + wheel-down therefore scrolls
/// the outer page, while wheel-up still scrolls the inner list.
///
/// If no scroll ancestor on the chain can consume the event (every
/// scroller is pinned at its limit for this direction), the event is
/// routed to the innermost scroller anyway so overshoot / rubber-band
/// handling still observes the gesture - `clamp_scroll_offsets` keeps
/// the offset in bounds, so this is a visual no-op on hard-clamp
/// platforms. The cursor being over chrome with no `Scroll` ancestor
/// at all keeps the old fallback: the world's first `Scroll` entity,
/// so wheel still does something sensible in single-scroller apps.
///
/// Drains the [`MouseWheel`] buffer into a local vec first to avoid losing
/// events during archetype churn (the previous `single_mut` version did).
pub fn accumulate_wheel(
    mut wheels: MessageReader<MouseWheel>,
    hovered: Query<Entity, With<Hovered>>,
    parents: Query<&ChildOf>,
    children_q: Query<(&ChildOf, &Transform)>,
    transforms: Query<&Transform>,
    sliders: Query<(), With<SliderValue>>,
    mut scrolls: Query<(Entity, &mut Scroll, &mut ScrollOffset)>,
) {
    let pending: Vec<MouseWheel> = wheels.read().copied().collect();
    if pending.is_empty() {
        return;
    }
    // A hovered `<slider>` (or its thumb child) consumes the wheel -
    // `lumen_primitives::controls::adjust_slider_on_wheel` turns it into
    // a value step - so it must never ALSO scroll an ancestor scroll
    // container (Qt: an accepted wheel event does not propagate).
    {
        let mut cur = hovered.iter().next();
        while let Some(e) = cur {
            if sliders.contains(e) {
                return;
            }
            cur = parents.get(e).ok().map(|c| c.parent());
        }
    }
    let total: Vec2 = pending.iter().map(|ev| ev.delta).sum();

    // Max scroll offset per axis for one container: bbox of its direct
    // children relative to its own rect - the same content-extent rule
    // `clamp_scroll_offsets` applies.
    let max_offset_of = |container: Entity| -> Vec2 {
        let Ok(self_tf) = transforms.get(container) else {
            // No layout yet - treat as freely scrollable so the first
            // tick doesn't drop events (matches pre-routing behavior).
            return Vec2::splat(f32::INFINITY);
        };
        let mut max_x = 0.0_f32;
        let mut max_y = 0.0_f32;
        for (child_of, kid) in &children_q {
            if child_of.parent() != container {
                continue;
            }
            max_x = max_x.max((kid.absolute.x - self_tf.absolute.x) + kid.size.x);
            max_y = max_y.max((kid.absolute.y - self_tf.absolute.y) + kid.size.y);
        }
        Vec2::new(
            (max_x - self_tf.size.x).max(0.0),
            (max_y - self_tf.size.y).max(0.0),
        )
    };

    // section 16.5 consumption test: `offset -= delta`, so a positive delta
    // moves toward 0 (needs offset > 0) and a negative delta moves
    // toward max (needs offset < max) - per axis, masked to the
    // container's allowed axes.
    let can_consume = |scroll: &Scroll, offset: Vec2, max_off: Vec2| -> bool {
        let masked = mask_delta(total, scroll.axis);
        let axis_ok = |d: f32, off: f32, max: f32| -> bool {
            if d > 0.0 {
                off > WHEEL_LIMIT_EPSILON
            } else if d < 0.0 {
                off < max - WHEEL_LIMIT_EPSILON
            } else {
                false
            }
        };
        axis_ok(masked.x, offset.x, max_off.x) || axis_ok(masked.y, offset.y, max_off.y)
    };

    // Walk the hover chain innermost-first, collecting scrollable
    // ancestors; pick the first that can consume the event.
    let mut chain: Vec<Entity> = Vec::new();
    let mut cur = hovered.iter().next();
    while let Some(e) = cur {
        if scrolls.contains(e) {
            chain.push(e);
        }
        cur = parents.get(e).ok().map(|c| c.parent());
    }
    let target = if chain.is_empty() {
        // Cursor over chrome with no scrollable ancestor: legacy
        // fallback to the world's first scroller.
        scrolls.iter().next().map(|(e, ..)| e)
    } else {
        chain
            .iter()
            .copied()
            .find(|&e| {
                scrolls
                    .get(e)
                    .map(|(_, s, off)| can_consume(s, off.0, max_offset_of(e)))
                    .unwrap_or(false)
            })
            // Every scroller in the chain is at its limit: route to the
            // innermost anyway so rubber-band / overshoot handling (and
            // sensitivity-scaled velocity) still observe the gesture -
            // `clamp_scroll_offsets` keeps it in bounds.
            .or_else(|| chain.first().copied())
    };
    let Some(target) = target else {
        return;
    };

    // Apply.
    if let Ok((_entity, mut scroll, mut offset)) = scrolls.get_mut(target) {
        let mut delta = Vec2::ZERO;
        for ev in &pending {
            delta += mask_delta(ev.delta, scroll.axis);
        }
        delta *= scroll.sensitivity;
        let inertia = scroll.inertia.clamp(0.0, 1.0);
        // Immediate portion: 1 - inertia. Velocity portion: inertia.
        offset.0 -= delta * (1.0 - inertia);
        // Velocity is stored in px/s. A wheel detent normally takes
        // ~16 ms (one 60 Hz tick) for the OS to fire, so the px/frame
        // equivalent multiplies by ~60 to land in px/s - preserves the
        // pre-delta-time-fix glide distance at 60 Hz while letting the
        // integrator scale correctly across refresh rates.
        const WHEEL_VELOCITY_HZ: f32 = 60.0;
        scroll.velocity += -delta * inertia * WHEEL_VELOCITY_HZ;
    }
}

/// Mask a wheel delta to a container's allowed [`ScrollAxis`].
fn mask_delta(delta: Vec2, axis: ScrollAxis) -> Vec2 {
    match axis {
        ScrollAxis::X => Vec2::new(delta.x, 0.0),
        ScrollAxis::Y => Vec2::new(0.0, delta.y),
        ScrollAxis::Both => delta,
    }
}

/// Keyboard scrolling for the focused entity, or - when the focused
/// entity isn't itself scrollable - its nearest [`Scroll`]-bearing
/// ancestor. Walks the [`ChildOf`] chain the same way
/// [`apply_a11y_scroll_into_view`] does for AT-driven `ScrollIntoView`.
///
/// - `ArrowUp` / `ArrowDown` / `ArrowLeft` / `ArrowRight`: one line
///   ([`crate::physics::LINE_HEIGHT_PX`]) along the arrow's direction,
///   masked to the container's allowed [`ScrollAxis`] (a horizontal-only
///   scroller ignores Up/Down and vice versa).
/// - `PageUp` / `PageDown` (forwarded by the winit backend as
///   `Key::Character("PageUp"/"PageDown")` - no dedicated `NamedKey`
///   variant exists yet): one viewport height.
/// - `Home` / `End`: jump to the top / bottom of the content.
///
/// Every target value - including `Home`/`End` - is computed against
/// the container's own content-extent bbox (the same bounding-box scan
/// [`clamp_scroll_offsets`] does) and clamped inline, so this system is
/// self-contained: it doesn't depend on `clamp_scroll_offsets` running
/// afterward to land in bounds, and (unlike a naive "seek to
/// `f32::MAX`") `End` lands exactly at the max offset even under
/// macOS's rubber-band overshoot curve rather than asymptotically
/// approaching it.
///
/// A focused [`TextInput`] owns arrows/Home/End for caret movement
/// (`type_into_focused` in lumen-input runs on the same raw key bus);
/// this system no-ops whenever the *focused* entity is itself a text
/// input, so a `<textarea>` sitting inside a scroll container doesn't
/// have its arrow keys double-handled - moving the caret AND scrolling
/// the container out from under it.
///
/// The same double-handling guard covers the widgets that own their
/// arrow / Home / End keys while focused (Qt: a focused control that
/// accepts a key consumes it - the view never also scrolls):
/// dropdown headers ([`crate::tabs::DropdownButton`] - closed-state
/// value stepping), popup rows ([`crate::tabs::DropdownOptionButton`]
/// / [`crate::tabs::MenuItemButton`] - highlight movement), and tab
/// strip buttons ([`crate::tabs::TabStripButton`] - tab switching).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn scroll_on_keys(
    mut keys: MessageReader<KeyPressed>,
    tracker: Res<FocusTracker>,
    parents: Query<&ChildOf>,
    text_inputs: Query<(), With<TextInput>>,
    key_owning_widgets: Query<
        (),
        Or<(
            With<crate::tabs::DropdownButton>,
            With<crate::tabs::DropdownOptionButton>,
            With<crate::tabs::MenuItemButton>,
            With<crate::tabs::TabStripButton>,
            // A freshly-opened menu parks focus on the panel itself
            // until the first arrow press picks a row.
            With<crate::popup::PopupPanel>,
        )>,
    >,
    children: Query<(&ChildOf, &Transform)>,
    mut scrolls: Query<(&Transform, &mut Scroll, &mut ScrollOffset)>,
) {
    let Some(focused) = tracker.0 else {
        keys.read().for_each(drop);
        return;
    };
    if text_inputs.contains(focused) || key_owning_widgets.contains(focused) {
        keys.read().for_each(drop);
        return;
    }
    // Nearest Scroll-bearing entity at/above `focused`.
    let mut current = Some(focused);
    let mut target: Option<Entity> = None;
    while let Some(e) = current {
        if scrolls.contains(e) {
            target = Some(e);
            break;
        }
        current = parents.get(e).ok().map(|c| c.parent());
    }
    let Some(target) = target else {
        keys.read().for_each(drop);
        return;
    };
    let Ok((tf, mut scroll, mut offset)) = scrolls.get_mut(target) else {
        keys.read().for_each(drop);
        return;
    };
    // Content extent, same bbox-of-direct-children approach
    // `clamp_scroll_offsets` uses (that one groups by parent across all
    // scrollers per tick; here we only need one target so a direct scan
    // is simpler and just as cheap).
    let mut max_x = 0.0f32;
    let mut max_y = 0.0f32;
    for (child_of, kid) in &children {
        if child_of.parent() != target {
            continue;
        }
        max_x = max_x.max((kid.absolute.x - tf.absolute.x) + kid.size.x);
        max_y = max_y.max((kid.absolute.y - tf.absolute.y) + kid.size.y);
    }
    let max_off = Vec2::new((max_x - tf.size.x).max(0.0), (max_y - tf.size.y).max(0.0));
    let touches_x = scroll.axis.allows_x();
    let touches_y = scroll.axis.allows_y();

    for ev in keys.read() {
        let new = match &ev.key {
            Key::Named(NamedKey::ArrowUp) => {
                if !touches_y {
                    continue;
                }
                offset.0 + Vec2::new(0.0, -crate::physics::LINE_HEIGHT_PX)
            }
            Key::Named(NamedKey::ArrowDown) => {
                if !touches_y {
                    continue;
                }
                offset.0 + Vec2::new(0.0, crate::physics::LINE_HEIGHT_PX)
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if !touches_x {
                    continue;
                }
                offset.0 + Vec2::new(-crate::physics::LINE_HEIGHT_PX, 0.0)
            }
            Key::Named(NamedKey::ArrowRight) => {
                if !touches_x {
                    continue;
                }
                offset.0 + Vec2::new(crate::physics::LINE_HEIGHT_PX, 0.0)
            }
            Key::Character(s) if s.as_str() == "PageUp" => {
                if !touches_y {
                    continue;
                }
                offset.0 + Vec2::new(0.0, -tf.size.y)
            }
            Key::Character(s) if s.as_str() == "PageDown" => {
                if !touches_y {
                    continue;
                }
                offset.0 + Vec2::new(0.0, tf.size.y)
            }
            Key::Named(NamedKey::Home) => Vec2::new(
                if touches_x { 0.0 } else { offset.0.x },
                if touches_y { 0.0 } else { offset.0.y },
            ),
            Key::Named(NamedKey::End) => Vec2::new(
                if touches_x { max_off.x } else { offset.0.x },
                if touches_y { max_off.y } else { offset.0.y },
            ),
            _ => continue,
        }
        .clamp(Vec2::ZERO, max_off);

        if new != offset.0 {
            if (new.x - offset.0.x).abs() > f32::EPSILON {
                scroll.velocity.x = 0.0;
            }
            if (new.y - offset.0.y).abs() > f32::EPSILON {
                scroll.velocity.y = 0.0;
            }
            offset.0 = new;
        }
    }
}

/// Clamp every [`ScrollOffset`] to `[0, content - viewport]` per axis,
/// with macOS-style rubber-band overshoot driven by
/// [`crate::physics::RUBBER_BAND_STIFFNESS`]. Runs after layout so child
/// `Transform`s reflect the current content extent.
///
/// Content extent is computed as the bounding box of the scroll
/// container's direct children - virtualized `<for>` blocks set their
/// own size to `rows x row_h`, so the bbox correctly reflects the
/// scrollable region.
///
/// Rubber-band model: when stiffness > 0, an offset past the bound is
/// pulled back via `bound + overflow / (1 + stiffness * |overflow|)`
/// (a Hooke spring approximation). When stiffness is 0 (Windows /
/// Linux defaults), the offset hard-clamps and velocity is zeroed on
/// the clamped axis so inertial flings don't keep pumping into the
/// wall - matching the pre-rubber-band behaviour.
pub fn clamp_scroll_offsets(
    children_q: Query<(&ChildOf, &Transform)>,
    mut scrolls: Query<(Entity, &Transform, &mut Scroll, &mut ScrollOffset)>,
) {
    use std::collections::HashMap;
    // Build child-by-parent lookup once.
    let mut by_parent: HashMap<Entity, Vec<&Transform>> = HashMap::new();
    for (parent, tf) in &children_q {
        by_parent.entry(parent.parent()).or_default().push(tf);
    }
    let stiffness = crate::physics::RUBBER_BAND_STIFFNESS;
    for (entity, self_tf, mut scroll, mut offset) in &mut scrolls {
        let Some(kids) = by_parent.get(&entity) else {
            // No children = no content; nothing to scroll.
            if offset.0 != Vec2::ZERO {
                offset.0 = Vec2::ZERO;
            }
            scroll.velocity = Vec2::ZERO;
            continue;
        };
        // Children's `absolute` is layout-space (pre-scroll), so the
        // local extent under the scroll container is the bbox of
        // child positions relative to `self_tf.absolute`.
        let mut max_x = 0.0_f32;
        let mut max_y = 0.0_f32;
        for kid in kids {
            let rx = (kid.absolute.x - self_tf.absolute.x) + kid.size.x;
            let ry = (kid.absolute.y - self_tf.absolute.y) + kid.size.y;
            if rx > max_x {
                max_x = rx;
            }
            if ry > max_y {
                max_y = ry;
            }
        }
        let max_off_x = (max_x - self_tf.size.x).max(0.0);
        let max_off_y = (max_y - self_tf.size.y).max(0.0);

        let new = Vec2::new(
            apply_bound(offset.0.x, 0.0, max_off_x, stiffness),
            apply_bound(offset.0.y, 0.0, max_off_y, stiffness),
        );
        if new != offset.0 {
            // Hit a bound - drop velocity along the clamped axis so
            // inertial flings don't keep pumping into the wall. With
            // stiffness > 0 (rubber-band), we still zero the velocity
            // beyond the bound: the next tick will pull `offset` back
            // toward the bound via `apply_bound` even with no inertial
            // assist, mirroring the AppKit spring-snap feel.
            if (new.x - offset.0.x).abs() > f32::EPSILON {
                scroll.velocity.x = 0.0;
            }
            if (new.y - offset.0.y).abs() > f32::EPSILON {
                scroll.velocity.y = 0.0;
            }
            offset.0 = new;
        }
    }
}

/// Apply a soft (rubber-band) or hard clamp to a single axis. With
/// `stiffness == 0` the function degenerates to `value.clamp(lo, hi)`.
/// With `stiffness > 0`, overflow past a bound is pulled back via the
/// Hooke approximation `bound + overflow / (1 + stiffness * |overflow|)`;
/// this is the same shape Cocoa's `NSScrollView` uses for its
/// rubber-band overshoot. The pullback is asymptotic: a 100 px
/// overshoot at stiffness 0.55 lands ~98% of the way back to the bound
/// on the same tick, the residual decays the next.
fn apply_bound(value: f32, lo: f32, hi: f32, stiffness: f32) -> f32 {
    if stiffness <= 0.0 {
        return value.clamp(lo, hi);
    }
    if value < lo {
        let overflow = lo - value; // positive
        return lo - overflow / (1.0 + stiffness * overflow);
    }
    if value > hi {
        let overflow = value - hi; // positive
        return hi + overflow / (1.0 + stiffness * overflow);
    }
    value
}

/// W5.3 consumer for inbound `Action::ScrollIntoView` AccessKit actions.
///
/// - Drains [`lumen_core::components::A11yScrollIntoViewRequests`] each
///   tick. For every requested entity, walks the [`ChildOf`] chain up
///   to the nearest ancestor that carries [`Scroll`] + [`ScrollOffset`]
///   and updates the offset so the target's bounding box sits inside
///   the container's viewport rect.
/// - Vertical-axis containers move the offset along Y, horizontal along
///   X, `Both` along both axes simultaneously.
/// - When the target is already fully visible, the offset is left
///   untouched (idempotent - repeated requests don't drift).
/// - Velocity on the target axis is zeroed so a previously-fired
///   inertial fling does not immediately scroll back away from the
///   focused element.
/// - The actual clamp to `[0, content - viewport]` is left to
///   [`clamp_scroll_offsets`] which runs in `TickStage::A11ySync` after
///   this system.
pub fn apply_a11y_scroll_into_view(
    mut requests: ResMut<lumen_core::components::A11yScrollIntoViewRequests>,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
    mut scrolls: Query<(&Transform, &mut Scroll, &mut ScrollOffset)>,
) {
    if requests.targets.is_empty() {
        return;
    }
    for target in requests.targets.drain(..) {
        // Find the nearest scroll-bearing ancestor.
        let mut current = Some(target);
        let mut container: Option<Entity> = None;
        while let Some(e) = current {
            if scrolls.contains(e) {
                container = Some(e);
                break;
            }
            current = parents.get(e).ok().map(|c| c.parent());
        }
        let Some(container_entity) = container else {
            continue;
        };
        let Ok(target_tf) = transforms.get(target) else {
            continue;
        };
        let target_min = target_tf.absolute;
        let target_max = target_tf.absolute + target_tf.size;
        let Ok((container_tf, mut scroll, mut offset)) = scrolls.get_mut(container_entity) else {
            continue;
        };
        let viewport_min = container_tf.absolute;
        let viewport_max = container_tf.absolute + container_tf.size;

        // Per-axis delta: positive shifts the visible area down/right
        // (i.e. the offset *increases*), revealing content further into
        // the scrollable region.
        let mut delta = Vec2::ZERO;
        let touches_x = scroll.axis.allows_x();
        let touches_y = scroll.axis.allows_y();
        if touches_x {
            if target_min.x < viewport_min.x {
                delta.x = target_min.x - viewport_min.x;
            } else if target_max.x > viewport_max.x {
                delta.x = target_max.x - viewport_max.x;
            }
        }
        if touches_y {
            if target_min.y < viewport_min.y {
                delta.y = target_min.y - viewport_min.y;
            } else if target_max.y > viewport_max.y {
                delta.y = target_max.y - viewport_max.y;
            }
        }
        if delta == Vec2::ZERO {
            continue;
        }
        offset.0 += delta;
        if delta.x != 0.0 {
            scroll.velocity.x = 0.0;
        }
        if delta.y != 0.0 {
            scroll.velocity.y = 0.0;
        }
    }
}

/// Per-tick: apply [`Scroll::velocity`] (px/s) to [`ScrollOffset`] and
/// decay velocity per the OS-specific [`crate::physics::INERTIA_DECAY`]
/// constant. Reads wall-clock `dt` from the shared [`Tick`] resource so
/// every scroller integrates against the same frame clock, clamped to
/// [`MAX_INTEGRATION_DT_MS`] so a post-suspend frame doesn't fling the
/// offset off the page.
///
/// Stops integrating when |velocity| falls below
/// [`VELOCITY_SLEEP_PX_PER_S`] to avoid eternal tiny redraws.
pub fn integrate_scroll(
    tick: Res<Tick>,
    anim: Res<lumen_core::render_world::AnimationsActive>,
    mut q: Query<(&mut ScrollOffset, &mut Scroll)>,
) {
    let max_dt = std::time::Duration::from_millis(u64::from(MAX_INTEGRATION_DT_MS));
    let dt = tick.dt.min(max_dt).as_secs_f32();
    for (mut offset, mut scroll) in &mut q {
        if scroll.velocity.length_squared() < VELOCITY_SLEEP_PX_PER_S * VELOCITY_SLEEP_PX_PER_S {
            scroll.velocity = Vec2::ZERO;
            continue;
        }
        if dt <= 0.0 {
            continue;
        }
        // Inertial glide still moving - keep the loop awake so momentum
        // scrolling coasts to a stop without needing further input events.
        anim.request();
        offset.0 += scroll.velocity * dt;
        // Exponential decay: v(t+dt) = v(t) * exp(-k * dt). Framerate-
        // independent - at 60 Hz with k = 8.0 this matches the legacy
        // glide envelope; at 120 Hz the per-tick decay halves and the
        // glide duration stays constant.
        let decay = (-crate::physics::INERTIA_DECAY * dt).exp();
        scroll.velocity *= decay;
    }
}

#[cfg(test)]
mod a11y_scroll_into_view_tests {
    //! W5.3: `Action::ScrollIntoView` actually moves the right
    //! ancestor's [`ScrollOffset`] toward the target entity. ECS-side
    //! roundtrip executed via `World::run_system_once` so we observe
    //! only the consumer system, not the full schedule.
    use super::*;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::components::A11yScrollIntoViewRequests;

    #[test]
    fn scrolls_target_into_visible_range_vertical() {
        let mut world = World::new();
        world.init_resource::<A11yScrollIntoViewRequests>();

        let container = world
            .spawn((
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(200.0, 100.0),
                    baseline_y: None,
                },
                Scroll::vertical(),
                ScrollOffset(Vec2::ZERO),
            ))
            .id();
        // Target is below the container's viewport.
        let target = world
            .spawn((
                Transform {
                    absolute: Vec2::new(0.0, 150.0),
                    size: Vec2::new(50.0, 30.0),
                    baseline_y: None,
                },
                ChildOf(container),
            ))
            .id();
        world
            .resource_mut::<A11yScrollIntoViewRequests>()
            .targets
            .push(target);

        world.run_system_once(apply_a11y_scroll_into_view).unwrap();

        // target_max.y = 180, viewport_max.y = 100 -> +80 px delta.
        let off = world.get::<ScrollOffset>(container).unwrap();
        assert!(
            (off.0.y - 80.0).abs() < f32::EPSILON,
            "expected +80px scroll, got {:?}",
            off.0,
        );
        assert!(
            world
                .resource::<A11yScrollIntoViewRequests>()
                .targets
                .is_empty(),
            "consumer must drain the request queue",
        );
    }

    #[test]
    fn idempotent_when_target_already_visible() {
        let mut world = World::new();
        world.init_resource::<A11yScrollIntoViewRequests>();
        let container = world
            .spawn((
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(200.0, 100.0),
                    baseline_y: None,
                },
                Scroll::vertical(),
                ScrollOffset(Vec2::new(7.0, 13.0)),
            ))
            .id();
        let target = world
            .spawn((
                Transform {
                    absolute: Vec2::new(20.0, 20.0),
                    size: Vec2::new(30.0, 30.0),
                    baseline_y: None,
                },
                ChildOf(container),
            ))
            .id();
        world
            .resource_mut::<A11yScrollIntoViewRequests>()
            .targets
            .push(target);

        world.run_system_once(apply_a11y_scroll_into_view).unwrap();

        let off = world.get::<ScrollOffset>(container).unwrap();
        assert_eq!(off.0, Vec2::new(7.0, 13.0), "offset must stay untouched");
    }

    #[test]
    fn walks_child_of_chain_to_nearest_scroll_ancestor() {
        let mut world = World::new();
        world.init_resource::<A11yScrollIntoViewRequests>();
        let outer = world
            .spawn((
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(400.0, 400.0),
                    baseline_y: None,
                },
                Scroll::vertical(),
                ScrollOffset(Vec2::ZERO),
            ))
            .id();
        let middle = world
            .spawn((
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(300.0, 300.0),
                    baseline_y: None,
                },
                ChildOf(outer),
            ))
            .id();
        let target = world
            .spawn((
                Transform {
                    absolute: Vec2::new(0.0, 500.0),
                    size: Vec2::new(20.0, 20.0),
                    baseline_y: None,
                },
                ChildOf(middle),
            ))
            .id();
        world
            .resource_mut::<A11yScrollIntoViewRequests>()
            .targets
            .push(target);
        world.run_system_once(apply_a11y_scroll_into_view).unwrap();
        let off = world.get::<ScrollOffset>(outer).unwrap();
        // target_max.y = 520, viewport_max.y = 400 -> +120 px.
        assert!((off.0.y - 120.0).abs() < f32::EPSILON);
    }
}

#[cfg(test)]
mod wheel_routing_tests {
    //! Spec section 16.5 - nested scroll areas: the innermost hovered
    //! scrollable handles the wheel first; when it can't scroll further
    //! in the event's direction it doesn't consume, and the ancestor
    //! scroll area scrolls instead.
    use super::*;
    use bevy_ecs::system::RunSystemOnce;

    /// Outer 400x400 scroller (content 400x2000) containing an inner
    /// 300x200 scroller (content 300x600) whose leaf tile carries
    /// `Hovered`. Returns `(world, outer, inner)`.
    fn nested_scrollers(inner_offset_y: f32) -> (World, Entity, Entity) {
        let mut world = World::new();
        world.init_resource::<Messages<MouseWheel>>();
        let outer = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(400.0, 400.0)),
                // inertia 0 -> whole delta applies to the offset at once,
                // so assertions can compare offsets directly.
                Scroll::vertical().with_inertia(0.0),
                ScrollOffset(Vec2::ZERO),
            ))
            .id();
        let outer_content = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(400.0, 2000.0)),
                ChildOf(outer),
            ))
            .id();
        let inner = world
            .spawn((
                Transform::new(Vec2::new(0.0, 50.0), Vec2::new(300.0, 200.0)),
                Scroll::vertical().with_inertia(0.0),
                ScrollOffset(Vec2::new(0.0, inner_offset_y)),
                ChildOf(outer_content),
            ))
            .id();
        // Inner content: 600 tall -> inner max offset = 400.
        world.spawn((
            Transform::new(Vec2::new(0.0, 50.0), Vec2::new(300.0, 600.0)),
            ChildOf(inner),
        ));
        // Hovered leaf inside the inner scroller.
        world.spawn((
            Transform::new(Vec2::new(0.0, 50.0), Vec2::new(300.0, 20.0)),
            ChildOf(inner),
            Hovered,
        ));
        (world, outer, inner)
    }

    fn wheel(world: &mut World, dy: f32) {
        world
            .resource_mut::<Messages<MouseWheel>>()
            .write(MouseWheel {
                delta: Vec2::new(0.0, dy),
                position: Vec2::new(10.0, 60.0),
            });
    }

    fn offset_y(world: &World, e: Entity) -> f32 {
        world.get::<ScrollOffset>(e).unwrap().0.y
    }

    #[test]
    fn inner_scrolls_while_it_can() {
        let (mut world, outer, inner) = nested_scrollers(0.0);
        // Wheel-down (negative delta -> offset increases).
        wheel(&mut world, -30.0);
        world.run_system_once(accumulate_wheel).unwrap();
        assert_eq!(offset_y(&world, inner), 30.0, "inner consumes the wheel");
        assert_eq!(offset_y(&world, outer), 0.0, "outer untouched");
    }

    #[test]
    fn wheel_down_at_inner_bottom_bubbles_to_outer() {
        // Inner pinned at its max offset (600 content - 200 viewport).
        let (mut world, outer, inner) = nested_scrollers(400.0);
        wheel(&mut world, -30.0);
        world.run_system_once(accumulate_wheel).unwrap();
        assert_eq!(
            offset_y(&world, inner),
            400.0,
            "inner at limit must not consume"
        );
        assert_eq!(offset_y(&world, outer), 30.0, "outer scrolls instead");
    }

    #[test]
    fn wheel_up_at_inner_bottom_still_scrolls_inner() {
        let (mut world, outer, inner) = nested_scrollers(400.0);
        // Wheel-up (positive delta -> offset decreases): inner CAN
        // consume this direction, so it must not bubble.
        wheel(&mut world, 30.0);
        world.run_system_once(accumulate_wheel).unwrap();
        assert_eq!(offset_y(&world, inner), 370.0, "inner scrolls back up");
        assert_eq!(offset_y(&world, outer), 0.0, "outer untouched");
    }

    #[test]
    fn wheel_up_at_top_of_both_goes_to_inner_and_clamps_later() {
        // Both at 0 and wheel-up: nobody can consume; the innermost
        // gets the (overshooting) delta and clamp handles it later.
        let (mut world, outer, inner) = nested_scrollers(0.0);
        wheel(&mut world, 30.0);
        world.run_system_once(accumulate_wheel).unwrap();
        assert_eq!(offset_y(&world, outer), 0.0, "outer untouched");
        assert_eq!(
            offset_y(&world, inner),
            -30.0,
            "innermost receives the unconsumable delta (clamped next stage)"
        );
    }

    #[test]
    fn no_scroll_ancestor_falls_back_to_first_world_scroller() {
        let mut world = World::new();
        world.init_resource::<Messages<MouseWheel>>();
        let scroller = world
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(400.0, 400.0)),
                Scroll::vertical().with_inertia(0.0),
                ScrollOffset(Vec2::ZERO),
            ))
            .id();
        world.spawn((
            Transform::new(Vec2::ZERO, Vec2::new(400.0, 1000.0)),
            ChildOf(scroller),
        ));
        // Hovered chrome entity entirely outside the scroller's tree.
        world.spawn((Transform::new(Vec2::ZERO, Vec2::new(50.0, 50.0)), Hovered));
        wheel(&mut world, -20.0);
        world.run_system_once(accumulate_wheel).unwrap();
        assert_eq!(offset_y(&world, scroller), 20.0);
    }
}

#[cfg(test)]
mod scroll_key_tests {
    //! `scroll_on_keys` - arrow / PageUp / PageDown / Home / End on the
    //! focused scrollable, or its nearest scrollable ancestor. Driven
    //! directly via `run_system_once` against a bare `World`.
    use super::*;
    use bevy_ecs::system::RunSystemOnce;

    /// A 400x400 vertical scroller containing one 400x1000 child (so
    /// `max_off.y = 1000 - 400 = 600`).
    fn setup_vertical_scroller() -> (World, Entity) {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        let container = world
            .spawn((
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(400.0, 400.0),
                    baseline_y: None,
                },
                Scroll::vertical(),
                ScrollOffset(Vec2::ZERO),
            ))
            .id();
        world.spawn((
            Transform {
                absolute: Vec2::ZERO,
                size: Vec2::new(400.0, 1000.0),
                baseline_y: None,
            },
            ChildOf(container),
        ));
        world.insert_resource(FocusTracker(Some(container)));
        (world, container)
    }

    fn press(world: &mut World, key: Key) {
        world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key,
                modifiers: Modifiers::default(),
                repeat: false,
            });
    }

    /// `run_system_once` builds a fresh system (and thus a fresh
    /// `MessageReader` cursor) on every call, so a message written
    /// before one call is still visible to the *next* call's brand-new
    /// reader unless the buffer is cleared in between. Tests that press
    /// more than one key in sequence must clear after each
    /// `run_system_once` to avoid re-processing stale key presses.
    fn run_and_clear(world: &mut World) {
        world.run_system_once(scroll_on_keys).unwrap();
        world.resource_mut::<Messages<KeyPressed>>().clear();
    }

    #[test]
    fn arrow_down_scrolls_by_one_line() {
        let (mut world, container) = setup_vertical_scroller();
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_and_clear(&mut world);
        let off = world.get::<ScrollOffset>(container).unwrap();
        assert_eq!(off.0.y, crate::physics::LINE_HEIGHT_PX);
    }

    #[test]
    fn arrow_left_right_are_ignored_on_a_vertical_only_scroller() {
        let (mut world, container) = setup_vertical_scroller();
        press(&mut world, Key::Named(NamedKey::ArrowRight));
        run_and_clear(&mut world);
        let off = world.get::<ScrollOffset>(container).unwrap();
        assert_eq!(
            off.0,
            Vec2::ZERO,
            "horizontal arrows no-op on a Y-only scroller"
        );
    }

    #[test]
    fn page_down_clamps_at_max_offset() {
        let (mut world, container) = setup_vertical_scroller();
        // Viewport height 400, content 1000 -> max_off.y = 600. Two
        // PageDowns (400 px each) would overshoot to 800 without a clamp.
        press(&mut world, Key::Named(NamedKey::ArrowDown)); // no-op filler to exercise the loop
        run_and_clear(&mut world);
        press(&mut world, Key::Character("PageDown".into()));
        press(&mut world, Key::Character("PageDown".into()));
        run_and_clear(&mut world);
        let off = world.get::<ScrollOffset>(container).unwrap();
        assert_eq!(
            off.0.y, 600.0,
            "PageDown must clamp at content max, not overshoot"
        );
    }

    #[test]
    fn end_jumps_to_exact_max_offset() {
        let (mut world, container) = setup_vertical_scroller();
        press(&mut world, Key::Named(NamedKey::End));
        run_and_clear(&mut world);
        let off = world.get::<ScrollOffset>(container).unwrap();
        assert_eq!(off.0.y, 600.0);
    }

    #[test]
    fn home_returns_to_zero() {
        let (mut world, container) = setup_vertical_scroller();
        press(&mut world, Key::Named(NamedKey::End));
        run_and_clear(&mut world);
        press(&mut world, Key::Named(NamedKey::Home));
        run_and_clear(&mut world);
        let off = world.get::<ScrollOffset>(container).unwrap();
        assert_eq!(off.0.y, 0.0);
    }

    #[test]
    fn nearest_scrollable_ancestor_of_focused_child_receives_scroll() {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        let container = world
            .spawn((
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(400.0, 400.0),
                    baseline_y: None,
                },
                Scroll::vertical(),
                ScrollOffset(Vec2::ZERO),
            ))
            .id();
        let row = world
            .spawn((
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(400.0, 1000.0),
                    baseline_y: None,
                },
                ChildOf(container),
            ))
            .id();
        // Focus lands on a plain child of the scroll container (e.g. a
        // focusable row), not the scroller itself.
        world.insert_resource(FocusTracker(Some(row)));
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_and_clear(&mut world);
        let off = world.get::<ScrollOffset>(container).unwrap();
        assert_eq!(off.0.y, crate::physics::LINE_HEIGHT_PX);
    }

    #[test]
    fn focused_text_input_is_not_double_handled() {
        let (mut world, container) = setup_vertical_scroller();
        // Re-point focus at a TextInput entity that also happens to be
        // a child of the scroll container.
        let input = world
            .spawn((
                ChildOf(container),
                TextInput {
                    placeholder: String::new(),
                    cursor: 0,
                    selection_anchor: None,
                    multiline: false,
                },
            ))
            .id();
        world.insert_resource(FocusTracker(Some(input)));
        press(&mut world, Key::Named(NamedKey::ArrowDown));
        run_and_clear(&mut world);
        let off = world.get::<ScrollOffset>(container).unwrap();
        assert_eq!(
            off.0,
            Vec2::ZERO,
            "a focused TextInput's arrows must not also scroll its scroll-container ancestor"
        );
    }
}
