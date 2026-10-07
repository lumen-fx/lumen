//! The candela script host the edit loop runs: the compiler, hot reload, and
//! the ahead-of-time checks and builds.
//!
//! [`CandelaHost`] compiles candela source with candela's `Engine` and runs
//! it, so a script can be edited and hot-reloaded while the app is up, and a
//! check can report a compile error without running anything.
//! [`CandelaHost::compile_bytecode`] is the build step that produces the
//! `.cdlb` image a shipped app runs on `lumen-candela`'s bytecode host,
//! folding in whatever the build had registered through
//! [`ScriptHost::register_script_fn`](lumen_script::ScriptHost::register_script_fn)
//! exactly as a live compile does, so a module or plugin function needs no
//! hand-written `host "<ns>" { .. }` block to reach the image.
//!
//! Both hosts register the identical builtin list, written once in
//! `lumen_candela_host::host_fns` behind [`HostFnSink`]. `candela::Engine` binds
//! those closures against the `host` declarations a fresh compile produced,
//! and `candela_vm::HostRegistry` binds them against the declarations an image
//! recorded; the checks are the same, so an image that loads is bound as
//! strictly as a script that compiles.
//!
//! The `lumen-candela-dev` runtime module packages this crate. Installing
//! [`CandelaDevPlugin`] registers the `candela` language with every entry the
//! edit loop and the build use: install over source, reload, check, compile,
//! read an image's exports, and read the `lmn!` blocks a script writes (see
//! [`markup`]). `lumenc` opens the bundled `lumen-candela-dev` module beside a
//! shared engine and links it in where there is none; a shipped app never
//! loads it.
//!
//! # Values and extension points
//!
//! candela's embedding `Value` carries `Array` and `Map` variants alongside
//! string / int / float / bool / null, and the host-fn marshalling accepts and
//! returns `Vec<T>` and `{string: T}` maps, so a
//! [`ScriptValue`](lumen_script::ScriptValue) round-trips structured values
//! recursively across `call` / `call_closure` / the signal mirror.
//!
//! - `derive(name, deps, f)`: the dep list marshals as a `string[]`, and,
//!   since a function is not one of the values that cross a host boundary, the
//!   recompute body is passed by the script function's *name*, which
//!   `ScriptHost::call_closure` re-invokes.
//! - `register_script_fn`: the host-neutral
//!   [`ScriptFn`](lumen_script::ScriptFn) an app, a plugin, the C ABI or the
//!   Rust SDK describes. A signature candela can name binds typed, so the call
//!   site is checked when the program compiles; a variadic or `any` signature
//!   binds as a `&[Value]` slice and is declared with a `...` arg list. A
//!   function that fails raises `host_fn_error` at the call, which the script
//!   can catch.

#![warn(missing_docs)]

mod compile_warnings;
mod diagnose;
mod dylib_check;
mod engine_host;
// The build script's install step. The library compiles it only to test it;
// cargo runs a build script but never tests one.
#[cfg(test)]
mod install;
pub mod markup;
mod package_entry;

use bevy_ecs::prelude::World;
use lumen_core::prelude::{App, Plugin};
use lumen_script::{
    ScriptCompile, ScriptHost, ScriptHostAccess, ScriptLanguage, ScriptLanguageAppExt,
    ScriptProgram,
};

pub use engine_host::{CandelaHost, CandelaScriptContext, ScriptCandelaPlugin};
pub use lumen_candela_host::{HOST_NAMESPACE, HostFnSink, LANGUAGE, NATIVE_NAMESPACE};

// The candela crate itself, so an embedder can name `candela::Engine` /
// `candela::Value` for a `ScriptCandelaPlugin::with_extension` closure
// without declaring its own dependency on it.
pub use candela;

/// Registers the `candela` language, run from source with the compiler.
#[derive(Debug, Clone, Copy, Default)]
pub struct CandelaDevPlugin;

impl Plugin for CandelaDevPlugin {
    fn build(self, app: &mut App) {
        app.add_script_language(language());
    }
}

