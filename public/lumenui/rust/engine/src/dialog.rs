//! Native file dialogs: open, open several, save, and pick a folder.
//!
//! [`FileDialog`] is the one dialog implementation the script functions
//! `pick_file`, `pick_files`, `save_file` and `pick_folder` use too, in its
//! typed form. Build the request and open it from a handler with
//! [`Ui::show_dialog`](crate::Ui::show_dialog); the callback gets the app and
//! the chosen paths on the main thread, on a tick after the dialog closes. A
//! system queues it with [`FileDialog::then`] instead.
//!
//! ```no_run
//! # extern crate lumenui_engine as lumenui;
//! use lumenui::prelude::*;
//!
//! #[derive(Resource, Default)]
//! struct Document(Option<std::path::PathBuf>);
//!
//! # fn demo() -> lumenui::Result<()> {
//! lumenui::App::from_markup(r#"<root><button id="open" text="Open"/></root>"#)
//!     .insert_resource(Document::default())
//!     .on_click("open", |ui, _| {
//!         let dialog = FileDialog::open().filter("Images", &["png", "aseprite"]);
//!         ui.show_dialog(dialog, |ui, paths| {
//!             if let Some(path) = paths.into_iter().next() {
//!                 ui.set("title", path.display().to_string());
//!                 ui.resource_mut::<Document>().0 = Some(path);
//!             }
//!         });
//!     })
//!     .run()
//! # }
//! ```

pub use crate::lumen_os_filedialog::{FileDialog, FileDialogKind, MimeFilter, RequestId};
