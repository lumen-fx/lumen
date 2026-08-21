//! The from-source candela host: compiles a program with [`candela::Engine`]
//! and runs it through [`candela::Program`].
//!
//! This is the host a desktop app runs. It carries the compiler, so a script
//! can be edited and reloaded while the app is up, and `lumenc check` can
//! report a compile error without a program ever running. The artifact host
//! ([`crate::vm_host`]) runs the same builtins without a compiler.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::*;
use candela_vm::Value;
use lumen_core::prelude::{App, Plugin};
use lumen_script::{
    CallOutcome, ScriptCommand, ScriptContext, ScriptError, ScriptFn, ScriptFnStore, ScriptHost,
    ScriptPlugin, ScriptValue,
};

use crate::host_fns::{Registries, register_lumen_host_fns, register_script_fn};
use crate::lmn;
use crate::prelude;
use crate::value::{candela_value_to_script, script_value_to_candela};

/// candela's [`Engine`](candela::Engine) and the compiled [`Program`](candela::Program)
/// bundled behind a hand-checked `Send`/`Sync` boundary.
///
/// # Safety discipline
///
/// candela's `Engine` and `Program` embed `Rc` (host-fn dispatch closures + the
/// VM's heap pools), so they are `!Send`/`!Sync`. The [`ScriptHost`] supertrait
/// nonetheless requires `Send + Sync` (hosts sit in a plain bevy `Resource`).
/// The genuinely correct fix is a `NonSend` plugin variant in the runtime
/// crate (the trait doc calls this out) - but that crate is out of scope for
/// this backend, so we assert the bound here instead.
///
/// The assertion is sound because Lumen only ever touches this VM through
/// `&mut CandelaHost`: bevy hands a `Resource` out under exclusive access, so the
/// embedded `Rc`s are never aliased across threads. Every `&self`
/// [`ScriptHost`] method deliberately reads only the `Arc`-guarded
/// [`Registries`] (which are truly `Send + Sync`) and never this field, so a
/// shared `&CandelaHost` never reaches the `Rc`s.
struct CandelaVm {
    engine: candela::Engine,
    program: Option<candela::Program>,
}

// SAFETY: see the `CandelaVm` doc comment. The inner candela state is only accessed
// under exclusive `&mut CandelaHost`; no `&self` path reaches it.
unsafe impl Send for CandelaVm {}
// SAFETY: see the `CandelaVm` doc comment.
unsafe impl Sync for CandelaVm {}

/// A candela [`ScriptHost`]: compiles + runs candela programs and exposes the
/// host-neutral registries the generic runtime drives.
#[derive(Resource)]
pub struct CandelaHost {
    vm: CandelaVm,
    registries: Registries,
    /// Source of the currently-loaded program, kept so `Diagnostic` byte
    /// spans can be resolved to `(line, col)` for compile errors.
    source: String,
    /// The [`ScriptFn`]s an embedder registered, kept so `compile_check` can
    /// replay them into the scratch engine it builds.
    script_fns: ScriptFnStore,
}

impl Default for CandelaHost {
    fn default() -> Self {
        Self::new()
    }
}

impl CandelaHost {
    /// Construct a fresh host with the `lumen` builtins registered and no
    /// program loaded.
    #[must_use]
    pub fn new() -> Self {
        let registries = Registries::default();
        let engine = build_engine(&registries);
        Self {
            vm: CandelaVm {
                engine,
                program: None,
            },
            registries,
            source: String::new(),
            script_fns: ScriptFnStore::default(),
        }
    }

    /// Mutable access to the inner candela [`Engine`](candela::Engine) so an
    /// embedder can register additional host functions (theme / navigation /
    /// FFI hooks) under their own namespace BEFORE the script source is
    /// compiled. Lumen itself only registers the UI/script primitives. Mirrors
    /// `LuaHost::lua_mut` / `RhaiHost::engine_mut`.
    pub fn engine_mut(&mut self) -> &mut candela::Engine {
        &mut self.vm.engine
    }

    /// Record what `source` declares, so the `lmn!` expander can map a
    /// component's props onto the function it names. Every compile path calls
    /// this on the source it is about to hand candela.
    fn index_source(&self, source: &str) {
        *self.registries.fn_index.lock().unwrap() = lmn::FnIndex::scan(source);
    }

