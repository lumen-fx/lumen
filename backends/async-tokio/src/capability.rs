//! The `async` capability: the executor file dialogs resolve on.
//!
//! Installed for an app whose sources call a dialog builtin (or whose
//! sources cannot be read), because the executor spawns worker threads a
//! dialog-free app should not pay for. It has to be there for dialogs to
//! work at all on macOS: `NSOpenPanel` only resolves while the main run loop
//! is pumping, so a dialog run inline deadlocks there and reports a cancel.

use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;

use crate::AsyncTokioPlugin;

/// What a static package looks for in the app's sources before it
/// carries this subsystem.
pub const SELECT: Select = Select::OnUse(lumen_script::FILE_DIALOG_BUILTINS);

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, env: &CapabilityEnv) {
    if env.sources_mention(lumen_script::FILE_DIALOG_BUILTINS) {
        app.add_plugin(AsyncTokioPlugin);
    }
}
