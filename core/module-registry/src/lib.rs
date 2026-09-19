//! The list a compiled-in module puts itself on.
//!
//! A runtime module reaches an app two ways. Opened at startup it is a
//! shared library the loader dlopens, verifies, and installs. Linked in it
//! has no file to open and no symbol for the loader to look up by path, so
//! it announces itself instead: the module's generated constructor runs
//! before `main` and leaves a [`StaticModule`] here, and the loader reads
//! the list when an app declares that name.
//!
//! Nothing in this crate names a module or a capability. Every entry comes
//! from the module's own crate, carrying the name the app declares it under
//! and the entry that installs it.
//!
//! ```
//! use lumen_module_registry::{StaticModule, register, registered};
//!
//! fn install(_app: &mut lumen_core::app::App, _config_toml: &str) -> u32 {
//!     0
//! }
//!
//! register(StaticModule {
//!     name: "shape-tools",
//!     install,
//! });
//! assert!(registered().iter().any(|m| m.name == "shape-tools"));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Mutex;

use lumen_core::app::App;

/// One module compiled into the running binary.
#[derive(Debug, Clone, Copy)]
pub struct StaticModule {
    /// The name an app declares the module under in `lumen.toml`.
    pub name: &'static str,
    /// The module's install entry: parse the `config` table, construct the
    /// plugin, and hand it to the app. The return value is the same status
    /// the opened-library path reports, so both arms of the loader read one
    /// set of codes.
    pub install: fn(&mut App, &str) -> u32,
}

/// Every module registered so far, in registration order.
///
/// A `Mutex` rather than a lock-free list because registration happens once
/// per module, before `main`, where a blocking lock has nobody to block
/// against; the cost lands nowhere a frame can see it.
static REGISTERED: Mutex<Vec<StaticModule>> = Mutex::new(Vec::new());

/// Add a module to the list.
///
/// Called from a module's pre-main constructor, so it must not panic on a
/// poisoned lock: a poisoned lock here would mean an earlier registration
/// panicked, and taking the list as it stands is better than aborting the
/// process before it starts.
pub fn register(module: StaticModule) {
    let mut list = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    list.push(module);
}

/// The registered modules, copied out so the caller holds no lock while it
/// installs them.
pub fn registered() -> Vec<StaticModule> {
    REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(_app: &mut App, _config_toml: &str) -> u32 {
        0
    }

    #[test]
    fn a_registered_module_is_readable_by_name() {
        register(StaticModule {
            name: "registry-test-one",
            install: ok,
        });
        register(StaticModule {
            name: "registry-test-two",
            install: ok,
        });
        let names: Vec<&str> = registered().iter().map(|m| m.name).collect();
        assert!(names.contains(&"registry-test-one"), "{names:?}");
        assert!(names.contains(&"registry-test-two"), "{names:?}");
    }

    #[test]
    fn the_install_entry_survives_the_round_trip() {
        register(StaticModule {
            name: "registry-test-install",
            install: ok,
        });
        let entry = registered()
            .into_iter()
            .find(|m| m.name == "registry-test-install")
            .expect("the entry is on the list");
        let mut app = App::new();
        assert_eq!((entry.install)(&mut app, ""), 0);
    }
}
