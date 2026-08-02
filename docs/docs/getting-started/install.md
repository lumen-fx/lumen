# Install

## Prebuilt toolchain

The installer fetches the prebuilt toolchain (`lumenc` and the `liblumen`
runtime), verifies every download against a sha256 manifest, and installs
under `~/.lumen`:

```bash
curl -fsSL https://lumenfx.dev/install.sh | sh
```

It prompts per component; useful flags:

```bash
# non-interactive, default components
curl -fsSL https://lumenfx.dev/install.sh | sh -s -- --no-confirm

# add the standalone candela toolchain
curl -fsSL https://lumenfx.dev/install.sh | sh -s -- --components "add:candela"

# pin a release, choose a prefix, remove an install
install.sh --version 0.1.0 --prefix ~/tools/lumen
install.sh --uninstall
```

Linux and macOS, x86_64 and aarch64. Prebuilt binaries ship with the first
alpha release; until it is published, build from source below.

The default components are enough to write and run candela apps: the
candela engine is part of the Lumen runtime. The separate `candela`
component is the standalone candela toolchain (`candela` and `candela-vm`),
for running candela programs outside a Lumen app.

## Toolchain

Rust 1.85 or newer (edition 2024). Install via [rustup](https://rustup.rs):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## From source

```bash
git clone https://github.com/lumen-fx/lumen.git
cd lumen
cargo run -p lumenc -- new hello my-app
cargo run -p lumenc -- run my-app
```

`lumenc` is the AOT compiler + runner. The first launch is slow
(wgpu + vello + cosmic-text + taffy compile in dev mode); subsequent
runs reuse the incremental cache.

For a faster dev loop, drop a release binary on your PATH:

```bash
cargo install --path lumenc
lumenc run my-app
```

## System dependencies

Lumen pulls in winit (windowing), wgpu (GPU), AccessKit (a11y), `rfd`
(native file dialogs), `muda` (native menu bars), and `notify-rust`
(toasts). Most ship as pure-Rust crates; a handful link against OS
libraries.

| Platform | Dependency | Required for | Install |
|---|---|---|---|
| Linux | GTK 3 + pkg-config | `rfd`'s GTK3 file dialog | `sudo apt install libgtk-3-dev pkg-config` (Debian / Ubuntu) <br> `sudo dnf install gtk3-devel pkgconf-pkg-config` (Fedora) <br> `sudo pacman -S gtk3 pkgconf` (Arch) |
| Linux | libxdo-dev | Native menu bars via `muda` (optional - Lumen builds without it on Linux; only macOS / Windows get menu bars) | `sudo apt install libxdo-dev` |
| Linux | libnotify | the `notify(...)` script builtin (most desktops bundle a notification daemon already) | `sudo apt install libnotify-bin` |
| Linux | Vulkan loader + ICD | wgpu device init | `sudo apt install libvulkan1 mesa-vulkan-drivers` |
| macOS | Xcode command-line tools | linker + Metal headers | `xcode-select --install` |
| Windows | Visual Studio Build Tools 2022 (or full VS), C++ workload | MSVC linker | <https://visualstudio.microsoft.com/downloads/> -> "Build Tools for Visual Studio" |
| Windows | DirectX 12 | wgpu DX12 backend (Vulkan also works if installed) | ships with Windows 10/11 |

> **Linux file dialog note.** Lumen pins `rfd` to its GTK3 feature.
> The XDG portal backend forces zbus into tokio mode which conflicts
> with the blocking zbus AccessKit uses, so the portal variant is
> disabled. Pure-Wayland sessions still work (GTK3 falls back to
> portal-less file dialogs through its own pathway).

### GPU backend per OS

A Lumen build compiles exactly one GPU backend for the host OS: Vulkan on
Linux, Metal on macOS, DirectX 12 on Windows. The other backends and their
shader translators are not built, which keeps the shipped runtime smaller. A
Linux host therefore needs a Vulkan loader + ICD (`libvulkan1` +
`mesa-vulkan-drivers`, or software Vulkan via lavapipe in CI / VMs).

For an old GPU or a Vulkan-less container, build `lumen-render-wgpu` with the
opt-in `gl-fallback` feature to re-add the OpenGL/GLES backend and an explicit
GL adapter fallback:

```bash
cargo build -p lumen-render-wgpu --features gl-fallback
```

## Distribution: full cdylib vs trimmed bundle

Lumen ships one full-featured shared `liblumen_ffi` cdylib that every app
dlopens (the fast dev + SDK path). For a size-sensitive release, build a
per-app static bundle instead:

```bash
lumenc bundle --static <app_dir> <out_dir>
```

This resolves the app's `[capabilities]` (`lumen.toml`, plus a conservative
source scan) and compiles the runtime seam with only the subsystems that app
uses; audio, MCP, the async bridge, unused script hosts, and `http-fetch` all
drop out when they are not needed. The shared cdylib and `lumenc run` stay
full-featured. See [`lumen.toml`](../authoring/lumen-toml.md#capabilities) for
the capability table.

## Editor support

- **VS Code**: install the `tools/vscode-lumen` extension (symlink it
  into `~/.vscode/extensions/`) and put `lumen-lsp` on PATH:

  ```bash
  cargo install --path lumen/lsp
  ```

  The extension claims `*.lmn` files (`onLanguage:lumen-markup`) and
  ships completion, hover, and diagnostics. Jump-to-definition and
  find-references are not yet implemented.

- **Other editors**: any client that speaks the Language Server
  Protocol can launch `lumen-lsp` over stdio. The crate publishes its
  capabilities document; point your editor at the binary and it
  negotiates the rest.

## Verifying the install

```bash
lumenc new hello hello-test
lumenc check hello-test       # parse-only - should exit 0
lumenc run hello-test         # opens a 960x720 window
```

If `lumenc check` fails on the scaffold, file an issue - the templates
are CI-tested against every shipped tag.
