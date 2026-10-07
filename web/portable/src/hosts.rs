//! Installing an app's program, and reaching its host, without naming a
//! language.
//!
//! A host is a module that registers its language with the app (see
//! `lumen_script::ScriptLanguage`). Whoever assembles the app installs the
//! modules it runs scripts with first; a browser page, which cannot open a
//! library, installs each one's plugin itself. From there a compiled app's
//! program goes to the host its manifest names, and what comes back is
//! [`ScriptHostAccess`]: the host's signals and exports, in [`ScriptHost`]
//! terms.
//!
//! [`ScriptHost`]: lumen_script::ScriptHost

use bevy_ecs::prelude::World;
use lumen_core::prelude::App;
use lumen_script::{ScriptLanguages, ScriptProgram, UnknownLanguage};

pub use lumen_script::ScriptHostAccess;

/// Install the host registered for `engine` over `program`, the compiled
/// program in the form that engine loads. `uri` names the program in a load
/// error.
///
/// # Errors
///
/// No module the app installed registered `engine`.
pub fn install(
    app: &mut App,
    engine: &str,
    program: &[u8],
    uri: &str,
) -> Result<ScriptHostAccess, UnknownLanguage> {
    lumen_script::install_program(
        app,
        engine,
        ScriptProgram {
            bytecode: Some(program.to_vec()),
            uri: uri.to_owned(),
            ..ScriptProgram::default()
        },
        false,
    )
}

/// The access table for the host of `engine`, for an app that installed it
/// as part of assembling and now wants to read through it.
pub fn access(world: &World, engine: &str) -> Option<ScriptHostAccess> {
    world
        .get_resource::<ScriptLanguages>()
        .and_then(|languages| languages.get(engine))
        .map(|language| (language.access)())
}

#[cfg(test)]
mod tests {
    use super::install;
    use lumen_core::prelude::App;
    use lumen_script::{ScriptError, ScriptValue};

    /// The image the build script compiled, the same one the browser suite
    /// loads.
    const SMOKE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/smoke.cdlb"));

    /// Every entry is a function pointer resolved before the host it names is
    /// installed, so the table answers rather than panics when the resource is
    /// absent.
    #[test]
    fn reaching_a_host_that_was_never_installed_is_reported_not_a_panic() {
        let mut world = bevy_ecs::prelude::World::new();
        let access = lumen_candela_host::access();

        assert_eq!((access.signal)(&world, "greeting"), None);
        let Err(ScriptError::Runtime(message)) = (access.call)(&mut world, "bump") else {
            panic!("calling into a world with no host resource is a failure a caller can show");
        };
        assert!(message.contains("no script is loaded"), "{message}");
    }

    #[test]
    fn the_installed_host_answers_for_the_program_the_app_shipped() {
        let mut app = App::new();
        app.add_plugin(lumen_candela_host::CandelaPlugin);
        let host = install(&mut app, "candela", SMOKE, "smoke.cdlb")
            .expect("the plugin registered candela");

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
        assert!(super::access(&app.world, "candela").is_some());
    }

    #[test]
    fn an_engine_no_host_answers_for_is_refused_by_name() {
        let mut app = App::new();
        let refusal = install(&mut app, "brainfuck", b"", "program")
            .err()
            .expect("no module registered that")
            .to_string();

        assert!(refusal.contains("brainfuck"), "{refusal}");
    }
}
