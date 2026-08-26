//! Test-only runtime plugin. What it registers is driven by the module's
//! `config` table, so one build of this cdylib covers the happy path and
//! every failure mode a real plugin can reach:
//!
//! - `ns`: `"extension"` (the default) or `"named:<name>"`, applied to every
//!   registered function.
//! - `fn_count`: that many `fixture_pad<i>` functions after the ones below,
//!   so a manifest's order and a call's index are checked past index zero.
//! - `prelude`: candela source registered under the same namespace.
//! - `fail_in_init` / `panic_in_init`: registration fails, or panics.
//! - `empty_hosts` / `declare_builtin_ns` / `duplicate_name`: a manifest the
//!   host must refuse.
//! - `thread_events` / `thread_commands`: spawn a thread that pushes that
//!   many events at the app and stops.
//! - env `LUMEN_RT_FIXTURE_CTOR_PANIC`: the constructor itself panics, for
//!   the first-call-construction failure path.
//!
//! The per-call behaviors (every return shape, a failure, emitted commands, a
//! panic, a host call) are functions rather than config, so one loaded copy
//! covers them all:
//!
//! - `fixture_echo(v)`: returns its argument.
//! - `fixture_shape(kind)`: returns a value of the named kind, one per
//!   `ScriptValue` variant.
//! - `fixture_fail(why)`: fails with `why`.
//! - `fixture_emit(n)`: emits `n` print commands.
//! - `fixture_emit_then_fail()`: emits one, then fails.
//! - `fixture_panic()`: panics.
//! - `fixture_event(key)`: pushes an event; returns whether it was taken.
//! - `fixture_commands()`: pushes commands; returns whether they were taken.
//! - `fixture_log(message)`: writes a line to the engine's log.
//! - `fixture_push_signal(name, value)`: pushes a signal write as a
//!   command batch, the mutation the engine's event drain applies.

use std::collections::HashMap;

use lumen_plugin::abi::LogLevel;
use lumen_plugin::{
    Error, HostSet, InitCx, PluginFn, Registrar, RuntimePlugin, ScriptCommand, ScriptNs, ScriptTy,
    ScriptValue, lumen_plugin,
};
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Cfg {
    ns: Option<String>,
    fn_count: usize,
    prelude: Option<String>,
    fail_in_init: bool,
    panic_in_init: bool,
    empty_hosts: bool,
    declare_builtin_ns: bool,
    duplicate_name: bool,
    thread_events: u32,
    thread_commands: u32,
}

impl Cfg {
    /// The namespace every registered function lands in.
    fn ns(&self) -> ScriptNs {
        match self.ns.as_deref() {
            Some(spec) => match spec.strip_prefix("named:") {
                Some(name) => ScriptNs::Named(name.to_string()),
                None => ScriptNs::Extension,
            },
            None => ScriptNs::Extension,
        }
    }
}

struct FixturePlugin;

impl FixturePlugin {
    fn new() -> Self {
        if std::env::var_os("LUMEN_RT_FIXTURE_CTOR_PANIC").is_some() {
            panic!("fixture panic in constructor");
        }
        FixturePlugin
    }
}

