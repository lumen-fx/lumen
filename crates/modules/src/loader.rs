//! The dlopen half: resolve each declared dependency to a library file, tell
//! which of the two runtime kinds it is from the symbols it exports, verify
//! it, and bring it into the app.
//!
//! One `[dependencies]` table declares both kinds, and the file itself says
//! which it is:
//!
//! - A library exporting `lumen_module_probe` is an **engine-locked runtime
//!   module** (`lumen-module`): a Rust dylib sharing the running engine, with
//!   full ECS reach. It loads only into a process that links the engine
//!   dynamically, and only when its build id equals the engine's exactly.
//! - A library exporting `lumen_plugin_v1` is a **portable plugin**
//!   (`lumen-plugin`): a C-ABI cdylib exchanging serialized bytes. It loads
//!   into any process, static hosts included, as long as its ABI and wire
//!   versions match.
//! - A library exporting neither is refused with a banner naming both
//!   symbols; a compiler plugin (`lumenc_plugin_v1`) is named as one.
//!
//! See the crate docs for the two hazards the engine-locked arm exists to
//! close and for the banner-and-continue failure policy, which applies to
//! both kinds.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use bevy_ecs::prelude::Res;
use bevy_ecs::resource::Resource;
use lumen_core::app::{App, EventLoopWaker};
use lumen_core::plugin_events::push_plugin_event;
use lumen_core::tick::TickStage;
use lumen_plugin::abi::LogLevel;
use lumen_plugin::{HostHooks, PluginEvent, PluginSet, codec};

use crate::{DepCfg, DependenciesCfg, ModuleSource, ResolvedModules, library_spellings};

pub use lumen_plugin::InitEnv;

/// The C-ABI probe every engine-locked module exports: returns its
/// NUL-terminated `BUILD_ID`, read before any Rust symbol is touched.
pub const PROBE_SYMBOL: &[u8] = b"lumen_module_probe\0";
/// The Rust-ABI install entry, called only after the probe matched exactly.
pub const INSTALL_SYMBOL: &[u8] = b"lumen_module_install\0";
/// The C-ABI entry a portable plugin exports.
pub const PLUGIN_ENTRY_SYMBOL: &[u8] = b"lumen_plugin_v1\0";
/// The entry a compiler plugin exports - the wrong kind to declare here.
const COMPILER_ENTRY_SYMBOL: &[u8] = b"lumenc_plugin_v1\0";

type ProbeFn = unsafe extern "C" fn() -> *const c_char;
/// Rust ABI on purpose: probe equality has already proven both sides are the
/// same build, which is what makes `&mut App` / `&str` safe to pass.
type InstallFn = unsafe fn(&mut App, &str) -> u32;

/// Install returned cleanly.
const INSTALL_OK: u32 = 0;
/// The module's constructor or `Plugin::build` panicked (caught on the module
/// side). Kept in lockstep with `lumen-module`, which cannot be depended on
/// from here (it sits on the other side of the engine dylib).
const INSTALL_PANICKED: u32 = 1;
/// The module rejected its `config` table.
const INSTALL_BAD_CONFIG: u32 = 2;

/// What the loader did, queryable for the life of the app.
///
/// Holds metadata only, for both kinds. An engine-locked library is
/// deliberately leaked (load-forever): the schedules hold function pointers
/// into each one, and components or resources a module registered carry drop
/// glue that lives in it, so there is no point in the app's life at which
/// unloading is sound. A portable plugin's open library lives in
/// [`PortablePlugins`] beside this.
#[derive(Debug, Default, Resource)]
pub struct LoadedModules {
    /// Every dependency that installed, in load (sorted-name) order.
    pub loaded: Vec<LoadedModule>,
    /// Every dependency that did not, with the reason its banner carried.
    pub failed: Vec<ModuleFailure>,
}

/// Which runtime kind a loaded dependency turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadedKind {
    /// An engine-locked runtime module (`lumen_module_probe`).
    EngineModule,
    /// A portable C-ABI plugin (`lumen_plugin_v1`).
    PortablePlugin,
}

