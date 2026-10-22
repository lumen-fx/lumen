# `Viewport` is now a `WindowViewport` component on each window

This affects Rust plugins, embedders, and tests that read or write the
`lumen_core::render_world::Viewport` resource. An app can have more than one
window, so the viewport lives on each window entity, in both worlds, as
`lumen_core::window::WindowViewport`, with the same `size`, `scale_factor`, and
`clear` fields.

Read and write the primary window's with the helpers:

```rust
let size = world.resource::<Viewport>().size;
let size = primary_viewport(&world).unwrap().size;

world.resource_mut::<Viewport>().size = Vec2::new(320.0, 200.0);
primary_viewport_mut(&mut world).size = Vec2::new(320.0, 200.0);
```

A system that took `Res<Viewport>` takes the `Viewports` system parameter
instead. `viewports.primary()` is the primary window's viewport, and
`viewports.of(entity)` is the viewport of the window an element shows in,
which is the one to use for anything positioned against the window. Both
answer `None` while no window exists:

```rust
fn place(viewport: Res<Viewport>) { let w = viewport.size.x; }
fn place(viewports: Viewports) { let w = viewports.primary().map_or(0.0, |v| v.size.x); }
```

`App::new` now spawns the primary window itself on desktop, so a bare `App`
always has one there (the browser build has none); `seed_primary_window(world, title, size)` became
`seed_window(world, window, title, size)`. The `A11yRootLabel` resource is
gone: the accessibility root takes each window's title from its `Window`.
`hidden_via_ancestors` takes the window partitions as a last argument
(`Option<&WindowPartitions>`, from an `Option<Res<WindowPartitions>>`
parameter); the walk up stops at a surface root.
