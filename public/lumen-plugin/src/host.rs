//! The loader side: dlopen plugin cdylibs, verify their descriptors, bring
//! them up, and bind what they registered onto an app. Compiled only under
//! the `host` feature; the runtime is the consumer, never plugin authors.
//!
//! A module that fails to load is collected rather than fatal. One broken
//! plugin should not take the app down with it, so [`PluginSet::load`] hands
//! back everything that came up and a [`LoadFailure`] for everything that did
//! not.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::Library;
use lumen_core::app::App;
use lumen_plugin_abi::dlopen::{self, CallError, HookOut, PrefixError};
use lumen_script::{ScriptFn, ScriptFnAppExt, ScriptFnCx, ScriptNs, ScriptPrelude, ScriptValue};

use crate::abi::{self, Buf, Desc, HostVtable, LogLevel};
use crate::wire::{Call, CallOut, FnDecl, InitCx, Manifest, PluginEvent};
use crate::{SCRIPT_WIRE_VERSION, codec};

/// One module to load: a name, a library file that already exists, and the
/// configuration the app declared for it.
///
/// Resolution happened before this: where the file came from (a path in the
/// app, the plugin cache, a bundle) is the caller's business, and the loader
/// only opens what it is given.
#[derive(Debug, Clone)]
pub struct ResolvedModule {
    /// The name the app declared. The library must report the same one.
    pub name: String,
    /// The library file to open.
    pub path: PathBuf,
    /// The module's own configuration, passed through untouched.
    pub config: toml::Table,
}

/// What every module in a set is told about the app.
#[derive(Debug, Clone)]
pub struct InitEnv {
    /// The app directory.
    pub app_dir: PathBuf,
    /// The app's id.
    pub app_id: String,
    /// Whether the app runs without a window.
    pub headless: bool,
    /// Whether the app was started with hot reload on.
    pub hot_reload: bool,
}

/// What the engine provides to the plugins it loads.
///
/// Every method is called from whichever thread the plugin used, at any time
/// from the moment the module initializes until the process ends, so an
/// implementation is a queue push rather than a piece of work.
pub trait HostHooks: Send + Sync + 'static {
    /// Take an event a module pushed. Returns false when the engine is no
    /// longer accepting events, which is what a plugin's worker thread sees
    /// while the app shuts down.
    fn event(&self, module: &str, event: PluginEvent) -> bool;

    /// Take a diagnostic line from a module.
    fn log(&self, module: &str, level: LogLevel, message: &str);

    /// Ask for another tick, so a queued event is drained without waiting
    /// for input.
    fn wake(&self);
}

/// Why one module did not load.
#[derive(Debug, thiserror::Error)]
pub enum FailureReason {
    #[error("failed to open: {0}")]
    Open(String),
    #[error("exports no lumen_plugin_v1 entry; is it a Lumen runtime plugin?")]
    MissingEntry,
    #[error(
        "exports lumenc_plugin_v1, not lumen_plugin_v1; this is a compiler plugin - declare it under [[plugins]]"
    )]
    CompilerPlugin,
    #[error("the entry returned a null descriptor")]
    NullDescriptor,
    #[error(
        "built for plugin ABI {got}, this runtime speaks {want}; rebuild the plugin against the matching Lumen tag"
    )]
    AbiMismatch { want: u32, got: u32 },
    #[error(
        "built against script wire version {got}, this runtime speaks {want}; rebuild the plugin against the matching Lumen tag"
    )]
    ScriptWireMismatch { want: u16, got: u16 },
    #[error("bad descriptor: {0}")]
    BadDescriptor(String),
    #[error(
        "the library reports itself as '{reported}'; fix the name it is declared under, or the path"
    )]
    NameMismatch { reported: String },
    #[error("already loaded as module '{0}'; declare one library once")]
    AlreadyLoaded(String),
    #[error("init failed: {0}")]
    Init(String),
    #[error("init panicked: {0}")]
    InitPanicked(String),
    #[error("bad manifest: {0}")]
    BadManifest(String),
}

