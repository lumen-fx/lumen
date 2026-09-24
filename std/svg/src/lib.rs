//! The `svg` module: an <svg-view> element that shows SVG markup a script hands it.
//!
//! Its implementation is its web half, `web/lumen-addon.toml` and the ES
//! module beside it, which a web build ships with the page. This crate is its
//! desktop half: every function the web half declares, each raising
//! `svg::<function> runs only in a browser` in the script that called it, so
//! a script written for a page compiles and runs on the desktop too.
//! Code that should do something else there picks a side with `@cfg(web)`.
//!
//! ```toml
//! [dependencies]
//! lumen-svg = { bundled = true }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// The web half's descriptor, which is where this module's functions are
/// declared.
pub const DESCRIPTOR: &str = include_str!("../web/lumen-addon.toml");

/// The module's name, as an app declares it.
pub const NAME: &str = "lumen-svg";

/// The desktop half as a plugin, for a static build that installs it itself.
#[must_use]
pub fn plugin() -> lumen_module::BrowserOnly {
    lumen_module::BrowserOnly::new(NAME, DESCRIPTOR)
}

lumen_module::lumen_module!("lumen-svg", |_config: lumen_module::ModuleConfig| plugin());

#[cfg(test)]
mod tests {
    use lumen_module::lumen_script::{ScriptFnCx, ScriptNs};

    use super::*;

    /// One surface on every target: the desktop half registers exactly the
    /// functions the web half declares, with the same signatures.
    #[test]
    fn the_desktop_half_is_the_web_half_s_surface() {
        let addon = plugin()
            .describe()
            .expect("the web half's descriptor reads");
        let fns = plugin().script_fns().expect("the functions bind");
        assert_eq!(fns.len(), addon.functions.len());
        for (native, declared) in fns.iter().zip(&addon.functions) {
            assert_eq!(native.name, declared.name);
            assert_eq!(native.ns, ScriptNs::Named(addon.namespace.clone()));
            let sig = lumen_module::lumen_script::addon::signature(&addon, declared)
                .expect("the types parse");
            let params = |sig: &lumen_module::lumen_script::ScriptSig| {
                sig.params
                    .iter()
                    .map(|p| (p.name.clone(), p.ty.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(params(&native.sig), params(&sig), "{}", declared.name);
            assert_eq!(native.sig.ret, sig.ret, "{}", declared.name);
            assert_eq!(native.sig.doc, sig.doc, "{}", declared.name);
            let mut out = Vec::new();
            let raised = (native.body)(&mut ScriptFnCx::new(&[], &mut out))
                .expect_err("a desktop call raises");
            assert_eq!(
                raised,
                format!(
                    "{}::{} runs only in a browser",
                    addon.namespace, declared.name
                )
            );
        }
    }
}
