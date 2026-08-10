//! CSS subset parser + cascade application.
//!
//! Grammar accepted (a documented subset of CSS Selectors-4 +
//! Cascade-5 + Media-Queries-5):
//!
//! ```text
//! stylesheet  := (at_rule | rule)*
//! at_rule     := "@media" media_query "{" rule* "}"
//! rule        := selector_list "{" declaration* "}"
//! selector_list := compound (combinator compound)* ("," compound (combinator compound)*)*
//! combinator  := " " | ">" | "+" | "~"
//! compound    := tag? ("." class | "#" id | ":" pseudo_class)*
//! pseudo_class:= "hover" | "focus" | "focus-visible" | "active"
//!              | "disabled" | "checked" | "selected"
//!              | "first-child" | "last-child"
//!              | "only-child" | "empty" | "root"
//!              | "nth-child(" an_plus_b ")"
//!              | "is(" selector_list ")"
//!              | "where(" selector_list ")"
//!              | "not(" selector_list ")"
//! declaration := ident ":" value ("!important")? ";"
//! ```
//!
//! Comments `/* ... */` are stripped. Tokenisation goes through a
//! hand-rolled scanner that mirrors the relevant subset of W3C
//! Syntax-3. The scanner is what the parser uses; the supported
//! selector and value coverage is narrow enough that a general CSS
//! tokeniser would cost more than it saves.
//!
//! ## Cascade ordering
//!
//! Per CSS Cascade-5 section 6.4: origin -> importance -> specificity -> source
//! order, and **later** rules win at equal weight (section 6.4.4). Within
//! `apply_css`, HTML inline attrs (origin: inline) beat CSS attrs
//! (origin: user/UA) - preserving the long-standing rule that
//! `<tile width="50px"/>` overrides `.t { width: 100px }`. Inline
//! `!important` is not authorable; user `!important` lifts a CSS
//! declaration above its origin's normal block.

// The CSS AST + Cascade-5 application moved to `lumen-ir`; re-export them so
// `lumenc::parser_css::{Stylesheet, apply_css, CssWarning, MediaContext, ...}`
// paths - and the tests below - keep resolving unchanged. `parse_css` (the
// hand-rolled front-end that PRODUCES a `Stylesheet`) stays here.
pub use lumen_ir::css::*;

use crate::layout_ir::ParseError;
// The cascade tests below (kept here because they drive the `parse_html` /
// `parse_css` front-end) reach these layout_ir types unqualified via
// `use super::*`.
#[cfg(test)]
use crate::layout_ir::{Attributes, DisplaySpec, Element, FlexAlign, LayoutIR, TrackSizeSpec};

// Top-level parser
// ---------------------------------------------------------------------------

/// Maximum nesting depth for recursive CSS constructs (`@media` blocks and
/// `:not()`/`:is()`/`:where()` selector lists). Past this, the parser returns
/// a `ParseError` rather than recursing further - the recursion is unbounded
/// on hostile input and a stack overflow is a SIGSEGV, not a catchable panic.
const MAX_NEST_DEPTH: u32 = 32;

/// Parse a CSS source string into a [`Stylesheet`].
pub fn parse_css(src: &str) -> Result<Stylesheet, ParseError> {
    let cleaned = strip_comments(src);
    let mut input = cleaned.as_str();
    let mut rules = Vec::new();
    let mut next_order = 0usize;
    parse_rule_list(&mut input, None, &mut rules, &mut next_order, 0)?;
    Ok(Stylesheet { rules })
}

fn parse_rule_list(
    input: &mut &str,
    media: Option<&MediaQuery>,
    out: &mut Vec<Rule>,
    next_order: &mut usize,
    depth: u32,
) -> Result<(), ParseError> {
    if depth > MAX_NEST_DEPTH {
        return Err(ParseError::Xml(format!(
            "css: @media nesting exceeded depth {MAX_NEST_DEPTH}"
        )));
    }
    loop {
        *input = input.trim_start();
        if input.is_empty() {
            break;
        }
        if let Some(rest) = input.strip_prefix("@media") {
            let mut after = rest.trim_start();
            // Find the `{` that opens the @media body.
            let brace = after
                .find('{')
                .ok_or_else(|| ParseError::Xml("css: @media missing '{'".into()))?;
            let prelude = after[..brace].trim();
            after = &after[brace + 1..];
            let mq = parse_media_query(prelude).map_err(ParseError::Xml)?;
            // Slice the brace-balanced body.
            let close = find_matching_brace(after)
                .ok_or_else(|| ParseError::Xml("css: @media missing '}'".into()))?;
            let body = &after[..close];
            *input = &after[close + 1..];
            let mut body_str = body;
            parse_rule_list(&mut body_str, Some(&mq), out, next_order, depth + 1)?;
            continue;
        }
        if input.starts_with('@') {
            skip_at_rule(input)?;
            continue;
        }
        let rule = parse_rule(input, media, *next_order)?;
        *next_order += 1;
        out.push(rule);
    }
    Ok(())
}

