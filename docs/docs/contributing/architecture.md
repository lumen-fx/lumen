# Architecture

How a Lumen app gets from source files to pixels, and which crate owns each
step. Read [Building Lumen](building-lumen.md) first if you have not compiled
the workspace yet.

## The shape of the system

A Lumen app is markup, CSS, and a script. At startup the compiler front end
parses those into an intermediate representation, the cascade is applied to
that representation, and the result is walked once to spawn `bevy_ecs`
entities. From then on the app is an ECS: input mutates components, layout
resolves geometry, scripts mutate state through a property bus, and a
per-frame extract step copies drawable data into a second world where the
renderer submits it.

Lumen uses `bevy_ecs` as a library, not `bevy_app`. The `App` type, the plugin
trait, and the schedules are Lumen's own.

## Where the crates live

The workspace root is the engine itself: `src/` builds `liblumen`, the shared
library every app executable, launcher stub, and SDK loads. Everything else
sits under `crates/`, grouped by the role it plays.

```
src/               the engine crate (package `lumen`, builds liblumen)
include/           the C ABI headers
crates/            the flat spine: core, ir, runtime, lumenc, the widgets
crates/backends/   swappable capability implementations
crates/os/         one desktop capability per crate
crates/script/     the scripting API and its three hosts
crates/dev/        tools that never ship inside an app
sdk/               the Rust, C++, and Python SDKs
apps/              example apps
fixtures/          small apps the test suite drives
tools/             the release plumbing and the VS Code extension
```

## Crate map

### Kernel

- **lumen-core**: the framework kernel. Owns `App`, the `Plugin` trait, the
  tick loop and its stages, the two worlds, the command queue, the ECS
  component vocabulary, the property store, input types, the retained node IR,
  and every backend capability trait. Depends on no other workspace crate.
- **lumen-ir**: the shared data model. The layout IR that markup parses into,
  the CSS abstract syntax tree and cascade application, the shared value
  parsers, the `var()` resolver, and the compiled-artifact container.

### Backends

- **lumen-layout-taffy**: layout. Dirty propagation, taffy style sync, text
  intrinsic sizing, and writing absolute coordinates back onto entities.
- **lumen-text**: the shaping abstraction (`TextShaper`) plus the rope-backed
  text editing model.
- **lumen-text-cosmic**: the cosmic-text shaper implementation, with a shape
  cache.
- **lumen-render-wgpu**: the GPU renderer. Encodes the retained node tree into
  a vello scene and renders it.
- **lumen-render-headless**: a deterministic software rasterizer used by golden
  tests, with no GPU or display dependency.
- **lumen-window-winit**: the on-screen window. winit event loop, surface and
  device setup, presentation, close-request veto, and hosting the accessibility
  adapter.
- **lumen-a11y-accesskit**: translates the entity tree into AccessKit tree
  updates each tick.
- **lumen-async-tokio**: the async bridge. A tokio runtime plus a queue that
  carries results from tasks back into the main world.

### Interaction and content

- **lumen-input**: hit testing, hover and press state, click dispatch, focus
  tracking.
- **lumen-primitives**: interaction primitives with no visual styling of their
  own: scroll, drag, press, hover tint, cursor shape, tooltip, tabs, checkbox,
  radio, switch, progress, transitions, validation.
- **lumen-widget**: the `Widget` trait, the attribute bag, and the tag registry
  the markup parser consults for custom tags.
- **lumen-widget-macros**: the `#[derive(Widget)]` macro, which emits the
  `Widget` implementation, a companion plugin, and the spawn glue.
- **lumen-assets**: the asset pipeline. Content-addressed cache, decode worker
  pool, SVG parsing, GPU upload cache, disk-change invalidation, and the
  `.lpak` bundle format.
- **lumen-audio**: playback over a rodio sink, degrading to a silent device
  when no output exists.
- **lumen-i18n**: translation catalogues over Fluent, plus locale-aware number,
  date, currency, and relative-time formatting over ICU4X.

### Scripting hosts

- **lumen-script**: the host-neutral scripting layer. The `ScriptHost` trait,
  the script command vocabulary, the host-generic systems, and the DOM query
  surface.