/// One module that did not load, named so a banner can say which and why.
#[derive(Debug, thiserror::Error)]
#[error("plugin '{module}' ({}): {reason}", path.display())]
pub struct LoadFailure {
    /// The name the app declared the module under.
    pub module: String,
    /// The library file the loader probed.
    pub path: PathBuf,
    /// What went wrong.
    pub reason: FailureReason,
}

/// The engine state one module's callbacks land in. Leaked at load: a plugin
/// may hold its [`Host`](crate::Host) on a worker thread that outlives every
/// borrow the engine could hand out, and the library it lives in is never
/// unloaded either.
struct HostCtx {
    module: String,
    hooks: Arc<dyn HostHooks>,
}

/// One loaded module: the open library, the descriptor inside it, and what
/// its init registered.
struct Loaded {
    name: String,
    path: PathBuf,
    fns: Vec<FnDecl>,
    preludes: Vec<ScriptPrelude>,
    /// Kept open for the process lifetime; `desc` points into it.
    _lib: Library,
    desc: *const Desc,
}

// `desc` points at a static inside the library, which stays open as long as
// `_lib` lives in the same struct; `Desc` itself is Sync.
unsafe impl Send for Loaded {}
unsafe impl Sync for Loaded {}

impl Loaded {
    fn desc(&self) -> &Desc {
        unsafe { &*self.desc }
    }

    /// Run one of this module's functions: encode the call, drive the hook,
    /// apply what came back.
    fn call(&self, index: u32, cx: &mut ScriptFnCx<'_>) -> Result<ScriptValue, String> {
        let label = format!(
            "{}/{}",
            self.name,
            self.fns
                .get(index as usize)
                .map(|d| d.name.as_str())
                .unwrap_or("?")
        );
        let desc = self.desc();
        // Both were required at load, so a module in a set has them.
        let (hook, free) = (
            desc.call.expect("verified at load"),
            desc.free.expect("verified at load"),
        );
        let call = Call {
            index,
            args: cx.args().to_vec(),
        };
        let bytes = codec::encode(&call).map_err(|e| format!("{label}: {e}"))?;
        match unsafe { dlopen::call_hook(hook, free, &bytes, &[]) } {
            Ok(HookOut::Bytes(out)) => {
                let out: CallOut = codec::decode(&out)
                    .map_err(|e| format!("{label}: returned undecodable data: {e}"))?;
                // Emitted before the result is read, so a call that failed
                // part way still applies what it managed, the same as an
                // in-process script function.
                for command in out.commands {
                    cx.emit(command);
                }
                out.ret
            }
            Ok(HookOut::Unchanged) => Err(format!(
                "{label}: answered UNCHANGED, which is not a call outcome"
            )),
            Err(CallError::Failed(message)) => Err(format!("{label}: {message}")),
            Err(CallError::Panicked(message)) => Err(format!("{label}: panicked: {message}")),
            Err(CallError::UnknownStatus(status)) => {
                Err(format!("{label}: returned unknown status {status}"))
            }
        }
    }
}

/// The loaded modules of one app, in declaration order.
///
/// Libraries are dlopen'd once and held for the process lifetime: a plugin
/// hands out function bodies, threads, and a host handle, none of which
/// survive the code they live in being unmapped.
pub struct PluginSet {
    modules: Vec<Arc<Loaded>>,
}

impl PluginSet {
    /// Load every module, in order. Each one that fails is collected with
    /// its name, the file that was probed, and the reason; the rest load.
    pub fn load(
        modules: &[ResolvedModule],
        env: &InitEnv,
        hooks: Arc<dyn HostHooks>,
    ) -> (Self, Vec<LoadFailure>) {
        let mut loaded: Vec<Arc<Loaded>> = Vec::with_capacity(modules.len());
        let mut failures = Vec::new();
        for module in modules {
            match load_one(module, env, &hooks, &loaded) {
                Ok(one) => loaded.push(Arc::new(one)),
                Err(reason) => failures.push(LoadFailure {
                    module: module.name.clone(),
                    path: module.path.clone(),
                    reason,
                }),
            }
        }
        (PluginSet { modules: loaded }, failures)
    }

