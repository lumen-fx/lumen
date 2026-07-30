//! Overlay-scrollbar interaction FSM + fade driver (spec section 16.2 / section 16.6).
//!
//! Painting lives in `lumen_core::render_world::extract_scrollbars`;
//! this module owns everything main-world:
//!
//! - auto-attaching [`ScrollbarState`] to every [`Scroll`] container,
//! - the pointer FSM (thumb hover highlight, thumb press + drag with
//!   absolute 1:1 mapping and pointer capture, track click =
//!   jump-to-position - consistent with the `<slider>` track-click
//!   decision in [`crate::controls`]),
//! - the overlay fade: bars stay fully visible while scrolling /
//!   hovered, then fade out after `ScrollbarStyle::fade_delay_secs` of
//!   inactivity. The fade frames self-schedule through
//!   [`lumen_core::render_world::AnimationsActive`] and go quiescent the
//!   moment alpha reaches its resting value.
//!
//! Wheel events are NEVER consumed here - hovering a bar resolves the
//! hit-test to the scroll container itself (see `lumen_input::hit_test`),
//! so the wheel keeps routing through the normal nested-scroll chain.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::prelude::*;

/// Pointer FSM + fade driver for overlay scrollbars. Runs in
/// `TickStage::Systems` strictly before `lumen_input::hit_test` so the
/// [`ScrollbarInteraction`] resource the hit-test consults reflects this
/// tick's pointer position.
///
/// All visual + timing knobs (thickness, minimum thumb, fade delay /
/// ramp) resolve through [`ScrollbarStyle`] - CSS `scrollbar-color` /
/// `scrollbar-width` per container, with the component [`Default`] as
/// the no-stylesheet fallback - so the FSM's hit regions always match
/// the styled paint.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_scrollbars(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<AnimationsActive>,
    pointer: Res<PointerState>,
    mut presses: MessageReader<PointerPressed>,
    mut releases: MessageReader<PointerReleased>,
    mut keys: MessageReader<KeyPressed>,
    mut interaction: ResMut<ScrollbarInteraction>,
    mut frame_dirty: ResMut<FrameDirty>,
    parents: Query<&ChildOf>,
    children_q: Query<(&ChildOf, &Transform)>,
    bare: Query<Entity, (With<Scroll>, Without<ScrollbarState>)>,
    styles: Query<&ScrollbarStyle>,
    mut scrolls: Query<(
        Entity,
        &Transform,
        &mut Scroll,
        &mut ScrollOffset,
        &mut ScrollbarState,
    )>,
) {
    // Auto-attach fade state to freshly-spawned scroll containers.
    for e in &bare {
        commands.entity(e).insert(ScrollbarState::default());
    }

    let dt = tick.dt.as_secs_f32().min(0.1);

    // Scroll offsets snapshot for ancestor-chain translation (a nested
    // scroller's viewport box moves with its ANCESTORS' offsets). Taken
    // up front because `scrolls` is borrowed mutably below.
    let offsets: std::collections::HashMap<Entity, Vec2> =
        scrolls.iter().map(|(e, _, _, off, _)| (e, off.0)).collect();

    // Content extents per scroll container (bbox of direct children -
    // the same rule `clamp_scroll_offsets` applies).
    let mut extents: std::collections::HashMap<Entity, Vec2> = std::collections::HashMap::new();
    for (child_of, kid) in &children_q {
        let parent = child_of.parent();
        if !scrolls.contains(parent) {
            continue;
        }
        let e = extents.entry(parent).or_insert(Vec2::ZERO);
        // Child extents are made relative below (needs the parent tf);
        // store absolute max corner for now.
        e.x = e.x.max(kid.absolute.x + kid.size.x);
        e.y = e.y.max(kid.absolute.y + kid.size.y);
    }

    // Resolve both bars' geometry for one container, honouring its
    // CSS-resolved [`ScrollbarStyle`] metrics.
    let geometry = |entity: Entity,
                    tf: &Transform,
                    scroll: &Scroll,
                    offset: Vec2|
     -> (Option<ScrollbarGeometry>, Option<ScrollbarGeometry>) {
        let style = styles.get(entity).copied().unwrap_or_default();
        // `scrollbar-width: none` - no bars, no interaction.
        let Some(metrics) = style.metrics() else {
            return (None, None);
        };
        let Some(max_corner) = extents.get(&entity) else {
            return (None, None);
        };
        let content_w = max_corner.x - tf.absolute.x;
        let content_h = max_corner.y - tf.absolute.y;
        let allow_y = scroll.axis.allows_y();
        let allow_x = scroll.axis.allows_x();
        let v_overflow = allow_y && content_h - tf.size.y > 0.5;
        let h_overflow = allow_x && content_w - tf.size.x > 0.5;
        // The viewport box translates with ANCESTOR scrollers only.
        let mut anc_off = Vec2::ZERO;
        let mut cur = entity;
        while let Ok(p) = parents.get(cur) {
            let parent = p.parent();
            if let Some(o) = offsets.get(&parent) {
                anc_off += *o;
            }
            cur = parent;
        }
        let origin = tf.absolute - anc_off;
        let v = if v_overflow {
            vertical_scrollbar(origin, tf.size, content_h, offset.y, h_overflow, metrics)
        } else {
            None
        };
        let h = if h_overflow {
            horizontal_scrollbar(origin, tf.size, content_w, offset.x, v_overflow, metrics)
        } else {
            None
        };
        (v, h)
    };

    let pressed = presses
        .read()
        .any(|p| matches!(p.button, PointerButton::Primary));
    let released = releases
        .read()
        .any(|r| matches!(r.button, PointerButton::Primary));
    let escape = keys
        .read()
        .any(|k| matches!(k.key, Key::Named(NamedKey::Escape)));

    // -- Escape cancels an in-flight thumb drag (Qt drag-cancel):
    // restore the pre-drag scroll offset and end the drag without a
    // release commit. -------------------------------------------------
    if escape && let Some(drag) = interaction.drag.take() {
        if let Ok((_, _, mut scroll, mut offset, _)) = scrolls.get_mut(drag.entity) {
            offset.0 = drag.start_offset;
            scroll.velocity = Vec2::ZERO;
        }
        frame_dirty.dirty = true;
    }

    // -- Drag continuation / termination (pointer capture) ------------
    if let Some(drag) = interaction.drag {
        let end = released || !pointer.primary_down;
        if end {
            interaction.drag = None;
            frame_dirty.dirty = true;
        } else if let Some(pos) = pointer.position
            && let Ok((entity, tf, mut scroll, mut offset, _state)) = scrolls.get_mut(drag.entity)
        {
            let (v, h) = geometry(entity, tf, &scroll, offset.0);
            let (geo, vertical) = match drag.axis {
                ScrollbarAxisPick::Vertical => (v, true),
                ScrollbarAxisPick::Horizontal => (h, false),
            };
            if let Some(geo) = geo {
                let along = if vertical { pos.y } else { pos.x };
                let new = geo.offset_for_thumb_pos(along, drag.grab, vertical);
                if vertical {
                    if (offset.0.y - new).abs() > f32::EPSILON {
                        offset.0.y = new;
                        scroll.velocity.y = 0.0;
                        frame_dirty.dirty = true;
                    }
                } else if (offset.0.x - new).abs() > f32::EPSILON {
                    offset.0.x = new;
                    scroll.velocity.x = 0.0;
                    frame_dirty.dirty = true;
                }
            }
        }
    }

    // -- Hover pick (visible bars only) -------------------------------
    let prev_hover = interaction.hover;
    let mut new_hover: Option<(Entity, ScrollbarAxisPick, ScrollbarPart)> = None;
    if interaction.drag.is_none()
        && let Some(pos) = pointer.position
    {
        for (entity, tf, scroll, offset, state) in scrolls.iter() {
            // Invisible (fully faded) bars are not interactive - an
            // invisible strip must not swallow clicks meant for rows.
            if state.alpha <= 0.05 {
                continue;
            }
            let (v, h) = geometry(entity, tf, scroll, offset.0);
            let pick = |geo: Option<ScrollbarGeometry>,
                        axis: ScrollbarAxisPick|
             -> Option<(Entity, ScrollbarAxisPick, ScrollbarPart)> {
                let geo = geo?;
                if geo.point_in_thumb(pos) {
                    Some((entity, axis, ScrollbarPart::Thumb))
                } else if geo.point_in_track(pos) {
                    Some((entity, axis, ScrollbarPart::Track))
                } else {
                    None
                }
            };
            if let Some(hit) = pick(v, ScrollbarAxisPick::Vertical)
                .or_else(|| pick(h, ScrollbarAxisPick::Horizontal))
            {
                // Deepest container wins when bars overlap (nested
                // scrollers): prefer the one with more ancestors.
                let depth = |mut e: Entity| -> u32 {
                    let mut d = 0;
                    while let Ok(p) = parents.get(e) {
                        d += 1;
                        e = p.parent();
                    }
                    d
                };
                new_hover = match new_hover {
                    Some(prev) if depth(prev.0) >= depth(hit.0) => Some(prev),
                    _ => Some(hit),
                };
            }
        }
    }
    if interaction.drag.is_none() {
        interaction.hover = new_hover;
        if prev_hover.map(|(e, a, _)| (e, a)) != new_hover.map(|(e, a, _)| (e, a)) {
            // Hover highlight / track visibility changed - repaint.
            frame_dirty.dirty = true;
        }
    }

    // -- Press: thumb grab or track jump-to-position ------------------
    if pressed
        && interaction.drag.is_none()
        && let Some((entity, axis, part)) = interaction.hover
        && let Some(pos) = pointer.position
        && let Ok((e, tf, mut scroll, mut offset, _state)) = scrolls.get_mut(entity)
    {
        let (v, h) = geometry(e, tf, &scroll, offset.0);
        let (geo, vertical) = match axis {
            ScrollbarAxisPick::Vertical => (v, true),
            ScrollbarAxisPick::Horizontal => (h, false),
        };
        if let Some(geo) = geo {
            // Snapshot for Escape-cancel BEFORE the press applies its
            // jump: cancelling restores the pre-press position.
            let start_offset = offset.0;
            let along = if vertical { pos.y } else { pos.x };
            let thumb_start = if vertical {
                geo.thumb_origin.y
            } else {
                geo.thumb_origin.x
            };
            let thumb_len = if vertical {
                geo.thumb_size.y
            } else {
                geo.thumb_size.x
            };
            let grab = match part {
                // Grab keeps the pointer glued to the same spot on the
                // thumb for the whole drag.
                ScrollbarPart::Thumb => along - thumb_start,
                // Track click: jump so the thumb centers under the
                // pointer (the slider track-click decision), then keep
                // dragging from the thumb's center.
                ScrollbarPart::Track => thumb_len / 2.0,
            };
            let new = geo.offset_for_thumb_pos(along, grab, vertical);
            if vertical {
                offset.0.y = new;
                scroll.velocity.y = 0.0;
            } else {
                offset.0.x = new;
                scroll.velocity.x = 0.0;
            }
            interaction.drag = Some(ScrollbarDrag {
                entity,
                axis,
                grab,
                start_offset,
            });
            frame_dirty.dirty = true;
        }
    }

    // -- Fade FSM -----------------------------------------------------
    let hover_entity = interaction.hover.map(|(e, ..)| e);
    let drag_entity = interaction.drag.map(|d| d.entity);
    for (entity, _tf, _scroll, offset, mut state) in &mut scrolls {
        let style = styles.get(entity).copied().unwrap_or_default();
        let fade_delay = style.fade_delay_secs.max(0.0);
        let fade_len = style.fade_secs.max(0.01);
        let scrolled = (offset.0 - state.last_offset).length_squared() > 0.001;
        let active = scrolled || hover_entity == Some(entity) || drag_entity == Some(entity);
        // Compute the next fade values without touching the component
        // yet - bevy change detection is write-triggered, and an
        // unconditional write would keep FrameDirty hot forever.
        let mut next = *state;
        next.last_offset = offset.0;
        if active {
            next.idle_secs = 0.0;
            next.alpha = 1.0;
        } else {
            next.idle_secs = (next.idle_secs + dt).min(fade_delay + fade_len + 1.0);
            if next.idle_secs > fade_delay {
                next.alpha = (next.alpha - dt / fade_len).max(0.0);
            }
        }
        if next != *state {
            // Only an actual alpha step is render-relevant.
            // `roll_up_frame_dirty` watches `Changed<ScrollbarState>`, so
            // clock-only updates (idle-delay countdown, `last_offset`
            // refresh, post-fade accumulator ticks) must go through
            // `bypass_change_detection` - marking them Changed turned
            // every countdown tick into a full repaint and kept
            // otherwise-idle apps rendering for seconds after the last
            // interaction.
            let visual_change = next.alpha != state.alpha;
            if visual_change {
                *state = next;
            } else {
                *state.bypass_change_detection() = next;
            }
            // Keep ticking while the fade clock still has somewhere to
            // go (idle countdown or alpha ramp) so the fade completes
            // without external events, then go quiescent.
            if state.alpha > 0.0 {
                anim.request();
            }
        }
    }
}

