//! One description of a native function every script host can bind.
//!
//! A [`ScriptFn`] says what a function is called, what it takes, what it
//! returns, which namespace it lives in, and which languages may see it. Each
//! host implements [`ScriptHost::register_script_fn`](crate::ScriptHost::register_script_fn)
//! once and gets every function an app, a plugin, the C ABI, or the Rust SDK
//! describes.
//!
//! The registration channel is [`ScriptFnRegistry`], a resource on the main
//! world. A [`Plugin`](lumen_core::app::Plugin) pushes into it through
//! [`ScriptFnAppExt::add_script_fn`]; the generic script plugin drains it into
//! the host it is about to load, then seals it. This is Lumen's counterpart of
//! QML's `QQmlContext::setContextProperty`: one embedder-facing surface, seen
//! by every scripting context the app runs.

use std::fmt;
use std::sync::Arc;

use bevy_ecs::prelude::Resource;
use lumen_core::app::App;
use lumen_core::warn_line;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{ScriptCommand, ScriptValue};

/// The shape of one parameter or return value.
///
/// A host binds a typed parameter to its own type where it can (Rhai resolves a
/// call by argument type) and checks the argument where it cannot (Lua binds
/// variadically and raises on a mismatch). [`ScriptTy::Any`] accepts whatever
/// the script passes.
///
/// Variants are append-only; see [`SCRIPT_WIRE_VERSION`](crate::SCRIPT_WIRE_VERSION).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptTy {
    /// Any value; no check.
    Any,
    /// No value (`()` in Rhai, `nil` in Lua, `null` in candela).
    Unit,
    /// Boolean.
    Bool,
    /// Signed 64-bit integer.
    Int,
    /// 64-bit float.
    Float,
    /// String.
    Str,
    /// List whose elements carry the inner type.
    Array(Box<ScriptTy>),
    /// String-keyed map whose values carry the inner type.
    Map(Box<ScriptTy>),
}

impl ScriptTy {
    /// Whether `value` satisfies this type. [`ScriptTy::Any`] accepts anything;
    /// an [`Array`](ScriptTy::Array) or [`Map`](ScriptTy::Map) also checks its
    /// elements.
    ///
    /// An integer satisfies a declared float: every scripting language Lumen
    /// hosts spells `1` and `1.0` as the same literal kind, so `seek(30)`
    /// is the call an author writes. The reverse does not hold; a float where
    /// an integer is declared would silently drop its fraction.
    pub fn accepts(&self, value: &ScriptValue) -> bool {
        match (self, value) {
            (Self::Any, _) => true,
            (Self::Unit, ScriptValue::Unit) => true,
            (Self::Bool, ScriptValue::Bool(_)) => true,
            (Self::Int, ScriptValue::I64(_)) => true,
            (Self::Float, ScriptValue::F64(_) | ScriptValue::I64(_)) => true,
            (Self::Str, ScriptValue::Str(_)) => true,
            (Self::Array(inner), ScriptValue::Array(items)) => {
                items.iter().all(|v| inner.accepts(v))
            }
            (Self::Map(inner), ScriptValue::Map(entries)) => {
                entries.values().all(|v| inner.accepts(v))
            }
            _ => false,
        }
    }

    /// The name a host puts in a type-mismatch message.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Unit => "unit",
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::Str => "string",
            Self::Array(_) => "array",
            Self::Map(_) => "map",
        }
    }
}

/// The type name of a value, for the other half of a mismatch message.
fn value_ty_name(value: &ScriptValue) -> &'static str {
    match value {
        ScriptValue::Unit => "unit",
        ScriptValue::Bool(_) => "bool",
        ScriptValue::I64(_) => "int",
        ScriptValue::F64(_) => "float",
        ScriptValue::Str(_) => "string",
        ScriptValue::Array(_) => "array",
        ScriptValue::Map(_) => "map",
    }
}

/// One declared parameter: the name a doc or an editor shows, and its type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptParam {
    /// Parameter name, for docs and diagnostics.
    pub name: String,
    /// Declared type.
    pub ty: ScriptTy,
}

/// The declared signature of a [`ScriptFn`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptSig {
    /// Positional parameters, in order.
    pub params: Vec<ScriptParam>,
    /// Return type.
    pub ret: ScriptTy,
    /// Accepts more arguments than [`Self::params`] declares.
    pub variadic: bool,
    /// Smallest accepted argument count. Parameters past it are optional and
    /// arrive as [`ScriptValue::Unit`] when the call omits them.
    pub min_arity: usize,
    /// One-line description, surfaced by editor tooling.
    pub doc: String,
}

/// How many arguments a variadic signature is bound for on a host that
/// dispatches by arity. Rhai has no native variadics, so it takes one
/// registration per count up to this bound.
pub const MAX_VARIADIC_ARITY: usize = 8;

impl ScriptSig {
    /// The inclusive range of argument counts a host binds this signature for.
    pub fn arity_range(&self) -> std::ops::RangeInclusive<usize> {
        let max = if self.variadic {
            self.params.len().max(MAX_VARIADIC_ARITY)
        } else {
            self.params.len()
        };
        self.min_arity.min(max)..=max
    }

    /// Whether any parameter declares a type.
    ///
    /// An all-[`Any`](ScriptTy::Any) signature says nothing to check, so a host
    /// that binds variadically passes whatever the script sent straight
    /// through; that is what `lumen_app_expose` and the SDK's `native_fn`
    /// describe, and a call with the wrong count reaches the body rather than
    /// failing to resolve.
    pub fn is_typed(&self) -> bool {
        self.params.iter().any(|p| p.ty != ScriptTy::Any)
    }