    /// Bind every loaded module's functions and language sources onto the
    /// app, in declaration order.
    ///
    /// The bodies call back into the plugin, so this runs before the script
    /// hosts load and the app keeps the set alive for as long as it runs.
    pub fn install(&self, app: &mut App) {
        for module in &self.modules {
            for (index, decl) in module.fns.iter().enumerate() {
                let target = Arc::clone(module);
                let index = index as u32;
                app.add_script_fn(ScriptFn {
                    name: decl.name.clone(),
                    ns: decl.ns.clone(),
                    sig: decl.sig.clone(),
                    hosts: decl.hosts,
                    body: Arc::new(move |cx: &mut ScriptFnCx<'_>| target.call(index, cx)),
                });
            }
            for prelude in &module.preludes {
                app.add_script_prelude(&prelude.lang, &prelude.ns, &prelude.source);
            }
        }
    }

    /// Tell every module the app is going down, in declaration order. Best
    /// effort: a module that exports no shutdown is skipped, and the
    /// libraries stay mapped either way.
    pub fn shutdown(&self) {
        for module in &self.modules {
            if let Some(shutdown) = module.desc().shutdown {
                unsafe { shutdown() };
            }
        }
    }

    /// The names of the loaded modules, in declaration order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.modules.iter().map(|m| m.name.as_str())
    }

