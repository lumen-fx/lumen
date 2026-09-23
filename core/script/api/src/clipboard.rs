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
use lumen_core::input::ClipboardRead;
use lumen_core::warn_line;
use lumen_os_clipboard::ClipboardHost;

use crate::ScriptCommand;
use crate::runtime::ScriptCommandEvent;

/// Reads that have an answer and have not been delivered yet.
///
/// A desktop read answers before the request call returns, so its reply is
/// delivered by the same run of the applier. A page answers from a promise,
/// on the page's own queue between ticks, and the reply waits here for the
/// next run.
pub struct PendingClipboardReads {
    /// Handed to each read, which sends `(tag, text)` once it has the text.
    tx: Sender<(String, String)>,
    /// Drained into [`ClipboardRead`] messages every run.
    rx: Receiver<(String, String)>,
}

impl Default for PendingClipboardReads {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx }
    }
}

/// Carry out the clipboard commands a script queued, and deliver the reads
/// that have an answer as [`ClipboardRead`] messages.
pub fn apply_clipboard_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    clipboard: Option<NonSend<ClipboardHost>>,
    pending: Local<PendingClipboardReads>,
    mut out: MessageWriter<ClipboardRead>,
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
        out.write(ClipboardRead { tag, text });
    }
}
