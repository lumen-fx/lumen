//! The plugin that puts child processes into an app: the `process` script
//! namespace and the three events a child produces.
//!
//! The engine has no process surface of its own; everything an app observes
//! comes from here, through the generic seams a plugin uses:
//!
//! - `process::start` and `process::stop` register on the app's
//!   `ScriptFnRegistry`, so every host (Rhai, Lua, candela) binds them before
//!   the program loads;
//! - a line of output and the exit are [`PluginEvent`]s on the plugin-event
//!   bus, so `on("process_stdout", tag, fn)` wins per child and
//!   `on_process_stdout(tag, line)` catches the rest, the routing every
//!   plugin event gets;
//! - the children started with `end_at_exit: true` are ended when the app's
//!   world is torn down, from the drop of a resource this plugin inserts, so
//!   the engine needs no exit hook that names processes.
//!
//! The module runs no systems. A child starts inside the call that asked for
//! it, which is what lets `process::start` answer whether the program is
//! running; everything after that arrives from the supervisor's own threads,
//! and pushing an event wakes a parked event loop on its own.
//!
//! A start that fails answers false and explains itself in one
//! `lumen-process:` line on stderr. It fires no event, because the tag never
//! named a running program, so a script branches on the value it got back.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use bevy_ecs::prelude::Resource;
use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::warn_line;
use lumen_module::lumen_script::{
    PluginEvent, ScriptFn, ScriptFnAppExt, ScriptNs, ScriptStruct, ScriptTy as T, ScriptValue,
    push_plugin_event,
};

use crate::child;

/// The namespace the functions live in: `process::start(..)` in Rhai and
/// candela, `process.start(..)` in Lua.
const NAMESPACE: &str = "process";

/// Child processes for a Lumen app: install it and `process::start` and
/// `process::stop` exist.
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
        let children = Arc::new(Children::default());
        app.add_script_fns(script_fns(&children));
        app.world.insert_resource(EndAtExit(children));
    }
}

/// The name of the options struct, `process::StartOptions` in candela.
const OPTIONS: &str = "StartOptions";

/// The fourth argument of `process::start`, described once for every host: a
/// candela struct, a Rhai or Lua map.
///
/// `cwd` is empty for the app directory, and `end_at_exit` is false so a child
/// outlives the app: a launcher starts a program and gets out of its way.
fn options_type() -> ScriptStruct {
    ScriptStruct::new(OPTIONS)
        .field_default("cwd", T::Str, "")
        .field("env", T::Map(Box::new(T::Str)))
        .field_default("end_at_exit", T::Bool, false)
}

/// Everything the fourth argument of `process::start` asks for.
#[derive(Debug, Default, PartialEq, Eq)]
struct StartOptions {
    child: child::Options,
    /// End the child when the app exits: a helper nobody expects to survive.
    end_at_exit: bool,
}

/// One child this app started that has not reported its exit yet.
struct Entry {
    id: u64,
    tag: String,
    running: child::Running,
    end_at_exit: bool,
}

/// The children still running, shared by the script functions, the sink
/// that retires a child when it exits, and the exit-time resource.
#[derive(Default)]
struct Children {
    live: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
}

