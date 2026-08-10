# Packaging an app

During development `lumenc run` parses your markup and CSS on every launch.
That is what makes hot reload work, and it is the wrong thing to ship: a
released app should do the parsing once, at build time.

`lumenc build` does that parsing once and writes the result to a file. Your
app then starts from that file instead of from source.

## Compile the app

```sh
lumenc build myapp myapp.lmna
```

This parses `main.lmn` and the stylesheet, runs the whole cascade, resolves
asset paths, splices includes and imports, and bakes the app's script into one
artifact. It prints the element count and the size of the file it wrote.

Run it back:

```sh
lumenc run myapp --artifact myapp.lmna
```

The app directory is still needed: it supplies `lumen.toml` and the assets
your markup refers to. What the artifact replaces is the parse, not the
directory.

Two things follow from compiling ahead of time:

- Startup skips the parse and the cascade.
- Hot reload is off. There is no source being watched, so edits need a
  rebuild.

Everything else behaves the same. Colours and metrics stay reachable through
CSS variables and design tokens, because the artifact carries the cascaded
stylesheet rather than freezing resolved values into the tree.

Rebuild the artifact whenever the markup, the stylesheet, or the script
changes. `lumenc check` is the fast way to confirm an app parses before you
compile it.

## Archive the app's files

```sh
lumenc bundle myapp myapp.lpak
```

This packs every regular file under the app directory into one `.lpak`
archive, skipping dotfiles and `target/` directories. It is the counterpart to
the artifact: `build` compiles the app, `bundle` collects the files around it.

An archive is read through the asset server, so it suits an app you embed in
your own program with the Rust SDK or the C API: register the archive and
asset lookups resolve out of it. `lumenc` has no command that runs a `.lpak`
directly, so a markup app started with `lumenc run` reads its assets from disk
as usual.

## Trim the runtime

An app that plays no audio and makes no network calls does not need the code
for either. `lumenc bundle --static` works out which subsystems an app uses
and builds a runtime library carrying only those.

```sh
lumenc bundle --static myapp out/
```

It prints the capability set it resolved, builds the trimmed runtime, and
copies the library into `out/`.

Detection is deliberately cautious: a subsystem is dropped only on a clear
signal that the app never uses it, and anything ambiguous is kept. Override
either direction in `lumen.toml`:

```toml
[capabilities]
audio = false
http-fetch = false
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

A `prebuild` hook runs before `lumenc run`, `build`, and `bundle`. A `prerun`
hook runs only before `lumenc run`, after every `prebuild` hook. `lumenc
check` never runs hooks, so a check stays free of side effects.

Listing `inputs` and `outputs` makes the hook skippable: when the outputs are
already newer than the inputs, the command does not run again. Leave either
list out and the hook runs every time.

Hooks run in declaration order with the app directory as their working
directory, and a failing hook stops the command. Give a hook an `os` when it
only makes sense on one platform, as in the example above; declare one entry
per platform to cover them all.

A hook is a shell command that a `lumen.toml` asks for, so treat an app from
someone else the way you would treat a project with a build script. Pass
`--no-hooks` to any of `run`, `build`, or `bundle` to skip them.

The full key list is in the [lumen.toml
reference](../reference/lumen-toml.md#hooks).

## What to ship

A released markup app is the artifact, the app directory's assets, and
`lumen.toml`. Lumen itself is a library the runner links, so your users need a
Lumen installation or a runner you ship alongside the app.

Alpha limits worth knowing before you plan a release:

- There is no command that produces a single self-contained executable from a
  markup app.
- `lumenc bundle --static` produces a trimmed runtime library, not a finished
  application binary.

If you want a single binary today, write the shell of the app with the Rust
SDK, embed your markup with `include_str!`, and build it with `cargo`. Apps
authored that way are detected by `lumenc run` and `lumenc build`, which hand
off to `cargo` for you.
