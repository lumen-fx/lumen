# Install

## Prebuilt toolchain

The installer fetches the prebuilt toolchain (`lumenc` and the `liblumen`
runtime), verifies every download against the sha256 GitHub reports for that
release asset, and installs under `~/.lumen`:

```bash
curl -fsSL https://lumenfx.dev/install.sh | sh
```

It asks for confirmation before installing and before touching your shell's
PATH; useful flags:

```bash
# non-interactive
curl -fsSL https://lumenfx.dev/install.sh | sh -s -- --no-confirm

# pin a release, choose a prefix, remove an install
install.sh --version 0.1.0 --prefix ~/tools/lumen
install.sh --uninstall
```

`--version` pins the install. The installer records the pin, and a pinned
copy is never offered a newer release. Run the installer again without
`--version` to move to the current release and lift the pin.

The installer covers Linux and macOS, on x86_64 and aarch64. Windows x86_64
has its own package, [below](#windows). Prebuilt binaries ship with the first
alpha release; until it is published, build Lumen from source instead. See
[Building Lumen from source](../contributing/building-lumen.md).

Candela, Lumen's scripting language, is compiled into `liblumen` directly
(the runtime and its compiler both ship as part of the library) - a Lumen
app with candela scripts needs nothing beyond this install. The standalone
candela language toolchain (for running candela programs outside a Lumen
app) is a separate product with its own installer, at candela's own release
channel - it is not part of this installer.

## Windows

Download and run the installer:

<https://github.com/lumen-fx/lumen/releases/latest/download/lumen-windows-x86_64.msi>

It installs `lumenc` and its runtime library for the current user, under
`%LOCALAPPDATA%\Programs\Lumen`, so it never asks for an administrator
password. It adds the `bin` directory to your user PATH; open a new terminal
for that to take effect.

The package is not signed yet, so SmartScreen may warn you about it. Choose
"More info", then "Run anyway".

`lumen-windows-x86_64.zip` on the same release page is the portable
alternative: unpack it wherever you like and put its `bin` directory on PATH
yourself. Keep `lumenc.exe` and `lumen_ffi.dll` in the same directory; that is
where `lumenc` looks for its runtime.

To remove Lumen, use Settings > Installed apps, or run:

```powershell
msiexec /x lumen-windows-x86_64.msi
```

## Staying up to date

`lumenc` looks for a newer release once a day and prints one line when it
finds one:

```
lumenc 0.2.0 is available (you have 0.1.0). Update: curl -fsSL https://lumenfx.dev/install.sh | sh
```

On a terminal it then asks `Update now? [y/N]`. Answer `y` and it runs that
installer command for you; anything else leaves the install alone.

On Windows the line names `lumen-windows-x86_64.msi` instead, and `y`
downloads it and installs it. The install starts once the current command
exits, because Windows cannot replace `lumenc.exe` while it is running; open a
new terminal afterwards to pick up the new version.

The check runs only for the commands you type yourself: `run`, `check`,
`build`, `bundle`, `new`, `fmt`, and `i18n`. It is skipped for `--headless`
runs, for the automation subcommands that drive a running app, when `CI` is
set in the environment, and when stderr is not a terminal. It never delays a
command; an answer that has not arrived by the time the command finishes is
dropped. A `lumenc` built from source is not an installed copy and never
checks.

Set `LUMEN_NO_UPDATE_CHECK` to any value to turn the check off, on every
platform. A pinned install (`--version` above) is never checked either. Only
`install.sh --version` pins; an MSI install is never pinned, and a copy
unpacked from the portable zip is not an installed copy and never checks.

## System dependencies

Lumen pulls in winit (windowing), wgpu (GPU), AccessKit (a11y), `rfd`
(native file dialogs), `muda` (native menu bars), `rodio` / `cpal`
(audio), and `notify-rust` (toasts). Most ship as pure-Rust crates; a
handful link against OS libraries.

| Platform | Dependency | Required for | Install |
|---|---|---|---|
| Linux | GTK 3 + pkg-config | `rfd`'s GTK3 file dialog | `sudo apt install libgtk-3-dev pkg-config` (Debian / Ubuntu) <br> `sudo dnf install gtk3-devel pkgconf-pkg-config` (Fedora) <br> `sudo pacman -S gtk3 pkgconf` (Arch) |
| Linux | ALSA | audio via `cpal` under `rodio` | `sudo apt install libasound2-dev` (Debian / Ubuntu) <br> `sudo dnf install alsa-lib-devel` (Fedora) <br> `sudo pacman -S alsa-lib` (Arch) |
| Linux | libxkbcommon | keyboard handling under winit | `sudo apt install libxkbcommon-dev` (Debian / Ubuntu) <br> `sudo dnf install libxkbcommon-devel` (Fedora) <br> `sudo pacman -S libxkbcommon` (Arch) |
| Linux | libwayland | Wayland session support under winit | `sudo apt install libwayland-dev` (Debian / Ubuntu) <br> `sudo dnf install wayland-devel` (Fedora) <br> `sudo pacman -S wayland` (Arch) |
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

> **Build hooks need their own tools.** An app's `lumen.toml` can declare
> `[[hooks]]` that run arbitrary build commands - the `apps/notes` example
> compiles a small C library this way. Building Lumen from source already
> needs a C toolchain (Rust's linker uses one), so this only bites if you
> installed the prebuilt toolchain above: a Linux host additionally needs
> a C compiler (`sudo apt install build-essential`, or your distro's
> equivalent); macOS and Windows reuse the Xcode command-line tools /
> Visual Studio Build Tools listed in the table above. See
> [`[[hooks]]`](../authoring/lumen-toml.md#hooks) and pass `--no-hooks` to
> `run` / `build` / `bundle` to skip an app's hooks entirely.

### GPU backend per OS

A Lumen build compiles exactly one GPU backend for the host OS: Vulkan on
Linux, Metal on macOS, DirectX 12 on Windows. The other backends and their
shader translators are not built, which keeps the shipped runtime smaller. A
Linux host therefore needs a Vulkan loader + ICD (`libvulkan1` +
`mesa-vulkan-drivers`, or software Vulkan via lavapipe in CI / VMs).

For an old GPU or a Vulkan-less container, the `gl-fallback` build feature
re-adds the OpenGL/GLES backend and an explicit GL adapter fallback; it needs
a source build, so see
[Building Lumen from source](../contributing/building-lumen.md).

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

This one command compiles the runtime from source, so it needs the Lumen
source tree and a Rust build environment even from a prebuilt-toolchain
install; see [Building Lumen from source](../contributing/building-lumen.md).
If it cannot find the source tree on its own, point it there with the
`LUMEN_WORKSPACE_DIR` environment variable.

## Editor support

`lumen-lsp` is not part of the prebuilt install channel yet; build it from
source (see [Building Lumen from source](../contributing/building-lumen.md))
to get completion, hover, and diagnostics for `.lmn` files.

- **VS Code**: install the `tools/vscode-lumen` extension (symlink it
  into `~/.vscode/extensions/`) and put the built `lumen-lsp` binary on
  PATH. The extension claims `*.lmn` files
  (`onLanguage:lumen-markup`). Jump-to-definition and find-references are
  not yet implemented.

- **Other editors**: any client that speaks the Language Server
  Protocol can launch `lumen-lsp` over stdio. The binary publishes its
  capabilities document; point your editor at it and it negotiates the
  rest.

## Verifying the install

```bash
lumenc new hello hello-test
lumenc check hello-test       # parse-only, exits 0
lumenc run hello-test         # opens a window saying hello
```

If `lumenc check` fails on the scaffold, file an issue - the templates
are CI-tested against every shipped tag.
