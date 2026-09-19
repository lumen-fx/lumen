//! The `os-tray` capability: the tray service, the per-tick click drain, and
//! the two tray commands.
//!
//! The service is empty until a script registers an icon, and the drain is
//! a cheap queue check, so it is installed for every app.

use std::path::PathBuf;

use bevy_ecs::message::{MessageReader, Messages};
use bevy_ecs::prelude::*;
use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_core::tick::TickStage;
use lumen_script::{ScriptCommand, ScriptCommandEvent, ScriptSet};

use crate::{TrayConfig, TrayMenu, TrayService};

/// What a static package looks for in the app's sources before it
/// carries this subsystem.
pub const SELECT: Select = Select::OnUse(&["tray_icon"]);

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    app.world.insert_non_send(TrayService::new());
    // Every target has a `poll_tray_events`: `tray-icon` backs macOS and
    // Windows, `ksni` backs Linux, and anything else gets an inert stub.
    app.add_systems(TickStage::Systems, crate::poll_tray_events);
    app.world.init_resource::<Messages<ScriptCommandEvent>>();
    app.add_systems(
        TickStage::Systems,
        apply_tray_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::DomInput)
            .after(ScriptSet::Frame)
            .after(ScriptSet::Fill),
    );
}

/// Register and unregister the tray icons the scripts ask for. Icon paths
/// resolve against the app directory the runtime published.
fn apply_tray_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut tray: NonSendMut<TrayService>,
) {
    let dir: PathBuf = lumen_core::app_paths::app_dir();
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::RegisterTrayIcon {
                id,
                icon_path,
                tooltip,
                menu,
                template,
            } => {
                let cfg = TrayConfig {
                    id: id.clone(),
                    icon_path: PathBuf::from(icon_path),
                    tooltip: tooltip.clone(),
                    menu: (!menu.is_empty()).then(|| TrayMenu::parse(menu)),
                    template: *template,
                };
                tray.register(&cfg, &dir);
            }
            ScriptCommand::UnregisterTrayIcon { id } => tray.unregister(id),
            _ => {}
        }
    }
}