/// Consume one at-rule that the cascade does not implement (`@keyframes`,
/// `@font-face`, `@import`, ...) and warn. A block at-rule loses its whole
/// brace-balanced body; a statement at-rule ends at its `;`. Skipping keeps
/// CSS error recovery: one unsupported rule must not take down the rest of
/// the stylesheet.
fn skip_at_rule(input: &mut &str) -> Result<(), ParseError> {
    let name: String = input[1..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let brace = input.find('{');
    let semi = input.find(';');
    match (brace, semi) {
        (Some(b), s) if s.is_none_or(|s| b < s) => {
            let after = &input[b + 1..];
            let close = find_matching_brace(after)
                .ok_or_else(|| ParseError::Xml(format!("css: @{name} missing '}}'")))?;
            *input = &after[close + 1..];
        }
        (_, Some(s)) => *input = &input[s + 1..],
        // No terminator left in the file: the at-rule runs to the end.
        _ => *input = "",
    }
    tracing::warn!(
        target: "lumenc::css",
        "@{name} is not supported - block skipped"
    );
    Ok(())
}

fn find_matching_brace(s: &str) -> Option<usize> {
    // Returns the byte index of the `}` that closes an already-opened
    // `{`. Tracks nested braces, brackets, parens, and string literals.
    let bytes = s.as_bytes();
    let mut depth: i32 = 1;
    let mut i = 0usize;
    let mut in_string: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match in_string {
            Some(q) => {
                if c == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if c == q {
                    in_string = None;
                }
            }
            None => match c {
                b'"' | b'\'' => in_string = Some(c),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

fn parse_rule(
    input: &mut &str,
    media: Option<&MediaQuery>,
    source_order: usize,
) -> Result<Rule, ParseError> {
    let brace = input
        .find('{')
        .ok_or_else(|| ParseError::Xml("css: rule missing '{'".into()))?;
    let selector_src = input[..brace].trim();
    let selectors = parse_selector_list_local(selector_src, 0).map_err(ParseError::Xml)?;
    let after = &input[brace + 1..];
    let close = find_matching_brace(after)
        .ok_or_else(|| ParseError::Xml("css: rule missing '}'".into()))?;
    let body = &after[..close];
    *input = &after[close + 1..];

    let mut declarations = Vec::new();
    for chunk in split_top_level_semicolons(body) {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let colon = chunk
            .find(':')
            .ok_or_else(|| ParseError::Xml(format!("css: declaration without ':' in '{chunk}'")))?;
        let name = chunk[..colon].trim().to_string();
        let mut value = chunk[colon + 1..].trim().to_string();
        let important = if let Some(stripped) = strip_trailing_important(&value) {
            value = stripped;
            true
        } else {
            false
        };
        declarations.push(Declaration {
            name,
            value,
            important,
        });
    }
    let shim = legacy_shim_from_selectors(&selectors);
    Ok(Rule {
        selectors,
        declarations,
        // Parser is origin-agnostic: every parsed rule defaults to the
        // author origin. The runtime re-tags the built-in skin sheet as
        // `Origin::UserAgent` before the combined cascade pass.
        origin: Origin::default(),
        source_order,
        media: media.cloned(),
        selector: shim,
    })
}

fn strip_trailing_important(value: &str) -> Option<String> {
    // Match `!<ws>important` at the tail, case-insensitive on the keyword.
    let trimmed = value.trim_end();
    let bang = trimmed.rfind('!')?;
    let tail = trimmed[bang + 1..].trim();
    if tail.eq_ignore_ascii_case("important") {
        Some(trimmed[..bang].trim_end().to_string())
    } else {
        None
    }
}

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            // Step past the closing `*/`. The legacy `min`-clamped
            // expression is replaced with a saturating bump.
            i = (i + 2).min(bytes.len());
            continue;
        }
        // `i` sits on a UTF-8 char boundary here (comment markers `/`, `*`
        // are ASCII and can never appear as a continuation byte), so copy
        // the whole scalar. The old `bytes[i] as char` Latin-1 cast shredded
        // every multibyte code point (e.g. `content: "caf\u{e9}"`).
        let ch = src[i..]
            .chars()
            .next()
            .expect("i is at a UTF-8 char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn split_top_level_semicolons(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string: Option<u8> = None;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match in_string {
            Some(q) => {
                // Forward-skip escapes (matches `find_matching_brace`): the
                // backward `bytes[i-1] != '\\'` test mis-handled an escaped
                // backslash (`"\\"`), treating the closing quote as escaped.
                if c == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if c == q {
                    in_string = None;
                }
            }
            None => match c {
                b'"' | b'\'' => in_string = Some(c),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b';' if depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

// ---------------------------------------------------------------------------
// Selector parser (Selectors-4 subset)
// ---------------------------------------------------------------------------

fn parse_selector_list_local(src: &str, depth: u32) -> Result<Vec<SelectorBuf>, String> {
    if depth > MAX_NEST_DEPTH {
        return Err(format!(
            "css: selector nesting (:not/:is/:where) exceeded depth {MAX_NEST_DEPTH}"
        ));
    }
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return Err("css: empty selector".into());
    }
    let mut out = Vec::new();
    for piece in split_top_level_commas(trimmed) {
        let p = piece.trim();
        if p.is_empty() {
            return Err("css: empty selector in list".into());
        }
        out.push(parse_selector(p, depth)?);
    }
    Ok(out)
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

fn parse_selector(src: &str, depth: u32) -> Result<SelectorBuf, String> {
    let tokens = tokenize_selector(src)?;
    if tokens.is_empty() {
        return Err(format!("css: empty selector '{src}'"));
    }
    let mut chain: Vec<(Combinator, CompoundSelector)> = Vec::new();
    let mut pending = Combinator::Subject;
    let mut i = 0usize;
    loop {
        while matches!(tokens.get(i), Some(SelTok::Whitespace)) {
            i += 1;
        }
        if i >= tokens.len() {
            break;
        }
        let (compound, consumed) = read_compound(&tokens[i..], depth)?;
        if compound.is_empty() {
            return Err(format!("css: empty compound in '{src}'"));
        }
        chain.push((pending, compound));
        i += consumed;
        let mut ws_seen = false;
        while matches!(tokens.get(i), Some(SelTok::Whitespace)) {
            ws_seen = true;
            i += 1;
        }
        if i >= tokens.len() {
            break;
        }
        pending = match tokens.get(i) {
            Some(SelTok::ChildCombinator) => {
                i += 1;
                Combinator::Child
            }
            Some(SelTok::AdjacentSibling) => {
                i += 1;
                Combinator::AdjacentSibling
            }
            Some(SelTok::GeneralSibling) => {
                i += 1;
                Combinator::GeneralSibling
            }
            _ if ws_seen => Combinator::Descendant,
            _ => Combinator::Descendant,
        };
        while matches!(tokens.get(i), Some(SelTok::Whitespace)) {
            i += 1;
        }
    }
    if chain.is_empty() {
        return Err(format!("css: empty selector '{src}'"));
    }
    Ok(SelectorBuf { chain })
}

#[derive(Debug, Clone)]
enum SelTok {
    Whitespace,
    Tag(String),
    Class(String),
    Id(String),
    Pseudo(String, Option<String>),
    ChildCombinator,
    AdjacentSibling,
    GeneralSibling,
}

fn tokenize_selector(src: &str) -> Result<Vec<SelTok>, String> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => {
                out.push(SelTok::Whitespace);
                while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
                    i += 1;
                }
            }
            b'>' => {
                out.push(SelTok::ChildCombinator);
                i += 1;
            }
            b'+' => {
                out.push(SelTok::AdjacentSibling);
                i += 1;
            }
            b'~' => {
                out.push(SelTok::GeneralSibling);
                i += 1;
            }
            b'.' => {
                i += 1;
                let s = read_ident(bytes, &mut i)?;
                out.push(SelTok::Class(s));
            }
            b'#' => {
                i += 1;
                let s = read_ident(bytes, &mut i)?;
                out.push(SelTok::Id(s));
            }
            b':' => {
                i += 1;
                if i < bytes.len() && bytes[i] == b':' {
                    return Err(format!(
                        "css: pseudo-elements not supported (got '::') in '{src}'"
                    ));
                }
                let name = read_ident(bytes, &mut i)?;
                let args = if i < bytes.len() && bytes[i] == b'(' {
                    let close = find_matching_paren(&src[i..])
                        .ok_or_else(|| format!("css: unterminated '(' in pseudo ':{name}'"))?;
                    let inner = src[i + 1..i + close].to_string();
                    i += close + 1;
                    Some(inner)
                } else {
                    None
                };
                out.push(SelTok::Pseudo(name, args));
            }
            b'*' => {
                out.push(SelTok::Tag("*".into()));
                i += 1;
            }
            _ if is_ident_start(c) => {
                let s = read_ident(bytes, &mut i)?;
                out.push(SelTok::Tag(s));
            }
            other => {
                return Err(format!(
                    "css: unexpected char '{}' in selector '{src}'",
                    other as char
                ));
            }
        }
    }
    Ok(out)
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'-'
}

fn is_ident_cont(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

fn read_ident(bytes: &[u8], i: &mut usize) -> Result<String, String> {
    let start = *i;
    while *i < bytes.len() && is_ident_cont(bytes[*i]) {
        *i += 1;
    }
    if *i == start {
        return Err(format!("css: expected identifier at byte {start}"));
    }
    Ok(std::str::from_utf8(&bytes[start..*i])
        .map_err(|e| format!("css: non-utf8 selector: {e}"))?
        .to_string())
}

fn find_matching_paren(s: &str) -> Option<usize> {
    // `s` starts with `(`. Returns the byte index of the matching `)`.
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    for (i, &c) in bytes.iter().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => {
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

fn read_compound(toks: &[SelTok], depth: u32) -> Result<(CompoundSelector, usize), String> {
    let mut out = CompoundSelector::default();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            SelTok::Tag(t) => {
                if out.tag.is_some() || !out.classes.is_empty() || out.id.is_some() {
                    // Tag must be first per Selectors-4; ambiguous
                    // compound - bail.
                    break;
                }
                out.tag = if t == "*" { None } else { Some(t.clone()) };
                i += 1;
            }
            SelTok::Class(c) => {
                out.classes.push(c.clone());
                i += 1;
            }
            SelTok::Id(id) => {
                out.id = Some(id.clone());
                i += 1;
            }
            SelTok::Pseudo(name, args) => {
                let p = parse_pseudo(name, args.as_deref(), depth)?;
                out.pseudo_classes.push(p);
                i += 1;
            }
            _ => break,
        }
    }
    Ok((out, i))
}

fn parse_pseudo(name: &str, args: Option<&str>, depth: u32) -> Result<PseudoClass, String> {
    match name {
        "hover" => no_args(name, args).map(|_| PseudoClass::Hover),
        "focus" => no_args(name, args).map(|_| PseudoClass::Focus),
        "focus-visible" => no_args(name, args).map(|_| PseudoClass::FocusVisible),
        "active" => no_args(name, args).map(|_| PseudoClass::Active),
        "disabled" => no_args(name, args).map(|_| PseudoClass::Disabled),
        "checked" => no_args(name, args).map(|_| PseudoClass::Checked),
        "selected" => no_args(name, args).map(|_| PseudoClass::Selected),
        "drag-over" => no_args(name, args).map(|_| PseudoClass::DragOver),
        "root" => no_args(name, args).map(|_| PseudoClass::Root),
        "first-child" => no_args(name, args).map(|_| PseudoClass::FirstChild),
        "last-child" => no_args(name, args).map(|_| PseudoClass::LastChild),
        "only-child" => no_args(name, args).map(|_| PseudoClass::OnlyChild),
        "empty" => no_args(name, args).map(|_| PseudoClass::Empty),
        "nth-child" => {
            let a = args.ok_or_else(|| "css: :nth-child requires '(an+b)'".to_string())?;
            Ok(PseudoClass::NthChild(parse_anb(a)?))
        }
        "is" => {
            let a = args.ok_or_else(|| "css: :is requires '(selector-list)'".to_string())?;
            Ok(PseudoClass::Is(parse_selector_list_local(a, depth + 1)?))
        }
        "where" => {
            let a = args.ok_or_else(|| "css: :where requires '(selector-list)'".to_string())?;
            Ok(PseudoClass::Where(parse_selector_list_local(a, depth + 1)?))
        }
        "not" => {
            let a = args.ok_or_else(|| "css: :not requires '(selector-list)'".to_string())?;
            Ok(PseudoClass::Not(parse_selector_list_local(a, depth + 1)?))
        }
        other => Err(format!(
            "css: unknown pseudo-class ':{other}' (supported: :hover, :focus, :focus-visible, :active, :disabled, :checked, :selected, :drag-over, :root, :first-child, :last-child, :only-child, :empty, :nth-child, :is, :where, :not)"
        )),
    }
}

fn no_args(name: &str, args: Option<&str>) -> Result<(), String> {
    if args.is_some() {
        Err(format!("css: ':{name}' takes no arguments"))
    } else {
        Ok(())
    }
}

fn parse_anb(src: &str) -> Result<AnB, String> {
    let s = src.trim().to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Ok(AnB { a: 2, b: 1 }),
        "even" => return Ok(AnB { a: 2, b: 0 }),
        _ => {}
    }
    if let Some(idx) = s.find('n') {
        let a_part = s[..idx].trim();
        let b_part = s[idx + 1..].trim();
        let a = if a_part.is_empty() || a_part == "+" {
            1
        } else if a_part == "-" {
            -1
        } else {
            a_part
                .parse::<i32>()
                .map_err(|e| format!("css: bad :nth-child a-coefficient '{a_part}': {e}"))?
        };
        let b = if b_part.is_empty() {
            0
        } else {
            let bp = b_part.replace(' ', "");
            bp.parse::<i32>()
                .map_err(|e| format!("css: bad :nth-child b-coefficient '{b_part}': {e}"))?
        };
        Ok(AnB { a, b })
    } else {
        let b = s
            .parse::<i32>()
            .map_err(|e| format!("css: bad :nth-child '{s}': {e}"))?;
        Ok(AnB { a: 0, b })
    }
}

// ---------------------------------------------------------------------------
// Media-query parser
// ---------------------------------------------------------------------------

fn parse_media_query(src: &str) -> Result<MediaQuery, String> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return Ok(MediaQuery { features: vec![] });
    }
    let mut features = Vec::new();
    // Split on the `and` keyword surrounded by whitespace to avoid
    // matching it inside identifiers.
    for piece in split_on_and(trimmed) {
        let p = piece.trim();
        if p.is_empty() {
            continue;
        }
        let body = p
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .ok_or_else(|| format!("css: media feature must be '(...)' - got '{p}'"))?
            .trim();
        let (key, val) = match body.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (body, ""),
        };
        let feat = match key {
            "prefers-color-scheme" => MediaFeature::PrefersColorScheme(match val {
                "dark" => ColorSchemePreference::Dark,
                "light" => ColorSchemePreference::Light,
                "no-preference" => ColorSchemePreference::NoPreference,
                other => {
                    return Err(format!(
                        "css: prefers-color-scheme expects dark|light|no-preference, got '{other}'"
                    ));
                }
            }),
            "prefers-reduced-motion" => MediaFeature::PrefersReducedMotion(match val {
                "reduce" => MotionPreference::Reduce,
                "no-preference" | "" => MotionPreference::NoPreference,
                other => {
                    return Err(format!(
                        "css: prefers-reduced-motion expects reduce|no-preference, got '{other}'"
                    ));
                }
            }),
            "prefers-contrast" => MediaFeature::PrefersContrast(match val {
                "more" => ContrastPreference::More,
                "less" => ContrastPreference::Less,
                "custom" => ContrastPreference::Custom,
                "no-preference" | "" => ContrastPreference::NoPreference,
                other => {
                    return Err(format!(
                        "css: prefers-contrast expects more|less|custom|no-preference, got '{other}'"
                    ));
                }
            }),
            "min-width" => MediaFeature::MinWidth(parse_px(val)?),
            "max-width" => MediaFeature::MaxWidth(parse_px(val)?),
            "width" => MediaFeature::Width(parse_px(val)?),
            other => {
                return Err(format!(
                    "css: unsupported @media feature '{other}' (supported: prefers-color-scheme, prefers-reduced-motion, prefers-contrast, min-width, max-width, width)"
                ));
            }
        };
        features.push(feat);
    }
    Ok(MediaQuery { features })
}

