//! Mouse-cursor shape selection (spec: I-beam over text inputs, pointer
//! over clickables, grab/grabbing on scrollbar + slider thumbs).
//!
//! [`update_cursor_request`] derives the wanted
//! [`CursorShape`](lumen_core::input::CursorShape) from the current
//! [`Hovered`] entity (plus the scrollbar / drag capture FSMs, which can
//! own the pointer while nothing is hovered) and writes it into the
//! [`CursorRequest`](lumen_core::input::CursorRequest) resource - only
//! on change, so the window backend's per-frame poll is a cheap
//! equality check. `lumen-window-winit` maps the shape onto winit's
//! `CursorIcon` and applies it to the OS window; headless runners never
//! read the resource.

use bevy_ecs::prelude::*;
use lumen_core::input::{CursorRequest, CursorShape, ScrollbarInteraction, ScrollbarPart};
use lumen_core::prelude::*;

use crate::controls::SliderThumb;
use crate::drag::DragState;
use crate::hover::Interaction;

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

/// Pick the cursor shape for this tick and write it into
/// [`CursorRequest`] (change-gated):
///
/// 1. **Grabbing** while a handle drag is in flight - a scrollbar thumb
///    drag ([`ScrollbarInteraction::drag`]) or an active [`DragState`]
///    on a slider or its thumb child.
/// 2. **Grab** with the pointer resting on a scrollbar thumb
///    ([`ScrollbarInteraction::hover`], thumb part) or a slider thumb.
/// 3. **Text** (I-beam) over [`TextInput`] entities.
/// 4. **Pointer** over clickable widgets: the hovered entity or an
///    ancestor carries [`Interaction`] (hover/press feedback - the
///    button class), [`Toggleable`], [`SliderValue`], or a
///    non-negative [`TabIndex`].
/// 5. **Default** otherwise. Disabled widgets are never [`Hovered`]
///    (`hit_test` skips them), so they fall here automatically.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update_cursor_request(
    mut req: ResMut<CursorRequest>,
    hovered: Query<Entity, With<Hovered>>,
    scrollbar_ix: Option<Res<ScrollbarInteraction>>,
    drags: Query<(Entity, &DragState)>,
    sliders: Query<(), With<SliderValue>>,
    thumbs: Query<(), With<SliderThumb>>,
    inputs: Query<(), With<TextInput>>,
    interactions: Query<(), With<Interaction>>,
    toggles: Query<(), With<Toggleable>>,
    tab_indices: Query<&TabIndex>,
    parents: Query<&ChildOf>,
) {
    // Self-or-ancestor membership walk (matches the control-resolution
    // walk in `crate::controls::resolve_control`).
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
    let is_slider_ish = |e: Entity| sliders.contains(e) || thumbs.contains(e);

    let ix = scrollbar_ix.as_deref();
    let scrollbar_dragging = ix.is_some_and(|ix| ix.drag.is_some());
    let slider_dragging = drags.iter().any(|(e, state)| {
        matches!(state, DragState::Active { .. }) && self_or_ancestor(e, &is_slider_ish)
    });
    let over_scrollbar_thumb = ix.is_some_and(|ix| {
        ix.hover
            .is_some_and(|(_, _, part)| matches!(part, ScrollbarPart::Thumb))
    });

    let shape = if scrollbar_dragging || slider_dragging {
        // 1. An in-flight handle drag owns the pointer regardless of
        //    what is hovered (capture may have moved it off the handle).
        CursorShape::Grabbing
    } else if let Ok(e) = hovered.single() {
        if over_scrollbar_thumb || thumbs.contains(e) {
            // 2. Resting on a grabbable handle.
            CursorShape::Grab
        } else if inputs.contains(e) {
            // 3. Editable text.
            CursorShape::Text
        } else if self_or_ancestor(e, &|x| {
            interactions.contains(x)
                || toggles.contains(x)
                || sliders.contains(x)
                || tab_indices.get(x).is_ok_and(|t| t.0 >= 0)
        }) {
            // 4. Clickable.
            CursorShape::Pointer
        } else {
            CursorShape::Default
        }
    } else if over_scrollbar_thumb {
        // Overlay-scrollbar hover resolves the scroll container through
        // the bar-target path; the thumb itself may hold no `Hovered`.
        CursorShape::Grab
    } else {
        CursorShape::Default
    };

    if req.0 != shape {
        req.0 = shape;
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

    fn run(world: &mut World) -> CursorShape {
        world.init_resource::<CursorRequest>();
        let mut s = Schedule::default();
        s.add_systems(update_cursor_request);
        s.run(world);
        world.resource::<CursorRequest>().0
    }

    #[test]
    fn text_input_wants_ibeam() {
        let mut world = World::new();
        world.spawn((Hovered, TextInput::default()));
        assert_eq!(run(&mut world), CursorShape::Text);
    }

    #[test]
    fn clickable_wants_pointer_inert_wants_default() {
        let mut world = World::new();
        let e = world.spawn((Hovered, Interaction::default())).id();
        assert_eq!(run(&mut world), CursorShape::Pointer);
        world.entity_mut(e).remove::<(Hovered, Interaction)>();
        world.spawn(Hovered);
        assert_eq!(run(&mut world), CursorShape::Default);
    }

    #[test]
    fn clickable_ancestor_makes_child_pointer() {
        let mut world = World::new();
        let button = world.spawn(Interaction::default()).id();
        world.spawn((Hovered, ChildOf(button)));
        assert_eq!(run(&mut world), CursorShape::Pointer);
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
        assert_eq!(run(&mut world), CursorShape::Grab, "resting on the thumb");

        world.entity_mut(thumb).insert(DragState::Active {
            start: Vec2::ZERO,
            last: Vec2::new(10.0, 0.0),
        });
        assert_eq!(run(&mut world), CursorShape::Grabbing, "thumb drag");

        // Dragged off the thumb (hover confined by pointer capture ->
        // nothing hovered): still grabbing.
        world.entity_mut(thumb).remove::<Hovered>();
        assert_eq!(run(&mut world), CursorShape::Grabbing, "capture keeps it");
    }

    #[test]
    fn generic_draggable_tile_does_not_grab() {
        let mut world = World::new();
        world.spawn((
            Hovered,
            DragState::Active {
                start: Vec2::ZERO,
                last: Vec2::ZERO,
            },
        ));
        assert_eq!(
            run(&mut world),
            CursorShape::Default,
            "only slider/scrollbar handles claim the grab cursors"
        );
    }

    #[test]
    fn tab_index_negative_is_not_clickable() {
        let mut world = World::new();
        world.spawn((Hovered, TabIndex(-1)));
        assert_eq!(run(&mut world), CursorShape::Default);
    }

    #[test]
    fn nothing_hovered_is_default() {
        let mut world = World::new();
        assert_eq!(run(&mut world), CursorShape::Default);
    }
}
