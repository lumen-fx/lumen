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
library every app executable, launcher stub, and SDK loads. The internal
crates sit beside it, grouped by role. The crates external code depends on
directly (the compiler, the plugin and module SDKs, the engine carrier) sit
under `public/`.

`core/` holds the engine kernel and the crates that extend it directly (the
IR, the runtime, the widgets, scripting). `backends/`, `os/`, `capabilities/`,
and `dev/` sit beside `core/`, not inside it, because the core stays ignorant
of its optional parts: nothing in `core/` names a specific backend, OS
integration, capability, or dev tool. `web/` groups everything whose reason to
exist is the web target, including the parts native tooling compiles.

```
src/               the engine crate (package `lumen`, builds liblumen)
include/           the C ABI headers
core/              the engine kernel and its direct extensions: core, ir,
                   runtime, the widgets, script
backends/          swappable capability implementations
os/                one desktop capability per crate
capabilities/      the registration marks a static link selects
dev/               tools that never ship inside an app
web/               the web target: emitter, browser runtime, SSR, prerender,
                   the web-only backends
public/            author-facing crates: lumenc, the plugin and module SDKs,
                   and the app SDKs (lumenui: Rust, C++, Python)
std/               first-party runtime modules: each a crate for the desktop
                   and, where it has one, a web/ half for pages
apps/              example apps
fixtures/          small apps the test suite drives
tools/             the release plumbing and the editor plugins
bucket/            the Scoop manifest; the repository is Lumen's Scoop bucket
```

## Crate map

### Kernel

- **lumen-core**: the framework kernel. Owns `App`, the `Plugin` trait, the
  tick loop and its stages, the two worlds, the command queue, the ECS
  component vocabulary, the property store, input types, the retained node IR,
  the window description a launch path resolves (size, title, clear color,
  chrome, menu bar), every backend capability trait, and the registries the
  render backends, the layout engine, and the window backend put themselves
  on. Depends on no other workspace crate.
- **lumen-ir**: the shared data model. The layout IR that markup parses into,
  the CSS abstract syntax tree and cascade application, the shared value
  parsers, the `var()` resolver, and the compiled-artifact container.
- **lumen-html**: what a Lumen app looks like as HTML. The element each markup
  tag becomes, the class and `data-lm-*` attributes an element carries, the
  node paths that name a node, and the manifest and seed types a site ships.
  The web emitter and the browser runtime both read it, so the page one writes
  is the page the other expects.
- **lumen-scene**: the scene layer, with no host in it. Spawns the element tree
  from the IR, reconciles the parts that depend on state (`<for>` rows, `<if>`
  branches, dialogs), instantiates fragments, applies the DOM mutations a
  script or the C ABI issues, and resolves navigation between pages. Mostly it
  registers nothing and a host adds the systems its own pipeline needs; the
  exceptions are navigation and the DOM pipeline, which go in as a unit because
  their internal order is not the host's to choose. `lumen-runtime` re-exports
  it, and the browser runtime uses it directly, which is what lets one scene
  run in a window and in a document.
- **lumen-paint**: what a frame looks like, independent of what draws it. The
  `Painter` trait, the walker that turns the retained node tree into painter
  calls, the leaf emitters (rects, gradients, borders, shadows, images, SVGs,
  text, the caret and selection), the fragment cache, the layer cache faded
  groups are kept in, the glyph flattening every backend fills text with,
  `Recording`, a painter that keeps its calls to replay later, and
  `PixelImage`, a tiled RGBA8 image that keeps one identity across writes and
  stamps each tile it writes, so a backend keeps one resident copy per image
  (a texture on the GPU, a premultiplied pixmap on the CPU) and re-uploads only
  the tiles whose stamps moved. Every render backend paints through it, so the
  backends agree on a frame by construction.

### Backends

- **lumen-layout-taffy**: the layout engine. Dirty propagation, taffy style
  sync, text intrinsic sizing through whichever shaper the app installed, and
  writing absolute coordinates back onto entities. It registers itself as the
  `layout-taffy` capability.
- **lumen-text**: the shaping and measuring abstraction (`TextShaper`, and the
  `ShaperService` resource that holds the installed one) plus the rope-backed
  text editing model.
- **lumen-text-cosmic**: the cosmic-text shaper implementation, with a shape
  cache.
- **lumen-render-wgpu**: the GPU renderer. Paints the frame through
  `lumen-paint` into a vello scene and renders it with wgpu, either into an
  offscreen texture or into a window's swap chain.
- **lumen-render-cpu**: the CPU renderer. Paints the same frame through
  `lumen-paint` into vello_cpu and rasterizes it with no GPU, driver, or
  graphics API, either into an offscreen buffer or into a window through
  softbuffer. A large frame rasterizes on vello_cpu's worker threads, one per
  share of its pixels up to the core count; a small one stays on the calling
  thread. It renders on machines no GPU renderer starts on, and an app that
  picks it links none of wgpu.
- **lumen-window-winit**: the on-screen window. winit event loop, input
  translation, redraw pacing, and the close-request veto. It owns no pixels: a
  renderer and an accessibility bridge are handed to it, and it drives both
  through their traits, so it compiles without naming a graphics API or an
  accessibility library. It registers itself as the `window-winit`
  capability.
- **lumen-archive** (`std/archive`): the whole archive capability, as a
  self-contained module the engine knows nothing about. It registers the
  `archive` script namespace through the generic registry, unpacks zip, tar,
  and gzip-compressed tar on the engine's spawn seam so the tick loop never
  waits on one, and reports each finished job over the plugin-event bus. An
  entry that would write outside the destination ends the extraction. One
  crate builds both link shapes: the cdylib is the bundled `lumen-archive`
  runtime module an app declares in `lumen.toml`, and a static build compiles
  the same plugin in.
- **lumen-audio** (`std/audio`): the whole audio capability, as a
  self-contained module the engine knows nothing about. It registers the
  `audio_*` script functions through the generic registry, runs playback in
  its own systems (wav and ogg over a rodio sink, silent when no output
  device exists), and delivers end-of-track over the plugin-event bus. One
  crate builds both link shapes: the cdylib is the bundled `lumen-audio`
  runtime module an app declares in `lumen.toml`, and a static build
  compiles the same plugin in.
