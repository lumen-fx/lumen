<p align="center">
  <img src="assets/colored-logo.png" alt="Lumen" width="220">
</p>

# Lumen

A declarative, markup-first UI framework for native desktop apps: Rust ECS
core, GPU rendering, and live hot reload of your markup, styles, and script.

You author a `.lmn` markup tree, an optional CSS-subset stylesheet, and an
optional script. `lumenc` compiles all three to a layout IR, spawns
entities across a two-world [`bevy_ecs`](https://docs.rs/bevy_ecs) core, and
renders through [`wgpu`](https://wgpu.rs) + [`vello`](https://github.com/linebender/vello)
with [`cosmic-text`](https://github.com/pop-os/cosmic-text) shaping and
[`taffy`](https://github.com/DioxusLabs/taffy) layout.

## Features

- **`.lmn` markup + CSS subset + scripting**: a fixed tag vocabulary, a
  familiar CSS-like styling layer, and an embedded script for view logic and
  reactive signals. Script in candela, Lua, or Rhai; the host is picked by
  file extension.
- **Hot reload**: markup, styles, and script all reload on save, preserving
  component state (focus, scroll position, signal values) across the swap.
- **GPU rendering**: `wgpu` + `vello` with `cosmic-text` glyph shaping.
- **Flexbox and grid layout** via `taffy`.
- **Accessibility**: an AccessKit-backed a11y tree.
- **Text input**: IME composition (preedit/commit) and Unicode BiDi
  (mixed LTR/RTL) text shaping.
- **C ABI** for embedding Lumen from other languages, with Rust, Python, and
  C/C++ SDKs on top.
- **Editor tooling**: an LSP server (diagnostics, completion, hover) and a
  formatter (`lumenc fmt`).
- **MCP server**: inspect a running app's entities, components, resources,
  and framebuffer from an MCP client over stdio, for automated testing or
  agent-driven UI work.
- **Cross-platform**: Linux, Windows, and macOS.

## Quick example

`main.lmn`:

```xml
<root bg="#0c1c30" padding="32" gap="20" align="center" justify="center">
  <label class="display" id="counter" width="100%" height="120px" text="0"
         bind-text="clicks" />
  <row gap="14" justify="center">
    <button class="primary" id="bump"  width="120px" height="48px" text="+1" />
    <button class="primary" id="reset" width="120px" height="48px" text="reset" />
  </row>
  <script src="main.rhai" />
</root>
```

`main.rhai`:

```rhai
fn on_start() {
    on("click", "bump",  "handle_bump");
    on("click", "reset", "handle_reset");
}

fn handle_bump(id) {
    let n = signal("clicks", 0);
    n.set(n.get() + 1);
}

fn handle_reset(id) {
    signal("clicks", 0).set(0);
}
```

Generate this exact app with `lumenc new counter <name>` (see below).

## Getting started

### Prerequisites

- Rust 1.85+ (edition 2024), via [rustup](https://rustup.rs).
- A working GPU/Vulkan (or Metal/DirectX) driver stack. Linux additionally
  needs GTK 3 for native file dialogs. See
  [the install guide](docs/docs/getting-started/install.md) for the full
  per-platform dependency list.

### Build from source

Lumen is pre-1.0 and not yet published to crates.io; build from source:

```sh
git clone https://github.com/lumen-fx/lumen.git
cd lumen
cargo build --release -p lumenc
```

Scaffold and run an app:

```sh
./target/release/lumenc new counter my-app
./target/release/lumenc run my-app
```

`lumenc run` watches `my-app/` and hot-reloads markup, CSS, and script on
save. See [Getting started](docs/docs/getting-started) in the docs for a full
walkthrough.

## Status

Lumen is in **alpha**. The author-facing surface (markup tags, the CSS
subset, and the script builtins) is functional and reasonably broad, but APIs
are not yet stable and may change without notice. No crates are published;
there is no compatibility guarantee between commits.

## Documentation

The full documentation lives under [`docs/`](docs) and is published at
[docs.lumenfx.dev](https://docs.lumenfx.dev). Build it locally with
[uv](https://docs.astral.sh/uv/):

```sh
cd docs
uv run zensical serve
```

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Licensed under the [Mozilla Public License 2.0](LICENSE).
