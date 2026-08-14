# Writing plugins

A plugin is how Rust code joins a Lumen app. It registers systems into the tick
and render schedules, installs resources, and can replace parts of the frame
pipeline. Every backend in the workspace is a plugin, so the extension surface
you get is the one the framework uses on itself.

Read [Architecture](architecture.md) first for the two-world model and the
stage ordering; this page assumes both.

## The trait

The trait lives in `lumen-core`:

```rust
pub trait Plugin: Sized {
    fn name(&self) -> &'static str { /* defaults to the type name */ }
    fn depends_on(&self) -> &'static [&'static str] { &[] }
    fn build(self, app: &mut App);
    fn cleanup(&mut self, _app: &mut App) {}
}
```

`build` takes `self` by value. That is deliberate: a plugin can carry a payload
that cannot be cloned, such as a text shaper, a GPU device, or an async
runtime, and move it into the world. It also means a plugin instance is
installed exactly once.

`cleanup` is declared but nothing calls it today; do not rely on it for
teardown. Release resources from a `Drop` implementation on whatever you insert
into the world instead.

`depends_on` names plugins that must already be installed. Installation checks
each name against the installed set and prints a warning when one is missing;
it does not defer, reorder, or fail. Order your installs correctly and treat a
warning as a bug in the install order.

## A minimal plugin

```rust
use bevy_ecs::prelude::*;
use lumen_core::prelude::*;

/// Counts entities that gained focus.
#[derive(Resource, Default)]
pub struct FocusCount(pub u64);

pub struct FocusCountPlugin;

impl Plugin for FocusCountPlugin {
    fn name(&self) -> &'static str {
        "FocusCountPlugin"
    }

    fn build(self, app: &mut App) {
        app.world.init_resource::<FocusCount>();
        app.add_systems(TickStage::Systems, count_focus);
    }
}

fn count_focus(mut count: ResMut<FocusCount>, q: Query<Entity, Added<Focused>>) {
    count.0 += q.iter().count() as u64;
}
```

Install it on an `App` before the run loop starts:

```rust
app.add_plugin(FocusCountPlugin);
```

Give `name` an explicit value when the plugin is part of a public surface. The
default is the Rust type path, which changes whenever the module moves, and
`depends_on` matches on the string.

## Carrying configuration

A plugin can hold tunables and turn them into a resource in `build`. The press
recognizer does exactly that: the plugin exposes a long-press threshold and a
double-click window, `Default` supplies platform-conventional values, and
`build` moves them into a config resource that systems re-read every frame, so
an app can change them at runtime.

```rust
app.add_plugin(PressPlugin {
    long_press: Duration::from_millis(700),
    ..Default::default()
});
```

## Registration surface

`App` exposes these:

- `add_systems(TickStage, systems)` adds main-world systems to one of the five
  tick stages.
- `add_render_systems(RenderStage, systems)` adds render-world systems to
  `Prepare` or `Render`.
- `add_extract_systems(ExtractSet, systems)` adds render-world systems that run
  between extract and render, for work that reads what extract just produced.
- `add_extract_fn(f)` appends a cross-world extract function.
- `add_message::<M>()` registers a message type so writers and readers work for
  it.
- `register_command::<T, _>(handler)` registers a handler for a typed command,
  which is how off-thread code reaches the world.
- `request_threads_at_least(n)` raises the worker budget. It is monotonic
  across plugins and takes effect at the first tick.
- `is_plugin_added::<P>()` and `plugin_added(name)` answer whether something is
  already installed, for a plugin that must not double-register.

The default stack that a full app gets is assembled in `lumen-runtime`, which
is the place to look for what is already registered and in what order.

## Ordering

The tick stages are the coarse ordering: input, then command drain, then
application systems, then layout, then accessibility. Within a stage, systems
run in parallel unless you say otherwise, and two systems that touch the same
data in conflicting ways get serialised in whatever order the executor picks.
That order can flip between ticks.

So state the edges you depend on. The scroll primitive orders every writer of
the scroll offset before the hit test, because without the edge a fling that
landed after the hit test left hover markers reflecting pre-scroll positions
under a stationary cursor. The hover and press paint systems order themselves
after the input dispatch for the same reason: otherwise the visual trails the
state by a frame.

Order against public system functions from the crate that owns them, not
against your own guesses about stage packing.

## Resources

Insert plain resources with `insert_resource`, or `init_resource` when
`Default` is right. Prefer `init_resource` and a presence check when another
plugin may have installed the same resource; several plugins in the tree do
this so install order stays flexible.

Types that are not `Send` go in through `insert_non_send` and come out through
`NonSendMut`. This is not a preference, it is a requirement for several of the
backends: the taffy layout tree stores a raw pointer inside its compact length
representation and is neither `Send` nor `Sync`, and GPU device handles are
non-send on some platforms. A non-send resource pins its systems to the main
thread, which is the intended trade.

`Viewport` exists in both worlds. If your plugin resizes or reconfigures the
surface, write both copies; layout reads the main-world one and the renderer
reads the render-world one.

## Render-world plugins

A render backend inserts itself into `app.render_world` and registers a system
in `RenderStage::Render`. The whole of the software rasterizer's plugin is a
resource insert and one system registration; the GPU backend adds a fragment
cache and an optional text shaper alongside.

