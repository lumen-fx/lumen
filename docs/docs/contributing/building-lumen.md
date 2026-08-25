# Building Lumen

How to get a Lumen checkout compiling, running, and passing the same gates CI
runs. For the contribution policy (issues, pull requests, the CLA, the
invariants you must not break), read `CONTRIBUTING.md` at the repository root.

## Toolchain

The repository pins its Rust toolchain in `rust-toolchain.toml`. Install
`rustup` and let it resolve the pin; `cargo` and `rustc` in a checkout then use
the pinned channel with `rustfmt` and `clippy` already attached.

The pin exists so `cargo fmt --check` produces the same answer everywhere. A
different toolchain reformats files that are already clean and turns the format
gate red. Bump the pin deliberately, on its own, never as a side effect of
another change.

The workspace targets Rust 2024 and declares a minimum supported Rust version
in `Cargo.toml` under `[workspace.package]`.

## System dependencies

On Linux the workspace links against a handful of system libraries. The
authoritative list is `.github/scripts/linux-deps.sh`, which CI runs verbatim:

- `pkg-config` to resolve the rest.
- `libgtk-3-dev` for the GTK file dialog behind `lumen-os-filedialog`.
- `libasound2-dev` for ALSA, reached through cpal under `lumen-audio-rodio`.
- `libxkbcommon-dev` and `libwayland-dev` for keyboard and Wayland handling
  under winit.
- `libvulkan1` for the Vulkan loader, since wgpu builds the Vulkan backend on
  Linux.
- `mesa-vulkan-drivers` for the lavapipe software device. Without a Vulkan
  device the renderer tests skip instead of running.

macOS and Windows need no system packages beyond the Rust toolchain. The
renderer selects Metal on macOS and DX12 on Windows, both of which ship with
the OS.

Building the Windows installer additionally needs WiX; the release workflow
installs it on the runner.

## Build and run

Build everything:

```sh
cargo build --workspace
```

Run one of the example apps in `apps/` through the CLI:

```sh
cargo run -p lumenc -- run apps/widget-garden
```

`apps/widget-garden` exercises every tag, attribute, and OS builtin, so it is
the fastest way to see whether a change broke something visible. The other
apps are narrower: `apps/scroll-tiles` and `apps/notes` for basic markup and
scripting, `apps/kanban` for drag and drop, `apps/pages-demo` for multi-page
navigation, `apps/music` for audio.

The build also writes `target/<profile>/libs`, the candela standard library
that `lumen-script-candela`'s build script stages out of the candela source
cargo resolved, with the C-backed modules built by the C compiler cargo already
uses for the other native dependencies. candela reads the tree from beside the
running executable, so it goes there rather than into `OUT_DIR`, and the
release archive ships the same tree next to `lumenc`.

