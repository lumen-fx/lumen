//! The loading half: for each declared dependency, find what answers to that
//! name, tell which runtime kind it is, verify it, and bring it into the app.
//!
//! A name is answered from the registry first. A module compiled into the
//! running binary has no file to open, so its constructor put it on
//! [`lumen_module_registry`] before `main`; a name found there installs
//! straight away, on every platform, and nothing is resolved or opened.
//!
//! Otherwise the name resolves to a library file, and the file itself says
//! which kind it is:
//!
//! - A library exporting `lumen_module_probe_<name>` is an **engine-locked
//!   runtime module** (`lumen-module`): a Rust dylib sharing the running
//!   engine, with full ECS reach. It loads only into a process that links
//!   the engine dynamically, and only when its build id equals the engine's
//!   exactly. The `<name>` half is the declared name, so two modules linked
//!   into one binary never define the same entry.
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
use lumen_module_registry::StaticModule;
use lumen_plugin::abi::LogLevel;
use lumen_plugin::{HostHooks, PluginEvent, PluginSet, codec};

use crate::{
    DepCfg, DependenciesCfg, INSTALL_PREFIX, ModuleSource, PROBE_PREFIX, ResolvedModules,
    entry_symbol, library_spellings,
};

pub use lumen_plugin::InitEnv;

/// The C-ABI entry a portable plugin exports. One name for every plugin: a
/// portable plugin is opened, never linked in, so two of them never share a
/// symbol table.
pub const PLUGIN_ENTRY_SYMBOL: &str = "lumen_plugin_v1";
/// The entry a compiler plugin exports - the wrong kind to declare here.
const COMPILER_ENTRY_SYMBOL: &str = "lumenc_plugin_v1";

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

/// Every module compiled into this binary, whatever put it there.
///
/// A thin wrapper so the one call the loader makes into the registry reads
/// as an arm of the search rather than as a dependency on any module.
fn registered(name: &str) -> Option<StaticModule> {
    lumen_module_registry::registered()
        .into_iter()
        .find(|m| m.name == name)
}

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
    /// A runtime module compiled into this binary, found on the registry its
    /// pre-main constructor put it on. No file was opened.
    Static,
    /// An engine-locked runtime module (`lumen_module_probe_<name>`).
    EngineModule,
    /// A portable C-ABI plugin (`lumen_plugin_v1`).
    PortablePlugin,
}

/// One installed dependency.
#[derive(Debug)]
pub struct LoadedModule {
    /// The declared name.
    pub name: String,
    /// The library file that was opened, or the running executable for a
    /// module that was compiled into it.
    pub path: PathBuf,
    /// Which kind the search found.
    pub kind: LoadedKind,
    /// The build id an engine-locked module reported; equal to the engine's
    /// own. Empty for the other two kinds: a portable plugin's handshake is
    /// the ABI and wire versions instead, and a compiled-in module is this
    /// build.
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
/// failure gets the unmissable banner; a name that no shape of this build
/// can answer gets one line, because that is a property of how the binary
/// was put together rather than a defect in the app, and shouting MODULE
/// LOAD FAILED on every launch would teach users to ignore the banner.
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
                        "lumen-runtime: dependency '{}' skipped: this build does not compile \
                         the module in and no engine dylib is present to load it dynamically. \
                         The app runs without it. Package it with `lumenc package --static` to \
                         compile the declared modules into the executable.",
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

/// Find, verify, and install one dependency. `Err` carries the reason and
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
    // The registry first, before anything is resolved or opened: a module
    // compiled into this binary is already here, and a file of the same name
    // beside the executable would be a second copy of it.
    if let Some(module) = registered(&dep.name) {
        return install_static_module(app, dep, module);
    }
    // A `bundled` dependency the registry did not answer, in a host that
    // compiled the engine in, is the same build-shape condition the
    // engine-locked refusal below notices quietly: the module is neither in
    // this binary nor loadable beside a shared engine this process does not
    // have. Every other resolution failure banners.
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

    // Kind dispatch: the exported entry says what the file is. The
    // engine-locked entries carry the declared name, so what is probed for
    // depends on what the app called this dependency.
    let has = |symbol: &str| {
        // SAFETY: presence probe only; the symbol is never called through
        // this lookup.
        unsafe { lib.get::<ProbeFn>(symbol) }.is_ok()
    };
    let probe_symbol = entry_symbol(PROBE_PREFIX, &dep.name);
    if has(&probe_symbol) {
        return install_engine_module(app, dep, path, lib, engine_id, &probe_symbol);
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
        "{} exports neither {probe_symbol} nor lumen_plugin_v1; the library is not a Lumen \
         runtime module built as '{}', or a portable plugin",
        path.display(),
        dep.name
    )))
}

/// The compiled-in arm: hand the registered entry the app and the config
/// table, exactly as the opened arm hands them to the entry it looked up.
///
/// No build-id check, and none possible: the module was compiled into this
/// binary, so it is this build by construction and there is no second engine
/// instance for it to run against.
fn install_static_module(
    app: &mut App,
    dep: &DepCfg,
    module: StaticModule,
) -> Result<LoadedModule, LoadError> {
    let config_toml = config_toml(dep)?;
    let status = guarded_install(|| (module.install)(app, &config_toml));
    read_install_status(status)?;
    Ok(LoadedModule {
        name: dep.name.clone(),
        path: std::env::current_exe().unwrap_or_else(|_| PathBuf::from(dep.name.clone())),
        kind: LoadedKind::Static,
        build_id: String::new(),
    })
}

