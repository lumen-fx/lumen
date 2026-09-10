//! The `os-power` capability: the sleep-inhibit holder and the two commands
//! that drive it. The holder only talks to the platform once a script asks
//! to keep the machine awake, so it is installed for every app.

use bevy_ecs::message::{MessageReader, Messages};
use bevy_ecs::prelude::*;
use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_core::tick::TickStage;
use lumen_script::{ScriptCommand, ScriptCommandEvent, ScriptSet};

use crate::{InhibitHolder, InhibitKinds};

/// What a static package looks for in the app's sources before it
/// carries this subsystem.
pub const SELECT: Select = Select::OnUse(&["keep_awake"]);

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, env: &CapabilityEnv) {
    // The name the platform shows beside the inhibit: the declared app id,
    // else the toolchain's own.
    let app_name = env
        .declared_app_id
        .clone()
        .unwrap_or_else(|| "lumen".to_string());
    app.world
        .insert_non_send(InhibitHolder::new().with_app_name(app_name));
    app.world.init_resource::<Messages<ScriptCommandEvent>>();
    app.add_systems(
        TickStage::Systems,
        apply_power_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::DomInput)
            .after(ScriptSet::Frame)
            .after(ScriptSet::Fill),
    );
}

/// Start and stop the inhibits the scripts ask for.
fn apply_power_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut inhibits: NonSendMut<InhibitHolder>,
) {
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::KeepAwake { name, reason } => {
                inhibits.start(
                    name,
                    reason,
                    InhibitKinds::DISPLAY.union(InhibitKinds::SUSPEND),
                );
            }
            ScriptCommand::AllowSleep { name } => inhibits.stop(name),
            _ => {}
        }
    }
}
