# Install Lumen

Installing Lumen gives you `lumenc`, the command you use to create, run, and
package apps, the Lumen runtime library it loads, the launcher a packaged app
is built from, and the candela standard library scripts import. You do not need
a Rust toolchain to build apps, or to package one for someone else.

## Linux and macOS

```sh
curl -fsSL https://lumenfx.dev/install.sh | sh
```

The installer resolves the current release, downloads the archive for your
platform, and unpacks it under `~/.lumen`. It prints what it is about to do and
asks before writing anything.

Every release publishes a `sha256sums.txt` covering its assets. The installer
downloads that file first and checks the archive against the line for it;
anything that does not match is not installed.

Builds are published for Linux and macOS on x86_64 and aarch64. The installer
needs `curl` or `wget`, and `sha256sum` or `shasum`.

Afterwards, `lumenc` lives in `~/.lumen/bin`. If that directory is not on your
`PATH`, the installer offers to add a line to your shell's startup file, and
tells you the line to add if you decline. Open a new shell and check:

```sh
lumenc --version
```

The install also carries a shell completion script for bash, zsh, and fish
under `~/.lumen/share`, and prints the one line your shell needs to load it.
See [shell completions](../reference/cli.md#completions).

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

## Platforms with no build

Releases cover Linux and macOS on x86_64 and aarch64, and Windows on x86_64.
On anything else - Windows on ARM, for instance - install from source:

```sh
cargo install lumenc
```

This needs a Rust toolchain. It builds `lumenc`, then fetches the matching
Lumen source and builds the runtime library and the launcher from it, putting
both beside the installed `lumenc` so packaging an app works the same as it
does from a release. Expect it to take a while: it is compiling the engine.

A source install carries no candela standard library, because cargo keeps only
the binary it installed: a script that reaches for `import "std/..."` or an
array method wants a release install, or a `libs/` directory copied beside
`lumenc` by hand.

Set `LUMEN_SKIP_ENGINE_BUILD=1` to install only the compiler, if you are
building the other two yourself. `lumenc run`, `build`, and `check` work
without them; `lumenc package` is the command that needs them, and it says so
and names the directory to put them in.

## Nightly builds

A build of `main` goes up every night as a prerelease, on one tag:

```
https://github.com/lumen-fx/lumen/releases/tag/nightly
```

Take one to try a fix or a feature before it is released. It carries the same
archives a release does, and the notes on it name the commit it was built from.

Download the archive for your platform and unpack it yourself; on Windows take
`lumen-windows-x86_64.zip`. Nothing installs a nightly for you. `install.sh`,
the setup-lumen action, and `lumenc`'s own update check all resolve the current
release, and a prerelease is not one, so a nightly never arrives on a machine
that did not ask for it and never offers to replace itself.

Three things to expect from a nightly:

- There is no Windows installer, only the portable zip. An installer would
  share product identity with a released install and take it over.
- `lumenc --version` reports the version `main` carries, which no release is
  behind. It does not say which night you have; the commit in the notes does.
- `lumenc web` and `lumenc package --target` download their extra files from
  the current release rather than from the nightly, because that is the only
  version they can resolve. A nightly compiler pairs them with a released
  browser runtime.

Every night's assets replace the last, so a link keeps working and the build
before it is gone. Keep a copy if you need one to stay around.

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

A pinned install is never offered a newer release, and the files it downloads
later stay on the pinned version: `lumenc package --target` and `lumenc web`
fetch from the release the pin names. Run the installer again without
`--version` to lift the pin.

Releases from before `sha256sums.txt` was published cannot be installed this
way; the installer has nothing to verify them against and stops.

## Continuous integration

A GitHub Actions workflow installs the toolchain with the setup-lumen action
instead of the script above, which prompts and writes to a shell startup file:

```yaml
steps:
  - uses: actions/checkout@v4
  - uses: lumen-fx/lumen/tools/setup-lumen@main
  - run: lumenc check .
```

It runs on Linux, macOS, and Windows runners, downloads the release built for
the runner, checks it against the release's `sha256sums.txt`, and puts `lumenc`
on `PATH`. Pass `version` to hold a workflow on a release:

```yaml
  - uses: lumen-fx/lumen/tools/setup-lumen@main
    with:
      version: "0.1.0"
```

The unpacked toolchain is kept in the workflow cache, keyed on the release and
the runner's platform, so later runs skip the download.

Two things behave differently in a workflow than on a workstation. The update
check never runs: a `CI` environment variable turns it off, and an unpacked
archive has no install receipt to check against in the first place. And
`lumenc run` loads the runtime library, which on Linux links GTK, ALSA, X11,
and Wayland; a job that runs an app installs those first, while `check`, `new`,
and `fmt` need none of them.

The action's own inputs and outputs are documented with it, in
[tools/setup-lumen](https://github.com/lumen-fx/lumen/tree/main/tools/setup-lumen).

## Next

- [Write your first app](first-app.md).
- [Every `lumenc` subcommand and flag](../reference/cli.md).
- [Build Lumen from source](../contributing/building-lumen.md) if you want to
  work on the framework itself.
