//! The `os-filedialog` capability: the dialog service, the handler that turns
//! a resolved dialog into the `FilePicked` message, and the command that
//! opens one.
//!
//! The service is a single counter and opens nothing until asked, so it is
//! installed for every app. The executor a dialog resolves on is a
//! capability of its own; a build without one runs the dialog inline.

use bevy_ecs::message::{MessageReader, Messages};
use bevy_ecs::prelude::*;
use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_core::input::FilePicked;
use lumen_core::tick::TickStage;
use lumen_script::{ScriptCommand, ScriptCommandEvent, ScriptSet};

use crate::{FileDialogKind, FileDialogRequest, FileDialogResultCommand, FileDialogService};

/// What a static package looks for in the app's sources before it
/// carries this subsystem.
pub const SELECT: Select = Select::OnUse(lumen_script::FILE_DIALOG_BUILTINS);

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    app.world.insert_resource(FileDialogService::new());
    // A resolved dialog comes back as a typed command from whichever thread
    // ran it. Without a handler for that payload the command drain discards
    // it and the script's `on_file_picked` never fires, so this registration
    // is what closes the loop between `pick_file(...)` and the callback.
    app.register_command::<FileDialogResultCommand, _>(|world, payload| {
        world.write_message(FilePicked::from(*payload));
    });
    app.world.init_resource::<Messages<ScriptCommandEvent>>();
    app.add_systems(
        TickStage::Systems,
        apply_dialog_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::DomInput)
            .after(ScriptSet::Frame)
            .after(ScriptSet::Fill),
    );
}

/// Open the dialogs the scripts ask for.
fn apply_dialog_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    file_dialog: Res<FileDialogService>,
    // The dialog runs on the app's executor when one is installed; the
    // resource is absent in a build with no async backend, and the dialog
    // then blocks the tick instead.
    spawn: Option<Res<lumen_core::task::SpawnService>>,
    command_queue: Res<lumen_core::command::CommandQueue>,
) {
    for ev in events.read() {
        let ScriptCommand::OpenFileDialog {
            kind,
            tag,
            filters,
            default_name,
        } = &ev.0
        else {
            continue;
        };
        let os_kind = match kind {
            lumen_script::FileDialogKind::Open => FileDialogKind::Open,
            lumen_script::FileDialogKind::OpenMulti => FileDialogKind::OpenMulti,
            lumen_script::FileDialogKind::Save => FileDialogKind::Save,
            lumen_script::FileDialogKind::PickFolder => FileDialogKind::PickFolder,
        };
        let req = FileDialogRequest {
            kind: os_kind,
            tag: tag.clone(),
            filters: filters
                .iter()
                .map(|(label, exts)| (label.clone(), exts.clone()).into())
                .collect(),
            default_name: default_name.clone(),
        };
        // The result lands as a `FileDialogResultCommand`, then `FilePicked`,
        // through the typed-command drain the install above registered.
        file_dialog.open_single_with(spawn.as_ref().map(|s| s.as_spawn()), &command_queue, req);
    }
}
