//! Typed C++ and Python wrappers over a module's script functions.
//!
//! A module's functions reach C, C++ and Python through one C call,
//! `lumen_call(ns, name, argc, argv, out)`, which takes and returns untyped
//! values. These generators turn the signatures the module registered into a
//! header and a Python module whose functions take and return the declared
//! types and make that call, so a C++ or Python app calls
//! `lumen::canvas::fill_rect("art", 0, 0, 8, 8)` or
//! `lumen.canvas.fill_rect("art", 0, 0, 8, 8)` the way a script calls
//! `canvas::fill_rect`. Both are named after the module, without its `lumen-`
//! prefix: `lumen-fs` is `lumen::fs` and `lumen.fs`, whatever namespace its
//! functions use in a script. The same two functions serve every module; the
//! first-party modules' wrappers are generated from them and checked in under
//! the C++ and Python SDKs.
//!
//! A parameter or a result of a shape the target language has no single type
//! for (a list, a map, a struct of options, an open value) is the SDK's
//! generic value: `lumen::Value` in C++, a plain Python value in Python. A
//! function with optional trailing parameters takes them all; a variadic one
//! takes a list of values.

use lumen_script::{ScriptFn, ScriptNs, ScriptTy};

/// The namespace a script writes for `ns`, and the one `lumen_call` takes.
pub fn namespace_name(ns: &ScriptNs) -> &str {
    match ns {
        ScriptNs::Builtin => "",
        ScriptNs::Extension => "native",
        ScriptNs::Named(name) => name,
    }
}

/// The name the wrappers of the module `module` go by: its package name
/// without the `lumen-` prefix (`lumen-canvas` is `canvas`).
pub fn wrapper_name(module: &str) -> String {
    module
        .strip_prefix("lumen-")
        .unwrap_or(module)
        .replace('-', "_")
}

/// The functions of `fns`, each namespace and name once (a later
/// registration shadows an earlier one, as in the script hosts), in
/// registration order.
pub fn distinct(fns: &[ScriptFn]) -> Vec<&ScriptFn> {
    let mut out: Vec<&ScriptFn> = Vec::new();
    for f in fns {
        match out
            .iter()
            .position(|seen| seen.name == f.name && seen.ns == f.ns)
        {
            Some(i) => out[i] = f,
            None => out.push(f),
        }
    }
    out
}

const CPP_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "asm",
    "auto",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "namespace",
    "new",
    "not",
    "operator",
    "or",
    "private",
    "protected",
    "public",
    "register",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    "xor",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

/// `name` as an identifier: a keyword gets a trailing underscore.
fn ident(name: &str, keywords: &[&str]) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if keywords.contains(&clean.as_str()) {
        format!("{clean}_")
    } else {
        clean
    }
}

/// A doc string's lines, prefixed for a comment.
fn doc_lines(doc: &str, prefix: &str) -> String {
    doc.lines()
        .map(|line| {
            if line.trim().is_empty() {
                format!("{}\n", prefix.trim_end())
            } else {
                format!("{prefix}{line}\n")
            }
        })
        .collect()
}

/// The C++ type of a parameter of type `ty`.
fn cpp_param(ty: &ScriptTy) -> &'static str {
    match ty {
        ScriptTy::Bool => "bool",
        ScriptTy::Int => "std::int64_t",
        ScriptTy::Float => "double",
        ScriptTy::Str => "std::string_view",
        _ => "const ::lumen::Value&",
    }
}

/// The C++ return type for `ty`, and how the call's `Value` becomes it.
fn cpp_ret(ty: &ScriptTy) -> (&'static str, &'static str) {
    match ty {
        ScriptTy::Unit => ("void", ""),
        ScriptTy::Bool => ("bool", ".as_bool()"),
        ScriptTy::Int => ("std::int64_t", ".as_int()"),
        ScriptTy::Float => ("double", ".as_float()"),
        ScriptTy::Str => ("std::string", ".as_string()"),
        _ => ("::lumen::Value", ""),
    }
}

/// How a C++ parameter becomes a `Value`.
fn cpp_arg(ty: &ScriptTy, name: &str) -> String {
    match ty {
        ScriptTy::Bool => format!("::lumen::Value::boolean({name})"),
        ScriptTy::Int => format!("::lumen::Value::integer({name})"),
        ScriptTy::Float => format!("::lumen::Value::floating({name})"),
        ScriptTy::Str => format!("::lumen::Value::string(std::string({name}))"),
        _ => name.to_string(),
    }
}

