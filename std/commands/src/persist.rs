//! Keeping the user's key bindings between runs: `keymap.json` in the app's
//! data directory, read once before the first key is heard and written at
//! the end of every tick that changed a binding.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bevy_ecs::prelude::Resource;
use lumen_module::lumen_core::app_paths;
use lumen_module::lumen_core::chord::Chord;
use serde_json::{Map, Value};

use crate::model::Core;

/// The file the bindings are kept in, under the app's data directory.
pub const FILE: &str = "keymap.json";

/// The version of the file's format, under its `lumen-commands` key.
const FORMAT: u64 = 1;

/// Whether the user's bindings are kept between runs.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Persist {
    /// In [`FILE`], read at start and written on every change.
    #[default]
    Auto,
    /// Only for this run.
    None,
}

/// Where the bindings are kept.
pub fn path() -> PathBuf {
    app_paths::data_dir().join(FILE)
}

/// The bindings as the file holds them: each command id with its chords.
pub fn to_json(overrides: &BTreeMap<String, Vec<Chord>>) -> String {
    let keys: Map<String, Value> = overrides
        .iter()
        .map(|(id, chords)| {
            let list = chords.iter().map(|c| Value::from(c.to_text())).collect();
            (id.clone(), Value::Array(list))
        })
        .collect();
    let mut file = Map::new();
    file.insert("lumen-commands".into(), Value::from(FORMAT));
    file.insert("keys".into(), Value::Object(keys));
    serde_json::to_string_pretty(&Value::Object(file)).unwrap_or_default()
}

/// Read the bindings from the file's text. A chord this platform cannot read
/// is dropped from its command's list.
///
/// # Errors
///
/// The text is not JSON, or not a keymap of a format this reads.
pub fn from_json(text: &str) -> Result<BTreeMap<String, Vec<Chord>>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    match value.get("lumen-commands").and_then(Value::as_u64) {
        Some(FORMAT) => {}
        Some(other) => return Err(format!("format {other} is newer than this app reads")),
        None => return Err("not a keymap".into()),
    }
    let mut out = BTreeMap::new();
    if let Some(keys) = value.get("keys").and_then(Value::as_object) {
        for (id, list) in keys {
            let chords = list
                .as_array()
                .map(|l| {
                    l.iter()
                        .filter_map(Value::as_str)
                        .filter_map(Chord::parse)
                        .collect()
                })
                .unwrap_or_default();
            out.insert(id.clone(), chords);
        }
    }
    Ok(out)
}

/// Read the kept bindings into `core`. A file that is missing changes
/// nothing; one that does not read is reported and left alone.
pub fn load(core: &mut Core) {
    let path = path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    match from_json(&text) {
        Ok(overrides) => {
            core.overrides = overrides;
            core.changed = true;
        }
        Err(e) => lumen_module::lumen_core::warn_line!(
            "lumen-commands: {} is left as it is: {e}",
            path.display()
        ),
    }
}

/// Write the bindings, aside and then moved into place, so a crash mid-write
/// never leaves half a file.
pub fn save(core: &Core) {
    let path = path();
    let tmp = path.with_extension("json.tmp");
    let result =
        std::fs::write(&tmp, to_json(&core.overrides)).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = result {
        lumen_module::lumen_core::warn_line!(
            "lumen-commands: could not keep the key bindings in {}: {e}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_round_trip_through_the_file() {
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "file.save".to_string(),
            Chord::parse_list("Ctrl+S, F2").unwrap(),
        );
        overrides.insert("view.zoom".to_string(), Vec::new());
        let text = to_json(&overrides);
        assert_eq!(from_json(&text).unwrap(), overrides);
        assert!(from_json("{}").is_err());
        assert!(from_json("{\"lumen-commands\": 9}").is_err());
        assert!(from_json("not json").is_err());
    }
}