impl Children {
    /// Start one child and record it, answering whether it is running.
    ///
    /// The record is made under the same lock the exit sink takes, so a child
    /// that ends before `start` returns is still retired: its exit waits for
    /// the record to exist.
    fn start(self: &Arc<Self>, cmd: &str, args: &[String], tag: &str, opts: StartOptions) -> bool {
        let Ok(mut live) = self.live.lock() else {
            return false;
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let sink = deliver(tag.to_string(), id, Arc::downgrade(self));
        match child::start(cmd, args, tag, &opts.child, sink) {
            Ok(running) => {
                live.push(Entry {
                    id,
                    tag: tag.to_string(),
                    running,
                    end_at_exit: opts.end_at_exit,
                });
                true
            }
            Err(message) => {
                warn_line!("lumen-process: {message}");
                false
            }
        }
    }

    /// End every running child started under `tag`, answering whether there
    /// was one.
    fn stop(&self, tag: &str) -> bool {
        let matching: Vec<child::Running> = self.select(|entry| entry.tag == tag);
        // Every child is asked, so no short-circuit.
        matching
            .iter()
            .fold(false, |stopped, running| running.stop() | stopped)
    }

    /// Drop the record of a child whose exit is about to be reported.
    fn retire(&self, id: u64) {
        if let Ok(mut live) = self.live.lock() {
            live.retain(|entry| entry.id != id);
        }
    }

    /// The handles of the recorded children `pick` chooses, taken out from
    /// under the lock so ending them does not hold it.
    fn select(&self, pick: impl Fn(&Entry) -> bool) -> Vec<child::Running> {
        self.live.lock().map_or_else(
            |_| Vec::new(),
            |live| {
                live.iter()
                    .filter(|entry| pick(entry))
                    .map(|entry| entry.running.clone())
                    .collect()
            },
        )
    }
}

/// Ends the children started with `end_at_exit: true` when the app's world is
/// torn down. The app drops its world as it exits, which drops this resource;
/// that is the whole hook, and it needs nothing from the engine.
///
/// The drop waits until those children have ended, up to the grace period a
/// stop gives a child before killing it. A process ended abruptly (a second
/// Ctrl+C, a crash) never drops its world, so its children are left running.
#[derive(Resource)]
struct EndAtExit(Arc<Children>);

impl Drop for EndAtExit {
    fn drop(&mut self) {
        let ending = self.0.select(|entry| entry.end_at_exit);
        child::stop_all(&ending);
    }
}

/// The `process` surface, described once for every host. Names, parameters,
/// and docs are the contract a script writes against.
fn script_fns(children: &Arc<Children>) -> Vec<ScriptFn> {
    let starting = Arc::clone(children);
    let stopping = Arc::clone(children);
    vec![
        ScriptFn::new("start")
            .ns(ScriptNs::Named(NAMESPACE.to_string()))
            .doc(
                "Start a program; its output and its exit arrive as events under that tag. \
                 Options: cwd, env, end_at_exit. False when it did not start.",
            )
            .param("cmd", T::Str)
            .param("args", T::Array(Box::new(T::Str)))
            .param("tag", T::Str)
            .param("opts", T::Struct(options_type()))
            .ret(T::Bool)
            .build(move |cx| {
                let cmd = cx.str_arg(0);
                let options = match start_options(cx.arg_ref(3)) {
                    Ok(options) => options,
                    Err(why) => {
                        warn_line!("lumen-process: start({cmd}): {why}");
                        return Ok(ScriptValue::Bool(false));
                    }
                };
                let args = arguments(cx.arg_ref(1));
                Ok(ScriptValue::Bool(starting.start(
                    &cmd,
                    &args,
                    &cx.str_arg(2),
                    options,
                )))
            }),
        ScriptFn::new("stop")
            .ns(ScriptNs::Named(NAMESPACE.to_string()))
            .doc(
                "End the program running under a tag; its exit still arrives as the last event. \
                 False when nothing is running under that tag.",
            )
            .param("tag", T::Str)
            .ret(T::Bool)
            .build(move |cx| Ok(ScriptValue::Bool(stopping.stop(&cx.str_arg(0))))),
    ]
}

/// Read the options `process::start` was given. The value arrives with every
/// field present, the ones a script left out at their defaults, so a field
/// that is missing or of the wrong kind is a host that did not check it; it
/// is refused rather than guessed at.
fn start_options(value: &ScriptValue) -> Result<StartOptions, String> {
    let ScriptValue::Map(map) = value else {
        return Err(format!("options must be a {OPTIONS}"));
    };
    let mut options = StartOptions::default();
    match map.get("cwd") {
        Some(ScriptValue::Str(dir)) if dir.is_empty() => {}
        Some(ScriptValue::Str(dir)) => options.child.cwd = Some(dir.clone()),
        _ => return Err("option `cwd` must be a string".to_string()),
    }
    match map.get("env") {
        Some(ScriptValue::Map(vars)) => options.child.env = environment(vars)?,
        // An empty Lua table crosses as an empty list.
        Some(ScriptValue::Array(items)) if items.is_empty() => {}
        _ => return Err("option `env` must be a map of strings".to_string()),
    }
    match map.get("end_at_exit") {
        Some(ScriptValue::Bool(end)) => options.end_at_exit = *end,
        _ => return Err("option `end_at_exit` must be a bool".to_string()),
    }
    Ok(options)
}

/// The variables an `env` map sets, in name order. Every value is a string:
/// an environment holds nothing else, and guessing how a number should be
/// spelled is how a child ends up reading `1.0` where `1` was meant.
fn environment(
    vars: &std::collections::HashMap<String, ScriptValue>,
) -> Result<Vec<(String, String)>, String> {
    let mut env = Vec::with_capacity(vars.len());
    for (name, value) in vars {
        match value {
            ScriptValue::Str(text) => env.push((name.clone(), text.clone())),
            _ => return Err(format!("`env` value for `{name}` must be a string")),
        }
    }
    env.sort();
    Ok(env)
}

/// The sink one child's lines and its exit travel through: the generic
/// plugin-event bus, keyed by the tag the script named. The exit retires the
/// child's record first, so a `process::stop` from the exit handler already
/// answers false.
fn deliver(tag: String, id: u64, children: Weak<Children>) -> child::Emit {
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
            child::Event::Exit(code) => {
                if let Some(children) = children.upgrade() {
                    children.retire(id);
                }
                ("process_exit", "on_process_exit", ScriptValue::I64(code))
            }
        };
        push_plugin_event(PluginEvent::Call {
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

    fn map(entries: &[(&str, ScriptValue)]) -> ScriptValue {
        ScriptValue::Map(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
        )
    }

    fn text(s: &str) -> ScriptValue {
        ScriptValue::Str(s.to_string())
    }

    /// The options as the body receives them: what the script set, with the
    /// rest filled in the way every host fills them.
    fn given(entries: &[(&str, ScriptValue)]) -> Result<StartOptions, String> {
        start_options(&options_type().complete(&map(entries)))
    }

    /// No fields set, an empty map, and an empty Lua table are all the
    /// defaults: the app directory, the inherited environment, and a child
    /// that outlives the app.
    #[test]
    fn no_options_are_the_defaults() {
        for value in [map(&[]), ScriptValue::Array(Vec::new())] {
            assert_eq!(
                start_options(&options_type().complete(&value)),
                Ok(StartOptions::default())
            );
        }
        assert!(!StartOptions::default().end_at_exit);
        assert_eq!(StartOptions::default().child.cwd, None);
    }

    /// Every field reads into the options it names.
    #[test]
    fn every_option_is_read() {
        let options = given(&[
            ("cwd", text("instances/a")),
            ("env", map(&[("B", text("2")), ("A", text("1"))])),
            ("end_at_exit", ScriptValue::Bool(true)),
        ])
        .expect("valid options");
        assert_eq!(options.child.cwd.as_deref(), Some("instances/a"));
        assert_eq!(
            options.child.env,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string())
            ]
        );
        assert!(options.end_at_exit);
    }

