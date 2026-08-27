//! SDK for Lumen runtime plugins.
//!
//! A runtime plugin is a Rust cdylib the engine loads when an app starts. It
//! registers native functions the app's scripts call, ships language source
//! that wraps them, and pushes events at the app from its own threads.
//! Authors implement [`RuntimePlugin`] and export it with [`lumen_plugin!`];
//! no unsafe code is involved on the author side.
//!
//! The plugin and the engine exchange bytes over a C ABI ([`abi`]), so a
//! plugin works with the prebuilt engine as long as both were built from the
//! same release tag (enforced by a version handshake at load).
//!
//! # A plugin is not in the engine's address space
//!
//! The cdylib links its own copy of every Lumen crate it names. The types
//! re-exported below are data: they encode, cross the boundary, and decode
//! into the engine's own copy of the same type. Anything that is not data
//! does not cross. A `static` inside the plugin is a different static from
//! the engine's, a resource the plugin inserts into a world of its own is not
//! the app's world, and a handle it holds means nothing on the other side.
//!
//! Every effect on the app therefore goes one of two ways: a
//! [`ScriptCommand`] emitted from a call through [`Cx::emit`], or an event
//! pushed at any time through [`Host`]. That is the whole surface, and it is
//! why this crate re-exports data types only and never a whole engine crate.

pub mod abi;
#[doc(hidden)]
pub mod export;
#[cfg(feature = "host")]
mod host;
#[cfg(feature = "testing")]
pub mod testing;
pub mod wire;

use serde::{Deserialize, Serialize};

use crate::abi::{HostVtable, LogLevel};

#[cfg(feature = "host")]
pub use host::{FailureReason, HostHooks, InitEnv, LoadFailure, PluginSet, ResolvedModule};
pub use wire::{Call, CallOut, FnDecl, InitCx, Manifest, PluginEvent};

// The wire codec is shared with the compiler plugin system; it lives in
// `lumen-plugin-abi` and is re-exported here so a plugin crate names only
// this one.
pub use lumen_plugin_abi::codec;
/// The script surface a plugin describes its functions in and answers calls
/// with. Data types, from the engine's own crate, so a value a plugin builds
/// decodes into the one the script layer routes.
pub use lumen_script::{
    HostSet, SCRIPT_WIRE_VERSION, ScriptCommand, ScriptNs, ScriptParam, ScriptPrelude, ScriptSig,
    ScriptTy, ScriptValue,
};

/// What a runtime plugin does.
///
/// One instance serves the whole process and [`register`](Self::register)
/// takes `&self`, so a plugin holding mutable state brings its own lock (the
/// `Send + Sync` bound is what makes that explicit).
pub trait RuntimePlugin: Send + Sync + 'static {
    /// Declare what the plugin offers. Runs once, before the app's scripts
    /// load; failing here fails the module's load and the app carries on
    /// without it.
    fn register(&self, r: &mut Registrar, cx: &InitCx) -> Result<(), Error>;

    /// Release what the plugin owns. Runs once, when the app is going down.
    /// The default does nothing.
    fn shutdown(&self) {
        // Most plugins own nothing that outlives the process.
    }
}

/// What a plugin registers into, handed to
/// [`RuntimePlugin::register`](RuntimePlugin::register).
pub struct Registrar {
    fns: Vec<PluginFn>,
    preludes: Vec<ScriptPrelude>,
    host: Host,
}

impl Default for Registrar {
    fn default() -> Self {
        Self {
            fns: Vec::new(),
            preludes: Vec::new(),
            host: Host::disconnected(),
        }
    }
}

impl Registrar {
    /// Registration with a host attached, which is what the engine hands a
    /// plugin.
    pub(crate) fn new(host: Host) -> Self {
        Self {
            host,
            ..Self::default()
        }
    }

