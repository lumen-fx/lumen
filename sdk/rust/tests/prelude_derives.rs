//! The ECS derives an app defines its own data with, from the prelude alone.
//!
//! Shaped like an app on the Rust SDK: `bevy_ecs` is not a dependency of this
//! package, so the derives below compile only if `lumenui::prelude::*` brings
//! both the derive macros and the crate their expansions name.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use lumenui::prelude::*;

#[derive(Component)]
struct Marker;

#[derive(Component)]
struct Hits(u32);

#[derive(Bundle)]
struct Target {
    marker: Marker,
    hits: Hits,
}

#[derive(Resource, Default)]
struct Total(u32);

#[derive(Message)]
struct Struck;

fn strike(mut out: MessageWriter<Struck>, mut q: Query<&mut Hits, With<Marker>>) {
    for mut hits in &mut q {
        hits.0 += 1;
        out.write(Struck);
    }
}

fn tally(mut seen: MessageReader<Struck>, mut total: ResMut<Total>) {
    total.0 += seen.read().count() as u32;
}

fn spawn_targets(mut commands: Commands, existing: Query<(), With<Marker>>) {
    if existing.is_empty() {
        for _ in 0..2 {
            commands.spawn(Target {
                marker: Marker,
                hits: Hits(0),
            });
        }
    }
}

#[test]
fn an_app_defines_components_resources_and_messages_from_the_prelude() {
    let mut app = lumenui::App::from_markup("<root/>")
        .insert_resource(Total::default())
        .add_message::<Struck>()
        .add_systems(TickStage::Systems, (spawn_targets, strike, tally).chain())
        .build_bare();

    app.tick();
    app.tick();
    app.tick();

    let mut hits = app.world.query::<&Hits>();
    let per_target: Vec<u32> = hits.iter(&app.world).map(|h| h.0).collect();
    assert_eq!(per_target, [3, 3]);
    assert_eq!(app.world.resource::<Total>().0, 6);
}