#[cfg(test)]
mod geometry_tests {
    //! Spec section 16.2 - thumb geometry: proportional length with theme
    //! minimum, position from offset / content extent, as-needed
    //! visibility, and the absolute 1:1 drag inverse.
    use lumen_core::prelude::*;

    #[test]
    fn no_bar_when_content_fits() {
        assert!(
            vertical_scrollbar(
                glam::Vec2::ZERO,
                glam::Vec2::new(200.0, 400.0),
                400.0,
                0.0,
                false,
                ScrollbarMetrics::default(),
            )
            .is_none(),
            "content == viewport must not show a bar (as-needed)"
        );
        assert!(
            vertical_scrollbar(
                glam::Vec2::ZERO,
                glam::Vec2::new(200.0, 400.0),
                300.0,
                0.0,
                false,
                ScrollbarMetrics::default(),
            )
            .is_none()
        );
    }

    #[test]
    fn thumb_length_is_proportional() {
        // Viewport 400, content 800 -> thumb = half the track.
        let geo = vertical_scrollbar(
            glam::Vec2::ZERO,
            glam::Vec2::new(200.0, 400.0),
            800.0,
            0.0,
            false,
            ScrollbarMetrics::default(),
        )
        .unwrap();
        let track_len = 400.0 - 2.0 * SCROLLBAR_MARGIN;
        assert!((geo.track_size.y - track_len).abs() < 0.01);
        assert!((geo.thumb_size.y - track_len / 2.0).abs() < 0.01);
        // Bar hugs the right edge.
        assert!(
            (geo.track_origin.x - (200.0 - SCROLLBAR_THICKNESS - SCROLLBAR_MARGIN)).abs() < 0.01
        );
    }

