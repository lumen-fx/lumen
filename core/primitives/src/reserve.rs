//! Room an element keeps clear for a part floating along one of its edges.
//!
//! A few widgets draw a part over an edge of the element that holds them
//! rather than in its flow: the in-window menu bar along the top of the
//! root, the step buttons along the right end of a number field. Floating
//! keeps the part out of the parent's own layout (the root's `justify` and
//! `align` place the app's content as they would without a bar; a text
//! field keeps measuring its text), and [`ReservedEdge`] on the part is what
//! keeps the parent's content from running under it: the parent's padding
//! on that edge is held at what its styles say plus the part's size.
//!
//! The part's size is its own `width` or `height` when that is fixed, which
//! the user-agent sheet gives every such part, so the first frame already
//! has room; otherwise the size layout measured, and a tick that changes the
//! reservation asks for another to lay out against it.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use lumen_core::components::{Edges, Length, Style};
use lumen_core::prelude::*;
use lumen_core::render_world::AnimationsActive;
use lumen_core::style_overrides::{Live, StyleFields, StyleOverrides, StyleOwner, StyleValues};

/// On a part that floats along an edge of its parent: the edge whose padding
/// the parent grows by the part's size.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservedEdge {
    /// The parent's top edge; the part's height is reserved.
    Top,
    /// The parent's right edge; the part's width is reserved.
    Right,
}

impl StyleOwner for ReservedEdge {
    const RANK: u8 = 10;
}

/// Hold each parent's padding at its styled value plus the size of every
/// part reserving one of its edges. See the module docs.
pub fn reserve_parent_edges(
    anim: Option<Res<AnimationsActive>>,
    parts: Query<(&ReservedEdge, &ChildOf, &Style, Option<&Transform>)>,
    mut parents: Query<(&mut Style, &mut StyleOverrides), Without<ReservedEdge>>,
) {
    // Per parent: the room to keep on the top and right edges, and whether
    // any of it came from a measured (not fixed) size.
    let mut room: HashMap<Entity, (f32, f32, bool)> = HashMap::new();
    for (edge, child_of, style, size) in &parts {
        let (fixed, measured) = match edge {
            ReservedEdge::Top => (style.height, size.map(|t| t.size.y)),
            ReservedEdge::Right => (style.width, size.map(|t| t.size.x)),
        };
        let (amount, from_layout) = match fixed {
            Length::Px(v) => (v, false),
            _ => (measured.unwrap_or(0.0), true),
        };
        let entry = room.entry(child_of.parent()).or_insert((0.0, 0.0, false));
        match edge {
            ReservedEdge::Top => entry.0 += amount,
            ReservedEdge::Right => entry.1 += amount,
        }
        entry.2 |= from_layout;
    }
    for (parent, (top, right, from_layout)) in room {
        let Ok((mut style, mut overrides)) = parents.get_mut(parent) else {
            continue;
        };
        let live = Live {
            style: Some(&style),
            ..Live::default()
        };
        let resting: Edges = overrides
            .below::<ReservedEdge>(StyleFields::PADDING, &live)
            .padding
            .unwrap_or(style.padding);
        let mut want = resting;
        want.top += top;
        want.right += right;
        let held = overrides.held_by::<ReservedEdge>().and_then(|v| v.padding);
        if held
            .is_some_and(|p| (p.top - want.top).abs() < 0.5 && (p.right - want.right).abs() < 0.5)
        {
            continue;
        }
        let out = overrides.hold::<ReservedEdge>(
            StyleValues {
                padding: Some(want),
                ..StyleValues::default()
            },
            &live,
        );
        out.write_style(&mut style);
        if from_layout && let Some(anim) = &anim {
            anim.request();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::system::RunSystemOnce;

    fn parent(world: &mut World) -> Entity {
        world
            .spawn((
                Style {
                    padding: Edges {
                        top: 10.0,
                        right: 6.0,
                        left: 4.0,
                        ..Default::default()
                    },
                    ..Style::default()
                },
                StyleOverrides::default(),
            ))
            .id()
    }

    /// The parent keeps a top part's height and a right part's width clear,
    /// on top of the padding its styles give it, and holding it again does
    /// not stack it.
    #[test]
    fn the_parent_keeps_its_parts_clear() {
        let mut world = World::new();
        let root = parent(&mut world);
        world.spawn((
            ReservedEdge::Top,
            Style {
                height: Length::Px(32.0),
                ..Style::default()
            },
            ChildOf(root),
        ));
        world.spawn((
            ReservedEdge::Right,
            Style {
                width: Length::Px(40.0),
                ..Style::default()
            },
            ChildOf(root),
        ));
        world.run_system_once(reserve_parent_edges).unwrap();
        let padding = world.get::<Style>(root).unwrap().padding;
        assert_eq!(
            (padding.top, padding.right, padding.left),
            (42.0, 46.0, 4.0)
        );

        world.run_system_once(reserve_parent_edges).unwrap();
        let padding = world.get::<Style>(root).unwrap().padding;
        assert_eq!((padding.top, padding.right), (42.0, 46.0));
    }

    /// A part with no fixed size reserves what layout measured, and asks for
    /// another tick, since the room changes the layout it was measured in.
    #[test]
    fn a_measured_part_asks_for_another_tick() {
        let mut world = World::new();
        world.init_resource::<AnimationsActive>();
        let root = parent(&mut world);
        world.spawn((
            ReservedEdge::Top,
            Style::default(),
            Transform::new(glam::Vec2::ZERO, glam::Vec2::new(300.0, 28.0)),
            ChildOf(root),
        ));
        world.run_system_once(reserve_parent_edges).unwrap();
        assert_eq!(world.get::<Style>(root).unwrap().padding.top, 38.0);
        assert!(world.resource::<AnimationsActive>().get());
    }
}