/// The engine-locked arm: verify the build id against the running engine and
/// call the Rust-ABI install entry.
fn install_engine_module(
    app: &mut App,
    dep: &DepCfg,
    path: PathBuf,
    lib: libloading::Library,
    engine_id: Option<&str>,
    probe_symbol: &str,
) -> Result<LoadedModule, LoadError> {
    // Hazard check: a host that compiled the engine in must never install an
    // engine-locked module (a second engine instance shares no worlds or
    // statics with the first, and the probe cannot tell). A portable plugin
    // took the other arm before this check on purpose, and a module compiled
    // into this binary answered from the registry before either. A notice
    // rather than the banner: the refusal is a property of the build shape.
    let Some(engine_id) = engine_id else {
        return Err(LoadError::notice(
            "this build does not compile the module in and no engine dylib is present to load \
             it dynamically.\nThe binary compiles the engine into itself, and a module opened \
             here would run against a second engine instance.\nA portable plugin \
             (lumen-plugin) loads here; an engine-locked module (lumen-module) has to be \
             compiled in.",
        ));
    };
    // SAFETY: the probe is a C-ABI symbol returning a NUL-terminated static;
    // it is the one symbol safe to call across a build-skewed boundary.
    let module_id = unsafe {
        let probe: libloading::Symbol<ProbeFn> = lib
            .get(probe_symbol)
            .expect("presence was probed before dispatch");
        let p = probe();
        if p.is_null() {
            return Err(LoadError::banner(format!(
                "{}: {probe_symbol} returned null",
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

    let config_toml = config_toml(dep)?;
    // SAFETY: exact build-id equality just proved the module and the running
    // engine are one build, which is the contract that makes the Rust-ABI
    // signature (`&mut App`, `&str`) sound to call.
    let install_symbol = entry_symbol(INSTALL_PREFIX, &dep.name);
    let install: libloading::Symbol<InstallFn> = unsafe { lib.get(install_symbol.as_str()) }
        .map_err(|_| {
            LoadError::banner(format!(
                "{} does not export {install_symbol}",
                path.display()
            ))
        })?;
    // SAFETY: as above - the probe already proved one build.
    let status = guarded_install(|| unsafe { install(app, &config_toml) });
    read_install_status(status)?;

    // Load-forever: see [`LoadedModules`] for why unloading is never sound.
    std::mem::forget(lib);
    Ok(LoadedModule {
        name: dep.name.clone(),
        path,
        kind: LoadedKind::EngineModule,
        build_id: module_id,
    })
}

/// The module's `config` table in the wire form every install entry reads.
/// One copy for both module arms, so a module cannot see a different table
/// depending on how it reached the app.
fn config_toml(dep: &DepCfg) -> Result<String, LoadError> {
    toml::to_string(&dep.config).map_err(|e| {
        LoadError::banner(format!(
            "could not serialize the module's config table: {e}"
        ))
    })
}

/// Call an install entry and turn an escaping panic into a status.
///
/// The module's own macro catches construction panics; this catch is the belt
/// for a panic escaping it anyway (one libstd either way, so the unwind
/// crosses cleanly). The payload is printed here rather than left to the
/// panic hook: an app's own hook may be silent, and the banner promises the
/// message was printed.
fn guarded_install(call: impl FnOnce() -> u32) -> u32 {
    catch_unwind(AssertUnwindSafe(call)).unwrap_or_else(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("(non-string panic payload)");
        eprintln!("lumen-runtime: the module's install entry panicked: {msg}");
        INSTALL_PANICKED
    })
}

/// What an install status means, in the words the banner carries. Shared by
/// both module arms: the codes are one set, so the messages are too.
fn read_install_status(status: u32) -> Result<(), LoadError> {
    match status {
        INSTALL_OK => Ok(()),
        INSTALL_PANICKED => Err(LoadError::banner(
            "the module's constructor panicked during install (its message is printed above)",
        )),
        INSTALL_BAD_CONFIG => Err(LoadError::banner(
            "the module rejected its `config` table (its message is printed above)",
        )),
        other => Err(LoadError::banner(format!(
            "the module's install entry failed (code {other})"
        ))),
    }
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
        // public/lumen-dylib/Cargo.toml), so no process qualifies.
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
    fn an_entry_symbol_carries_the_declared_name() {
        assert_eq!(
            entry_symbol(PROBE_PREFIX, "lumen-audio"),
            "lumen_module_probe_lumen_audio"
        );
        assert_eq!(
            entry_symbol(INSTALL_PREFIX, "fixture"),
            "lumen_module_install_fixture"
        );
        // Anything a symbol cannot carry becomes an underscore, so a name
        // the toml table accepts always maps to one that links.
        assert_eq!(
            entry_symbol(PROBE_PREFIX, "shape.tools+2"),
            "lumen_module_probe_shape_tools_2"
        );
    }

    #[test]
    fn a_static_test_binary_has_no_dynamic_engine() {
        // This test binary compiles its crates in statically, which is
        // exactly the process shape the engine-locked arm must refuse: no
        // `lumen_engine_build_id` in the dynamic symbol table, and no
        // `liblumen_engine` mapped for RTLD_NOLOAD to find.
        assert_eq!(engine_build_id(), None);
    }
}