    #[test]
    fn thumb_respects_theme_minimum() {
        // Content 100 000 px -> proportional thumb would be ~1.6 px;
        // the theme minimum must win.
        let geo = vertical_scrollbar(
            glam::Vec2::ZERO,
            glam::Vec2::new(200.0, 400.0),
            100_000.0,
            0.0,
            false,
            ScrollbarMetrics::default(),
        )
        .unwrap();
        assert!((geo.thumb_size.y - SCROLLBAR_MIN_THUMB).abs() < 0.01);
    }

    #[test]
    fn thumb_position_tracks_offset() {
        let viewport = glam::Vec2::new(200.0, 400.0);
        let content = 800.0;
        let max_off = content - viewport.y; // 400
        let m = ScrollbarMetrics::default();
        let geo_top =
            vertical_scrollbar(glam::Vec2::ZERO, viewport, content, 0.0, false, m).unwrap();
        let geo_bottom =
            vertical_scrollbar(glam::Vec2::ZERO, viewport, content, max_off, false, m).unwrap();
        assert!((geo_top.thumb_origin.y - geo_top.track_origin.y).abs() < 0.01);
        let track_end = geo_bottom.track_origin.y + geo_bottom.track_size.y;
        assert!(
            (geo_bottom.thumb_origin.y + geo_bottom.thumb_size.y - track_end).abs() < 0.01,
            "at max offset the thumb's trailing edge sits at the track end"
        );
        // Midpoint.
        let geo_mid =
            vertical_scrollbar(glam::Vec2::ZERO, viewport, content, max_off / 2.0, false, m)
                .unwrap();
        let expected = geo_mid.track_origin.y + 0.5 * (geo_mid.track_size.y - geo_mid.thumb_size.y);
        assert!((geo_mid.thumb_origin.y - expected).abs() < 0.01);
    }