/// One installed dependency.
#[derive(Debug)]
pub struct LoadedModule {
    /// The declared name.
    pub name: String,
    /// The library file that was opened.
    pub path: PathBuf,
    /// Which kind the file's exports said it is.
    pub kind: LoadedKind,
    /// The build id an engine-locked module reported; equal to the engine's
    /// own. Empty for a portable plugin, whose handshake is the ABI and wire
    /// versions instead of a build id.
    pub build_id: String,
}

/// One dependency that failed to load. The app keeps running without it.
#[derive(Debug)]
pub struct ModuleFailure {
    /// The declared name.
    pub name: String,
    /// The reason, as printed in the banner (or the one-line notice the
    /// static-host refusal prints instead).
    pub reason: String,
}

/// Why one dependency did not load, and how loudly to say so. A genuine
/// failure gets the unmissable banner; the static-host refusal of an
/// engine-locked module gets one line, because that shape is a property of
/// the build rather than a defect - a static bundle compiles the module's
/// plugin in instead, and shouting MODULE LOAD FAILED on every
/// launch of a working app would teach users to ignore the banner.
struct LoadError {
    reason: String,
    banner: bool,
}

impl LoadError {
    fn banner(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            banner: true,
        }
    }

    fn notice(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            banner: false,
        }
    }
}

/// The portable plugins of one app: the loaded [`PluginSet`]s, held for the
/// process lifetime because the app's script registry points into them.
/// Dropping the resource - the app going down - tells every plugin to shut
/// down, best effort.
#[derive(Default, Resource)]
pub struct PortablePlugins {
    sets: Vec<PluginSet>,
}

impl Drop for PortablePlugins {
    fn drop(&mut self) {
        for set in &self.sets {
            set.shutdown();
        }
    }
}

/// Load every declared dependency into `app`, in sorted-name order, and
/// record the outcome in a [`LoadedModules`] resource. Any failure is a
/// stderr banner plus a failure entry; the app boots without that module.
///
/// `resolved` carries the compiler's `version`-source resolutions (see
/// [`ResolvedModules`]); pass a default when nothing resolved anything.
/// `env` is what every portable plugin is told about the app.
pub fn load_modules(
    app: &mut App,
    dir: &Path,
    deps: &DependenciesCfg,
    resolved: &ResolvedModules,
    env: &InitEnv,
) {
    let mut state = LoadedModules::default();
    let mut portable = PortablePlugins::default();
    if deps.0.is_empty() {
        app.world.insert_resource(state);
        app.world.insert_resource(portable);
        return;
    }
    // Read once per process: whether the engine is dynamically linked here.
    // `None` refuses the engine-locked arm only; a portable plugin loads
    // into a static host all the same.
    let engine_id = engine_build_id();
    // One waker slot and one hook set serve every portable plugin. The waker
    // is filled by the system below once a backend inserts the resource.
    let waker: Arc<OnceLock<EventLoopWaker>> = Arc::new(OnceLock::new());
    let hooks: Arc<dyn HostHooks> = Arc::new(BusHooks {
        waker: Arc::clone(&waker),
    });
    for dep in &deps.0 {
        match load_one(
            app,
            dir,
            dep,
            resolved,
            engine_id.as_deref(),
            env,
            &hooks,
            &mut portable,
            &state.loaded,
        ) {
            Ok(loaded) => state.loaded.push(loaded),
            Err(err) => {
                if err.banner {
                    banner(&dep.name, &err.reason);
                } else {
                    eprintln!(
                        "lumen-runtime: dependency '{}' skipped: this build compiles the \
                         engine in; a bundled or engine-locked module loads only beside the \
                         shared engine. The app runs without it.",
                        dep.name
                    );
                }
                state.failed.push(ModuleFailure {
                    name: dep.name.clone(),
                    reason: err.reason,
                });
            }
        }
    }
    if !portable.sets.is_empty() {
        app.add_systems(TickStage::Input, wire_plugin_waker(waker));
    }
    app.world.insert_resource(state);
    app.world.insert_resource(portable);
}

