# Plugin author guide

Lumen exposes a `Plugin` trait modelled on `bevy::Plugin`. Crate authors
register systems into `TickStage::*` (main world), add render-world
systems via `add_render_systems`, push extract fns via `add_extract_fn`,
and insert resources via `world.insert_resource`.

This page is the practical guide: the trait shape, the tick pipeline,
two worked examples (`TooltipPlugin`, `DragPlugin`), and the conventions
that make plugins compose without fighting each other.

## The trait

```rust
use lumen_core::prelude::*;

pub trait Plugin: Sized {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
    fn build(self, app: &mut App);
}
```

`build` takes `self` so plugins can own non-clone payloads (text
shapers, async runtimes, sockets) and move them into the world. The
host calls `build` exactly once during `App::add_plugin`.

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
struct MyConfig { /* … */ }

fn my_tick_system(/* ECS params */) {
    // …
}

fn extract_my_things(main: &mut World, render: &mut World) {
    // Copy data from main → render world.
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
| `Input` | Ingest OS events → typed messages. Window backend writes here. | `pointer_move_to_messages`, IME input, file drops |
| `CommandDrain` | Drain the bounded `CommandQueue`. Deferred mutations are applied here. | `apply_script_commands` |
| `Systems` | App tick logic: animations, state mutations, script callbacks, validation. | `step_opacity_transitions`, `apply_tooltip_dwell`, `apply_validation` |
| `LayoutSync` | Taffy compute + Transform sync. | `compute_layout`, `update_transforms` |
| `A11ySync` | AccessKit tree diff + push. | `diff_a11y_tree`, `roll_up_frame_dirty` |

Render happens **after** the main schedule completes (not a TickStage —
runs in a separate world):

1. Extract fns copy main → render world (per-fn, in registration order).
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

Extract fns run sequentially. They're the **only** API for crossing
the world boundary. Don't try to share components or entities
directly — the worlds intentionally have independent IDs.

## Worked example 1 — `TooltipPlugin`

The shipped tooltip primitive is the simplest stateful widget.
Source: [`lumen/primitives/src/tooltip.rs`](https://github.com/lumen-ui/lumen/blob/main/lumen/primitives/src/tooltip.rs).

```rust
pub struct TooltipPlugin;

impl Plugin for TooltipPlugin {
    fn build(self, app: &mut App) {
        app.add_systems(
            TickStage::Systems,
            (
                stamp_hover_started_at,
                spawn_tooltip_popup_when_dwell_exceeds_delay,
                despawn_tooltip_popup_on_hover_out,
            ),
        );
    }
}
```

Three systems, all in `TickStage::Systems`:

1. **`stamp_hover_started_at`** — on `Added<Hovered>`, record the
   instant.
2. **`spawn_tooltip_popup_when_dwell_exceeds_delay`** — for each
   trigger entity with a `TooltipSource` + `HoverStartedAt`, if the
   dwell exceeds `delay_ms`, spawn a `<overlay>`-shaped entity with
   the tooltip text.
3. **`despawn_tooltip_popup_on_hover_out`** — when `Hovered` is
   removed, despawn the popup entity.

No extract fn — the popup is just an entity with `Visuals` +
`TextContent`, so the standard rect/text extract handles it.

The markup parser strips the `<tooltip>` wrapper and attaches
`TooltipSource` to the inner child, so the plugin sees an ECS
component rather than a raw parse tree node. That separation —
**parser collapses to ECS, plugin operates on ECS** — is the canonical
shape for any widget that wants markup ergonomics + author-side
declarative API.

## Worked example 2 — `DragPlugin`

Drag is event-driven and needs to compose with the input pipeline.
Source: [`lumen/primitives/src/drag.rs`](https://github.com/lumen-ui/lumen/blob/main/lumen/primitives/src/drag.rs).

```rust
pub struct DragPlugin;

impl Plugin for DragPlugin {
    fn build(self, app: &mut App) {
        app.world.insert_resource(DragConfig::default());

        MessageRegistry::register_message::<DragStartEvent>(&mut app.world);
        MessageRegistry::register_message::<DragMoveEvent>(&mut app.world);
        MessageRegistry::register_message::<DragEndEvent>(&mut app.world);

        app.add_systems(
            TickStage::Systems,
            (
                recognise_drag_threshold,
                emit_drag_moves,
                translate_draggable_on_drag_move,
                end_drag_on_release,
            ),
        );
    }
}
```

Highlights:

- **Resources first.** `DragConfig` lives in the main world. Apps that
  want a non-default drag threshold insert their own `DragConfig`
  before adding the plugin, OR mutate the resource at runtime.
- **Register new message types.** `MessageRegistry::register_message` is
  required so producers / consumers can use `MessageWriter` /
  `MessageReader`. Skipping it makes Rust's type system happy but
  panics at runtime on first send.
- **Systems chain.** All four systems live in `TickStage::Systems`.
  Within a stage, order is set by `.chain()` or scheduling labels —
  read the per-system docstrings before parallelizing.
- **No render-world touch.** Drag is pure input → component mutation.
  The render path picks up the `Transform` deltas via the standard
  extract.

## Resource conventions

| Pattern | Where to declare | Initialised by |
|---|---|---|
| **Plugin config** | `world.insert_resource(MyCfg::default())` in `build` | Plugin author; apps mutate. |
| **Per-tick scratch state** | `Local<T>` inside the system, not a Resource | bevy_ecs handles. |
| **Cross-stage shared state** | Resource; document the access stage | Plugin or app. |
| **Per-entity state** | Component, not Resource | Plugin's tick systems. |

> **Single-init rule.** A plugin is `add_plugin`ed once. Subsequent
> calls with the same Plugin type are no-ops if you check
> `world.contains_resource::<MyResource>()` first; otherwise duplicate
> system registrations stack and the schedule runs them twice.

## Threading

Lumen sizes the bevy_ecs task pool to `LUMEN_DEFAULT_THREADS` (4) by
default. If your plugin runs systems that benefit from extra
parallelism, bump the request:

```rust
app.request_threads_at_least(6);
```

Monotonic max — multiple plugins compete and the highest wins. The
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
   `add_render_systems(RenderStage::…)`) and emits draws.

The shipped image / SVG / scene-fragment plugins do exactly this —
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

## Where this is heading

The public plugin trait is settled. Open questions still on the list:

- **Plugin registry / `lumenc add foo`** — index file mapping
  `name → git URL`. Post-v1.
- **Plugin manifest / capability declaration** — let apps opt out of
  specific plugin powers (network, files).
- **Hot-reload-friendly plugins** — currently a plugin's systems
  persist across `replace_ast`; full re-add semantics need a clean
  shutdown hook.