    /// Check `args` against the declared parameters. Returns the message a host
    /// raises to the script on a mismatch.
    ///
    /// Too few arguments is a mismatch; extra arguments are one only when the
    /// signature is not variadic. A parameter past [`Self::min_arity`] accepts
    /// [`ScriptValue::Unit`] whatever its declared type, so a host that pads a
    /// short call with unit placeholders still passes.
    pub fn check_args(&self, args: &[ScriptValue]) -> Result<(), String> {
        if args.len() < self.min_arity {
            return Err(format!(
                "expected at least {} argument(s), got {}",
                self.min_arity,
                args.len()
            ));
        }
        if !self.variadic && args.len() > self.params.len() {
            return Err(format!(
                "expected at most {} argument(s), got {}",
                self.params.len(),
                args.len()
            ));
        }
        for (i, arg) in args.iter().enumerate() {
            let Some(param) = self.params.get(i) else {
                break;
            };
            let optional = i >= self.min_arity && matches!(arg, ScriptValue::Unit);
            if !optional && !param.ty.accepts(arg) {
                return Err(format!(
                    "argument {} (`{}`) expects {}, got {}",
                    i + 1,
                    param.name,
                    param.ty.name(),
                    value_ty_name(arg)
                ));
            }
        }
        Ok(())
    }
}

impl Default for ScriptSig {
    fn default() -> Self {
        Self {
            params: Vec::new(),
            ret: ScriptTy::Any,
            variadic: false,
            min_arity: 0,
            doc: String::new(),
        }
    }
}

/// Where a function lives in the script's name space.
///
/// Variants are append-only; see [`SCRIPT_WIRE_VERSION`](crate::SCRIPT_WIRE_VERSION).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptNs {
    /// The runtime's own surface. Global on Rhai and Lua; the `lumen` host
    /// namespace on candela.
    Builtin,
    /// An embedder's surface (the C ABI's `lumen_app_expose`, the Rust SDK, a
    /// plugin). Global on Rhai and Lua; the `native` host namespace on candela.
    Extension,
    /// A namespace of the embedder's choosing: a static module on Rhai, a
    /// global table on Lua, a host namespace on candela.
    Named(String),
}

/// Which languages may see a function.
///
/// Some functions exist for one host only. The runtime's own `page` family, for
/// instance, is declared in candela's prelude under the `lumen` namespace, so
/// registering it again would give candela a second spelling backed by a
/// different bus; it ships as `RHAI | LUA`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostSet(u8);

impl HostSet {
    /// The Rhai host.
    pub const RHAI: Self = Self(1 << 0);
    /// The Lua host.
    pub const LUA: Self = Self(1 << 1);
    /// Both candela hosts (compiler and artifact).
    pub const CANDELA: Self = Self(1 << 2);
    /// Every host.
    pub const ALL: Self = Self(0b111);

    /// Whether `other`'s languages are all in this set. The empty set is in
    /// none: a host that names a language Lumen does not know is not one of
    /// the languages any function was described for.
    pub fn contains(self, other: Self) -> bool {
        !other.is_empty() && self.0 & other.0 == other.0
    }

    /// Whether this set names no language.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The set for a [`ScriptHost::lang`](crate::ScriptHost::lang) tag. An
    /// unknown tag is the empty set, so a host Lumen does not ship sees nothing
    /// until it names itself.
    pub fn from_lang(lang: &str) -> Self {
        match lang {
            "rhai" => Self::RHAI,
            "lua" => Self::LUA,
            "candela" => Self::CANDELA,
            _ => Self(0),
        }
    }
}

impl Default for HostSet {
    fn default() -> Self {
        Self::ALL
    }
}

impl std::ops::BitOr for HostSet {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for HostSet {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// A set travels as its bits, so it stays one byte on the wire.
impl Serialize for HostSet {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

/// A bit no language claims is dropped rather than refused: a peer built
/// against a later Lumen may name a host this one does not ship, and the set
/// that reaches it here is the languages both sides know.
impl<'de> Deserialize<'de> for HostSet {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self(u8::deserialize(d)? & Self::ALL.0))
    }
}

/// The call a [`ScriptFn`] body receives: its arguments, and the sink it emits
/// commands into.
///
/// The command scratch is a plain `Vec`. A host moves what the body emitted
/// into its own sink after the body returns, so a body that calls back into the
/// host cannot deadlock against a lock the call is already holding.
pub struct ScriptFnCx<'a> {
    args: &'a [ScriptValue],
    out: &'a mut Vec<ScriptCommand>,
}

