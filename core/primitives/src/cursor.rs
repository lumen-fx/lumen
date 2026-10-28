//! Mouse-cursor selection.
//!
//! [`update_cursor_request`] derives the wanted cursor from the hovered
//! element's computed CSS `cursor` ([`Cursor`]), unless a widget handle the
//! stylesheet cannot reach owns the pointer: the overlay scrollbar's thumb
//! and a slider's thumb show `grab` while hovered and `grabbing` while
//! dragged. It writes the result into the
//! [`CursorRequest`](lumen_core::input::CursorRequest) resource - only on
//! change, so the window backend's per-frame poll is a cheap equality
//! check. The asset pipeline fills in the decoded image of a `url(...)`
//! cursor, and `lumen-window-winit` hands the result to the OS window;
//! headless runners never read the resource.
//!
//! The user-agent stylesheet gives text fields `cursor: text` and links
//! `cursor: pointer`; everything else shows the platform arrow until a
//! stylesheet says otherwise.

use bevy_ecs::prelude::*;
use lumen_core::input::{
    Cursor, CursorKeyword, CursorRequest, ScrollbarInteraction, ScrollbarPart,
};
use lumen_core::prelude::*;

use crate::controls::SliderThumb;
use crate::drag::DragState;

/// Plugin: installs [`CursorRequest`] and registers
/// [`update_cursor_request`] after `hit_test` so it reads this tick's
/// hover.
pub struct CursorPlugin;

impl Plugin for CursorPlugin {
    fn build(self, app: &mut App) {
        app.world.init_resource::<CursorRequest>();
        app.add_systems(
            TickStage::Systems,
            update_cursor_request.after(lumen_input::hit_test),
        );
    }
}

/// Pick the cursor for this tick and write it into [`CursorRequest`]
/// (change-gated):
///
/// 1. **grabbing** while a handle drag is in flight - a scrollbar thumb
///    drag ([`ScrollbarInteraction::drag`]) or an active [`DragState`]
///    on a slider or its thumb child.
/// 2. **grab** with the pointer resting on a scrollbar thumb
///    ([`ScrollbarInteraction::hover`], thumb part) or a slider thumb.
/// 3. Otherwise the hovered element's computed `cursor`: the [`Cursor`]
///    it or its nearest ancestor declares, which is how the property
///    inherits; `auto` when none does. Every element with a box is a
///    hit-test candidate, so a transparent box shows its own cursor.
///    Disabled widgets are never [`Hovered`] (`hit_test` skips them).
#[allow(clippy::type_complexity)]
pub fn update_cursor_request(
    mut req: ResMut<CursorRequest>,
    hovered: Query<Entity, With<Hovered>>,
    cursors: Query<&Cursor>,
    scrollbar_ix: Option<Res<ScrollbarInteraction>>,
    drags: Query<(Entity, &DragState)>,
    // A slider or its thumb, and whether it is the thumb.
    slider_parts: Query<Has<SliderThumb>, Or<(With<SliderValue>, With<SliderThumb>)>>,
    parents: Query<&ChildOf>,
) {
    let self_or_ancestor = |start: Entity, hit: &dyn Fn(Entity) -> bool| -> bool {
        let mut cur = Some(start);
        while let Some(e) = cur {
            if hit(e) {
                return true;
            }
            cur = parents.get(e).ok().map(|c| c.parent());
        }
        false
    };
    let is_slider_ish = |e: Entity| slider_parts.contains(e);
    let is_thumb = |e: Entity| slider_parts.get(e).unwrap_or(false);

    let ix = scrollbar_ix.as_deref();
    let scrollbar_dragging = ix.is_some_and(|ix| ix.drag.is_some());
    let slider_dragging = drags.iter().any(|(e, state)| {
        matches!(state, DragState::Active { .. }) && self_or_ancestor(e, &is_slider_ish)
    });
    let over_scrollbar_thumb = ix.is_some_and(|ix| {
        ix.hover
            .is_some_and(|(_, _, part)| matches!(part, ScrollbarPart::Thumb))
    });
    let hovered = hovered.single().ok();

    let cursor = if scrollbar_dragging || slider_dragging {
        // An in-flight handle drag owns the pointer regardless of what is
        // hovered (capture may have moved it off the handle).
        Cursor::keyword(CursorKeyword::Grabbing)
    } else if over_scrollbar_thumb || hovered.is_some_and(|h| self_or_ancestor(h, &is_thumb)) {
        // Resting on a grabbable handle. Overlay-scrollbar hover resolves
        // the scroll container through the bar-target path, so the thumb
        // itself may hold no `Hovered`.
        Cursor::keyword(CursorKeyword::Grab)
    } else {
        // Inheritance: the nearest declaration from the hovered element up.
        let mut at = hovered;
        let mut found = None;
        while let Some(e) = at {
            if let Ok(cursor) = cursors.get(e) {
                found = Some(cursor.clone());
                break;
            }
            at = parents.get(e).ok().map(|c| c.parent());
        }
        found.unwrap_or_default()
    };

    if req.cursor != cursor {
        req.cursor = cursor;
        // The decoded image belonged to the cursor being replaced; the
        // asset pipeline finds the new one's.
        req.image = None;
    }
}

