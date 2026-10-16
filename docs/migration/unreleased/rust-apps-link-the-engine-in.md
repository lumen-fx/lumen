# A packaged Rust app is one executable with the engine linked in

This affects Rust SDK apps. `lumenc run`, `lumenc build` and `lumenc package`
now build a Rust app with plain cargo, with no `-C prefer-dynamic` added to
`RUSTFLAGS`, so the engine is linked into the executable on every platform, as
the published `lumenui` already links it. `lumenc package` copies that
executable and the app's files; `liblumen_engine` and the Rust standard
library no longer travel beside it, and a package script that expected them
should stop looking for them.

A Rust app that declares a runtime module under `[dependencies]` in
`lumen.toml` is now refused by `lumenc package` on every platform, not only
Windows: a runtime module loads only into a shared engine, and a Rust app has
none. Portable plugins still travel.

`lumen_source!("src/main.lmn", "src/main.css")` now loads the app at the
directory above `src/`, so `lumen.toml` beside `src/` is read in a debug
build. Before, a markup file under `src/` made `src/` itself the app
directory, and the run stopped with the "found code files at the app root"
error. Keep markup and stylesheet under `src/` and point the macro there:

```rust
// before: markup beside the code, which the src/ layout rejects
lumen_source!("ui/main.lmn", "ui/main.css")
// after
lumen_source!("src/main.lmn", "src/main.css")
```