impl<'a> ScriptFnCx<'a> {
    /// Wrap the arguments of one call and the scratch its commands land in.
    pub fn new(args: &'a [ScriptValue], out: &'a mut Vec<ScriptCommand>) -> Self {
        Self { args, out }
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

    /// Argument `i` as a string. Non-strings take their canonical rendering; a
    /// missing argument is the empty string.
    pub fn str_arg(&self, i: usize) -> String {
        self.args
            .get(i)
            .map(ScriptValue::stringify)
            .unwrap_or_default()
    }

    /// Argument `i` as an integer. Floats truncate, numeric strings parse, and
    /// anything else (including a missing argument) is `0`.
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
    /// non-empty strings other than `"false"` are true, and a missing argument
    /// is false.
    pub fn bool_arg(&self, i: usize) -> bool {
        match self.args.get(i) {
            Some(ScriptValue::Bool(b)) => *b,
            Some(ScriptValue::I64(v)) => *v != 0,
            Some(ScriptValue::F64(v)) => *v != 0.0,
            Some(ScriptValue::Str(s)) => !s.is_empty() && s != "false" && s != "0",
            _ => false,
        }
    }

    /// Queue a command. The host forwards it to the ECS message bus on the tick
    /// the call happened.
    pub fn emit(&mut self, cmd: ScriptCommand) {
        self.out.push(cmd);
    }
}

/// The callable half of a [`ScriptFn`]. Shared so one description binds into
/// several hosts, `Send + Sync` so it runs on any of their threads.
///
/// A body that fails hands back the message the script sees. Each host raises
/// it the way its language raises: a Rhai runtime error, a Lua
/// `error(...)`, a candela `host_fn_error` the script can catch. The
/// message names the function, so a failure is attributable without a
/// stack trace.
pub type ScriptFnBody = Arc<dyn Fn(&mut ScriptFnCx<'_>) -> ScriptResult + Send + Sync>;

/// What a [`ScriptFn`] body hands back: a value, or the message to raise in
/// the script that called it.
pub type ScriptResult = Result<ScriptValue, String>;

/// One native function, in terms every script host understands.
#[derive(Clone)]
pub struct ScriptFn {
    /// The name the script calls it by.
    pub name: String,
    /// The namespace it lives in.
    pub ns: ScriptNs,
    /// Its declared signature.
    pub sig: ScriptSig,
    /// The languages that may see it.
    pub hosts: HostSet,
    /// The function body.
    pub body: ScriptFnBody,
}

impl ScriptFn {
    /// Start a typed description.
    ///
    /// ```
    /// use lumen_script::{ScriptFn, ScriptTy, ScriptValue};
    ///
    /// let f = ScriptFn::new("set_pin")
    ///     .param("pin", ScriptTy::Int)
    ///     .ret(ScriptTy::Bool)
    ///     .doc("Drive a GPIO pin high.")
    ///     .build(|cx| Ok(ScriptValue::Bool(cx.int_arg(0) > 0)));
    /// assert_eq!(f.sig.params.len(), 1);
    /// ```
    ///
    /// [`ScriptFn::from_fn`] takes a plain Rust closure instead and reads the
    /// signature off its types; the builder is for a function that also wants
    /// a doc line, optional arguments, a host set, or the command sink.
    // A `ScriptFn` is not complete until it has a body, so the entry point
    // hands back the builder that collects one.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(name: impl Into<String>) -> ScriptFnBuilder {
        ScriptFnBuilder {
            name: name.into(),
            ns: ScriptNs::Extension,
            sig: ScriptSig::default(),
            hosts: HostSet::ALL,
        }
    }

    /// Describe an untyped, value-returning function of `arity` arguments: the
    /// shape the C ABI's `lumen_app_expose` and the Rust SDK's `native_fn`
    /// produce. Lands in [`ScriptNs::Extension`].
    pub fn value<F>(name: impl Into<String>, arity: usize, f: F) -> Self
    where
        F: Fn(&[ScriptValue]) -> ScriptValue + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            ns: ScriptNs::Extension,
            sig: any_sig(arity),
            hosts: HostSet::ALL,
            body: Arc::new(move |cx: &mut ScriptFnCx<'_>| Ok(f(cx.args()))),
        }
    }

    /// Describe a function from a plain Rust closure, reading its signature off
    /// the argument and return types.
    ///
    /// This is the short form: no builder, no [`ScriptTy`] spelled out. Each
    /// parameter type maps onto the declared type a host binds it as, so
    /// `|pin: i64, high: bool|` declares `(int, bool)` and a call passing
    /// anything else fails at the call site. A closure returning
    /// `Result<T, String>` may fail, and the message reaches the script.
    ///
    /// Parameters are named `arg0`, `arg1`, ... unless
    /// [`param_names`](Self::param_names) renames them.
    ///
    /// ```
    /// use lumen_script::{ScriptFn, ScriptTy};
    ///
    /// let f = ScriptFn::from_fn("gpio_read", |pin: i64| -> Result<bool, String> {
    ///     if pin < 0 {
    ///         return Err(format!("pin {pin} is out of range"));
    ///     }
    ///     Ok(pin % 2 == 0)
    /// })
    /// .param_names(["pin"]);
    ///
    /// assert_eq!(f.sig.params[0].ty, ScriptTy::Int);
    /// assert_eq!(f.sig.ret, ScriptTy::Bool);
    /// ```
    pub fn from_fn<Marker, F>(name: impl Into<String>, f: F) -> Self
    where
        F: IntoScriptFn<Marker>,
    {
        let (params, ret, body) = f.into_script_fn_parts();
        let arity = params.len();
        Self {
            name: name.into(),
            ns: ScriptNs::Extension,
            sig: ScriptSig {
                params: params
                    .into_iter()
                    .enumerate()
                    .map(|(i, ty)| ScriptParam {
                        name: format!("arg{i}"),
                        ty,
                    })
                    .collect(),
                ret,
                variadic: false,
                min_arity: arity,
                doc: String::new(),
            },
            hosts: HostSet::ALL,
            body,
        }
    }

    /// Rename the parameters, in order. Names past the declared arity are
    /// ignored, and a parameter the list does not reach keeps `argN`.
    ///
    /// Only docs and diagnostics read these, so a function built with
    /// [`ScriptFn::from_fn`] works without them; they are what makes an
    /// editor's signature hint and a type-mismatch message readable.
    #[must_use]
    pub fn param_names<N: Into<String>>(mut self, names: impl IntoIterator<Item = N>) -> Self {
        for (param, name) in self.sig.params.iter_mut().zip(names) {
            param.name = name.into();
        }
        self
    }

