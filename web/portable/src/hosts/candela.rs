//! The candela host, behind the `host-candela` feature.
//!
//! The program it runs is a precompiled `.cdlb` image: the compiler stays out
//! of the runtime, so an app's `.cdl` is built to bytecode ahead of time and
//! the image's `host` declarations bind by name against the builtins the
//! artifact host registers.

use bevy_ecs::prelude::World;
use lumen_core::prelude::App;
use lumen_script_candela::{CandelaVmHost, ScriptCandelaVmPlugin};

use crate::hosts::{ScriptHostAccess, register_host_systems};

/// The engine name a manifest names this host by.
pub(crate) const ENGINE: &str = "candela";

/// Install the host over the `.cdlb` image `program`.
pub(crate) fn install(app: &mut App, program: &[u8], uri: &str) -> ScriptHostAccess {
    app.add_plugin(ScriptCandelaVmPlugin::new(program.to_vec()).with_uri(uri));
    register_host_systems::<CandelaVmHost>(app);
    access()
}

/// Reach a host that is already installed.
pub(crate) fn access() -> ScriptHostAccess {
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
    use lumen_script::ScriptHost;

    use super::{CandelaVmHost, ENGINE};

    #[test]
    fn the_engine_a_manifest_names_is_what_the_host_calls_itself() {
        assert_eq!(CandelaVmHost::new(Vec::new()).lang(), ENGINE);
    }
}
