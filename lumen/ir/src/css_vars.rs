//! CSS `var(--name [, fallback])` resolver shared by `parser_css::apply_to_element` and `run::load_ir`.

use std::collections::HashMap;

/// Recursively replaces every `var(--name)` and `var(--name, fallback)` substring in `src` with the matching entry from `vars`.
///
/// - Returns `Err` for a missing variable with no fallback.
/// - Limits recursion to depth 16; cyclic chains return an error.
/// - Prepends `error_prefix` to any returned error message.
pub fn resolve(
    src: &str,
    vars: &HashMap<String, String>,
    error_prefix: &str,
) -> Result<String, String> {
    resolve_inner(src, vars, error_prefix, 0)
}

fn resolve_inner(
    src: &str,
    vars: &HashMap<String, String>,
    err_prefix: &str,
    depth: u32,
) -> Result<String, String> {
    if depth > 16 {
        return Err(format!("{err_prefix}var() recursion depth exceeded"));
    }
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(idx) = rest.find("var(") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 4..];
        let close = find_matching_paren(after)
            .ok_or_else(|| format!("{err_prefix}unterminated var( call"))?;
        let inner = &after[..close];
        rest = &after[close + 1..];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let key = name
            .strip_prefix("--")
            .ok_or_else(|| format!("{err_prefix}var() name must start with '--': '{name}'"))?;
        let resolved = match vars.get(key) {
            Some(v) => v.clone(),
            None => match fallback {
                Some(f) => f.to_string(),
                None => return Err(format!("{err_prefix}unknown CSS variable '--{key}'")),
            },
        };
        let nested = resolve_inner(&resolved, vars, err_prefix, depth + 1)?;
        out.push_str(&nested);
    }
    out.push_str(rest);
    Ok(out)
}

/// Outcome of [`resolve_lenient`]: the substituted text plus a human-readable
/// message for every `var()` call that could not resolve, in the order
/// encountered. `warnings` is empty when every call resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LenientResolution {
    /// `src` with every resolvable `var()` call substituted. A call that
    /// could not resolve is replaced with an empty string rather than left
    /// as literal `var(...)` text or aborting the rest of the document.
    pub output: String,
    /// One entry per `var()` call that could not resolve.
    pub warnings: Vec<String>,
}

/// Same substitution as [`resolve`], but never fails: an unresolvable
/// `var()` call (unknown name with no fallback, unterminated call, or a
/// recursion/cycle depth over 16) degrades to an empty string and records a
/// message in [`LenientResolution::warnings`], instead of aborting the rest
/// of `src`.
///
/// This mirrors how the CSS cascade (`lumen_ir::css::apply_to_element`)
/// treats an unresolvable `var()` in a stylesheet declaration: the one
/// declaration is dropped and a [`crate::css::CssWarning`] is recorded, but
/// the rest of the stylesheet still applies. `resolve` (strict) has no
/// per-declaration boundary to drop at when it runs over a whole document
/// (e.g. markup text carrying many attributes) rather than one declaration
/// value, so this variant drops per-*call* instead and never returns `Err`.
pub fn resolve_lenient(src: &str, vars: &HashMap<String, String>) -> LenientResolution {
    let mut warnings = Vec::new();
    let output = resolve_lenient_inner(src, vars, 0, &mut warnings);
    LenientResolution { output, warnings }
}

fn resolve_lenient_inner(
    src: &str,
    vars: &HashMap<String, String>,
    depth: u32,
    warnings: &mut Vec<String>,
) -> String {
    if depth > 16 {
        warnings.push("var() recursion depth exceeded".to_string());
        return String::new();
    }
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(idx) = rest.find("var(") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 4..];
        let Some(close) = find_matching_paren(after) else {
            warnings.push("unterminated var( call".to_string());
            // No matching close paren to resync on - stop scanning rather
            // than misinterpret the remainder as plain text.
            rest = "";
            break;
        };
        let inner = &after[..close];
        rest = &after[close + 1..];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let Some(key) = name.strip_prefix("--") else {
            warnings.push(format!("var() name must start with '--': '{name}'"));
            continue;
        };
        let resolved = match vars.get(key) {
            Some(v) => v.clone(),
            None => match fallback {
                Some(f) => f.to_string(),
                None => {
                    warnings.push(format!("unknown CSS variable '--{key}'"));
                    continue;
                }
            },
        };
        let nested = resolve_lenient_inner(&resolved, vars, depth + 1, warnings);
        out.push_str(&nested);
    }
    out.push_str(rest);
    out
}

/// Returns the byte index of the `)` that closes an implicitly opened `(` preceding `s`.
/// Tracks parenthesis depth, so nested calls such as `rgb(1,2,3)` inside a fallback are handled.
fn find_matching_paren(s: &str) -> Option<usize> {
    let mut depth: i32 = 1;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
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

    fn vars() -> HashMap<String, String> {
        let mut v = HashMap::new();
        v.insert("primary".into(), "#ff0000".into());
        v.insert("accent".into(), "var(--primary)".into());
        v
    }

    #[test]
    fn resolves_simple_var() {
        assert_eq!(resolve("var(--primary)", &vars(), "").unwrap(), "#ff0000");
    }

    #[test]
    fn resolves_nested_var() {
        assert_eq!(resolve("var(--accent)", &vars(), "").unwrap(), "#ff0000");
    }

    #[test]
    fn fallback_used_when_missing() {
        assert_eq!(
            resolve("var(--missing, #00ff00)", &vars(), "").unwrap(),
            "#00ff00"
        );
    }

    #[test]
    fn errors_on_unknown_without_fallback() {
        assert!(resolve("var(--missing)", &vars(), "").is_err());
    }

    #[test]
    fn handles_nested_parens_in_fallback() {
        let v = HashMap::new();
        assert_eq!(
            resolve("var(--missing, rgb(1, 2, 3))", &v, "").unwrap(),
            "rgb(1, 2, 3)"
        );
    }

    #[test]
    fn lenient_resolves_known_vars_with_no_warnings() {
        let r = resolve_lenient("<tile bg=\"var(--primary)\"/>", &vars());
        assert_eq!(r.output, "<tile bg=\"#ff0000\"/>");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn lenient_degrades_unknown_var_instead_of_failing() {
        let r = resolve_lenient("<tile bg=\"var(--missing)\"/>", &vars());
        // The unresolved call drops to empty text - the document keeps
        // parsing instead of the whole load aborting.
        assert_eq!(r.output, "<tile bg=\"\"/>");
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("--missing"));
    }

    #[test]
    fn lenient_keeps_resolving_after_an_unknown_var() {
        // One bad var() must not poison the rest of the document, matching
        // the CSS cascade's "drop just this declaration" behavior.
        let r = resolve_lenient("a=var(--missing) b=var(--primary)", &vars());
        assert_eq!(r.output, "a= b=#ff0000");
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn lenient_reports_cyclic_var_instead_of_hanging() {
        let mut v = HashMap::new();
        v.insert("a".into(), "var(--b)".into());
        v.insert("b".into(), "var(--a)".into());
        let r = resolve_lenient("var(--a)", &v);
        assert_eq!(r.output, "");
        assert!(!r.warnings.is_empty());
        assert!(r.warnings.last().unwrap().contains("recursion"));
    }
}
