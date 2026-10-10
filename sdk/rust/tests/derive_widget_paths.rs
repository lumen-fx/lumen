//! `#[derive(Widget)]` in a crate that depends on the Rust SDK alone.
//!
//! This test crate is shaped like such an app: none of `lumen_widget`,
//! `lumen_core` or `bevy_ecs` is a dependency of this package, so none of them
//! is in its extern prelude, and the derive compiles only if its expansion
//! reaches them through the SDK.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use lumenui::prelude::*;

#[derive(Component, Clone, Debug, Default, Widget)]
#[widget(tag = "derive-paths-badge")]
struct Badge {
    #[widget(prop)]
    label: String,
    #[widget(prop)]
    count: i64,
}

#[test]
fn the_derive_compiles_against_the_sdk_alone() {
    assert_eq!(Badge::name(), "Badge");
    assert_eq!(Badge::parser_tag(), "derive-paths-badge");
    let plugin = BadgePlugin;
    assert_eq!(lumenui::ecs_app::Plugin::name(&plugin), "BadgePlugin");
}

#[test]
fn the_derived_spawn_reads_props_from_the_attribute_bag() {
    let mut world = lumenui::bevy_ecs::world::World::new();
    let parent = world.spawn_empty().id();
    let attrs = Attributes::from([("label", "seven"), ("count", "7")]);
    let entity = Badge::spawn(parent, &attrs, &mut world);
    let badge = world.get::<Badge>(entity).expect("spawned badge");
    assert_eq!(badge.label, "seven");
    assert_eq!(badge.count, 7);
}
