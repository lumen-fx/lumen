//! The `os-launcher` capability: the URL and file launcher, and the three
//! commands that call it. Stateless and idle until called, so it is
//! installed for every app.

use bevy_ecs::message::{MessageReader, Messages};
use bevy_ecs::prelude::*;
use lumen_capability::CapabilityEnv;
use lumen_core::app::App;
use lumen_core::tick::TickStage;
use lumen_script::{ScriptCommand, ScriptCommandEvent, ScriptSet};

use crate::{Launcher, OpenResult};

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    app.world.insert_resource(Launcher::new());
    app.world.init_resource::<Messages<ScriptCommandEvent>>();
    app.add_systems(
        TickStage::Systems,
        apply_launcher_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::DomInput)
            .after(ScriptSet::Frame)
            .after(ScriptSet::Fill),
    );
}

/// Open the URLs and paths the scripts ask for. Paths resolve against the
/// app directory.
fn apply_launcher_commands(mut events: MessageReader<ScriptCommandEvent>, launcher: Res<Launcher>) {
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::OpenUrl { url } => {
                report_launch(&launcher.open_url(url), "open_url", url)
            }
            ScriptCommand::OpenPath { path } => {
                let resolved = lumen_core::app_paths::resolve(path);
                report_launch(
                    &launcher.open_path(&resolved),
                    "open_path",
                    &resolved.display().to_string(),
                );
            }
            ScriptCommand::RevealPath { path } => {
                let resolved = lumen_core::app_paths::resolve(path);
                report_launch(
                    &launcher.reveal_in_file_manager(&resolved),
                    "reveal_path",
                    &resolved.display().to_string(),
                );
            }
            _ => {}
        }
    }
}

/// Log a failed launch. Success is silent: the platform helper exiting
/// zero says the handler started, not that the user saw anything, so
/// there is nothing useful to report.
fn report_launch(result: &OpenResult, builtin: &str, target: &str) {
    if let OpenResult::Failed(msg) = result {
        eprintln!("lumenc: {builtin}('{target}'): {msg}");
    }
}
