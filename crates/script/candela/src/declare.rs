//! Turning a [`ScriptFn`] into the candela `host` declaration that binds it.
//!
//! candela resolves a host call through a declaration, so every function a
//! script can call has to be spelled out in a `host "<ns>" { .. }` block. Two
//! places need that spelling and both come from here: the prelude an app
//! imports, whose declarations are generated from the shared builtin table,
//! and the blocks the host synthesizes for a plugin's namespace so an app can
//! call a plugin function without declaring it by hand.

use lumen_script::{ScriptFn, ScriptNs, ScriptTy};

/// How candela spells a type in a declaration.
fn ty_name(ty: &ScriptTy) -> String {
    match ty {
        ScriptTy::Int => "int".to_string(),
        ScriptTy::Float => "float".to_string(),
        ScriptTy::Bool => "bool".to_string(),
        ScriptTy::Str => "string".to_string(),
        ScriptTy::Unit => "null".to_string(),
        ScriptTy::Any => "any".to_string(),
        ScriptTy::Array(inner) => format!("{}[]", ty_name(inner)),
        ScriptTy::Map(value) => format!("{{string: {}}}", ty_name(value)),
    }
}

/// The declaration line that binds `f`, without indentation.
///
/// ```text
/// set_text(string, string);
/// string local_id(string, string);
/// any parse_json(...);
/// ```
///
/// A signature the shape adapter cannot bind typed is bound variadically, and
/// candela rejects a declaration that disagrees with the binding, so the
/// argument list follows the adapter rather than the signature.
pub(crate) fn declaration(f: &ScriptFn) -> String {
    if !crate::host_fns::binds_typed(f) {
        return format!("any {}(...);", f.name);
    }
    let params: Vec<String> = f.sig.params.iter().map(|p| ty_name(&p.ty)).collect();
    let args = params.join(", ");
    match f.sig.ret {
        ScriptTy::Unit => format!("{}({args});", f.name),
        ref ret => format!("{} {}({args});", ty_name(ret), f.name),
    }
}

/// The namespace `f` is declared under, or `None` when it is not a host
/// function candela reaches by namespace.
pub(crate) fn namespace(f: &ScriptFn) -> &str {
    match &f.ns {
        ScriptNs::Builtin => crate::host_fns::HOST_NAMESPACE,
        ScriptNs::Extension => crate::host_fns::NATIVE_NAMESPACE,
        ScriptNs::Named(ns) => ns.as_str(),
    }
}

/// A whole `host "<ns>" { .. }` block, one declaration per line.
pub(crate) fn host_block(ns: &str, lines: impl IntoIterator<Item = String>) -> String {
    let mut out = format!("host \"{ns}\" {{\n");
    for line in lines {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&format!("    {line}\n"));
        }
    }
    out.push_str("}\n");
    out
}

/// A block folded onto one physical line.
///
/// A synthesized block costs the diagnostics one line of offset per block
/// rather than one per declaration, and the offset is what
/// [`crate::prelude::PreparedSource`] carries so an error still points at the
/// author's line.
pub(crate) fn one_line_block(ns: &str, fns: &[ScriptFn]) -> String {
    let decls: Vec<String> = fns.iter().map(declaration).collect();
    format!("host \"{ns}\" {{ {} }}", decls.join(" "))
}

/// The generated half of the prelude: every declaration the shared table and
/// the host's own registrations put behind the `import "lumen.cdl";` line.
pub(crate) fn generated_prelude() -> String {
    let mut out = String::new();
    out.push_str(GENERATED_HEADER);

    let table = lumen_script::builtin_script_fns();
    let mut namespaces: Vec<&str> = Vec::new();
    for (ns, _) in crate::host_fns::RESIDUAL_DECLARATIONS {
        if !namespaces.contains(ns) {
            namespaces.push(ns);
        }
    }
    // The shared table is all one namespace today; take it from the entries
    // rather than assuming so.
    for f in table.iter().filter(|f| f.visible_to("candela")) {
        let ns = namespace(f);
        if !namespaces.contains(&ns) {
            namespaces.push(ns);
        }
    }

    for ns in namespaces {
        let mut lines: Vec<String> = Vec::new();
        let shared: Vec<String> = table
            .iter()
            .filter(|f| f.visible_to("candela") && namespace(f) == ns)
            .map(declaration)
            .collect();
        if !shared.is_empty() {
            lines.push("// Shared with the other hosts.".to_string());
            lines.extend(shared);
        }
        let residual: Vec<String> = crate::host_fns::RESIDUAL_DECLARATIONS
            .iter()
            .filter(|(entry_ns, _)| *entry_ns == ns)
            .map(|(_, decl)| (*decl).to_string())
            .collect();
        if !residual.is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push("// Registered by the candela host itself.".to_string());
            lines.extend(residual);
        }
        out.push('\n');
        out.push_str(&host_block(ns, lines));
    }
    out
}

