# Plugin author guide

A plugin is how you add behaviour to Lumen from Rust: a widget, a visual
primitive, an integration with something the framework does not ship. The
`Plugin` trait is modelled on Bevy's. A plugin registers systems into the
tick stages, inserts resources, and, if it draws something, pushes an
extract function and render-world systems.

Reach for one when a script cannot express what you need, or when the
behaviour should be reusable across apps. Everything below assumes you are
writing Rust against `lumen-core`.

## The trait

```rust
use lumen_core::prelude::*;

pub trait Plugin: Sized {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
    fn depends_on(&self) -> &'static [&'static str] {
        &[]
    }
    fn build(self, app: &mut App);
    fn cleanup(&mut self, _app: &mut App) {}
}
```

`build` takes `self` so plugins can own non-clone payloads (text
shapers, async runtimes, sockets) and move them into the world. The
host calls `build` exactly once during `App::add_plugin`.

`depends_on` names other plugins (by `name()`) that must already be
installed. Today `add_plugin` only checks the list and prints a warning to
stderr for a missing dependency; it does not yet block the build or
reorder installation, so get the `add_plugin` call order right yourself.
`cleanup` is a teardown hook for a future version of `App` that invokes it
on drop; nothing calls it yet, so do not rely on it for now.

## Minimal skeleton

```rust
use bevy_ecs::prelude::*;
use lumen_core::prelude::*;

pub struct MyPlugin;

impl Plugin for MyPlugin {
    fn build(self, app: &mut App) {
        // 1. Resources the plugin needs.
        app.world.insert_resource(MyConfig::default());

        // 2. Systems on the main schedule.
        app.add_systems(TickStage::Systems, my_tick_system);

        // 3. (optional) Render-world systems + extract fns.
        app.add_extract_fn(extract_my_things);
    }
}

#[derive(Resource, Default)]
struct MyConfig { /* ... */ }

fn my_tick_system(/* ECS params */) {
    // ...
}

fn extract_my_things(main: &mut World, render: &mut World) {
    // Copy data from main -> render world.
}
```

Add it to an app:

```rust
let mut app = App::new();
app.add_plugin(MyPlugin);
```

## TickStages

The main schedule runs these five stages in order each tick, enforced
via `.chain()` in `App::new`. **Plugin invariant: no system may cross
stage boundaries.**

| Stage | Purpose | Examples shipped here |
|---|---|---|
| `Input` | Ingest OS events -> typed messages. Window backend writes here. | `pointer_move_to_messages`, IME input, file drops |
| `CommandDrain` | Drain the bounded `CommandQueue`. Deferred mutations are applied here. | `apply_script_commands` |
| `Systems` | App tick logic: animations, state mutations, script callbacks, validation. | `step_opacity_transitions`, `apply_tooltip_dwell`, `apply_validation` |
| `LayoutSync` | Taffy compute + Transform sync. | `compute_layout`, `update_transforms` |
| `A11ySync` | AccessKit tree diff + push. | `diff_a11y_tree`, `roll_up_frame_dirty` |

Render happens **after** the main schedule completes (not a TickStage -
runs in a separate world):

1. Extract fns copy main -> render world (per-fn, in registration order).
2. The `Render` schedule runs in the render world.

## RenderStages

The render schedule has its own stage enum. Most plugins don't touch
this; the lumen-render-wgpu crate owns it. But if you add custom
extract fns:

```rust
app.add_extract_fn(|main, render| {
    let mut q = main.query::<(&MyTag, &Transform)>();
    let snapshot: Vec<(Transform,)> = q
        .iter(main)
        .map(|(_, t)| (*t,))
        .collect();
    render.insert_resource(MyExtracted(snapshot));
});
```

Extract fns run sequentially, in registration order. They are the only way
to cross the world boundary; the two worlds have independent entity ids,
so a component or entity from one means nothing in the other.

Iterate deterministically. Archetype order shifts as marker components
like `Hovered` and `Pressed` come and go, so an extract that emits draws
in raw query order reshuffles painter order from frame to frame and paints
the wrong thing on top. Collect, sort by a stable key, then emit.

## Worked example 1 - `TooltipPlugin`