    /// Describe an untyped function of `arity` arguments whose whole effect is
    /// the commands it emits. Returns [`ScriptValue::Unit`] to the script.
    pub fn commands<F>(name: impl Into<String>, arity: usize, f: F) -> Self
    where
        F: Fn(&mut ScriptFnCx<'_>) + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            ns: ScriptNs::Extension,
            sig: ScriptSig {
                ret: ScriptTy::Unit,
                ..any_sig(arity)
            },
            hosts: HostSet::ALL,
            body: Arc::new(move |cx: &mut ScriptFnCx<'_>| {
                f(cx);
                Ok(ScriptValue::Unit)
            }),
        }
    }

    /// Move it into another namespace.
    #[must_use]
    pub fn with_ns(mut self, ns: ScriptNs) -> Self {
        self.ns = ns;
        self
    }

    /// Restrict (or widen) the languages that see it.
    #[must_use]
    pub fn with_hosts(mut self, hosts: HostSet) -> Self {
        self.hosts = hosts;
        self
    }

    /// Accept fewer arguments than the signature declares; the trailing
    /// parameters become optional.
    #[must_use]
    pub fn with_min_arity(mut self, min_arity: usize) -> Self {
        self.sig.min_arity = min_arity;
        self
    }

    /// Whether `lang` may see this function.
    pub fn visible_to(&self, lang: &str) -> bool {
        self.hosts.contains(HostSet::from_lang(lang))
    }

    /// Run the body over `args`, appending whatever it emitted to `out`.
    ///
    /// Every host adapter goes through here, so a body observes the same
    /// argument slice and the same command scratch whichever language called
    /// it. `out` is the caller's buffer rather than a fresh one, so a host that
    /// reuses [`CallScratch`] pays no allocation per call.
    ///
    /// A body that fails still leaves what it emitted before failing: a host
    /// forwards those commands and then raises, which is what the runtime has
    /// always done with a partially-completed call.
    ///
    /// # Errors
    ///
    /// Whatever the body reported, verbatim.
    pub fn invoke_into(&self, args: &[ScriptValue], out: &mut Vec<ScriptCommand>) -> ScriptResult {
        let mut cx = ScriptFnCx::new(args, out);
        (self.body)(&mut cx)
    }

    /// Run the body over `args`, returning its result and whatever it emitted.
    ///
    /// The allocating convenience over [`Self::invoke_into`], for a caller with
    /// no scratch of its own.
    pub fn invoke(&self, args: &[ScriptValue]) -> (ScriptResult, Vec<ScriptCommand>) {
        let mut out = Vec::new();
        let ret = self.invoke_into(args, &mut out);
        (ret, out)
    }
}

/// The two buffers one call needs: the arguments going in, and the commands
/// coming out.
///
/// A host builds the argument list on every call, and most bodies queue a
/// command, so both were an allocation per script call. Borrowing them from
/// [`with_call_scratch`] instead keeps the capacity between calls.
#[derive(Default)]
pub struct CallScratch {
    /// The arguments, converted out of the engine's own value type.
    pub args: Vec<ScriptValue>,
    /// What the body emitted, to be moved into the host's sink.
    pub commands: Vec<ScriptCommand>,
}

impl CallScratch {
    /// Empty buffers, allocating nothing until something is pushed.
    const fn new() -> Self {
        Self {
            args: Vec::new(),
            commands: Vec::new(),
        }
    }
}

thread_local! {
    /// This thread's scratch buffers, kept between calls so their capacity is
    /// paid for once.
    static SCRATCH: std::cell::RefCell<CallScratch> =
        const { std::cell::RefCell::new(CallScratch::new()) };
}

/// Borrow this thread's cleared [`CallScratch`] for the length of one call.
///
/// The buffers keep their capacity between calls, so a host adapter that wraps
/// its call in this stops allocating once the app is warm. A body that calls
/// back into another function finds the shared buffers taken and gets fresh
/// ones, so a nested call cannot disturb the one it is inside.
pub fn with_call_scratch<R>(f: impl FnOnce(&mut CallScratch) -> R) -> R {
    SCRATCH.with(|cell| match cell.try_borrow_mut() {
        Ok(mut scratch) => {
            scratch.args.clear();
            scratch.commands.clear();
            f(&mut scratch)
        }
        Err(_) => f(&mut CallScratch::new()),
    })
}

/// A signature of `arity` untyped parameters, all required.
fn any_sig(arity: usize) -> ScriptSig {
    ScriptSig {
        params: (0..arity)
            .map(|i| ScriptParam {
                name: format!("arg{i}"),
                ty: ScriptTy::Any,
            })
            .collect(),
        min_arity: arity,
        ..ScriptSig::default()
    }
}

impl fmt::Debug for ScriptFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptFn")
            .field("name", &self.name)
            .field("ns", &self.ns)
            .field("sig", &self.sig)
            .field("hosts", &self.hosts)
            .finish_non_exhaustive()
    }
}

/// Builder for a typed [`ScriptFn`]. Start it with [`ScriptFn::new`].
pub struct ScriptFnBuilder {
    name: String,
    ns: ScriptNs,
    sig: ScriptSig,
    hosts: HostSet,
}

impl ScriptFnBuilder {
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
    #[must_use]
    pub fn ns(mut self, ns: ScriptNs) -> Self {
        self.ns = ns;
        self
    }

    /// Choose the languages. Defaults to every host.
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
    pub fn build<F>(self, body: F) -> ScriptFn
    where
        F: Fn(&mut ScriptFnCx<'_>) -> ScriptResult + Send + Sync + 'static,
    {
        ScriptFn {
            name: self.name,
            ns: self.ns,
            sig: self.sig,
            hosts: self.hosts,
            body: Arc::new(body),
        }
    }
}

/// A Rust type [`ScriptFn::from_fn`] can carry across the script boundary.
///
/// Implemented for the scalars every host shares (`i64`, `f64`, `bool`,
/// `String`), for `()`, and recursively for `Vec<T>` and
/// `HashMap<String, T>`. The conversion into Rust is the same coercion the
/// [`ScriptFnCx`] accessors apply, so an integer arrives where a float is
/// declared and a missing argument reads as the type's empty value.
pub trait ScriptType: Sized {
    /// The declared type a host binds this parameter or return slot as.
    fn script_ty() -> ScriptTy;

