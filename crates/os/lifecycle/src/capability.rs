//! The `os-lifecycle` capability: single-instance launch, recent files,
//! autostart, and the commands that drive them.
//!
//! The three services are a few cloned paths each, no thread and no
//! connection, so they are installed for every app. Single-instance locking
//! is the exception: binding a socket is not side-effect free, so it happens
//! in the preflight of an interactive launch only, never for a headless run,
//! a test, or an SDK embedding, which would otherwise fight over one socket.
//! A second instance forwards its argv to the first and exits from that
//! preflight before any other work starts.

use std::sync::Mutex;

use bevy_ecs::message::{MessageReader, MessageWriter, Messages};
use bevy_ecs::prelude::*;
use lumen_capability::{CapabilityEnv, Preflight};
use lumen_core::app::App;
use lumen_core::tick::TickStage;
use lumen_script::{ScriptCommand, ScriptCommandEvent, ScriptSet};
use serde::Deserialize;

use crate::{
    AppId, AutostartService, LifecycleService, RecentFile, RecentFilesService, SingleInstance,
};

/// The service the preflight bound, handed to the install so the per-tick
/// poll drains the inbox the listener thread feeds rather than a second,
/// never-bound one.
static BOUND: Mutex<Option<LifecycleService>> = Mutex::new(None);

/// The one `[app]` key this capability reads.
#[derive(Default, Deserialize)]
struct AppSection {
    #[serde(default)]
    single_instance: bool,
}

/// The interactive-launch preflight. See the module docs.
pub fn preflight(env: &CapabilityEnv) -> Preflight {
    if !env.section::<AppSection>("app").single_instance {
        return Preflight::Continue;
    }
    let lifecycle = LifecycleService::new();
    let id = AppId::from(env.app_id.clone());
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match lifecycle.ensure_single_instance(&id, &argv) {
        SingleInstance::Secondary { .. } => Preflight::Exit,
        SingleInstance::Primary => {
            *BOUND.lock().unwrap_or_else(|e| e.into_inner()) = Some(lifecycle);
            Preflight::Continue
        }
    }
}

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, env: &CapabilityEnv) {
    let id = AppId::from(env.app_id.clone());
    let lifecycle = BOUND
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .unwrap_or_default();
    let recent = RecentFilesService::new(lifecycle.data_dir(&id));
    // The autostart entry has to point somewhere; a `lumenc run` dev session
    // launches through the `lumenc` binary itself, which is the best answer
    // available without a packaged app's own launcher path.
    let exe = std::env::current_exe().unwrap_or_default();
    let autostart = AutostartService::new(id, exe);
    app.world.insert_resource(lifecycle);
    app.world.insert_resource(recent);
    app.world.insert_resource(autostart);
    app.add_systems(TickStage::Systems, crate::poll_second_instance);
    app.world.init_resource::<Messages<ScriptCommandEvent>>();
    app.add_systems(
        TickStage::Systems,
        apply_lifecycle_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::DomInput)
            .after(ScriptSet::Frame)
            .after(ScriptSet::Fill),
    );
}

/// Apply the recent-files and autostart commands the scripts issue.
fn apply_lifecycle_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    recent_files: Res<RecentFilesService>,
    autostart: Res<AutostartService>,
    mut recent_files_out: MessageWriter<lumen_core::input::RecentFilesRead>,
    mut autostart_out: MessageWriter<lumen_core::input::AutostartRead>,
) {
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::AddRecentFile { path, label } => {
                let resolved = lumen_core::app_paths::resolve(path);
                let label = (!label.is_empty()).then(|| label.clone());
                recent_files.add(RecentFile {
                    path: resolved,
                    label,
                    mime: None,
                    last_opened: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                });
            }
            ScriptCommand::ListRecentFiles { tag } => {
                let paths = recent_files
                    .list(recent_files.max_entries)
                    .into_iter()
                    .map(|e| e.path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("|");
                recent_files_out.write(lumen_core::input::RecentFilesRead {
                    tag: tag.clone(),
                    paths,
                });
            }
            ScriptCommand::ClearRecentFiles => recent_files.clear(),
            ScriptCommand::SetAutostart { on } => {
                if !autostart.set_enabled(*on) {
                    eprintln!("lumenc: set_autostart({on}): platform helper failed");
                }
            }
            ScriptCommand::QueryAutostart { tag } => match autostart.is_enabled() {
                Some(enabled) => {
                    autostart_out.write(lumen_core::input::AutostartRead {
                        tag: tag.clone(),
                        enabled,
                    });
                }
                // The platform helper could not resolve where to look (for
                // one, `HOME` unset). That is not the same thing as
                // "disabled", so the query goes unanswered rather than
                // reporting a state nobody observed.
                None => eprintln!(
                    "lumenc: query_autostart({tag}): could not resolve the autostart location"
                ),
            },
            _ => {}
        }
    }
}