- **lumen-canvas** (`std/canvas`): the whole drawing capability, as a
  self-contained module the engine knows nothing about. It registers the
  `canvas` script namespace through the generic registry, the `<canvas>` tag
  through the generic tag registry, and paints through the generic
  native-paint seam. A drawing call records into a journal rather than
  drawing, because a script-function body has no world; one system per tick
  replays the journal into a retained `lumen_paint::Recording`, which the
  extract hands the render world and the module's painter replays into the
  frame on either render backend. The script functions are thin callers of the
  crate's Rust API (`Context2d`, `PixelBuffer`), which is what `lumenui-canvas`
  re-exports; buffers are `lumen_paint::PixelImage`s, so a draw call records a
  snapshot of the buffer's tiles, the pixels at the call as on the web, and
  copies none of them. One crate builds both link shapes: the cdylib is the
  bundled `lumen-canvas` runtime module an app declares in `lumen.toml`, and a
  static build compiles the same plugin in. Its web half in `std/canvas/web`
  draws the same calls with Canvas 2D; a unit test in this crate holds the web
  half's functions equal to the ones it registers.
- **lumen-dock** (`std/dock`): docks and windows, as a self-contained module
  the engine knows nothing about. The arrangement is a plain-data model in
  `lumen-dock-model` (`std/dock/model`): docks of splits and tab stacks, the
  windows panels were popped out into, where hidden panels go back to, and
  the operations and saved-layout format over them, property tested on their
  own. The module reads `<dock>` markup into that model and keeps the
  elements following it: each stack is a core `<tabs>`, each split a core
  `<split>`, each window a core window entity whose surface root (`WindowOf`)
  sits under the dock, and every structure it builds is `DomHidden`, so the
  app's panels keep the dock as their DOM parent. A script function body has
  no world, so the model sits behind one lock the functions and the systems
  share; a call changes the model and the realize system makes the elements
  follow on the same tick. Modality goes through the core `InputBlocked`,
  closing a window through `WindowClosing`, and the events through the
  custom-event seam.
- **lumen-download** (`std/download`): file downloads, as a self-contained
  module the engine knows nothing about. It registers the `download` script
  namespace through the generic registry and streams each transfer to disk on
  the app's spawn seam, hashing as it writes, reporting progress and the
  outcome over the plugin-event bus. A body lands in a temp file and is
  renamed into place only once it is complete and its checksum verifies. One
  crate builds both link shapes: the cdylib is the bundled `lumen-download`
  runtime module an app declares in `lumen.toml`, and a static build compiles
  the same plugin in.
- **lumen-fs** (`std/fs`): the whole filesystem capability, as a
  self-contained module the engine knows nothing about. It registers the
  `files` script namespace through the generic registry and answers each call
  in place, with no systems of its own: paths resolve against the app
  directory, and a refusal degrades to a false or an empty value with one
  stderr line. One crate builds both link shapes: the cdylib is the bundled
  `lumen-fs` runtime module an app declares in `lumen.toml`, and a static
  build compiles the same plugin in.
- **lumen-process** (`std/process`): the whole child-process capability, as a
  self-contained module the engine knows nothing about. It registers the
  `process` script namespace through the generic registry and supervises each
  child on threads of its own, delivering the output lines and the exit over
  the plugin-event bus. One crate builds both link shapes: the cdylib is the
  bundled `lumen-process` runtime module an app declares in `lumen.toml`, and
  a static build compiles the same plugin in.
- **lumen-storage**, **lumen-cookie**, **lumen-websocket** (`std/storage`,
  `std/cookie`, `std/websocket`): modules with both halves. The web half in
  `web/` is the browser's storage, `document.cookie` and WebSocket; the crate
  is the desktop half with the same functions, taken from the web half's
  descriptor at compile time through `lumen_module::web_half_fns`. Storage
  keeps a JSON file in the app's data directory. The cookie jar reaches the
  scripts' HTTP requests through the generic `HttpHook` seam in lumen-script,
  which the core offers to any plugin and which names no cookie. The
  WebSocket client runs a thread per connection and delivers events over the
  plugin-event bus.
- **lumen-surface** (`std/surface`): the `<surface>` element, which shows
  frames another renderer draws on the GPU. It serves one channel per element
  on a session thread, imports the producer's images into the GPU renderer's
  device through `GpuContext`, presents them through `GpuTextures`, and paints
  each element as a backend texture. The engine knows nothing about it: the
  device reaches it through the render world, input through the ordinary
  input messages, and the pointer lock through `PointerLockAsk`. The web half
  is a placeholder element.
- **lumen-surface-protocol** (`std/surface/protocol`): the surface protocol
  for producers, under MPL-2.0: the wire format, the endpoint string, the
  Unix-socket transport, a producer-side state machine, and a C ABI with its
  header. `SPEC.md` beside it is the specification, and `producer/reference.c`
  is a complete producer in C and Vulkan the test suite drives.
- **lumen-js**, **lumen-browser**, **lumen-svg** (`std/js`, `std/browser`,
  `std/svg`): modules whose work is all in the web half. The crate is a
  desktop half built by `lumen_module::BrowserOnly` from the web half's
  descriptor: every function raises "runs only in a browser" and every
  element is registered as a tag, so a script and its markup run on every
  target.

  Every web half is found at `web/` under the module's root, for any source,
  beside the module's `lumen-module.toml`, which every module ships and which
  lists the markup tags it brings (`lumen_modules::manifest`). The toolchain
  archive carries the first-party ones as `bin/modules/<name>/`
  (`.github/scripts/stage-module-data.sh` stages the module descriptors, the
  web halves and the language descriptors), and a `lumenc` built from a
  checkout reads them from `std/` in place. Every compile, the run from source
  and the desktop's included, publishes the tags the declared modules'
  `lumen-module.toml` files list before it parses, and declares a web half's
  functions to the app (`lumenc::addons::target_deps`); a
  desktop run then binds those calls to the functions the loaded library
  registers. A module with no web half has no `lumen-addon.toml` to declare
  its functions, so a desktop compile installs it into a scratch headless app through the module loader
  and declares what it registers, each body answering the zero of its return
  type (`lumenc::addons::compile_deps`, `lumen_runtime::module_surface`).
  `public/lumenc/tests/std_modules.rs` reads every descriptor, and
  `.github/scripts/web-addons-smoke.py` drives each web half in headless
  Chrome.
- **lumen-http-ureq**: the HTTP client behind the scripts' `fetch()` and
  `http()` builtins. One blocking request per call over ureq, with a bounded
  body read.
- **lumen-web-http**: the same builtins in a browser, run on the page's own
  `fetch`. It replaces the dispatcher rather than the client, because a page
  has no thread to block in a request: the reply comes back from a promise and
  travels the same channel a desktop reply does.
