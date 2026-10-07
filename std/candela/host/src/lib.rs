//! The candela script host a shipped Lumen app runs.
//!
//! candela is Lumen's script language. A shipped app carries its program as a
//! `.cdlb` image the build compiled ahead of time, and this crate runs that
//! image on `candela-vm`, with no compiler in the process: [`CandelaVmHost`]
//! loads it, binds the `host "lumen" { ... }` declarations it recorded against
//! the builtins registered here, and drives its handlers through the same
//! registries every Lumen host shares.
//!
//! The compiler, hot reload, and the ahead-of-time checks are the edit loop's
//! business and live in `lumen-candela-dev`, which builds on this crate and
//! registers the same builtin list through [`HostFnSink`], so a compiled
//! program and an image bind as strictly as each other.
//!
//! The `lumen-candela` runtime module packages this crate. Installing
//! [`CandelaPlugin`] registers the `candela` language with the app (see
//! `lumen_script::ScriptLanguage`); the app assembly installs the host over
//! the program from there. A packaged app opens the bundled `lumen-candela`
//! module; a binary with no shared engine links it in, and a browser page
//! installs the plugin itself.
//!
//! # Builtins
//!
//! candela reaches builtins through a typed `host "lumen" { ... }` block, so a
//! script opts into the ones it uses, or into the whole surface with one line,
//! `import "lumen.cdl";`, which [`resolve_prelude`] splices into the
//! equivalent block before compilation (see the [`prelude`] module).
//!
//! A builtin whose value has no single concrete shape (an array signal's
//! records, an `http` request map, `parse_json`'s result) registers
//! variadically and is declared with a `...` argument list and, where it
//! returns a value, the `any` return type candela's type checker treats
//! permissively. `signal<T>(name)` is a prelude struct over the name-keyed
//! `signal_get_*` / `signal_set_*` builtins rather than a host function.

#![warn(missing_docs)]

pub mod builtins;
#[doc(hidden)]
pub mod declare;
#[doc(hidden)]
pub mod host_fns;
#[doc(hidden)]
pub mod library_dir;
pub mod lmn;
mod node_fns;
mod pages;
pub mod prelude;
#[doc(hidden)]
pub mod value;
#[doc(hidden)]
pub mod vm_host;
#[doc(hidden)]
pub mod vm_panic;

use bevy_ecs::prelude::World;
use lumen_core::prelude::{App, Plugin};
use lumen_core::warn_line;
use lumen_script::{
    ScriptFn, ScriptHostAccess, ScriptLanguage, ScriptLanguageAppExt, ScriptLoadFailure,
    ScriptProgram,
};

pub use builtins::{BUILTINS, BuiltinFn, BuiltinParam};
pub use host_fns::{HOST_NAMESPACE, HostFnSink, NATIVE_NAMESPACE};
pub use prelude::{PRELUDE_MODULE, PRELUDE_SOURCE, resolve_prelude};
pub use vm_host::{CandelaVmHost, ScriptCandelaVmPlugin, image_exports};

// The runtime half of the toolchain, for an embedder that drives the host
// directly.
pub use candela_vm;

/// The name the language is registered and declared under.
pub const LANGUAGE: &str = "candela";

/// Every builtin a candela host binds under the `lumen` namespace: the table
/// every Lumen host shares, with the page writer and history steps spelled the
/// candela way, and the DOM and event surface candela reaches through free
/// functions over an `int` node id.
pub fn builtin_fns() -> Vec<ScriptFn> {
    let mut fns = Vec::new();
    for f in lumen_script::builtin_script_fns() {
        if f.name == "page_current" {
            fns.push(pages::page());
            fns.push(f);
            fns.extend(pages::steps());
        } else {
            fns.push(f);
        }
    }
    fns.extend(node_fns::node_script_fns());
    fns
}

/// Registers the `candela` language, run from precompiled bytecode.
#[derive(Debug, Clone, Copy, Default)]
pub struct CandelaPlugin;

impl Plugin for CandelaPlugin {
    fn build(self, app: &mut App) {
        app.add_script_language(language());
    }
}

/// The `candela` language as this crate runs it: a program's bytecode on
/// [`CandelaVmHost`].
pub fn language() -> ScriptLanguage {
    let mut language = ScriptLanguage::new(LANGUAGE, install, access);
    language.image_exports =
        Some(|image, fns| image_exports(image, fns).map_err(|e| e.to_string()));
    language
}

/// Install a [`CandelaVmHost`] over `program`'s bytecode.
fn install(app: &mut App, program: ScriptProgram, multi_host: bool) {
    let Some(image) = program.bytecode else {
        let message = format!(
            "{}: the candela program has no bytecode, and this host runs precompiled .cdlb \
             images only; build the app with `lumenc build` or `lumenc package`",
            program.uri
        );
        warn_line!("lumen-candela: {message}");
        app.world.insert_resource(ScriptLoadFailure(message));
        return;
    };
    let mut plugin = ScriptCandelaVmPlugin::new(image).with_uri(program.uri);
    if let Some(dir) = program.lib_dir {
        plugin = plugin.with_library_dir(dir);
    }
    app.add_plugin(plugin);
    lumen_scene::script_host::install::<CandelaVmHost>(app, multi_host);
}

/// Reach an installed [`CandelaVmHost`].
pub fn access() -> ScriptHostAccess {
    ScriptHostAccess::of::<CandelaVmHost>(exports)
}

/// The functions the loaded image exports: defined in the built file, not
/// `main`, and annotating every parameter.
fn exports(world: &World) -> Vec<String> {
    world
        .get_resource::<CandelaVmHost>()
        .map(CandelaVmHost::exports)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_name_is_bound_once() {
        let mut seen = std::collections::HashSet::new();
        for f in builtin_fns() {
            assert!(seen.insert(f.name.clone()), "`{}` is bound twice", f.name);
        }
    }
}
