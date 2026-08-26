//! The runtime-module SDK: how a [`Plugin`] ships as a prebuilt shared
//! library an app declares in `lumen.toml` instead of compiling in.
//!
//! A runtime module is the same [`lumen_core::app::Plugin`] an in-process
//! plugin implements, built as a `cdylib` that links the engine dynamically
//! and exported through [`lumen_module!`]. At startup the engine's loader
//! opens the library, verifies it, and calls the generated install entry,
//! which constructs the plugin from the app's `config` table and hands it to
//! [`App::add_plugin`]. From there it is an ordinary plugin with full ECS
//! reach: real systems, components, resources, and queries over the app's own
//! state.
//!
//! # The lockstep contract
//!
//! A module is version-locked to the **exact engine build** it compiled
//! against. The generated probe returns [`BUILD_ID`], inlined at the module's
//! compile time; the loader compares it against the running engine's value
//! and refuses anything but exact equality, because nothing else detects a
//! layout-changed rebuild - the dynamic linker resolves happily and `TypeId`
//! equality still passes while field reads are shifted. A module is rebuilt
//! per engine release, and a mismatch is a startup banner: the app boots
//! without the module.
//!
//! Two more consequences of the design:
//!
//! - **Load-forever.** A loaded module is never unloaded; the app's schedules
//!   hold function pointers into it for as long as the process lives.
//! - **Windows is not supported** for prebuilt modules: no linkable engine
//!   dylib exists there (its import-library format caps far below the
//!   engine's export count). Compile the plugin into the app instead.
//!
//! # Authoring
//!
//! ```toml
//! [lib]
//! crate-type = ["cdylib"]
//!
//! [dependencies]
//! lumen-module = { git = "https://github.com/lumen-fx/lumen", tag = "v0.0.6" }
//! lumen-core = { git = "https://github.com/lumen-fx/lumen", tag = "v0.0.6" }
//! ```
//!
//! ```ignore
//! use lumen_module::{ModuleConfig, lumen_module};
//!
//! struct ShapeTools { units: String }
//!
//! impl lumen_module::Plugin for ShapeTools {
//!     fn build(self, app: &mut lumen_module::App) { /* systems, resources */ }
//! }
//!
//! lumen_module!(|config: ModuleConfig| ShapeTools {
//!     units: config.str("units").unwrap_or("px").to_string(),
//! });
//! ```
//!
//! Build with the engine taken as a shared library: `-C prefer-dynamic`
//! together with an explicit `--target` (which keeps the flag off build
//! scripts and proc macros). The module authoring guide in the Lumen docs
//! carries the full recipe.
//!
//! Authors write no unsafe: the macro generates the two exported entries, and
//! a panic in the constructor (or in `Plugin::build`) is caught here, its
//! message printed to stderr, and reported to the loader as a failed install.

use std::panic::{AssertUnwindSafe, catch_unwind};

/// The byte-source seam: a module that reads app data (tracks, archives,
/// shipped files) resolves it through the app's asset sources, not the raw
/// filesystem, so bundled `.lpak` entries and `lumen://app/...` URIs work.
/// Take a [`lumen_assets::SourceReader`] from the [`lumen_assets::AssetServer`]
/// resource on the main thread and read on any thread.
pub use lumen_assets::{AssetServer, SourceReader};
pub use lumen_core;
pub use lumen_core::app::{App, Plugin};
/// The script surface a module extends: register functions with
/// [`lumen_script::ScriptFnAppExt::add_script_fn`], order systems against
/// [`lumen_script::ScriptSet`], and deliver events through
/// [`lumen_script::push_plugin_event`]. Taken through the module SDK so a
/// module crate depends on `lumen-module` alone.
pub use lumen_script;

#[cfg(not(windows))]
pub use lumen_engine as lumen_dylib;
#[cfg(not(windows))]
pub use lumen_engine::{BUILD_ID, BUILD_ID_C};

/// Install returned cleanly.
pub const INSTALL_OK: u32 = 0;
/// The constructor or `Plugin::build` panicked. [`install_with`] prints the
/// captured panic message to stderr before returning this, so it lands even
/// when the app installed its own (silent) panic hook.
pub const INSTALL_PANICKED: u32 = 1;
/// The `config` table did not parse back out of its wire form.
pub const INSTALL_BAD_CONFIG: u32 = 2;

/// The module's `config` table from `lumen.toml`, handed to the constructor
/// the module registered with [`lumen_module!`].
#[derive(Debug, Clone, Default)]
pub struct ModuleConfig {
    table: toml::Table,
}

impl ModuleConfig {
    /// The raw table.
    pub fn table(&self) -> &toml::Table {
        &self.table
    }