/// A C++ header wrapping the functions `fns` the module `module` (its
/// package name) registers.
pub fn cpp_header(module: &str, fns: &[ScriptFn]) -> String {
    let cpp_ns = ident(&wrapper_name(module), CPP_KEYWORDS);
    let mut out = format!(
        "// Generated by `lumenc` from the script functions of the `{module}` module.\n\
         // Do not edit: regenerate with the module's signatures instead.\n\
         //\n\
         // Each function makes one `lumen::call`, so it works once the app that\n\
         // declares the module is running; see the C++ section of the FFI reference.\n\
         #pragma once\n\n\
         #include <cstdint>\n\
         #include <string>\n\
         #include <string_view>\n\
         #include <vector>\n\n\
         #include <lumen.hpp>\n\n\
         namespace lumen::{cpp_ns} {{\n"
    );
    for f in distinct(fns) {
        let space = namespace_name(&f.ns);
        out.push('\n');
        if let Some(doc) = Some(f.sig.doc.as_str()).filter(|d| !d.trim().is_empty()) {
            out.push_str(&doc_lines(doc, "/// "));
        }
        let (ret, read) = cpp_ret(&f.sig.ret);
        let name = ident(&f.name, CPP_KEYWORDS);
        let (params, args) = if f.sig.variadic {
            (
                "const std::vector<::lumen::Value>& args".to_string(),
                "args".to_string(),
            )
        } else {
            let params: Vec<String> = f
                .sig
                .params
                .iter()
                .map(|p| format!("{} {}", cpp_param(&p.ty), ident(&p.name, CPP_KEYWORDS)))
                .collect();
            let args: Vec<String> = f
                .sig
                .params
                .iter()
                .map(|p| cpp_arg(&p.ty, &ident(&p.name, CPP_KEYWORDS)))
                .collect();
            (params.join(", "), format!("{{{}}}", args.join(", ")))
        };
        let call = format!("::lumen::call(\"{space}\", \"{}\", {args})", f.name);
        let body = if ret == "void" {
            format!("{call};")
        } else {
            format!("return {call}{read};")
        };
        out.push_str(&format!(
            "inline {ret} {name}({params}) {{\n    {body}\n}}\n"
        ));
    }
    out.push_str(&format!("\n}} // namespace lumen::{cpp_ns}\n"));
    out
}

/// The Python annotation for a value of type `ty`.
fn py_type(ty: &ScriptTy) -> &'static str {
    match ty {
        ScriptTy::Unit => "None",
        ScriptTy::Bool => "bool",
        ScriptTy::Int => "int",
        ScriptTy::Float => "float",
        ScriptTy::Str => "str",
        ScriptTy::Array(_) => "list",
        ScriptTy::Map(_) | ScriptTy::Struct(_) => "dict",
        ScriptTy::Any | ScriptTy::Dynamic => "object",
    }
}

