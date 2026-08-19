//! Applying the script commands that touch nothing but the scene.
//!
//! A script's builtins queue commands rather than mutating the world, and the
//! appliers are grouped by what they need to reach. This one needs the scene
//! and nothing else: a signal, an array, a property, an element's text. Every
//! host has those, which is why they are here rather than beside the ones
//! that open a file dialog or register a tray icon.
//!
//! A host registers this alongside its own applier, with the same ordering
//! edges: everything that reads a signal is dirty-gated on the tick the write
//! lands, so a write applied after its reader is a write its reader never
//! sees.
//!
//! The desktop runtime does not use this yet; its own applier still carries
//! these arms. Moving it over is one system away and is held up by the
//! desktop applier's position in the schedule, which rests on how the
//! executor breaks a tie rather than on an edge: adding a second reader of
//! the command stream moves it ahead of the handler dispatch that fills the
//! stream, and the edge that would pin it closes a cycle through the text
//! editor.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_core::components::{LumenId, TextContent, TextInput};
use lumen_core::property_store::{PropertyStore, push_external_property};
use lumen_core::signals::ArraySignals;
use lumen_core::warn_line;
use lumen_script::ScriptCommand;
use lumen_script::runtime::ScriptCommandEvent;

/// Apply the commands whose whole effect is on the scene.
///
/// Commands this does not answer for are left for the applier that holds
/// what they need; each command has exactly one applier.
pub fn apply_scene_script_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    ids: Query<(Entity, &LumenId)>,
    mut texts: Query<&mut TextContent>,
    mut inputs: Query<&mut TextInput>,
    mut store: ResMut<PropertyStore>,
    mut array_signals: ResMut<ArraySignals>,
) {
    for event in events.read() {
        match &event.0 {
            ScriptCommand::Print(line) => warn_line!("[script] {line}"),
            ScriptCommand::SetText { target_id, text } => {
                for (entity, id) in &ids {
                    if id.0 != *target_id {
                        continue;
                    }
                    if let Ok(mut content) = texts.get_mut(entity) {
                        content.0 = text.clone();
                    }
                    // Replacing text from a script must clamp the caret, or
                    // it points past the end of the new buffer and the next
                    // keystroke inserts out of bounds.
                    if let Ok(mut input) = inputs.get_mut(entity) {
                        input.cursor = text.len();
                    }
                }
            }
            ScriptCommand::SetSignal { name, value } => {
                store.set_global_str(name, value.as_str());
            }
            ScriptCommand::SetArray { name, items } => {
                array_signals.set(name, items.clone());
            }
            ScriptCommand::SetProperty { key, value } => {
                // A typed write, coalesced onto the next tick. A host that
                // wants the immediate cross-thread path calls
                // `push_external_property` itself.
                push_external_property(key.clone(), value.clone());
            }
            _ => {}
        }
    }
}
