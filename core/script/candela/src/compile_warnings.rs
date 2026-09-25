//! The warnings a compile raised, as lines a build tool prints.
//!
//! candela raises a warning where it finds something that does not stop the
//! build, and left alone it prints each one to stderr itself. A build tool
//! has its own channel for what it warns about, which is what `--strict`
//! counts, so the host collects candela's warnings and hands them back as text
//! for that channel instead.
//!
//! candela raises one `unannotated_host_parameter` warning per bare parameter,
//! all at the function's name. An author fixes a function, not a parameter, so
//! the warnings one function raised become one line naming every bare
//! parameter it takes. The names are read from the source at the function's
//! span, the way [`crate::diagnose`] reads a path back; nothing is parsed out
//! of candela's message.

use std::collections::HashSet;
use std::ops::Range;

use candela_vm::Diagnostic;

use crate::prelude::PreparedSource;

/// The code candela stamps on a parameter a host passes as `any` because the
/// function does not say what it takes.
const UNANNOTATED_HOST_PARAMETER: &str = "unannotated_host_parameter";

/// One line per thing the compile warned about, in the order candela raised
/// them, each prefixed with where it is in the author's own source.
///
/// `prepared` is the text handed to the compiler and `uri` the name it was
/// compiled under.
pub(crate) fn relay(prepared: &PreparedSource, uri: &str, warnings: &[Diagnostic]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen_functions: HashSet<Range<usize>> = HashSet::new();
    let mut seen_lines: HashSet<String> = HashSet::new();
    for warning in warnings {
        let message = if warning.code == UNANNOTATED_HOST_PARAMETER && warning.filename == uri {
            if !seen_functions.insert(warning.span.clone()) {
                continue;
            }
            unannotated(prepared, &warning.span).unwrap_or_else(|| warning.message.clone())
        } else {
            warning.message.clone()
        };
        let line = format!("{}: {message}", location(prepared, uri, warning));
        if seen_lines.insert(line.clone()) {
            lines.push(line);
        }
    }
    lines
}

/// Where `warning` points, as `uri:line:col` in the author's source, or the
/// plugin that owns the line when it sits in a plugin's wrapper.
fn location(prepared: &PreparedSource, uri: &str, warning: &Diagnostic) -> String {
    if warning.filename != uri {
        return warning.filename.clone();
    }
    let at = prepared.locate(warning.span.start);
    match at.wrapper {
        Some(ns) => format!("{uri} (plugin namespace `{ns}`)"),
        None => format!("{uri}:{}:{}", at.line, at.col),
    }
}

/// The one line for a function with bare parameters, or `None` when the
/// source at `span` does not read as a function declaration.
fn unannotated(prepared: &PreparedSource, span: &Range<usize>) -> Option<String> {
    let name = prepared.text.get(span.clone())?.trim();
    let params = bare_parameters(prepared.text.get(span.end..)?)?;
    let (listed, pronoun, advice) = match params.as_slice() {
        [] => return None,
        [one] => (format!("`{one}`"), "it", "annotate it"),
        many => (
            many.iter()
                .map(|param| format!("`{param}`"))
                .collect::<Vec<_>>()
                .join(", "),
            "them",
            "annotate each",
        ),
    };
    Some(format!(
        "`{name}` takes {listed} with no type, so a host calling it passes {pronoun} as `any`; \
         {advice} with the type the host passes"
    ))
}

/// The parameters with no annotation in the list that opens `rest`, which is
/// the source right after a function's name.
///
/// A comma inside an annotation's brackets (`Map<string, int>`) separates no
/// parameters, so the list is split at depth zero only.
fn bare_parameters(rest: &str) -> Option<Vec<String>> {
    let rest = rest.trim_start();
    // A generic function names its type parameters before the list.
    let rest = match rest.strip_prefix('<') {
        Some(generic) => &generic[closing(generic, '<', '>')? + 1..],
        None => rest,
    }
    .trim_start()
    .strip_prefix('(')?;
    let list = &rest[..closing(rest, '(', ')')?];
    let mut params = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (at, ch) in list.char_indices() {
        match ch {
            '(' | '<' | '[' | '{' => depth += 1,
            ')' | '>' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                params.push(&list[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    params.push(&list[start..]);
    Some(
        params
            .into_iter()
            .map(str::trim)
            .filter(|param| !param.is_empty() && !param.contains(':'))
            .map(str::to_string)
            .collect(),
    )
}

/// The byte offset of the `close` that ends a group whose `open` was already
/// consumed, counting nested pairs.
fn closing(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (at, ch) in text.char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            if depth == 0 {
                return Some(at);
            }
            depth -= 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_is_read_past_annotations_and_their_brackets() {
        assert_eq!(
            bare_parameters("(name, count: int, m: Map<string, int>, last) {"),
            Some(vec!["name".to_string(), "last".to_string()])
        );
    }

    #[test]
    fn a_generic_function_is_read_past_its_type_parameters() {
        assert_eq!(
            bare_parameters("<T>(value, other: T) {"),
            Some(vec!["value".to_string()])
        );
    }

    #[test]
    fn a_function_that_takes_nothing_has_no_bare_parameter() {
        assert_eq!(bare_parameters("() {"), Some(Vec::new()));
    }

    #[test]
    fn text_that_opens_no_list_is_not_read() {
        assert_eq!(bare_parameters(" = 3;"), None);
    }
}
