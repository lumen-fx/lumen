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
}
