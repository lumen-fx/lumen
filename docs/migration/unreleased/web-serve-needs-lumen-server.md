# `lumenc web --serve` runs the separate `lumen-server` binary

`lumenc web --serve` no longer serves the site from inside `lumenc`. It starts
`lumen-server --dev` on the built site and waits for it. `lumenc` looks for
`lumen-server` beside itself, then at the path in `$LUMEN_SERVER`, then on
`PATH`, and fails naming those three places when none has it.

A release install (the install script, the MSI, Homebrew, Scoop, the AUR
package, or the setup-lumen action) puts `lumen-server` beside `lumenc`, so
nothing changes there.

A source install from `cargo install lumenc` does not build `lumen-server`.
Build it from a Lumen checkout of the same version and point `LUMEN_SERVER` at
it:

```sh
cargo build --release -p lumen-server
export LUMEN_SERVER="$PWD/target/release/lumen-server"
lumenc web myapp --serve
```

The `--host`, `--port`, and `--allow-host` flags work as before; `lumenc`
passes them on to `lumen-server`.
