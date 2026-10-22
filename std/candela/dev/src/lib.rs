//! The `lumen-candela-dev` runtime module: the candela script host the edit
//! loop runs, with the compiler, hot reload, and the ahead-of-time checks and
//! builds.
//!
//! The host is `lumen-candela-dev-host`, re-exported here whole. This crate
//! adds the module entry: `lumenc` opens its library as the bundled
//! `lumen-candela-dev` module beside a shared engine and links it in where
//! there is none. Either way, installing it registers the `candela` language
//! with every entry the edit loop and the build use.

pub use lumen_candela_dev_host::*;

// The module entry: whether the loader opened this crate's library or found
// it linked in, installing it registers the language.
lumen_module::lumen_module!(
    "lumen-candela-dev",
    |_config: lumen_module::ModuleConfig| CandelaDevPlugin,
    language = include_str!("../lumen-language.toml"),
);

#[cfg(test)]
mod tests {
    use lumen_candela_dev_host::{CandelaHost, LANGUAGE};
    use lumen_script::ScriptHost;

    #[test]
    fn the_language_a_descriptor_names_is_what_the_host_calls_itself() {
        assert_eq!(CandelaHost::new().lang(), LANGUAGE);
        let descriptor = include_str!("../lumen-language.toml");
        assert!(descriptor.contains(&format!("name = \"{LANGUAGE}\"")));
    }
}