    #[test]
    fn drag_mapping_is_exact_inverse() {
        let viewport = glam::Vec2::new(200.0, 400.0);
        let content = 4000.0;
        for off in [0.0_f32, 123.0, 1800.0, 3600.0] {
            let geo = vertical_scrollbar(
                glam::Vec2::ZERO,
                viewport,
                content,
                off,
                false,
                ScrollbarMetrics::default(),
            )
            .unwrap();
            // Grab the thumb dead center, don't move the pointer: the
            // recovered offset must equal the input offset.
            let grab = geo.thumb_size.y / 2.0;
            let pointer = geo.thumb_origin.y + grab;
            let recovered = geo.offset_for_thumb_pos(pointer, grab, true);
            assert!(
                (recovered - off).abs() < 0.5,
                "offset {off} -> recovered {recovered}"
            );
        }
    }

    #[test]
    fn horizontal_bar_sits_on_bottom_edge() {
        let geo = horizontal_scrollbar(
            glam::Vec2::new(10.0, 20.0),
            glam::Vec2::new(300.0, 150.0),
            900.0,
            0.0,
            false,
            ScrollbarMetrics::default(),
        )
        .unwrap();
        assert!(
            (geo.track_origin.y - (20.0 + 150.0 - SCROLLBAR_THICKNESS - SCROLLBAR_MARGIN)).abs()
                < 0.01
        );
        assert!((geo.track_size.x - (300.0 - 2.0 * SCROLLBAR_MARGIN)).abs() < 0.01);
    }

