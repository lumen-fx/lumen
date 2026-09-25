//! Checking a script that imports a native library, without opening one.
//!
//! candela's compiler opens the library a `dylib "..."` block names and takes a
//! code pointer for every symbol the block declares, because a running program
//! calls through those pointers. The types come from the block itself: the
//! declared signatures are what the call sites are checked against, and the
//! library is read for addresses only.
//!
//! A check runs no handler and keeps no program, so it never needs an address.
//! Requiring one anyway makes an app whose library a build hook produces
//! impossible to check, because `lumenc check` runs no hooks: the library is
//! missing, or left over from older sources, and the check fails on a symbol
//! rather than on anything the author wrote.
//!
//! So a check reads such a block as a `host` block instead. The two parse
//! through the same grammar and type-check through the same path; a `host`
//! block binds to closures in the process rather than to symbols on disk, and
//! each declared function is bound here to a stub returning the zero value of
//! its declared return type. That is enough for the one top-level run
//! `candela::Engine::compile` performs, and no check reads the answer.
//!
//! The rewrite keeps every byte offset, so a diagnostic still points at the
//! column the author wrote: `dylib` and `host ` are both five bytes.
//!
//! Three shapes are left alone, and for them a check still resolves the library
//! the way a run does: a block that names its library by path rather than by
//! bare name (the namespace candela derives from a path is not the text in the
//! source), a block whose library name is a namespace the text already declares
//! (binding stubs under it would displace what the app resolves against), and a
//! block declaring a type that does not cross the host boundary (a C struct, a
//! pointer). All three compile exactly as they did before.
//!
//! Only the text a check hands to `candela::Engine::compile` is read this way,
//! which is the app's own script. candela resolves `import "..."` by reading the
//! file from disk while it compiles, and its compiler takes no source from a
//! caller, so a `dylib` block in an imported file still opens its library.
//! Closing that needs a check-only compile in candela itself, which is filed as
//! lumen-fx/candela#177.
//!
//! What a stub answers is the zero of its declared type, and
//! `candela::Engine::compile` runs `main` once, so a program computing with a
//! value the library would have produced can reach a conclusion a run never
//! would: `100 / md_count()` divides by zero. The types are still the block's
//! own, so every call site is checked against what the author declared.

use candela_vm::{HostError, HostType, Value};

use crate::host_fns::{HOST_NAMESPACE, HostFnSink};
use crate::prelude::{code_of, declares_namespace};

/// The keyword a block opens with, and what a check reads it as. Both are five
/// bytes, which is what keeps the rewrite offset-preserving.
const DYLIB: &str = "dylib";
const AS_HOST: &str = "host ";

/// One function a `dylib` block declares, as a check binds it.
pub(crate) struct Stub {
    namespace: String,
    name: String,
    args: Vec<HostType>,
    ret: HostType,
}

/// `source` with every `dylib` block a check can read as a `host` block
/// rewritten into one, plus the declarations those blocks made.
///
/// `None` means no block was rewritten and the caller compiles the text it
/// already has.
pub(crate) fn as_host_blocks(source: &str) -> (Option<String>, Vec<Stub>) {
    let lines = lines_with_offsets(source);
    let mut rewritten: Option<String> = None;
    let mut stubs: Vec<Stub> = Vec::new();
    let mut taken: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let (offset, line) = lines[i];
        let Some((at, name)) = opener(line) else {
            i += 1;
            continue;
        };
        if spoken_for(source, name) || taken.contains(&name) {
            // The namespace answers for something else already, and a check
            // that bound stubs under it would resolve the app's own calls
            // against them. The block stays a `dylib` block.
            i += 1;
            continue;
        }
        let Some((end, declared)) = block(&lines, i + 1, name) else {
            // A declaration this module cannot read leaves the block a `dylib`
            // block, so the check behaves as it did before.
            i += 1;
            continue;
        };
        let text = rewritten.get_or_insert_with(|| source.to_owned());
        let keyword = offset + at;
        text.replace_range(keyword..keyword + DYLIB.len(), AS_HOST);
        stubs.extend(declared);
        taken.push(name);
        i = end + 1;
    }
    (rewritten, stubs)
}

/// Whether `name` is a namespace something other than this block already
/// answers for: the prelude's own, one the runtime or an embedder declared, or
/// one the author wrote a `host` block for.
///
/// candela resolves a namespace against its first block of that name, so a
/// second block under a taken name declares functions no call reaches, and the
/// stubs bound for it would sit over bindings the app does use.
fn spoken_for(source: &str, name: &str) -> bool {
    name == HOST_NAMESPACE || declares_namespace(source, name)
}

