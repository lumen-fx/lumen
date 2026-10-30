# A Rust app's UI is compiled by its build script; `lumen_source!` is gone

This affects every Rust SDK app.

A Rust app is now described by its `lumen.toml` like any Lumen app, and its
build compiles the UI `lumen.toml` names into the executable, in debug and
release builds alike. A markup or style error fails `cargo build`. A debug
build still reloads the UI on an edit under `src/` while the window stays
open. A package no longer ships `src/`: the executable carries the UI, and
only `lumen.toml` and the assets travel beside it.

Add the build script and its crate:

```toml
# Cargo.toml
[build-dependencies]
lumenui-build = "X.Y.Z"   # the same version as lumenui
```

```rust
// build.rs
fn main() {
    lumenui_build::compile();
}
```

Make sure `lumen.toml` names the entry (`[app] entry = "main.lmn"`, the file
under `src/`), then start the app from the compiled UI:

```rust
// before
lumenui::App::new()
    .add_plugins(LumenDefaultPlugins.with_source(lumen_source!("src/main.lmn", "src/main.css")))

// after
lumenui::app!()
```

The build script compiles with `lumenc` of the same version: the one
`LUMEN_LUMENC` names, else `lumenc` on `PATH` when its version matches, else
the release's toolchain archive, downloaded once into the user cache
directory.

These are removed, each with what replaces it:

- `lumen_source!`, `Source`, `with_source`: `lumenui::app!()`.
- `LumenDefaultPlugins`, `LumenPluginsBuilder` and the `LayoutPlugin` /
  `WindowPlugin` / `ScriptPlugin` / ... markers: the stack is the app's;
  `App::new` runs all of it. `.headless()` / `disable::<WindowPlugin>()`:
  `App::run_headless` or `App::run_headless_rendered`.
- `with_markup(text)`: `App::from_markup(text)`, for a test or an example
  with no build script.
- `with_dir(dir)`: `App::new(CompiledUi::compile(dir)?)` to compile an app
  directory when the program starts, or `App::app_dir(dir)` to read
  `lumen.toml` and the assets from `dir`.
- `with_title` / `with_size`: `[window] title` / `size` in `lumen.toml`;
  `App::window_title` / `App::window_size` for a value worked out at start.
- `hot_reload(bool)`: `[runtime] hot_reload` in `lumen.toml`, or
  `CompiledUi::hot_reload`.

A widget the app defines with `#[derive(Widget)]` compiles into the UI when
its `#[widget(tag = "...")]` is in a file under the crate's `src/`; a widget
from another crate is not seen by the compile.