    /// Read one argument.
    fn from_script_value(value: &ScriptValue) -> Self;

    /// Hand one value back to the script.
    fn into_script_value(self) -> ScriptValue;
}

impl ScriptType for i64 {
    fn script_ty() -> ScriptTy {
        ScriptTy::Int
    }

    fn from_script_value(value: &ScriptValue) -> Self {
        match value {
            ScriptValue::I64(v) => *v,
            ScriptValue::F64(v) => *v as Self,
            ScriptValue::Bool(b) => Self::from(*b),
            ScriptValue::Str(s) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn into_script_value(self) -> ScriptValue {
        ScriptValue::I64(self)
    }
}

impl ScriptType for f64 {
    fn script_ty() -> ScriptTy {
        ScriptTy::Float
    }

    fn from_script_value(value: &ScriptValue) -> Self {
        match value {
            ScriptValue::F64(v) => *v,
            ScriptValue::I64(v) => *v as Self,
            ScriptValue::Str(s) => s.trim().parse().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    fn into_script_value(self) -> ScriptValue {
        ScriptValue::F64(self)
    }
}

impl ScriptType for bool {
    fn script_ty() -> ScriptTy {
        ScriptTy::Bool
    }

    fn from_script_value(value: &ScriptValue) -> Self {
        match value {
            ScriptValue::Bool(b) => *b,
            ScriptValue::I64(v) => *v != 0,
            ScriptValue::F64(v) => *v != 0.0,
            ScriptValue::Str(s) => !s.is_empty() && s != "false" && s != "0",
            _ => false,
        }
    }

    fn into_script_value(self) -> ScriptValue {
        ScriptValue::Bool(self)
    }
}

impl ScriptType for String {
    fn script_ty() -> ScriptTy {
        ScriptTy::Str
    }

    fn from_script_value(value: &ScriptValue) -> Self {
        value.stringify()
    }

    fn into_script_value(self) -> ScriptValue {
        ScriptValue::Str(self)
    }
}

impl ScriptType for () {
    fn script_ty() -> ScriptTy {
        ScriptTy::Unit
    }

    fn from_script_value(_value: &ScriptValue) -> Self {}

    fn into_script_value(self) -> ScriptValue {
        ScriptValue::Unit
    }
}

impl<T: ScriptType> ScriptType for Vec<T> {
    fn script_ty() -> ScriptTy {
        ScriptTy::Array(Box::new(T::script_ty()))
    }

    fn from_script_value(value: &ScriptValue) -> Self {
        match value {
            ScriptValue::Array(items) => items.iter().map(T::from_script_value).collect(),
            _ => Self::new(),
        }
    }

    fn into_script_value(self) -> ScriptValue {
        ScriptValue::Array(self.into_iter().map(T::into_script_value).collect())
    }
}

impl<T: ScriptType> ScriptType for std::collections::HashMap<String, T> {
    fn script_ty() -> ScriptTy {
        ScriptTy::Map(Box::new(T::script_ty()))
    }

    fn from_script_value(value: &ScriptValue) -> Self {
        match value {
            ScriptValue::Map(entries) => entries
                .iter()
                .map(|(k, v)| (k.clone(), T::from_script_value(v)))
                .collect(),
            _ => Self::new(),
        }
    }

    fn into_script_value(self) -> ScriptValue {
        ScriptValue::Map(
            self.into_iter()
                .map(|(k, v)| (k, T::into_script_value(v)))
                .collect(),
        )
    }
}

/// What a [`ScriptFn::from_fn`] closure may return: a value, or a
/// `Result` whose error message reaches the script.
///
/// The declared return type is `T` either way, so the same declaration binds a
/// closure that can fail and one that cannot.
pub trait ScriptRet {
    /// The declared return type.
    fn script_ty() -> ScriptTy;

    /// The result the body hands the host.
    fn into_script_result(self) -> ScriptResult;
}

impl<T: ScriptType> ScriptRet for T {
    fn script_ty() -> ScriptTy {
        <T as ScriptType>::script_ty()
    }

    fn into_script_result(self) -> ScriptResult {
        Ok(self.into_script_value())
    }
}

impl<T: ScriptType> ScriptRet for Result<T, String> {
    fn script_ty() -> ScriptTy {
        <T as ScriptType>::script_ty()
    }

    fn into_script_result(self) -> ScriptResult {
        self.map(ScriptType::into_script_value)
    }
}

/// Adapts a plain Rust closure into the body and signature of a [`ScriptFn`].
///
/// The `Marker` parameter disambiguates the per-arity impls, the same trick
/// Rhai and Bevy use so one entry point accepts closures of many shapes without
/// a type annotation. Arity runs to [`MAX_VARIADIC_ARITY`]; past that, describe
/// the function with [`ScriptFn::new`].
pub trait IntoScriptFn<Marker> {
    /// The declared parameter types, the declared return type, and the body.
    ///
    /// Driven by [`ScriptFn::from_fn`]; not meant to be called directly.
    fn into_script_fn_parts(self) -> (Vec<ScriptTy>, ScriptTy, ScriptFnBody);
}

/// Marker for a closure that takes no arguments.
pub struct Arity0;

impl<F, R> IntoScriptFn<Arity0> for F
where
    F: Fn() -> R + Send + Sync + 'static,
    R: ScriptRet,
{
    fn into_script_fn_parts(self) -> (Vec<ScriptTy>, ScriptTy, ScriptFnBody) {
        (
            Vec::new(),
            <R as ScriptRet>::script_ty(),
            Arc::new(move |_cx: &mut ScriptFnCx<'_>| self().into_script_result()),
        )
    }
}

/// One `IntoScriptFn` impl per arity. The marker is the tuple of parameter
/// types, which keeps the impls disjoint from each other and from [`Arity0`].
macro_rules! impl_into_script_fn {
    ($($ty:ident $idx:tt),+) => {
        impl<F, R, $($ty,)+> IntoScriptFn<($($ty,)+)> for F
        where
            F: Fn($($ty,)+) -> R + Send + Sync + 'static,
            R: ScriptRet,
            $( $ty: ScriptType + 'static, )+
        {
            fn into_script_fn_parts(self) -> (Vec<ScriptTy>, ScriptTy, ScriptFnBody) {
                (
                    vec![ $( <$ty as ScriptType>::script_ty(), )+ ],
                    <R as ScriptRet>::script_ty(),
                    Arc::new(move |cx: &mut ScriptFnCx<'_>| {
                        self( $( <$ty as ScriptType>::from_script_value(cx.arg_ref($idx)), )+ )
                            .into_script_result()
                    }),
                )
            }
        }
    };
}

impl_into_script_fn!(A0 0);
impl_into_script_fn!(A0 0, A1 1);
impl_into_script_fn!(A0 0, A1 1, A2 2);
impl_into_script_fn!(A0 0, A1 1, A2 2, A3 3);
impl_into_script_fn!(A0 0, A1 1, A2 2, A3 3, A4 4);
impl_into_script_fn!(A0 0, A1 1, A2 2, A3 3, A4 4, A5 5);
impl_into_script_fn!(A0 0, A1 1, A2 2, A3 3, A4 4, A5 5, A6 6);
impl_into_script_fn!(A0 0, A1 1, A2 2, A3 3, A4 4, A5 5, A6 6, A7 7);

/// The functions a host registered, kept so it can put them back.
///
/// A host that rebuilds its engine on [`ScriptHost::reset`](crate::ScriptHost::reset)
/// (Lua does) would otherwise lose every embedder registration, and the
/// candela compiler host replays the store into the scratch engine
/// `compile_check` builds so a source declaring `host "native" { .. }` still
/// checks.
#[derive(Clone, Default)]
pub struct ScriptFnStore(Vec<ScriptFn>);

impl ScriptFnStore {
    /// Record `f`, replacing any earlier function of the same namespace and
    /// name so a replay reproduces what the script last saw.
    pub fn record(&mut self, f: &ScriptFn) {
        match self.0.iter_mut().find(|e| e.name == f.name && e.ns == f.ns) {
            Some(existing) => *existing = f.clone(),
            None => self.0.push(f.clone()),
        }
    }

    /// Every recorded function, in registration order.
    pub fn iter(&self) -> std::slice::Iter<'_, ScriptFn> {
        self.0.iter()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ScriptFnStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.iter()).finish()
    }
}

/// Source a plugin ships in a script language, compiled ahead of the app's own
/// program.
///
/// This is how a plugin offers sugar over the functions it registered: a struct
/// and an `impl` block in that language, so a script calls `Gpio::read(pin)`
/// rather than the free function. Only the host whose language it names sees
/// it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptPrelude {
    /// The language tag the source is written in (`"candela"`, `"rhai"`,
    /// `"lua"`).
    pub lang: String,
    /// The namespace the functions it wraps are registered under.
    pub ns: String,
    /// The source itself.
    pub source: String,
}

/// The app-wide registration channel every script host drains.
///
/// Order is preserved and meaningful: a later function of the same namespace
/// and name shadows an earlier one, because that is what each engine does when
/// the same name is registered twice.
#[derive(Resource, Default, Debug)]
pub struct ScriptFnRegistry {
    fns: Vec<ScriptFn>,
    preludes: Vec<ScriptPrelude>,
    sealed: bool,
}

impl ScriptFnRegistry {
    /// Append a function.
    pub fn push(&mut self, f: ScriptFn) {
        self.fns.push(f);
    }

    /// Append a plugin's language source.
    ///
    /// Two plugins may ship source for the same namespace, and both are kept:
    /// one plugin registers the functions and another wraps them, which is a
    /// composition the registry has no business breaking. It is worth saying
    /// out loud, because the two sources are compiled into the same program and
    /// a name written in both is a compile error the app author did not write.
    pub fn push_prelude(&mut self, prelude: ScriptPrelude) {
        if let Some(prior) = self
            .preludes
            .iter()
            .find(|p| p.lang == prelude.lang && p.ns == prelude.ns)
        {
            warn_line!(
                "add_script_prelude(`{lang}`, `{ns}`): `{ns}` already carries {prior_len} bytes of \
                 {lang} source from an earlier plugin, and this adds {next_len} more; both \
                 compile, in registration order, so a name written in both is a compile error \
                 pointing into the wrapper",
                lang = prelude.lang,
                ns = prelude.ns,
                prior_len = prior.source.len(),
                next_len = prelude.source.len(),
            );
        }
        self.preludes.push(prelude);
    }

    /// Every registered function, in registration order.
    pub fn fns(&self) -> &[ScriptFn] {
        &self.fns
    }

    /// The sources `lang` is to compile ahead of the app's program, in
    /// registration order.
    pub fn preludes_for_lang(&self, lang: &str) -> Vec<&ScriptPrelude> {
        self.preludes.iter().filter(|p| p.lang == lang).collect()
    }

    /// The functions `lang` may see, cloned for handing to a host.
    pub fn for_lang(&self, lang: &str) -> Vec<ScriptFn> {
        self.fns
            .iter()
            .filter(|f| f.visible_to(lang))
            .cloned()
            .collect()
    }

    /// Close the channel. Every host has bound what it is going to bind.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    /// Whether the channel is closed.
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }
}

/// Register script functions on an [`App`].
///
/// Install the plugin that calls these through `RunOptions::with_plugin`, or
/// through the Rust SDK's `lumenui::App::add_plugin`; both run before the
/// script hosts load. A registration made after that is refused: Rhai and Lua
/// would bind a function no already-compiled program resolves, and candela
/// binds its host declarations when the program compiles or the artifact loads,
/// so there is nothing left to bind to.
pub trait ScriptFnAppExt {
    /// Register one function.
    fn add_script_fn(&mut self, f: ScriptFn) -> &mut Self;