fn split_on_and(s: &str) -> Vec<&str> {
    // We only honour the `and` keyword at top level - `or` and the
    // newer ` , ` list form aren't parsed yet.
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut depth: i32 = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && i + 4 < bytes.len() {
            let c_before = if i == 0 {
                b' '
            } else {
                bytes[i.saturating_sub(1)]
            };
            // "and" preceded and followed by whitespace.
            if c_before.is_ascii_whitespace()
                && bytes[i] == b'a'
                && bytes[i + 1] == b'n'
                && bytes[i + 2] == b'd'
                && bytes[i + 3].is_ascii_whitespace()
            {
                out.push(&s[start..i]);
                start = i + 3;
                i += 3;
                continue;
            }
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

fn parse_px(v: &str) -> Result<f32, String> {
    let s = v.trim().strip_suffix("px").unwrap_or(v.trim());
    s.trim()
        .parse::<f32>()
        .map_err(|e| format!("css: bad pixel value '{v}': {e}"))
}

// ---------------------------------------------------------------------------
// Back-compat shim
// ---------------------------------------------------------------------------

/// Build the back-compat [`LegacySelectorShim`] for a rule's selector list.
///
/// Surfaces the SUBJECT compound (rightmost) of the first selector - the
/// entity the rule targets - which the class-invalidation set + legacy
/// `extract_root_vars` callers read via `rule.selector.{tag,classes}`.
/// (Formerly `LegacySelectorShim::from_selectors`; moved here as a free fn
/// because the shim type now lives in `lumen-ir`.)
fn legacy_shim_from_selectors(sels: &[SelectorBuf]) -> LegacySelectorShim {
    let subject = sels.first().and_then(|s| s.chain.last()).map(|(_, c)| c);
    match subject {
        Some(c) => LegacySelectorShim {
            tag: c.tag.clone(),
            classes: c.classes.clone(),
        },
        None => LegacySelectorShim::default(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod cascade_tests {
    use super::*;
    use crate::parse_html;

    fn tile_with_class(cls: &str) -> LayoutIR {
        parse_html(&format!(r#"<root><tile class="{cls}" /></root>"#)).expect("html")
    }

    fn solid(bg: &crate::layout_ir::BgSpec) -> crate::layout_ir::Rgba {
        match bg {
            crate::layout_ir::BgSpec::Solid(c) => *c,
            _ => panic!("expected solid"),
        }
    }

    #[test]
    fn last_wins_for_repeated_property() {
        let mut ir = tile_with_class("t");
        let css = parse_css(".t { bg: #ff0000; } .t { bg: #00ff00; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(c.g > 0.99 && c.r < 0.01, "second .t rule wins");
    }

    #[test]
    fn higher_specificity_wins() {
        let mut ir = tile_with_class("btn primary");
        let css = parse_css(".btn { bg: #444444; } .btn.primary { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(c.r > 0.99, ".btn.primary (b=2) beats .btn (b=1)");
    }

    #[test]
    fn lower_specificity_loses_even_when_later() {
        let mut ir = tile_with_class("btn primary");
        let css = parse_css(".btn.primary { bg: #ff0000; } .btn { bg: #00ff00; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(
            c.r > 0.99,
            ".btn.primary (b=2) beats .btn (b=1) regardless of order"
        );
    }

    #[test]
    fn important_beats_normal_at_higher_specificity() {
        let mut ir = tile_with_class("t high");
        let css =
            parse_css(".t.high { bg: #ff0000; } .t { bg: #00ff00 !important; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(c.g > 0.99, "!important .t beats normal .t.high");
    }

    #[test]
    fn important_decl_beats_later_normal_decl() {
        // The important bg must win over a later normal bg rule.
        let mut ir = tile_with_class("a");
        let css = parse_css(".a { bg: #ff0000 !important; } .a { bg: #00ff00; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(c.r > 0.99, "!important bg beats a later normal bg");
    }

    #[test]
    fn normal_sibling_of_important_decl_does_not_beat_later_normal() {
        // `.a { bg: red !important; color: blue }` - the important `bg`
        // must not drag the normal sibling `color: blue` above a later
        // normal `color` rule. Per-declaration importance means the
        // later `.b` color wins.
        let mut ir = tile_with_class("a b");
        let css = parse_css(
            ".a { bg: #ff0000 !important; text-color: #0000ff; } \
             .b { text-color: #00ff00; }",
        )
        .expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let bg = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(bg.r > 0.99, "!important bg still wins");
        let col = ir.root.children[0].attrs.text_color.expect("text_color");
        assert!(
            col.g > 0.99 && col.b < 0.01,
            "later normal .b text-color beats the normal sibling of an important decl"
        );
    }

    #[test]
    fn text_color_inherits_two_levels() {
        let mut ir = parse_html(
            r#"<root><column class="outer"><column class="mid"><tile class="leaf" /></column></column></root>"#,
        )
        .expect("html");
        let css = parse_css(".outer { text-color: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let leaf = &ir.root.children[0].children[0].children[0];
        let c = leaf.attrs.text_color.expect("inherited text_color");
        assert!(
            c.r > 0.99 && c.g < 0.01,
            "text-color inherits down two levels to the leaf"
        );
    }

    #[test]
    fn child_rule_overrides_inherited_text_color() {
        let mut ir =
            parse_html(r#"<root><column class="outer"><tile class="leaf" /></column></root>"#)
                .expect("html");
        let css = parse_css(".outer { text-color: #ff0000; } .leaf { text-color: #00ff00; }")
            .expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let leaf = &ir.root.children[0].children[0];
        let c = leaf.attrs.text_color.expect("text_color");
        assert!(
            c.g > 0.99 && c.r < 0.01,
            "a matching child rule overrides the inherited value"
        );
    }

    #[test]
    fn inline_text_color_overrides_inherited() {
        let mut ir = parse_html(
            r##"<root><column class="outer"><tile text-color="#00ff00" /></column></root>"##,
        )
        .expect("html");
        let css = parse_css(".outer { text-color: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let leaf = &ir.root.children[0].children[0];
        let c = leaf.attrs.text_color.expect("text_color");
        assert!(
            c.g > 0.99 && c.r < 0.01,
            "inline text-color beats the inherited value"
        );
    }

    #[test]
    fn descendant_combinator_matches() {
        let mut ir =
            parse_html(r#"<root><column class="outer"><tile class="inner" /></column></root>"#)
                .expect("html");
        let css = parse_css(".outer .inner { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let inner = &ir.root.children[0].children[0];
        let c = solid(inner.attrs.bg.as_ref().expect("bg"));
        assert!(c.r > 0.99);
    }

    #[test]
    fn child_combinator_matches_direct_child_only() {
        let mut ir = parse_html(
            r#"<root><column class="o"><column><tile class="i" /></column></column></root>"#,
        )
        .expect("html");
        let css = parse_css(".o > .i { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let grandchild = &ir.root.children[0].children[0].children[0];
        assert!(
            grandchild.attrs.bg.is_none(),
            "child combinator must not cross generations"
        );
    }

    #[test]
    fn child_combinator_matches_one_level() {
        let mut ir = parse_html(r#"<root><column class="o"><tile class="i" /></column></root>"#)
            .expect("html");
        let css = parse_css(".o > .i { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let inner = &ir.root.children[0].children[0];
        let c = solid(inner.attrs.bg.as_ref().expect("bg"));
        assert!(c.r > 0.99);
    }

    #[test]
    fn nth_child_matches_odd() {
        let mut ir = parse_html(
            r#"<root>
                <tile class="x" />
                <tile class="x" />
                <tile class="x" />
            </root>"#,
        )
        .expect("html");
        let css = parse_css(".x:nth-child(odd) { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert!(ir.root.children[0].attrs.bg.is_some(), "1st matches");
        assert!(
            ir.root.children[1].attrs.bg.is_none(),
            "2nd must not match :nth-child(odd)"
        );
        assert!(ir.root.children[2].attrs.bg.is_some(), "3rd matches");
    }

    #[test]
    fn adjacent_sibling_matches_only_the_next_element() {
        let mut ir = parse_html(
            r#"<root>
                <tile class="a" />
                <tile class="b" />
                <tile class="b" />
            </root>"#,
        )
        .expect("html");
        let css = parse_css(".a + .b { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert!(
            ir.root.children[1].attrs.bg.is_some(),
            ".b right after .a matches"
        );
        assert!(
            ir.root.children[2].attrs.bg.is_none(),
            "the second .b is not adjacent to .a"
        );
    }

    #[test]
    fn general_sibling_matches_every_later_element() {
        let mut ir = parse_html(
            r#"<root>
                <tile class="b" />
                <tile class="a" />
                <tile class="b" />
                <tile class="b" />
            </root>"#,
        )
        .expect("html");
        let css = parse_css(".a ~ .b { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert!(
            ir.root.children[0].attrs.bg.is_none(),
            "a .b before .a must not match"
        );
        assert!(ir.root.children[2].attrs.bg.is_some());
        assert!(ir.root.children[3].attrs.bg.is_some());
    }

    #[test]
    fn sibling_step_chains_with_a_child_step() {
        let mut ir = parse_html(
            r#"<root>
                <column class="wrap">
                    <tile class="a" />
                    <tile class="b" />
                </column>
                <column>
                    <tile class="a" />
                    <tile class="b" />
                </column>
            </root>"#,
        )
        .expect("html");
        let css = parse_css(".wrap > .a + .b { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert!(ir.root.children[0].children[1].attrs.bg.is_some());
        assert!(
            ir.root.children[1].children[1].attrs.bg.is_none(),
            "the second column is not .wrap"
        );
    }

    #[test]
    fn not_argument_with_a_combinator_filters() {
        // An argument carrying a combinator used to never match, which
        // inverted to `:not()` matching everything.
        let mut ir = parse_html(
            r#"<root>
                <column class="list"><tile class="row" /></column>
                <column><tile class="row" /></column>
            </root>"#,
        )
        .expect("html");
        let css = parse_css(".row:not(.list > .row) { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert!(
            ir.root.children[0].children[0].attrs.bg.is_none(),
            "a .row inside .list is excluded"
        );
        assert!(
            ir.root.children[1].children[0].attrs.bg.is_some(),
            "a .row elsewhere still matches"
        );
    }

    #[test]
    fn is_argument_with_a_combinator_matches() {
        let mut ir = parse_html(
            r#"<root>
                <column class="list"><tile class="row" /></column>
                <column><tile class="row" /></column>
            </root>"#,
        )
        .expect("html");
        let css = parse_css(":is(.list > .row) { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert!(ir.root.children[0].children[0].attrs.bg.is_some());
        assert!(ir.root.children[1].children[0].attrs.bg.is_none());
    }

    #[test]
    fn markup_attribute_beats_css_for_overflow() {
        // `overflow` was one of the properties the inline-origin restore
        // missed, so CSS silently won over the attribute.
        let mut ir =
            parse_html(r#"<root><scroll class="s" overflow="hidden" /></root>"#).expect("html");
        let css = parse_css(".s { overflow: scroll; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert_eq!(
            ir.root.children[0].attrs.overflow,
            Some(crate::layout_ir::OverflowSpec::Hidden)
        );
    }

    #[test]
    fn markup_attribute_beats_css_for_caret_width() {
        let mut ir =
            parse_html(r#"<root><input class="f" caret-width="6" /></root>"#).expect("html");
        let css = parse_css(".f { caret-width: 2; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert_eq!(ir.root.children[0].attrs.caret_width, Some(6.0));
    }

    #[test]
    fn standard_property_spellings_are_accepted() {
        let mut ir = parse_html(r#"<root><image class="x" /></root>"#).expect("html");
        let css = parse_css(
            ".x { justify-content: center; object-fit: cover; flex-shrink: 0; shrink: 0; }",
        )
        .expect("css");
        let warnings = apply_css(&mut ir, &css).expect("apply");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let attrs = &ir.root.children[0].attrs;
        assert_eq!(attrs.justify, Some(crate::layout_ir::FlexJustify::Center));
        assert_eq!(attrs.image_fit, Some(crate::layout_ir::ImageFitSpec::Cover));
        assert_eq!(attrs.shrink, Some(0.0));
    }

    #[test]
    fn not_pseudo_excludes() {
        let mut ir = parse_html(r#"<root><tile class="x" /><tile class="x special" /></root>"#)
            .expect("html");
        let css = parse_css(".x:not(.special) { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert!(
            ir.root.children[0].attrs.bg.is_some(),
            ".x without .special"
        );
        assert!(
            ir.root.children[1].attrs.bg.is_none(),
            ".special excluded by :not(.special)"
        );
    }

    #[test]
    fn is_pseudo_unions() {
        let mut ir =
            parse_html(r#"<root><tile class="a" /><tile class="b" /><tile class="c" /></root>"#)
                .expect("html");
        let css = parse_css(":is(.a, .b) { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert!(ir.root.children[0].attrs.bg.is_some());
        assert!(ir.root.children[1].attrs.bg.is_some());
        assert!(ir.root.children[2].attrs.bg.is_none());
    }

    #[test]
    fn reapply_single_with_media_selects_dark_rule() {
        // The runtime theme-flip re-resolver rebuilds a `tag.class`
        // target and cascades it against the live MediaContext. Under a
        // dark context, the `@media (prefers-color-scheme: dark)` bg
        // must win over the base rule.
        let css = parse_css(
            r#"
            .card { bg: #ffffff; }
            @media (prefers-color-scheme: dark) {
                .card { bg: #000000; }
            }
        "#,
        )
        .expect("css");
        let mut el = Element {
            tag: "tile".into(),
            attrs: Attributes {
                classes: vec!["card".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let dark = MediaContext {
            color_scheme: Some(ColorSchemePreference::Dark),
            ..Default::default()
        };
        reapply_single_with_media(&mut el, &css, &dark).expect("reapply");
        let c = solid(el.attrs.bg.as_ref().expect("bg"));
        assert!(c.r < 0.01, "dark @media rule selected under dark context");

        // Same stylesheet, light context -> base rule keeps winning.
        let mut el2 = Element {
            tag: "tile".into(),
            attrs: Attributes {
                classes: vec!["card".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let light = MediaContext {
            color_scheme: Some(ColorSchemePreference::Light),
            ..Default::default()
        };
        reapply_single_with_media(&mut el2, &css, &light).expect("reapply");
        let c2 = solid(el2.attrs.bg.as_ref().expect("bg"));
        assert!(c2.r > 0.99, "base rule kept under light context");
    }

    #[test]
    fn reapply_single_copies_back_extended_whitelist() {
        // Regression: pre-W4.7 `reapply_single` only restored a text/box
        // subset, so a theme flip couldn't restyle bg / radius / width /
        // margin / opacity / shadow / hover-bg / press-bg. All must now
        // round-trip through the cascade.
        let css = parse_css(
            r#"
            .themed {
                bg: #112233;
                radius: 9px;
                width: 120px;
                margin: 7px;
                opacity: 0.4;
                shadow: 1px 2px 3px #000000;
                hover-bg: #445566;
                press-bg: #778899;
            }
        "#,
        )
        .expect("css");
        let mut el = Element {
            tag: "tile".into(),
            attrs: Attributes {
                classes: vec!["themed".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        reapply_single(&mut el, &css).expect("reapply");
        let a = &el.attrs;
        assert!(a.bg.is_some(), "bg copied back");
        assert_eq!(a.radius, Some(9.0), "radius copied back");
        assert_eq!(
            a.width,
            Some(crate::layout_ir::LengthSpec::Px(120.0)),
            "width copied back"
        );
        assert!(a.margin.is_some(), "margin copied back");
        assert_eq!(a.opacity, Some(0.4), "opacity copied back");
        assert_eq!(a.shadows.len(), 1, "shadow copied back");
        assert!(a.hover_bg.is_some(), "hover-bg copied back");
        assert!(a.press_bg.is_some(), "press-bg copied back");
    }

    fn card_el() -> Element {
        Element {
            tag: "tile".into(),
            attrs: Attributes {
                classes: vec!["card".into()],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn reapply_ancestors_resolves_descendant_theme_rule() {
        // `.theme-dark .card { bg }` must re-resolve on a nested card only
        // when a `.theme-dark` ancestor is actually present in the chain.
        let css = parse_css(
            r#"
            .card { bg: #ffffff; }
            .theme-dark .card { bg: #000000; }
        "#,
        )
        .expect("css");
        let media = MediaContext::default();

        // Root flipped to theme-dark -> descendant rule wins.
        let root = AncestorInfo::new("root", vec!["theme-dark".into()], None);
        let mut el = card_el();
        reapply_with_ancestors(&mut el, &css, &media, &[root]).expect("reapply");
        let c = solid(el.attrs.bg.as_ref().expect("bg"));
        assert!(c.r < 0.01, "descendant .theme-dark .card selected");

        // Root not theme-dark -> base rule keeps winning.
        let root_light = AncestorInfo::new("root", vec!["theme-light".into()], None);
        let mut el2 = card_el();
        reapply_with_ancestors(&mut el2, &css, &media, &[root_light]).expect("reapply");
        let c2 = solid(el2.attrs.bg.as_ref().expect("bg"));
        assert!(c2.r > 0.99, "base .card kept without .theme-dark ancestor");
    }

    #[test]
    fn reapply_ancestors_resolves_per_theme_var_scope() {
        // `:root.theme-dark { --bg }` is a var scope gated on a root
        // class; `.card { bg: var(--bg) }` on a descendant must pick up
        // the dark value after the flip.
        let css = parse_css(
            r#"
            :root { --bg: #ffffff; }
            :root.theme-dark { --bg: #000000; }
            .card { bg: var(--bg); }
        "#,
        )
        .expect("css");
        let media = MediaContext::default();

        let dark_root = AncestorInfo::new("root", vec!["theme-dark".into()], None);
        let mut el = card_el();
        reapply_with_ancestors(&mut el, &css, &media, &[dark_root]).expect("reapply");
        let c = solid(el.attrs.bg.as_ref().expect("bg"));
        assert!(c.r < 0.01, "descendant var(--bg) resolved to dark scope");

        let plain_root = AncestorInfo::new("root", vec![], None);
        let mut el2 = card_el();
        reapply_with_ancestors(&mut el2, &css, &media, &[plain_root]).expect("reapply");
        let c2 = solid(el2.attrs.bg.as_ref().expect("bg"));
        assert!(c2.r > 0.99, "var(--bg) resolved to :root default");
    }

    #[test]
    fn reapply_ancestors_respects_child_combinator() {
        // `parent > child` must bind to the IMMEDIATE parent only. With a
        // `parent` immediate ancestor it matches; when `parent` is a
        // grandparent (an intervening ancestor sits between), it must not.
        let css = parse_css(r#"tile.parent > tile.card { bg: #000000; }"#).expect("css");
        let media = MediaContext::default();

        // Immediate parent is `.parent` -> matches.
        let parent = AncestorInfo::new("tile", vec!["parent".into()], None);
        let mut el = card_el();
        reapply_with_ancestors(&mut el, &css, &media, &[parent]).expect("reapply");
        assert!(
            el.attrs.bg.is_some(),
            "direct child of .parent matches parent > child"
        );

        // `.parent` is a grandparent, an anonymous container in between ->
        // child combinator must fail.
        let grandparent = AncestorInfo::new("tile", vec!["parent".into()], None);
        let middle = AncestorInfo::new("tile", vec!["mid".into()], None);
        let mut el2 = card_el();
        reapply_with_ancestors(&mut el2, &css, &media, &[grandparent, middle]).expect("reapply");
        assert!(
            el2.attrs.bg.is_none(),
            "grandchild of .parent does not match parent > child"
        );
    }

    #[test]
    fn where_pseudo_has_zero_specificity() {
        let mut ir = parse_html(r#"<root><tile class="a" /></root>"#).expect("html");
        // :where(.a) -> (0,0,0); .a -> (0,1,0). .a should win even
        // though :where comes later.
        let css = parse_css(":where(.a) { bg: #ff0000; } .a { bg: #00ff00; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(c.g > 0.99, ".a beats :where(.a) by specificity");
    }

    #[test]
    fn media_prefers_color_scheme_dark() {
        let mut ir = parse_html(r#"<root><tile class="t" /></root>"#).expect("html");
        let css = parse_css(
            r#"
            .t { bg: #ffffff; }
            @media (prefers-color-scheme: dark) {
                .t { bg: #000000; }
            }
        "#,
        )
        .expect("css");
        let ctx = MediaContext {
            color_scheme: Some(ColorSchemePreference::Dark),
            ..Default::default()
        };
        apply_css_with_media(&mut ir, &css, &ctx).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(c.r < 0.01, "dark rule wins under prefers-color-scheme:dark");
    }

    #[test]
    fn media_prefers_color_scheme_dark_ignored_when_light() {
        let mut ir = parse_html(r#"<root><tile class="t" /></root>"#).expect("html");
        let css = parse_css(
            r#"
            .t { bg: #ffffff; }
            @media (prefers-color-scheme: dark) {
                .t { bg: #000000; }
            }
        "#,
        )
        .expect("css");
        let ctx = MediaContext {
            color_scheme: Some(ColorSchemePreference::Light),
            ..Default::default()
        };
        apply_css_with_media(&mut ir, &css, &ctx).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(c.r > 0.99, "light mode keeps the base .t rule");
    }

    #[test]
    fn media_max_width_matches() {
        let mut ir = parse_html(r#"<root><tile class="t" /></root>"#).expect("html");
        let css = parse_css(
            r#"
            .t { bg: #ffffff; }
            @media (max-width: 600px) {
                .t { bg: #ff0000; }
            }
        "#,
        )
        .expect("css");
        let ctx = MediaContext {
            viewport_width: Some(500.0),
            ..Default::default()
        };
        apply_css_with_media(&mut ir, &css, &ctx).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(c.r > 0.99);
    }

    #[test]
    fn inline_attr_still_beats_css() {
        // Regression for `css_does_not_override_html_inline` in
        // tests/parse.rs.
        let mut ir = parse_html(r#"<root><tile class="t" width="50px"/></root>"#).expect("html");
        let css = parse_css(".t { width: 100px; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let tile = &ir.root.children[0];
        assert_eq!(
            tile.attrs.width,
            Some(crate::layout_ir::LengthSpec::Px(50.0))
        );
    }

    #[test]
    fn cascade_lint_flags_repeated_property() {
        let css = parse_css(".a { bg: #ff0000; } .a { bg: #00ff00; }").expect("css");
        let div = cascade_lint(&css);
        assert_eq!(div.len(), 1);
        assert_eq!(div[0].property, "bg");
        assert_eq!(div[0].first_wins, "#ff0000");
        assert_eq!(div[0].last_wins, "#00ff00");
    }

    #[test]
    fn pseudo_elements_rejected() {
        let r = parse_css(".x::before { bg: #fff; }");
        assert!(r.is_err(), "::pseudo-element must be a hard parse error");
    }

    #[test]
    fn structural_root_matches_root_var() {
        let mut ir = parse_html(r#"<root><tile class="x" /></root>"#).expect("html");
        let css = parse_css(":root { --bg: #ff0000; } .x { bg: var(--bg); }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let c = solid(ir.root.children[0].attrs.bg.as_ref().expect("bg"));
        assert!(c.r > 0.99);
    }

    #[test]
    fn nth_child_an_plus_b_form() {
        // 3n+1 -> 1, 4, 7, ...
        let mut ir = parse_html(
            r#"<root>
                <tile class="x" /><tile class="x" /><tile class="x" />
                <tile class="x" /><tile class="x" /><tile class="x" />
                <tile class="x" />
            </root>"#,
        )
        .expect("html");
        let css = parse_css(".x:nth-child(3n+1) { bg: #ff0000; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        for (i, child) in ir.root.children.iter().enumerate() {
            let pos = i as i32 + 1;
            let want_match = pos == 1 || pos == 4 || pos == 7;
            assert_eq!(
                child.attrs.bg.is_some(),
                want_match,
                "child #{pos}: nth-child(3n+1)"
            );
        }
    }
}

#[cfg(test)]
mod grid_tests {
    use super::*;
    use crate::parse_html;

    fn tile_with_class(cls: &str) -> LayoutIR {
        parse_html(&format!(r#"<root><tile class="{cls}" /></root>"#)).expect("html")
    }

    #[test]
    fn display_grid_parses() {
        let mut ir = tile_with_class("box");
        let css = parse_css(".box { display: grid; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert_eq!(ir.root.children[0].attrs.display, Some(DisplaySpec::Grid));
    }

    #[test]
    fn display_flex_parses() {
        let mut ir = tile_with_class("box");
        let css = parse_css(".box { display: flex; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert_eq!(ir.root.children[0].attrs.display, Some(DisplaySpec::Flex));
    }

    #[test]
    fn grid_template_columns_parses_fr() {
        let mut ir = tile_with_class("g");
        let css = parse_css(".g { grid-template-columns: 1fr 2fr; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let gt = ir.root.children[0]
            .attrs
            .grid_template
            .as_ref()
            .expect("grid template");
        assert_eq!(gt.columns.len(), 2);
        assert_eq!(gt.columns[0], TrackSizeSpec::Fr(1.0));
        assert_eq!(gt.columns[1], TrackSizeSpec::Fr(2.0));
    }

    #[test]
    fn grid_template_rows_parses_mixed_tracks() {
        let mut ir = tile_with_class("g");
        let css = parse_css(".g { grid-template-rows: 100px auto min-content max-content; }")
            .expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let gt = ir.root.children[0]
            .attrs
            .grid_template
            .as_ref()
            .expect("grid template");
        assert_eq!(gt.rows.len(), 4);
        assert_eq!(gt.rows[0], TrackSizeSpec::Fixed(100.0));
        assert_eq!(gt.rows[1], TrackSizeSpec::Auto);
        assert_eq!(gt.rows[2], TrackSizeSpec::MinContent);
        assert_eq!(gt.rows[3], TrackSizeSpec::MaxContent);
    }

    #[test]
    fn grid_template_columns_parses_minmax() {
        let mut ir = tile_with_class("g");
        let css = parse_css(".g { grid-template-columns: minmax(100px, 1fr); }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let gt = ir.root.children[0]
            .attrs
            .grid_template
            .as_ref()
            .expect("grid template");
        assert_eq!(gt.columns.len(), 1);
        match &gt.columns[0] {
            TrackSizeSpec::MinMax(a, b) => {
                assert!(matches!(**a, TrackSizeSpec::Fixed(_)));
                assert!(matches!(**b, TrackSizeSpec::Fr(_)));
            }
            other => panic!("expected MinMax, got {:?}", other),
        }
    }

    #[test]
    fn gap_per_axis_splits_into_row_column() {
        let mut ir = tile_with_class("g");
        let css = parse_css(".g { gap: 8 16; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let attrs = &ir.root.children[0].attrs;
        assert_eq!(attrs.gap_row, Some(8.0));
        assert_eq!(attrs.gap_column, Some(16.0));
    }

    #[test]
    fn gap_shorthand_sets_legacy_field() {
        let mut ir = tile_with_class("g");
        let css = parse_css(".g { gap: 12; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let attrs = &ir.root.children[0].attrs;
        assert_eq!(attrs.gap, Some(12.0));
    }

    #[test]
    fn row_gap_and_column_gap_parse_independently() {
        let mut ir = tile_with_class("g");
        let css = parse_css(".g { row-gap: 8; column-gap: 16; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let attrs = &ir.root.children[0].attrs;
        assert_eq!(attrs.gap_row, Some(8.0));
        assert_eq!(attrs.gap_column, Some(16.0));
    }

    #[test]
    fn align_items_baseline_parses() {
        let mut ir = tile_with_class("g");
        let css = parse_css(".g { align-items: baseline; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        assert_eq!(ir.root.children[0].attrs.align, Some(FlexAlign::Baseline));
    }

    #[test]
    fn align_self_and_justify_items_parse() {
        let mut ir = tile_with_class("g");
        let css =
            parse_css(".g { align-self: baseline; justify-items: center; justify-self: end; }")
                .expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let attrs = &ir.root.children[0].attrs;
        assert_eq!(attrs.align_self, Some(FlexAlign::Baseline));
        assert_eq!(attrs.justify_items, Some(FlexAlign::Center));
        assert_eq!(attrs.justify_self, Some(FlexAlign::End));
    }

    #[test]
    fn grid_row_and_grid_column_parse_line_numbers() {
        let mut ir = tile_with_class("c");
        let css = parse_css(".c { grid-row: 1 / 3; grid-column: 2; }").expect("css");
        apply_css(&mut ir, &css).expect("apply");
        let attrs = &ir.root.children[0].attrs;
        assert_eq!(attrs.grid_row, Some((1, 3)));
        assert_eq!(attrs.grid_column, Some((2, 0)));
    }
}

#[cfg(test)]
mod invalidation_tests {
    use super::*;

    #[test]
    fn class_invalidation_set_collects_all_class_names() {
        let src = r#"
            .card { bg: #ffffff; }
            .btn.primary { bg: #0000ff; }
            .theme-dark .card { bg: #000011; }
            tag-only { padding: 4; }
        "#;
        let sheet = parse_css(src).expect("parse");
        let set = sheet.class_invalidation_set();
        assert!(set.contains("card"));
        assert!(set.contains("btn"));
        assert!(set.contains("primary"));
        assert!(set.contains("theme-dark"));
        assert!(!set.contains("tag-only"));
    }
}

#[cfg(test)]
mod bughunt_tests {
    use super::*;

    #[test]
    fn strip_comments_preserves_non_ascii() {
        // The old `bytes[i] as char` Latin-1 cast shredded every multibyte
        // scalar; walking chars keeps `caf\u{e9}` / `->` / emoji intact.
        let src = ".x { content: \"caf\u{e9} -> \u{4e16}\u{754c} \u{1f680}\"; }";
        let out = strip_comments(src);
        assert_eq!(
            out, src,
            "no comments to strip, non-ASCII must survive verbatim"
        );
    }

    #[test]
    fn strip_comments_non_ascii_around_comment() {
        let out = strip_comments("caf\u{e9}/* x */\u{4e16}\u{754c}");
        assert_eq!(out, "caf\u{e9}\u{4e16}\u{754c}");
    }

    #[test]
    fn non_ascii_css_round_trips_through_parse() {
        let sheet = parse_css(".x { content: \"caf\u{e9}\"; }").expect("parse");
        let decl = &sheet.rules[0].declarations[0];
        assert_eq!(decl.name, "content");
        assert_eq!(decl.value, "\"caf\u{e9}\"");
    }

    #[test]
    fn unsupported_at_rules_are_skipped() {
        let css = parse_css(
            r#"
            .a { color: #ffffff; }
            @keyframes spin {
                from { transform: rotate(0deg); }
                to { transform: rotate(360deg); }
            }
            @font-face { font-family: "X"; src: url(x.ttf); }
            @charset "utf-8";
            .b { color: #000000; }
            "#,
        )
        .expect("an unsupported at-rule must not fail the stylesheet");
        let selectors: Vec<String> = css
            .rules
            .iter()
            .map(|r| format!("{:?}", r.selectors))
            .collect();
        assert_eq!(css.rules.len(), 2, "got rules: {selectors:?}");
    }

    #[test]
    fn deeply_nested_not_returns_error_not_crash() {
        // 200 levels of `:not(...)` nesting: unbounded recursion used to
        // SIGSEGV here. It must now surface a bounded ParseError.
        let mut sel = String::from("a");
        for _ in 0..200 {
            sel = format!(":not({sel})");
        }
        let css = format!("{sel} {{ color: #fff; }}");
        let err = parse_css(&css).expect_err("must reject, not overflow the stack");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("depth"),
            "expected a depth-cap error, got: {msg}"
        );
    }

    #[test]
    fn deeply_nested_media_returns_error_not_crash() {
        let mut css = String::new();
        for _ in 0..200 {
            css.push_str("@media (min-width: 1px) {");
        }
        css.push_str(".x { color: #fff; }");
        for _ in 0..200 {
            css.push('}');
        }
        let err = parse_css(&css).expect_err("must reject, not overflow the stack");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("depth"),
            "expected a depth-cap error, got: {msg}"
        );
    }

    #[test]
    fn shallow_not_and_media_still_parse() {
        parse_css(":not(.a) { color: #fff; }").expect("single :not is fine");
        parse_css("@media (min-width: 1px) { @media (max-width: 9px) { .x { color: #fff; } } }")
            .expect("two levels of @media are fine");
    }

    #[test]
    fn anb_matches_handles_i32_min_offset() {
        // `index - i32::MIN` overflowed; widening to i64 must not panic.
        let anb = AnB { a: 2, b: i32::MIN };
        assert!(!anb.matches(1));
        assert!(!anb.matches(i32::MAX));
    }

    #[test]
    fn grid_line_out_of_i16_range_clamps() {
        // `40000 as i16` silently wrapped negative; now it clamps.
        let (s, _e) = parse_grid_line_pair(".x", "grid-row", "40000").expect("parse");
        assert_eq!(s, i16::MAX);
    }

    #[test]
    fn split_top_level_semicolons_escaped_backslash() {
        // `"\\"` ends the string; the `;` after it is top-level.
        let parts = split_top_level_semicolons(r#"content: "\\"; color: red"#);
        assert_eq!(parts.len(), 2, "escaped backslash must not swallow the ';'");
    }
}