    /// How many modules loaded.
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Whether nothing loaded.
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

/// Open one module, verify it, and bring it up.
fn load_one(
    module: &ResolvedModule,
    env: &InitEnv,
    hooks: &Arc<dyn HostHooks>,
    already: &[Arc<Loaded>],
) -> Result<Loaded, FailureReason> {
    // Two entries pointing at one file would share the library's statics,
    // and a runtime plugin holds one instance per process; the second entry
    // would run against the first one's configuration.
    if let Some(prior) = already.iter().find(|l| same_file(&l.path, &module.path)) {
        return Err(FailureReason::AlreadyLoaded(prior.name.clone()));
    }
    let lib = dlopen::open_library(&module.path).map_err(FailureReason::Open)?;
    let desc = match unsafe { dlopen::entry_descriptor(&lib, abi::ENTRY) } {
        Some(desc) => desc,
        // A compiler plugin is a cdylib too, and pointing the runtime at one
        // is a mistake worth naming instead of calling it "not a plugin".
        None if unsafe { dlopen::entry_descriptor(&lib, b"lumenc_plugin_v1\0") }.is_some() => {
            return Err(FailureReason::CompilerPlugin);
        }
        None => return Err(FailureReason::MissingEntry),
    };
    if desc.is_null() {
        return Err(FailureReason::NullDescriptor);
    }
    let desc = desc.cast::<Desc>();
    verify(&module.name, desc)?;
    let manifest = init(&module.name, desc, module, env, hooks)?;
    validate(&manifest).map_err(FailureReason::BadManifest)?;
    if !manifest.fns.is_empty() && unsafe { &*desc }.call.is_none() {
        return Err(FailureReason::BadDescriptor(format!(
            "registered {} function(s) but exports no call entry",
            manifest.fns.len()
        )));
    }
    Ok(Loaded {
        name: module.name.clone(),
        path: module.path.clone(),
        fns: manifest.fns,
        preludes: manifest.preludes,
        _lib: lib,
        desc,
    })
}

/// Whether two paths name one file. Canonicalized, so `./lib.so` and
/// `lib.so` are the same entry; a path that cannot be canonicalized (it was
/// opened, so this is rare) compares as itself.
fn same_file(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(a) == real(b)
}

/// The handshake: refuse anything about the descriptor that would make the
/// byte payloads or the pointers untrustworthy. The first two fields sit at
/// frozen offsets and are read through the raw pointer before a `&Desc` for
/// the whole struct exists, so a truncated or foreign descriptor is refused
/// without ever forming a reference past its end.
///
/// # Safety (internal)
/// `desc` is non-null and points at at least 8 readable bytes; the caller
/// checked null, and any exporter of the entry symbol provides at least the
/// frozen prefix.
fn verify(declared: &str, desc: *const Desc) -> Result<(), FailureReason> {
    unsafe { dlopen::verify_prefix(desc.cast(), abi::ABI_VERSION, std::mem::size_of::<Desc>()) }
        .map_err(|e| match e {
            PrefixError::AbiMismatch { want, got } => FailureReason::AbiMismatch { want, got },
            short => FailureReason::BadDescriptor(short.to_string()),
        })?;
    let desc: &Desc = unsafe { &*desc };
    if desc.flags & abi::FLAG_PANIC_ABORT != 0 {
        return Err(FailureReason::BadDescriptor(
            "built with panic = \"abort\"; the panic-to-error contract needs unwinding, rebuild \
             with the default panic = \"unwind\""
                .to_string(),
        ));
    }
    if desc.script_wire_version != SCRIPT_WIRE_VERSION {
        return Err(FailureReason::ScriptWireMismatch {
            want: SCRIPT_WIRE_VERSION,
            got: desc.script_wire_version,
        });
    }
    let c_str = |ptr: *const std::os::raw::c_char, what: &str| {
        unsafe { dlopen::c_string(ptr, what) }.map_err(FailureReason::BadDescriptor)
    };
    let reported = c_str(desc.name, "name")?;
    c_str(desc.version, "version")?;
    if reported.is_empty() {
        return Err(FailureReason::BadDescriptor("empty name".to_string()));
    }
    if reported != declared {
        return Err(FailureReason::NameMismatch { reported });
    }
    if desc.init.is_none() {
        return Err(FailureReason::BadDescriptor("no init function".to_string()));
    }
    if desc.free.is_none() {
        return Err(FailureReason::BadDescriptor("no free function".to_string()));
    }
    Ok(())
}

/// Bring one verified module up and take back its manifest.
fn init(
    name: &str,
    desc: *const Desc,
    module: &ResolvedModule,
    env: &InitEnv,
    hooks: &Arc<dyn HostHooks>,
) -> Result<Manifest, FailureReason> {
    let desc: &Desc = unsafe { &*desc };
    // A `toml::Table` always re-serializes: keys are strings and the
    // serializer orders values ahead of tables itself.
    let config_toml = toml::to_string(&module.config).expect("a toml::Table always re-serializes");
    let cx = InitCx::new(
        env.app_dir.clone(),
        env.app_id.clone(),
        env.headless,
        env.hot_reload,
        env!("CARGO_PKG_VERSION").to_string(),
        config_toml,
    );
    let bytes = codec::encode(&cx).expect("InitCx always encodes");
    let table = vtable(name, Arc::clone(hooks));
    let (init, free) = (
        desc.init.expect("verified at load"),
        desc.free.expect("verified at load"),
    );
    match unsafe { call_init(init, free, &bytes, table) } {
        Ok(HookOut::Bytes(out)) => {
            codec::decode(&out).map_err(|e| FailureReason::BadManifest(format!("undecodable: {e}")))
        }
        Ok(HookOut::Unchanged) => Err(FailureReason::Init(
            "answered UNCHANGED, which is not an init outcome".to_string(),
        )),
        Err(CallError::Failed(message)) => Err(FailureReason::Init(message)),
        Err(CallError::Panicked(message)) => Err(FailureReason::InitPanicked(message)),
        Err(CallError::UnknownStatus(status)) => Err(FailureReason::Init(format!(
            "returned unknown status {status}"
        ))),
    }
}

/// One init call. The same buffer-and-status dance as
/// [`dlopen::call_hook`], which init cannot use: it takes the host table
/// where a hook takes a second byte slice.
///
/// # Safety
/// `init` and `free` come from the same verified descriptor, so the buffer
/// init allocates is the one `free` knows how to release.
unsafe fn call_init(
    init: abi::InitFn,
    free: abi::FreeFn,
    ctx: &[u8],
    host: *const HostVtable,
) -> Result<HookOut, CallError> {
    let mut out = Buf::empty();
    let status = unsafe { init(ctx.as_ptr(), ctx.len(), host, &mut out) };
    let bytes = if out.ptr.is_null() {
        Vec::new()
    } else {
        let copied = unsafe { std::slice::from_raw_parts(out.ptr, out.len) }.to_vec();
        unsafe { free(out.ptr, out.len, out.cap) };
        copied
    };
    match status {
        abi::OK => Ok(HookOut::Bytes(bytes)),
        abi::UNCHANGED => Ok(HookOut::Unchanged),
        abi::ERR => Err(CallError::Failed(
            String::from_utf8_lossy(&bytes).into_owned(),
        )),
        abi::PANICKED => Err(CallError::Panicked(
            String::from_utf8_lossy(&bytes).into_owned(),
        )),
        other => Err(CallError::UnknownStatus(other)),
    }
}

/// What a manifest may not say. Everything here is a plugin bug rather than
/// a user's, so the message is written for whoever wrote the plugin.
fn validate(manifest: &Manifest) -> Result<(), String> {
    if !manifest.capabilities.is_empty() {
        return Err(format!(
            "declares capabilities ({}); they are reserved and must be empty",
            manifest.capabilities.join(", ")
        ));
    }
    let mut seen: Vec<(&ScriptNs, &str)> = Vec::with_capacity(manifest.fns.len());
    for decl in &manifest.fns {
        if decl.name.trim().is_empty() {
            return Err("a function has an empty name".to_string());
        }
        if decl.ns == ScriptNs::Builtin {
            return Err(format!(
                "function '{}' claims the builtin namespace, which is the runtime's own",
                decl.name
            ));
        }
        if decl.hosts.is_empty() {
            return Err(format!(
                "function '{}' is visible to no language",
                decl.name
            ));
        }
        if seen.contains(&(&decl.ns, decl.name.as_str())) {
            return Err(format!("declares '{}' twice", decl.name));
        }
        seen.push((&decl.ns, decl.name.as_str()));
    }
    Ok(())
}

/// Build one module's host table. Leaked: see [`HostCtx`].
fn vtable(module: &str, hooks: Arc<dyn HostHooks>) -> &'static HostVtable {
    let ctx: &'static HostCtx = Box::leak(Box::new(HostCtx {
        module: module.to_string(),
        hooks,
    }));
    Box::leak(Box::new(HostVtable {
        struct_size: std::mem::size_of::<HostVtable>() as u32,
        _pad: 0,
        ctx: std::ptr::from_ref(ctx).cast_mut().cast::<c_void>(),
        emit_event: Some(emit_event),
        log: Some(log),
        wake: Some(wake),
    }))
}