    /// Map a candela compile-phase [`Diagnostic`](candela::Diagnostic) to the
    /// structured [`ScriptError::Compile`], resolving `(line, col)`.
    fn compile_error(&self, d: &candela::Diagnostic, uri: &str) -> ScriptError {
        let (line, col) = span_line_col(&self.source, d.span.start);
        ScriptError::Compile {
            uri: uri.to_owned(),
            line,
            col,
            message: d.message.clone(),
        }
    }
}

/// Build an [`Engine`](candela::Engine) carrying the whole builtin surface,
/// each closure closing over `r`'s registries.
/// The `lmn!` expander: a markup block becomes the candela call that
/// instantiates it, against whatever `index` last recorded.
fn lmn_expander(
    index: Arc<Mutex<lmn::FnIndex>>,
) -> impl Fn(&str) -> Result<String, candela::macros::MacroError> + 'static {
    move |body: &str| {
        let index = index.lock().unwrap();
        lmn::expand(body, &index).map_err(|e| candela::macros::MacroError::at(e.message, e.offset))
    }
}

fn build_engine(r: &Registries) -> candela::Engine {
    let mut engine = candela::Engine::new();
    register_lumen_host_fns(&mut engine, r);
    engine.register_macro(lmn::MACRO_NAME, lmn_expander(r.fn_index.clone()));
    engine
}

impl ScriptHost for CandelaHost {
    // `lmn!` is candela's macro, so a component marker in the tree names a
    // function in this program and no other host should answer for it.
    const FILLS_COMPONENTS: bool = true;

    /// candela has no first-class closure value (higher-order calls inline by
    /// symbol at compile time), so a derivation body is modeled by the script
    /// function's *name*. `derive(name, deps, f)` passes `f` as that name, and
    /// [`Self::call_closure`] re-invokes it via [`candela::Program::call`].
    type Closure = String;

    fn compile_check(&self, source: &str, uri: &str) -> Result<(), ScriptError> {
        // Compile against a throwaway engine + registries: candela's `compile`
        // runs `main` once (module instantiation), so checking on the live
        // engine would leak that run's commands into the real sink. A scratch
        // engine keeps the check side-effect free, matching the trait contract.
        let scratch = Registries::default();
        let mut engine = build_engine(&scratch);
        // Replay the embedder's registrations. candela binds every `host` block
        // while it compiles, so a source that declares `host "native" { .. }`
        // does not check against an engine carrying only Lumen's own builtins:
        // the check would fail on an app that runs.
        for f in self.script_fns.iter() {
            register_script_fn(&mut engine, &scratch, f);
        }
        // Splice the `host "lumen" { ... }` prelude in for a sentinel import; the
        // splice is single-line, so `span_line_col` still resolves user lines.
        let resolved = prelude::resolve_prelude(source);
        *scratch.fn_index.lock().unwrap() = lmn::FnIndex::scan(resolved.as_ref());
        engine
            .compile(resolved.as_ref(), uri)
            .map(|_| ())
            .map_err(|d| {
                let (line, col) = span_line_col(resolved.as_ref(), d.span.start);
                ScriptError::Compile {
                    uri: uri.to_owned(),
                    line,
                    col,
                    message: d.message,
                }
            })
    }

    fn load(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        let resolved = prelude::resolve_prelude(source);
        self.index_source(resolved.as_ref());
        let program = self
            .vm
            .engine
            .compile(resolved.as_ref(), uri)
            .map_err(|d| self.compile_error(&d, uri))?;
        self.source = source.to_owned();
        self.vm.program = Some(program);
        Ok(())
    }