/// Bind `stubs` on `sink`, so the calls a check compiles reach a closure
/// instead of a symbol.
pub(crate) fn register_stubs<S: HostFnSink>(sink: &mut S, stubs: Vec<Stub>) {
    for stub in stubs {
        let answer = zero_value(&stub.ret);
        sink.register_host_fn_typed(
            &stub.namespace,
            &stub.name,
            stub.args,
            stub.ret,
            move |_args: &[Value]| -> Result<Value, HostError> { Ok(answer.clone()) },
        );
    }
}

/// Every line of `source` with the byte offset it starts at, newline dropped.
fn lines_with_offsets(source: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let text = line.strip_suffix('\n').unwrap_or(line);
        let text = text.strip_suffix('\r').unwrap_or(text);
        out.push((offset, text));
        offset += line.len();
    }
    out
}

/// Where the keyword sits in `line` and the library it names, for a line that
/// opens a block a check can read as a `host` block.
///
/// The library has to be named the way candela's own namespace is spelled: a
/// bare name, which is both the search name and the namespace a call writes.
/// A path form names the library one way and the namespace another, and the
/// block stays a `dylib` block.
fn opener(line: &str) -> Option<(usize, &str)> {
    let code = code_of(line);
    let at = code.find(DYLIB)?;
    if !code[..at].trim().is_empty() {
        return None;
    }
    let after = &code[at + DYLIB.len()..];
    if !after.starts_with(char::is_whitespace) && !after.starts_with('"') {
        return None;
    }
    let rest = after.trim_start().strip_prefix('"')?;
    let (name, rest) = rest.split_once('"')?;
    if rest.trim() != "{" {
        return None;
    }
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((at, name))
}

/// The declarations between `start` and the line closing the block, and that
/// line's index.
///
/// `None` for a block that runs off the end of the source or declares anything
/// this module cannot bind.
fn block(lines: &[(usize, &str)], start: usize, namespace: &str) -> Option<(usize, Vec<Stub>)> {
    let mut declared = Vec::new();
    for (i, (_, line)) in lines.iter().copied().enumerate().skip(start) {
        let code = code_of(line).trim();
        if code.is_empty() {
            continue;
        }
        if code == "}" {
            return Some((i, declared));
        }
        declared.push(declaration(code, namespace)?);
    }
    None
}

/// One declaration line, e.g. `string md_class(string);` or `md_log(string);`.
fn declaration(code: &str, namespace: &str) -> Option<Stub> {
    let decl = code.strip_suffix(';')?;
    let open = decl.find('(')?;
    let close = decl.rfind(')')?;
    if close < open || !decl[close + 1..].trim().is_empty() {
        return None;
    }
    let (ret, name) = head(decl[..open].trim_end())?;
    let list = decl[open + 1..close].trim();
    let mut args = Vec::new();
    if !list.is_empty() {
        for arg in list.split(',') {
            args.push(host_type(arg.trim())?);
        }
    }
    Some(Stub {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        args,
        ret,
    })
}

/// The return type and the function name out of what stands before the
/// argument list. A declaration with no return type returns `null`.
///
/// The name ends where the last character that cannot be in one does, over
/// every character rather than over the ASCII ones: a name candela will not
/// accept is carried whole, so the diagnostic names what the author wrote
/// instead of a tail of it.
fn head(text: &str) -> Option<(HostType, &str)> {
    let at = text
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map_or(0, |i| i + char_width(text, i));
    let name = &text[at..];
    if name.is_empty() || name.starts_with(char::is_numeric) {
        return None;
    }
    let ret = text[..at].trim();
    if ret.is_empty() {
        return Some((HostType::Unit, name));
    }
    Some((host_type(ret)?, name))
}

/// The byte width of the character at `i` in `text`.
fn char_width(text: &str, i: usize) -> usize {
    text[i..].chars().next().map_or(1, char::len_utf8)
}

/// The host type a declared type spells, or `None` for one that does not cross
/// the host boundary.
fn host_type(name: &str) -> Option<HostType> {
    Some(match name {
        "int" => HostType::Int,
        "float" => HostType::Float,
        "bool" => HostType::Bool,
        "string" => HostType::String,
        "null" => HostType::Unit,
        _ => return None,
    })
}

