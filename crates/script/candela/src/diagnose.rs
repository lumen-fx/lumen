//! Naming the symbol a candela `unknown_namespace` diagnostic failed on.
//!
//! candela resolves a namespaced path by walking its compile-time namespace
//! tree, and a `host "<ns>" { .. }` block is not a node in that tree: it binds
//! a dynamic-library entry instead. A call to a name a declared namespace does
//! not have therefore falls through to the tree walk, which reports the
//! namespace as invalid. That points an author at the import line when the
//! import is fine and the function is what moved.
//!
//! The diagnostic carries the byte span of the path text, so the host reads
//! `lumen::read_file` back out of the source the compiler saw and says which
//! half is missing. Nothing is parsed out of candela's message.

use candela_vm::Diagnostic;

use crate::host_fns::HOST_NAMESPACE;
use crate::prelude::{PRELUDE_MODULE, PreparedSource, code_only, declares_namespace};

/// The code candela stamps on a namespace path that does not resolve.
const UNKNOWN_NAMESPACE: &str = "unknown_namespace";

/// A message naming the symbol `d` failed to resolve, or `None` when candela's
/// own message is the one to report.
///
/// `prepared` is the text handed to the compiler and `uri` the name it was
/// compiled under. A diagnostic against any other file came from a source this
/// host never assembled (an author's own `import "other.cdl";`), so its span
/// indexes text that is not here and the message is left alone.
pub(crate) fn explain(prepared: &PreparedSource, uri: &str, d: &Diagnostic) -> Option<String> {
    if d.code != UNKNOWN_NAMESPACE || d.filename != uri {
        return None;
    }
    let path = prepared.text.get(d.span.start..d.span.end)?.trim();
    let (ns, name) = path.rsplit_once("::")?;

    // The builtin namespace is asked of the prepared source rather than
    // searched for: the host declares a block under it for an embedder's own
    // registrations, so its presence does not mean the surface is in.
    let declared = if ns == HOST_NAMESPACE {
        prepared.declares_builtins
    } else {
        declares_namespace(&prepared.text, ns)
    };

    // Every scan below reads the block a namespace opens, so it reads the
    // code without its comments: an app that describes a block it does not
    // write must not be read as writing it.
    let code = code_only(&prepared.text);

    if declared {
        let mut message = format!("the `{ns}` namespace has no `{name}` (called as `{path}`)");
        if let Some(other) = elsewhere(&code, ns, name) {
            message.push_str(&format!("; `{other}::{name}` exists"));
        }
        return Some(message);
    }

    if ns == HOST_NAMESPACE {
        return Some(format!(
            "{path}: the `{ns}` builtins are not declared here; add `import \"{PRELUDE_MODULE}\";`"
        ));
    }

    let mut message = format!("no `{ns}` namespace is declared here (called as `{path}`)");
    let elsewhere = declared_namespaces(&code);
    if !elsewhere.is_empty() {
        let list: Vec<String> = elsewhere.iter().map(|ns| format!("`{ns}`")).collect();
        message.push_str(&format!("; declared here: {}", list.join(", ")));
    }
    Some(message)
}

/// A declared namespace other than `ns` that declares `name`.
///
/// `code` is the prepared text with its comments dropped, as every scan here
/// takes.
///
/// The lookup is exact: `lumen::data_dir` points at `files::data_dir` because
/// the bare names match. A near miss is candela's to suggest - it owns the
/// closest-name suggester the resolver would reach with the namespace found.
fn elsewhere(code: &str, ns: &str, name: &str) -> Option<String> {
    declared_namespaces(code)
        .into_iter()
        .find(|other| other != ns && declared_names(code, other).iter().any(|d| d == name))
}

/// Every namespace a `host "<ns>" { .. }` block in `code` opens, first
/// occurrence first.
///
/// A name that opens no block is a mention rather than a declaration, so it is
/// left out: [`block_body`] is what decides.
fn declared_namespaces(code: &str) -> Vec<String> {
    const OPENER: &str = "host \"";
    let mut out: Vec<String> = Vec::new();
    let mut rest = code;
    while let Some(at) = rest.find(OPENER) {
        rest = &rest[at + OPENER.len()..];
        let Some(end) = rest.find('"') else { break };
        let ns = &rest[..end];
        if !out.iter().any(|seen| seen == ns) && block_body(code, ns).is_some() {
            out.push(ns.to_owned());
        }
        rest = &rest[end + 1..];
    }
    out
}

/// The names the first `host "<ns>" { .. }` block declares.
///
/// candela resolves a namespace against its first block, so that block is the
/// surface the call was checked against. Reading it out of the prepared text
/// keeps the answer to "what does this namespace have" from disagreeing with
/// what the compiler saw.
fn declared_names(code: &str, ns: &str) -> Vec<String> {
    let Some(body) = block_body(code, ns) else {
        return Vec::new();
    };
    body.split(';')
        .filter_map(|decl| {
            let head = decl.split_once('(')?.0.trim_end();
            let name: String = head
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then(|| name.chars().rev().collect())
        })
        .collect()
}

/// The text between the braces of the first `host "<ns>" { .. }` block.
///
/// The walk counts braces because a declaration can carry a map type
/// (`{string: float}`) whose closing brace is not the block's.
fn block_body<'a>(code: &'a str, ns: &str) -> Option<&'a str> {
    let opener = format!("host \"{ns}\"");
    let at = code.find(&opener)? + opener.len();
    let rest = &code[at..];
    let open = rest.find('{')?;
    // Only whitespace stands between the namespace and its block; anything
    // else means the match was a mention rather than a declaration.
    if !rest[..open].trim().is_empty() {
        return None;
    }
    let mut depth = 0usize;
    for (i, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "host \"lumen\" { string signal_get(string); {string: float} table(); }\n\
                        host \"files\" { string read(string); string data_dir(); }\n";

    #[test]
    fn every_declared_namespace_is_listed() {
        assert_eq!(declared_namespaces(TEXT), vec!["lumen", "files"]);
    }

    #[test]
    fn a_map_return_type_does_not_close_the_block() {
        assert_eq!(declared_names(TEXT, "lumen"), vec!["signal_get", "table"]);
    }

    /// A source that describes a block in prose ahead of writing one, as the
    /// smoke fixture does.
    const COMMENTED: &str = "// declare them with `host \"files\" { ... }`\n\
                             host \"lumen\" { string signal_get(string); }\n\
                             host \"files\" { string data_dir(); }\n";

    #[test]
    fn a_block_named_in_a_comment_does_not_stand_in_for_the_real_one() {
        let code = code_only(COMMENTED);
        assert_eq!(declared_names(&code, "files"), vec!["data_dir"]);
        assert_eq!(
            elsewhere(&code, "lumen", "data_dir"),
            Some("files".to_owned())
        );
    }

    #[test]
    fn a_bare_name_in_another_namespace_is_found() {
        assert_eq!(
            elsewhere(TEXT, "lumen", "data_dir"),
            Some("files".to_owned())
        );
        assert_eq!(elsewhere(TEXT, "lumen", "signal_get"), None);
    }
}
