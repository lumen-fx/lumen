//! candela [`ScriptHost`](lumen_script::ScriptHost) backends for Lumen.
//!
//! candela is the intended default Lumen script language; this crate is the
//! sibling of [`lumen-script-rhai`](../lumen_script_rhai/index.html) (the
//! compat host). It bridges the scalar host builtins onto the same
//! host-neutral registries the generic runtime (`lumen-script`) drives - the
//! command sink, the signal mirror, and the per-id handler registry - and
//! dispatches lifecycle (`on_start`) plus event handlers exactly like the Rhai
//! host.
//!
//! # Two hosts, one builtin surface
//!
//! A candela program reaches Lumen either way it is shipped:
//!
//! - `CandelaHost` compiles source with candela's `Engine` and runs it. It
//!   carries the compiler, so a script can be edited and hot-reloaded while the
//!   app is up, and `lumenc check` can report a compile error without running
//!   anything.
//! - [`CandelaVmHost`] loads a precompiled `.cdlb` image on `candela-vm`. No
//!   compiler is in the process, which is what a shipped app and the browser
//!   target want; `CandelaHost::compile_bytecode` is the build step that
//!   produces the image, folding in whatever the build host had registered
//!   through [`ScriptHost::register_script_fn`](lumen_script::ScriptHost::register_script_fn)
//!   exactly as a live compile does, so a module or plugin function needs no
//!   hand-written `host "<ns>" { .. }` block to reach the image.
//!
//! Both register the identical builtin list, written once in `host_fns` behind
//! [`HostFnSink`]. `candela::Engine` binds those closures against the `host`
//! declarations a fresh compile produced, and `candela_vm::HostRegistry` binds
//! them against the declarations the artifact recorded; the checks are the
//! same, so an artifact that loads is bound as strictly as a script that
//! compiles. Adding a builtin in one place adds it to both.
//!
//! The compiler half is behind the default-on `compiler` feature. Turning it
//! off leaves the artifact host, the builtin surface, and the prelude, and
//! drops the compiler front-end from the dependency graph.
//!
//! # Builtin surface + remaining gaps
//!
//! candela's embedding `Value` carries `Array` and `Map`
//! variants alongside string / int / float / bool / null, and the host-fn
//! marshalling (candela's `FromHostValue` / `IntoHostValue`) accepts / returns
//! `Vec<T>` and `{string: T}` maps. So [`ScriptValue`](lumen_script::ScriptValue)
//! round-trips structured values recursively across `call` / `call_closure` /
//! the signal mirror.
//!
//! Two host-neutral extension points work through the newer embedding API:
//!
//! - `derive(name, deps, f)`: the dep list marshals as a `string[]`, and -
//!   since candela has no first-class closure value - the recompute body is
//!   passed by the script function's *name* (a plain string), which
//!   `ScriptHost::call_closure` re-invokes. This matches how candela already
//!   references functions (by symbol).
//! - `register_script_fn`: the host-neutral
//!   [`ScriptFn`](lumen_script::ScriptFn) an app, a plugin, the C ABI or the
//!   Rust SDK describes. A signature candela can name binds typed, so the call
//!   site is checked when the program compiles; a variadic or `any` signature
//!   binds as a `&[Value]` slice and is declared with a `...` arg list. A
//!   function that fails raises `host_fn_error` at the call, which the script
//!   can catch.
//!
//! # Dynamically-shaped builtins
//!
//! A fixed host-fn signature names one concrete host type: scalars,
//! homogeneous arrays, and string-keyed maps of one value type. The builtins
//! whose value has no single such shape - an array signal's records, an `http`
//! request map, `parse_json`'s result, a markdown block list, a matched-rule
//! list - register variadically instead. The script declares them with a `...`
//! argument list and, where they return a value, the `any` return type
//! candela's type checker treats permissively:
//!
//! ```candela
//! host "lumen" {
//!     signal_array_push(...);
//!     any parse_json(...);
//! }
//! ```
//!
//! Runtime marshalling is unaffected: the VM converts whatever `Value` the
//! closure returns, so nested maps and arrays round-trip. Read the result with
//! candela's `as_map` / `as_list` / `as_str` / `as_int` downcasts.
//!
//! One Rhai spelling has no candela counterpart and stays absent:
//!
//! | Rhai builtin | why it is still blocked |
//! |---|---|
//! | `signals.a.b.set(v)` chaining | Rhai's property-chain fallback has no candela analogue; write the path out (`lumen::signal_set("a.b", v)`). |
//!
//! `signal(name)` is a prelude struct rather than a host fn: candela has no
//! user-defined value object type to hand back, so `Signal` holds only the
//! signal *name* and its methods call the name-keyed `signal_get_*` /
//! `signal_set_*` builtins. `ArraySignal` works the same way.
//!
//! Because candela reaches builtins through a typed `host "lumen" { ... }` block
//! (rather than Rhai's bare globals), a Lumen candela script opts into the
//! builtins it uses, e.g.
//!
//! ```candela
//! host "lumen" {
//!     string signal_get(string);
//!     signal_set(string, string);
//!     on(string, string, string);
//! }
//!
//! fn on_start() {
//!     lumen::signal_set("greeting", "hi");
//!     lumen::on("click", "save", "handle_save");
//! }
//! ```
//!
//! Or opt into the *whole* surface with one line, `import "lumen.cdl";`,
//! which [`resolve_prelude`] splices into the equivalent `host "lumen" { ... }`
//! block before compilation (see the [`prelude`] module). Without the import
//! (or a hand-written block) the builtins stay opt-in: candela resolves host fns
//! lazily, so the source loads, but *calling* one is a runtime error
//! (`"lumen is not a valid namespace"`).

#![warn(missing_docs)]

pub mod builtins;
mod declare;
#[cfg(feature = "compiler")]
mod engine_host;
mod host_fns;
mod library_dir;
pub mod lmn;
pub mod prelude;
mod value;
mod vm_host;

pub use builtins::{BUILTINS, BuiltinFn, BuiltinParam};
pub use host_fns::{HOST_NAMESPACE, HostFnSink, NATIVE_NAMESPACE};
pub use prelude::{PRELUDE_MODULE, PRELUDE_SOURCE, resolve_prelude};
pub use vm_host::{CandelaVmHost, ScriptCandelaVmPlugin, image_exports};

#[cfg(feature = "compiler")]
pub use engine_host::{CandelaHost, CandelaScriptContext, ScriptCandelaPlugin};

// Re-export the underlying candela crate so embedders (lumenc) can name
// `candela::Engine` / `candela::Value` for `ScriptCandelaPlugin::with_extension`
// closures without declaring their own direct `candela` git dependency.
#[cfg(feature = "compiler")]
pub use candela;
// The runtime half of the same surface, for an embedder that ships artifacts
// and never links the compiler.
pub use candela_vm;

// Host-generic runtime re-exports, mirroring `lumen-script-rhai`: embedders
// (lumenc, tests) instantiate these generic systems as e.g.
// `tick_script::<CandelaHost>`.
pub use lumen_script::{
    FetchRegistry, ScriptCommandEvent, ScriptLoadFailure, ScriptPlugin, ScriptStartedAt,
    TimerRegistry, apply_derivations, dispatch_clicks_and_doubles, dispatch_close_to_script,
    drain_fetch_commands, drain_timer_commands, fire_due_timers, fire_fetched_responses,
    reload_script, sync_signals_into_host, tick_script,
};
