//! End-to-end check that `#[derive(Widget)]` expands cleanly and the
//! generated Plugin compiles + installs into an `App`.

use bevy_ecs::prelude::*;
use lumen_core::app::{App, Plugin};
use lumen_widget::{Attributes, Widget};
use lumen_widget_macros::Widget as DeriveWidget;

#[derive(Component, DeriveWidget, Default, Debug)]
#[widget(tag = "hello")]
struct Hello {
    #[widget(prop)]
    greeting: String,
    #[widget(prop)]
    repeat: u32,
    #[widget(state)]
    shown: bool,
}

#[test]
fn derive_emits_widget_impl_with_tag_and_name() {
    assert_eq!(Hello::parser_tag(), "hello");
    assert_eq!(Hello::name(), "Hello");
}

#[test]
fn spawn_populates_props_from_attribute_bag() {
    let mut app = App::new();
    let parent = app.world.spawn_empty().id();
    let attrs: Attributes = [("greeting", "Hi"), ("repeat", "7")].into();
    let id = Hello::spawn(parent, &attrs, &mut app.world);
    let h = app
        .world
        .entity(id)
        .get::<Hello>()
        .expect("component present");
    assert_eq!(h.greeting, "Hi");
    assert_eq!(h.repeat, 7);
    assert!(!h.shown);
}

#[test]
fn spawn_leaves_state_fields_at_default() {
    let mut app = App::new();
    let parent = app.world.spawn_empty().id();
    // `shown` is a state field - even when an attribute named "shown"
    // is present the derive must ignore it.
    let attrs: Attributes = [("greeting", "Hi"), ("shown", "true")].into();
    let id = Hello::spawn(parent, &attrs, &mut app.world);
    let h = app.world.entity(id).get::<Hello>().unwrap();
    assert!(!h.shown, "state fields must not be filled from attrs");
}

#[test]
fn missing_attribute_keeps_default_value() {
    let mut app = App::new();
    let parent = app.world.spawn_empty().id();
    let attrs = Attributes::new();
    let id = Hello::spawn(parent, &attrs, &mut app.world);
    let h = app.world.entity(id).get::<Hello>().unwrap();
    assert_eq!(h.greeting, "");
    assert_eq!(h.repeat, 0);
}

#[test]
fn derived_plugin_installs_into_app() {
    let mut app = App::new();
    let p = HelloPlugin;
    assert_eq!(p.name(), "HelloPlugin");
    app.add_plugin(HelloPlugin);
    assert!(app.is_plugin_added::<HelloPlugin>());
}

#[derive(Component, DeriveWidget, Default)]
#[widget(tag = "marker", name = "MarkerOverride")]
struct Marker;

#[test]
fn unit_struct_with_name_override() {
    assert_eq!(Marker::parser_tag(), "marker");
    assert_eq!(Marker::name(), "MarkerOverride");
}
