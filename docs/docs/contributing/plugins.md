# Writing plugins

Lumen has four plugin kinds. Runtime plugins join a running app (systems,
resources, script functions) and are compiled into it. [Runtime
modules](#runtime-modules) are the same plugins built as prebuilt shared
libraries an app declares in `lumen.toml` and the engine loads at startup.
[Portable plugins](#portable-plugins) are prebuilt libraries too, declared in
the same table, but speak a serialized C ABI instead of linking the engine:
narrower reach, no version lock to one engine build. [Compiler
plugins](#compiler-plugins) change what `lumenc` produces from an app's
sources. The first part of this page covers runtime plugins; the other kinds
have their own sections at the end.

A runtime plugin is how Rust code joins a Lumen app. It registers systems into
the tick and render schedules, installs resources, and can replace parts of
the frame pipeline. Every backend in the workspace is a plugin, so the
extension surface you get is the one the framework uses on itself.

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

The text shaper is one of these. It lives in `ShaperService`, and a plugin that
brings its own shaper installs it with `insert_non_send`. Layout measures text
through whatever is installed, so the swap covers sizing as well as drawing.

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

### Painting your own pixels

A plugin whose content the built-in primitives cannot describe, such as a
chart, a map, or a drawing surface, paints it itself. The scene graph
positions, orders, and clips an opaque leaf, and what goes inside that leaf is
between the plugin and the render backend. Qt calls the same arrangement
`QSGRenderNode` and GTK calls it `GskGLShaderNode`.

It takes four moves.

**A main-world component** holds the state you draw from, like any other
plugin state.

**An extract function** turns that state into an `ExtractedNative` in the
render world. Place each leaf with `NativeExtract`: it resolves the paint order
for the entity's position in the document, subtracts the scroll offset of every
ancestor, reports the opacity the entity inherits, and returns nothing at all
for an entity that is hidden or scrolled out of its container. Placing a leaf
from `Transform::absolute` by hand instead pins it in place while its scroll
container scrolls. Hand the finished leaves to `upsert_native_leaves`, which
keeps one render-world entity per leaf across frames and retires the ones that
went away. Its bookkeeping is scoped to your extension id, so two plugins
extracting in the same frame never evict each other's leaves.

**A painter** registered for the same extension id draws the leaf when its
turn comes:

```rust
app.register_native_painter("acme.sparkline", SparklinePainter);
```

A painter receives its draw target as `&mut dyn Any` and downcasts it to the
backend's scene type, which for the wgpu backend is a `vello::Scene`. That
downcast is a `TypeId` match, and two builds of the same vello version are two
different types as far as `TypeId` is concerned: a crate that declared vello
itself would compile, register, and then quietly paint nothing. Take it from
the one place that cannot drift - `lumen_render_wgpu::vello`, re-exported by
the module SDK behind its off-by-default `paint` feature:

```toml
[dependencies]
lumen-module = { workspace = true, features = ["paint"] }
```

**A system that raises `FrameDirty`** when your state changes. Nothing else
will: the dirty roll-up watches the framework's own components, not yours, so
a tick that only changed your state is a clean tick and extract never runs. If
the content animates, raise `AnimationsActive` while it is still moving.

Together:

```rust
use lumen_core::prelude::*;
use std::sync::Arc;

#[derive(Component)]
struct Sparkline {
    samples: Vec<f32>,
    revision: u64,
}

impl Sparkline {
    /// Every content change takes a new stamp. Without one the frame diff reads
    /// the leaf as unchanged and the new samples never reach the screen.
    fn push(&mut self, sample: f32) {
        self.samples.push(sample);
        self.revision = next_revision();
    }
}

/// What the painter is handed. The extension id promises this type.
struct SparklinePayload {
    samples: Vec<f32>,
    opacity: f32,
}

struct SparklinePainter;

impl NativePainter for SparklinePainter {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        let Some(payload) = ctx.payload_as::<SparklinePayload>() else {
            return;
        };
        // Draw into the backend's target, in logical coordinates placed by
        // `ctx.device_transform()`. On lumen-render-wgpu the target is a
        // `vello::Scene`, reached through that crate's `vello` re-export.
        let _ = (payload, ctx.bounds, ctx.opacity);
    }
}

fn extract_sparklines(main: &mut World, render: &mut World) {
    let mut place = NativeExtract::new(main);
    let mut q = main.query::<(Entity, &Transform, &Sparkline, Option<&Opacity>)>();
    let leaves: Vec<(Entity, ExtractedNative)> = q
        .iter(main)
        .filter_map(|(e, transform, sparkline, opacity)| {
            let placed = place.place(e, transform, opacity)?;
            Some((
                e,
                ExtractedNative {
                    extension_id: "acme.sparkline".into(),
                    payload: Arc::new(SparklinePayload {
                        samples: sparkline.samples.clone(),
                        opacity: placed.opacity,
                    }),
                    bounds: placed.bounds,
                    order: placed.order,
                    revision: sparkline.revision,
                    clip_to_bounds: true,
                },
            ))
        })
        .collect();
    upsert_native_leaves(render, "acme.sparkline", leaves);
}

fn redraw_when_samples_change(
    changed: Query<(), Changed<Sparkline>>,
    mut frame_dirty: ResMut<FrameDirty>,
) {
    if !changed.is_empty() {
        frame_dirty.dirty = true;
    }
}
```

The contracts that come with the seam:

- **Bounds enclose the paint.** They must cover every pixel the painter touches,
  and they must have area. Damage is computed from them, so paint outside them
  stays on screen as a stale smear until something else repaints that region,
  and a leaf that declares zero width or height falls back to repainting the
  whole viewport because an empty rect is no damage at all. `clip_to_bounds`
  enforces the first half of the promise for you, at the cost of a clip layer.
- **The revision is pixel identity.** Two leaves with the same extension id,
  bounds, clip flag, and revision are taken to be the same pixels and cost no
  repaint. Give the leaf a new revision, from `next_revision()`, whenever its
  content changes, or the frame never repaints. The payload is not compared:
  producers rebuild it every dirty frame, so comparing it would report a change
  every time.
- **A painter takes `&self`.** One painter serves every leaf carrying its
  extension id, so per-frame state goes behind interior mutability.
- **The extension id names a contract**, covering the payload type and what the
  painter expects of the backend. A backend with no painter for an id skips
  that leaf in silence, which is what lets one scene render on a backend that
  does not implement the extension.
- **Opacity is the painter's to apply.** A bounds clip composites nothing, so
  asking to be clipped never changes a leaf's alpha. `ctx.opacity` carries what
  ancestor opacity groups accumulated; the entity's own CSS `opacity` reaches
  you as `NativePlacement::opacity` at extract, to fold into the payload the way
  the built-in extractors fold it into their colours.
- **Leave the layer stack as you found it.** The walker closes any layer a
  painter left open before it moves on, so a painter cannot cost the rest of the
  frame its clips, but a painter that pops more than it pushed has already
  closed someone else's.

Paint order always comes from the placement. Leaves that share an order key
sort by extension id and then by leaf, so what a frame paints on top does not
change between frames.

Which backends paint these leaves:

| Backend | Native leaves |
|---|---|
| `lumen-render-wgpu` | Painted. The draw target is a `vello::Scene`, reached through that crate's `vello` re-export so a painter cannot version-skew its downcast. `BACKEND_ID` names the backend in the paint context. |
| `lumen-render-headless` | Not painted. It rasterises the extracted rects directly and never walks the node tree. |
| `lumen-web-dom` | Not painted. The web target emits DOM nodes rather than walking the retained tree. |

The paint context carries no text shaper. Draw text through a sibling text
element, or shape it yourself before extract.

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
  renderer, layout, window, spawner, and timer roles are traits there and the
  concrete crates live outside.
- **Reach a backend through its service resource, not its crate.** Text goes
  through `ShaperService`, async work through `SpawnService` and
  `TimerService`. A plugin that names an implementation crate to get at one has
  chosen the app's backend for it, and an app that swaps that backend then
  runs two of them.
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

A [runtime module](#runtime-modules) that brings a tag has the same job and
one more problem: a compile loads no module. `lumenc build`, `lumenc check`,
and `lumenc package` run the parser on a machine where nothing was opened, so
the element would be refused however diligently the module registers it. The
app states the claim instead:

```toml
[dependencies]
acme-charts = { bundled = true, tags = ["sparkline"] }
```

Do both. The module registers its tags from `Plugin::build`
(`lumen_module::lumen_widget::register_widget_tag_owned`), which is what makes
a run accept them; the key is what makes a build accept them. Modules install
before the markup is parsed precisely so the first half works, which means an
app that forgot the key runs and then fails to build - so a module's
documentation says to write it. A tag the language already owns is refused
where it is declared, because a module reinterpreting `<button>` would be
settled by load order rather than by anything an author wrote.

## Exposing functions to scripts

A plugin can give the app's script functions of its own. One description covers
every language: describe the function once and a Rhai, Lua, or candela script
calls it.

The short form takes a plain Rust closure and reads the signature off its
types:

```rust
use lumen_core::prelude::*;
use lumen_script::{ScriptFn, ScriptFnAppExt, ScriptNs};

pub struct GpioPlugin;

impl Plugin for GpioPlugin {
    fn build(self, app: &mut App) {
        app.add_script_fn(
            ScriptFn::from_fn("level", |pin: i64| -> Result<i64, String> {
                match pin {
                    0..=27 => Ok(read_pin(pin)),
                    _ => Err(format!("pin {pin} is out of range")),
                }
            })
            .param_names(["pin"])
            .with_ns(ScriptNs::Named("gpio".to_string())),
        );
    }
}
```

`i64`, `f64`, `bool`, `String`, `Vec<T>`, `HashMap<String, T>` and `()` are the
types that cross, up to eight arguments. The declared return type is the value
type either way, so the same declaration binds a closure that can fail and one
that cannot.

The builder form spells the signature out, and is what you want for a doc line,
optional trailing arguments, a restricted set of languages, or a body that
queues [script commands](architecture.md):

```rust
use lumen_script::{ScriptFn, ScriptNs, ScriptTy, ScriptValue};

ScriptFn::new("level")
    .ns(ScriptNs::Named("gpio".to_string()))
    .param("pin", ScriptTy::Int)
    .ret(ScriptTy::Int)
    .doc("Read a GPIO pin.")
    .build(|cx| Ok(ScriptValue::I64(read_pin(cx.int_arg(0)))));
```

A `ScriptFn` carries the name, the namespace, a typed signature, the languages
that may see it, and the body. The body takes a call context: the arguments, and
a sink it emits script commands into when its effect belongs to the runtime
rather than to the return value. `ScriptFn::value` and `ScriptFn::commands` are
shorthands for the untyped shapes.

### Reporting a failure

A body returns `Err(message)` to raise in the script that called it. Each host
raises the way its language does, and the message names the function:

| Language | What the script sees |
| --- | --- |
| Rhai | a runtime error, caught with `try { .. } catch (e) { .. }` |
| Lua | an error, caught with `pcall` |
| candela | a `host_fn_error`, caught with `try { .. } catch "host_fn_error" { .. }` |

An uncaught failure ends that one call and is reported like any other script
error. The app keeps running, and commands the body queued before it failed are
still applied.

### Where the registration has to happen

Install a plugin that registers script functions through the plugin phase:
`RunOptions::with_plugin`, or `App::add_plugin` on the Rust SDK. That phase runs
before the script hosts load the app's program, which is the window in which a
registration can still be bound. candela resolves its `host` declarations while
the program compiles, and the artifact host binds them while the image loads, so
a function registered afterwards has nothing left to bind to. A late
registration warns and is ignored rather than half-working.

Registrations arrive in an app-wide `ScriptFnRegistry` resource. Each host
drains the entries its language may see just before it loads, then the registry
is sealed. Order is meaningful: a later function of the same namespace and name
shadows an earlier one, which is how a plugin replaces one of the runtime's own
builtins.

A plugin installed this way builds before the SDK builder's own
`insert_resource` and `add_systems` calls run, so a plugin that needs something
the builder inserts should read it from a system rather than from `build`.

### Namespaces

| `ScriptNs` | Rhai | Lua | candela |
| --- | --- | --- | --- |
| `Builtin` | `level(21)` | `level(21)` | `lumen::level(21)` |
| `Extension` | `level(21)` | `level(21)` | `native::level(21)` |
| `Named("gpio")` | `gpio::level(21)` | `gpio.level(21)` | `gpio::level(21)` |

`Builtin` is the runtime's own surface; a plugin normally takes `Extension` or a
name of its own. Rhai gets a static module per named namespace, Lua a global
table, candela a host namespace.

candela needs a declaration behind every call, and the host writes it from the
signature the plugin registered, so an app calls a plugin function without
declaring anything. An app that spells the block itself keeps it: the host skips
a namespace the source already declares, which is what a script written against
an older release, and an artifact built from one, rely on.

`lumen`, `window`, `document` and `history` are the runtime's own namespaces and
the prelude declares them. A plugin that registers into one of them gets a
warning and no declaration, because a second block for a namespace displaces the
first and would cost the app every runtime function in it. Pick a name of your
own.

A name candela cannot spell in a declaration is refused at registration, with a
message naming the plugin's namespace and function. That covers a keyword, a
hyphen, a quote and the empty string. The app compiles and runs without the
function rather than failing to compile at all.

A signature candela can name is declared with its types, whatever they are, and
the call's result is typed as declared. A call passing the wrong types or the
wrong number of arguments is refused when it runs, naming the parameter. A
variadic signature, an untyped parameter, or an optional trailing argument has
no such spelling, and is declared `any name(...)`, which candela accepts at any
shape and leaves to the body.

### Shipping candela sugar

A plugin can ship candela source of its own, compiled ahead of the app's
program, to offer method syntax over the functions it registered:

```rust
app.add_script_prelude(
    "candela",
    "gpio",
    r#"
struct Pin { number: int }
fn pin(number) { return Pin { number: number }; }
impl Pin {
    fn level(self) { return gpio::level(self.number); }
}
"#,
);
```

The script then calls `pin(21).level()`. An error inside that source is reported
against the plugin's namespace, not against a line of the app.

The sugar is compiled with the app, so it can call the runtime's own surface
(`lumen::print(..)`) as well as the plugin's. Two plugins may ship source for
the same namespace and both are kept, in registration order; a name written in
both is a compile error pointing into the wrapper.

### Limits

Values cross as scalars, strings, arrays, and string-keyed maps. A handle to
something in the world does not; pass an id and look it up.

Editor tooling reads the builtin metadata tables, which describe the runtime's
own surface, so a plugin's functions do not appear in completion or hover.

A precompiled `.cdlb` gets the same declarations a live compile gets, folded
in from whatever was registered on the host before it compiled;
`CandelaHost::compile_bytecode` folds a registered function into its
namespace's block exactly as `lumenc check` and `lumenc run` do. `lumenc build`
does not register a plugin before it compiles your script's `.cdlb`, though, so
a script that will be compiled ahead of time still writes the
`host "<ns>" { .. }` block itself. The build does not object when it is missing;
the call compiles, the function holding it is left out of the image, and the
app starts without it. For the same reason a `.cdl` wrapper cannot reach an
artifact, and the artifact host says so when it is handed one. An artifact whose
block names a function no plugin registered fails its load, naming the
function.

## Extending the asset server

The asset pipeline decides what a path becomes through a registry of loaders.
An `AssetLoader` claims file extensions, declares which kind of asset it
produces, and turns one load into a payload. The built-in image and SVG
paths are loaders like any other, registered by default and replaceable.

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

The same chain serves plugins and modules that load their own data. Take a
`SourceReader` from `AssetServer::source_reader` on the main thread and read
on any thread you like; it resolves bundles, then registered sources, then
the filesystem, so app data reaches you the way every other asset does
rather than through a direct filesystem read that a packaged app would miss.
The reader is a snapshot of the chain, so take a fresh one per request. The
audio module's track loading is the in-tree example.

## Runtime modules

A runtime module is a plugin the app does not compile in: a shared library
built from the same `Plugin` trait as everything above, declared in the app's
`lumen.toml` (see the
[`[dependencies]` reference](../reference/lumen-toml.md#dependencies)), and
loaded at startup. The loader tells a runtime module from a
[portable plugin](#portable-plugins) by the entry symbol the file exports, so
the table entry looks the same for both. Once installed a module has the same
reach as any plugin: real systems in the tick stages, its own components and
resources, queries over the app's entities.

That reach comes from linking the engine as a Rust dynamic library rather
than speaking a serialized ABI, and it sets the contract:

- **A module is version-locked to the exact engine build.** At load the
  engine reads the module's build id, inlined when the module compiled, and
  compares it against its own for exact equality. Nothing looser is safe:
  Rust's layout is not stable across rebuilds, and a skewed module would
  corrupt memory rather than fail. Rebuild modules for every engine release.
- **Any load failure is a banner, not a dead app.** A missing file, a
  mismatched build id, a library that is not a module, or a panicking
  constructor prints an unmissable stderr banner naming the module and the
  reason, and the app boots without that module. The outcome is queryable
  from the `LoadedModules` resource.
- **A prebuilt module is opened only by a dynamically linked engine.** On
  Linux and macOS that is every ordinary path: the installed `lumenc` (so
  `lumenc run` markup apps load modules), a `lumenc package` folder (the
  launcher's `liblumen` links the shared engine beside it), and a Rust SDK
  app built `prefer-dynamic`. A process that compiled the engine into itself
  instead (a static `--bundle`, a plain `cargo run` binary, a source-built
  `lumenc` without the `dynamic-engine` feature) cannot open one: it would
  map a second engine instance that shares no state with the first.
  Portable plugins still load there.
- **A module the binary was built with loads anywhere.** Compiled in, a
  module has no file to open and nothing to verify: its constructor puts it
  on a registry before `main`, and the loader answers the declared name from
  there. That works on every platform, Windows included, and takes
  precedence over anything of the same name on disk. A name that this build
  neither compiles in nor can open gets a single stderr line rather than the
  failure banner, pointing at `lumenc package --static`, which is how an app
  gets that shape without a Rust toolchain.
- **A loaded module is never unloaded.** The schedules hold function
  pointers into the library for as long as the app lives.

### Authoring a module

A module crate builds both link shapes from one source, and depends on the
SDK crate at the engine release it targets:

```toml
[lib]
crate-type = ["lib", "cdylib"]

[dependencies]
lumen-module = { git = "https://github.com/lumen-fx/lumen", tag = "v0.0.6" }
lumen-core = { git = "https://github.com/lumen-fx/lumen", tag = "v0.0.6" }
bevy_ecs = "0.19"
```

Implement `Plugin` as usual and export it with `lumen_module!`. The first
argument is the name apps declare the module under; the constructor
expression after it receives the `config` table the app declared:

```rust
use lumen_module::{lumen_module, App, ModuleConfig, Plugin};

struct ShapeTools {
    units: String,
}

impl Plugin for ShapeTools {
    fn build(self, app: &mut App) {
        // Systems, resources, components - the full surface above.
    }
}

lumen_module!("shape-tools", |config: ModuleConfig| ShapeTools {
    units: config.str("units").unwrap_or("px").to_string(),
});
```

The macro generates the entries the engine calls, names them after the
module so two modules can live in one binary, and pulls in the engine-dylib
linkage, so the crate contains no unsafe code and cannot forget the link. An
app that declares the module under a different name will not find it. A
panic in the constructor or in `build` is caught inside the module and
reported to the loader as a failed install.

Build with the engine taken as a shared library. `-C prefer-dynamic` selects
it; passing an explicit `--target` keeps the flag off build scripts and proc
macros. Scope both in the module crate's `.cargo/config.toml`:

```toml
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "prefer-dynamic"]

[build]
target = "x86_64-unknown-linux-gnu"
```

Declare the result in the app:

```toml
[dependencies]
shape-tools = { path = "modules/shape-tools/target/x86_64-unknown-linux-gnu/release/shape_tools", config = { units = "mm" } }
```

### A worked example: the audio module

`std/audio` in the tree is the shape above in production, and the engine
knows nothing about it. The one crate builds both link shapes (`lib` plus
`cdylib`), depends on `lumen-module` alone, and brings its whole surface
through the generic seams: the `audio_*` functions register through
`add_script_fns`, playback runs in the module's own systems and `NonSend`
state, the position lands in shared signals through the `PropertyStore`, and
end-of-track goes out as a plugin event the script's `on_audio_end(path)`
handler receives. A script-function body has no world access, so the
functions hand commands to the systems over a queue the module owns.
First-party modules under `std/` build in the release's own cargo
invocation, which is what keeps their build ids equal to the engine they
ship beside. A crate added there is also in the link kit that release
publishes, so `lumenc package --static` can compile it into an app the day it
ships; there is no equivalent for a module from outside the toolchain, which
travels beside the executable instead.

### A worked example: the canvas module

`std/canvas` is the same shape with pixels in it. It brings a markup element
rather than only functions, so it registers the `canvas` tag from
`Plugin::build` and the app declares the same tag under `[dependencies]` for
the compile. It adopts each element it answers for by watching for the tag,
and gives it a box by inserting an `ImageComponent` with a natural size, which
is the leaf shape the layout engine already sizes the way a canvas needs;
nothing loads, because the asset pipeline keys off a source component a canvas
never carries. It paints through the seam above, with the module's `paint`
feature supplying the vello its painter downcasts to.

The rest of its design is about where a script can run. A script-function body
has no world, so a drawing call records into a journal the module owns and one
system per tick replays the journal into a retained scene; the scene is what
the extract hands the render world, and a tick that recorded nothing
re-encodes nothing.

## Portable plugins

A portable plugin is the other prebuilt kind: a cdylib declared in the same
[`[dependencies]` table](../reference/lumen-toml.md#dependencies), loaded at
startup, that talks to the engine over a serialized C ABI instead of linking
it. That trade defines it:

- **Narrower reach, by design.** A portable plugin does not touch the ECS.
  It registers native functions the app's scripts call, ships language
  source that wraps them, and pushes events at the app from its own threads.
  A plugin wrapping a device, a service, or a native library fits here; a
  plugin that needs systems and components is a runtime module.
- **No engine build lock.** The plugin and the engine exchange bytes, so a
  built plugin works across engine builds as long as the handshake passes:
  the plugin ABI version plus the script wire version, each checked at load
  with an error naming both numbers. In practice a plugin is built once per
  release tag rather than per engine binary.
- **Every desktop platform, every host shape.** No engine dylib is needed,
  so a portable plugin loads on Windows and into statically linked builds -
  the shapes that refuse runtime modules.
- **The same failure policy.** A load failure banners and the app boots
  without the plugin; the outcome lands in the same `LoadedModules` resource.

### Authoring a portable plugin

A plugin crate is a `cdylib` depending on the SDK crate at a release tag:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
lumen-plugin = { git = "https://github.com/lumen-fx/lumen", tag = "v0.0.6" }
```

Implement `RuntimePlugin` and export it with `lumen_plugin!`. Registration
describes functions with the same shapes the in-process
[script-function surface](#exposing-functions-to-scripts) uses:

```rust
use lumen_plugin::{
    lumen_plugin, Error, InitCx, PluginFn, Registrar, RuntimePlugin, ScriptTy, ScriptValue,
};

struct Gpio;

impl RuntimePlugin for Gpio {
    fn register(&self, r: &mut Registrar, cx: &InitCx) -> Result<(), Error> {
        r.script_fn(
            PluginFn::new("gpio_read")
                .param("pin", ScriptTy::Int)
                .ret(ScriptTy::Bool)
                .doc("Read a GPIO pin.")
                .build(|cx| {
                    let level = read_pin(cx.int_arg(0)).map_err(|e| e.to_string())?;
                    Ok(ScriptValue::Bool(level))
                }),
        );
        r.prelude("candela", "gpio", "struct Gpio {}\n/* wrappers */");
        Ok(())
    }
}

lumen_plugin!(|| Gpio);
```

The macro generates the whole C-ABI surface; a plugin crate contains no
unsafe code, and a panic in a function body reaches the script as an error
rather than an abort. `register` runs once, before the app's scripts load,
and receives the app's directory, id, and the entry's `config` table through
`InitCx`. A function body gets the call's arguments and a command sink
(`Cx::emit`), so a call applies signal and tree writes on the tick it ran.

### Events from a plugin's own threads

`Registrar::host` hands out a `Host`: a cheap, thread-safe handle a worker
thread keeps for the life of the process. Through it a plugin that watches a
file, polls a device, or waits on a socket delivers what it found without
being asked:

- `host.call_handler(event, key, fallback, args)` calls a handler in the
  app's script. A per-key `on(event, key, fn)` registration wins, else the
  `fallback` fires; either way the handler receives `key` as its first
  argument, then `args`. One handler serves many sources by branching on the
  key.
- `host.emit(commands)` applies script commands - signal writes, tree
  edits - with no handler involved.
- `host.log(level, message)` writes a line to the engine's diagnostic
  output, prefixed with the plugin's name.

Delivery is asynchronous: an event queues, wakes a parked app, and fires on
the next tick, on every active script host. Both calls return `false` once
the engine stops taking events, which is what a worker thread sees while the
app shuts down; the plugin's `shutdown` runs then, best effort.

One limit worth knowing: `lumenc package` currently applies the runtime
module gates to the whole `[dependencies]` table, so packaging an app whose
only entries are portable plugins still requires the shared engine and skips
Windows staging.

## Compiler plugins

A compiler plugin is a Rust cdylib that `lumenc` loads while compiling an app.
It can rewrite the entry markup and CSS before parsing, transform the parsed
tree before the cascade, lint the cascaded tree, and emit extra build outputs.
An app declares its plugins in `lumen.toml` (see the
[`[[plugins]]` reference](../reference/lumen-toml.md#plugins)); they run on
every compile path, `lumenc check` included.

Author one by depending on the SDK crate from the Lumen repo at a release tag
and building a cdylib:

```toml
[package]
name = "markdown"

[lib]
crate-type = ["cdylib"]

[dependencies]
lumenc-plugin = { git = "https://github.com/lumen-fx/lumen", tag = "v0.0.6" }
```

Implement the `CompilerPlugin` trait and export it. Every hook has a default
no-op body; implement the ones the plugin needs:

```rust
use lumenc_plugin::{lumenc_plugin, CompilerPlugin, Ctx, Error, LayoutIR};
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Cfg {
    flavor: String,
}

#[derive(Default)]
struct Markdown;

impl CompilerPlugin for Markdown {
    fn transform_markup(&self, src: &str, ctx: &Ctx) -> Result<Option<String>, Error> {
        let cfg: Cfg = ctx.config()?;
        Ok(Some(expand_markdown(src, &cfg.flavor)))
    }
}

lumenc_plugin!(Markdown::default);
```

The full hook set, all defaulted to no-ops:

```rust
fn transform_markup(&self, src: &str, ctx: &Ctx) -> Result<Option<String>, Error>;
fn transform_css(&self, src: &str, ctx: &Ctx) -> Result<Option<String>, Error>;
fn transform_ir(&self, ir: &mut LayoutIR, ctx: &Ctx) -> Result<(), Error>;
fn lint(&self, ir: &LayoutIR, ctx: &Ctx) -> Result<Vec<Finding>, Error>;
fn emit(&self, ir: &LayoutIR, ctx: &Ctx) -> Result<Vec<Output>, Error>;
```

`Ctx::config` deserializes the entry's whole `config` table into any serde
type; a key the type does not declare is ignored unless the type opts into
`deny_unknown_fields`.

The macro generates the whole C-ABI surface; a plugin crate contains no unsafe
code. One instance serves the process and hooks take `&self`, so a plugin
holding mutable state brings its own lock.

The hooks, in pipeline order:

- `transform_markup` and `transform_css` rewrite the entry file text before
  `<include>` and `@import` splicing, so emitted directives resolve like
  hand-written ones. Only the entry markup and entry CSS pass through;
  included files and sibling page files do not. An app that ships no
  `main.css` runs the CSS hook over the empty string, so a plugin can
  synthesize the stylesheet.
- `transform_ir` receives the parsed tree after multi-page assembly and
  before asset resolution and the cascade, so an injected element gets its
  asset paths resolved and its styles applied like a hand-written one.
- `lint` reads the cascaded tree and returns findings, printed beside the
  built-in lint findings. They are advisory, never fail the build, and are
  not baked into the compiled artifact.
- `emit` returns extra build products, written under
  `.lumen/generated/<plugin>/` in the app directory. Outputs are side
  products (manifests, reports, generated sources for a later compile), not
  inputs to the compile that produced them. Under `lumenc check` the hook
  still runs and its outputs are discarded.

The plugin and the compiler exchange serialized bytes over a versioned C ABI,
so a prebuilt `lumenc` loads plugins built by any Rust toolchain. At load,
`lumenc` verifies the plugin's ABI version and the IR format version it was
built against, and that the library reports the `name` the app declared; a
mismatch fails the compile with an error naming the plugin and the fix
(rebuild against the matching Lumen tag). Plugins must build with the
default `panic = "unwind"`: the generated thunks catch a hook panic and
turn it into a compile error, which needs unwinding, so a plugin built with
`panic = "abort"` is refused at load with an error saying so.

The development loop pairs a `prebuild` hook with a `path` source, so the
plugin rebuilds before every compile that needs it:

```toml
[[hooks]]
when    = "prebuild"
run     = "cargo build --release --manifest-path plugins/markdown/Cargo.toml"
inputs  = ["plugins/markdown/src/lib.rs"]
outputs = ["plugins/markdown/target/release/libmarkdown.so"]

[[plugins]]
name = "markdown"
path = "plugins/markdown/target/release/markdown"
```

`lumenc check` runs no hooks, so checking a clean tree before the first build
fails with "no file at" the declared path; run `lumenc build` once first.
Cargo names the built file with underscores (`libmy_plugin.so` for a package
named `my-plugin`); the probe tries both spellings, so an extensionless
`path` finds it either way.

The chain is loaded once per process: editing `[[plugins]]` or rebuilding a
`path` plugin during a `lumenc run` session takes effect on the next start,
not on a hot reload (a reload reruns the hooks of the already-loaded
libraries).

Known limits: plugin functions and transformed sources are invisible to
editor tooling (the LSP, `lumenc lint --signals`, and `lumenc lint
--css-cascade` all read the untransformed files), a plugin's lint findings
do not appear in `lumenc lint --signals` output, and a parse error in
rewritten markup carries positions into the rewritten text, flagged with
"in the markup as rewritten by compiler plugins".