    /// The host, for a plugin that starts a thread while it registers.
    ///
    /// A plugin that watches a file, polls a device, or waits on a socket
    /// starts doing so here, and the handle it needs to deliver what it
    /// finds is this one. It stays valid for the process.
    pub fn host(&self) -> Host {
        self.host
    }

    /// Offer a function. Registration order is the order the app's script
    /// hosts bind it in, and a later function of the same namespace and name
    /// shadows an earlier one.
    pub fn script_fn(&mut self, f: PluginFn) -> &mut Self {
        self.fns.push(f);
        self
    }

    /// Ship source in `lang` that the host of that language compiles ahead of
    /// the app's own program, wrapping the functions registered under `ns`.
    ///
    /// This is how a plugin offers sugar over what it registered: a struct
    /// and an `impl` block in that language, so a script calls
    /// `Gpio::read(pin)` rather than the free function.
    pub fn prelude(&mut self, lang: &str, ns: &str, source: &str) -> &mut Self {
        self.preludes.push(ScriptPrelude {
            lang: lang.to_string(),
            ns: ns.to_string(),
            source: source.to_string(),
        });
        self
    }

    /// The manifest describing what was registered.
    fn manifest(&self) -> Manifest {
        Manifest {
            fns: self.fns.iter().map(PluginFn::decl).collect(),
            preludes: self.preludes.clone(),
            capabilities: Vec::new(),
        }
    }

    /// Take the bodies, leaving the registrar empty.
    fn take_fns(&mut self) -> Vec<PluginFn> {
        std::mem::take(&mut self.fns)
    }
}

/// The body of a [`PluginFn`]: the arguments and the command sink go in, a
/// value or the message the script raises comes out.
pub type PluginFnBody = Box<dyn Fn(&mut Cx<'_>) -> Result<ScriptValue, String> + Send + Sync>;

/// One function a plugin offers to the app's scripts.
///
/// Describes the same thing `lumen_script::ScriptFn` does in-process: a name,
/// a namespace, a signature, the languages that may see it, and a body.
pub struct PluginFn {
    name: String,
    ns: ScriptNs,
    sig: ScriptSig,
    hosts: HostSet,
    body: PluginFnBody,
}

impl PluginFn {
    /// Start a description. Finish it with
    /// [`PluginFnBuilder::build`](PluginFnBuilder::build).
    ///
    /// ```
    /// use lumen_plugin::{PluginFn, ScriptTy, ScriptValue};
    ///
    /// let f = PluginFn::new("gpio_read")
    ///     .param("pin", ScriptTy::Int)
    ///     .ret(ScriptTy::Bool)
    ///     .doc("Read a GPIO pin.")
    ///     .build(|cx| Ok(ScriptValue::Bool(cx.int_arg(0) % 2 == 0)));
    /// ```
    // A `PluginFn` is not complete until it has a body, so the entry point
    // hands back the builder that collects one.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(name: impl Into<String>) -> PluginFnBuilder {
        PluginFnBuilder {
            name: name.into(),
            ns: ScriptNs::Extension,
            sig: ScriptSig::default(),
            hosts: HostSet::ALL,
        }
    }

    /// The name the script calls it by.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run the body over one call.
    pub(crate) fn invoke(&self, cx: &mut Cx<'_>) -> Result<ScriptValue, String> {
        (self.body)(cx)
    }

    /// How this function appears in the manifest.
    fn decl(&self) -> FnDecl {
        FnDecl {
            name: self.name.clone(),
            ns: self.ns.clone(),
            sig: self.sig.clone(),
            hosts: self.hosts,
        }
    }
}

/// Builder for a [`PluginFn`]. Start it with [`PluginFn::new`].
pub struct PluginFnBuilder {
    name: String,
    ns: ScriptNs,
    sig: ScriptSig,
    hosts: HostSet,
}

