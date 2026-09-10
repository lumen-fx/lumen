//! The `os-hotkey` capability: the global-hotkey manager and the commands
//! that register through it.
//!
//! Installed only for an app whose sources call `register_hotkey` (or whose
//! sources cannot be read): the manager opens an X11 connection on Linux,
//! which a hotkey-free app should not pay for. When the platform refuses a
//! manager, nothing is installed and the builtins say so.

use bevy_ecs::message::{MessageReader, Messages};
use bevy_ecs::prelude::*;
use lumen_capability::CapabilityEnv;
use lumen_core::app::App;
use lumen_core::tick::TickStage;
use lumen_script::{ScriptCommand, ScriptCommandEvent, ScriptSet};

use crate::HotkeyRegistry;

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, env: &CapabilityEnv) {
    if !env.sources_mention(&["register_hotkey"]) {
        return;
    }
    let Some(registry) = HotkeyRegistry::new() else {
        return;
    };
    // Some platforms own a main-thread channel, so the manager is non-send.
    // `HotkeyPressed` and `HotkeyReleased` are registered beside every other
    // input message, so the poll writes them with no registration here.
    app.world.insert_non_send(registry);
    app.add_systems(TickStage::Systems, crate::poll_hotkeys);
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