/// The `candela` language as this crate runs it: source compiled on load and
/// on every edit, plus the checks and the build a toolchain runs ahead of
/// time.
pub fn language() -> ScriptLanguage {
    ScriptLanguage {
        name: LANGUAGE,
        install,
        reload: Some(lumen_script::reload_script::<CandelaHost>),
        access,
        check: Some(check),
        compile: Some(compile),
        image_exports: lumen_candela_host::language().image_exports,
        markup_blocks: Some(markup::markup_blocks),
    }
}

/// Install a [`CandelaHost`] over `program`'s source. A program that travels
/// as bytecode alone runs on the bytecode host instead, the way a shipped app
/// would run it.
fn install(app: &mut App, program: ScriptProgram, multi_host: bool) {
    if program.source.trim().is_empty() && program.bytecode.is_some() {
        (lumen_candela_host::language().install)(app, program, multi_host);
        return;
    }
    let mut plugin = ScriptCandelaPlugin::new(program.source)
        .with_uri(program.uri)
        .with_import_roots(program.import_roots)
        .with_cfg_flags(&program.cfg_flags);
    if let Some(dir) = program.lib_dir {
        plugin = plugin.with_library_dir(dir);
    }
    app.add_plugin(plugin);
    lumen_scene::script_host::install::<CandelaHost>(app, multi_host);
}

/// Reach the installed host: the compiler host, or the bytecode host a
/// bytecode-only program was given.
pub fn access() -> ScriptHostAccess {
    fn signal(world: &World, name: &str) -> Option<lumen_script::ScriptValue> {
        if world.contains_resource::<CandelaHost>() {
            (ScriptHostAccess::of::<CandelaHost>(|_| Vec::new()).signal)(world, name)
        } else {
            (lumen_candela_host::access().signal)(world, name)
        }
    }
    fn call(
        world: &mut World,
        name: &str,
    ) -> Result<Option<lumen_script::ScriptValue>, lumen_script::ScriptError> {
        if world.contains_resource::<CandelaHost>() {
            (ScriptHostAccess::of::<CandelaHost>(|_| Vec::new()).call)(world, name)
        } else {
            (lumen_candela_host::access().call)(world, name)
        }
    }
    fn exports(world: &World) -> Vec<String> {
        if world.contains_resource::<CandelaHost>() {
            Vec::new()
        } else {
            (lumen_candela_host::access().exports)(world)
        }
    }
    ScriptHostAccess {
        signal,
        call,
        exports,
    }
}

/// A compiler set up the way every ahead-of-time path sets it up: the app's
/// library directory, the packages it imports, the compile-time flags of the
/// target, the modules' functions declared, and their candela sources staged
/// ahead of the program.
///
/// # Errors
///
/// A function could not be registered.
pub fn compiler(against: &ScriptCompile<'_>) -> Result<CandelaHost, String> {
    let mut host = CandelaHost::new();
    if let Some(dir) = against.lib_dir {
        host.set_library_dir(dir);
    }
    for (name, dir) in against.import_roots {
        host.add_import_root(name.clone(), dir.clone());
    }
    host.set_cfg_flags(against.cfg_flags);
    for f in against.fns {
        host.register_script_fn(f).map_err(|e| e.to_string())?;
    }
    for prelude in against.preludes.iter().filter(|p| p.lang == LANGUAGE) {
        host.add_prelude(&prelude.ns, &prelude.source);
    }
    Ok(host)
}

/// Compile-check `source` without running it.
fn check(source: &str, against: &ScriptCompile<'_>) -> Result<(), String> {
    compiler(against)?
        .compile_check(source, against.uri)
        .map_err(|e| e.to_string())
}

/// Compile `source` to a `.cdlb` image.
fn compile(source: &str, against: &ScriptCompile<'_>) -> Result<(Vec<u8>, Vec<String>), String> {
    compiler(against)?
        .compile_bytecode(source, against.uri)
        .map_err(|e| e.to_string())
}
