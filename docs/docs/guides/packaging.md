# Packaging an app

During development `lumenc run` parses your markup and CSS on every launch.
That is what makes hot reload work, and it is the wrong thing to ship: a
released app should do the parsing once, before anyone downloads it.

This page covers going from an app directory to something you can hand
someone. The short answer is `lumenc package`; the rest of the page is the
pieces underneath it and when you would reach for them on their own. It
describes a markup app throughout, and the last section covers what differs for
an app written against one of the SDKs.

## Package the app

```sh
lumenc package myapp
```

This writes `myapp/dist/myapp/`: a folder holding your app as an executable,
the Lumen runtime library beside it, `lumen.toml`, and your app's files at the
same relative paths your markup names them by. Copy that folder to another
machine, run the executable, and the app starts. Nobody needs Lumen installed,
and nobody needs a compiler.

The Lumen runtime library sits in the folder next to the executable. On
Linux and macOS the shared engine and the Rust standard library it was built
against travel beside it, and any [runtime
modules](../reference/lumen-toml.md#dependencies) the app declares are
staged into a `modules/` subfolder: `path` and `bundled` modules are copied
from where the declaration points, and a `version` module resolves through
the same cache and `lumen.lock` as `lumenc run`, so the shipped folder
carries the exact library the lock pins. All of it belongs to the app; keep
the folder together when you move it.

A Windows package stays one library plus the executable. Runtime modules do
not load beside it, because there is no shared engine there for one to load
into, so an app that declares them is packaged with
[`--static`](#one-self-contained-executable) instead.

Choose the name and the destination yourself:

```sh
lumenc package myapp build/Notes --name Notes
```

To hand the result to someone rather than run it, ask for an archive as well:

```sh
lumenc package myapp --zip
```

That writes `myapp/dist/myapp.zip` beside the folder, holding the folder
itself, so unpacking it gives the same directory back rather than scattering an
executable and its libraries.

The markup, the stylesheet, and the scripts are compiled into the executable,
so `src/` stays behind; a [multi-page app](pages.md) compiles every page in.
Everything else in the app directory travels: images, fonts, sounds,
translation catalogues, data files. Dotfiles and a `target/` directory are left
behind, and so is the output folder itself.

One thing does not travel: the candela standard library, which the toolchain
carries and a package does not, so a script that reaches for `import "std/..."`
or an array method works under `lumenc run` and not in the folder this writes.

Packaging a markup app needs no Rust toolchain. It copies a prebuilt launcher
and appends your compiled app to the copy. On macOS it links the app in
instead, because a signed Mach-O executable cannot carry trailing data, and
that step needs the Xcode Command Line Tools (`xcode-select --install`).

### Packaging for another platform

```sh
lumenc package myapp --target windows-x86_64
```

The targets are `linux-x86_64`, `linux-aarch64`, `macos-x86_64`,
`macos-aarch64`, and `windows-x86_64`. For a markup app packaging is file
assembly rather than compilation, so any host can produce any of them; an SDK
app is compiled, and [what that needs](#cross-packaging-an-sdk-app) is below.

The runtime library and the launcher for the other platform come from a
published Lumen release: the one you installed, or the newest one if this
`lumenc` was not installed from a release. They are downloaded once, kept in a
cache keyed by that release, and checked against the checksums published with
it. If you already have those two files, point at them instead and nothing is
downloaded:

```sh
lumenc package myapp --target windows-x86_64 --lib-dir /path/to/files
```

A macOS package built from another platform ships the compiled app as a file
beside the executable rather than inside it, since linking it in needs a macOS
linker. It runs the same way.

An app declaring `[dependencies]` cross-packages when its modules are
`bundled`: the target's module archive is downloaded from the same release
as the toolchain files, verified, and cached beside them. A `path` module
cannot cross-package - a local library is built for one platform - and
neither can a `version` one until the module registry exists; package those
on a machine of the target platform instead.

### One self-contained executable

```sh
lumenc package myapp --static
```

This writes the same folder with one difference: the app is a single
executable. The engine is inside it, and so is every runtime module the app
declares, so there is no runtime library beside it and no `modules/`
subfolder. Copy the executable and the app's files, and that is the whole app.

Reach for it when you want one file to hand over rather than a folder to keep
together, and on Windows when your app declares `[dependencies]`, which is the
only shape that loads a module there.

It needs a linker on the machine doing the packaging: a C toolchain on Linux
(`build-essential`, `base-devel`, or your distribution's equivalent), the
Xcode Command Line Tools on macOS, and the Visual Studio Build Tools with the
"Desktop development with C++" workload on Windows. The first `--static`
package for a platform also downloads that platform's link kit from the
release channel and caches it, which takes a moment and happens once.

Four things it will not do, each with an exit code of 2 and a message saying
so:

- Package an app written against an SDK. Those bring their own executable from
  their own toolchain.
- Package for a platform other than the one you are on. The link runs through
  the tools installed here.
- Package an app declaring `[capabilities]`. The linked engine is the full
  one; choosing which subsystems a runtime carries is what
  [`bundle --static`](#trim-the-runtime) does, by compiling from source.
- Link a `path` or `version` module. Only the modules that ship with the
  toolchain (`bundled = true`) are in the kit; package without `--static` and
  the library is staged beside the executable instead.

### What a packaged app does at startup

The executable reads the app compiled into it, opens the runtime library
sitting next to it, and runs; a `--static` executable skips the opening and
runs what it already carries. Its own directory is the app directory: relative
paths in your markup, and `lumen.toml`, resolve against it, so the folder works
wherever it is copied.

Pass `--headless --ticks N` to a packaged app to run it window-free for a fixed
number of ticks, which is how you smoke-test a package in CI.

## Compile without packaging

`lumenc build` runs the same compile step and writes the result to a file:

```sh
lumenc build myapp myapp.lmna
```

It parses `src/main.lmn` and the stylesheet, runs the whole cascade, splices
includes and imports, records which engine runs each part of the script, and
writes one artifact. A candela script is compiled to bytecode as well, and both
forms go in: the artifact runs the same either way, and the bytecode is what a
runtime shipped without the candela compiler loads. Run it back with:

```sh
lumenc run myapp --artifact myapp.lmna
```

The app directory is still needed here: it supplies `lumen.toml` and the files
your markup refers to. What the artifact replaces is the parse, not the
directory.

Reach for `build` when you want the compiled app on its own: to embed it in a
host application through the C ABI, to measure startup without the parser, or
to check that an app compiles at all. Reach for `package` when you want
something to ship.

Two things follow from compiling ahead of time, whichever command you use:

- Startup skips the parse and the cascade.
- Hot reload is off. There is no source being watched, so edits need a
  rebuild.

Everything else behaves the same. Colours and metrics stay reachable through
CSS variables and design tokens, because the artifact carries the cascaded
stylesheet rather than freezing resolved values into the tree.

`lumenc check` is the fast way to confirm an app parses before you compile it.

### Limits of the compiled form

An app that reaches for a file no part of the app directory holds keeps
pointing at the absolute path it was given, which will not exist on anyone
else's machine. Keep what your app needs inside the app directory.

A [multi-page app](pages.md) compiles whole: every page goes into the
executable along with the page names navigation resolves against, so a packaged
app routes without carrying any `.lmn` files. Adding a page then means
rebuilding, since there are no page files left to reload.

## Archive the app's files

```sh
lumenc bundle myapp myapp.lpak
```

This packs every regular file under the app directory into one `.lpak`
archive, skipping dotfiles, `target/`, and `src/`; an archive holds the app's
assets, and the code is compiled rather than read by name. Use it when you want
those files as a single addressable blob rather than a folder, for instance to
serve them from one file or to keep a build output tidy.

Run against the archive with `--assets`:

```sh
lumenc run myapp --assets myapp.lpak
```

Every image, icon, and sound the markup names is then read out of the archive.
Lookups are keyed by the path relative to the app directory, the same path you
write in the markup, so nothing in the app changes. A file the archive does not
carry still comes from disk, which lets you keep one loose file for a quick edit
without repacking. Fonts are the exception: they load through the system font
database and are read from disk even when the archive carries them.

Pair it with an artifact to run an app as two files plus `lumen.toml`:

```sh
lumenc run myapp --artifact myapp.lmna --assets myapp.lpak
```

Rebuild the archive whenever an asset changes. A missing or corrupt archive
stops the run rather than quietly falling back to the directory.

## Trim the runtime

An app that makes no network calls and opens no file dialogs does not need
the code for either. `lumenc bundle --static` works out which subsystems an
app uses and builds a runtime library carrying only those.

```sh
lumenc bundle --static myapp out/
```

It prints the capability set it resolved, builds the trimmed runtime, and
copies the library into `out/`. Put that library in a package in place of the
one `lumenc package` copied, and the packaged app opens the trimmed build
instead.

Detection is deliberately cautious: a subsystem is dropped only on a clear
signal that the app never uses it, and anything ambiguous is kept. Override
either direction in `lumen.toml`:

```toml
[capabilities]
http-fetch = false
async = false
```

See [the capabilities table](../reference/lumen-toml.md#capabilities) for what
each key covers.

This command compiles the runtime from Lumen's source, so it needs a copy of
that source tree; point `LUMEN_WORKSPACE_DIR` at it. Without one, the command
still prints the resolved capability set and then stops.

## Build steps your app needs

Some apps need something built before they can run: a C library the script
loads, a generated data file, a downloaded asset. Declare those as hooks in
`lumen.toml` and Lumen runs them for you.

```toml
[[hooks]]
when    = "prebuild"
os      = "linux"
run     = "cc -shared -fPIC -O2 -o libmd.so md.c"
inputs  = ["md.c"]
outputs = ["libmd.so"]
```

A `prebuild` hook runs before `lumenc run`, `build`, `bundle`, and `package`. A
`prerun` hook runs only before `lumenc run`, after every `prebuild` hook.
`lumenc check` never runs hooks, so a check stays free of side effects.

Listing `inputs` and `outputs` makes the hook skippable: when the outputs are
already newer than the inputs, the command does not run again. Leave either
list out and the hook runs every time.

Hooks run in declaration order with the app directory as their working
directory, and a failing hook stops the command. Give a hook an `os` when it
only makes sense on one platform, as in the example above; declare one entry
per platform to cover them all.

A hook is a shell command that a `lumen.toml` asks for, so treat an app from
someone else the way you would treat a project with a build script. Pass
`--no-hooks` to any of `run`, `build`, `bundle`, or `package` to skip them.

The full key list is in the [lumen.toml
reference](../reference/lumen-toml.md#hooks).

## Apps written against an SDK

An app authored with the Rust, C++, or Python SDK is a program in that
language, not a markup directory, and it is built by that language's own
toolchain. `lumenc run`, `lumenc build`, and `lumenc package` all detect one
from its contents, or from `[app] kind` in `lumen.toml`, and hand the build to
`cargo`, `cmake`, or the interpreter.

`lumenc package` then assembles the same folder around whatever that build
produced, so packaging one is the same command:

```sh
lumenc package myapp
```

Every kind produces an executable, and the runtime library goes beside it, the
same as for a markup app. How the executable is produced is what differs:

- **Rust.** `cargo build --release` runs, and the binary it reports is copied
  in under your app's name. On Linux and macOS a Rust app links the engine
  rather than compiling a copy into itself, so the executable is small and the
  engine travels beside it, out of the same build. On Windows the runtime is
  inside the executable and nothing travels with it.
- **C++.** CMake configures and builds, and the executable from the build tree
  is copied in. If the project builds more than one executable, the most recent
  one is packaged and the others are named on the way past; give the app its
  own directory to keep that unambiguous.
- **Python.** The app is frozen into an executable with
  [PyInstaller](https://pyinstaller.org), which bundles the interpreter and the
  app's modules into one file. Install it first (`pip install pyinstaller`).

Unlike a markup app, an SDK app reads its markup, stylesheet, and scripts at
run time, so those files travel with it. What stays behind is the source it was
compiled from and the build tree that compile left.

### Cross-packaging an SDK app

`--target` works here too, and the SDK's own toolchain does the compiling:

```sh
lumenc package myapp --target linux-aarch64
```

For a Rust app the target triple is passed to cargo, so
`rustup target add aarch64-unknown-linux-gnu` is what makes it work. For a C++
app, set `CMAKE_TOOLCHAIN_FILE` to a toolchain file for that platform; there is
nothing Lumen can supply in its place, so packaging says so rather than
building this machine's binary under another platform's name. A Python app is
frozen against the interpreter doing the freezing and can only be packaged for
the platform you are on.

A Rust app needs nothing from the release channel: the engine it links comes
out of its own cargo build, so `rustup target add` is the whole requirement.
The other kinds open the C library, and that one is fetched for the platform
you asked for.

Every flag on every command here is in the [CLI
reference](../reference/cli.md).