    fn replace(&mut self, source: &str, uri: &str) -> Result<(), ScriptError> {
        // Compile first (no live state touched on a parse error), then swap.
        // Snapshot + clear the registries so the re-run of `main` that
        // compilation performs repopulates them from the new source, with full
        // rollback on failure. The signal mirror is preserved across the swap.
        let prior_handlers = self.registries.handlers.read().unwrap().clone();
        let prior_event_handlers = self.registries.event_handlers.read().unwrap().clone();
        let prior_bindings = lumen_script::event::take_host_bindings();
        self.registries.handlers.write().unwrap().clear();
        self.registries.event_handlers.write().unwrap().clear();

        let resolved = prelude::resolve_prelude(source);
        self.index_source(resolved.as_ref());
        match self.vm.engine.compile(resolved.as_ref(), uri) {
            Ok(program) => {
                self.source = source.to_owned();
                self.vm.program = Some(program);
                // Merge the snapshot back under the new registrations. candela
                // has no top level beyond `main`, and apps bind from
                // `on_start`, which the runtime fires once at app construction
                // and never re-fires; without the merge the handler map would
                // come back empty and every click would reach nothing.
                lumen_script::carry_forward(
                    &mut self.registries.handlers.write().unwrap(),
                    prior_handlers,
                );
                let dropped = lumen_script::event::restore_host_bindings(prior_bindings);
                let mut events = self.registries.event_handlers.write().unwrap();
                lumen_script::carry_forward(&mut events, prior_event_handlers);
                for token in dropped {
                    events.remove(&token);
                }
                Ok(())
            }
            Err(d) => {
                *self.registries.handlers.write().unwrap() = prior_handlers;
                *self.registries.event_handlers.write().unwrap() = prior_event_handlers;
                lumen_script::event::clear_host_bindings();
                lumen_script::event::restore_host_bindings(prior_bindings);
                Err(self.compile_error(&d, uri))
            }
        }
    }

    fn reset(&mut self) {
        self.vm.program = None;
        self.source.clear();
        self.registries.reset();
    }

    fn call(&mut self, fn_name: &str, args: &[ScriptValue]) -> Result<CallOutcome, ScriptError> {
        let kargs: Vec<Value> = args.iter().map(script_value_to_candela).collect();
        let mut runtime_err: Option<ScriptError> = None;
        let mut ret: Option<ScriptValue> = None;

        if let Some(program) = self.vm.program.as_mut() {
            match program.call(fn_name, &kargs) {
                Ok(value) => ret = Some(candela_value_to_script(&value)),
                // A missing function is silent success - the runtime probes
                // optional handlers (`on_start`, `on_click`, ...) and treats a
                // miss as `found: false`. candela reports it as an
                // `unknown_function` compile diagnostic naming the callee.
                Err(d) if d.code == "unknown_function" && d.message.contains(fn_name) => {}
                Err(d) => runtime_err = Some(ScriptError::Runtime(d.message)),
            }
        }

        // Drain even on error / miss: builtins may have queued commands before
        // the failure (the v1 host behaved this way and the runtime relies on
        // it).
        let commands = self.registries.drain();
        if let Some(err) = runtime_err {
            return Err(err);
        }
        Ok(CallOutcome {
            commands,
            found: ret.is_some(),
            ret,
        })
    }

    fn call_closure(
        &mut self,
        closure: &Self::Closure,
        args: &[ScriptValue],
    ) -> Result<ScriptValue, ScriptError> {
        let kargs: Vec<Value> = args.iter().map(script_value_to_candela).collect();
        let program = self
            .vm
            .program
            .as_mut()
            .ok_or_else(|| ScriptError::Runtime("no candela program loaded".to_owned()))?;
        program
            .call(closure, &kargs)
            .map(|v| candela_value_to_script(&v))
            .map_err(|d| ScriptError::Runtime(d.message))
    }

    fn dispatch_event_handler(&mut self, token: u64) -> Result<bool, ScriptError> {
        let Some(name) = self.registries.event_handler(token) else {
            return Ok(false);
        };
        // The handler receives the event id (the token); its `event_*`
        // accessors read the current-event cell. Commands it queues drain
        // through the normal sink path.
        let Some(program) = self.vm.program.as_mut() else {
            return Ok(false);
        };
        let arg = [Value::Int(token as i64)];
        match program.call(&name, &arg) {
            Ok(_) => Ok(true),
            Err(d) if d.code == "unknown_function" && d.message.contains(&name) => Ok(false),
            Err(d) => Err(ScriptError::Runtime(d.message)),
        }
    }

    fn drop_event_handler(&mut self, token: u64) {
        self.registries.drop_event_handler(token);
    }