/// A Python module wrapping the functions `fns` the module `module` (its
/// package name) registers.
pub fn python_module(module: &str, fns: &[ScriptFn]) -> String {
    let mut out = format!(
        "\"\"\"The `{module}` module's script functions, for Python.\n\n\
         Generated by ``lumenc`` from the module's signatures; do not edit. Each\n\
         function makes one :func:`lumen.call`, so it works once the app that\n\
         declares the module is running.\n\
         \"\"\"\n\n\
         from __future__ import annotations\n\n\
         from .api import call\n"
    );
    for f in distinct(fns) {
        let space = namespace_name(&f.ns);
        let name = ident(&f.name, PYTHON_KEYWORDS);
        let (params, args) = if f.sig.variadic {
            ("*args: object".to_string(), "*args".to_string())
        } else {
            let params: Vec<String> = f
                .sig
                .params
                .iter()
                .map(|p| format!("{}: {}", ident(&p.name, PYTHON_KEYWORDS), py_type(&p.ty)))
                .collect();
            let args: Vec<String> = f
                .sig
                .params
                .iter()
                .map(|p| ident(&p.name, PYTHON_KEYWORDS))
                .collect();
            (params.join(", "), args.join(", "))
        };
        let ret = py_type(&f.sig.ret);
        let call_args = if args.is_empty() {
            String::new()
        } else {
            format!(", {args}")
        };
        out.push_str(&format!("\n\ndef {name}({params}) -> {ret}:\n"));
        if let Some(doc) = Some(f.sig.doc.as_str()).filter(|d| !d.trim().is_empty()) {
            let doc = doc.replace("\"\"\"", "'''");
            out.push_str(&format!("    \"\"\"{}\"\"\"\n\n", doc.trim()));
        }
        let call = format!("call(\"{space}\", \"{}\"{call_args})", f.name);
        if ret == "None" {
            out.push_str(&format!("    {call}\n"));
        } else {
            out.push_str(&format!(
                "    return {call}  # type: ignore[return-value]\n"
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_script::ScriptValue;

    fn sample() -> Vec<ScriptFn> {
        let ns = ScriptNs::Named("shapes".into());
        vec![
            ScriptFn::new("area")
                .ns(ns.clone())
                .doc("The area of a box.")
                .param("w", ScriptTy::Float)
                .param("h", ScriptTy::Float)
                .ret(ScriptTy::Float)
                .build(|_| Ok(ScriptValue::F64(0.0))),
            ScriptFn::new("delete")
                .ns(ns.clone())
                .param("id", ScriptTy::Str)
                .param("from", ScriptTy::Array(Box::new(ScriptTy::Str)))
                .ret(ScriptTy::Unit)
                .build(|_| Ok(ScriptValue::Unit)),
            ScriptFn::new("log")
                .ns(ns.clone())
                .variadic()
                .build(|_| Ok(ScriptValue::Unit)),
            ScriptFn::new("elsewhere")
                .ns(ScriptNs::Builtin)
                .build(|_| Ok(ScriptValue::Unit)),
            ScriptFn::new("area")
                .ns(ns)
                .param("side", ScriptTy::Int)
                .ret(ScriptTy::Int)
                .build(|_| Ok(ScriptValue::I64(0))),
        ]
    }

    /// The header takes the declared types, makes a keyword a legal name,
    /// keeps a later registration over an earlier one of the same name, and
    /// calls each function in its own namespace.
    #[test]
    fn a_header_wraps_each_function_in_its_types() {
        let header = cpp_header("lumen-shape-kit", &sample());
        assert!(header.contains("namespace lumen::shape_kit {"), "{header}");
        assert!(
            header.contains(
                "inline std::int64_t area(std::int64_t side) {\n    return \
                 ::lumen::call(\"shapes\", \"area\", {::lumen::Value::integer(side)}).as_int();"
            ),
            "{header}"
        );
        assert!(
            header.contains(
                "inline void delete_(std::string_view id, const ::lumen::Value& from) {\n    \
                 ::lumen::call(\"shapes\", \"delete\", \
                 {::lumen::Value::string(std::string(id)), from});"
            ),
            "{header}"
        );
        assert!(
            header.contains("log(const std::vector<::lumen::Value>& args)"),
            "{header}"
        );
        assert!(
            header.contains("::lumen::call(\"\", \"elsewhere\", {})"),
            "{header}"
        );
        assert_eq!(header.matches(" area(").count(), 1, "{header}");
    }

    /// The Python module does the same in Python's terms.
    #[test]
    fn a_python_module_wraps_each_function_in_its_types() {
        let module = python_module("lumen-shape-kit", &sample());
        assert!(
            module.contains(
                "def area(side: int) -> int:\n    return call(\"shapes\", \"area\", side)"
            ),
            "{module}"
        );
        assert!(
            module.contains("def delete(id: str, from_: list) -> None:\n    call(\"shapes\", \"delete\", id, from_)"),
            "{module}"
        );
        assert!(
            module.contains("def log(*args: object) -> object:"),
            "{module}"
        );
        assert!(module.contains("from .api import call"));
    }

    #[test]
    fn names_follow_the_module_and_the_script() {
        assert_eq!(wrapper_name("lumen-canvas"), "canvas");
        assert_eq!(wrapper_name("acme-charts"), "acme_charts");
        assert_eq!(namespace_name(&ScriptNs::Builtin), "");
        assert_eq!(namespace_name(&ScriptNs::Extension), "native");
        assert_eq!(namespace_name(&ScriptNs::Named("canvas".into())), "canvas");
    }
}