    /// A string value by key.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.table.get(key).and_then(|v| v.as_str())
    }

    /// A boolean value by key.
    pub fn bool(&self, key: &str) -> Option<bool> {
        self.table.get(key).and_then(|v| v.as_bool())
    }

    /// An integer value by key.
    pub fn int(&self, key: &str) -> Option<i64> {
        self.table.get(key).and_then(|v| v.as_integer())
    }

    /// Deserialize the whole table into a serde type. A key the type does not
    /// declare is ignored unless the type opts into `deny_unknown_fields`.
    pub fn typed<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        self.table.clone().try_into().map_err(|e| e.to_string())
    }
}

/// The body behind the generated `lumen_module_install`: parse the config,
/// run the constructor, hand the plugin to the app, and turn any panic into a
/// status the loader banners on instead of an unwind into it.
#[doc(hidden)]
pub fn install_with<P, F>(app: &mut App, config_toml: &str, ctor: F) -> u32
where
    P: Plugin + 'static,
    F: FnOnce(ModuleConfig) -> P,
{
    let table = match toml::from_str::<toml::Table>(config_toml) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lumen-module: the module's config table did not parse: {e}");
            return INSTALL_BAD_CONFIG;
        }
    };
    let config = ModuleConfig { table };
    match catch_unwind(AssertUnwindSafe(|| {
        app.add_plugin(ctor(config));
    })) {
        Ok(()) => INSTALL_OK,
        Err(payload) => {
            // Print the payload here rather than trusting the panic hook: an
            // app that installed a silent hook would otherwise swallow the
            // only explanation of why the module failed to install.
            eprintln!(
                "lumen-module: the module's constructor panicked: {}",
                panic_message(payload.as_ref())
            );
            INSTALL_PANICKED
        }
    }
}

/// The human-readable half of a panic payload: the `&str` / `String` message
/// `panic!` produces, or a placeholder for a payload of another type.
#[doc(hidden)]
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(non-string panic payload)")
}

/// Export a [`Plugin`] from a `cdylib` crate as a runtime module.
///
/// Takes a constructor expression called with the module's [`ModuleConfig`];
/// it runs once, at load:
///
/// ```ignore
/// lumen_module!(|config: ModuleConfig| MyPlugin::new(&config));
/// ```
///
/// Expands to the module's two exports: the C-ABI `lumen_module_probe`
/// returning [`BUILD_ID`] (the loader's exact-equality handshake) and the
/// Rust-ABI `lumen_module_install` that constructs and installs the plugin.
/// The expansion also names `lumen-dylib`, so the produced library records
/// its dependency on the shared engine even before install is called.
#[macro_export]
macro_rules! lumen_module {
    ($ctor:expr) => {
        const _: () = {
            #[cfg(windows)]
            compile_error!(
                "prebuilt runtime modules are not supported on Windows; compile the plugin \
                 into the app instead"
            );

            // The linkage anchor: name the engine dylib from the module crate
            // itself, so an author cannot build a module that forgot it.
            #[cfg(not(windows))]
            use $crate::lumen_dylib as _;

            #[unsafe(no_mangle)]
            pub extern "C" fn lumen_module_probe() -> *const ::std::os::raw::c_char {
                $crate::BUILD_ID_C.as_ptr() as *const ::std::os::raw::c_char
            }

            #[unsafe(no_mangle)]
            pub fn lumen_module_install(app: &mut $crate::App, config_toml: &str) -> u32 {
                $crate::install_with(app, config_toml, $ctor)
            }
        };
    };
}

#[cfg(all(test, not(windows)))]
mod tests {
    #[test]
    fn build_id_has_the_documented_shape() {
        let id = super::BUILD_ID;
        let fields: Vec<&str> = id.split(' ').collect();
        assert_eq!(fields.len(), 5, "{id}");
        assert_eq!(fields[0], "lumen-engine", "{id}");
        assert!(!fields[1].is_empty(), "{id}");
        assert!(
            fields[2].starts_with("git:") || fields[2] == "nogit",
            "{id}"
        );
        assert!(fields[3].starts_with("rustc:"), "{id}");
        assert_eq!(fields[3].len(), "rustc:".len() + 16, "{id}");
        assert!(fields[4].starts_with("features:"), "{id}");
    }

    #[test]
    fn the_probe_form_is_the_id_plus_nul() {
        assert_eq!(super::BUILD_ID_C.as_bytes().last(), Some(&0));
        assert_eq!(
            &super::BUILD_ID_C[..super::BUILD_ID_C.len() - 1],
            super::BUILD_ID
        );
    }
}
