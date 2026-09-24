//! Text values under text keys, kept for an app from one run to the next.
//!
//! Install [`StoragePlugin`] and the app gains the `storage` namespace, in
//! every host:
//!
//! ```text
//! storage::get_item(key) -> string or null     storage::session_get_item(key)
//! storage::set_item(key, value) -> bool         storage::session_set_item(key, value)
//! storage::remove_item(key)                     storage::session_remove_item(key)
//! storage::keys() -> string[]                   storage::session_keys()
//! storage::clear()                              storage::session_clear()
//! ```
//!
//! The surface is declared once, in the module's web half
//! (`web/lumen-addon.toml`), which a web build ships as the browser's local
//! and session storage. On the desktop the local set is a JSON object in
//! `storage.json` under the app's data directory, written through on every
//! change, and the session set lives in memory for the length of the process.
//!
//! `on_storage_change(key, new_value, old_value)` reports a change another
//! browser tab made. A desktop app is one process with one copy of the file,
//! so on the desktop it never fires.
//!
//! ```toml
//! [dependencies]
//! lumen-storage = { bundled = true }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod store;

pub use store::Store;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::app_paths;
use lumen_module::lumen_script::{ScriptFnAppExt, ScriptFnBody, ScriptResult, ScriptValue};

/// The web half's descriptor, which is where this module's functions are
/// declared.
pub const DESCRIPTOR: &str = include_str!("../web/lumen-addon.toml");

/// The module's name, as an app declares it.
pub const NAME: &str = "lumen-storage";

/// The file the local set is kept in, under the app's data directory.
pub const FILE_NAME: &str = "storage.json";

/// Storage for a Lumen app: install it and the `storage` functions exist.
///
/// Ships as the bundled `lumen-storage` runtime module, and works the same
/// added as an ordinary plugin in a static build.
#[derive(Debug, Clone, Default)]
pub struct StoragePlugin {
    file: Option<PathBuf>,
}

impl StoragePlugin {
    /// Keep the local set in `file` rather than in the app's data directory.
    #[must_use]
    pub fn at(file: impl Into<PathBuf>) -> Self {
        Self {
            file: Some(file.into()),
        }
    }
}

impl Plugin for StoragePlugin {
    fn build(self, app: &mut App) {
        // Resolved at the first call rather than here: the data directory
        // follows the app id, which the app publishes before it runs a
        // script.
        let file = self.file;
        let local = Arc::new(Mutex::new(Store::persistent(move || {
            file.unwrap_or_else(|| app_paths::data_dir().join(FILE_NAME))
        })));
        let session = Arc::new(Mutex::new(Store::memory()));
        match script_fns(&local, &session) {
            Ok(fns) => {
                app.add_script_fns(fns);
            }
            Err(reason) => eprintln!("lumen-runtime: {reason}"),
        }
    }
}

/// The ten functions, bound to the two stores.
fn script_fns(
    local: &Arc<Mutex<Store>>,
    session: &Arc<Mutex<Store>>,
) -> Result<Vec<lumen_module::lumen_script::ScriptFn>, String> {
    let mut bodies: Vec<(String, ScriptFnBody)> = Vec::new();
    for (prefix, store) in [("", local), ("session_", session)] {
        let name = |f: &str| format!("{prefix}{f}");
        let s = Arc::clone(store);
        bodies.push((
            name("get_item"),
            Arc::new(move |cx| {
                with(&s, |store| {
                    Ok(store
                        .get(&cx.str_arg(0))
                        .map_or(ScriptValue::Unit, ScriptValue::Str))
                })
            }),
        ));
        let s = Arc::clone(store);
        bodies.push((
            name("set_item"),
            Arc::new(move |cx| {
                with(&s, |store| {
                    Ok(ScriptValue::Bool(store.set(cx.str_arg(0), cx.str_arg(1))))
                })
            }),
        ));
        let s = Arc::clone(store);
        bodies.push((
            name("remove_item"),
            Arc::new(move |cx| {
                with(&s, |store| {
                    store.remove(&cx.str_arg(0));
                    Ok(ScriptValue::Unit)
                })
            }),
        ));
        let s = Arc::clone(store);
        bodies.push((
            name("keys"),
            Arc::new(move |_| {
                with(&s, |store| {
                    Ok(ScriptValue::Array(
                        store.keys().into_iter().map(ScriptValue::Str).collect(),
                    ))
                })
            }),
        ));
        let s = Arc::clone(store);
        bodies.push((
            name("clear"),
            Arc::new(move |_| {
                with(&s, |store| {
                    store.clear();
                    Ok(ScriptValue::Unit)
                })
            }),
        ));
    }
    lumen_module::web_half_fns(NAME, DESCRIPTOR, bodies)
}

/// Run `f` on the store behind `store`.
fn with(store: &Mutex<Store>, f: impl FnOnce(&mut Store) -> ScriptResult) -> ScriptResult {
    let mut guard = store
        .lock()
        .map_err(|_| "storage: the store is poisoned".to_string())?;
    f(&mut guard)
}

lumen_module::lumen_module!("lumen-storage", |_config: lumen_module::ModuleConfig| {
    StoragePlugin::default()
});

#[cfg(test)]
mod tests {
    use super::*;

    /// One surface on every target: every function the web half declares
    /// has a desktop body.
    #[test]
    fn the_desktop_half_answers_every_function_the_web_half_declares() {
        let local = Arc::new(Mutex::new(Store::memory()));
        let session = Arc::new(Mutex::new(Store::memory()));
        let fns = script_fns(&local, &session).expect("every function has a body");
        let declared =
            lumen_module::describe_web_half(NAME, DESCRIPTOR).expect("the descriptor reads");
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        let expected: Vec<&str> = declared.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, expected);
    }
}