    /// Register several functions, in order.
    fn add_script_fns(&mut self, fns: impl IntoIterator<Item = ScriptFn>) -> &mut Self;

    /// Register source in `lang` that the host of that language compiles ahead
    /// of the app's own program, wrapping the functions registered under `ns`.
    fn add_script_prelude(&mut self, lang: &str, ns: &str, source: &str) -> &mut Self;
}

impl ScriptFnAppExt for App {
    fn add_script_fn(&mut self, f: ScriptFn) -> &mut Self {
        let mut registry = registry_mut(self);
        if registry.is_sealed() {
            warn_line!(
                "add_script_fn(`{}`): the script hosts have already bound their functions; \
                 register it from a plugin installed through `RunOptions::with_plugin` or \
                 `lumenui::App::add_plugin`, which run before they load",
                f.name
            );
            return self;
        }
        registry.push(f);
        self
    }

    fn add_script_fns(&mut self, fns: impl IntoIterator<Item = ScriptFn>) -> &mut Self {
        for f in fns {
            self.add_script_fn(f);
        }
        self
    }

    fn add_script_prelude(&mut self, lang: &str, ns: &str, source: &str) -> &mut Self {
        let mut registry = registry_mut(self);
        if registry.is_sealed() {
            warn_line!(
                "add_script_prelude(`{lang}`, `{ns}`): the script hosts have already bound their \
                 functions; register it from a plugin installed through \
                 `RunOptions::with_plugin` or `lumenui::App::add_plugin`, which run before they \
                 load"
            );
            return self;
        }
        registry.push_prelude(ScriptPrelude {
            lang: lang.to_string(),
            ns: ns.to_string(),
            source: source.to_string(),
        });
        self
    }
}

/// The app's registry, created on first use.
fn registry_mut(app: &mut App) -> bevy_ecs::change_detection::Mut<'_, ScriptFnRegistry> {
    if !app.world.contains_resource::<ScriptFnRegistry>() {
        app.world.insert_resource(ScriptFnRegistry::default());
    }
    app.world.resource_mut::<ScriptFnRegistry>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_signature_rejects_the_wrong_shape_and_accepts_the_right_one() {
        let f = ScriptFn::new("set_pin")
            .param("pin", ScriptTy::Int)
            .param("high", ScriptTy::Bool)
            .ret(ScriptTy::Bool)
            .build(|cx| Ok(ScriptValue::Bool(cx.bool_arg(1))));

        assert!(
            f.sig
                .check_args(&[ScriptValue::I64(3), ScriptValue::Bool(true)])
                .is_ok()
        );
        let err = f
            .sig
            .check_args(&[ScriptValue::Str("three".into()), ScriptValue::Bool(true)])
            .expect_err("a string where an int is declared is a mismatch");
        assert!(err.contains("pin") && err.contains("int"), "{err}");
        assert!(
            f.sig.check_args(&[ScriptValue::I64(3)]).is_err(),
            "both parameters are required"
        );
    }

