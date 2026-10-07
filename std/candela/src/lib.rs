//! The `lumen-candela` runtime module: the candela script host a shipped Lumen
//! app runs, precompiled `.cdlb` bytecode on `candela-vm` with no compiler in
//! the process.
//!
//! The host is `lumen-candela-host`, re-exported here whole. This crate adds
//! the module entry: a packaged app opens its library as the bundled
//! `lumen-candela` module, and a binary with no shared engine links it in.
//! Either way, installing it registers the `candela` language with the app.

pub use lumen_candela_host::*;

// The module entry: whether the loader opened this crate's library or found
// it linked in, installing it registers the language.
#[cfg(not(target_arch = "wasm32"))]
lumen_module::lumen_module!(
    "lumen-candela",
    |_config: lumen_module::ModuleConfig| CandelaPlugin,
    language = include_str!("../lumen-language.toml"),
);

#[cfg(test)]
mod tests {
    use lumen_candela_host::{CandelaVmHost, LANGUAGE};
    use lumen_script::ScriptHost;

    #[test]
    fn the_language_a_descriptor_names_is_what_the_host_calls_itself() {
        assert_eq!(CandelaVmHost::new(Vec::new()).lang(), LANGUAGE);
        let descriptor = include_str!("../lumen-language.toml");
        assert!(descriptor.contains(&format!("name = \"{LANGUAGE}\"")));
    }
}
