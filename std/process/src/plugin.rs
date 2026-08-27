//! The plugin that puts child processes into an app: the `process` script
//! namespace and the three events a child produces.
//!
//! The engine has no process surface of its own; everything an app observes
//! comes from here, through the generic seams a plugin uses:
//!
//! - `process::start` registers on the app's `ScriptFnRegistry`, so every
//!   host (Rhai, Lua, candela) binds it before the program loads;
//! - a line of output and the exit are [`PluginEvent`]s on the plugin-event
//!   bus, so `on("process_stdout", tag, fn)` wins per child and
//!   `on_process_stdout(tag, line)` catches the rest, the routing every
//!   plugin event gets.
//!
//! The module runs no systems and holds no world state. A child starts inside
//! the call that asked for it, which is what lets `process::start` answer
//! whether the program is running; everything after that arrives from the
//! supervisor's own threads, and pushing an event wakes a parked event loop
//! on its own.
//!
//! A start that fails answers false and explains itself in one
//! `lumen-process:` line on stderr. It fires no event, because the tag never
//! named a running program, so a script branches on the value it got back.

use std::sync::Arc;

use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::warn_line;
use lumen_module::lumen_script::{
    PluginEvent, ScriptFn, ScriptFnAppExt, ScriptNs, ScriptTy as T, ScriptValue, push_plugin_event,
};

use crate::child;

/// The namespace the functions live in: `process::start(..)` in Rhai and
/// candela, `process.start(..)` in Lua.
const NAMESPACE: &str = "process";

/// Child processes for a Lumen app: install it and `process::start` exists.
///
/// Ships as the bundled `lumen-process` runtime module (an app declares
/// `lumen-process = { bundled = true }` under `[dependencies]`), and works the
/// same added as an ordinary plugin in a static build. Without it the function
/// does not exist and a script call fails with the host's ordinary
/// unknown-function error.
pub struct ProcessPlugin;

impl ProcessPlugin {
    /// Build from the module's `config` table. The module takes no settings,
    /// so a key an app writes there is ignored rather than refused.
    #[must_use]
    pub fn new(_config: ModuleConfig) -> Self {
        Self
    }
}

impl Plugin for ProcessPlugin {
    fn build(self, app: &mut App) {
        app.add_script_fns(script_fns());
    }
}

/// The `process` surface, described once for every host. Names, parameters,
/// and docs are the contract a script writes against.
fn script_fns() -> Vec<ScriptFn> {
    vec![
        ScriptFn::new("start")
            .ns(ScriptNs::Named(NAMESPACE.to_string()))
            .doc(
                "Start a program in the app directory; its output and its exit arrive as \
                 events under that tag. False when it did not start.",
            )
            .param("cmd", T::Str)
            .param("args", T::Array(Box::new(T::Str)))
            .param("tag", T::Str)
            .ret(T::Bool)
            .build(|cx| {
                let args = arguments(cx.arg_ref(1));
                Ok(ScriptValue::Bool(start(
                    &cx.str_arg(0),
                    &args,
                    &cx.str_arg(2),
                )))
            }),
    ]
}

/// Start one child and report whether it is running.
fn start(cmd: &str, args: &[String], tag: &str) -> bool {
    match child::start(cmd, args, tag, deliver(tag.to_string())) {
        Ok(_pid) => true,
        Err(message) => {
            warn_line!("lumen-process: {message}");
            false
        }
    }
}

/// The sink one child's lines and its exit travel through: the generic
/// plugin-event bus, keyed by the tag the script named.
fn deliver(tag: String) -> child::Emit {
    Arc::new(move |event| {
        let (name, fallback, arg) = match event {
            child::Event::Stdout(line) => (
                "process_stdout",
                "on_process_stdout",
                ScriptValue::Str(line),
            ),
            child::Event::Stderr(line) => (
                "process_stderr",
                "on_process_stderr",
                ScriptValue::Str(line),
            ),
            child::Event::Exit(code) => ("process_exit", "on_process_exit", ScriptValue::I64(code)),
        };
        push_plugin_event(&PluginEvent::Call {
            event: name.to_string(),
            key: tag.clone(),
            fallback: fallback.to_string(),
            args: vec![arg],
        });
    })
}

/// The argument list a script passed: the elements of a list, each in the
/// spelling its host stringifies it to, or a single argument written on its
/// own. A call that passed nothing runs the program bare.
fn arguments(value: &ScriptValue) -> Vec<String> {
    match value {
        ScriptValue::Array(items) => items.iter().map(ScriptValue::stringify).collect(),
        ScriptValue::Unit => Vec::new(),
        single => vec![single.stringify()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list becomes the argument list in the spelling each host stringifies
    /// its values to, a value written on its own is one argument, and nothing
    /// at all runs the program bare.
    #[test]
    fn an_argument_list_is_the_strings_it_holds() {
        assert_eq!(
            arguments(&ScriptValue::Array(vec![
                ScriptValue::Str("--fast".to_string()),
                ScriptValue::I64(3),
                ScriptValue::Bool(true),
            ])),
            vec!["--fast", "3", "true"]
        );
        assert!(arguments(&ScriptValue::Array(Vec::new())).is_empty());
        assert!(arguments(&ScriptValue::Unit).is_empty());
        assert_eq!(
            arguments(&ScriptValue::Str("--only".to_string())),
            vec!["--only"]
        );
    }
}