    #[test]
    fn optional_trailing_parameters_widen_the_arity_range() {
        let f = ScriptFn::new("page")
            .param("path", ScriptTy::Str)
            .min_arity(0)
            .build(|cx| Ok(ScriptValue::Str(cx.str_arg(0))));

        assert_eq!(f.sig.arity_range(), 0..=1);
        assert!(f.sig.check_args(&[]).is_ok());
        assert!(
            f.sig.check_args(&[ScriptValue::Unit]).is_ok(),
            "a unit placeholder stands in for an omitted optional argument"
        );
    }

    #[test]
    fn a_variadic_signature_binds_up_to_the_arity_bound() {
        let f = ScriptFn::new("log")
            .min_arity(0)
            .variadic()
            .build(|_cx| Ok(ScriptValue::Unit));
        assert_eq!(f.sig.arity_range(), 0..=MAX_VARIADIC_ARITY);
        let args = vec![ScriptValue::I64(1); 5];
        assert!(f.sig.check_args(&args).is_ok());
    }

    #[test]
    fn a_command_body_emits_into_the_call_scratch() {
        let f = ScriptFn::commands("shout", 1, |cx| {
            let text = cx.str_arg(0).to_uppercase();
            cx.emit(ScriptCommand::Print(text));
        });
        let (ret, cmds) = f.invoke(&[ScriptValue::Str("hi".into())]);
        assert_eq!(ret, Ok(ScriptValue::Unit));
        assert!(matches!(&cmds[..], [ScriptCommand::Print(s)] if s == "HI"));
    }

    #[test]
    fn a_host_set_hides_a_function_from_the_languages_it_excludes() {
        let f = ScriptFn::value("page", 1, |_| ScriptValue::Unit)
            .with_hosts(HostSet::RHAI | HostSet::LUA);
        assert!(f.visible_to("rhai"));
        assert!(f.visible_to("lua"));
        assert!(!f.visible_to("candela"));
        assert!(
            !ScriptFn::value("f", 0, |_| ScriptValue::Unit).visible_to("prolog"),
            "a language Lumen does not know is not one of the languages `ALL` names"
        );
    }

