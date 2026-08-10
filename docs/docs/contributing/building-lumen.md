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
- `libasound2-dev` for ALSA, reached through cpal under `lumen-audio`.
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
apps are narrower: `apps/counter` and `apps/notes` for basic markup and
scripting, `apps/kanban` for drag and drop, `apps/pages-demo` for multi-page
navigation, `apps/music` for audio.

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

## Feature flags that matter

Three crates carry flags you will meet while working on the tree.

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
winit, cosmic-text, taffy, or the script hosts.

**`lumen-runtime`** defaults to every subsystem on: `audio`, `mcp`, `async`,
`host-lua`, `host-candela`, `http-fetch`, `runtime-parse`. The Rhai host is
always compiled. Per-app trimming happens only on the static bundle path, where
`lumenc` selects the exact feature set an app needs; the development path stays
full featured.

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

Two families of test skip themselves rather than fail when the machine cannot
support them, printing the reason:

- Framebuffer readback on a software adapter. Direct3D's WARP rasterizer faults
  the test process when a texture is read back, so those cases want a real GPU.
- The screenshot goldens in `lumenc/tests/golden.rs`. Baselines carry the font
  set of the machine that captured them, and a machine that resolves a
  different default sans-serif redraws every case containing text. They run
  locally and skip when `CI` is set.

Useful targeted runs while working in one area:

```sh
cargo test -p lumen-render-headless --test golden_rects
cargo test -p lumen-render-wgpu --test smoke
cargo test -p lumen-layout-taffy --test dirty_invariant
```

Golden images are regenerated, not hand-edited. `UPDATE_GOLDENS=1` rewrites the
software rasterizer baseline in `lumen-render-headless`;
`LUMEN_GOLDEN_UPDATE=1` rewrites the screenshot baselines in `lumenc`. On a
mismatch the screenshot suite writes the actual and diff images under a
`lumen-golden-failures` directory inside `CARGO_TARGET_DIR`.

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

## Documentation

The documentation site is built with Zensical from `docs/`:

```sh
cd docs
uv run zensical build --strict
```

`--strict` turns broken links and unknown navigation entries into errors. The
navigation lives in `docs/zensical.toml`.

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
