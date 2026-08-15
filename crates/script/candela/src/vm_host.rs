//! The artifact candela host: runs a precompiled `.cdlb` image through
//! `candela-vm`, with no compiler in the process.
//!
//! Same builtins as [`crate::engine_host`], same registries, same command
//! sink; the difference is where the program comes from. The image records the
//! `host "lumen" { ... }` declarations it made, and
//! [`register_lumen_host_fns`] fills a registry that binds them by name at
//! load. An unregistered or mis-shaped builtin is reported before a single
//! instruction runs, so a broken artifact surfaces as a load failure rather
//! than a crash mid-call.
//!
//! What the compiler carried and this host does not: source compilation, hot
//! reload, and `lumenc check`. [`ScriptHost::replace`] and
//! [`ScriptHost::compile_check`] report that rather than pretending to work.
//! Only functions the artifact exports are callable; a function is exported
//! when it is defined in the built file, is not `main`, and annotates every
//! parameter.

use std::collections::HashSet;

use bevy_ecs::prelude::*;
use candela_vm::{CallError, HostRegistry, RuntimeProgram, Value, load_program};
use lumen_core::prelude::{App, Plugin};
use lumen_script::{
    CallOutcome, CommandFn, ScriptCommand, ScriptError, ScriptHost, ScriptPlugin, ScriptValue,
};

use crate::host_fns::{Registries, register_lumen_host_fns};
use crate::value::{candela_value_to_script, script_value_to_candela};

/// The registry awaiting a load, and the program once loaded, behind a
/// hand-checked `Send`/`Sync` boundary.
///
/// # Safety discipline
///
/// A `HostRegistry` holds `Rc<dyn Fn>` dispatchers and a `RuntimeProgram` holds
/// the VM's heap pools, so both are `!Send`/`!Sync`, while [`ScriptHost`]
/// requires `Send + Sync` because a host sits in a plain bevy `Resource`. The
/// assertion is sound for the same reason it is on the compiler host: Lumen
/// only reaches this state through `&mut CandelaVmHost`, which bevy hands out
/// under exclusive access, and every `&self` method reads the `Arc`-guarded
/// [`Registries`] instead. It is stronger here than there, because the browser
/// target this host exists for is single-threaded outright.
struct VmState {
    registry: Option<HostRegistry>,
    program: Option<RuntimeProgram>,
}

// SAFETY: see the `VmState` doc comment. The inner candela state is only
// accessed under exclusive `&mut CandelaVmHost`; no `&self` path reaches it.
unsafe impl Send for VmState {}
// SAFETY: see the `VmState` doc comment.
unsafe impl Sync for VmState {}

/// A candela [`ScriptHost`] that runs a `.cdlb` image on `candela-vm`.
#[derive(Resource)]
pub struct CandelaVmHost {
    vm: VmState,
    registries: Registries,
    /// The image to load. Held rather than loaded at construction so a load
    /// failure is reported through [`ScriptHost::load`], the one place the
    /// generic plugin already turns a failure into a `ScriptLoadFailure` the
    /// embedder can show.
    image: Vec<u8>,
}

impl CandelaVmHost {
    /// Construct a host over the `.cdlb` bytes `image`, with the `lumen`
    /// builtins registered and nothing loaded yet.
    #[must_use]
    pub fn new(image: Vec<u8>) -> Self {
        let registries = Registries::default();
        let mut registry = HostRegistry::new();
        register_lumen_host_fns(&mut registry, &registries);
        Self {
            vm: VmState {
                registry: Some(registry),
                program: None,
            },
            registries,
            image,
        }
    }

    /// Mutable access to the pending [`HostRegistry`] so an embedder can
    /// register additional host functions under its own namespace before the
    /// image loads. Mirrors `CandelaHost::engine_mut`.
    ///
    /// Returns `None` once the image has loaded: `candela-vm` binds every
    /// declaration at load, and a closure registered after that has nothing
    /// left to bind to.
    pub fn registry_mut(&mut self) -> Option<&mut HostRegistry> {
        self.vm.registry.as_mut()
    }