- **lumen-script-candela**, **lumen-script-rhai**, **lumen-script-lua**: the
  three hosts. Each implements `ScriptHost` and ships a plugin.

### Operating system surfaces

Each `os-*` crate owns one capability, so an app links only what it uses.

- **lumen-os-mime**: the shared payload and action types the others exchange.
- **lumen-os-clipboard**: clipboard read, write, and clear, including the Linux
  primary selection.
- **lumen-os-dnd**: drag sources, drop targets, and inbound file drops.
- **lumen-os-filedialog**: open, save, and folder pickers.
- **lumen-os-menu**: the menu model and native menu bar attachment.
- **lumen-os-tray**: tray icon, tooltip, and context menu.
- **lumen-os-notify**: desktop notifications with action buttons.
- **lumen-os-hotkey**: global hotkey registration and polling.
- **lumen-os-launcher**: opening URLs, paths, and file-manager reveals.
- **lumen-os-power**: screen-saver and sleep inhibition.
- **lumen-os-lifecycle**: single-instance enforcement, autostart, and recent
  files. A Rust-only surface: no script builtin or config key reaches it yet.

### Assembly and tooling

- **lumen-runtime**: the runtime core. The run loop, `RunOptions`, the default
  plugin stack, hot reload, file-based pages, `lumen.toml` config, the skins,
  and the loaders for both compiled artifacts and source. Links no parser.
- **lumenc**: the compiler front end and the CLI. Markup and CSS parsers, the
  include and import resolver, the formatter, the scaffolder, and the
  `check` / `run` / `build` / `bundle` / `package` subcommands.
- **lumen**: the engine crate at the workspace root. It exports the C ABI, an
  opaque app handle, a tagged value type, and the node binding, and builds as
  the shared `liblumen` plus a static library.
- **lumen-launcher**: the executable stub `lumenc package` turns into a shipped
  app. It reads the artifact packaging put inside it, opens the shared runtime
  library beside it, and runs. It links the dlopen seam and nothing else, so it
  carries no renderer, window backend, or script host of its own.
- **lumenui** (in `sdk/rust`): the Rust SDK. Plugin groups, typed signals, safe
  node handles, and event-condition helpers.
- **lumen-devtools**: the in-window overlay, itself authored in Lumen markup
  and CSS.
- **lumen-mcp**: in-app introspection. Per-tick snapshots, message rings,
  screenshots, and a local JSON-RPC server.
- **lumen-mcp-server**: a standalone binary bridging stdio JSON-RPC to a
  running app's port.
- **lumen-lsp**: the language server for markup, CSS, and scripts.

## Two worlds

`App` holds two `bevy_ecs` worlds.

The **main world** carries application and UI state: the entity tree, styles,
layout results, focus, scripts. Its schedule runs five stages in a fixed order:

1. `Input` ingests OS events and cycles the message buffers.
2. `CommandDrain` drains the bounded command queue and applies deferred
   mutations.
3. `Systems` runs application logic: state mutation, animations, scripts.
4. `LayoutSync` runs the layout engine and writes absolute coordinates back.
5. `A11ySync` computes the accessibility diff and pushes it to the platform.

The **render world** carries per-frame drawable data and GPU resource caches.
Its schedule runs two stages: `Prepare` builds buffers, scenes, and cache
lookups; `Render` submits draw work.

`Viewport` is a resource in both worlds. The window backend writes both copies
on resize, which is why layout and rendering agree on the coordinate space.

## A tick

`App::tick` does this, in order:

1. Advance the frame clock resource: a fresh instant, the delta since the
   previous tick, and a monotonic frame counter.
2. Run the main schedule.
3. Rotate the main world's removal and despawn buffers. Standalone `bevy_ecs`
   never does this on its own, so without the explicit rotation every removed
   marker component accumulates forever.
4. Check the frame-dirty flag. When it is unset, stop here; the previous
   frame's extracted data stays in the render world for the backend to present
   again.
5. Clear the transient extracted entities, then run every registered extract
   function against both worlds.