/// The header the generated file opens with.
const GENERATED_HEADER: &str = "\
// Generated file. Do not edit.
//
// These are the declarations that bind the Lumen host surface to the closures
// the candela host registers. An app pulls them in with one line:
//
//     import \"lumen.cdl\";
//
// The method sugar over them is hand-written and lives in `wrappers.cdl`; the
// two are spliced together where the import stood.
//
// Refresh after changing a builtin:
//
//     UPDATE_PRELUDE=1 cargo test -p lumen-script-candela --test prelude_generated
";

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use candela_vm::{IntoHostFn, Value};
    use lumen_script::{ScriptTy as T, ScriptValue};

    use super::*;
    use crate::host_fns::{HostFnSink, Registries};

    fn probe(name: &str) -> lumen_script::ScriptFnBuilder {
        ScriptFn::new(name).ns(ScriptNs::Builtin)
    }

    #[test]
    fn a_unit_return_is_spelled_by_leaving_it_out() {
        let f = probe("set_text")
            .param("id", T::Str)
            .param("text", T::Str)
            .ret(T::Unit)
            .build(|_| ScriptValue::Unit);
        assert_eq!(declaration(&f), "set_text(string, string);");
    }

    #[test]
    fn a_declared_return_leads_the_line() {
        let f = probe("node_rect")
            .param("node", T::Int)
            .ret(T::Map(Box::new(T::Float)))
            .build(|_| ScriptValue::Unit);
        assert_eq!(declaration(&f), "{string: float} node_rect(int);");

        let f = probe("node_query")
            .param("selector", T::Str)
            .ret(T::Array(Box::new(T::Int)))
            .build(|_| ScriptValue::Unit);
        assert_eq!(declaration(&f), "int[] node_query(string);");
    }

    #[test]
    fn a_shape_candela_cannot_name_is_variadic() {
        let f = probe("parse_json")
            .param("text", T::Str)
            .ret(T::Any)
            .build(|_| ScriptValue::Unit);
        assert_eq!(declaration(&f), "any parse_json(...);");

        let f = probe("log").variadic().build(|_| ScriptValue::Unit);
        assert_eq!(declaration(&f), "any log(...);");
    }

    /// A signature candela could name, but the shape adapter has no closure
    /// for, still declares variadically: the declaration follows the binding.
    #[test]
    fn a_shape_the_adapter_cannot_bind_declares_variadic() {
        let f = probe("mix")
            .param("a", T::Float)
            .param("b", T::Float)
            .ret(T::Float)
            .build(|_| ScriptValue::Unit);
        assert!(!crate::host_fns::binds_typed(&f));
        assert_eq!(declaration(&f), "any mix(...);");
    }

    /// A sink that binds nothing and remembers every name offered to it.
    #[derive(Default)]
    struct Recorder(Vec<(String, String)>);

    impl HostFnSink for Recorder {
        fn register_host_fn<Marker, F>(&mut self, namespace: &str, name: &str, _f: F)
        where
            F: IntoHostFn<Marker>,
        {
            self.0.push((namespace.to_owned(), name.to_owned()));
        }

        fn register_host_fn_variadic<F>(&mut self, namespace: &str, name: &str, _f: F)
        where
            F: Fn(&[Value]) -> Value + 'static,
        {
            self.0.push((namespace.to_owned(), name.to_owned()));
        }
    }

    /// The `(namespace, function name)` pairs a block of declarations spells.
    fn declared(source: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut ns = String::new();
        for line in source.lines() {
            let code = line.split("//").next().unwrap_or("").trim();
            if let Some(rest) = code.strip_prefix("host \"") {
                ns = rest.split('"').next().unwrap_or("").to_owned();
                continue;
            }
            let Some(args) = code.find('(') else { continue };
            let name = code[..args].rsplit([' ', ']', '}']).next().unwrap_or("");
            if !name.is_empty() {
                out.push((ns.clone(), name.to_owned()));
            }
        }
        out
    }

    /// Every function the host binds is a function the prelude declares.
    ///
    /// The sink sees the registrations themselves, so a builtin added without
    /// its declaration fails here rather than at the first call from an app.
    #[test]
    fn the_generated_declarations_cover_every_registration() {
        let mut recorder = Recorder::default();
        crate::host_fns::register_lumen_host_fns(&mut recorder, &Registries::default());

        let declared: HashSet<(String, String)> =
            declared(&generated_prelude()).into_iter().collect();
        let mut missing: Vec<String> = recorder
            .0
            .into_iter()
            .filter(|entry| !declared.contains(entry))
            .map(|(ns, name)| format!("{ns}::{name}"))
            .collect();
        missing.sort_unstable();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "these host functions are registered but undeclared, so a script cannot call them: \
             {missing:?}"
        );
    }
}
