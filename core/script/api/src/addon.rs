//! Binding a browser add-on's functions into the script hosts.
//!
//! An add-on describes its functions as data ([`lumen_ir::addon::Addon`]),
//! and every target binds the same description: a page to the JavaScript
//! module it loaded, a compile to a body that is never called, and everything
//! else (a build-time render, a server, a desktop run) to a body that says the
//! function runs only in a browser. One description, one signature per
//! function, so a program compiled against one of them binds against all of
//! them.
//!
//! An asynchronous function takes a tag after its declared arguments and
//! returns nothing: its result arrives later as the `<event>` handler, called
//! with the tag and the value, or `<event>_error`, called with the tag and the
//! reason.

use std::sync::Arc;

use lumen_ir::addon::{Addon, AddonFunction};

use crate::ScriptTy;
use crate::script_fn::{ScriptFn, ScriptFnBody, ScriptNs, ScriptParam, ScriptResult, ScriptSig};

/// The name of the argument an asynchronous function takes its tag in.
pub const TAG_PARAM: &str = "tag";

/// The event an asynchronous function's failure arrives as.
pub fn error_event(event: &str) -> String {
    format!("{event}_error")
}

/// The signature a script sees for `function`: its declared parameters, a
/// trailing tag for an asynchronous one, and what it returns.
///
/// # Errors
///
/// A parameter or the return names no type.
pub fn signature(addon: &Addon, function: &AddonFunction) -> Result<ScriptSig, String> {
    let ty = |spelling: &str, what: &str| -> Result<ScriptTy, String> {
        spelling.parse::<ScriptTy>().map_err(|e| {
            format!(
                "add-on '{}': {}::{} {what}: {e}",
                addon.name, addon.namespace, function.name
            )
        })
    };
    let mut params = Vec::with_capacity(function.params.len() + 1);
    for param in &function.params {
        params.push(ScriptParam {
            name: param.name.clone(),
            ty: ty(&param.ty, &format!("parameter `{}`", param.name))?,
        });
    }
    let ret = if function.event.is_some() {
        params.push(ScriptParam {
            name: TAG_PARAM.to_owned(),
            ty: ScriptTy::Str,
        });
        ScriptTy::Unit
    } else if function.returns.is_empty() {
        ScriptTy::Unit
    } else {
        ty(&function.returns, "return")?
    };
    Ok(ScriptSig {
        min_arity: params.len(),
        params,
        ret,
        variadic: false,
        doc: function.doc.clone(),
    })
}

/// Every function `addon` offers, bound to the body `body` builds for it.
///
/// `body` is called once per function with its position in
/// [`Addon::functions`] and its description.
///
/// # Errors
///
/// A function's signature names no type.
pub fn script_fns(
    addon: &Addon,
    mut body: impl FnMut(usize, &AddonFunction) -> ScriptFnBody,
) -> Result<Vec<ScriptFn>, String> {
    let mut fns = Vec::with_capacity(addon.functions.len());
    for (index, function) in addon.functions.iter().enumerate() {
        fns.push(ScriptFn {
            name: function.name.clone(),
            ns: ScriptNs::Named(addon.namespace.clone()),
            sig: signature(addon, function)?,
            hosts: Default::default(),
            body: body(index, function),
        });
    }
    Ok(fns)
}

/// Something told about a call a target answered with "only in a browser".
pub type OnBrowserOnlyCall = Arc<dyn Fn(&str) + Send + Sync>;

/// Every function `addon` offers, bound to a body that raises
/// `<namespace>::<function> runs only in a browser` in the script that called
/// it.
///
/// This is what a target with no page binds: the program was compiled against
/// the add-on, so its calls have to resolve, and the answer to one is that it
/// cannot run here. `on_call`, when given, hears the qualified name
/// of each call before it raises, which is how a build reports them.
///
/// # Errors
///
/// A function's signature names no type.
pub fn browser_only_fns(
    addon: &Addon,
    on_call: Option<OnBrowserOnlyCall>,
) -> Result<Vec<ScriptFn>, String> {
    script_fns(addon, |_, function| {
        let qualified = format!("{}::{}", addon.namespace, function.name);
        let on_call = on_call.clone();
        Arc::new(move |_| -> ScriptResult {
            if let Some(on_call) = &on_call {
                on_call(&qualified);
            }
            Err(format!("{qualified} runs only in a browser"))
        })
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use lumen_ir::addon::AddonParam;

    use super::*;
    use crate::ScriptValue;

    fn echo() -> Addon {
        Addon {
            name: "echo".into(),
            namespace: "echo".into(),
            functions: vec![
                AddonFunction {
                    name: "shout".into(),
                    params: vec![AddonParam {
                        name: "text".into(),
                        ty: "string".into(),
                    }],
                    returns: "string".into(),
                    event: None,
                    doc: "Upper-case it.".into(),
                },
                AddonFunction {
                    name: "later".into(),
                    params: vec![AddonParam {
                        name: "ms".into(),
                        ty: "int".into(),
                    }],
                    returns: String::new(),
                    event: Some("on_echo".into()),
                    doc: String::new(),
                },
            ],
            elements: Vec::new(),
        }
    }

    #[test]
    fn a_function_binds_under_the_addons_namespace_with_its_declared_types() {
        let fns = script_fns(&echo(), |_, _| Arc::new(|_| Ok(ScriptValue::Unit))).unwrap();
        assert_eq!(fns[0].name, "shout");
        assert_eq!(fns[0].ns, ScriptNs::Named("echo".into()));
        assert_eq!(fns[0].sig.params[0].ty, ScriptTy::Str);
        assert_eq!(fns[0].sig.ret, ScriptTy::Str);
        assert_eq!(fns[0].sig.doc, "Upper-case it.");
    }

    #[test]
    fn an_asynchronous_function_takes_a_tag_and_returns_nothing() {
        let fns = script_fns(&echo(), |_, _| Arc::new(|_| Ok(ScriptValue::Unit))).unwrap();
        let params: Vec<(&str, &ScriptTy)> = fns[1]
            .sig
            .params
            .iter()
            .map(|p| (p.name.as_str(), &p.ty))
            .collect();
        assert_eq!(
            params,
            [("ms", &ScriptTy::Int), (TAG_PARAM, &ScriptTy::Str)]
        );
        assert_eq!(fns[1].sig.ret, ScriptTy::Unit);
        assert_eq!(fns[1].sig.min_arity, 2);
    }

    #[test]
    fn a_type_no_host_reads_names_the_function() {
        let mut addon = echo();
        addon.functions[0].params[0].ty = "text".into();
        let err = script_fns(&addon, |_, _| Arc::new(|_| Ok(ScriptValue::Unit))).unwrap_err();
        assert!(err.contains("echo::shout parameter `text`"), "{err}");
    }

    #[test]
    fn a_browser_only_call_raises_and_is_heard() {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&heard);
        let fns = browser_only_fns(
            &echo(),
            Some(Arc::new(move |name: &str| {
                sink.lock().unwrap().push(name.to_owned());
            })),
        )
        .unwrap();
        let (ret, _) = fns[0].invoke(&[ScriptValue::Str("hi".into())]);
        assert_eq!(ret, Err("echo::shout runs only in a browser".to_owned()));
        assert_eq!(*heard.lock().unwrap(), ["echo::shout"]);
    }
}
