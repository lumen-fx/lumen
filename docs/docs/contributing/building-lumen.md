# Building Lumen from source

This page is for people working on Lumen itself: fixing a bug in a crate,
adding a feature, or building the language server from a checkout. If you
just want to write and run Lumen apps, install the prebuilt toolchain
instead; see [Install](../getting-started/install.md). Everything below
needs a Rust toolchain and, on some platforms, a C toolchain and a few
system libraries.

## Toolchain

The workspace pins its Rust toolchain in `rust-toolchain.toml`
(currently 1.97.0, edition 2024). Install [rustup](https://rustup.rs) and
it picks up the pin automatically the first time you run `cargo` inside
the repo:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

The pin matters for `cargo fmt` and `cargo clippy`: their output depends on
the exact toolchain version, and CI runs the pinned one. A newer or older
toolchain can report different results than CI.

## System dependencies

Lumen links against a handful of OS libraries through winit (windowing),
wgpu (GPU), AccessKit (accessibility), `rfd` (native file dialogs), `muda`
(native menu bars), `rodio`/`cpal` (audio), and `notify-rust` (toasts).
This table is what a clean machine needs; it matches
`.github/scripts/linux-deps.sh`, the script CI runs before building on
Linux.

| Platform | Dependency | Required for | Install |
|---|---|---|---|
| Linux | pkg-config | resolves the rest | `sudo apt install pkg-config` |
| Linux | GTK 3 dev headers | `rfd`'s GTK3 file dialog (`lumen-os-filedialog`) | `sudo apt install libgtk-3-dev` |
| Linux | ALSA dev headers | audio via `cpal` under `rodio` (`lumen-audio`) | `sudo apt install libasound2-dev` |
| Linux | libxkbcommon dev headers | keyboard handling under winit | `sudo apt install libxkbcommon-dev` |
| Linux | libwayland dev headers | Wayland session support under winit | `sudo apt install libwayland-dev` |
| Linux | Vulkan loader + ICD | wgpu device init | `sudo apt install libvulkan1 mesa-vulkan-drivers` |
| Linux | libxdo-dev | native menu bars via `muda` (optional; Lumen builds without it on Linux, only macOS/Windows get menu bars) | `sudo apt install libxdo-dev` |
| Linux | libnotify | the `notify(...)` script builtin | `sudo apt install libnotify-bin` |
| Linux | a C toolchain | Rust's linker calls a C linker; also needed to build any app's `[[hooks]]` that compile C | `sudo apt install build-essential` |
| macOS | Xcode command-line tools | linker + Metal headers | `xcode-select --install` |
| Windows | Visual Studio Build Tools 2022 (C++ workload) | MSVC linker | <https://visualstudio.microsoft.com/downloads/> |
| Windows | DirectX 12 | wgpu DX12 backend | ships with Windows 10/11 |

Fedora and Arch equivalents for the Linux packages: `sudo dnf install
gtk3-devel pkgconf-pkg-config alsa-lib-devel libxkbcommon-devel
wayland-devel` / `sudo pacman -S gtk3 pkgconf alsa-lib libxkbcommon wayland`.

`mesa-vulkan-drivers` gives you `lavapipe`, a software Vulkan device. CI
runs on headless runners with no GPU, and this is the only adapter wgpu
finds there; it is also useful locally in a VM or container. A Linux
host with a real GPU still needs the Vulkan loader package
(`libvulkan1`) even if you have proprietary or Mesa GPU drivers already.

## Build and run from a checkout

```bash
git clone https://github.com/lumen-fx/lumen.git
cd lumen
cargo run -p lumenc -- new hello my-app
cargo run -p lumenc -- run my-app
```

The first build compiles the full stack in dev mode (wgpu, vello,
cosmic-text, taffy) and is slow; subsequent builds reuse the incremental
cache. For a faster inner loop, build once and put the release binary on
your PATH:

```bash
cargo install --path lumenc
lumenc run my-app
```

`cargo build --workspace` builds every crate in the workspace, which is
what CI does before running tests.

## Feature flags

`lumenc` and `lumen-runtime` are both cargo-feature-gated so a thin build
(the language server, or a size-trimmed release bundle) can drop backends
it does not need. Features marked "weak" (`crate?/feature`) only take
effect when something else has already pulled the crate in; they never
force it on by themselves.

### `lumenc` (`lumenc/Cargo.toml`)

Default features: `http-fetch`, `runtime-parse`, `dev-run`, `bundle`.

| Feature | Default | What it does |
|---|---|---|
| `runtime-parse` | on | Pulls in `roxmltree` and the markup/CSS front end (`parser_html`, `formatter`, `resolve`), so `lumenc` can compile `.lmn`/`.css` from source. Also forwards to `lumen-runtime`'s own `runtime-parse` (the from-source load + hot-reload code path). Turning it off builds a parser-free `lumenc` library that only consumes precompiled `.lmna` artifacts; `lumen-lsp` links `lumenc` this way. |
| `dev-run` | on | Links `lumen-runtime` statically (plus `rhai` and `lumen-window-winit`), and turns on the runtime's `audio`, `mcp`, `async`, `host-lua`, and `host-candela` features. This is what makes `lumenc run` / `build` / `check` run an app in-process. |
| `bundle` | on | Links `lumen-assets` so the `lumenc bundle` subcommand can pack an app into a `.lpak` archive or build a trimmed static runtime with `--static`. |
| `dlopen-run` | off | Builds a thin `lumenc` that compiles source to `.lmna` in-process (needs `runtime-parse`) and dlopens the shared `liblumen` cdylib over the C ABI to run it, instead of statically linking `lumen-runtime` via `dev-run`. This is the small-binary distribution shape: `cargo build -p lumenc --no-default-features --features "runtime-parse,dlopen-run"`. |
| `devtools` | off | Weak forward to `lumen-runtime/devtools`; needs `dev-run` already active. Links the in-window devtools overlay into `lumenc run`. |
| `http-fetch` | on | Weak forward to `lumen-runtime/http-fetch`. Compiles the scripts' `fetch(url, tag)` builtin. |
| `profiling` | off | Weak forward to `lumen-runtime/profiling`. Enables `lumenc run --profile chrome\|stderr`. |
| `profiling-tracy` | off | `profiling` plus a weak forward to `lumen-runtime/profiling-tracy`. Enables `--profile tracy`. |

### `lumen-runtime` (`lumen/runtime/Cargo.toml`)

Default features: `http-fetch`, `runtime-parse`, `audio`, `mcp`, `async`,
`host-lua`, `host-candela`. The default set is deliberately full, so the
shared `liblumen_ffi` cdylib and the `lumenc run` dev path both carry
every subsystem; per-app trimming happens only on `lumenc bundle
--static`, which builds `lumen-ffi` with a resolved, narrower feature set.

| Feature | Default | What it does |
|---|---|---|
| `audio` | on | Links `lumen-audio` (rodio/cpal/symphonia). |
| `mcp` | on | Links `lumen-mcp`, the JSON-RPC introspection server used by `lumenc mcp` and devtools tooling. |
| `async` | on | Links `lumen-async-tokio`, the spawn/timer bridge scripts and the async file-dialog path use. |
| `host-lua` | on | Links `lumen-script-lua` and `mlua`, adding the Lua script host alongside the always-on Rhai host. |
| `host-candela` | on | Links `lumen-script-candela`, adding the candela script host. |
| `devtools` | off | Links `lumen-devtools`, the in-window overlay. Off by default; a release or `--bundle` build never carries it. |
| `http-fetch` | on | Forwards to `lumen-script`, `lumen-script-rhai`, and weakly to `lumen-script-lua`/`lumen-script-candela`, compiling the scripts' HTTP `fetch()` builtin. |
| `runtime-parse` | on | No dependency of its own; gates the from-source load + hot-reload code path itself. |
| `profiling` | off | Adds `tracing-subscriber` and `tracing-chrome`, and turns on `bevy_ecs`'s `trace`/`debug` features, so every ECS system and schedule run emits a span. |
| `profiling-tracy` | off | `profiling` plus `tracing-tracy`, for the Tracy profiler. |

Rhai itself is not behind a feature; it is the always-compiled default
host. `host-lua` and `host-candela` add the other two.

### `lumen-render-wgpu` (`lumen/render-wgpu/Cargo.toml`)

| Feature | Default | What it does |
|---|---|---|
| `gl-fallback` | off | Re-adds the OpenGL/GLES backend (`wgpu/gles`) and an explicit GL adapter fallback in offscreen renderer setup. A shipped per-OS release otherwise compiles exactly one native backend (Vulkan on Linux, Metal on macOS, DX12 on Windows). Turn this on for an old GPU, a GL-only VM, or a Vulkan-less headless container: `cargo build -p lumen-render-wgpu --features gl-fallback`. |

## Tests and lint gates

CI (`.github/workflows/ci.yml`) runs three jobs; reproduce them locally
with the same commands:

```bash
# formatting
cargo fmt --all --check

# lints, denied as errors
cargo clippy --workspace --all-targets -- -D warnings

# build, then the full test suite
cargo build --workspace
cargo test --workspace --no-fail-fast
```

`cargo fmt` and `cargo clippy` are repo law; see `CONTRIBUTING.md`. The
test job runs on Linux, macOS, and Windows in CI; a couple of GPU-dependent
tests (framebuffer readback on a software adapter, and the screenshot
goldens, which are sensitive to the host's default font) detect a
runner-like environment and skip themselves with a printed reason rather
than failing.

The `lumen-benches` crate holds tests for runtime hot paths, including a
check that repeat text shaping is served from the cache rather than reshaped.
They run with the rest of the suite.

## Installing the language server from source

```bash
cargo install --path lumen/lsp
```

`lumen-lsp` links `lumenc` with `--no-default-features --features
runtime-parse`, so it gets the markup/CSS parser without the runtime and
its backends (wgpu, winit, vello, cosmic-text, taffy, rodio, mlua). Point
an editor's LSP client at the installed `lumen-lsp` binary; it speaks
stdio and negotiates its capabilities with the client.

## Crate layout

The workspace lives under `lumen/`, plus `lumenc`, `sdk/rust`, and
`benches` at the repo root. Rough map, for deciding where a change
belongs:

- **`lumen-core`**: the tick loop, ECS setup, command queue, and the
  backend traits (window, renderer, text shaper, layout engine, script
  host) that everything else implements. Never depends on an impl crate.
- **`lumen-ir`**: the IR shared between the compiler and the runtime -
  layout IR, the CSS AST and cascade, and the compiled `.lmna` artifact
  format.
- **`lumen-runtime`**: the run loop, default plugin stack, hot reload,
  file-based pages, and app loaders. Parser-free; `lumenc` injects a
  parser via `SourceParser`.
- **`lumenc`**: the markup/CSS compiler front end and the CLI (`new`,
  `run`, `build`, `check`, `bundle`, `fmt`, `lint`, `screenshot`, `mcp`).
- **Backend impl crates**: `lumen-render-wgpu` (wgpu/vello renderer),
  `lumen-render-headless` (in-memory RGBA renderer for CI/tests),
  `lumen-window-winit` (window + raw input), `lumen-layout-taffy`
  (layout), `lumen-text-cosmic` (text shaping), `lumen-a11y-accesskit`
  (accessibility tree), `lumen-audio` (playback), `lumen-input` (focus,
  hit-test, event dispatch).
- **Script hosts**: `lumen-script` (the `ScriptHost`/`StateProxy` trait),
  and the three implementations, `lumen-script-rhai`, `lumen-script-lua`,
  `lumen-script-candela`.
- **OS integration crates** (`lumen-os-*`): clipboard, drag-and-drop,
  file dialogs, tray, notifications, hotkeys, menu bars, launcher,
  lifecycle, power/inhibit. Each wraps one native subsystem behind a
  small trait, with `lumen-os-mime` holding types shared across several
  of them.
- **`lumen-widget` / `lumen-widget-macros`**: the `Widget` trait and the
  `#[derive(Widget)]` proc macro that generates its plugin/spawn glue.
- **`lumen-ffi`**: the C ABI (`liblumen_ffi`), for driving a Lumen app
  from another language.
- **`lumen-lsp`**: the language server.
- **`lumen-mcp` / `lumen-mcp-server`**: the in-app JSON-RPC introspection
  server and the standalone stdio bridge that fronts it for MCP clients
  such as Claude Code.
- **`lumen-devtools`**: the in-window devtools overlay.
- **`sdk/rust`** (crate name `lumen`): the single-dependency Rust SDK for
  building or embedding a Lumen app, built on `lumen-runtime` plus
  `lumenc`'s parser.
- **`benches`** (crate name `lumen-benches`): tests for runtime hot paths.

A change to how something looks or behaves at the app level usually
belongs in `lumen-runtime` or a backend crate; a change to markup/CSS
syntax or the CLI belongs in `lumenc`; a new native capability (another
OS integration) gets its own `lumen-os-*` crate following the existing
pattern.