impl RuntimePlugin for FixturePlugin {
    fn register(&self, r: &mut Registrar, cx: &InitCx) -> Result<(), Error> {
        let cfg: Cfg = cx.config()?;
        if cfg.panic_in_init {
            panic!("fixture panic in init");
        }
        if cfg.fail_in_init {
            return Err(Error::from("fixture failure in init"));
        }
        let ns = cfg.ns();

        r.script_fn(
            PluginFn::new("fixture_echo")
                .param("value", ScriptTy::Any)
                .ns(ns.clone())
                .build(|cx| Ok(cx.arg(0))),
        );
        r.script_fn(
            PluginFn::new("fixture_shape")
                .param("kind", ScriptTy::Str)
                .ns(ns.clone())
                .build(|cx| shape(&cx.str_arg(0))),
        );
        r.script_fn(
            PluginFn::new("fixture_fail")
                .param("why", ScriptTy::Str)
                .ns(ns.clone())
                .build(|cx| Err(format!("fixture failure: {}", cx.str_arg(0)))),
        );
        r.script_fn(
            PluginFn::new("fixture_emit")
                .param("count", ScriptTy::Int)
                .ret(ScriptTy::Unit)
                .ns(ns.clone())
                .build(|cx| {
                    for i in 0..cx.int_arg(0) {
                        cx.emit(ScriptCommand::Print(format!("emit {i}")));
                    }
                    Ok(ScriptValue::Unit)
                }),
        );
        r.script_fn(
            PluginFn::new("fixture_emit_then_fail")
                .ns(ns.clone())
                .build(|cx| {
                    cx.emit(ScriptCommand::Print("before the failure".to_string()));
                    Err("fixture failed after emitting".to_string())
                }),
        );
        r.script_fn(PluginFn::new("fixture_panic").ns(ns.clone()).build(|_| {
            panic!("fixture panic in call");
        }));
        r.script_fn(
            PluginFn::new("fixture_event")
                .param("key", ScriptTy::Str)
                .ret(ScriptTy::Bool)
                .ns(ns.clone())
                .build(|cx| {
                    let key = cx.str_arg(0);
                    let taken = cx.host().call_handler(
                        "on_fixture",
                        &key,
                        "on_any",
                        vec![ScriptValue::Str(key.clone())],
                    );
                    Ok(ScriptValue::Bool(taken))
                }),
        );
        r.script_fn(
            PluginFn::new("fixture_commands")
                .ret(ScriptTy::Bool)
                .ns(ns.clone())
                .build(|cx| {
                    let taken = cx
                        .host()
                        .emit(vec![ScriptCommand::Print("from the host".to_string())]);
                    Ok(ScriptValue::Bool(taken))
                }),
        );
        r.script_fn(
            PluginFn::new("fixture_log")
                .param("message", ScriptTy::Str)
                .ret(ScriptTy::Unit)
                .ns(ns.clone())
                .build(|cx| {
                    cx.host().log(LogLevel::Warn, &cx.str_arg(0));
                    Ok(ScriptValue::Unit)
                }),
        );
        r.script_fn(
            PluginFn::new("fixture_push_signal")
                .param("name", ScriptTy::Str)
                .param("value", ScriptTy::Str)
                .ret(ScriptTy::Bool)
                .ns(ns.clone())
                .build(|cx| {
                    let taken = cx.host().emit(vec![ScriptCommand::SetSignal {
                        name: cx.str_arg(0),
                        value: cx.str_arg(1),
                    }]);
                    Ok(ScriptValue::Bool(taken))
                }),
        );
        for i in 0..cfg.fn_count {
            r.script_fn(
                PluginFn::new(format!("fixture_pad{i}"))
                    .ret(ScriptTy::Int)
                    .ns(ns.clone())
                    .build(move |_| Ok(ScriptValue::I64(i as i64))),
            );
        }

        if cfg.duplicate_name {
            r.script_fn(
                PluginFn::new("fixture_echo")
                    .ns(ns.clone())
                    .build(|_| Ok(ScriptValue::Unit)),
            );
        }
        if cfg.declare_builtin_ns {
            r.script_fn(
                PluginFn::new("fixture_builtin")
                    .ns(ScriptNs::Builtin)
                    .build(|_| Ok(ScriptValue::Unit)),
            );
        }
        if cfg.empty_hosts {
            r.script_fn(
                PluginFn::new("fixture_hidden")
                    // A language Lumen does not ship is the empty set.
                    .hosts(HostSet::from_lang("nonesuch"))
                    .ns(ns.clone())
                    .build(|_| Ok(ScriptValue::Unit)),
            );
        }
        if let Some(source) = &cfg.prelude {
            let ns_name = match &ns {
                ScriptNs::Named(name) => name.clone(),
                _ => "fixture".to_string(),
            };
            r.prelude("candela", &ns_name, source);
        }

        if cfg.thread_events > 0 || cfg.thread_commands > 0 {
            let host = r.host();
            let (events, commands) = (cfg.thread_events, cfg.thread_commands);
            std::thread::spawn(move || {
                for i in 0..events {
                    host.call_handler(
                        "on_fixture",
                        &format!("thread{i}"),
                        "on_any",
                        vec![ScriptValue::I64(i64::from(i))],
                    );
                }
                for i in 0..commands {
                    host.emit(vec![ScriptCommand::Print(format!("thread command {i}"))]);
                }
            });
        }
        Ok(())
    }
}

/// One value of each [`ScriptValue`] variant, by name.
fn shape(kind: &str) -> Result<ScriptValue, String> {
    Ok(match kind {
        "unit" => ScriptValue::Unit,
        "bool" => ScriptValue::Bool(true),
        "int" => ScriptValue::I64(-7),
        "float" => ScriptValue::F64(2.5),
        "str" => ScriptValue::Str("fixture".to_string()),
        "array" => ScriptValue::Array(vec![ScriptValue::I64(1), ScriptValue::Str("two".into())]),
        "map" => ScriptValue::Map(HashMap::from([
            ("n".to_string(), ScriptValue::I64(3)),
            ("nested".to_string(), ScriptValue::Array(Vec::new())),
        ])),
        other => return Err(format!("no such shape: {other}")),
    })
}

lumen_plugin!(FixturePlugin::new);