    fn drain_commands(&mut self) -> Vec<ScriptCommand> {
        self.registries.drain()
    }

    fn push_commands(&mut self, cmds: Vec<ScriptCommand>) {
        self.registries.push_front(cmds);
    }

    fn mirror_get(&self, name: &str) -> Option<ScriptValue> {
        self.registries.mirror_get(name)
    }

    fn mirror_set(&mut self, name: &str, value: ScriptValue) {
        self.registries.mirror_set(name, value);
    }

    fn mirror_sync_str(&mut self, name: &str, value: &str) {
        self.registries.mirror_sync_str(name, value);
    }

    fn handler_for(&self, event: &str, key: &str) -> Option<String> {
        self.registries.handler_for(event, key)
    }

    fn derivations_matching(
        &self,
        dirty: &HashSet<&str>,
        pending: &HashSet<String>,
    ) -> Vec<(String, Vec<String>, Self::Closure)> {
        self.registries.derivations_matching(dirty, pending)
    }

    fn pending_initial(&self) -> HashSet<String> {
        self.registries.pending_initial()
    }

    fn clear_pending(&mut self, evaluated: &[String]) {
        self.registries.clear_pending(evaluated);
    }

    fn register_script_fn(&mut self, f: &ScriptFn) -> Result<(), ScriptError> {
        // candela host fns are registered before `compile`; the variadic
        // registration hands the closure a `&[Value]` slice of any length, so
        // one registration serves any arity (like the Lua host). The script
        // declares it in a `host "<ns>" { ... }` block with a `...` arg list
        // and calls it as `<ns>::<name>(...)`.
        register_script_fn(&mut self.vm.engine, &self.registries, f);
        self.script_fns.record(f);
        Ok(())
    }

    fn lang(&self) -> &'static str {
        "candela"
    }

    fn builtins(&self) -> &'static [lumen_script::BuiltinFn] {
        crate::BUILTINS
    }
}

/// [`ScriptContext`] borrowing the live [`CandelaHost`] state, mirroring the Rhai
/// host's `RhaiScriptContext`. Reads + writes flow through the same signal
/// mirror the script side sees. Array writes build the `SetArray` command
/// host-side (no candela marshalling), so they work despite the scalar boundary.
pub struct CandelaScriptContext<'a> {
    host: &'a mut CandelaHost,
}

impl<'a> CandelaScriptContext<'a> {
    /// Borrow a context over `host`.
    #[must_use]
    pub fn new(host: &'a mut CandelaHost) -> Self {
        Self { host }
    }
}

impl ScriptContext for CandelaScriptContext<'_> {
    fn get(&self, name: &str) -> Option<ScriptValue> {
        self.host.registries.mirror_get(name)
    }

    fn set(&mut self, name: &str, value: ScriptValue) {
        self.host.registries.set_signal(name, value);
    }

    fn array_push(&mut self, name: &str, value: ScriptValue) {
        let mut next = self.host.registries.array_items(name);
        next.push(value);
        self.host.registries.set_array(name, next);
    }

    fn array_clear(&mut self, name: &str) {
        self.host.registries.set_array(name, Vec::new());
    }
}