    /// The names of every function the loaded image exports, or an empty list
    /// before the load.
    pub fn exports(&self) -> Vec<String> {
        self.vm
            .program
            .as_ref()
            .map(|p| p.exports().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

impl ScriptHost for CandelaVmHost {
    /// candela references a function by name, so a derivation body is the
    /// script function's name, exactly as on the compiler host.
    type Closure = String;

    fn compile_check(&self, _source: &str, uri: &str) -> Result<(), ScriptError> {
        Err(ScriptError::Runtime(format!(
            "{uri}: the candela artifact host carries no compiler; check the source with the \
             compiler host before building the .cdlb"
        )))
    }

    /// Load the image handed to [`Self::new`], binding its `host` declarations
    /// against the registered builtins and running `main`.
    ///
    /// The `source` argument is unused: an artifact host runs bytecode, and the
    /// program it runs was chosen at construction. `uri` names the image in the
    /// error.
    fn load(&mut self, _source: &str, uri: &str) -> Result<(), ScriptError> {
        let registry = self.vm.registry.take().ok_or_else(|| {
            ScriptError::Runtime(format!("{uri}: this candela artifact host already loaded"))
        })?;
        let mut program = load_program(&self.image, &registry).map_err(|e| {
            // Put the registry back so a retry after a swapped image can bind.
            ScriptError::Compile {
                uri: uri.to_owned(),
                line: 0,
                col: 0,
                message: e.to_string(),
            }
        })?;
        // Top-level setup: the image's `main` runs once, before the first
        // handler call, exactly as `Engine::compile` runs it on the compiler
        // host.
        program.run();
        self.vm.program = Some(program);
        Ok(())
    }

    fn replace(&mut self, _source: &str, uri: &str) -> Result<(), ScriptError> {
        Err(ScriptError::Runtime(format!(
            "{uri}: the candela artifact host carries no compiler, so it cannot reload from source"
        )))
    }

    fn reset(&mut self) {
        self.vm.program = None;
        self.registries.reset();
    }

    fn call(&mut self, fn_name: &str, args: &[ScriptValue]) -> Result<CallOutcome, ScriptError> {
        let kargs: Vec<Value> = args.iter().map(script_value_to_candela).collect();
        let mut runtime_err: Option<ScriptError> = None;
        let mut ret: Option<ScriptValue> = None;

        if let Some(program) = self.vm.program.as_mut() {
            match program.call(fn_name, &kargs) {
                Ok(value) => ret = Some(candela_value_to_script(&value)),
                // The runtime probes optional handlers (`on_start`,
                // `on_click`, ...) on every host and treats a miss as
                // `found: false`.
                Err(CallError::UnknownFunction(_)) => {}
                Err(e) => runtime_err = Some(ScriptError::Runtime(e.to_string())),
            }
        }

        // Drain even on error / miss: builtins may have queued commands before
        // the failure.
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
            .ok_or_else(|| ScriptError::Runtime("no candela artifact loaded".to_owned()))?;
        program
            .call(closure, &kargs)
            .map(|v| candela_value_to_script(&v))
            .map_err(|e| ScriptError::Runtime(e.to_string()))
    }

    fn dispatch_event_handler(&mut self, token: u64) -> Result<bool, ScriptError> {
        let Some(name) = self.registries.event_handler(token) else {
            return Ok(false);
        };
        let Some(program) = self.vm.program.as_mut() else {
            return Ok(false);
        };
        match program.call(&name, &[Value::Int(token as i64)]) {
            Ok(_) => Ok(true),
            Err(CallError::UnknownFunction(_)) => Ok(false),
            Err(e) => Err(ScriptError::Runtime(e.to_string())),
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

    fn register_command_fn(
        &mut self,
        name: &str,
        _arity: usize,
        f: CommandFn,
    ) -> Result<(), ScriptError> {
        let registry = self.vm.registry.as_mut().ok_or_else(|| {
            ScriptError::Runtime(format!(
                "{name}: candela-vm binds host functions when the artifact loads; register this \
                 before the load"
            ))
        })?;
        self.registries.register_command_fn(registry, name, f);
        Ok(())
    }

    fn lang(&self) -> &'static str {
        "candela"
    }

    fn builtins(&self) -> &'static [lumen_script::BuiltinFn] {
        crate::BUILTINS
    }
}

/// Plugin: build a [`CandelaVmHost`] over a `.cdlb` image and delegate to the
/// host-generic [`ScriptPlugin`](lumen_script::ScriptPlugin), which loads it,
/// fires `on_start`, installs the host resource, and registers the dispatcher /
/// derivation / timer / fetch system set.
///
/// Selectable alternative to [`ScriptCandelaPlugin`](crate::ScriptCandelaPlugin)
/// for a target that ships no compiler.
pub struct ScriptCandelaVmPlugin {
    /// The `.cdlb` image loaded on app start.
    pub image: Vec<u8>,
    /// Name reported in load errors. Defaults to `<artifact>`.
    pub uri: Option<String>,
}

impl ScriptCandelaVmPlugin {
    /// Wrap a `.cdlb` image.
    #[must_use]
    pub fn new(image: Vec<u8>) -> Self {
        Self { image, uri: None }
    }

    /// Set the name reported in load errors (typically the artifact path).
    #[must_use]
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }
}

impl Plugin for ScriptCandelaVmPlugin {
    fn build(self, app: &mut App) {
        let host = CandelaVmHost::new(self.image);
        // The artifact host takes its program from the image, so the generic
        // plugin's source string is empty; the uri is what names it.
        ScriptPlugin::new(host, String::new())
            .with_uri(self.uri.unwrap_or_else(|| "<artifact>".to_owned()))
            .build(app);
    }
}
