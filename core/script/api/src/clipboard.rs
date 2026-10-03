//! The applier behind the `clipboard_write` and `clipboard_read` builtins.
//!
//! The backend is whatever [`ClipboardHost`] the assembly put in the world:
//! the OS clipboard on the desktop, the page's `navigator.clipboard` in a
//! browser. This applier is the same on both, and an app with no host in the
//! world still answers every read, with empty text, so a script waiting on
//! `on_clipboard(tag, text)` is never left hanging.

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use lumen_core::warn_line;
use lumen_os_clipboard::ClipboardHost;

use crate::runtime::ScriptCommandEvent;
use crate::{PluginEvent, ScriptCommand, ScriptValue};

/// Reads that have an answer and have not been delivered yet.
///
/// A desktop read answers before the request call returns, so its reply is
/// delivered by the same run of the applier. A page answers from a promise,
/// on the page's own queue between ticks, and the reply waits here for the
/// next run.
pub struct PendingClipboardReads {
    /// Handed to each read, which sends `(tag, text)` once it has the text.
    tx: Sender<(String, String)>,
    /// Drained into `on_clipboard` events every run.
    rx: Receiver<(String, String)>,
}

impl Default for PendingClipboardReads {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx }
    }
}

/// Carry out the clipboard commands a script queued, and deliver the reads
/// that have an answer as [`clipboard_read_event`]s.
pub fn apply_clipboard_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    clipboard: Option<NonSend<ClipboardHost>>,
    pending: Local<PendingClipboardReads>,
    mut out: MessageWriter<PluginEvent>,
) {
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::ClipboardWrite { text } => match clipboard.as_deref() {
                Some(clip) => {
                    if !clip.write_text(text) {
                        warn_line!("clipboard_write: the clipboard refused the text");
                    }
                }
                None => warn_line!("clipboard_write: no clipboard backend"),
            },
            ScriptCommand::ClipboardRead { tag } => {
                let tx = pending.tx.clone();
                let tag = tag.clone();
                match clipboard.as_deref() {
                    Some(clip) => clip.read_text_then(move |text| {
                        let _ = tx.send((tag, text));
                    }),
                    None => {
                        let _ = tx.send((tag, String::new()));
                    }
                }
            }
            _ => {}
        }
    }
    for (tag, text) in pending.rx.try_iter() {
        out.write(clipboard_read_event(tag, text));
    }
}

/// The event one answered `clipboard_read(tag)` reaches the script as:
/// `on_clipboard(tag, text)`, with a per-tag `on("clipboard", tag, fn)`
/// winning. A clipboard holding no text answers with empty text, so a script
/// waiting on the read can still clear its pending state.
pub fn clipboard_read_event(tag: String, text: String) -> PluginEvent {
    PluginEvent::Call {
        event: "clipboard".to_string(),
        key: tag,
        fallback: "on_clipboard".to_string(),
        args: vec![ScriptValue::Str(text)],
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::message::{MessageRegistry, Messages};

    use super::*;

    /// An app with no clipboard in the world still answers a read, with empty
    /// text, and a write goes nowhere without taking the tick down.
    #[test]
    fn with_no_clipboard_a_read_answers_empty_and_a_write_is_dropped() {
        let mut world = World::new();
        MessageRegistry::register_message::<ScriptCommandEvent>(&mut world);
        crate::runtime::register_plugin_event_message(&mut world);
        world.write_message(ScriptCommandEvent(ScriptCommand::ClipboardWrite {
            text: "copied".to_string(),
        }));
        world.write_message(ScriptCommandEvent(ScriptCommand::ClipboardRead {
            tag: "paste".to_string(),
        }));

        let mut schedule = Schedule::default();
        schedule.add_systems(apply_clipboard_commands);
        schedule.run(&mut world);

        let reads = world.resource::<Messages<PluginEvent>>();
        let delivered: Vec<(&str, &str, &str, &[ScriptValue])> = reads
            .iter_current_update_messages()
            .filter_map(|event| match event {
                PluginEvent::Call {
                    event,
                    key,
                    fallback,
                    args,
                } => Some((event.as_str(), key.as_str(), fallback.as_str(), &args[..])),
                PluginEvent::Commands(_) => None,
            })
            .collect();
        assert_eq!(
            delivered,
            [(
                "clipboard",
                "paste",
                "on_clipboard",
                &[ScriptValue::Str(String::new())][..]
            )]
        );
    }
}