Getting data across is the extract step. An extract function is a plain
function pointer, `fn(&mut World, &mut World)`, not a closure, so any state it
needs lives in a render-world resource. It takes the main world mutably only
because building a query caches component ids on the world; nothing about the
app's state is meant to change there.

Adding a new drawable primitive is three pieces: an extracted component, an
extract function that produces it, and a render system that consumes it.
Replacing an existing entry in the chain, rather than appending, is how a
plugin changes what every primitive sees.

Two rules apply to anything on this path:

- **Iterate deterministically.** Paint order comes from document order, z-index,
  and entity identity. Never let it fall out of archetype iteration order:
  adding and removing hover and press markers moves entities between
  archetypes, and painter order would shuffle with them.
- **Respect the dirty flag.** The tick skips extract and render entirely when
  nothing render-relevant changed. If your plugin produces visible change
  through a path the roll-up does not observe, raise the flag; if it animates,
  raise the per-tick animation flag while it still has motion and stop raising
  it the moment it settles. A driver that raises the flag unconditionally keeps
  the window redrawing forever.

## Visual constants belong in CSS

A plugin does not hardcode how things look. Colours, metrics, and timings must
be reachable from CSS or a design token, and Rust holds at most one fallback
for the case where the author specified nothing.

The state tweens are the pattern to copy. Each has a built-in duration
constant, but the systems read the entity's transition specification first and
fall back to the constant only when CSS said nothing, so
`transition: background-color ...` overrides both the duration and the easing
curve. Scrollbar colours and metrics work the same way, resolving from
`scrollbar-color` and `scrollbar-width` with a defaulted style component
behind them.

Where a value is a per-tag default rather than a per-widget one, it goes in the
user-agent stylesheet in `lumen-runtime` instead of in Rust. That sheet applies
to every app beneath any skin and beneath the app's own CSS, so an author
overrides it with an ordinary rule. Only defaults that CSS cannot express, such
as one that applies conditionally on another property being unset, stay in
Rust.

## Constraints to respect

- **Do not add backend dependencies to `lumen-core`.** If your plugin needs a
  type in core to talk about, add the type, not the dependency. That is why the
  renderer, layout, and window roles are marker traits there and the concrete
  crates live outside.
- **Provide an alternative.** A backend trait needs at least one default
  implementation and one other path, so removing the default does not break the
  build. A headless or stub implementation counts.
- **Never let a panic cross the C ABI.** If your plugin is reachable from the
  C ABI, its failure has to become a status code, not an unwind through a
  foreign frame.
- **Keep the core stack ungated.** Layout, input, text editing, and the
  interaction primitives are always installed. Gating a subsystem on a usage
  scan is fine only when a false negative is impossible; when in doubt, install
  it and let it idle.

## Custom tags

A plugin that adds a markup tag registers it with the widget registry, which
the markup parser consults after its built-in tag table misses.
`#[derive(Widget)]` generates that registration together with a companion
plugin, so installing the plugin is enough. Registration has to happen before
the app's markup is parsed. Suppressing the generated plugin also suppresses
the registration, in which case call the generated `register` function
yourself.

## Extending the asset server

The asset pipeline decides what a path becomes through a registry of loaders.
An `AssetLoader` claims file extensions, declares which kind of asset it
produces, and turns one load into a payload. The built-in image, SVG, and
audio paths are loaders like any other, registered by default and replaceable.

```rust
use lumen_assets::{AssetKind, AssetLoader, LoadContext, LoadErrorKind, LoadedAsset};

struct QoiLoader;

impl AssetLoader for QoiLoader {
    fn extensions(&self) -> &[&str] {
        &["qoi"]
    }

    fn kind(&self) -> AssetKind {
        AssetKind::Image
    }

    fn load(&self, ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind> {
        let bytes = ctx.read_bytes()?;
        let image = decode_qoi(&bytes)?;
        Ok(LoadedAsset::Image(image))
    }
}

struct QoiPlugin;

impl Plugin for QoiPlugin {
    fn build(self, app: &mut App) {
        lumen_assets::register_asset_loader(app, QoiLoader);
    }
}
```

Four things about that signature are worth knowing before you write one.

Loading is synchronous, and a loader blocks a thread from the decode pool
rather than awaiting. Do the expensive work there; that is the point of the
pool.

A load is a path plus, optionally, bytes that were already resolved. Call
`read_bytes` when the decoder wants bytes and it does the right thing either
way; reach for `path` when the decoder wants to open the file itself, as the
image loader does. A path carrying pre-resolved bytes may be a
`lumen://app/...` URI rather than a filesystem location, so do not assume you
can open it.

The loader is chosen on the main thread when the load is queued, and travels
with the job. Registering a loader while a decode is running is therefore safe
and affects the next load, not the one in flight.

A later registration wins the extension, so registering `png` replaces the
built-in image path for PNGs. Paths whose extension nothing claims go to the
fallback loader, which is the image loader by default;
`loaders_mut().set_fallback(None)` makes them fail as unsupported instead.

Where the bytes come from is a separate seam. An `AssetSource` answers "do you
have this path?" with bytes or nothing, and the server asks each one before
queueing the load. The `.lpak` bundle source is installed by default, which is
why a packaged app resolves `icons/sun.png` out of its archive; register
another with `AssetServer::register_source` to serve assets from somewhere
else, such as bytes embedded in the binary. Sources run on the main thread
while the load is being queued, so keep them to an index lookup.
