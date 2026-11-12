//! Commands for a Lumen app.
//!
//! A command is a named action: it has a label, the chords that run it, and
//! whether it runs right now. Menus name it (`<menuitem command="...">`) and
//! take their label, shortcut and greyed-out state from it; a palette lists
//! it; the user rebinds its keys and the bindings are kept between runs.
//! Whatever runs it (a key, a menu item, the palette, `commands::run`), the
//! app hears one `command` event on the document.
//!
//! ```lmn
//! <commands>
//!   <command id="file.save" label="Save" keys="CommandOrControl+S"/>
//! </commands>
//! <command-palette/>
//! ```
//!
//! ```candela
//! document_node().on("command", fn(e) { save(); });
//! ```
//!
//! One implementation, two link shapes, like every std module: the `cdylib`
//! is the bundled `lumen-commands` module an app declares in `lumen.toml`
//! (`lumen-commands = { bundled = true }` under `[dependencies]`), and the
//! `lib` is [`CommandsPlugin`] for a build that installs it itself.

#![warn(missing_docs)]

pub mod api;
mod model;
mod palette;
mod persist;
mod plugin;

pub use api::Commands;
pub use lumen_module::lumen_core::chord::Chord;
pub use model::{Command, CommandError, PALETTE};
pub use palette::{OPEN_SIGNAL, Palette, PaletteInput, PaletteRow};
pub use persist::{FILE, Persist};
pub use plugin::{CommandElement, CommandRun, CommandsPlugin, DESCRIPTOR, NAME};

/// The tags the module adopts.
pub const TAGS: [&str; 3] = ["commands", "command", "command-palette"];

// The module entry: the loader constructs the plugin from the app's `config`
// table, whether it opened this crate's library or found it linked in.
lumen_module::lumen_module!("lumen-commands", |config: lumen_module::ModuleConfig| {
    CommandsPlugin::new(config)
});