The shipped tooltip primitive is the simplest stateful widget.
Source: [`lumen/primitives/src/tooltip.rs`](https://github.com/lumen-fx/lumen/blob/main/lumen/primitives/src/tooltip.rs).

```rust
pub struct TooltipPlugin;

impl Plugin for TooltipPlugin {
    fn build(self, app: &mut App) {
        app.add_systems(
            TickStage::Systems,
            (
                record_hover_started,
                spawn_tooltip_popups,
                apply_tooltip_defaults,
                despawn_tooltip_popups,
            ),
        );
    }
}
```

Four systems, all in `TickStage::Systems`:

1. **`record_hover_started`** - on `Added<Hovered>`, record the
   instant.
2. **`spawn_tooltip_popups`** - for each trigger entity with a
   `TooltipSource` + `HoverStartedAt`, if the dwell exceeds
   `delay_ms`, spawn a `<overlay>`-shaped entity with the tooltip
   text.
3. **`apply_tooltip_defaults`** - gives a spawned popup a default fill
   and text style if it doesn't already carry one.
4. **`despawn_tooltip_popups`** - when `Hovered` is removed, despawn
   the paired popup entity.

No extract fn - the popup is just an entity with `Visuals` +
`TextContent`, so the standard rect/text extract handles it.

The markup parser strips the `<tooltip>` wrapper and attaches
`TooltipSource` to the inner child, so the plugin sees an ECS
component rather than a raw parse tree node. That separation -
**parser collapses to ECS, plugin operates on ECS** - is the canonical
shape for any widget that wants markup ergonomics + author-side
declarative API.

## Worked example 2 - `DragPlugin`

Drag is event-driven and needs to compose with the input pipeline.
Source: [`lumen/primitives/src/drag.rs`](https://github.com/lumen-fx/lumen/blob/main/lumen/primitives/src/drag.rs).

```rust
#[derive(Default)]
pub struct DragPlugin {
    /// Initial config; apps can also mutate the resource at runtime.
    pub config: DragConfig,
}

impl Plugin for DragPlugin {
    fn build(self, app: &mut App) {
        app.world.insert_resource(self.config);

        app.add_systems(
            TickStage::Systems,
            attach_drag_pending.after(lumen_input::dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            update_drag_on_move.after(attach_drag_pending),
        );
        app.add_systems(
            TickStage::Systems,
            translate_draggable.after(update_drag_on_move),
        );
        app.add_systems(
            TickStage::Systems,
            release_drag_on_unpress.after(translate_draggable),
        );
    }
}
```

Highlights:

- **Config as a plugin field, not a default-then-overwrite.** `DragPlugin`
  carries its own `DragConfig`; an app that wants a non-default drag
  threshold constructs `DragPlugin { config: DragConfig { threshold_px: 8.0 } }`
  instead of reaching into the world after the fact. `#[derive(Default)]`
  keeps `DragPlugin::default()` working for the common case.
- **Explicit ordering.** Each system names the one it must run after via
  `.after(...)`, which is what keeps `attach_drag_pending` from running
  before `lumen_input::dispatch_clicks` has inserted `Pressed` for the
  same tick.
- **New message types still need registration.** `DragStartEvent` /
  `DragMoveEvent` / `DragEndEvent` are registered once, centrally, when
  `App::new` builds the core message set - not by this plugin. A plugin
  that introduces its own message type still must call
  `MessageRegistry::register_message::<T>(&mut app.world)` for it in
  `build`; skipping that compiles fine but panics at runtime on the first
  send.
- **No render-world touch.** Drag is pure input -> component mutation.
  The render path picks up the `Transform` deltas via the standard
  extract.

## Resource conventions

| Pattern | Where to declare | Initialised by |
|---|---|---|
| **Plugin config** | `world.insert_resource(MyCfg::default())` in `build` | Plugin author; apps mutate. |
| **Per-tick scratch state** | `Local<T>` inside the system, not a Resource | bevy_ecs handles. |
| **Cross-stage shared state** | Resource; document the access stage | Plugin or app. |
| **Per-entity state** | Component, not Resource | Plugin's tick systems. |

A type is either a resource or a component, never both: `Resource` implies
`Component`, so `#[derive(Resource)]` already gives you the component impl and
adding `#[derive(Component)]` alongside it does not compile. Split the type in
two when you need one value per entity and one global value.

> **Single-init rule.** `add_plugin` does not itself guard against a
> duplicate call: adding the same plugin type twice re-runs `build` and
> stacks a second copy of every system it registered, so the schedule runs
> them twice. Check `app.is_plugin_added::<MyPlugin>()` (or
> `app.plugin_added("name")` by name) before adding a plugin that another
> plugin might already have installed.

## Threading

Lumen sizes the bevy_ecs task pool to `LUMEN_DEFAULT_THREADS` (4) by
default. If your plugin runs systems that benefit from extra
parallelism, bump the request:

```rust
app.request_threads_at_least(6);
```

Monotonic max - multiple plugins compete and the highest wins. The
`LUMEN_THREADS` env var overrides everything.

Effective at first `App::tick`; subsequent bumps no-op (the pool is
global-init).

## Naming + crate layout

The shipped primitive plugins live in `lumen/primitives`. Crate-style
plugins should publish on crates.io as `lumen-<feature>` and expose a
single `pub struct FeaturePlugin` that implements `Plugin`. Internal
components / events stay in the same crate; only the plugin type and
the author-facing components / message types should be `pub`.

## Render-world plugins

If your plugin draws something custom (a new visual primitive, a
shader effect, a debug overlay), the shape is:

1. Define an `Extracted*` component for render-world data.
2. Push an extract fn that scans the main world and inserts those
   `Extracted*`s into the render world.
3. Register a render-side system that consumes them (via
   `add_render_systems(RenderStage::...)`) and emits draws.

The shipped image / SVG / scene-fragment plugins do exactly this -
see `lumen/render-wgpu/src/` for the live shape.

## Things to avoid

- **Don't reach across worlds.** Use extract fns, period.
- **Don't write to `Transform.absolute` in `Systems`.** That's
  `LayoutSync`'s job; doing it earlier is overwritten the same tick.
- **Don't query `Changed<Transform>` in `Input`.** No system has run
  since the previous tick wrote it; the query is always empty.
- **Don't block in a system.** wgpu's render loop is fast; a blocking
  HTTP call in `TickStage::Systems` stalls every other system. Use
  the `fetch(url, tag)` builtin (off-thread; callback fires on the
  next tick) or spawn your own worker and post results back via a
  channel + drain system.

## Limits

`name` and `build` are settled. Two trait methods are declared but not
finished: a missing `depends_on` entry only logs a warning rather than
ordering installation or refusing a cycle, and `cleanup` is never called,
so do not put teardown you rely on there.

A plugin's systems persist across a script hot reload; there is no way to
remove and re-add one at runtime.

There is no plugin registry and no `lumenc add` command yet, so a plugin
reaches an app as an ordinary Cargo dependency.
