# Install Lumen

Installing Lumen gives you two things: `lumenc`, the command you use to
create, run, and package apps, and the Lumen runtime library it loads. You do
not need a Rust toolchain to build apps.

## Linux and macOS

```sh
curl -fsSL https://lumenfx.dev/install.sh | sh
```

The installer resolves the current release, downloads the archive for your
platform, checks it against the SHA-256 digest published with the release, and
unpacks it under `~/.lumen`. It prints what it is about to do and asks before
writing anything.

Builds are published for Linux and macOS on x86_64 and aarch64. The installer
needs `curl` or `wget`, and `sha256sum` or `shasum`.

Afterwards, `lumenc` lives in `~/.lumen/bin`. If that directory is not on your
`PATH`, the installer offers to add a line to your shell's startup file, and
tells you the line to add if you decline. Open a new shell and check:

```sh
lumenc --version
```

### Installer options

| Option | Effect |
| --- | --- |
| `--prefix DIR` | Install root. Default `~/.lumen`. |
| `--version VERSION` | Install a specific release and pin to it. |
| `--no-confirm` | Accept the defaults without prompting. |
| `--no-modify-path` | Never write to a shell startup file. |
| `--force` | Reinstall even when already at the target version. |
| `--uninstall` | Remove every file the installer wrote. |
| `-h`, `--help` | Show the installer's own help. |

`LUMEN_PREFIX` sets the install root like `--prefix`.

To pass options through the pipe, hand them to `sh`:

```sh
curl -fsSL https://lumenfx.dev/install.sh | sh -s -- --prefix ~/tools/lumen
```

### Uninstall

```sh
curl -fsSL https://lumenfx.dev/install.sh | sh -s -- --uninstall
```

The installer records every path it writes, so an uninstall removes exactly
those files and nothing else. A `PATH` line added to a shell startup file stays
behind; delete it by hand.

## Windows

Download and run the per-user installer:

```
https://github.com/lumen-fx/lumen/releases/latest/download/lumen-windows-x86_64.msi
```

It installs under your user profile, so it needs no administrator rights, and
it adds `lumenc` to your user `PATH`. Open a new terminal afterwards. Remove it
from Settings > Installed apps, which also removes the `PATH` entry.

Each release also publishes `lumen-windows-x86_64.zip`, a portable archive you
can unpack anywhere. A portable copy never checks for updates; you replace it
by unpacking a newer zip.

## Staying up to date

An installed `lumenc` looks for a newer release at most once a day and prints a
single line when it finds one:

```
lumenc 0.2.0 is available (you have 0.1.0). Update: curl -fsSL https://lumenfx.dev/install.sh | sh
```

In a terminal it then offers to update for you. On Linux and macOS that reruns
the installer. On Windows it downloads the new installer and runs it once the
current command exits, because Windows cannot replace a running `lumenc.exe`.

The check is deliberately quiet:

- Only the commands you type by hand are checked: `run`, `check`, `build`,
  `bundle`, `new`, `fmt`, and `i18n`. Automation subcommands stay silent.
- Only an installed copy checks. A copy built from source, or unpacked from the
  portable Windows zip, never reaches the network.
- Anything with `--headless`, a non-terminal stderr, or a `CI` environment
  variable turns it off.
- `LUMEN_NO_UPDATE_CHECK` set to any non-empty value turns it off everywhere.

### Pinning a version

Install with `--version` to hold a project on a known release:

```sh
curl -fsSL https://lumenfx.dev/install.sh | sh -s -- --version 0.1.0
```

A pinned install is never offered a newer release. Run the installer again
without `--version` to lift the pin.

## Next

- [Write your first app](first-app.md).
- [Every `lumenc` subcommand and flag](../reference/cli.md).
- [Build Lumen from source](../contributing/building-lumen.md) if you want to
  work on the framework itself.