    #[test]
    fn corner_reservation_shortens_track() {
        let plain = vertical_scrollbar(
            glam::Vec2::ZERO,
            glam::Vec2::new(200.0, 400.0),
            800.0,
            0.0,
            false,
            ScrollbarMetrics::default(),
        )
        .unwrap();
        let reserved = vertical_scrollbar(
            glam::Vec2::ZERO,
            glam::Vec2::new(200.0, 400.0),
            800.0,
            0.0,
            true,
            ScrollbarMetrics::default(),
        )
        .unwrap();
        assert!(
            reserved.track_size.y < plain.track_size.y,
            "corner reservation must shorten the track so bars don't overlap"
        );
    }
}

#[cfg(test)]
mod fsm_tests {
    //! Interaction FSM: thumb press + drag (1:1, pointer capture), track
    //! click jump, fade after idle, reappear on scroll.
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;

    /// 200x400 vertical scroller with 800-px content => max offset 400.
    fn world_with_scroller() -> (World, Entity) {
        let mut world = World::new();
        world.init_resource::<Messages<PointerPressed>>();
        world.init_resource::<Messages<PointerReleased>>();
        world.init_resource::<Messages<KeyPressed>>();
        world.insert_resource(Tick::default());
        world.insert_resource(AnimationsActive::default());
        world.insert_resource(PointerState::default());
        world.insert_resource(ScrollbarInteraction::default());
        world.insert_resource(FrameDirty { dirty: false });
        let scroller = world
            .spawn((
                Transform::new(glam::Vec2::ZERO, glam::Vec2::new(200.0, 400.0)),
                Scroll::vertical().with_inertia(0.0),
                ScrollOffset::default(),
                ScrollbarState::default(),
            ))
            .id();
        world.spawn((
            Transform::new(glam::Vec2::ZERO, glam::Vec2::new(200.0, 800.0)),
            ChildOf(scroller),
        ));
        (world, scroller)
    }