6. Run the extract schedule, then the render schedule.
7. Rotate the render world's removal buffers.

The dirty flag is what keeps an idle app idle. It is raised from change filters
on render-relevant components and from the property store's notify queue, and
cleared by the window backend once it has presented a frame. A separate
per-tick flag is raised by animation drivers while a value is still in motion,
so the window backend can schedule a follow-up frame instead of parking
mid-tween. Neither flag spins at rest: the animation flag is cleared at the
start of every tick and only re-raised by a driver that still has motion left.

## The extract step

Extract functions are plain function pointers, not closures, so per-extract
state lives in render-world resources rather than being captured. Each takes
both worlds mutably; the main world is taken mutably only because building a
query caches component-id resolution on the world.

The default chain stashes the hidden-entity set first (priming the shared
hierarchy memos for the rest), then extracts shadows, rectangles, borders,
text, clips, and scrollbars. Plugins append to the chain or replace entries
outright, which is how a plugin that needs to alter drawable positions gets its
change into every primitive at once.

Iteration order has to be deterministic. Paint order is derived from document
order, z-index, and entity identity, never from the order archetypes happen to
iterate; without that, adding and removing hover and press markers reshuffles
archetypes and the wrong things draw on top.

The `Prepare` stage then culls: entities outside the viewport, and entities
whose main-world entity is hidden by a `Visible(false)` on itself or an
ancestor. What survives is folded into a retained node tree, a typed scene
graph with container, transform, opacity, clip, rect, shadow, outline, text,
image, and native variants. The variants map one to one onto Qt's scene graph
and GTK's GSK render nodes, so a renderer backend only has to translate each
variant to its native equivalent. Children are shared behind reference counts,
so an unchanged subtree compares equal by pointer and the frame diff
short-circuits.

## From markup to entities

The front end lives in `lumenc` and the runtime links none of it. The runtime
declares a `SourceParser` trait and whoever drives it from source (the CLI, the
Rust SDK, the C ABI development path) hands over an implementation. That
inversion exists because `lumenc` depends on `lumen-runtime` for its `run`
subcommand; a direct parser dependency the other way would be a cycle.

The load path:

1. Parse `main.lmn` into the layout IR, splicing `<include>` directives, and
   parse `main.css`, resolving `@import`.
2. Build the combined stylesheet. The built-in palette goes first, then the
   always-on user-agent baseline, then the selected skin, then the app's own
   CSS. The first three share a user-agent origin and are ordered among
   themselves by source position, so the app's rules win at equal specificity.
3. Merge the custom-property declarations from every layer into one root set
   and resolve `var()` against it.
4. Apply the cascade to the IR in a single pass. Selectors are matched, and
   declarations land on IR nodes; the cascade runs before any entity exists.
5. Walk the IR and spawn entities. Sizing attributes become a style component,
   background and radius and shadow become visuals, text and its typography
   become text components, and so on. Children link to parents through the
   `ChildOf` relationship, and `Children` is derived from that. Each entity
   also records its position in the depth-first walk, which is what gives
   painting a stable order later. Every spawned entity starts dirty so layout
   runs on the first tick.

A precompiled artifact skips steps 1 through 4 entirely: parsing, cascade, and
script concatenation all happened at build time, and the artifact carries the
finished IR. The container is a magic number, a format version, and a bincode
body holding the IR, the script source, the split of that source by the engine
that runs each part, and the page set of a multi-page app; a version the
runtime does not recognise is rejected before decoding. The engine split and
the page set are recorded rather than rediscovered, because a shipped app has
neither script files nor page files left to read them off. Pages are assembled
at compile time exactly as the from-source path assembles them, so the IR
already holds every page behind its gate and only the routing data travels
separately. Relative asset paths in the IR resolve against the directory the
artifact is run with, so a packaged app finds its files wherever it is copied.
A runtime built without the source-load path can run only artifacts, and links
no parser at all.

Some styling is re-resolved after spawn. A theme flip, a media-query change, or
a root class change bumps a style version, and a system rebuilds a synthetic
element per entity from its tag, classes, and id and re-runs selector matching
against the retained stylesheet. Interaction pseudo-classes do not take that
path: the cascade lowers `:hover`, `:focus`, `:active`, and `:disabled` into a
state component at parse time, and a system swaps between the stored variants.

