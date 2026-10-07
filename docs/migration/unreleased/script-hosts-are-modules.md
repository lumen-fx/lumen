# The script hosts are runtime modules

The engine no longer carries a script host. Each language runs on a runtime
module the toolchain ships: `lumen-candela` runs the bytecode a built or
packaged app carries, `lumen-candela-dev` compiles source for `lumenc run`,
`check` and `build`, and `lumen-rhai` and `lumen-lua` run the deprecated
languages. Lumen loads the module each script needs; an app declares nothing.

This affects you in these cases.

**Your app is written in Rhai or Lua.** It runs as before under `lumenc run`
and in a package, on every platform. `lumenc package --static` refuses it,
because a static executable carries no Rhai or Lua host, and a Linux or macOS
toolchain installed with `--no-modules` runs it without its script, with a
banner that says so. candela is the supported language.

**You embed Lumen from Rust.** `RunOptions::rhai_extensions`,
`RunOptions::with_rhai_extension`, `lumen_runtime::run_with`, `lumenc::run_with`,
`AppBuilder::rhai_extension` and the `lumenui::rhai` re-export are gone. Expose
a function with `AppBuilder::native_fn` or `AppBuilder::script_fn` (or
`RunOptions::with_native_fn` / a plugin's `add_script_fn`), which every
language reaches.

**You select cargo features.** The `host-rhai`, `host-lua` and `host-candela`
features are gone from `lumen`, `lumen-runtime`, `lumenui`, `lumen-portable`,
`lumen-prerender`, `lumen-ssr` and `lumen-web-runtime`; remove them from your
manifest. The crates `lumen-script-candela`, `lumen-script-rhai` and
`lumen-script-lua` are now `lumen-candela` plus `lumen-candela-dev`,
`lumen-rhai` and `lumen-lua`. A browser assembly installs
`lumen_candela::CandelaPlugin` itself before it hands a program to
`lumen_portable::hosts::install`.

**You built a portable plugin.** The plugin wire moved to version 4: a
function no longer says which languages see it (`PluginFnBuilder::hosts` and
`lumen_script::HostSet` are gone; every language sees every function).
Rebuild the plugin against this release; an older one is refused at load.

**You load precompiled apps.** The artifact format moved again: a candela
program now ships as bytecode alone, with the module that runs it named. See
"Rebuild every `.lmna` artifact".

**You install with `--no-modules`.** The script hosts are in the modules
archive, so a toolchain installed without it runs and builds no script.

**You use the LSP's Rhai support.** `lumen-lsp`'s `lang-rhai` feature is now
off by default; build with `--features lang-rhai` to keep `.rhai` diagnostics.