/// Compile a candela program to the `.cdlb` bytecode image a compiler-free
/// runtime loads, with the `lumen.cdl` prelude spliced in exactly as
/// [`CandelaHost::load`] splices it.
///
/// This is the ahead-of-time counterpart to
/// [`compile_check`](ScriptHost::compile_check): same source, same prelude,
/// same diagnostics, but the product is an image rather than a verdict. A
/// build step calls it so a shipped app carries the compiled program beside
/// its source, and a host that links `candela-vm` without the compiler has
/// something to run.
///
/// The image binds its `host "lumen" { ... }` declarations by name at load,
/// so the runtime that loads it registers the same closures
/// [`CandelaHost`] does or the load fails naming what is missing.
///
/// # Errors
///
/// [`ScriptError::Compile`] when the program does not compile, carrying the
/// line and column in the user's own source, and [`ScriptError::Runtime`]
/// when a compiled program cannot be serialized.
pub fn compile_bytecode(source: &str, uri: &str) -> Result<Vec<u8>, ScriptError> {
    let resolved = prelude::resolve_prelude(source);
    // `build_bytecode` is a free function with no engine behind it, so the
    // `lmn!` expander comes from an environment installed for this compile.
    let mut macros = candela::macros::MacroEnv::new();
    macros.register(
        lmn::MACRO_NAME,
        lmn_expander(Arc::new(Mutex::new(lmn::FnIndex::scan(resolved.as_ref())))),
    );
    // candela reports a compile error by unwinding into the diagnostic sink
    // `collect_diagnostic` installs. Without the sink the same error ends the
    // process, which a build tool must not do to the shell it was run from.
    macros
        .scope(|| {
            candela::collect_diagnostic(|| candela::build_bytecode(resolved.to_string(), uri))
        })
        .map_err(|d| {
            let (line, col) = span_line_col(resolved.as_ref(), d.span.start);
            ScriptError::Compile {
                uri: uri.to_owned(),
                line,
                col,
                message: d.message,
            }
        })?
        .map_err(ScriptError::Runtime)
}

// -----------------------------------------------------------------------------
// Plugin
// -----------------------------------------------------------------------------

/// A single `candela::Engine` extension callback; aliased to keep clippy's
/// `type_complexity` lint quiet (mirrors `lumen-script-lua`'s `LuaExtension`).
type CandelaExtension = Box<dyn FnOnce(&mut candela::Engine) + Send + 'static>;

/// Plugin: build a [`CandelaHost`], apply embedder extensions, and delegate to the
/// host-generic [`ScriptPlugin`](lumen_script::ScriptPlugin) - which
/// loads the source (stderr banner + `ScriptLoadFailure` on failure), fires
/// `on_start`, installs the host resource, and registers the full dispatcher /
/// derivation / timer / fetch system set.
///
/// Selectable alternative to `lumen_script_rhai::ScriptRhaiPlugin` /
/// `lumen_script_lua::ScriptLuaPlugin`; identical shape so an embedder swaps one
/// for the other.
pub struct ScriptCandelaPlugin {
    /// Inline candela source loaded on app start.
    pub source: String,
    /// Source URI reported in compile errors and used as the base directory for
    /// a `dylib "..."` import's library search. Defaults to `<inline>`; set it
    /// to the entry file path so a bare `dylib "md"` resolves `libmd.so` next to
    /// the app under `lumenc run`, matching `lumenc check`.
    pub uri: Option<String>,
    /// Extension callbacks invoked on the inner `candela::Engine` after Lumen's
    /// built-in `host "lumen" { ... }` registrations but before the script is
    /// compiled. Use this to register app-specific host functions (`page()`,
    /// theme, FFI, OS APIs) that the script declares in its own `host` block.
    pub extensions: Vec<CandelaExtension>,
}

impl ScriptCandelaPlugin {
    /// Wrap a source string.
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            uri: None,
            extensions: Vec::new(),
        }
    }

    /// Set the source URI (typically the entry file path). Reported in compile
    /// errors and used as the base directory for `dylib` library resolution.
    #[must_use]
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    /// Register a callback that runs on the inner `candela::Engine` before the
    /// script compiles. Lets the embedding binary register extra host functions
    /// without forking the framework crate.
    #[must_use]
    pub fn with_extension<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut candela::Engine) + Send + 'static,
    {
        self.extensions.push(Box::new(f));
        self
    }
}

impl Plugin for ScriptCandelaPlugin {
    fn build(self, app: &mut App) {
        let mut host = CandelaHost::new();
        for ext in self.extensions {
            ext(host.engine_mut());
        }
        let mut plugin = ScriptPlugin::new(host, self.source);
        if let Some(uri) = self.uri {
            plugin = plugin.with_uri(uri);
        }
        plugin.build(app);
    }
}

/// Byte-span -> `(line, col)` over a source string. `(0, 0)` for the
/// unknown-position sentinel candela uses when it has no span.
fn span_line_col(source: &str, byte: usize) -> (u32, u32) {
    if byte == 0 {
        return (0, 0);
    }
    let mut line = 1u32;
    let mut col = 1u32;
    for (i, ch) in source.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
