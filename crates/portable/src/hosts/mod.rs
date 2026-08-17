//! Choosing a script host, and reaching one, without naming a language.
//!
//! An app declares the engine that runs its scripts in the manifest, and this
//! module installs the host this build carries for that name. A host lives
//! behind one feature (`host-<engine>`) and one module here; nothing outside
//! this module mentions a host type. What the rest of the crate gets back is
//! [`ScriptHostAccess`], whose entries are [`ScriptHost`] calls plus the one
//! thing that trait does not carry, the export list.
//!
//! Adding a language is a feature, a module, and an arm.

// A build with no host compiled in installs none, so it constructs no access
// table; the seam still compiles, and reports the missing host through the same
// path a build with hosts uses for an engine none of them claims. Extend the
// list when a host joins.
#![cfg_attr(not(any(feature = "host-candela")), allow(dead_code))]

#[cfg(feature = "host-candela")]
mod candela;

use std::error::Error;
use std::fmt;

use bevy_ecs::component::Mutable;
use bevy_ecs::prelude::{Resource, World};
use lumen_core::prelude::App;
use lumen_script::{ScriptError, ScriptHost, ScriptValue};

/// Every engine name this build has a host for.
pub const COMPILED_ENGINES: &[&str] = &[
    #[cfg(feature = "host-candela")]
    candela::ENGINE,
];

/// The app declared an engine no compiled-in host answers for.
#[derive(Debug)]
pub struct UnknownEngine(String);

impl fmt::Display for UnknownEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no script host for engine \"{}\"; ", self.0)?;
        if COMPILED_ENGINES.is_empty() {
            f.write_str("this runtime was built with no script host")
        } else {
            write!(
                f,
                "this runtime was built with: {}",
                COMPILED_ENGINES.join(", ")
            )
        }
    }
}

impl Error for UnknownEngine {}

/// Reaching the installed host, in the terms a caller asks in.
///
/// Every entry resolves the host resource itself, so the app owns no borrow of
/// it between calls.
pub struct ScriptHostAccess {
    /// The value the script last wrote to a signal, from the host's mirror.
    pub signal: fn(&World, &str) -> Option<ScriptValue>,
    /// Call an exported function with no arguments, returning what it returned
    /// when the host has such a function.
    pub call: fn(&mut World, &str) -> Result<Option<ScriptValue>, ScriptError>,
    /// The names a caller may call.
    pub exports: fn(&World) -> Vec<String>,
}

impl ScriptHostAccess {
    /// The table an app with no script answers through: every entry reports
    /// nothing rather than the caller having to ask whether there is a host.
    pub fn absent() -> Self {
        Self {
            signal: |_, _| None,
            call: |_, _| Err(ScriptError::Runtime("no script is loaded".to_owned())),
            exports: |_| Vec::new(),
        }
    }

    /// Access table for a host stored as the resource `H`. `exports` comes from
    /// the host module: [`ScriptHost`] carries no export list, and what counts
    /// as an exported name is the host's own rule, so a host without one
    /// answers with an empty list rather than every host answering for a
    /// question only some can.
    fn of<H>(exports: fn(&World) -> Vec<String>) -> Self
    where
        H: ScriptHost + Resource<Mutability = Mutable>,
    {
        Self {
            signal: signal_of::<H>,
            call: call_of::<H>,
            exports,
        }
    }
}

/// The access table for an engine, without installing anything.
///
/// For a caller that installed the host as part of assembling an app and
/// now wants to read through it.
pub fn access(engine: &str) -> Option<ScriptHostAccess> {
    match engine {
        #[cfg(feature = "host-candela")]
        candela::ENGINE => Some(candela::access()),
        _ => None,
    }
}