    fn run(world: &mut World) {
        world.run_system_once(update_scrollbars).unwrap();
        world.resource_mut::<Messages<PointerPressed>>().clear();
        world.resource_mut::<Messages<PointerReleased>>().clear();
        world.resource_mut::<Messages<KeyPressed>>().clear();
    }

    fn set_pointer(world: &mut World, pos: Option<glam::Vec2>, down: bool) {
        *world.resource_mut::<PointerState>() = PointerState {
            position: pos,
            primary_down: down,
        };
    }

    fn press(world: &mut World, pos: glam::Vec2) {
        set_pointer(world, Some(pos), true);
        world
            .resource_mut::<Messages<PointerPressed>>()
            .write(PointerPressed {
                position: pos,
                button: PointerButton::Primary,
            });
    }

    fn release(world: &mut World, pos: glam::Vec2) {
        set_pointer(world, Some(pos), false);
        world
            .resource_mut::<Messages<PointerReleased>>()
            .write(PointerReleased {
                position: pos,
                button: PointerButton::Primary,
            });
    }

    fn offset_y(world: &World, e: Entity) -> f32 {
        world.get::<ScrollOffset>(e).unwrap().0.y
    }

    /// The x coordinate inside the vertical bar strip.
    fn bar_x() -> f32 {
        200.0 - SCROLLBAR_THICKNESS / 2.0 - SCROLLBAR_MARGIN
    }

    #[test]
    fn hovering_the_thumb_is_tracked() {
        let (mut world, scroller) = world_with_scroller();
        // Thumb occupies the top half of the track at offset 0.
        set_pointer(&mut world, Some(glam::Vec2::new(bar_x(), 50.0)), false);
        run(&mut world);
        let ix = world.resource::<ScrollbarInteraction>();
        assert_eq!(
            ix.hover.map(|(e, _, p)| (e, p)),
            Some((scroller, ScrollbarPart::Thumb)),
        );
    }

    #[test]
    fn thumb_drag_maps_one_to_one_and_captures_pointer() {
        let (mut world, scroller) = world_with_scroller();
        // Grab the thumb at y=50 (inside the thumb at offset 0)...
        set_pointer(&mut world, Some(glam::Vec2::new(bar_x(), 50.0)), false);
        run(&mut world);
        press(&mut world, glam::Vec2::new(bar_x(), 50.0));
        run(&mut world);
        assert!(
            world.resource::<ScrollbarInteraction>().drag.is_some(),
            "press on thumb starts a drag"
        );
        // ...move down 99 px: track_len - thumb_len ~ 198, max 400 =>
        // offset ~ 99/198x400 ~ 200. Pointer is far OUTSIDE the bar
        // horizontally - capture keeps the drag alive.
        set_pointer(&mut world, Some(glam::Vec2::new(500.0, 149.0)), true);
        run(&mut world);
        let off = offset_y(&world, scroller);
        assert!(
            (off - 200.0).abs() < 6.0,
            "absolute 1:1 drag mapping (got {off})"
        );
        // Release ends the drag.
        release(&mut world, glam::Vec2::new(500.0, 149.0));
        run(&mut world);
        assert!(world.resource::<ScrollbarInteraction>().drag.is_none());
    }

