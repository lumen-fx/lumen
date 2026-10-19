# Rust handlers take a `Ui` and an `Event`, and find the nearest element with a handler

This affects Rust SDK apps that use `on_click` and its siblings,
`lumenui::simple::App`, `insert_signal`, or the `lumenui::ecs` /
`lumenui::input` / `lumenui::runtime` style paths.

A handler is now `FnMut(&mut Ui, &Event)`. `Ui` reads and writes signals, the
`<for>` lists, the element tree, menus, and the app's resources; `Event` says
which element and row the event was on, where, and with which button and
modifiers. `EventCtx` is gone:

```rust
// before
.insert_signal("count", 0i64)
.on_click("bump", |ctx| {
    let n = ctx.get_or::<i64>("count", 0) + 1;
    ctx.set("count", n);
})

// after
.signal("count", 0i64)
.on_click("bump", |ui, _| ui.update("count", |n: i64| n + 1))
```

An event now goes to the nearest element at or above its target that has a
handler: a click on a button's icon runs the button's `on_click`, where it
used to reach only a handler on the icon itself and otherwise the
`on_any_click` one with an empty id. `Event::id` in an `on_any_*` handler is
the nearest id at or above the target. Code that walked `ChildOf` to find
the clicked id can read `Event::id` instead. The `on_click` / `on_toggle` run
conditions match a click inside the element too.

`lumenui::simple::App` is removed; its `native_fn` and `script_fn` are on
`App`. `insert_signal` is now `signal`.

`App::add_systems(TickStage::Systems, ..)` puts the systems in
`lumen_core::tick::AppSet`, after the tick's clicks and control pushes and
before the binding readers, so the `.after(ClickSet::Producers)` and
`.before(apply_text_bindings)` edges apps added are no longer needed. An edge
that puts such a system after a binding reader now makes a cycle; add that
system from a plugin instead.

One name per crate: `lumenui::ecs` is `lumenui::bevy_ecs`, `lumenui::ecs_app`
is the prelude's `EcsApp` and `Plugin`, `lumenui::runtime` is
`lumenui::lumen_runtime`, `lumenui::widget` is `lumenui::lumen_widget`, and
`lumenui::input`, `components`, `tick`, `command`, `property_store` and
`core_signals` are the `lumenui::lumen_core` modules of those names. The
prelude now brings the input types events carry (`PointerButton`, `Key`,
`NamedKey`, `Modifiers`, `ModifiersState`, `MenuClicked`,
`ContextMenuEvent`, ...), `Transform`, `WakeDeadline`, `Vec2`, `ChildOf`,
`Children`, `SystemParam` and `ParamSet`, so most of those paths need no
import at all. `Property` and `PropertyStore` are in the prelude only.