impl PluginFnBuilder {
    /// Append a required parameter. Call in argument order.
    #[must_use]
    pub fn param(mut self, name: impl Into<String>, ty: ScriptTy) -> Self {
        self.sig.params.push(ScriptParam {
            name: name.into(),
            ty,
        });
        self.sig.min_arity = self.sig.params.len();
        self
    }

    /// Set the return type. Defaults to [`ScriptTy::Any`].
    #[must_use]
    pub fn ret(mut self, ty: ScriptTy) -> Self {
        self.sig.ret = ty;
        self
    }

    /// Set the one-line description editor tooling shows.
    #[must_use]
    pub fn doc(mut self, doc: impl Into<String>) -> Self {
        self.sig.doc = doc.into();
        self
    }

    /// Choose the namespace. Defaults to [`ScriptNs::Extension`].
    /// [`ScriptNs::Builtin`] is the runtime's own surface and a host refuses
    /// a manifest that claims it.
    #[must_use]
    pub fn ns(mut self, ns: ScriptNs) -> Self {
        self.ns = ns;
        self
    }

    /// Choose the languages. Defaults to every host; the empty set is
    /// refused at load, since a function no language sees is a mistake
    /// rather than a choice.
    #[must_use]
    pub fn hosts(mut self, hosts: HostSet) -> Self {
        self.hosts = hosts;
        self
    }

    /// Make the trailing parameters optional: a call may pass as few as
    /// `min_arity` arguments, and the body reads the rest as
    /// [`ScriptValue::Unit`].
    #[must_use]
    pub fn min_arity(mut self, min_arity: usize) -> Self {
        self.sig.min_arity = min_arity;
        self
    }

    /// Accept arguments past the declared parameters.
    #[must_use]
    pub fn variadic(mut self) -> Self {
        self.sig.variadic = true;
        self
    }

    /// Finish with the body.
    ///
    /// The body returns `Ok(value)`, or `Err(message)` to raise `message` in
    /// the script that called it.
    pub fn build<F>(self, body: F) -> PluginFn
    where
        F: Fn(&mut Cx<'_>) -> Result<ScriptValue, String> + Send + Sync + 'static,
    {
        PluginFn {
            name: self.name,
            ns: self.ns,
            sig: self.sig,
            hosts: self.hosts,
            body: Box::new(body),
        }
    }
}

/// The call a [`PluginFn`] body receives: its arguments, the sink it emits
/// commands into, and the host it can reach.
///
/// Mirrors `lumen_script::ScriptFnCx`, which is what a body would receive if
/// the same function were registered in-process.
pub struct Cx<'a> {
    args: &'a [ScriptValue],
    out: &'a mut Vec<ScriptCommand>,
    host: Host,
}

impl<'a> Cx<'a> {
    /// Wrap the arguments of one call, the scratch its commands land in, and
    /// the host it may call back into.
    pub fn new(args: &'a [ScriptValue], out: &'a mut Vec<ScriptCommand>, host: Host) -> Self {
        Self { args, out, host }
    }

    /// Every argument the script passed.
    pub fn args(&self) -> &[ScriptValue] {
        self.args
    }

    /// Argument `i`, or [`ScriptValue::Unit`] when the call passed fewer.
    pub fn arg(&self, i: usize) -> ScriptValue {
        self.arg_ref(i).clone()
    }

    /// Argument `i` by reference, for a body that only reads it.
    pub fn arg_ref(&self, i: usize) -> &ScriptValue {
        self.args.get(i).unwrap_or(&ScriptValue::Unit)
    }

    /// Argument `i` as a string. Non-strings take their canonical rendering;
    /// a missing argument is the empty string.
    pub fn str_arg(&self, i: usize) -> String {
        self.args
            .get(i)
            .map(ScriptValue::stringify)
            .unwrap_or_default()
    }