/// Install the host that runs `engine` over `program`, the script in whatever
/// form that engine loads. `uri` names the program in a load error.
pub fn install(
    app: &mut App,
    engine: &str,
    program: &[u8],
    uri: &str,
) -> Result<ScriptHostAccess, UnknownEngine> {
    match engine {
        #[cfg(feature = "host-candela")]
        candela::ENGINE => Ok(candela::install(app, program, uri)),
        _ => {
            let _ = (app, program, uri);
            Err(UnknownEngine(engine.to_owned()))
        }
    }
}

/// Read `name` from the host's signal mirror.
fn signal_of<H>(world: &World, name: &str) -> Option<ScriptValue>
where
    H: ScriptHost + Resource<Mutability = Mutable>,
{
    world.get_resource::<H>().and_then(|h| h.mirror_get(name))
}

/// Call `name` with no arguments. Commands the call queued are put back so the
/// next tick carries them, exactly as the app's own dispatchers do.
fn call_of<H>(world: &mut World, name: &str) -> Result<Option<ScriptValue>, ScriptError>
where
    H: ScriptHost + Resource<Mutability = Mutable>,
{
    let Some(mut host) = world.get_resource_mut::<H>() else {
        return Err(ScriptError::Runtime("no script is loaded".to_owned()));
    };
    let outcome = host.call(name, &[])?;
    host.push_commands(outcome.commands);
    Ok(outcome.ret.filter(|_| outcome.found))
}

#[cfg(test)]
mod tests {
    use super::{COMPILED_ENGINES, ScriptHostAccess, install};
    use bevy_ecs::prelude::World;
    use lumen_core::prelude::App;
    use lumen_script::{ScriptError, ScriptValue};

    /// The image the build script compiled, the same one the browser suite
    /// loads.
    #[cfg(feature = "host-candela")]
    const SMOKE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/smoke.cdlb"));

    /// Every entry is a function pointer resolved before the host it names is
    /// installed, so the table answers rather than panics when the resource is
    /// absent.
    #[cfg(feature = "host-candela")]
    #[test]
    fn reaching_a_host_that_was_never_installed_is_reported_not_a_panic() {
        use lumen_script_candela::CandelaVmHost;

        let mut world = World::new();
        let access = ScriptHostAccess::of::<CandelaVmHost>(|_| Vec::new());

        assert_eq!((access.signal)(&world, "greeting"), None);
        let Err(ScriptError::Runtime(message)) = (access.call)(&mut world, "bump") else {
            panic!("calling into a world with no host resource is a failure a caller can show");
        };
        assert!(message.contains("no script is loaded"), "{message}");
    }

    #[cfg(feature = "host-candela")]
    #[test]
    fn the_installed_host_answers_for_the_program_the_app_shipped() {
        let mut app = App::new();
        app.extract_fns.clear();
        let host =
            install(&mut app, "candela", SMOKE, "smoke.cdlb").expect("this build carries candela");

        assert!(
            (host.exports)(&app.world).iter().any(|e| e == "bump"),
            "the export list comes from the loaded image, not from a fixed list"
        );
        assert_eq!(
            (host.signal)(&app.world, "greeting"),
            Some(ScriptValue::Str("hello from candela".to_owned())),
            "on_start ran during the install and its write reads back through the table"
        );
        assert_eq!(
            (host.call)(&mut app.world, "bump").expect("an exported name runs"),
            Some(ScriptValue::I64(1)),
            "the call returns through the table"
        );
        assert_eq!(
            (host.call)(&mut app.world, "on_click").expect("a miss is not an error"),
            None,
            "a name the image does not export answers with nothing rather than failing"
        );
    }

    #[test]
    fn an_engine_no_host_answers_for_is_refused_by_name() {
        let mut app = App::new();
        let refusal = install(&mut app, "brainfuck", b"", "program")
            .err()
            .expect("no build of this crate carries a host for that")
            .to_string();

        assert!(refusal.contains("brainfuck"), "{refusal}");
        assert!(
            refusal.contains(
                COMPILED_ENGINES
                    .first()
                    .copied()
                    .unwrap_or("no script host")
            ),
            "the refusal says what this build does carry: {refusal}"
        );
    }
}