/// Borrow the context one of the entries below was called with.
///
/// # Safety
/// `ctx` is the pointer [`vtable`] leaked, which lives for the process.
unsafe fn host_ctx<'a>(ctx: *mut c_void) -> Option<&'a HostCtx> {
    (!ctx.is_null()).then(|| unsafe { &*ctx.cast::<HostCtx>() })
}

/// Take one event. Called from whichever thread the plugin used, so the
/// panic is caught here: unwinding out of an `extern "C"` function aborts,
/// and the thread it would abort is the plugin's.
unsafe extern "C" fn emit_event(ctx: *mut c_void, ptr: *const u8, len: usize) -> i32 {
    let Some(host) = (unsafe { host_ctx(ctx) }) else {
        return abi::ERR;
    };
    if ptr.is_null() && len > 0 {
        return abi::ERR;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let taken = catch_unwind(AssertUnwindSafe(|| match codec::decode(bytes) {
        Ok(event) => host.hooks.event(&host.module, event),
        Err(_) => false,
    }))
    .unwrap_or(false);
    if taken { abi::OK } else { abi::ERR }
}

/// Take one diagnostic line. Same threading and panic contract as
/// [`emit_event`].
unsafe extern "C" fn log(ctx: *mut c_void, level: i32, ptr: *const u8, len: usize) {
    let Some(host) = (unsafe { host_ctx(ctx) }) else {
        return;
    };
    if ptr.is_null() && len > 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let message = String::from_utf8_lossy(bytes);
    let _ = catch_unwind(AssertUnwindSafe(|| {
        host.hooks
            .log(&host.module, LogLevel::from(level), &message)
    }));
}

/// Ask for a tick. Same threading and panic contract as [`emit_event`].
unsafe extern "C" fn wake(ctx: *mut c_void) {
    let Some(host) = (unsafe { host_ctx(ctx) }) else {
        return;
    };
    let _ = catch_unwind(AssertUnwindSafe(|| host.hooks.wake()));
}

#[cfg(test)]
mod tests {
    use lumen_plugin_abi::raw::{fill, free_buf};
    use lumen_script::{HostSet, ScriptCommand, ScriptSig};

    use super::*;
    use crate::abi::ABI_VERSION;

    fn good_desc() -> Desc {
        Desc {
            abi_version: ABI_VERSION,
            struct_size: std::mem::size_of::<Desc>() as u32,
            script_wire_version: SCRIPT_WIRE_VERSION,
            flags: 0,
            reserved: 0,
            name: c"demo".as_ptr(),
            version: c"0.1.0".as_ptr(),
            init: Some(init_stub),
            call: None,
            shutdown: None,
            free: Some(free_buf),
        }
    }

    unsafe extern "C" fn init_stub(
        _ctx: *const u8,
        _ctx_len: usize,
        _host: *const HostVtable,
        _out: *mut Buf,
    ) -> i32 {
        abi::OK
    }

    #[test]
    fn a_good_descriptor_verifies() {
        verify("demo", &good_desc() as *const Desc).unwrap();
    }

    #[test]
    fn a_foreign_abi_version_is_refused() {
        let mut d = good_desc();
        d.abi_version = ABI_VERSION + 1;
        let err = verify("demo", &d as *const Desc).unwrap_err();
        assert!(matches!(err, FailureReason::AbiMismatch { got, .. } if got == ABI_VERSION + 1));
        assert!(err.to_string().contains("matching Lumen tag"), "{err}");
    }

    #[test]
    fn a_truncated_descriptor_is_refused() {
        let mut d = good_desc();
        d.struct_size = 8;
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("descriptor is 8 bytes"), "{err}");
    }

    #[test]
    fn a_wire_version_mismatch_names_both_numbers() {
        let mut d = good_desc();
        d.script_wire_version = SCRIPT_WIRE_VERSION + 7;
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("script wire version"), "{err}");
        assert!(
            err.contains(&(SCRIPT_WIRE_VERSION + 7).to_string()),
            "{err}"
        );
        assert!(err.contains(&SCRIPT_WIRE_VERSION.to_string()), "{err}");
    }

    #[test]
    fn a_panic_abort_plugin_is_refused() {
        let mut d = good_desc();
        d.flags = abi::FLAG_PANIC_ABORT;
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("panic = \"abort\""), "{err}");
    }

    #[test]
    fn a_missing_entry_function_is_refused() {
        for (mutate, want) in [
            (
                (|d: &mut Desc| d.init = None) as fn(&mut Desc),
                "no init function",
            ),
            (|d: &mut Desc| d.free = None, "no free function"),
        ] {
            let mut d = good_desc();
            mutate(&mut d);
            let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
            assert!(err.contains(want), "{err}");
        }
    }

    #[test]
    fn a_name_that_is_missing_unreadable_or_someone_elses_is_refused() {
        let mut d = good_desc();
        d.name = std::ptr::null();
        assert!(
            verify("demo", &d as *const Desc)
                .unwrap_err()
                .to_string()
                .contains("null name")
        );

        let mut d = good_desc();
        d.name = c"\xff\xfe".as_ptr();
        assert!(
            verify("demo", &d as *const Desc)
                .unwrap_err()
                .to_string()
                .contains("not UTF-8")
        );

        let mut d = good_desc();
        d.name = c"".as_ptr();
        assert!(
            verify("", &d as *const Desc)
                .unwrap_err()
                .to_string()
                .contains("empty name")
        );

        let err = verify("other", &good_desc() as *const Desc).unwrap_err();
        assert!(matches!(&err, FailureReason::NameMismatch { reported } if reported == "demo"));
    }

    fn decl(name: &str) -> FnDecl {
        FnDecl {
            name: name.to_string(),
            ns: ScriptNs::Extension,
            sig: ScriptSig::default(),
            hosts: HostSet::ALL,
        }
    }

    #[test]
    fn a_manifest_declaring_nothing_wrong_validates() {
        validate(&Manifest {
            fns: vec![decl("a"), decl("b")],
            preludes: vec![ScriptPrelude {
                lang: "candela".to_string(),
                ns: "demo".to_string(),
                source: String::new(),
            }],
            capabilities: Vec::new(),
        })
        .unwrap();
    }

    #[test]
    fn a_manifest_is_refused_for_what_it_may_not_claim() {
        let with = |fns: Vec<FnDecl>, capabilities: Vec<String>| Manifest {
            fns,
            preludes: Vec::new(),
            capabilities,
        };
        let err = validate(&with(Vec::new(), vec!["fs".to_string()])).unwrap_err();
        assert!(err.contains("reserved"), "{err}");

        let err = validate(&with(vec![decl("  ")], Vec::new())).unwrap_err();
        assert!(err.contains("empty name"), "{err}");

        let mut builtin = decl("print");
        builtin.ns = ScriptNs::Builtin;
        let err = validate(&with(vec![builtin], Vec::new())).unwrap_err();
        assert!(err.contains("builtin namespace"), "{err}");

        let mut nobody = decl("hidden");
        nobody.hosts = HostSet::from_lang("prolog");
        let err = validate(&with(vec![nobody], Vec::new())).unwrap_err();
        assert!(err.contains("no language"), "{err}");

        let err = validate(&with(vec![decl("twice"), decl("twice")], Vec::new())).unwrap_err();
        assert!(err.contains("declares 'twice' twice"), "{err}");

        // The same name in two namespaces is two functions, not a clash.
        let mut named = decl("read");
        named.ns = ScriptNs::Named("gpio".to_string());
        validate(&with(vec![decl("read"), named], Vec::new())).unwrap();
    }

    // -- hand-built descriptor harness ------------------------------------
    //
    // A `Loaded` over the test binary's own handle (`Library::this`) and a
    // static descriptor, so a call can be driven against hook behaviors no
    // well-formed plugin produces: absent payloads, garbage payloads,
    // unknown status codes.

    #[cfg(unix)]
    fn loaded_with(desc: &'static Desc) -> Loaded {
        Loaded {
            name: "harness".to_string(),
            path: PathBuf::from("/harness"),
            fns: vec![decl("f")],
            preludes: Vec::new(),
            _lib: Library::from(libloading::os::unix::Library::this()),
            desc,
        }
    }

    #[cfg(unix)]
    fn call_harness(desc: &'static Desc) -> Result<ScriptValue, String> {
        let loaded = loaded_with(desc);
        let args = [ScriptValue::I64(1)];
        let mut out = Vec::new();
        let mut cx = ScriptFnCx::new(&args, &mut out);
        loaded.call(0, &mut cx)
    }

    unsafe extern "C" fn leak_free(_ptr: *mut u8, _len: usize, _cap: usize) {}

    unsafe extern "C" fn unchanged_hook(
        _input: *const u8,
        _input_len: usize,
        _ctx: *const u8,
        _ctx_len: usize,
        _out: *mut Buf,
    ) -> i32 {
        abi::UNCHANGED
    }

    unsafe extern "C" fn weird_status_hook(
        _input: *const u8,
        _input_len: usize,
        _ctx: *const u8,
        _ctx_len: usize,
        _out: *mut Buf,
    ) -> i32 {
        99
    }

    unsafe extern "C" fn garbage_hook(
        _input: *const u8,
        _input_len: usize,
        _ctx: *const u8,
        _ctx_len: usize,
        out: *mut Buf,
    ) -> i32 {
        let bytes = std::mem::ManuallyDrop::new(vec![0xffu8, 0xfe, 0x00, 0x9d]);
        unsafe {
            (*out).ptr = bytes.as_ptr() as *mut u8;
            (*out).len = bytes.len();
            (*out).cap = bytes.capacity();
        }
        abi::OK
    }

    #[cfg(unix)]
    fn harness_desc(hook: abi::HookFn) -> Desc {
        Desc {
            call: Some(hook),
            free: Some(leak_free),
            ..good_desc()
        }
    }

    #[cfg(unix)]
    #[test]
    fn hook_answers_no_call_produces_are_refused_by_name() {
        for (hook, want) in [
            (unchanged_hook as abi::HookFn, "answered UNCHANGED"),
            (weird_status_hook, "unknown status 99"),
            (garbage_hook, "undecodable data"),
        ] {
            let desc: &'static Desc = Box::leak(Box::new(harness_desc(hook)));
            let err = call_harness(desc).unwrap_err();
            assert!(err.contains(want), "{err}");
            assert!(err.starts_with("harness/f: "), "{err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_out_of_range_index_names_the_module_without_a_function() {
        let desc: &'static Desc = Box::leak(Box::new(harness_desc(unchanged_hook)));
        let loaded = loaded_with(desc);
        let mut out = Vec::new();
        let mut cx = ScriptFnCx::new(&[], &mut out);
        let err = loaded.call(9, &mut cx).unwrap_err();
        assert!(err.starts_with("harness/?: "), "{err}");
    }

    /// A host that ignores what it is handed. The index test below only
    /// needs a module to exist.
    #[cfg(feature = "testing")]
    struct Silent;

    #[cfg(feature = "testing")]
    impl HostHooks for Silent {
        fn event(&self, _module: &str, _event: PluginEvent) -> bool {
            true
        }

        fn log(&self, _module: &str, _level: LogLevel, _message: &str) {}

        fn wake(&self) {}
    }

    /// The other half of the out-of-range arm: a real plugin refusing an
    /// index it registered no function for. Nothing the loader does can
    /// produce that call, so it takes a loaded module driven by hand.
    #[cfg(feature = "testing")]
    #[test]
    fn a_plugin_refuses_an_index_it_registered_nothing_for() {
        let app_dir =
            std::env::temp_dir().join(format!("lumen-plugin-host-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&app_dir);
        std::fs::create_dir_all(&app_dir).unwrap();
        let (set, failures) = PluginSet::load(
            &[ResolvedModule {
                name: "lumen-plugin-fixture".to_string(),
                path: crate::testing::fixture_copy("host-index"),
                config: toml::Table::new(),
            }],
            &InitEnv {
                app_dir,
                app_id: "host-index".to_string(),
                headless: true,
                hot_reload: false,
            },
            Arc::new(Silent),
        );
        assert!(failures.is_empty(), "{failures:?}");
        let mut out = Vec::new();
        let mut cx = ScriptFnCx::new(&[], &mut out);
        let err = set.modules[0].call(99, &mut cx).unwrap_err();
        assert!(err.contains("no function at index 99"), "{err}");
    }

    /// Answers with a `CallOut` carrying one command and a failure, so the
    /// "emitted before the result is read" contract is checked without the
    /// fixture.
    unsafe extern "C" fn commands_then_fail_hook(
        _input: *const u8,
        _input_len: usize,
        _ctx: *const u8,
        _ctx_len: usize,
        out: *mut Buf,
    ) -> i32 {
        let bytes = codec::encode(&CallOut {
            ret: Err("no".to_string()),
            commands: vec![ScriptCommand::Print("before".into())],
        })
        .unwrap();
        fill(unsafe { &mut *out }, bytes);
        abi::OK
    }

    #[cfg(unix)]
    #[test]
    fn what_a_failing_call_emitted_is_still_applied() {
        static DESC: std::sync::OnceLock<Desc> = std::sync::OnceLock::new();
        let desc = DESC.get_or_init(|| Desc {
            call: Some(commands_then_fail_hook),
            free: Some(free_buf),
            ..good_desc()
        });
        let loaded = loaded_with(desc);
        let mut out = Vec::new();
        let mut cx = ScriptFnCx::new(&[], &mut out);
        assert_eq!(loaded.call(0, &mut cx), Err("no".to_string()));
        assert_eq!(out.len(), 1);
    }
}
