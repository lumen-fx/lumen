//! Minimal `#[derive(Widget)]` walk-through.
//!
//! Demonstrates the smallest possible custom widget: a marker
//! component carrying a single `greeting: String` prop and a `shown:
//! bool` state field.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p lumen-widget --example hello_widget
//! ```

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_widget::{Attributes, Widget};
use lumen_widget_macros::Widget as DeriveWidget;

#[derive(Component, DeriveWidget, Default, Debug)]
#[widget(tag = "hello")]
pub struct Hello {
    /// Parsed from `<hello greeting="Hi">` markup.
    #[widget(prop)]
    pub greeting: String,
    /// Runtime state - never sourced from the attribute bag.
    #[widget(state)]
    pub shown: bool,
}

fn main() {
    let mut app = App::new();
    // The derive emits `HelloPlugin`. Author still owns any
    // widget-specific systems (none here - Hello is a marker).
    app.add_plugin(HelloPlugin);

    let parent = app.world.spawn_empty().id();
    let attrs: Attributes = [("greeting", "Hi from the derive!")].into();
    let entity = Hello::spawn(parent, &attrs, &mut app.world);

    let hello = app.world.entity(entity).get::<Hello>().unwrap();
    println!(
        "spawned {} with greeting={:?} shown={}",
        Hello::name(),
        hello.greeting,
        hello.shown,
    );
    assert_eq!(Hello::parser_tag(), "hello");
    assert_eq!(hello.greeting, "Hi from the derive!");
}