`<for>` and `<if>` are not resolved at spawn time. They stay as markers that
reconcilers in the `Systems` stage keep in sync with the data behind them.
`<if>` has two policies: rebuild the subtree on each transition, or mount it
once and toggle visibility, which preserves focus, scroll position, and
per-row state.

Custom tags come from the widget registry. `#[derive(Widget)]` registers the
tag string at startup, and the markup parser consults the registry after its
built-in tag table misses, so an unknown tag is still an error but a registered
one is not.

## Layout, text, and rendering

Layout runs in `LayoutSync`. Change filters on styles, children, text, images,
and writing direction mark entities dirty; dirtiness propagates up to the
nearest relayout boundary; taffy computes the tree with a measure function that
calls the text shaper for intrinsic sizes; and the results are written back as
absolute coordinates. The taffy tree is a non-send resource, because taffy's
compact length representation stores a raw pointer and is neither `Send` nor
`Sync`.

Text shaping goes through the `TextShaper` trait: a string, a pixel size, and
shaping options in, a shaped run out, segmented by font and bidi level. The
cosmic-text implementation caches results, since the same label reshapes every
frame otherwise.

Rendering walks the retained node tree. The GPU backend encodes each leaf into
a vello scene, reusing cached fragments for leaves that have not changed, and
diffs against the previous frame's tree; an empty diff skips encode and submit
entirely and leaves the last frame on screen.

The window backend owns presentation. Vello's compute pipeline pins its render
target to a linear RGBA format, while most swap chains expose a BGRA sRGB
surface, so Lumen renders into an intermediate texture of the required format
and blits that onto the surface. Which GPU backend is compiled is decided at
the manifest level, one per operating system.

## Pluggable backends

Every backend role is a trait, and the trait lives away from any
implementation of it. Marker traits in `lumen-core` name the roles: renderer,
layout engine, window backend, accessibility bridge, task spawner, timer. The
shaping trait lives in `lumen-text`, the scripting trait in `lumen-script`, the
parser trait in `lumen-runtime`.

An implementation crate depends on the trait crate and ships a plugin that
installs itself. Nothing depends on an implementation crate except the assembly
layer that chooses one, which is why the software rasterizer and the GPU
renderer are interchangeable behind the same render stage, and why a shaper or
a script host can be swapped the same way.

`CONTRIBUTING.md` states the rule that keeps the seam usable: every backend
trait needs at least one default implementation and one alternative path, so
removing the default does not break the build.

## Scripting

`lumen-script` owns the `ScriptHost` trait and every system that drives a host:
loading, reloading, ticking, routing events to handlers, firing timers,
delivering HTTP responses, and running derivations to a fixed point. Those
systems are generic over the host, so a host crate is an engine, a value
conversion layer, and a plugin whose `build` does little more than hand the
host to the generic plugin.

The metadata describing every builtin, which the LSP reads for completion and
hover, is not per host. It lives once in `crates/script/api/builtins.ron`,
listing for each builtin the hosts that expose it and, where a host spells a
signature or a doc line differently, that host's override. A build script turns
that file into the per-host tables each host crate re-exports, so the tables
cost nothing at run time and cannot drift apart by hand.

The trait covers lifecycle (compile check, load, replace, reset), invocation
(call a function, call a closure, evaluate a derivation), a command sink, a
signal mirror, the handler and derivation registries, dynamic-DOM event
dispatch, and metadata. Its associated closure type is what each engine calls a
callable: a function pointer in Rhai, a function value in Lua, and a function
*name* in candela, which has no first-class closure value and inlines
higher-order calls by symbol at compile time.

One host runs per language the app ships. Each script file picks its engine
from its own extension, and the files of one language concatenate into a single
program. An inline `<script>` has no extension to read, so it joins the app's
one external language when there is exactly one and falls to the default,
candela, otherwise.