    /// Argument `i` as an integer. Floats truncate, numeric strings parse,
    /// and anything else (including a missing argument) is `0`.
    pub fn int_arg(&self, i: usize) -> i64 {
        match self.args.get(i) {
            Some(ScriptValue::I64(v)) => *v,
            Some(ScriptValue::F64(v)) => *v as i64,
            Some(ScriptValue::Bool(b)) => i64::from(*b),
            Some(ScriptValue::Str(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    /// Argument `i` as a float. Integers widen, numeric strings parse, and
    /// anything else (including a missing argument) is `0.0`.
    pub fn float_arg(&self, i: usize) -> f64 {
        match self.args.get(i) {
            Some(ScriptValue::F64(v)) => *v,
            Some(ScriptValue::I64(v)) => *v as f64,
            Some(ScriptValue::Str(s)) => s.trim().parse().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// Argument `i` as a boolean. `0` and `""` are false, other numbers and
    /// non-empty strings other than `"false"` are true, and a missing
    /// argument is false.
    pub fn bool_arg(&self, i: usize) -> bool {
        match self.args.get(i) {
            Some(ScriptValue::Bool(b)) => *b,
            Some(ScriptValue::I64(v)) => *v != 0,
            Some(ScriptValue::F64(v)) => *v != 0.0,
            Some(ScriptValue::Str(s)) => !s.is_empty() && s != "false" && s != "0",
            _ => false,
        }
    }

    /// Queue a command. The engine applies it on the tick the call happened,
    /// even when the call goes on to fail.
    pub fn emit(&mut self, cmd: ScriptCommand) {
        self.out.push(cmd);
    }

    /// The host, for the effects that outlive the call.
    pub fn host(&self) -> &Host {
        &self.host
    }
}

/// The engine, as a plugin can reach it.
///
/// Cheap to clone and usable from any thread, so a plugin keeps one in a
/// worker and pushes at the app whenever it has something. Nothing here
/// blocks on the tick thread: a call queues its payload and returns.
#[derive(Clone, Copy)]
pub struct Host {
    inner: &'static HostInner,
}

/// The vtable entries, copied out of the host's table at init.
pub(crate) struct HostInner {
    ctx: *mut std::ffi::c_void,
    emit_event: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize) -> i32>,
    log: Option<unsafe extern "C" fn(*mut std::ffi::c_void, i32, *const u8, usize)>,
    wake: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
}

// The context pointer is opaque host state that lives for the process, and
// every entry above is documented as callable from any thread.
unsafe impl Send for HostInner {}
unsafe impl Sync for HostInner {}

impl HostInner {
    /// A host that offers nothing. What a plugin sees when the loader passed
    /// no table at all, which no Lumen release does.
    pub(crate) const fn disconnected() -> Self {
        Self {
            ctx: std::ptr::null_mut(),
            emit_event: None,
            log: None,
            wake: None,
        }
    }

    /// Copy the entries out of a host's table, taking only the fields its
    /// `struct_size` says are there.
    ///
    /// # Safety
    /// `table`, when non-null, points at a `HostVtable` whose leading `u32`
    /// is the size of the struct the host built.
    pub(crate) unsafe fn from_vtable(table: *const HostVtable) -> Self {
        if table.is_null() {
            return Self::disconnected();
        }
        let size = unsafe { table.cast::<u32>().read() } as usize;
        /// Read one field, when the host's `struct_size` covers it.
        ///
        /// # Safety
        /// `offset` is that field's offset in `HostVtable`, so the read is
        /// aligned and in bounds once the size check passes.
        unsafe fn field<T: Copy>(
            table: *const HostVtable,
            size: usize,
            offset: usize,
        ) -> Option<T> {
            (offset + std::mem::size_of::<T>() <= size)
                .then(|| unsafe { table.cast::<u8>().add(offset).cast::<T>().read() })
        }
        unsafe {
            Self {
                ctx: field(table, size, std::mem::offset_of!(HostVtable, ctx))
                    .unwrap_or(std::ptr::null_mut()),
                emit_event: field(table, size, std::mem::offset_of!(HostVtable, emit_event))
                    .flatten(),
                log: field(table, size, std::mem::offset_of!(HostVtable, log)).flatten(),
                wake: field(table, size, std::mem::offset_of!(HostVtable, wake)).flatten(),
            }
        }
    }
}

impl Host {
    /// Wrap the entries the loader handed this plugin.
    pub(crate) fn new(inner: &'static HostInner) -> Self {
        Self { inner }
    }

    /// A host that takes nothing: what a plugin holds when it was built
    /// outside a load, and what every call reports failure through.
    pub(crate) fn disconnected() -> Self {
        static DISCONNECTED: HostInner = HostInner::disconnected();
        Self::new(&DISCONNECTED)
    }

    /// Call a handler in the app's script.
    ///
    /// `event` is the handler name, `key` the identifier it receives so one
    /// handler can serve several sources, and `fallback` a handler to try
    /// when the app defines no `event` (empty for none).
    ///
    /// Returns false when the engine is no longer taking events, which is
    /// what a plugin's worker thread sees while the app shuts down.
    pub fn call_handler(
        &self,
        event: &str,
        key: &str,
        fallback: &str,
        args: Vec<ScriptValue>,
    ) -> bool {
        self.send(PluginEvent::Call {
            event: event.to_string(),
            key: key.to_string(),
            fallback: fallback.to_string(),
            args,
        })
    }

    /// Apply commands, as if a call had emitted them.
    ///
    /// Returns false when the engine is no longer taking events.
    pub fn emit(&self, commands: Vec<ScriptCommand>) -> bool {
        self.send(PluginEvent::Commands(commands))
    }

    /// Write a line to the engine's diagnostic output.
    pub fn log(&self, level: LogLevel, message: &str) {
        let Some(log) = self.inner.log else {
            return;
        };
        unsafe {
            log(
                self.inner.ctx,
                level.into(),
                message.as_ptr(),
                message.len(),
            )
        };
    }

    /// Encode an event, hand it over, and ask for a tick to drain it. The
    /// wake is not the caller's to remember: an event delivered to a
    /// sleeping app that is never woken arrives whenever the next input
    /// does, which reads as a plugin that sometimes works.
    fn send(&self, event: PluginEvent) -> bool {
        let (Some(emit), Ok(bytes)) = (self.inner.emit_event, codec::encode(&event)) else {
            return false;
        };
        let status = unsafe { emit(self.inner.ctx, bytes.as_ptr(), bytes.len()) };
        if status != abi::OK {
            return false;
        }
        if let Some(wake) = self.inner.wake {
            unsafe { wake(self.inner.ctx) };
        }
        true
    }
}

/// A failure inside a plugin. The message reaches the user prefixed with the
/// module's name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    /// What went wrong.
    pub message: String,
}

// Blanket over Display so `?` works on any error in a plugin body. This
// compiles only while `Error` itself implements neither Display nor
// std::error::Error; adding either collides with core's reflexive
// `From<T> for T`. If that trade ever needs reversing, replace this with
// explicit From impls for the common error types.
impl<E: std::fmt::Display> From<E> for Error {
    fn from(e: E) -> Self {
        Error {
            message: e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host::disconnected()
    }

    #[test]
    fn a_builder_describes_what_it_was_told_and_defaults_the_rest() {
        let f = PluginFn::new("gpio_read")
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Bool)
            .doc("Read a GPIO pin.")
            .ns(ScriptNs::Named("gpio".to_string()))
            .hosts(HostSet::RHAI | HostSet::LUA)
            .min_arity(0)
            .variadic()
            .build(|cx| Ok(ScriptValue::Bool(cx.int_arg(0) > 0)));

        let decl = f.decl();
        assert_eq!(decl.name, "gpio_read");
        assert_eq!(decl.ns, ScriptNs::Named("gpio".to_string()));
        assert_eq!(decl.sig.params[0].ty, ScriptTy::Int);
        assert_eq!(decl.sig.ret, ScriptTy::Bool);
        assert_eq!(decl.sig.doc, "Read a GPIO pin.");
        assert_eq!(decl.sig.min_arity, 0);
        assert!(decl.sig.variadic);
        assert!(!decl.hosts.contains(HostSet::CANDELA));

        let default = PluginFn::new("plain")
            .build(|_| Ok(ScriptValue::Unit))
            .decl();
        assert_eq!(default.ns, ScriptNs::Extension);
        assert!(default.hosts.contains(HostSet::CANDELA));
    }

    #[test]
    fn the_registrar_keeps_registration_order_and_declares_no_capabilities() {
        let mut r = Registrar::default();
        r.script_fn(PluginFn::new("a").build(|_| Ok(ScriptValue::Unit)))
            .script_fn(PluginFn::new("b").build(|_| Ok(ScriptValue::Unit)))
            .prelude("candela", "gpio", "fn wrap() {}");

        let manifest = r.manifest();
        let names: Vec<&str> = manifest.fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(manifest.preludes[0].lang, "candela");
        assert_eq!(manifest.preludes[0].ns, "gpio");
        assert!(manifest.capabilities.is_empty());
        assert_eq!(r.take_fns().len(), 2);
        assert!(r.manifest().fns.is_empty());
    }

    #[test]
    fn the_call_context_coerces_its_arguments() {
        let mut out = Vec::new();
        let args = [
            ScriptValue::Str("7".into()),
            ScriptValue::F64(2.5),
            ScriptValue::Bool(true),
        ];
        let mut cx = Cx::new(&args, &mut out, host());
        assert_eq!(cx.int_arg(0), 7);
        assert_eq!(cx.float_arg(1), 2.5);
        assert_eq!(cx.int_arg(1), 2);
        assert!(cx.bool_arg(2));
        assert_eq!(cx.str_arg(1), "2.5");
        assert_eq!(cx.arg(9), ScriptValue::Unit);
        assert_eq!(cx.args().len(), 3);
        assert!(!cx.bool_arg(9));
        assert_eq!(cx.int_arg(9), 0);
        assert_eq!(cx.float_arg(9), 0.0);
        assert_eq!(cx.str_arg(9), "");

        cx.emit(ScriptCommand::Print("hi".into()));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_disconnected_host_reports_that_nothing_was_delivered() {
        let host = host();
        assert!(!host.call_handler("on_tick", "k", "", Vec::new()));
        assert!(!host.emit(vec![ScriptCommand::Print("x".into())]));
        // Nothing to log through, and saying so must not be a panic.
        host.log(LogLevel::Info, "ignored");
    }

    #[test]
    fn a_short_host_table_yields_only_the_fields_it_covers() {
        unsafe extern "C" fn emit(_ctx: *mut std::ffi::c_void, _p: *const u8, _l: usize) -> i32 {
            abi::OK
        }
        let table = HostVtable {
            // Everything past `emit_event` is what a host built against an
            // earlier ABI would not have.
            struct_size: (std::mem::offset_of!(HostVtable, emit_event)
                + std::mem::size_of::<usize>()) as u32,
            _pad: 0,
            ctx: std::ptr::null_mut(),
            emit_event: Some(emit),
            log: None,
            wake: None,
        };
        let inner = unsafe { HostInner::from_vtable(&table) };
        assert!(inner.emit_event.is_some());
        assert!(inner.log.is_none() && inner.wake.is_none());

        let none = unsafe { HostInner::from_vtable(std::ptr::null()) };
        assert!(none.emit_event.is_none());
    }

    #[test]
    fn any_display_type_converts_into_error() {
        assert_eq!(Error::from("plain").message, "plain");
        assert_eq!(
            Error::from(std::fmt::Error).message,
            std::fmt::Error.to_string()
        );
    }
}
