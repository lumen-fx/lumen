//! Docks and windows for a Lumen app.
//!
//! A `<dock>` is a workspace of panels the user arranges: tab stacks in
//! resizable splits, panels shown and hidden, collapsed to their header,
//! maximized, popped out into windows of their own and docked back, and the
//! whole arrangement saved and restored. A `<dock-panel>` outside any dock
//! is a window the app opens by showing it: a preferences window, an about
//! box, a modal export dialog.
//!
//! ```lmn
//! <dock id="editor">
//!   <dock-split axis="row">
//!     <dock-stack basis="260"><dock-panel id="files" title="Files">...</dock-panel></dock-stack>
//!     <dock-stack center="true"><dock-panel id="code" title="Code">...</dock-panel></dock-stack>
//!   </dock-split>
//! </dock>
//! ```
//!
//! ```candela
//! dock::detach("files");   // its own window
//! dock::attach("files");   // back where it was
//! ```
//!
//! Moving a panel never copies or rebuilds it: its element moves in the one
//! document, so everything inside keeps its state wherever it goes.
//!
//! One implementation, two link shapes, like every std module: the `cdylib`
//! is the bundled `lumen-dock` module an app declares in `lumen.toml`
//! (`lumen-dock = { bundled = true }` under `[dependencies]`), and the `lib`
//! is [`DockPlugin`] for a build that installs it itself.

#![warn(missing_docs)]

mod adopt;
pub mod api;
pub mod drag;
mod frame;
mod keys;
mod menu;
mod overflow;
mod persist;
mod plugin;
mod realize;
mod state;

pub use adopt::{DockChrome, DockElement, DockPanel, TAGS};
pub use api::{Dock, DockError};
pub use drag::{DockDrag, Payload, TearoffProbe, Zone};
pub use frame::{
    DockFloat, DockFloatHandle, DockPinButton, DockSlideOut, DockUnpinButton, Edges, FLOAT_MIN,
};
pub use keys::{Chord, Keys};
pub use lumen_dock_model as model;
pub use lumen_dock_model::Workspace as Layout;
pub use menu::{DockMenuItem, MENU_ID, MenuAction};
pub use overflow::{DockOverflowButton, DockOverflowItem, DockStrip, StripScroll};
pub use persist::{FILE, Persist};
pub use plugin::{
    DESCRIPTOR, DockCloseButton, DockEvent, DockPlugin, DockSet, DockTabButton, DockTabCaption,
    DockTabIcon, NAME, WindowClose,
};
pub use realize::{DockStack, DockTab, DockWindow};

// The module entry: the loader constructs the plugin from the app's `config`
// table, whether it opened this crate's library or found it linked in.
lumen_module::lumen_module!("lumen-dock", |config: lumen_module::ModuleConfig| {
    DockPlugin::new(config)
});
