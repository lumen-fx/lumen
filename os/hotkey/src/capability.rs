//! The `os-hotkey` capability: the global-hotkey manager and the commands
//! that register through it.
//!
//! Installed only for an app whose sources call `register_hotkey` (or whose
//! sources cannot be read): the manager opens an X11 connection on Linux,
//! which a hotkey-free app should not pay for. When the platform refuses a
//! manager, nothing is installed and the builtins say so.

use bevy_ecs::message::{MessageReader, Messages};
use bevy_ecs::prelude::*;
use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_core::tick::TickStage;
use lumen_script::{ScriptCommand, ScriptCommandEvent, ScriptSet};

use crate::HotkeyRegistry;

/// The builtin names that mean the app uses this subsystem.
pub const MARKERS: &[&str] = &["register_hotkey"];

/// What a static package looks for in the app's sources before it
/// carries this subsystem.
pub const SELECT: Select = Select::OnUse(MARKERS);

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, env: &CapabilityEnv) {
    if !env.sources_mention(MARKERS) {
        return;
    }
    let Some(registry) = HotkeyRegistry::new() else {
        return;
    };
    // Some platforms own a main-thread channel, so the manager is non-send.
    app.world.insert_non_send(registry);
    // A press or release reaches the script on the tick it was polled: the
    // poll writes plugin events ahead of the drain that delivers them.
    lumen_script::register_plugin_event_message(&mut app.world);
    app.add_systems(
        TickStage::Systems,
        crate::poll_hotkeys.before(lumen_script::collect_plugin_events),
    );
    app.world.init_resource::<Messages<ScriptCommandEvent>>();
    app.add_systems(
        TickStage::Systems,
        apply_hotkey_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::DomInput)
            .after(ScriptSet::Frame)
            .after(ScriptSet::Fill),
    );
}

/// Register and unregister the hotkeys the scripts ask for.
fn apply_hotkey_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut hotkeys: NonSendMut<HotkeyRegistry>,
) {
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::RegisterHotkey { name, accelerator } => {
                hotkeys.register(name, accelerator);
            }
            ScriptCommand::UnregisterHotkey { name } => hotkeys.unregister(name),
            _ => {}
        }
    }
}
