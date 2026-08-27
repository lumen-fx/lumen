//! What the two sides say to each other, as serde shapes.
//!
//! Each of these is encoded once per crossing through [`crate::codec`]: the
//! host sends an [`InitCx`] and takes back a [`Manifest`], sends a [`Call`]
//! and takes back a [`CallOut`], and receives a [`PluginEvent`] whenever the
//! plugin pushes one. The types a plugin describes its functions with are
//! `lumen_script`'s own, so a manifest says exactly what the engine's script
//! registry stores.

use std::path::PathBuf;

use lumen_script::{HostSet, ScriptCommand, ScriptNs, ScriptPrelude, ScriptSig, ScriptValue};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::Error;

/// What a plugin knows about the app it was loaded into.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitCx {
    /// The app directory, canonicalized. A plugin resolves its own files
    /// against this rather than against the process's working directory.
    pub app_dir: PathBuf,
    /// The app's id from `lumen.toml`, or its directory name when it
    /// declares none.
    pub app_id: String,
    /// True when the app runs without a window.
    pub headless: bool,
    /// True when the app was started with hot reload on, so a plugin that
    /// caches derived state knows the sources under it can change.
    pub hot_reload: bool,
    /// The engine's version, as a semver string.
    pub lumen_version: String,
    /// This module's own `config` table, re-serialized. Read it through
    /// [`InitCx::config`].
    config_toml: String,
}

impl InitCx {
    /// Build a context. Host-side; a plugin only ever reads one.
    pub fn new(
        app_dir: PathBuf,
        app_id: String,
        headless: bool,
        hot_reload: bool,
        lumen_version: String,
        config_toml: String,
    ) -> Self {
        InitCx {
            app_dir,
            app_id,
            headless,
            hot_reload,
            lumen_version,
            config_toml,
        }
    }

    /// Deserialize the module's `config` table into any serde type. An app
    /// that declares no table yields the type's view of an empty table.
    pub fn config<T: DeserializeOwned>(&self) -> Result<T, Error> {
        toml::from_str(&self.config_toml).map_err(Error::from)
    }
}

/// What a plugin registered, answered from its init.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// The functions the plugin offers, in registration order. A
    /// [`Call`] names one by its index here, so the order is the calling
    /// convention rather than a presentation detail.
    pub fns: Vec<FnDecl>,
    /// Language sources the matching host compiles ahead of the app's own
    /// program.
    pub preludes: Vec<ScriptPrelude>,
    /// Reserved for the capability grants a sandboxed plugin will ask for.
    /// Must be empty; a host refuses a manifest that declares any.
    pub capabilities: Vec<String>,
}

/// One function a plugin offers, in the terms every script host understands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FnDecl {
    /// The name the script calls it by.
    pub name: String,
    /// The namespace it lives in. [`ScriptNs::Builtin`] is the runtime's own
    /// surface and a plugin may not claim it.
    pub ns: ScriptNs,
    /// Its declared signature.
    pub sig: ScriptSig,
    /// The languages that may see it.
    pub hosts: HostSet,
}

/// One call into a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    /// Index into [`Manifest::fns`].
    pub index: u32,
    /// The arguments the script passed.
    pub args: Vec<ScriptValue>,
}

/// What one call produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallOut {
    /// The value the script receives, or the message it raises. A function
    /// that fails is a completed call, so this travels under a success
    /// status; the error statuses are for the boundary itself.
    pub ret: Result<ScriptValue, String>,
    /// What the call emitted before returning. Applied even when `ret` is an
    /// error, matching what an in-process script function does.
    pub commands: Vec<ScriptCommand>,
}

// The event a plugin pushes outside a call is a script-surface shape, so it
// lives with the other script wire types and is re-exported here: the engine
// side decodes it in `lumen-script`'s per-tick drain, which cannot depend on
// this crate.
pub use lumen_script::PluginEvent;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    fn cx(config_toml: &str) -> InitCx {
        InitCx::new(
            PathBuf::from("/app"),
            "demo".to_string(),
            true,
            false,
            "0.1.0".to_string(),
            config_toml.to_string(),
        )
    }

    #[test]
    fn init_cx_config_deserializes_the_table_and_reports_type_errors() {
        let cx = cx("poll_ms = 250");
        let table: toml::Table = cx.config().unwrap();
        assert_eq!(table.get("poll_ms").and_then(|v| v.as_integer()), Some(250));

        #[derive(Deserialize, Debug)]
        struct Wrong {
            #[allow(dead_code)]
            poll_ms: String,
        }
        let err = cx.config::<Wrong>().unwrap_err();
        assert!(err.message.contains("invalid type"), "{}", err.message);
    }

    #[test]
    fn an_absent_config_table_is_the_types_empty_view() {
        #[derive(Deserialize, Debug, Default)]
        #[serde(default)]
        struct Cfg {
            poll_ms: u64,
        }
        assert_eq!(cx("").config::<Cfg>().unwrap().poll_ms, 0);
    }

    #[test]
    fn the_context_survives_the_boundary() {
        let bytes = codec::encode(&cx("poll_ms = 250")).unwrap();
        let back: InitCx = codec::decode(&bytes).unwrap();
        assert_eq!(back.app_id, "demo");
        assert!(back.headless);
        assert_eq!(
            back.config::<toml::Table>()
                .unwrap()
                .get("poll_ms")
                .and_then(|v| v.as_integer()),
            Some(250)
        );
    }

    #[test]
    fn a_call_and_its_outcome_survive_the_boundary() {
        let call = Call {
            index: 3,
            args: vec![ScriptValue::Str("x".into()), ScriptValue::I64(7)],
        };
        let back: Call = codec::decode(&codec::encode(&call).unwrap()).unwrap();
        assert_eq!(back.index, 3);
        assert_eq!(back.args[1], ScriptValue::I64(7));

        let out = CallOut {
            ret: Err("no such device".to_string()),
            commands: vec![ScriptCommand::Print("tried".into())],
        };
        let back: CallOut = codec::decode(&codec::encode(&out).unwrap()).unwrap();
        assert_eq!(back.ret, Err("no such device".to_string()));
        assert_eq!(back.commands.len(), 1);
    }

    #[test]
    fn an_event_survives_the_boundary() {
        let event = PluginEvent::Call {
            event: "on_file_changed".to_string(),
            key: "watcher".to_string(),
            fallback: String::new(),
            args: vec![ScriptValue::Bool(true)],
        };
        let back: PluginEvent = codec::decode(&codec::encode(&event).unwrap()).unwrap();
        assert!(matches!(back, PluginEvent::Call { event, .. } if event == "on_file_changed"));
    }
}