#[cfg(test)]
mod tests {
    //! Cursor selection: I-beam over inputs, pointer over clickables
    //! (self or ancestor), grab over slider thumbs, grabbing during a
    //! slider drag, default over inert tiles and disabled widgets
    //! (which are simply never hovered).
    use super::*;
    use bevy_ecs::schedule::Schedule;
    use glam::Vec2;

    fn run(world: &mut World) -> Cursor {
        world.init_resource::<CursorRequest>();
        let mut s = Schedule::default();
        s.add_systems(update_cursor_request);
        s.run(world);
        world.resource::<CursorRequest>().cursor.clone()
    }

    fn keyword(k: CursorKeyword) -> Cursor {
        Cursor::keyword(k)
    }

    #[test]
    fn the_hovered_element_cursor_wins() {
        let mut world = World::new();
        world.spawn((Hovered, keyword(CursorKeyword::Crosshair)));
        assert_eq!(run(&mut world), keyword(CursorKeyword::Crosshair));
    }

    #[test]
    fn a_child_inherits_the_nearest_declared_cursor() {
        let mut world = World::new();
        let outer = world.spawn(keyword(CursorKeyword::Move)).id();
        let inner = world
            .spawn((keyword(CursorKeyword::Auto), ChildOf(outer)))
            .id();
        let leaf = world.spawn((Hovered, ChildOf(inner))).id();
        assert_eq!(
            run(&mut world),
            keyword(CursorKeyword::Auto),
            "an explicit auto stops the parent's value"
        );
        world.entity_mut(leaf).remove::<Hovered>();
        world.spawn((Hovered, ChildOf(outer)));
        assert_eq!(run(&mut world), keyword(CursorKeyword::Move));
    }

    #[test]
    fn an_element_with_no_cursor_is_auto() {
        let mut world = World::new();
        world.spawn(Hovered);
        assert_eq!(run(&mut world), keyword(CursorKeyword::Auto));
    }

    #[test]
    fn an_image_cursor_reaches_the_request_with_its_fallback() {
        let mut world = World::new();
        let pen = Cursor {
            images: std::sync::Arc::new([lumen_core::input::CursorImageRef {
                path: "pen.png".into(),
                hotspot: Some((2.0, 14.0)),
            }]),
            keyword: CursorKeyword::Crosshair,
        };
        world.spawn((Hovered, pen.clone()));
        assert_eq!(run(&mut world), pen);
    }

    #[test]
    fn a_new_cursor_drops_the_old_decoded_image() {
        let mut world = World::new();
        world.insert_resource(CursorRequest {
            cursor: keyword(CursorKeyword::Wait),
            image: Some(lumen_core::input::CursorImage {
                id: 1,
                width: 1,
                height: 1,
                rgba: std::sync::Arc::new([0, 0, 0, 255]),
                hotspot: (0, 0),
            }),
        });
        world.spawn((Hovered, keyword(CursorKeyword::Move)));
        run(&mut world);
        assert!(world.resource::<CursorRequest>().image.is_none());
    }

    #[test]
    fn slider_thumb_wants_grab_and_drag_wants_grabbing() {
        let mut world = World::new();
        let slider = world
            .spawn(SliderValue {
                value: 0.0,
                min: 0.0,
                max: 100.0,
                step: None,
            })
            .id();
        let thumb = world.spawn((SliderThumb, ChildOf(slider), Hovered)).id();
        assert_eq!(
            run(&mut world),
            keyword(CursorKeyword::Grab),
            "resting on the thumb"
        );

        world.entity_mut(thumb).insert(DragState::Active {
            start: Vec2::ZERO,
            last: Vec2::new(10.0, 0.0),
        });
        assert_eq!(
            run(&mut world),
            keyword(CursorKeyword::Grabbing),
            "thumb drag"
        );

        // Dragged off the thumb (hover confined by pointer capture ->
        // nothing hovered): still grabbing.
        world.entity_mut(thumb).remove::<Hovered>();
        assert_eq!(
            run(&mut world),
            keyword(CursorKeyword::Grabbing),
            "capture keeps it"
        );
    }

    #[test]
    fn generic_draggable_tile_keeps_its_own_cursor() {
        let mut world = World::new();
        world.spawn((
            Hovered,
            DragState::Active {
                start: Vec2::ZERO,
                last: Vec2::ZERO,
            },
            keyword(CursorKeyword::Move),
        ));
        assert_eq!(
            run(&mut world),
            keyword(CursorKeyword::Move),
            "only slider/scrollbar handles claim the grab cursors"
        );
    }

    #[test]
    fn nothing_hovered_is_auto() {
        let mut world = World::new();
        assert_eq!(run(&mut world), keyword(CursorKeyword::Auto));
    }
}
