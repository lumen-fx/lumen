//! Regression guard: laying out a deeply nested document costs work
//! proportional to the number of elements, not to the nesting depth.
//!
//! A column-direction container whose width comes from its content is asked
//! about twice per level - once with the width the parent will stretch it to,
//! once without, because flexbox needs the hypothetical cross size before it
//! can size the line. taffy files both answers in the same fixed cache slot,
//! so before the layout backend kept its own exact measure memo each level
//! recomputed both of its child's answers and the total work doubled per
//! level. Twenty levels took tens of seconds; a row-direction document of the
//! same shape took milliseconds.
//!
//! The assertion is on the number of nodes the solver computes, not on the
//! clock: a visit count is the thing that used to explode, and it is the same
//! on a fast laptop and a loaded CI runner.

use bevy_ecs::hierarchy::ChildOf;
use lumen_core::prelude::*;
use lumen_layout_taffy::{LayoutResource, TaffyLayoutPlugin};

/// Build a chain of `depth` nested containers around a single fixed-size
/// leaf, tick once, and report how many nodes the layout solve computed.
fn visits_for_chain(depth: usize, direction: FlexDirection) -> usize {
    let mut app = App::new();
    app.add_plugin(TaffyLayoutPlugin);
    {
        let mut layout = app.world.get_non_send_mut::<LayoutResource>().unwrap();
        layout.set_viewport(800.0, 600.0);
    }

    let root = app
        .world
        .spawn(Style {
            width: Length::Percent(100.0),
            height: Length::Percent(100.0),
            flex_direction: FlexDirection::Column,
            ..Default::default()
        })
        .id();
    let mut parent = root;
    for _ in 0..depth {
        parent = app
            .world
            .spawn((
                Style {
                    flex_direction: direction,
                    ..Default::default()
                },
                ChildOf(parent),
            ))
            .id();
    }
    app.world.spawn((
        Style {
            width: Length::Px(40.0),
            height: Length::Px(14.0),
            ..Default::default()
        },
        ChildOf(parent),
    ));

    app.tick();
    let layout = app.world.get_non_send::<LayoutResource>().unwrap();
    assert!(
        layout.visits_last_sync() > 0,
        "the tick must have solved something"
    );
    layout.visits_last_sync()
}

/// Doubling the depth may at most roughly double the work. The pre-fix
/// backend multiplied it by about a thousand over the same span, so the
/// margin here is wide enough to absorb a change in how many queries flexbox
/// makes per level while still catching a return of exponential growth.
#[test]
fn nesting_columns_costs_linear_work() {
    let shallow = visits_for_chain(8, FlexDirection::Column);
    let deep = visits_for_chain(16, FlexDirection::Column);
    assert!(
        deep <= shallow * 6,
        "column nesting must stay linear in depth: 8 levels visited {shallow} nodes, \
         16 levels visited {deep}"
    );
}

/// The row direction was always linear; it is here so a future change to the
/// memo cannot buy the column case by making this one worse.
#[test]
fn nesting_rows_costs_linear_work() {
    let shallow = visits_for_chain(8, FlexDirection::Row);
    let deep = visits_for_chain(16, FlexDirection::Row);
    assert!(
        deep <= shallow * 6,
        "row nesting must stay linear in depth: 8 levels visited {shallow} nodes, \
         16 levels visited {deep}"
    );
}

/// Per-element cost is what should drive the count, so a deep tree and a flat
/// tree with the same number of elements should land in the same ballpark.
#[test]
fn depth_costs_no_more_than_breadth() {
    let deep = visits_for_chain(16, FlexDirection::Column);

    let mut app = App::new();
    app.add_plugin(TaffyLayoutPlugin);
    {
        let mut layout = app.world.get_non_send_mut::<LayoutResource>().unwrap();
        layout.set_viewport(800.0, 600.0);
    }
    let root = app
        .world
        .spawn(Style {
            width: Length::Percent(100.0),
            height: Length::Percent(100.0),
            flex_direction: FlexDirection::Column,
            ..Default::default()
        })
        .id();
    for _ in 0..16 {
        app.world.spawn((
            Style {
                width: Length::Px(40.0),
                height: Length::Px(14.0),
                ..Default::default()
            },
            ChildOf(root),
        ));
    }
    app.tick();
    let flat = app
        .world
        .get_non_send::<LayoutResource>()
        .unwrap()
        .visits_last_sync();

    assert!(
        deep <= flat * 6,
        "16 nested containers visited {deep} nodes against {flat} for 16 siblings"
    );
}