/// Resolve, verify, and install one dependency. `Err` carries the reason and
/// how loudly to report it.
#[allow(clippy::too_many_arguments)]
fn load_one(
    app: &mut App,
    dir: &Path,
    dep: &DepCfg,
    resolved: &ResolvedModules,
    engine_id: Option<&str>,
    env: &InitEnv,
    hooks: &Arc<dyn HostHooks>,
    portable: &mut PortablePlugins,
    already: &[LoadedModule],
) -> Result<LoadedModule, LoadError> {
    // A `bundled` dependency missing from a host that compiled the engine in
    // is the same build-shape condition the engine-locked refusal below
    // notices quietly: bundled modules ship beside the shared engine, and
    // this process has none. Every other resolution failure banners.
    let path = resolve(dir, dep, resolved).map_err(|reason| {
        if engine_id.is_none() && matches!(dep.source, ModuleSource::Bundled) {
            LoadError::notice(reason)
        } else {
            LoadError::banner(reason)
        }
    })?;
    // Two entries pointing at one file would share the library's statics,
    // and both kinds hold one instance per process; the second entry would
    // run against the first one's configuration.
    if let Some(prior) = already.iter().find(|l| same_file(&l.path, &path)) {
        return Err(LoadError::banner(format!(
            "already loaded as module '{}'; declare one library once",
            prior.name
        )));
    }
    // SAFETY: opening a first-party (or app-declared) library runs its
    // initializers; a dependency is native code in the app's process by
    // contract, the same trust model as [[hooks]]. RTLD_NOW is libloading's
    // default, so a missing symbol fails here rather than mid-frame.
    let lib = unsafe { libloading::Library::new(&path) }
        .map_err(|e| LoadError::banner(format!("could not open {}: {e}", path.display())))?;

    // Kind dispatch: the exported entry says what the file is.
    let has = |symbol: &[u8]| {
        // SAFETY: presence probe only; the symbol is never called through
        // this lookup.
        unsafe { lib.get::<ProbeFn>(symbol) }.is_ok()
    };
    if has(PROBE_SYMBOL) {
        return install_engine_module(app, dep, path, lib, engine_id);
    }
    if has(PLUGIN_ENTRY_SYMBOL) {
        return install_portable_plugin(app, dep, path, env, hooks, portable);
    }
    if has(COMPILER_ENTRY_SYMBOL) {
        return Err(LoadError::banner(format!(
            "{} exports lumenc_plugin_v1: this is a compiler plugin - declare it under \
             [[plugins]], not [dependencies]",
            path.display()
        )));
    }
    Err(LoadError::banner(format!(
        "{} exports neither lumen_module_probe nor lumen_plugin_v1; the library is not a \
         Lumen runtime module or portable plugin",
        path.display()
    )))
}

