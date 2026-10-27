//! Keeping the arrangement between runs, when the app asks for it with
//! `persist = "auto"`: the live workspace and the user's named layouts in one
//! file under the app's data directory, read once the page has mounted and
//! written at the end of every tick that changed them.
//!
//! The primary window's own size and place are not here: the runtime keeps
//! those in its window state, and one value has one writer.

use std::path::PathBuf;

use bevy_ecs::prelude::*;
use lumen_dock_model::LayoutFile;
use lumen_module::lumen_core::app_paths;

use crate::state::{Reason, Shared};

/// The file the arrangement is kept in, under the app's data directory.
pub const FILE: &str = "dock-layout.json";

/// Whether the arrangement is kept between runs.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Persist {
    /// Only when the app saves and restores it itself (`dock::save`).
    #[default]
    None,
    /// In [`FILE`], read at start and written on every change.
    Auto,
}

/// Where the arrangement is kept.
pub fn path() -> PathBuf {
    app_paths::data_dir().join(FILE)
}

/// Read the kept arrangement once the first dock has been read, when the
/// app keeps one. A file of another format or layout version is set aside
/// with a `layouterror`, and the markup's layout stays.
pub fn restore_if_due(world: &mut World, shared: &Shared) {
    let persist = world.get_resource::<Persist>().copied().unwrap_or_default();
    let mut core = shared.lock();
    if !core.restore_due || core.docks.is_empty() && core.panels.is_empty() {
        return;
    }
    core.restore_due = false;
    if persist != Persist::Auto {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path()) else {
        return;
    };
    match LayoutFile::from_json(&text, core.layout_version) {
        Ok((live, layouts)) => {
            core.user = layouts;
            match live {
                Some(ws) => core.op_restore(ws, Reason::Layout),
                None => core.errors.push((
                    "version".into(),
                    format!(
                        "{} was saved for another layout version; the markup's layout is used",
                        FILE
                    ),
                )),
            }
        }
        Err(e) => core.errors.push((e.reason().to_string(), e.to_string())),
    }
    // Reading the file is not a change to write back.
    core.save_due = false;
    core.dirty = true;
}

/// Write the arrangement when it changed this tick and the app keeps it.
pub fn save_if_due(shared: Res<Shared>, persist: Option<Res<Persist>>) {
    if persist.is_none_or(|p| *p != Persist::Auto) {
        return;
    }
    let mut core = shared.lock();
    if !core.save_due || core.restore_due {
        return;
    }
    core.save_due = false;
    let file = LayoutFile {
        live: core.snapshot(),
        layouts: core.user.clone(),
    };
    drop(core);
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Written aside and moved into place, so a crash mid-write never leaves
    // a half file behind.
    let tmp = path.with_extension("json.tmp");
    let result = std::fs::write(&tmp, file.to_json()).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = result {
        lumen_module::lumen_core::warn_line!(
            "lumen-dock: could not keep the layout in {}: {e}",
            path.display()
        );
    }
}
