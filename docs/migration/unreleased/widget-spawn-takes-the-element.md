# `Widget::spawn` makes a given entity the widget

This affects Rust code that calls `Widget::spawn` itself or implements the
`Widget` trait by hand. An element written in markup with a
`#[derive(Widget)]` tag now becomes that widget: the spawner hands the
element and its attributes to `Widget::spawn`, which inserts the component on
it. So `spawn` no longer spawns an entity of its own; it takes the entity to
make the widget and returns nothing.

```rust
// before
let gauge = Gauge::spawn(parent, &attrs, world);
// after
let gauge = world.spawn_empty().id();
Gauge::spawn(gauge, &attrs, world);
```

A hand-written `impl Widget` changes its `spawn` the same way: insert the
component on the entity it is given.

```rust
// before
fn spawn(_parent: Entity, attrs: &Attributes, world: &mut World) -> Entity {
    world.spawn(Gauge::from(attrs)).id()
}
// after
fn spawn(entity: Entity, attrs: &Attributes, world: &mut World) {
    world.entity_mut(entity).insert(Gauge::from(attrs));
}
```

A hand-written `impl Widget` registered with `register_widget_tag` still only
makes its tag parse. Register it with `lumen_widget::register_widget::<Gauge>()`
for its elements to become the widget, as the derive's `register` now does.