    /// The declared struct refuses an unknown field or a value of the wrong
    /// kind before the body runs, naming it; that is the check Rhai and Lua
    /// get, and candela refuses the same at compile time.
    #[test]
    fn a_bad_option_is_refused_by_name() {
        let start = script_fns(&Arc::new(Children::default()))
            .into_iter()
            .find(|f| f.name == "start")
            .expect("start is registered");
        let refused = |opts: ScriptValue, needle: &str| {
            let args = [
                text("true"),
                ScriptValue::Array(Vec::new()),
                text("t"),
                opts,
            ];
            let why = start.sig.check_args(&args).expect_err("refused");
            assert!(why.contains(needle), "{why:?} should name {needle}");
        };
        refused(map(&[("cdw", text("x"))]), "no field `cdw`");
        refused(map(&[("cwd", ScriptValue::I64(1))]), "`cwd`");
        refused(map(&[("env", text("A=1"))]), "`env`");
        refused(
            map(&[("env", map(&[("PORT", ScriptValue::I64(80))]))]),
            "`env`",
        );
        refused(map(&[("end_at_exit", text("stop"))]), "`end_at_exit`");
        refused(text("cwd"), "StartOptions");
        assert!(
            start
                .sig
                .check_args(&[text("true"), ScriptValue::Array(Vec::new()), text("t")])
                .is_err(),
            "the options are required"
        );
    }
}