    #[test]
    fn escape_cancels_thumb_drag_and_restores_offset() {
        let (mut world, scroller) = world_with_scroller();
        // Start from a non-zero offset so the restore is observable.
        world.get_mut::<ScrollOffset>(scroller).unwrap().0.y = 60.0;
        // Hover + grab the thumb (offset 60 -> thumb ~ y 30..129).
        set_pointer(&mut world, Some(glam::Vec2::new(bar_x(), 80.0)), false);
        run(&mut world);
        press(&mut world, glam::Vec2::new(bar_x(), 80.0));
        run(&mut world);
        assert!(world.resource::<ScrollbarInteraction>().drag.is_some());
        // Drag down - offset moves away from 60.
        set_pointer(&mut world, Some(glam::Vec2::new(bar_x(), 180.0)), true);
        run(&mut world);
        assert!((offset_y(&world, scroller) - 60.0).abs() > 50.0);
        // Escape mid-drag: offset restored, drag ended.
        world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(NamedKey::Escape),
                modifiers: Modifiers::default(),
                repeat: false,
            });
        run(&mut world);
        assert_eq!(
            offset_y(&world, scroller),
            60.0,
            "Escape restores the pre-drag scroll offset"
        );
        assert!(
            world.resource::<ScrollbarInteraction>().drag.is_none(),
            "Escape ends the drag"
        );
        // Further pointer motion while the button is still held must
        // not resume the cancelled drag.
        set_pointer(&mut world, Some(glam::Vec2::new(bar_x(), 300.0)), true);
        run(&mut world);
        assert_eq!(offset_y(&world, scroller), 60.0);
    }

    #[test]
    fn track_click_jumps_to_position() {
        let (mut world, scroller) = world_with_scroller();
        // Click near the bottom of the track (thumb is at the top).
        let click_y = 390.0;
        set_pointer(&mut world, Some(glam::Vec2::new(bar_x(), click_y)), false);
        run(&mut world);
        assert_eq!(
            world
                .resource::<ScrollbarInteraction>()
                .hover
                .map(|(_, _, p)| p),
            Some(ScrollbarPart::Track),
        );
        press(&mut world, glam::Vec2::new(bar_x(), click_y));
        run(&mut world);
        let off = offset_y(&world, scroller);
        assert!(
            off > 350.0,
            "track click near the bottom jumps close to max offset (got {off})"
        );
    }

    #[test]
    fn bars_fade_after_idle_and_reappear_on_scroll() {
        let (mut world, scroller) = world_with_scroller();
        // Simulate a long idle stretch: step the fade clock in 100 ms
        // slices (dt is clamped to 100 ms per tick).
        for _ in 0..20 {
            world.resource_mut::<Tick>().dt = std::time::Duration::from_millis(100);
            run(&mut world);
        }
        let faded = world.get::<ScrollbarState>(scroller).unwrap().alpha;
        assert!(
            faded <= 0.001,
            "bars fade out after ~1s idle (alpha {faded})"
        );
        // Any scroll-offset change brings them straight back.
        world.get_mut::<ScrollOffset>(scroller).unwrap().0.y = 40.0;
        run(&mut world);
        let alpha = world.get::<ScrollbarState>(scroller).unwrap().alpha;
        assert!(
            (alpha - 1.0).abs() < f32::EPSILON,
            "scroll reactivates the bars (alpha {alpha})"
        );
    }

    #[test]
    fn invisible_bars_do_not_capture_the_pointer() {
        let (mut world, _scroller) = world_with_scroller();
        for _ in 0..20 {
            world.resource_mut::<Tick>().dt = std::time::Duration::from_millis(100);
            run(&mut world);
        }
        // Pointer over the (now invisible) bar strip: no hover pick.
        set_pointer(&mut world, Some(glam::Vec2::new(bar_x(), 50.0)), false);
        run(&mut world);
        assert!(
            world.resource::<ScrollbarInteraction>().hover.is_none(),
            "fully faded bars must not steal pointer input"
        );
    }
}