/// What a stub answers with: the zero value of the type it is declared to
/// return, which a check compiles against and never reads.
fn zero_value(ty: &HostType) -> Value {
    match ty {
        HostType::Int => Value::Int(0),
        HostType::Float => Value::Float(0.0),
        HostType::Bool => Value::Bool(false),
        HostType::String => Value::String(String::new()),
        // `host_type` names no other type, so nothing else is declared here.
        HostType::Unit | HostType::Array(_) | HostType::Map(_) | HostType::Struct(_) => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES: &str = "dylib \"md\" {\n    string md_class(string);\n    string md_text(string);\n}\n\nfn main() {}\n";

    #[test]
    fn a_bare_name_block_is_read_as_a_host_block() {
        let (text, stubs) = as_host_blocks(NOTES);
        let text = text.expect("the block is rewritten");
        assert!(text.starts_with("host  \"md\" {"));
        assert_eq!(text.len(), NOTES.len(), "every byte offset is preserved");
        assert_eq!(stubs.len(), 2);
        assert_eq!(stubs[0].namespace, "md");
        assert_eq!(stubs[0].name, "md_class");
        assert_eq!(stubs[0].args, vec![HostType::String]);
        assert_eq!(stubs[0].ret, HostType::String);
    }

    #[test]
    fn a_source_with_no_block_is_left_alone() {
        let (text, stubs) = as_host_blocks("fn main() {}\n");
        assert!(text.is_none());
        assert!(stubs.is_empty());
    }

    #[test]
    fn a_library_named_by_path_keeps_its_block() {
        // candela derives the namespace from the file name, not from the text
        // in the source, so the two do not swap.
        let (text, stubs) = as_host_blocks("dylib \"lib/libmd.so\" {\n    int f();\n}\n");
        assert!(text.is_none());
        assert!(stubs.is_empty());
    }

    #[test]
    fn a_type_that_cannot_cross_the_boundary_keeps_its_block() {
        let (text, _) = as_host_blocks("dylib \"md\" {\n    Node* parse(string);\n}\n");
        assert!(text.is_none());
    }

    #[test]
    fn an_unclosed_block_keeps_its_block() {
        let (text, _) = as_host_blocks("dylib \"md\" {\n    int f();\n");
        assert!(text.is_none());
    }

    #[test]
    fn the_keyword_is_read_as_a_statement_not_as_prose() {
        assert!(opener("// dylib \"md\" {").is_none());
        assert!(opener("let s = \"dylib \\\"md\\\" {\";").is_none());
        assert!(opener("    dylib \"md\" {").is_some());
    }

    #[test]
    fn a_declaration_with_no_return_type_returns_null() {
        let stub = declaration("md_log(string, int);", "md").expect("reads");
        assert_eq!(stub.name, "md_log");
        assert_eq!(stub.args, vec![HostType::String, HostType::Int]);
        assert_eq!(stub.ret, HostType::Unit);
    }

    #[test]
    fn a_declaration_with_no_arguments_reads() {
        let stub = declaration("int md_version();", "md").expect("reads");
        assert!(stub.args.is_empty());
        assert_eq!(stub.ret, HostType::Int);
    }

    #[test]
    fn a_variadic_declaration_is_not_a_dylib_declaration() {
        assert!(declaration("any log(...);", "md").is_none());
    }

    #[test]
    fn a_stub_answers_the_zero_of_its_type() {
        assert_eq!(zero_value(&HostType::Int), Value::Int(0));
        assert_eq!(zero_value(&HostType::String), Value::String(String::new()));
        assert_eq!(zero_value(&HostType::Unit), Value::Null);
    }

    #[test]
    fn a_namespace_the_text_already_declares_keeps_its_block() {
        // An embedder's block, the runtime's own, or one the author wrote:
        // candela resolves a namespace against its first block, so rewriting
        // this one would bind stubs the app's own calls never reach.
        let src = "host \"native\" { print(string); }\ndylib \"native\" {\n    int f();\n}\n";
        let (text, stubs) = as_host_blocks(src);
        assert!(text.is_none());
        assert!(stubs.is_empty());
    }

    #[test]
    fn the_prelude_namespace_is_never_taken_over() {
        // Even with no prelude in the text, `lumen` is the namespace the host
        // binds its own builtins under.
        let (text, _) = as_host_blocks("dylib \"lumen\" {\n    int f();\n}\n");
        assert!(text.is_none());
    }

    #[test]
    fn a_second_block_of_one_name_keeps_its_block() {
        let src = "dylib \"a\" {\n    int f();\n}\ndylib \"a\" {\n    int g();\n}\n";
        let (text, stubs) = as_host_blocks(src);
        let text = text.expect("the first block is rewritten");
        assert_eq!(text.matches("host  \"").count(), 1);
        assert_eq!(stubs.len(), 1);
    }

    #[test]
    fn a_name_outside_ascii_is_read_whole() {
        // candela's identifiers are ASCII, so this declaration is not one it
        // accepts; what matters is that the name is not cut short, which would
        // bind a stub under a name nothing declared and report that instead of
        // the line the author wrote.
        let stub = declaration("int h\u{e9}llo(string);", "md").expect("reads");
        assert_eq!(stub.name, "h\u{e9}llo");
        assert_eq!(stub.ret, HostType::Int);
    }

    #[test]
    fn two_blocks_both_rewrite() {
        let src = "dylib \"a\" {\n    int f();\n}\ndylib \"b\" {\n    int g();\n}\n";
        let (text, stubs) = as_host_blocks(src);
        let text = text.expect("both blocks are rewritten");
        assert_eq!(text.matches("host  \"").count(), 2);
        assert_eq!(text.len(), src.len());
        assert_eq!(stubs.len(), 2);
    }
}
