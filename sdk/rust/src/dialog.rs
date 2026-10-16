//! Native file dialogs: open, open several, save, and pick a folder.
//!
//! [`FileDialog`] is the one dialog implementation the script functions
//! `pick_file`, `pick_files`, `save_file` and `pick_folder` use too, in its
//! typed form. Build the request, then either queue it from a system with
//! [`FileDialog::then`] or open it with the world in hand with
//! [`FileDialog::show`]; the callback gets the world and the chosen paths on
//! the main thread, on a tick after the dialog closes.
//!
//! ```no_run
//! # extern crate lumenui_engine as lumenui;
//! use lumenui::prelude::*;
//!
//! #[derive(Resource, Default)]
//! struct Document(Option<std::path::PathBuf>);
//!
//! fn open_menu(mut clicks: MessageReader<ClickEvent>, ids: Query<&LumenId>, mut commands: Commands) {
//!     for click in clicks.read() {
//!         if ids.get(click.entity).is_ok_and(|id| id.0 == "open") {
//!             commands.queue(
//!                 FileDialog::open()
//!                     .filter("Images", &["png", "aseprite"])
//!                     .then(|world: &mut World, paths| {
//!                         if let Some(path) = paths.into_iter().next() {
//!                             world.resource_mut::<Document>().0 = Some(path);
//!                         }
//!                     }),
//!             );
//!         }
//!     }
//! }
//! # use lumenui::ecs::world::World;
//! ```

pub use crate::lumen_os_filedialog::{FileDialog, FileDialogKind, MimeFilter, RequestId};