    #[test]
    fn the_store_replays_the_last_registration_of_a_name() {
        let mut store = ScriptFnStore::default();
        store.record(&ScriptFn::value("f", 0, |_| ScriptValue::I64(1)));
        store.record(&ScriptFn::value("g", 0, |_| ScriptValue::I64(2)));
        store.record(&ScriptFn::value("f", 0, |_| ScriptValue::I64(3)));

        let names: Vec<&str> = store.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["f", "g"], "one entry per name, in first order");
        let (ret, _) = store.iter().next().unwrap().invoke(&[]);
        assert_eq!(ret, Ok(ScriptValue::I64(3)), "the last body wins");
    }

    #[test]
    fn a_body_that_fails_hands_back_its_message_and_what_it_emitted() {
        let f = ScriptFn::new("gpio_write")
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Unit)
            .build(|cx| {
                cx.emit(ScriptCommand::Print("about to write".into()));
                Err(format!("pin {} is not exported", cx.int_arg(0)))
            });
        let (ret, cmds) = f.invoke(&[ScriptValue::I64(21)]);
        assert_eq!(ret, Err("pin 21 is not exported".to_string()));
        assert_eq!(cmds.len(), 1, "what ran before the failure still counts");
    }

    #[test]
    fn a_plain_closure_declares_the_types_of_its_arguments() {
        let f = ScriptFn::from_fn("local_id", |source: String, suffix: String| {
            format!("{source}:{suffix}")
        })
        .param_names(["source", "suffix"]);

        assert_eq!(f.sig.params.len(), 2);
        assert_eq!(f.sig.params[0].name, "source");
        assert_eq!(f.sig.params[1].ty, ScriptTy::Str);
        assert_eq!(f.sig.ret, ScriptTy::Str);
        assert_eq!(f.sig.min_arity, 2);
        let (ret, _) = f.invoke(&[
            ScriptValue::Str("card".into()),
            ScriptValue::Str("label".into()),
        ]);
        assert_eq!(ret, Ok(ScriptValue::Str("card:label".into())));
    }

    #[test]
    fn a_plain_closure_may_fail_and_still_declares_the_value_type() {
        let f = ScriptFn::from_fn("half", |n: i64| -> Result<i64, String> {
            if n % 2 == 0 {
                Ok(n / 2)
            } else {
                Err(format!("{n} is odd"))
            }
        });
        assert_eq!(
            f.sig.ret,
            ScriptTy::Int,
            "the declared type is the `Ok` one"
        );
        assert_eq!(f.invoke(&[ScriptValue::I64(8)]).0, Ok(ScriptValue::I64(4)));
        assert_eq!(
            f.invoke(&[ScriptValue::I64(7)]).0,
            Err("7 is odd".to_string())
        );
    }

    #[test]
    fn a_plain_closure_carries_lists_maps_and_no_arguments_at_all() {
        let nullary = ScriptFn::from_fn("answer", || 42i64);
        assert!(nullary.sig.params.is_empty());
        assert_eq!(nullary.invoke(&[]).0, Ok(ScriptValue::I64(42)));

        let listy = ScriptFn::from_fn("join", |parts: Vec<String>| parts.join("-"));
        assert_eq!(
            listy.sig.params[0].ty,
            ScriptTy::Array(Box::new(ScriptTy::Str))
        );
        let (ret, _) = listy.invoke(&[ScriptValue::Array(vec![
            ScriptValue::Str("a".into()),
            ScriptValue::Str("b".into()),
        ])]);
        assert_eq!(ret, Ok(ScriptValue::Str("a-b".into())));

        let mappy = ScriptFn::from_fn("sizes", || {
            std::collections::HashMap::from([("w".to_string(), 3i64)])
        });
        assert_eq!(mappy.sig.ret, ScriptTy::Map(Box::new(ScriptTy::Int)));
    }

    #[test]
    fn the_widest_closure_binds_and_the_names_stop_where_the_list_does() {
        let f = ScriptFn::from_fn(
            "wide",
            |a: i64, b: i64, c: i64, d: i64, e: i64, g: i64, h: i64, i: i64| {
                a + b + c + d + e + g + h + i
            },
        )
        .param_names(["a", "b"]);
        assert_eq!(f.sig.params.len(), MAX_VARIADIC_ARITY);
        assert_eq!(f.sig.params[1].name, "b");
        assert_eq!(f.sig.params[2].name, "arg2", "unnamed keeps its position");
        let args: Vec<ScriptValue> = (1..=8).map(ScriptValue::I64).collect();
        assert_eq!(f.invoke(&args).0, Ok(ScriptValue::I64(36)));
    }

    #[test]
    fn the_call_scratch_comes_back_cleared_and_nests() {
        let f = ScriptFn::commands("shout", 1, |cx| {
            cx.emit(ScriptCommand::Print(cx.str_arg(0)));
        });
        for _ in 0..3 {
            with_call_scratch(|scratch| {
                scratch.args.push(ScriptValue::Str("hi".into()));
                assert!(scratch.commands.is_empty(), "borrowed cleared");
                let ret = f.invoke_into(&scratch.args, &mut scratch.commands);
                assert_eq!(ret, Ok(ScriptValue::Unit));
                assert_eq!(scratch.commands.len(), 1);
                // A body that calls back into another function gets its own
                // pair, so the outer one is untouched.
                with_call_scratch(|inner| {
                    assert!(inner.args.is_empty() && inner.commands.is_empty());
                });
                assert_eq!(scratch.commands.len(), 1);
            });
        }
    }

    #[test]
    fn registration_after_the_seal_is_refused() {
        let mut app = App::new();
        app.add_script_fn(ScriptFn::value("early", 0, |_| ScriptValue::Unit));
        app.world.resource_mut::<ScriptFnRegistry>().seal();
        app.add_script_fn(ScriptFn::value("late", 0, |_| ScriptValue::Unit));

        let registry = app.world.resource::<ScriptFnRegistry>();
        let names: Vec<&str> = registry.fns().iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["early"]);
    }
}
