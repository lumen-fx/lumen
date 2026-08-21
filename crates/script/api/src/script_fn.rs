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

use crate::{ScriptCommand, ScriptValue};

/// The shape of one parameter or return value.
///
/// A host binds a typed parameter to its own type where it can (Rhai resolves a
/// call by argument type) and checks the argument where it cannot (Lua binds
/// variadically and raises on a mismatch). [`ScriptTy::Any`] accepts whatever
/// the script passes.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub fn accepts(&self, value: &ScriptValue) -> bool {
        match (self, value) {
            (Self::Any, _) => true,
            (Self::Unit, ScriptValue::Unit) => true,
            (Self::Bool, ScriptValue::Bool(_)) => true,
            (Self::Int, ScriptValue::I64(_)) => true,
            (Self::Float, ScriptValue::F64(_)) => true,
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
#[derive(Clone, Debug)]
pub struct ScriptParam {
    /// Parameter name, for docs and diagnostics.
    pub name: String,
    /// Declared type.
    pub ty: ScriptTy,
}

/// The declared signature of a [`ScriptFn`].
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
        self.args.get(i).cloned().unwrap_or(ScriptValue::Unit)
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
pub type ScriptFnBody = Arc<dyn Fn(&mut ScriptFnCx<'_>) -> ScriptValue + Send + Sync>;

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
    ///     .build(|cx| ScriptValue::Bool(cx.int_arg(0) > 0));
    /// assert_eq!(f.sig.params.len(), 1);
    /// ```
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
            body: Arc::new(move |cx: &mut ScriptFnCx<'_>| f(cx.args())),
        }
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
                ScriptValue::Unit
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

    /// Run the body over `args`, returning its value and whatever it emitted.
    ///
    /// Every host adapter goes through here, so a body observes the same
    /// argument slice and the same one-shot command scratch whichever language
    /// called it.
    pub fn invoke(&self, args: &[ScriptValue]) -> (ScriptValue, Vec<ScriptCommand>) {
        let mut out = Vec::new();
        let ret = {
            let mut cx = ScriptFnCx::new(args, &mut out);
            (self.body)(&mut cx)
        };
        (ret, out)
    }
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
    pub fn build<F>(self, body: F) -> ScriptFn
    where
        F: Fn(&mut ScriptFnCx<'_>) -> ScriptValue + Send + Sync + 'static,
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

/// candela source spliced ahead of an app's own program so a namespace's
/// functions can be called without the app declaring them.
///
/// candela resolves a host call through a declared `host "<ns>" { .. }` block,
/// so a plugin that registers into a namespace also describes the block that
/// declares it. Stored here now; the splice lands with the prelude work.
#[derive(Clone, Debug)]
pub struct CandelaWrapper {
    /// The host namespace the source declares.
    pub ns: String,
    /// The candela source to splice.
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
    candela_wrappers: Vec<CandelaWrapper>,
    sealed: bool,
}

impl ScriptFnRegistry {
    /// Append a function.
    pub fn push(&mut self, f: ScriptFn) {
        self.fns.push(f);
    }

    /// Append a candela declaration block.
    pub fn push_candela_wrapper(&mut self, wrapper: CandelaWrapper) {
        self.candela_wrappers.push(wrapper);
    }

    /// Every registered function, in registration order.
    pub fn fns(&self) -> &[ScriptFn] {
        &self.fns
    }

    /// Every registered candela declaration block.
    pub fn candela_wrappers(&self) -> &[CandelaWrapper] {
        &self.candela_wrappers
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
/// Install the plugin that calls these through
/// `RunOptions::with_plugin`, which runs before the script hosts load. A
/// registration made after that is refused: Rhai and Lua would bind a function
/// no already-compiled program resolves, and candela binds its host
/// declarations when the program compiles or the artifact loads, so there is
/// nothing left to bind to.
pub trait ScriptFnAppExt {
    /// Register one function.
    fn add_script_fn(&mut self, f: ScriptFn) -> &mut Self;

    /// Register several functions, in order.
    fn add_script_fns(&mut self, fns: impl IntoIterator<Item = ScriptFn>) -> &mut Self;

    /// Register the candela `host "<ns>" { .. }` block that declares a
    /// namespace's functions.
    fn add_candela_wrapper(&mut self, ns: &str, source: &str) -> &mut Self;
}

impl ScriptFnAppExt for App {
    fn add_script_fn(&mut self, f: ScriptFn) -> &mut Self {
        let mut registry = registry_mut(self);
        if registry.is_sealed() {
            warn_line!(
                "add_script_fn(`{}`): the script hosts have already bound their functions; \
                 register it from a plugin installed through `RunOptions::with_plugin`, which \
                 runs before they load",
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

    fn add_candela_wrapper(&mut self, ns: &str, source: &str) -> &mut Self {
        let mut registry = registry_mut(self);
        if registry.is_sealed() {
            warn_line!(
                "add_candela_wrapper(`{ns}`): the script hosts have already bound their \
                 functions; register it from a plugin installed through \
                 `RunOptions::with_plugin`, which runs before they load"
            );
            return self;
        }
        registry.push_candela_wrapper(CandelaWrapper {
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
            .build(|cx| ScriptValue::Bool(cx.bool_arg(1)));

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
            .build(|cx| ScriptValue::Str(cx.str_arg(0)));

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
            .build(|_cx| ScriptValue::Unit);
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
        assert_eq!(ret, ScriptValue::Unit);
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
        assert_eq!(ret, ScriptValue::I64(3), "the last body wins");
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