/// The engine-locked arm: verify the build id against the running engine and
/// call the Rust-ABI install entry.
fn install_engine_module(
    app: &mut App,
    dep: &DepCfg,
    path: PathBuf,
    lib: libloading::Library,
    engine_id: Option<&str>,
) -> Result<LoadedModule, LoadError> {
    // Hazard check: a host that compiled the engine in must never install an
    // engine-locked module (a second engine instance shares no worlds or
    // statics with the first, and the probe cannot tell). A portable plugin
    // took the other arm before this check on purpose. A notice rather than
    // the banner: the refusal is a property of the build shape, and a static
    // build of an app that declares a bundled module compiles the module's
    // plugin in instead.
    let Some(engine_id) = engine_id else {
        return Err(LoadError::notice(
            "this build does not link the engine dynamically; engine-locked runtime modules \
             need the dynamic engine.\nA static bundle, or a plain cargo-built binary, \
             compiles the engine into itself,\nand a module loaded there would run against a \
             second engine instance.\nA portable plugin (lumen-plugin) loads here; an \
             engine-locked module (lumen-module) does not.",
        ));
    };
    // SAFETY: the probe is a C-ABI symbol returning a NUL-terminated static;
    // it is the one symbol safe to call across a build-skewed boundary.
    let module_id = unsafe {
        let probe: libloading::Symbol<ProbeFn> = lib
            .get(PROBE_SYMBOL)
            .expect("presence was probed before dispatch");
        let p = probe();
        if p.is_null() {
            return Err(LoadError::banner(format!(
                "{}: lumen_module_probe returned null",
                path.display()
            )));
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    };
    if module_id != engine_id {
        return Err(LoadError::banner(format!(
            "{} was built against a different engine build.\n  module reports: {module_id}\n  \
             engine is:      {engine_id}\nRebuild the module against this exact engine release.",
            path.display()
        )));
    }

    let config_toml = toml::to_string(&dep.config).map_err(|e| {
        LoadError::banner(format!(
            "could not serialize the module's config table: {e}"
        ))
    })?;
    // SAFETY: exact build-id equality just proved the module and the running
    // engine are one build, which is the contract that makes the Rust-ABI
    // signature (`&mut App`, `&str`) sound to call.
    let status = unsafe {
        let install: libloading::Symbol<InstallFn> = lib.get(INSTALL_SYMBOL).map_err(|_| {
            LoadError::banner(format!(
                "{} does not export lumen_module_install",
                path.display()
            ))
        })?;
        // The module's own macro catches construction panics; this catch is
        // the belt for a panic escaping the module anyway (shared libstd, so
        // the unwind crosses cleanly). Print the payload ourselves: an app's
        // own panic hook may be silent, and the banner below promises the
        // message was printed.
        catch_unwind(AssertUnwindSafe(|| install(app, &config_toml))).unwrap_or_else(|payload| {
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("(non-string panic payload)");
            eprintln!("lumen-runtime: the module's install entry panicked: {msg}");
            INSTALL_PANICKED
        })
    };
    match status {
        INSTALL_OK => {}
        INSTALL_PANICKED => {
            return Err(LoadError::banner(
                "the module's constructor panicked during install (its message is printed \
                 above)",
            ));
        }
        INSTALL_BAD_CONFIG => {
            return Err(LoadError::banner(
                "the module rejected its `config` table (its message is printed above)",
            ));
        }
        other => {
            return Err(LoadError::banner(format!(
                "the module's install entry failed (code {other})"
            )));
        }
    }

    // Load-forever: see [`LoadedModules`] for why unloading is never sound.
    std::mem::forget(lib);
    Ok(LoadedModule {
        name: dep.name.clone(),
        path,
        kind: LoadedKind::EngineModule,
        build_id: module_id,
    })
}

/// The portable arm: hand the file to the C-ABI loader, bind what it
/// registered onto the app, and keep the set alive for the process.
fn install_portable_plugin(
    app: &mut App,
    dep: &DepCfg,
    path: PathBuf,
    env: &InitEnv,
    hooks: &Arc<dyn HostHooks>,
    portable: &mut PortablePlugins,
) -> Result<LoadedModule, LoadError> {
    let (set, mut failures) = PluginSet::load(
        &[lumen_plugin::ResolvedModule {
            name: dep.name.clone(),
            path: path.clone(),
            config: dep.config.clone(),
        }],
        env,
        Arc::clone(hooks),
    );
    if let Some(failure) = failures.pop() {
        return Err(LoadError::banner(failure.reason.to_string()));
    }
    // Before the script hosts load, like every registration here: the fns go
    // into the one `ScriptFnRegistry` each host drains as it comes up.
    set.install(app);
    portable.sets.push(set);
    Ok(LoadedModule {
        name: dep.name.clone(),
        path,
        kind: LoadedKind::PortablePlugin,
        build_id: String::new(),
    })
}

/// What the engine offers every portable plugin: events onto the core bus,
/// logs to stderr, wakes through whatever loop the backend later installs.
struct BusHooks {
    waker: Arc<OnceLock<EventLoopWaker>>,
}

impl HostHooks for BusHooks {
    fn event(&self, _module: &str, event: PluginEvent) -> bool {
        match codec::encode(&event) {
            Ok(bytes) => push_plugin_event(bytes),
            Err(_) => false,
        }
    }

    fn log(&self, module: &str, level: LogLevel, message: &str) {
        let level = match level {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        };
        eprintln!("lumen-runtime: plugin '{module}' {level}: {message}");
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.get() {
            waker.wake();
        }
    }
}

/// The system that closes the ordering gap between the loader (plugin-build
/// time) and the backend's [`EventLoopWaker`] (event-loop start): copy the
/// resource into the hooks' slot once it exists. The `OnceLock` write makes
/// every call after the first a no-op; a headless app that never inserts a
/// waker keeps hitting the `None` arm, and its driver ticks on
/// `work_pending` instead.
fn wire_plugin_waker(
    slot: Arc<OnceLock<EventLoopWaker>>,
) -> impl Fn(Option<Res<'_, EventLoopWaker>>) + Send + Sync + 'static {
    move |waker: Option<Res<EventLoopWaker>>| {
        if let Some(waker) = waker {
            let _ = slot.set(waker.clone());
        }
    }
}

/// The running engine's build id, read from the process's own loaded
/// `liblumen_engine` via its exported C symbol. `None` when the process does
/// not link the engine dynamically - the hazard the engine-locked arm
/// refuses on.
///
/// Two probes, because the engine reaches a process two ways. A binary that
/// links it (`lumenc`, a packaged Rust app) has it among its own `NEEDED`
/// libraries, in the global symbol scope the first probe searches. A host
/// that dlopens `liblumen` (the launcher, the C++ and Python SDKs) pulls the
/// engine in as a dependency of that local-scope load, where the global
/// search cannot see it; `RTLD_NOLOAD` asks the dynamic linker whether the
/// library is mapped in the process at all, whatever scope it arrived in,
/// and loads nothing when it is not.
fn engine_build_id() -> Option<String> {
    #[cfg(unix)]
    {
        use libloading::os::unix::{Library, RTLD_LAZY, Symbol};

        // SAFETY: looking up and calling the engine's own C-ABI export, which
        // returns a NUL-terminated static valid for the process lifetime.
        unsafe {
            let this = Library::this();
            if let Ok(sym) = this.get::<ProbeFn>(b"lumen_engine_build_id\0") {
                return read_probe(&sym);
            }
        }
        for name in ["liblumen_engine.so", "liblumen_engine.dylib"] {
            // SAFETY: RTLD_NOLOAD never runs initializers - it only returns a
            // handle to a library some earlier load already mapped - and the
            // symbol contract is the same as above.
            unsafe {
                let Ok(lib) = Library::open(Some(name), libc::RTLD_NOLOAD | RTLD_LAZY) else {
                    continue;
                };
                let id = lib
                    .get::<ProbeFn>(b"lumen_engine_build_id\0")
                    .ok()
                    .and_then(|sym: Symbol<ProbeFn>| read_probe(&sym));
                // Balance nothing: the handle came from a library already
                // held open by whoever loaded it, but dropping would still
                // dlclose one reference, so keep ours for the process's life
                // like every other engine mapping.
                std::mem::forget(lib);
                if id.is_some() {
                    return id;
                }
            }
        }
        None
    }
    #[cfg(not(unix))]
    {
        // Windows has no linkable engine dylib at all (see
        // sdk/rust-dylib/Cargo.toml), so no process qualifies.
        None
    }
}

/// Call one build-id probe and copy the string out.
#[cfg(unix)]
unsafe fn read_probe(probe: &ProbeFn) -> Option<String> {
    // SAFETY: the caller established the symbol contract (NUL-terminated
    // static, valid for the process lifetime).
    unsafe {
        let p = probe();
        if p.is_null() {
            return None;
        }
        Some(CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}

/// Whether two paths name one file. Canonicalized, so `./lib.so` and
/// `lib.so` are the same entry; a path that cannot be canonicalized (it was
/// opened, so this is rare) compares as itself.
fn same_file(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(a) == real(b)
}

/// Resolve one dependency to the library file to open. `Err` is the reason,
/// listing every probed path.
fn resolve(dir: &Path, dep: &DepCfg, resolved: &ResolvedModules) -> Result<PathBuf, String> {
    let mut probed: Vec<PathBuf> = Vec::new();
    match &dep.source {
        ModuleSource::Path(p) => {
            let base = {
                let pp = Path::new(p);
                if pp.is_absolute() {
                    pp.to_path_buf()
                } else {
                    dir.join(pp)
                }
            };
            if base.extension().is_some() {
                if base.is_file() {
                    return Ok(base);
                }
                probed.push(base);
            } else {
                let stem = base
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let parent = base.parent().unwrap_or(Path::new(".")).to_path_buf();
                for f in library_spellings(&stem) {
                    probed.push(parent.join(f));
                }
            }
            push_module_dirs(dir, &dep.name, &mut probed);
        }
        ModuleSource::Bundled => {
            // Beside the running engine: the executable's directory, then
            // $LUMEN_LIB_DIR - the same order the liblumen loader probes -
            // then the `modules/` directories a packaged app stages into.
            if let Ok(exe) = std::env::current_exe()
                && let Some(exe_dir) = exe.parent()
            {
                for f in library_spellings(&dep.name) {
                    probed.push(exe_dir.join(f));
                }
            }
            if let Some(lib_dir) = std::env::var_os("LUMEN_LIB_DIR") {
                let lib_dir = PathBuf::from(lib_dir);
                for f in library_spellings(&dep.name) {
                    probed.push(lib_dir.join(f));
                }
            }
            push_module_dirs(dir, &dep.name, &mut probed);
        }
        ModuleSource::Version(v) => {
            // The runtime never resolves a version. The compiler does, and
            // hands its answer in; failing that, the probe only finds a
            // module a build step already put in a modules directory.
            match resolved.0.get(&dep.name) {
                Some(Ok(path)) => {
                    if path.is_file() {
                        return Ok(path.clone());
                    }
                    return Err(format!(
                        "the resolved copy of version \"{v}\" is gone: {}",
                        path.display()
                    ));
                }
                Some(Err(reason)) => return Err(reason.clone()),
                None => {}
            }
            push_module_dirs(dir, &dep.name, &mut probed);
            if let Some(hit) = probed.iter().find(|c| c.is_file()) {
                return Ok(hit.clone());
            }
            return Err(format!(
                "version \"{v}\" is not resolved at runtime; run through `lumenc run` or ship \
                 with `lumenc package` - the runtime does not resolve versions.\nProbed:\n{}",
                list(&probed)
            ));
        }
    }
    if let Some(hit) = probed.iter().find(|c| c.is_file()) {
        return Ok(hit.clone());
    }
    Err(format!(
        "no module library found.\nProbed:\n{}",
        list(&probed)
    ))
}

/// The `modules/` directories a module may ship in: the app's own, then the
/// running executable's.
fn push_module_dirs(dir: &Path, name: &str, probed: &mut Vec<PathBuf>) {
    for f in library_spellings(name) {
        probed.push(dir.join("modules").join(f));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        for f in library_spellings(name) {
            probed.push(exe_dir.join("modules").join(f));
        }
    }
}

fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| format!("  {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The unmissable stderr banner, in the visual style of the script
/// load-failure one: a module failing to load must never read as "the app
/// silently lost a feature".
fn banner(name: &str, reason: &str) {
    eprintln!(
        "\n\
         ================================================================\n\
         lumen-runtime: MODULE LOAD FAILED: {name}\n\
         \n\
         {reason}\n\
         \n\
         The app keeps running without this module.\n\
         ================================================================\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_static_test_binary_has_no_dynamic_engine() {
        // This test binary compiles its crates in statically, which is
        // exactly the process shape the engine-locked arm must refuse: no
        // `lumen_engine_build_id` in the dynamic symbol table, and no
        // `liblumen_engine` mapped for RTLD_NOLOAD to find.
        assert_eq!(engine_build_id(), None);
    }
}