- **lumen-a11y-accesskit**: accessibility. Translates the entity tree into
  AccessKit tree updates each tick, and binds them to a live window for the
  platform's screen readers.
- **lumen-async-tokio**: the async bridge. A tokio runtime published as the
  app's `SpawnService` and `TimerService`; a task reports back to the main
  world through the shared command queue.
- **lumen-web-dom**: the browser as a render backend. Binds each entity to a
  real element (adopting the one the prerendered page already has for it, or
  building one where the page has none), projects what the world changes onto
  the document, and turns DOM events into the messages a window backend would
  have produced. It never lays out or paints: the page does both. An element
  a module's web half answers for is bound like any other, but the walk does
  not descend into it: its content is the web half's, reached through its
  mount, update and unmount hooks.

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
  `.lpak` bundle format. Formats and byte sources are registered, so an app
  adds either without changing the crate; see
  [Writing plugins](plugins.md#extending-the-asset-server).
- **lumen-i18n**: translation catalogues over Fluent, plus locale-aware number,
  date, currency, and relative-time formatting over ICU4X.

### Scripting hosts

- **lumen-script**: the host-neutral scripting layer. The `ScriptHost` trait,
  the script command vocabulary, the host-generic systems, the DOM query
  surface, the `ScriptLanguage` registry a host module registers its language
  in, and the `HttpClient` and `HttpDispatch` traits the HTTP builtins run on:
  one says how a request is performed, the other who performs it.
- The hosts are runtime modules under `std/`, and the engine names none of
  them: **lumen-candela** (`std/candela`), **lumen-candela-dev**
  (`std/candela/dev`), and the deprecated **lumen-rhai** (`std/rhai`) and
  **lumen-lua** (`std/lua`). Each implements `ScriptHost` and registers a
  `ScriptLanguage` when it installs. The two candela modules are thin module
  entries over library crates, **lumen-candela-host** (`std/candela/host`) and
  **lumen-candela-dev-host** (`std/candela/dev/host`), so a build script can
  compile a fixture to bytecode without building a module: cargo writes a
  module's library under one file name, and a build-time copy beside the
  regular one would overwrite it.

candela is two modules, because a candela program reaches Lumen two ways.
`lumen-candela` loads a precompiled `.cdlb` image and carries no compiler,
which is what a shipped app, a page and a server render run. `lumen-candela-dev`
builds on it with the compiler: it compiles source, reloads it while the app
runs, checks it, compiles the image a shipped app carries, and reads the `lmn!`
markup blocks a script writes. Both register the same builtin list, written
once against a registration sink the compiler's engine and the runtime's host
registry each implement, so a builtin added for one is bound by the other.

### Operating system surfaces

Each `os-*` crate owns one capability, so an app links only what it uses.

- **lumen-os-mime**: the shared payload and action types the others exchange.
- **lumen-os-clipboard**: clipboard read, write, and clear. In a browser it is
  the page's `navigator.clipboard`, text only, with reads answered
  asynchronously.
- **lumen-os-dnd**: drag sources, drop targets, and inbound file drops.
- **lumen-os-filedialog**: open, save, and folder pickers.
- **lumen-os-menu**: native menu bar attachment and menu click delivery. The
  menu model itself is a core type, so window options can carry it.
- **lumen-os-tray**: tray icon, tooltip, and context menu.
- **lumen-os-notify**: desktop notifications with action buttons.
- **lumen-os-hotkey**: global hotkey registration and polling.
- **lumen-os-launcher**: opening URLs, paths, and file-manager reveals.
- **lumen-os-power**: screen-saver and sleep inhibition.
- **lumen-os-lifecycle**: single-instance enforcement, autostart, and recent
  files. A Rust-only surface: no script builtin or config key reaches it yet.
- **lumen-os-crash**: crash handling inside the app: the panic hook, the
  native fault handler (EmbarkStudios' `crash-handler`), the start of the
  helper, and the in-process report when there is no helper. Its preflight
  installs it before the app is built, so a crash while the window or the
  GPU starts is caught.
- **lumen-crash** (the `lumen-crash` executable): the out-of-process helper
  that writes the minidump of a crashed app (`minidumper`), the readable
  report beside it, and the message box. It links nothing of the engine.
- **lumen-crash-report**: the messages between an app and its helper and
  the report format both of them write.

### Assembly and tooling

- **lumen-portable**: the app every platform starts from. It installs what the
  platform it lands on does not already do (the reconcilers, the two-way
  bindings, the script command stream, key routing, and the widget behaviour
  a browser has no native control for: radio groups, tab strips and their
  panels, progress bindings, validation) and the script host for the engine
  an app names. In a browser it also installs the page's `fetch` and
  clipboard behind the script builtins. It leaves out layout, paint,
  windowing, the rest of the OS surface, and the pointer, text-editing and
  form-control half of the input layer, which a browser drives itself and a
  window installs through `lumen-input`.
  Nothing it installs is bound to one thread, so
  the app it builds can be built, ticked and dropped anywhere; its `Tick`
  schedule keeps the single-threaded executor rather than the platform
  default to hold that guarantee, since it has no layout or render work for
  a second thread to pick up anyway. Its app logic stays serial within a
  tick too. A renderer built on this crate, such as `lumen-ssr`, gets its
  parallelism from running many requests at once on separate threads, not
  from splitting one request's tick across threads.
- **lumen-runtime**: the runtime core. The run loop, `RunOptions`, the core
  plugin stack, hot reload, page discovery, `lumen.toml` config, the skins,
  and the loaders for both compiled artifacts and source. Links no parser,
  and names none of the optional subsystems it installs: each of those is a
  capability (see below) its own crate registers.
- **lumen-capability**: the list an optional subsystem puts itself on before
  `main`, and the environment it is installed with. The tray and notification
  hosts, the file dialogs, global hotkeys, the launcher, sleep inhibit, the
  lifecycle services, the HTTP client, the async executor, the introspection
  server, the devtools overlay, and the two render backends are all
  capabilities. The run loop reads
  the list at three points of the build and installs what it finds; a
  capability decides for itself whether an app uses it. Each one exports a
  symbol the linker can select it by, which is what lets a link leave a
  subsystem out with no compiler involved: `lumenc package` relinks the
  shared engine for each app that way from the release's engine kit, and
  `--static` links the executable from the link kit.
- **lumenc**: the compiler front end and the CLI. Markup and CSS parsers, the
  include and import resolver, the formatter, the scaffolder, and the
  `check` / `run` / `build` / `bundle` / `package` subcommands.
- **lumen-web**: the web target's emitter. Turns an app into a static site:
  one HTML document per page with the markup already in it, plus the manifest
  the browser runtime boots from. It emits into memory and touches no files.
- **lumen-prerender**: runs an app and reads the state it settles into, which
  is how a page is written with state already in it. It builds the app from
  `lumen-portable`, answers its HTTP calls itself so a page comes out the same
  wherever it is built, and stops when the app's state stops changing rather
  than when its frame loop goes quiet.
- **lumen-ssr**: renders an app to a document once per request, from the
  compiled app and the `lumen.site.json` a build writes. It has no transport:
  a request goes in as a struct and a response comes back as one. One
  renderer per process, on a thread of its own, behind a queue that can be
  bounded.
- **lumen-server** (`web/server`): the HTTP side, and the only HTTP server in
  the tree. The `lumen-server` binary serves a built site in production with
  `lumen-ssr` behind it: bounded connections and render queue, deadlines on
  every read and write, health endpoints, an access log, a graceful stop, and
  on unix a supervisor that runs worker processes on one inherited listening
  socket. `lumenc web --serve` starts the binary with `--dev`; lumenc links
  none of this crate, because it compiles and serves nothing itself. It is
  std networking and threads, with no async runtime.
- **lumen-web-runtime**: the browser runtime, built as a wasm module. One
  prebuilt module serves every app: a page loads it, hands it the app's
  compiled data, and it runs the same tick a desktop app runs. It installs no
  layout backend and extracts no scene, because the page's own CSS engine lays
  out and the DOM is the scene. The app itself and its script hosts come from
  `lumen-portable`; each host is a feature of the crate, and candela is the one
  the default build carries. It runs precompiled bytecode, so no compiler
  reaches the page. `boot()` is the whole entry point: it reads the document,
  fetches what the manifest names, binds the web halves of the site's modules to the
  modules the page imported, and starts the app.
- **lumen**: the engine crate at the workspace root. It exports the C ABI, an
  opaque app handle, a tagged value type, and the node binding, and builds as
  the shared `liblumen` plus a static library. That shared form is a `cdylib`:
  it exports the `extern "C"` surface and nothing else, which is what the
  launcher and the C++ and Python SDKs open.
- **lumen-dylib** (in `public/lumen-dylib`): the engine built as a Rust `dylib`
  (`liblumen_engine`), which carries Rust metadata and so can be *linked*
  rather than opened. It names the engine crates directly - never `lumen` or
  `lumenc`, which are its consumers: the Rust SDK links it always (off
  Windows), and the shipped `lumenc` binary and `liblumen` link it under
  their `dynamic-engine` feature, which is how one engine instance comes to
  be shared by the process and the runtime modules it loads. Deliberately
  not a workspace member: a `dylib` exports the whole crate graph, and on
  Windows the import library describing those exports overflows its
  65535-entry limit. Building with `-C prefer-dynamic` selects the shared
  form; without the flag, and on Windows, cargo takes the static one and
  the binary carries the runtime.
- **lumen-launcher**: the executable stub `lumenc package` turns into a shipped
  app. It reads the artifact packaging put inside it, opens the shared runtime
  library beside it, and runs. It links the dlopen seam and nothing else, so it
  carries no renderer, window backend, or script host of its own.
- **link-recorder** (in `tools/link-recorder`): a linker shim the release
  workflow puts in the linker's place for one build, so the command that
  produced the static launcher and the files it read can be kept and replayed
  later. Build tooling: no archive carries it. See
  [Building Lumen](building-lumen.md) for what the kit it feeds is for.
- **lumenui-engine** (in `public/lumenui/rust/engine`, library
  `lumenui_engine`): the Rust SDK's code. The `App`, the handlers on ids and
  the `Ui` they get (dispatched by one exclusive system in
  `lumen_core::tick::AppSet`, which resolves the nearest id and the `<for>`
  row), typed signals, safe node handles, and event-condition helpers. It
  reaches every other crate through the engine's `sdk` re-export module rather
  than depending on them again; a second path to `lumen-core` or `bevy_ecs`
  would put a second copy of those types in an app, and two copies do not share
  a signal store. Not published: each release compiles it once per desktop
  target (`tools/release/lumenui-engine.py`, run by
  `.github/workflows/lumenui-engine.yml`) and ships the compiled crates as
  `lumenui-engine-<target>.tar.zst`.
- **lumenui** (in `public/lumenui/rust`): the Rust SDK as an app depends on it,
  and the crate to publish on crates.io. It holds no code beyond `pub use
  lumenui_engine::*`; its build script (`lumenui_build::link_engine`) downloads
  the compiled set for its own version and target, checks the manifest's rustc
  against the one building, and points rustc at the set with `-L`, so an app
  compiles none of the engine. Outside the workspace, with its own
  `[workspace]` table, because its manifest names no engine crate;
  `LUMEN_ENGINE_ARTIFACTS` points it at a set built from a checkout.
- **lumenui-build** (in `public/lumenui/rust/build`): the build-script crate,
  published beside `lumenui` and outside the workspace the same way. An app's
  `build.rs` calls `compile()`, which runs `lumenc build --ui` of the same
  version (from `LUMEN_LUMENC`, `PATH`, or the release's toolchain archive)
  into `OUT_DIR` and hands the path to `lumenui::ui!()` as `LUMENUI_UI`;
  `lumenui`'s own build script calls `link_engine()`. Every download from a
  release, the engine set and the toolchain archive alike, goes through its
  `fetch` module.
- **lumenui-canvas** and the other `lumenui-<name>` module crates: a runtime
  module for a Rust SDK app, published the same way, and generated rather than
  kept in the tree. `MODULES` in `tools/release/lumenui-engine.py` lists the
  modules; the engine set compiles each in the same build as the engine, and
  the script's `wrappers` command writes each one's crate (a manifest from the
  module's and `public/lumenui/rust`'s, and `pub use lumen_<name>::*`) outside
  the checkout, for CI to lint and build and a publish to upload. The search
  path `lumenui`'s build script sets reaches the crate because it depends on
  `lumenui`. A module directory's `sdk/examples/` and `sdk/smoke/` (the canvas
  module's are in `std/canvas/sdk`) become the crate's examples and the smoke
  app CI builds against the set and runs. The engine crate names no module: a
  module reaches the SDK through its own crate.
- **lumen-devtools**: the in-window overlay, itself authored in Lumen markup
  and CSS.
- **lumen-mcp**: in-app introspection. Per-tick snapshots, message rings,
  screenshots read back from whichever renderer the app runs, and a local
  JSON-RPC server.
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

Each window's `WindowViewport` (its logical size, scale factor, and clear
colour) is a component on the window's entity in both worlds. The window
backend writes both copies on resize, which is why layout and rendering agree
on the coordinate space.

Each schedule runs on `bevy_ecs`'s multi-threaded executor: systems within a
stage that touch disjoint data run across the worker pool, while systems the
scheduler finds in conflict (same resource, same component set) still run in
the order `.chain()` or `.after()` puts them in. Some systems are pinned,
though: the layout shaper and the clipboard take `NonSendMut` params, and the
system driving an offscreen renderer takes the whole render world. Those stay
pinned to the thread that calls `App::tick`, which is
the window backend's event-loop thread. The worker count comes from
`App::desired_threads` (`min(cpu count, 4)` by default, raised by a plugin
via `request_threads_at_least` or overridden by `[runtime] threads` /
`LUMEN_THREADS`) and is read once, at the first tick. The CPU renderer's
rasterizing threads are not part of that budget: vello_cpu runs its own pool,
sized from the frame (see lumen-render-cpu above).

## Windows

An app has one or more windows, and each is an entity in the main world.
On desktop, `App::new` spawns the primary one, marked `PrimaryWindow`; spawning another
entity with a `Window` component opens a window, and despawning it closes the
window. `Window` is what the app wants: title, size, kind, visibility,
decorations, position, size limits, an owner window, and whether it is
maximized or fullscreen. `WindowViewport` is what the window has, and
`WindowRuntime` is what the platform reports back: focus, occlusion,
minimized, maximized and fullscreen state, position, and monitor.
App code writes `Window`; after each tick the window backend hands what changed
to the platform window and writes back what the platform settled on, including
a resize, maximize or fullscreen change the user made. A headless run installs
`HeadlessWindowPlugin` in the backend's place, which applies `Window` straight
to the viewport and places windows on a virtual desktop; in its Wayland mode
(`HeadlessDesktop { positioning: false, .. }`) it places windows itself and
reports no position. The `Monitors` resource lists the displays; positions are physical
desktop pixels.

A drag that crosses windows goes through one seam in `lumen_core::surface_drag`.
The dragging side (the dock module tearing a tab off) writes
`SurfaceDragRequest`s: `Begin` in the window the press happened in, `Follow` to
make a window move with the pointer, and `End`. The window backend writes
`SurfaceDragEvent`s: `Over` and `Dropped` name the app window under the pointer
(`None` for none) with a position in its logical pixels, and `Cancelled` says
the platform ended the drag. A backend that carries the seam inserts
`SurfaceDragSupport`, whose `follow` says whether a window can follow the
pointer. Where the backend sees every pointer move of the drag itself (the
headless stand-in; Windows, macOS and X11, where the pressed window keeps
receiving moves past its edge), `InProcessDrag` turns each move into the event
and the following window's place from the windows' desktop rectangles. The
winit backend's side is `lumen-window-winit-drag`, which the
`window-winit-drag` capability registers: on Windows, macOS and X11 it tracks
the drag from the pressed window's moves, and on Wayland it runs a
`wl_data_device` drag with a private MIME type on winit's own connection, moves
the window with `xdg_toplevel_drag_v1` where the compositor offers it, and
reports `follow: false` where it does not. A link that leaves the capability
out carries none of it; the backend then inserts no `SurfaceDragSupport` and a
drag stays inside its window. The headless stand-in carries the seam on its
virtual desktop; `toplevel_drag` emulates the protocol in its Wayland mode.
Content dragged to or from other applications is the os-dnd capability's, not
this seam's.

A capability that needs the platform's own window events and the event loop
itself, as that drag does, rides the winit backend as a
`lumen_window_winit::extension::LoopExtension` registered with
`add_loop_extension` when it installs. The backend calls every extension at
the same points (the loop resuming, each window event, before the loop waits,
a window opening and closing) and wakes the loop when one asks to; it names
none of them.

Windows do not own trees. There is one document, and an element shows in a
secondary window when it carries `WindowOf(window)`; it is then a surface
root. It stays where it is in the document, so queries, selectors, signals,
inherited styles, and event bubbling see it under its parent as before. It
leaves its parent's layout and paint instead: layout solves it as a root
against its window's viewport, and it paints in that window, outside every
ancestor's clip, scroll offset, opacity group, and visibility. The
`WindowPartitions` resource answers which window an element is in. The
window core keeps it, with the rest of the per-window bookkeeping, in one
system that runs before layout and only while a surface root exists or one
was just added or removed, so an app with one window never runs it. The
browser runtime does not install the window core at all.

Input follows the same split. Hit testing considers only the elements in the
window under the pointer (`PointerState::window`), but the hover chain that
puts `:hover` on an element's ancestors, and event bubbling, run up the one
tree across window boundaries, the way they would in one browser document.
Every entity with a `Transform` is a hit candidate; the hit test walks the
tree once from its roots, carrying scroll offsets, clips, `pointer-events`
and the hidden and disabled state down, and skips a subtree whole when it is
hidden, disabled or clipped away at the pointer. Among the boxes under the
pointer the one painted last wins, in the paint order the extract uses (top
layer first, then document order with `z-index` among siblings). It carries
`Hovered`, the element a press went down on carries `Pressed`, and the chain
markers `HoverWithin` and `PressWithin` sit on those and their ancestors;
widget code that needs the control under the pointer walks up from the
target. Media
queries evaluate per window: an element matches against its own window's
viewport. One element has focus app-wide; each window remembers the element it
last had in `LastFocus` and gets it back when it regains focus, focusing an
element in a window that lacks focus raises that window, and Tab cycles within
the focused window. A window carrying `InputBlocked` ignores input and refuses
accessibility actions, simulated MCP input included. Dropdown panels, menus,
and the color picker are in the tree under the element that opens them, so
they show in its window and clamp to its viewport. A tooltip, which is
spawned at the top of the tree, gets `WindowOf` from its trigger's window and
opens there.

Closing goes through one path. The platform close button,
`WindowRequest::Close(window)`, and `QuitRequest` all run a `CloseRequest`
veto tick carrying the window being closed. A window about to close announces
it with `WindowClosing` a tick ahead of its despawn, so whatever put content
in it can take that content back; content still under it despawns with it.
Closing the primary window quits the app, unless `[app] quit = "explicit"`
makes it hide every window and keep running until the app quits.

The `window.title()` and `window.size()` script getters read a process-wide
mirror of what each window has, since a script host has no world to read. They
answer for the window of the event being handled, and for the primary window
outside a handler.

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
on render-relevant components, from the property store's notify queue, and by
content that reaches the tree outside both, such as a decoded asset arriving
from a worker thread; the window backend clears it once it has presented a
frame. A separate per-tick flag is raised while a driver still has work in
flight, an animation mid-tween or an element still waiting on content that has
not arrived, so the window backend schedules a follow-up frame instead of
parking on it. Neither flag spins at rest: the second is cleared at the start
of every tick and only re-raised by a driver that still has work left, and a
driver keys its claim on something in the tree rather than on its own
bookkeeping, so the claim dies with that thing even when the work behind it
never finishes.

Work that comes due at a known instant rather than now, such as a script
timer, asks for a wake deadline instead (`WakeDeadline` in `lumen_core::tick`).
The deadline is cleared at the start of every tick and re-requested by each
driver whose deadline is still outstanding; the earliest request wins. An idle
loop sleeps until it (the windowed backend through `ControlFlow::WaitUntil`,
the headless loop through a timed park) and ticks once it passes, so a pending
timer neither polls nor waits for unrelated input.

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

The chain, the dirty roll-up that gates it, and the `Prepare` systems below
are installed by the render backend through `install_extract_pipeline`, not by
`App::new`. The window backend and `install_offscreen`, which puts a renderer
drawing offscreen into an app, each ask for it, and asking twice is a no-op. An app with no render
backend never asks: the browser runtime and a server render tick the main
world and stop, and their builds link none of the extract step.

Iteration order has to be deterministic. Paint order is derived from document
order, z-index, and entity identity, never from the order archetypes happen to
iterate; without that, adding and removing hover and press markers reshuffles
archetypes and the wrong things draw on top.

Extract runs once per frame for every window. Each window has a
`RenderWindow` entity in the render world holding its own retained tree, the
tree its last frame painted, and its `WindowViewport`. When an app has surface
roots, every drawable from an element in a secondary window is tagged with
that window (`ExtractedWindow`) after the chain runs, and the rest belong to
the primary window, so a single-window app pays nothing for the split.

The `Prepare` stage then culls each window's drawables against that window's
viewport: entities outside it, and entities
whose main-world entity is hidden by a `Visible(false)` on itself or an
ancestor. What survives is folded into one retained node tree per window, a typed scene
graph with container, transform, opacity, clip, rect, shadow, outline, text,
image, and native variants. An opacity node is a group: its subtree paints into
a layer that fades once, which is how CSS `opacity` on an element with children
reaches the renderer. The variants map one to one onto Qt's scene graph
and GTK's GSK render nodes, so a renderer backend only has to translate each
variant to its native equivalent. Children are shared behind reference counts,
so an unchanged subtree compares equal by pointer and the frame diff
short-circuits. The native variant is the seam a plugin paints its own pixels
through; see
[Painting your own pixels](plugins.md#painting-your-own-pixels).

## From markup to entities

The front end lives in `lumenc` and the runtime links none of it. The runtime
declares a `SourceParser` trait and whoever drives it from source (the CLI, the
Rust SDK, the C ABI development path) hands over an implementation. That
inversion exists because `lumenc` depends on `lumen-runtime` for its `run`
subcommand; a direct parser dependency the other way would be a cycle.

The load path:

1. Parse `src/main.lmn` into the layout IR, splicing `<include>` directives,
   and parse `src/main.css`, resolving `@import`.
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
that runs each part, the page set of a multi-page app, the fragment table, and
the app's translation catalogues with their fallback chain; a version the
runtime does not recognise is rejected before decoding. The catalogues travel
as Fluent source rather than as parsed bundles, and a loose `locale/<tag>.ftl`
beside the app wins over the copy inside it, so the desktop runtime loads the
directory first and fills in only the locales it did not find there. The engine split and
the page set are recorded rather than rediscovered, because a shipped app has
neither script files nor page files left to read them off. Pages are assembled
at compile time exactly as the from-source path assembles them, so the IR
already holds every page behind its gate and only the routing data travels
separately. Relative asset paths in the IR resolve against the directory the
artifact is run with, so a packaged app finds its files wherever it is copied.
A runtime built without the source-load path can run only artifacts, and links
no parser at all.

Styling is re-resolved after spawn, from scratch. A theme flip, a media-query
change or a root class change bumps a style version, which restyles the whole
document; a class, id, inline-style or disabled change on one element marks it
`StyleDirty`, which restyles its subtree, and a tree edit marks the element
whose children changed the same way. An edit that leaves the element as it
was (a class it already has, an inline value it already holds) marks nothing.
The restyle
walks top-down with `lumen_ir::css::RestyleWalk`: each element is resolved
from its tag, classes, id and place in the tree, with its markup declarations
(the `MarkupStyle` component, recorded by the parser for what an author wrote
and for the geometry of the widgets it builds) and its inline style ranking
above every rule, so it comes out as the first cascade made it. Every style
component is then rebuilt by `lumen_scene::computed::ComputedStyle`, the same
computation spawning runs, and a property nothing sets any more returns to its
initial value. A tag's defaults live in the user-agent sheet for the same
reason: the parser writes no style values of its own into the IR.

Runtime systems that own a style value while they run (a hidden `<if>`'s
`display`, an open popup's `inset`, a virtualized row's slot, a progress
fill's width, a toggle's checked fill, the hover and press tints, the state
patches) hold it in the `StyleOverrides` component instead of writing the live
component behind the cascade's back. Holders stack by rank; a restyle records
the new resting values there and keeps the held ones on top, and a holder that
lets go lands on the element's styles as they stand then. Interaction
pseudo-classes reach that layer through state components: the cascade lowers
`:hover`, `:focus`, `:active`, and `:disabled` into them, and a system swaps
between the stored variants.

`<for>` and `<if>` are not resolved at spawn time. They stay as markers that
reconcilers in the `Systems` stage keep in sync with the data behind them.
`<for>` keeps a row whose key survives a write: when its record changed, the
row's placeholders are resolved again and written onto the entities it
already has, so focus, scroll, and animation state stay with it.
`<if>` has two policies: rebuild the subtree on each transition, or mount it
once and toggle visibility, which preserves focus, scroll position, and
per-row state.

Custom tags come from the widget registry. `#[derive(Widget)]` registers the
tag string at startup, and the markup parser consults the registry after its
built-in tag table misses, so an unknown tag is still an error but a registered
one is not. The parser keeps a custom element's attributes as written, and the
spawner, once it has built the element, hands it and those attributes to the
`Widget::spawn` registered for the tag, which inserts the widget's component.

## Layout, text, and rendering

Layout runs in `LayoutSync`. Change filters on styles, children, text, images,
and writing direction mark entities dirty; dirtiness propagates up to the
nearest relayout boundary; taffy computes the tree from each topmost dirty
element with a measure function that calls the text shaper for intrinsic sizes;
and the results are written back as absolute coordinates. A dirty boundary
with a dirty ancestor lays out in that ancestor's solve; only a boundary whose
ancestors are all clean solves on its own, in the box it had before. The taffy tree is a non-send resource, because taffy's
compact length representation stores a raw pointer and is neither `Send` nor
`Sync`.

taffy measures leaves only, so an element whose own text sits beside element
children gets an anonymous text box: a taffy leaf with no entity, measured from
the element's text and placed first among its children, as CSS does. The
engine writes where it landed as the element's `AnonymousTextBox`, and text
extraction paints the run in that box instead of the content box. A leaf
carries no such component and takes the unchanged path. Editable fields never
get one, and neither does text that is only white space, which CSS does not
render beside children.

The engine is a `LayoutEngine` from the `LayoutBackends` registry in
`lumen-core`. Its capability crate registers it in the `Backends` phase, before
the core stack is installed, and the runtime installs the highest-priority
engine where the stack has always put layout. The engine contributes systems
rather than being called per node: layout is change-driven and its state is
usually not `Send`, so the solve runs as ordinary systems the scheduler
orders. The systems that write each entity's `Transform` go in the
`LayoutSolve` set, and code that reads this tick's boxes, such as the caret
keep-visible pass, orders itself after that set without naming an engine.

Text shaping goes through the `TextShaper` trait: a string, a pixel size, and
shaping options in, a shaped run out, segmented by font and bidi level.
Measuring is on the same trait, so a text leaf's intrinsic size and its
painted glyphs always come from one backend; the default measure shapes the
run and reports its width, height, and first-line baseline, and a backend that
can answer more cheaply overrides it. The cosmic-text implementation caches
results, since the same label reshapes every frame otherwise.

The app holds one shaper per world, as the non-send `ShaperService` resource.
Layout, the editing systems, and the caret pass read the main world's; the
renderer reads the render world's. Replacing either from an app hook changes
what that half does. A build that installs none gets `NullShaper`, which shapes
nothing and measures every run as an empty box, and a renderer with no shaper
paints no text.

Rendering walks the retained node tree. The walker in `lumen-paint` turns each
node into calls on a `Painter`, the immediate-mode trait every render backend
implements over its own draw target: the GPU backend's sink encodes into a
vello scene, the CPU backend's draws into a vello_cpu context. A sink that can
record work offers fragments, and the walker then encodes each repeated
appearance once and replays it at every position; the GPU sink does, the CPU
sink paints every leaf in place. A sink that can paint offscreen offers
layers, and both do: the walker paints an opacity group into one, keeps it in
the renderer's layer cache under the faded element's identity, and while the
group paints the same as when it was kept, composites the kept pixels at the
group's current alpha instead of painting its subtree. A group is kept once it
has held still for a frame, a scale or size change drops every kept layer, a
group holding a native leaf is never kept, and the cache holds at most a fixed
budget of pixels; a group it does not keep paints directly. The GPU sink
renders the layers a frame closes packed into shared textures, one vello pass
per texture rather than one per layer. Every image a vello pass samples is
copied into one image atlas, 8192 pixels square, so the GPU renderer keeps
each pass inside it. A frame whose images would not fit together, a paint
program's stack of large canvas buffers say, is painted a second time in split
mode: an image drawn at a fraction of its size draws through a copy the size
of the pixels it covers, kept across frames while the draw repeats, and a
layer whose content outgrows a pass renders what it holds so far and carries
on from that, so every image draws and opacity, blend modes and masks compose
as in one pass. A frame that fits renders as one pass. The GPU renderer also
keeps the atlas through a pass that samples no image at all, which vello would
otherwise empty. A pixel image wider or taller than the atlas, or than the
device's texture limit when that is smaller, is not drawn, and the renderer
says so once.
Text is the one shape the walker does
not leave to each rasterizer's own curve flattening, because two rasterizers
flattening a glyph's curves their own ways draw visibly different edges at
text sizes. `GlyphOutlines` in `lumen-paint` reads each glyph's outline,
flattens it once per glyph, size, and variation instance in target pixels,
and caches the polygons; both sinks fill those same polygons, the GPU sink
keeping them pre-encoded for vello, and text inside an offscreen layer fills
them through the layer's transform like any other shape. Colour glyphs (COLR
and bitmap faces) are the exception and draw through each backend's own glyph
renderer. The frame logic around the walk is shared
too, in `lumen-paint`'s `frame` module: both backends diff the tree against
the previous frame's, skip the frame entirely when nothing visible changed
(leaving the last one on screen), and answer a screenshot request from the
frame they just painted.

A frame another renderer drew reaches the walker as a backend texture:
`Painter::draw_texture` with a `TextureId`. A sink that cannot show it draws
the fallback pixels, if any, and says why through `TextureRefusal`. The GPU
sink shows a texture beneath the frame rather than in it: it cuts a hole
through the frame where the texture lands, inside whatever clips are open,
and records the placement. A compose pass then draws each placed texture
into the window and the frame over it, so the texture is never copied into
the vello scene and content painted over it still draws on top. The texture
is shown as it is: inside a kept layer, under an opacity, a mask or a blend
mode, or through a rotation or flip, the sink refuses it. When only the
textures changed, the renderer composes again without walking the tree, and
`FrameDirty::present` lets a plugin ask for that frame without dirtying the
tree. The textures live in `GpuTextures`, a table keyed by `TextureId` with a
callback per texture that fires once the GPU is done sampling the one it
replaced; it and `GpuContext`, the renderer's device and queue, are render
world resources the renderer inserts through `Renderer::publish` once a
target is attached. `GpuContext::generation` changes when the device does, so
a holder of device resources can tell its textures are gone.

Which backend renders is the app's choice, `[render] backend` in `lumen.toml`.
Each backend registers itself into the `RenderBackends` registry in
`lumen-core` from its capability crate while the app builds, under a name and
a priority, with a constructor for its renderer. The runtime asks the registry
for the configured name, or under `auto` for every backend by priority, and
binds the first renderer that starts, so a machine with no usable GPU falls
through to the CPU. The core and the runtime name neither backend. A static
package links only the backends the app's setting selects, through the
capability's `Select::OnConfig` rule; see
[optional subsystems](plugins.md#optional-subsystems).

Each backend has one renderer, behind the `Renderer` trait, and a window and
an offscreen image are two kinds of target it attaches to. Targets are keyed
by the main-world window entity, and one renderer holds every window's target
on one device. The window backend attaches each window as it opens, reports
resizes, asks for a frame per window, and detaches a window that closed; a
headless run attaches an offscreen image per window and hands the renderer to
`install_offscreen`, whose render-world system keeps a target for every window,
resizes each to its viewport, and asks for frames the same way. Each window
presents only when its own tree changed. The focused window presents in step
with the display, and the others present without waiting on it, so one window
never stalls another. Everything from the retained tree to the pixels stays on the
renderer's side of that line, so no scene or device type is ever named by the
caller, and a headless run exercises the same device bring-up, frame gate,
walk, and encode a windowed app ships. Vello's compute pipeline pins its render
target to a linear RGBA format, while most swap chains expose a BGRA sRGB
surface, so the GPU renderer always draws into a texture of the required
format; a window composes that onto the surface, with any backend textures
beneath it, and an offscreen target reads the composed image back. Which GPU backend is compiled is decided at the manifest level, one per
operating system. The CPU renderer hands its rasterized frame to softbuffer,
the platform's own path for showing a CPU buffer in a window.

The window backend comes from the `WindowBackends` registry the same way. An
interactive launch hands the highest-priority backend the built app, the
window options, the renderers to try in order, and the accessibility bridge
factory, through `WindowBackend::run`, once for the whole run. The
window-free half every window shares, `WindowCorePlugin` (the focus,
occlusion, and IME messages, the `RedrawScheduler`, and the extract
pipeline), lives in `lumen-core`, so a headless run installs it alone and
reads the same window state with no window open.

Accessibility splits along the same line. The world-side half walks the tree
once per tick in `A11ySync` and leaves an update behind for each window; the
platform half, behind the `A11yBackend` trait, binds one adapter per window,
publishes that window's update, and carries requests the other way. The
primary window's tree is the document minus the content of other windows; a
secondary window's tree is rooted at a window node named by its title and
holds its surface roots. A window opens hidden and is shown only once its
adapter is bound, the order the platform adapters require. Screen readers arrive on their own threads, so those requests are queued
and applied on the main thread, in the tick that paints their result.

## Pluggable backends

Every backend role is a trait, and the trait lives away from any
implementation of it. The traits in `lumen-core` name the roles: renderer,
layout engine, window backend, accessibility bridge, task spawner, timer. Each
declares what a caller drives it with, which is what lets one window backend
drive any renderer and any accessibility bridge, and lets the runtime install a
layout engine and run a window backend it never names. The renderer, layout
engine, and window backend register into one registry shape in
`lumen_core::backends`: a name, a priority, and a constructor. The shaping
trait lives in `lumen-text`, the scripting trait in `lumen-script`, the parser
trait in `lumen-runtime`.

An implementation crate depends on the trait crate and ships a plugin that
installs itself. Nothing depends on an implementation crate except the assembly
layer that chooses one, which is why the CPU and GPU renderers are
interchangeable behind the same render stage, and why a shaper or a script
host can be swapped the same way.

A consumer reaches the installed implementation through a resource rather than
by naming a crate: `ShaperService` for text, `SpawnService` and `TimerService`
for async work. Absence is part of the contract. An app that installs no async
backend leaves both async resources unset, and a consumer reads that as "do it
inline": that is why a file dialog still opens in a build with no executor,
blocking the tick it was opened on.

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
hover, is per host: each host crate keeps a table of the builtins it registers,
spelled the way its language names their types, and candela's suite checks its
table against what it binds.

The trait covers lifecycle (compile check, load, replace, reset), invocation
(call a function, call a closure, evaluate a derivation), a command sink, a
signal mirror, the handler and derivation registries, dynamic-DOM event
dispatch, and metadata. A host must implement lifecycle, `call`,
`call_closure`, `register_script_fn`, and `lang`; the command sink, the mirror,
the registries, and the builtin table default to empty, so a minimal host or a
test double writes only what it uses. Its associated closure type is what each engine calls a
callable: a function pointer in Rhai, a function value in Lua, and a function
*name* in candela. candela has function values of its own, but the values that
cross a host-function boundary are the marshalled ones (null, the scalars,
arrays, maps, and enums), and a function is not among them, so a candela script
names the callable and the host looks it up.

One host runs per language the app ships. Which module runs which language is
data: each host module carries a `lumen-language.toml` descriptor naming its
language, the extensions it owns, the form it runs (`source` or `bytecode`),
and whether it is the default for an inline `<script>`
(`core/modules/src/language.rs` reads them; the toolchain ships them under
`bin/modules/<name>/`, a checkout reads them from `std/`). Each script file
belongs to the language its extension names, and the files of one language
concatenate into a single program. An inline `<script>` has no extension to
read, so it joins the app's one external language when there is exactly one and
falls to the default language otherwise.

`build_app` reads the `<script>` elements before the parse, loads the module
that runs each language as an implied `[dependencies]` entry (the source form
from source, so a run can reload; the module a compiled artifact names
otherwise), then installs each part of the program through the language its
module registered. A language with no loaded module is a banner and the app
carries on. `lumenc check` and `build` load the same modules into a scratch
app and call the language's check and compile entries; a language with a
bytecode form ships the bytecode and drops the source.

DOM events reach the hosts through one dispatcher: each event is read once,
walks its propagation path once, and a handler token is offered to each host
until the one that owns it runs it, so a native handler fires once however
many languages the app runs.

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
one language, whatever the extensions say. A precompiled artifact carries the
per-language split the compiler recorded, with the module that runs each part,
and is the only source of it, since an app's script files do not travel beside
its compiled form.

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

The binding readers read the dirty queue through `PropertyStore::observe_dirty`,
which records where in the queue the earliest of them looked. The end of a
tick forgets the writes the readers saw and keeps the ones that landed after,
from a system the schedule ran after the readers, so the next tick applies
them; `work_pending` reports the kept writes, so a loop that parks once a tick
leaves nothing behind runs that tick. A DOM mutation queued from the C ABI or an
SDK after the tick's DOM commands were collected asks for the next tick the
same way. Either reaches the screen one tick later, without waiting for input.

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
  `raw-window-handle` is on the allowed side of that line: it is the handle
  vocabulary a window backend and a renderer use to describe a window to each
  other, and it contains no platform code.
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