The apps `lumenc new` scaffolds are maintained outside this repository, one per
template under [lumen-fx](https://github.com/lumen-fx), and a release ships a
copy of every one beside the toolchain. Download them for a local run with:

```sh
tools/fetch-templates.sh
```

They land at the root of cargo's target directory, where `lumenc` finds them
the way an installed copy finds the ones next to it. Until they are there,
`lumenc new` says so and every test that scaffolds an app skips itself with a
printed reason; CI fetches them before it tests.

## Developing without a window

Add `--headless` to run the whole pipeline (layout, shaping, GPU render, MCP
server) with no window at all, and `--ticks N` to run a fixed number of ticks
and exit:

```sh
cargo run -p lumenc -- run apps/widget-garden --headless --ticks 5
```

Headless is the mode to use for automation, for CI, and on machines with no
display. `--size` and `--dpr` set the offscreen surface geometry and only apply
together with `--headless`.

`lumenc check <dir>` parses markup and CSS and reports diagnostics without
running anything, which is the quickest gate while editing an app.

The same mode is what app authors use for automated testing; see
[Testing](../guides/testing.md).

## Building for the browser

`crates/web-runtime` builds the wasm module a Lumen page loads. It needs the
`wasm32-unknown-unknown` target, which `rust-toolchain.toml` lists, so `rustup`
installs it with the rest of the toolchain.

The module carries one script host per `host-<engine>` feature, and the default
build carries candela (`host-candela`). It is the only host that runs in a
browser today: rhai is not wired up for this target yet, and lua's C core does
not build for `wasm32-unknown-unknown` at all. An app names its engine in the
manifest, so a module built without a host for that engine refuses to boot the
app and says which engine it was asked for.

Two more tools, neither a cargo dependency:

- `wasm-bindgen-cli`, which turns the raw module into one a browser can import
  and generates the JavaScript glue beside it. Its version must match the
  `wasm-bindgen` version the workspace pins, or it refuses the module:
  `cargo install wasm-bindgen-cli --version <the pinned version>`.
- `wasm-opt`, from a [binaryen](https://github.com/WebAssembly/binaryen)
  release. Optional locally; CI runs it, and the size gate measures its output.

Build the module, generate the bindings, and shrink it:

```sh
cargo build -p lumen-web-runtime --target wasm32-unknown-unknown --profile wasm-release
wasm-bindgen --target web --no-typescript --out-dir web-dist \
  target/wasm32-unknown-unknown/wasm-release/lumen_web_runtime.wasm
wasm-opt -Oz --enable-bulk-memory --enable-bulk-memory-opt \
  --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals \
  --enable-reference-types --enable-multivalue \
  web-dist/lumen_web_runtime_bg.wasm -o web-dist/lumen_web_runtime_bg.wasm
```

`wasm-opt` validates against the features it is told about, and the ones rustc
emits for this target are not all on by default, which is what the long flag
list is for.

The `wasm-release` profile is where the size settings live. A page downloads
this module, so it optimises for size over speed and aborts on panic, which
wasm32 does anyway for want of an unwinder. CI records the byte size on every
run and fails when it passes the budget named in `.github/workflows/ci.yml`.

### Running the browser tests

The browser runtime's tests run in a real browser, driven over WebDriver. A DOM
shim is not an option: this target exists because the browser is the layout
engine, so a fake one would pass on things a page rejects.

Install Chrome and a `chromedriver` of the same major version, then:

```sh
cargo test -p lumen-web-runtime --target wasm32-unknown-unknown
```

`.cargo/config.toml` sets the runner that makes that work: a `.wasm` file is
not executable, so cargo hands it to `wasm-bindgen-test-runner`, which serves
it to the browser. Set `CHROMEDRIVER` to the binary if it is not on `PATH`.

The same config file carries the `getrandom` backend flag every wasm build of
the workspace needs. `getrandom` has no default for a target that names no
operating system, so it asks to be told, and the flag points it at the
browser's crypto interface.

## Debug info in dev builds

Dev builds keep line tables for the workspace crates, so panic backtraces
name the file and line, and compile dependencies with no debug info at all.
This keeps the target directory to a fraction of what full debug info costs
across a dependency tree this size. To step through code in a debugger with
full variable information, override the profile for that one session:

```sh
CARGO_PROFILE_DEV_DEBUG=true cargo build -p <crate>
```

## Feature flags that matter

A handful of crates carry flags you will meet while working on the tree.

**`lumenc`** builds in two shapes. The default shape statically links
`lumen-runtime` (feature `dev-run`) so `run`, `build`, `check`, and the
integration tests drive an app in process. The thin shape drops that and loads
the shared `liblumen` over the C ABI instead:

```sh
cargo build -p lumenc --no-default-features --features "runtime-parse,dlopen-run"
```

`runtime-parse` compiles the markup and CSS front end into the compiler; a
build without it consumes only precompiled artifacts. `bundle` adds the `.lpak`
packer. `devtools` is off by default and compiles the in-window overlay.
`profiling` and `profiling-tracy` add the `--profile` backends.

Dropping every default feature yields a compiler library with no backends at
all. That is what `lumen-lsp` links, which is why the LSP does not pull wgpu,
winit, cosmic-text, or taffy.

**`lumen-lsp`** has one flag, `lang-rhai`, on by default. It carries the Rhai
engine and builtin table the server analyses `.rhai` buffers with. Markup, CSS,
and the cross-file id features do not depend on it, so a server built without
it still serves those.

**`lumenui`** (the Rust SDK) has `host-rhai`, on by default. It gates
`AppBuilder::rhai_extension`, which takes a `rhai::Engine` and so reaches one
host. `AppBuilder::native_fn` registers into every host and is always
available.

**`lumen-runtime`** defaults to every subsystem on: `audio-rodio`, `mcp`,
`async`, `host-rhai`, `host-lua`, `host-candela`, `http-fetch`,
`runtime-parse`. Each script host is its own feature, so a build can carry
exactly the languages its app ships. Per-app trimming happens only on the
static bundle path, where `lumenc` selects the exact feature set an app needs;
the development path stays full featured.

**`lumen-script-candela`** has `compiler`, on by default. It carries the
candela compiler, and with it source compilation, hot reload, `lumenc check`,
and `compile_bytecode`, the build step that produces a `.cdlb` image. Off, the
crate keeps the whole builtin surface and the host that runs such an image, and
the compiler front end leaves the dependency graph. That is what the browser
runtime builds against.

`audio` compiles the audio subsystem, and `audio-rodio` adds the playback
backend Lumen ships. Selecting `audio` alone gives a build whose transport
works and makes no sound, which is where an embedder supplying its own
`AudioBackend` starts.

`http-fetch` adds the HTTP client behind the scripts' `fetch()` and `http()`
builtins, and costs about a megabyte of release text for the TLS stack. A build
without it still parses and queues both calls, and answers every request with an
error naming the missing feature; that is where an embedder supplying its own
`lumen_script::HttpClient` starts.

**`lumen-render-wgpu`** selects one wgpu backend per operating system through
target-scoped dependencies, so a build compiles the Vulkan, Metal, or DX12
backend and nothing else. The off-by-default `gl-fallback` feature adds the
OpenGL backend for GL-only virtual machines and Vulkan-less containers.

## Gates

CI runs these, and they are the same commands to run locally before opening a
pull request:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Format and clippy run on Linux only, since neither depends on the platform.
Tests run on Linux, macOS, and Windows. That three-way matrix is also the
release parity check, so a red macOS or Windows leg is a portability gap to
fix, not a platform to drop.

The suite also runs every app the repository ships. Each directory under
`apps/` and `fixtures/`, and each app `lumenc new` scaffolds, is run headless
for a few ticks and has to exit clean with nothing on stderr beyond the lines
a runner is expected to print. A silent failure this catches: when a script
fails to compile the runtime prints a banner and keeps going with every
handler disabled, so the process still exits zero. `apps/sysmon` is a CMake
project rather than a markup app, and the suite asserts that the markup runner
turns it away. These cases run on Linux, since the set of lines a clean run
prints is what makes the check sharp and that set is per environment; the app
sources are the same everywhere.

Three families of test skip themselves rather than fail when the machine cannot
support them, printing the reason:

- Every case that scaffolds an app, when the templates have not been
  downloaded. `tools/fetch-templates.sh` is what they want; CI runs it before
  the suite.
- Framebuffer readback on a software adapter. Direct3D's WARP rasterizer faults
  the test process when a texture is read back, so those cases want a real GPU.
- The screenshot goldens in `crates/lumenc/tests/golden.rs`. Baselines carry
  the font set of the machine that captured them, and a machine that resolves a
  different default sans-serif redraws every case containing text. They run
  locally and skip when `CI` is set.

Useful targeted runs while working in one area:

```sh
cargo test -p lumen-render-headless --test golden_rects
cargo test -p lumen-render-wgpu --test smoke
cargo test -p lumen-layout-taffy --test dirty_invariant
```

Two more gates cover the browser target. The first keeps the crates the web
runtime shares with the desktop build compiling for the web, so a native-only
API reintroduced into one of them fails on the pull request that adds it:

```sh
cargo check --target wasm32-unknown-unknown \
  -p lumen-core -p lumen-ir -p lumen-html -p lumen-script -p lumen-script-candela
```

The second builds the module, measures it against the size budget, and runs the
browser suite; see [Building for the browser](#building-for-the-browser).

Coverage is measured on top of the same suite, with `cargo llvm-cov`, on every
push to `main` and every pull request, and reported to Codecov. It is a report
rather than a gate: what decides a pull request is the suite itself, and the project-wide Codecov status is informational, while the patch status
is a required check that fails when the diff drops meaningfully below the
baseline coverage. Doctests are outside the measurement, since collecting coverage
from them needs a nightly toolchain. The on-screen presentation path
(`lumen-render-wgpu`'s `surface.rs`) is left out of the report as well: it only
runs against a real GPU, and the tests that reach it skip themselves on a
runner. `codecov.yml` holds that exclusion.

The editor integrations under `tools/`, the release scripts, and the SDKs
build in a separate workflow, `tools.yml`, gated per directory so a change to
one tool runs one job. None of it needs `liblumen`. The JetBrains Plugin
Verifier is the exception to the per-pull-request rule: it downloads a full
IDE per version it checks, so it runs weekly and on demand.

Golden images are regenerated, not hand-edited. `UPDATE_GOLDENS=1` rewrites the
software rasterizer baseline in `lumen-render-headless`;
`LUMEN_GOLDEN_UPDATE=1` rewrites the screenshot baselines in `lumenc`. On a
mismatch the screenshot suite writes the actual and diff images under a
`lumen-golden-failures` directory inside `CARGO_TARGET_DIR`.

## Measuring how long an app takes to start

`lumen-portable` carries a benchmark for the sequence every host runs to start
an app: build the app, install its script host, spawn the tree, tick until the
state stops moving, read that state, drop the app. It matters wherever an app
is started more than once, such as once per rendered page.

It takes compiled apps rather than directories, because compiling is not part
of starting one:

```sh
cargo run -p lumenc --release -- build apps/tracker /tmp/tracker.lmna
cargo bench -p lumen-portable --bench boot -- /tmp/tracker.lmna
```

Each phase is reported on its own, since what a slow start costs is only
actionable if you know which phase is slow. The run is also reported in
tenths, so a cost that grows with the number of starts shows as a trend
instead of disappearing into one number. `--iterations` sets the sample count.

## Language server and editor extension

The language server is a normal workspace binary:

```sh
cargo build --release -p lumen-lsp
```

That produces `lumen-lsp` in the target directory. It links the compiler
library without the runtime, so it builds quickly and starts without touching a
GPU.

The VS Code extension lives in `tools/vscode-lumen` and is TypeScript:

```sh
cd tools/vscode-lumen
npm install
npm run compile
npm run package
```

`compile` emits the extension entry point; `package` produces a `.vsix` you can
install into VS Code directly.

The extension finds its binaries by searching the workspace target directories
(honoring `CARGO_TARGET_DIR`) and then `PATH`. Point it at a specific build
with the `lumen.serverPath` and `lumen.lumencPath` settings, or turn the search
off entirely. Its live preview drives `lumenc run --headless` and the
screenshot path, so it never opens a window either.

The JetBrains plugin lives in `tools/jetbrains-lumen` and is Kotlin on Gradle.
It needs JDK 21:

```sh
cd tools/jetbrains-lumen
./gradlew buildPlugin
./gradlew verifyPlugin
```

`buildPlugin` writes an installable zip to `build/distributions/`, and
`verifyPlugin` runs the JetBrains Plugin Verifier over it. `./gradlew runIde`
starts a sandbox IDE with the plugin loaded. The build copies the TextMate
grammars out of `tools/vscode-lumen`, so a grammar fix reaches both editors.

## Documentation

The documentation site is built with Zensical from `docs/`:

```sh
cd docs
uv run zensical build --strict
```

`--strict` turns broken links and unknown navigation entries into errors. The
navigation lives in `docs/zensical.toml`.

The test suite compiles the code blocks on those pages that are complete
programs, because a reader copies them verbatim. Two rules decide which blocks
those are:

- A candela block is a whole script when it carries the prelude import line
  `import "lumen.cdl";`. Every shipped script opens with it, so a block that
  has it is offering itself as something to copy, and it is compiled with
  `lumenc check`. That includes the `fn main() {}` candela requires; a block
  without one fails the gate.
- A markup block is a whole document when it opens `<root` and its last line
  closes it. The check writes a placeholder for every file its `src` attributes
  name, so what fails is the block rather than art a page cannot ship.

Anything else is an excerpt and is left alone. To show a fragment of a script,
leave the import line out and let the prose say where the lines go.

The Rust API documentation is separate from that site and comes from the crates
themselves:

```sh
cargo doc --workspace --no-deps
```

The same build is published on every push to `main` and serves the crate docs
for the current tree at [api.lumenfx.dev](https://api.lumenfx.dev).

Every change that adds, changes, or removes something a user can observe
updates the matching page in the same commit. That includes tags, attributes,
CSS properties, `lumen.toml` keys, CLI flags, scripting builtins, defaults, and
supported platforms.

## Third-party licences

`about.toml` holds the accepted-licence allowlist for the dependency graph.
Regenerate the attribution page with `cargo about`:

```sh
cargo about generate about.hbs > third-party-licenses.html
```

A licence that is not on the allowlist shows up as an error, which is the point:
a new dependency carrying an unexpected licence gets noticed before it ships.

## Where to go next

- [Architecture](architecture.md) for the crate map and how a frame is
  produced.
- [Writing plugins](plugins.md) for extending the runtime from Rust.