Two languages mean two hosts running side by side, each driving its own copy of
the generic systems, so lifecycle and event callbacks reach every host that
defines them. What the hosts share is the property store, and only that: the
programs are separate, so no call crosses a language, and a signal one host
writes reaches the other through the store. That last part costs a second sync
pass. A cross-host write reaches the store mid-tick and its dirty flag clears
at the end of the same tick, so with more than one host each mirror refreshes
again inside that window, or the write would be invisible to the other language
forever.

`[script] engine` in `lumen.toml` overrides all of it and puts every script on
one host, whatever the extensions say. A precompiled artifact carries the
per-engine split the compiler recorded, and is the only source of it, since an
app's script files do not travel beside its compiled form.

Each host is monomorphised into the scheduling edges it drives. That is
load-bearing: an ordering constraint naming a different host type resolves
against an empty system set and silently drops the ordering rather than failing
to compile.

## The property bus

State lives in one property store in the main world: a typed key-value map
keyed either globally or per entity, carrying a per-key generation counter and
a dirty queue of what changed since the previous tick. Writing a value equal to
the current one does not mark it dirty, which is what keeps an idle app from
re-rendering.

Scripts do not touch the store directly. A dirty-gated system pulls changed
global string cells into the host's own mirror, and writes flow back either as
script commands on a message bus or straight onto a cross-thread channel.
Derivations are the one exception, writing the store directly, since they run
to a fixed point within a tick.

Markup bindings are a separate, older path. A `bind-` attribute becomes a
marker component, and a dirty-gated system per binding kind copies between the
component and the global namespace of the store. Text bindings skip any entity
that currently has focus or an active input method session, so a signal write
cannot race a keystroke.

The `Bindable` trait in `lumen-core` describes where this is heading: one
type-erased pipeline keyed on entity properties, replacing the per-kind marker
components. It has one reference implementation and no dispatch behind it yet.
Treat it as a design, not a hook.

There is also a deprecated string-only signals resource. It is a compatibility
shim: writes pass through to the property store, and a per-tick system mirrors
global string cells back into it so embedders holding a reference keep seeing
current values. New code uses the property store.

## Crossing threads

Off-thread code never mutates a world directly. There are three paths in.

The command queue is a bounded channel with a cloneable producer resource and a
main-thread consumer. Commands carry a type id and are dispatched to a handler
registered for that type, so a plugin adds its own command without anything
downcasting blindly. A full queue emits an overflow message rather than
blocking. Async tasks, the file dialog, and the window backend use this.

The property channel is a process-global typed channel that any thread can push
onto. It is drained in `CommandDrain`, and drained a second time later in the
tick by a distinct system, because a handler running in `Systems` writes the
bus after `CommandDrain` has already gone by and would otherwise wait a full
frame. The C ABI's typed setters use this path.

The DOM command bus carries script commands rather than world commands, and is
what the C ABI's node mutators enqueue.

## Dependency rules

These are load-bearing. Breaking one is a bug, not a style question.

- **`lumen-core` imports no implementation crate.** It carries `bevy_ecs`,
  `bevy_tasks`, and small utility crates, and nothing else. No renderer, no
  windowing, no shaper, no layout engine, no script engine. CI checks this.
- **The runtime links no parser.** `lumen-runtime` reaches the front end only
  through the injected `SourceParser`. The edge runs the other way: `lumenc`
  depends on `lumen-runtime`.
- **Hierarchy comes from `bevy_ecs::hierarchy`.** Never add `bevy_hierarchy` as
  a separate dependency; it lags behind and would create a duplicate type
  graph.
- **The wgpu version follows vello.** Use vello's re-export rather than
  declaring a second wgpu dependency, or types stop matching at the boundary.
- **Plugins consume themselves.** `Plugin::build` takes `self`, so a plugin can
  move a non-clonable payload such as a shaper or an async runtime into the
  world.

The workspace `Cargo.toml` carries the reasoning behind each pinned version
next to the pin. Read the comment before changing one, particularly for glam,
which moves independently of bevy and needs a duplicate check after any bevy
bump.

## Where to go next

- [Writing plugins](plugins.md) for the extension surface itself.
