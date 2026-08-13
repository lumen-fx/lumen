//! Minimal Lumen-flavoured HTML parser.
//!
//! Accepts an XML-shaped subset of HTML - tags must close, attributes are
//! double-quoted. Tag and attribute name handling defined in the crate
//! root docs.
//!
//! # Signal references (`$` prefix)
//!
//! Binding attributes (`bind-text`, `bind-checked`, `bind-value`) and
//! `{interpolation}` placeholders inside text content + string attribute
//! values both accept a leading `$` on the signal name. The dollar
//! prefix is the **preferred** form for new code - the bare shorthand
//! (`bind-text="count"`, `{count}`) keeps working unchanged so existing
//! markup migrates one site at a time.
//!
//! - `bind-text="$count"` is identical to `bind-text="count"`.
//! - `<label>{$count}</label>` is identical to `<label>{count}</label>`.
//! - `bind-text="$self.field"` is the per-entity binding form - the
//!   spawn layer installs a [`lumen_core::components::BindSelfText`]
//!   marker so the field is read from this entity's per-entity property
//!   bag at runtime. (The consuming system lands in a follow-up
//!   commit - today the marker is recorded but inert.)
//! - `bind-text="$parent.field"` is the analogous parent-entity form
//!   (one [`bevy_ecs::hierarchy::ChildOf`] step up the tree).
//!
//! The same `$self.` / `$parent.` shapes are accepted inside
//! `{interpolation}` placeholders; they currently lower to a literal
//! empty-string substitution until the per-entity consumer system lands.

use crate::layout_ir::{
    Attributes, BindKind, BindSpec, Element, FlexAlign, FlexAxis, FlexJustify, ImageFitSpec,
    InterpolationSlot, LayoutIR, LengthSpec, LineHeightSpec, LintFinding, LintKind, LintSeverity,
    OutlineSpec, OverflowSpec, ParseError, PositionSpec, ScrollAxisSpec, TextAlignSpec,
    TextWrapSpec,
};
use crate::values::{bad, parse_bg, parse_color, parse_edges, parse_f32, parse_i32, parse_length};
// `parse_duration_ms` lives on the CSS cascade side (`lumen_ir::css`)
// because it's shared with `transition-duration`; reused here rather
// than re-implementing the `Nms` / `Ns` unit handling for the inline
// markup mirror of `caret-blink` / `scrollbar-fade-*`.
use lumen_ir::css::parse_duration_ms;

/// Recognized layout tag names. Unknown tags produce
/// [`ParseError::UnknownTag`]. `script` is special-cased (collected into
/// `LayoutIR::script_source`, not a layout node).
const KNOWN_TAGS: &[&str] = &[
    // Layout primitives. Direction is encoded in the tag - no separate
    // `flex="row|column"` attribute; pick `<row>` / `<column>` /
    // `<spacer>`.
    "root",
    "column",
    "row",
    "spacer",
    "scroll",
    "tile",
    "label",
    "div",
    // Real anchor. `<a href="settings">Settings</a>` maps 1:1 onto a DOM
    // `<a href>` on the future web-transpile target; on desktop, clicking it
    // navigates the active page (file-based pages). The `href` is a page
    // path, resolved by longest existing `.lmn` prefix - not a URL scheme.
    "a",
    "input",
    "textarea",
    "image",
    // `<overlay>` floats out of normal flow. Defaults: position=absolute
    // and inset=0 0 0 0 so it covers its nearest positioned ancestor
    // (typically the root). Use it for modal backdrops, dropdowns,
    // tooltips - anything that should paint above its siblings.
    "overlay",
    // Stateful controls. `button` is a tile with focusable defaults,
    // `toggle` is a checkbox/switch hybrid (bool state, two render
    // styles), `slider` carries a 0..1 value the user drags.
    "button",
    "toggle",
    // `switch` is a `toggle` in switch presentation: bool state over the
    // same `Toggleable` machinery, rendered as a pill track with a thumb
    // that slides (and animates) between off and on.
    "switch",
    "slider",
    // Form controls (W5). `checkbox` = box + label over the Toggleable
    // machinery (tri-state via indeterminate="true"); `radio` =
    // name-grouped exclusive selection writing the `group` signal;
    // `progress` = determinate (value/max, bind-value) or indeterminate
    // (no value - animated sweep) bar, non-interactive.
    "checkbox",
    "radio",
    "progress",
    // Reactive iteration. `<for each="rows" key="id">` spawns one copy
    // of its inline children per item in the named `ArraySignals`
    // entry. `{field}` placeholders inside attrs / text get replaced
    // by the matching item field at reconcile time.
    "for",
    // Reactive branch. `<if signal="loaded">...</if>` mounts its inline
    // children only when the named `Signals` entry is truthy
    // (non-empty AND not literal "false" / "0"). Toggling the signal
    // spawns / despawns the subtree on the next tick.
    "if",
    // Modal overlay. `<dialog open="show">` is sugar for an absolute-
    // positioned full-viewport container whose visibility is bound to
    // a signal. Children compose any markup; authors style via the
    // `dialog` tag selector or a class.
    "dialog",
    // Title-bar region for frameless windows
    // (`<root frameless="true">`). Defaults to a 32px-high full-width
    // row at the top. Adding `drag="true"` makes pressing-and-moving
    // the bar request a native window drag via the window backend.
    "title-bar",
    // Hover-delay popup. Wraps exactly one trigger child; the
    // tooltip body text + delay are attributes on `<tooltip>` itself
    // and the parser collapses `<tooltip>` away, attaching a
    // `TooltipSource` to the child. The popup is spawned at runtime
    // by `lumen-primitives::TooltipPlugin` once dwell time elapses.
    "tooltip",
    // Tabbed container. Children must be `<tab name label>...</tab>`
    // elements. Parser flattens to a column with a button strip on
    // top + per-tab `<if signal=... mode="hide" eq="...">...</if>` bodies.
    "tabs",
    "tab",
    // Dropdown widget. `<dropdown bind-value="signal">` + `<option value="x" label="X"/>` children collapse to a header button plus an absolutely-positioned options panel.
    "dropdown",
    "option",
    // Menu widget. `<menu id="m">` + `<menuitem id label/>` + `<separator/>` children collapse to an absolute-positioned panel toggled via `__menu_open:<id>`. `<menu>` inside a `<menubar>` is consumed earlier by `extract_menubar`.
    "menu",
    "menuitem",
    "separator",
    // Date/time pickers. Author writes a date or time string into a validated `<input>`; the built-in pattern enforces the shape.
    "date-picker",
    "time-picker",
];

/// Parse a Lumen-markup string into a [`LayoutIR`].
///
/// `<script>` tags are collected into `LayoutIR::script_source` and
/// stripped from the element tree (they are not layout nodes).
///
/// `<template name="X">...body...</template>` blocks define reusable subtrees
/// (see [`expand_templates`]). Authors instantiate them with `<X k="v"/>`
/// or `<use template="X" k="v"/>`; `{k}` placeholders inside the template
/// body are textually substituted at expand-time.
pub fn parse_html(src: &str) -> Result<LayoutIR, ParseError> {
    parse_html_impl(src, std::path::Path::new(""), None)
}

/// Same as [`parse_html`] but resolves `<include src="..."/>` directives
/// through `loader`, treating relative paths as relative to `self_path`'s
/// directory. `self_path` is the file `src` was read from; it seeds cycle
/// detection and error positions. The resolved include file paths land in
/// [`LayoutIR::included_files`] so the runtime can watch them.
pub fn parse_html_with_loader(
    src: &str,
    self_path: &std::path::Path,
    loader: &dyn crate::resolve::FileLoader,
) -> Result<LayoutIR, ParseError> {
    parse_html_impl(src, self_path, Some(loader))
}

fn parse_html_impl(
    src: &str,
    self_path: &std::path::Path,
    loader: Option<&dyn crate::resolve::FileLoader>,
) -> Result<LayoutIR, ParseError> {
    let mut included_files = Vec::new();
    let spliced = crate::resolve::resolve_includes(src, self_path, loader, &mut included_files)?;
    let expanded = expand_templates(&spliced)?;
    let doc = roxmltree::Document::parse(&expanded).map_err(|e| ParseError::Xml(e.to_string()))?;
    let mut script_source = String::new();
    let mut external_scripts = Vec::new();
    let mut lint_findings: Vec<LintFinding> = Vec::new();
    // `<root skin="...">` is the opt-in surface for the embedded
    // user-agent stylesheet. Pulled before `build_element` because
    // `skin` is not a layout attribute and shouldn't survive in
    // `Attributes` - it's metadata for the runtime.
    let skin = doc.root_element().attribute("skin").map(|s| s.to_string());
    let frameless = bool_attribute_of(
        doc.root_element(),
        "frameless",
        &expanded,
        &mut lint_findings,
    );
    // Extract `<menubar>` blocks from the layout tree before
    // `build_element` walks the root - they live in `LayoutIR.menubar`
    // and the window backend builds an OS-native menu from them.
    let menubar = extract_menubar(doc.root_element())?;
    let root = build_element(
        doc.root_element(),
        &mut script_source,
        &mut external_scripts,
        &expanded,
        &mut lint_findings,
        false,
        0,
    )?;
    Ok(LayoutIR {
        root,
        script_source,
        external_scripts,
        skin,
        frameless,
        menubar,
        combined_stylesheet: None,
        lint_findings,
        included_files,
    })
}

/// Parsed `<template>` block. `defaults` lists every attribute on the
/// `<template ...>` opening tag besides `name=` - those values fill in
/// placeholders the use-site omits. `body` is the raw markup between
/// `<template ...>` and `</template>`.
type TemplateEntry = (Vec<(String, String)>, String);

/// Two-pass textual template expander.
///
/// 1. Collect every `<template name="X">...</template>` block. The body is
///    stored verbatim (raw markup) and the block is stripped from the source.
/// 2. Repeatedly expand `<use template="X" k="v"/>` and `<X k="v"/>`
///    invocations by substituting `{k}` with the given attribute value
///    inside the template body. Expansion runs to a fixed point so templates
///    may reference other templates.
///
/// Both forms accept any attribute name; only `template`/`name` are reserved.
/// Attributes on the `<template ...>` tag itself (other than `name`) seed
/// defaults that fill in placeholders the use-site omits.
/// Unmatched `{placeholder}` markers in the body are left as-is so the
/// caller can spot typos via downstream parse errors (`bad attribute`).
fn expand_templates(src: &str) -> Result<String, ParseError> {
    use std::collections::HashMap;
    let mut templates: HashMap<String, TemplateEntry> = HashMap::new();
    let mut working = String::with_capacity(src.len());

    let mut i = 0usize;
    let bytes = src.as_bytes();
    while let Some(rel) = src[i..].find("<template") {
        let start = i + rel;
        let header_end = src[start..]
            .find('>')
            .ok_or_else(|| ParseError::Xml("unterminated <template> tag".into()))?
            + start;
        let header = &src[start..=header_end];
        let name = extract_attr(header, "name")
            .ok_or_else(|| ParseError::Xml("<template> missing name=\"...\"".into()))?;
        // Defaults: every other attribute on `<template ...>` seeds a
        // fallback for the matching placeholder. Use-site attrs win.
        let defaults: Vec<(String, String)> = extract_all_attrs(header)
            .into_iter()
            .filter(|(k, _)| k != "name")
            .collect();
        let body_start = header_end + 1;
        let body_end_rel = src[body_start..]
            .find("</template>")
            .ok_or_else(|| ParseError::Xml(format!("unterminated <template name=\"{name}\">")))?;
        let body_end = body_start + body_end_rel;
        let body = src[body_start..body_end].trim().to_string();
        let after_close = body_end + "</template>".len();

        working.push_str(&src[i..start]);
        templates.insert(name, (defaults, body));
        i = after_close;
    }
    working.push_str(&src[i..]);
    let _ = bytes; // keep for index sanity

    // Fixed-point expansion of <use template="..."> and <Name .../>.
    let mut depth = 0;
    loop {
        depth += 1;
        if depth > 32 {
            return Err(ParseError::Xml(
                "template expansion exceeded depth 32 (recursive?)".into(),
            ));
        }
        let next = expand_once(&working, &templates)?;
        if next == working {
            break;
        }
        working = next;
    }

    Ok(working)
}

/// Find the byte offset (relative to `s`, which must start at `<`) of the
/// `>` that closes this tag, skipping any `>` that appears inside a quoted
/// attribute value (`<Card label="a > b"/>`). Comments swallow everything -
/// including `>` and quotes - up to their `-->`. Returns `None` if the tag
/// (or comment) is unterminated.
fn find_tag_gt(s: &str) -> Option<usize> {
    if let Some(rest) = s.strip_prefix("<!--") {
        // Index of the `>` in the closing `-->`.
        return rest.find("-->").map(|i| "<!--".len() + i + 2);
    }
    let bytes = s.as_bytes();
    let mut in_quote: Option<u8> = None;
    for (i, &c) in bytes.iter().enumerate() {
        match in_quote {
            Some(q) => {
                if c == q {
                    in_quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => in_quote = Some(c),
                b'>' => return Some(i),
                _ => {}
            },
        }
    }
    None
}

fn expand_once(
    src: &str,
    templates: &std::collections::HashMap<String, TemplateEntry>,
) -> Result<String, ParseError> {
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        let Some(rel) = src[i..].find('<') else {
            out.push_str(&src[i..]);
            break;
        };
        let lt = i + rel;
        out.push_str(&src[i..lt]);
        let gt_rel =
            find_tag_gt(&src[lt..]).ok_or_else(|| ParseError::Xml("unterminated tag".into()))?;
        let gt = lt + gt_rel;
        let raw = &src[lt..=gt]; // includes < and >
        // Skip comments / closing / processing entities - leave them alone.
        if raw.starts_with("<!--") || raw.starts_with("</") || raw.starts_with("<?") {
            out.push_str(raw);
            i = gt + 1;
            continue;
        }
        // Determine the tag name after `<` up to whitespace or `/` or `>`.
        let inner = &raw[1..raw.len() - 1];
        let self_closing = inner.ends_with('/');
        let stripped = inner.strip_suffix('/').unwrap_or(inner);
        let tag_end = stripped
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(stripped.len());
        let tag_name = &stripped[..tag_end];

        // Resolve which template (if any) this tag instantiates.
        let template_key = if tag_name == "use" {
            extract_attr(raw, "template")
        } else if templates.contains_key(tag_name) {
            Some(tag_name.to_string())
        } else {
            None
        };

        if let Some(key) = template_key {
            let (defaults, template_body) = templates
                .get(&key)
                .ok_or_else(|| ParseError::Xml(format!("unknown template '{key}'")))?
                .clone();
            // Merge use-site attrs first (so they win on placeholder
            // substitution - `substitute_placeholders` only replaces the
            // first match; later duplicates are no-ops), then fill in
            // defaults for keys the use-site omitted.
            let use_attrs = extract_all_attrs(raw);
            let mut attrs: Vec<(String, String)> =
                Vec::with_capacity(use_attrs.len() + defaults.len());
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for (k, v) in &use_attrs {
                seen.insert(k.clone());
                attrs.push((k.clone(), v.clone()));
            }
            for (k, v) in &defaults {
                if !seen.contains(k) {
                    attrs.push((k.clone(), v.clone()));
                }
            }
            // Self-closing -> no slot content; open -> capture inner XML up
            // to the matching close tag, supporting nested uses of the
            // same template name.
            let (children_xml, advance_to) = if self_closing {
                (String::new(), gt + 1)
            } else {
                let close_name = if tag_name == "use" { "use" } else { tag_name };
                let (close_lt, close_end) = find_matching_close(src, gt + 1, close_name)?;
                (src[gt + 1..close_lt].trim().to_string(), close_end)
            };
            // Per-instance id prefix. The use's literal `id` attr (after
            // outer placeholder substitution) seeds a prefix that is
            // prepended to every `id="..."` inside the template body.
            // Uses without an `id` get no prefix - single-instance
            // templates keep their stable ids.
            let prefix = extract_attr(raw, "id").map(|s| format!("{s}:"));
            let mut expanded = substitute_placeholders(&template_body, &attrs);
            if let Some(p) = &prefix {
                expanded = inject_id_prefix(&expanded, p);
            }
            expanded = substitute_slot(&expanded, &children_xml);
            out.push_str(&expanded);
            i = advance_to;
            continue;
        } else {
            out.push_str(raw);
        }
        i = gt + 1;
    }
    Ok(out)
}

/// Scan `src` starting at `start` for the matching `</tag_name>` close,
/// respecting nested opens of the same name. Returns
/// `(close_open_offset, advance_past_close_offset)`.
fn find_matching_close(
    src: &str,
    start: usize,
    tag_name: &str,
) -> Result<(usize, usize), ParseError> {
    let mut i = start;
    let mut depth = 1i32;
    while i < src.len() {
        let Some(rel) = src[i..].find('<') else {
            break;
        };
        let lt = i + rel;
        let gt_rel = find_tag_gt(&src[lt..]).ok_or_else(|| {
            ParseError::Xml(format!("unterminated tag while seeking </{tag_name}>"))
        })?;
        let gt = lt + gt_rel;
        let raw = &src[lt..=gt];
        if raw.starts_with("<!--") || raw.starts_with("<?") {
            i = gt + 1;
            continue;
        }
        let is_close = raw.starts_with("</");
        let inner = &raw[if is_close { 2 } else { 1 }..raw.len() - 1];
        let self_closing = inner.ends_with('/');
        let stripped = inner.strip_suffix('/').unwrap_or(inner);
        let tag_end = stripped
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(stripped.len());
        let this_name = &stripped[..tag_end];
        if this_name == tag_name {
            if is_close {
                depth -= 1;
                if depth == 0 {
                    return Ok((lt, gt + 1));
                }
            } else if !self_closing {
                depth += 1;
            }
        }
        i = gt + 1;
    }
    Err(ParseError::Xml(format!(
        "unterminated template use <{tag_name}>"
    )))
}

/// Prepend `prefix` to every `id="..."` attribute value in `body`.
/// Boundary check: the byte preceding `id=` must be whitespace, `<`, or a
/// recognized attribute separator, so substrings like `mid="..."` are not
/// rewritten. Existing prefixes compose: `id="b"` under a prefix `"a:"`
/// becomes `id="a:b"`; on a subsequent expansion that uses the resulting
/// `id="a:b"` as a use-site prefix, inner ids stack to `a:b:c`.
fn inject_id_prefix(body: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(body.len() + prefix.len() * 4);
    let mut i = 0;
    while i < body.len() {
        let Some(rel) = body[i..].find("id=\"") else {
            out.push_str(&body[i..]);
            break;
        };
        let pos = i + rel;
        let before = if pos == 0 {
            None
        } else {
            body.as_bytes().get(pos - 1).copied()
        };
        let is_boundary =
            pos == 0 || matches!(before, Some(b' ' | b'\t' | b'\n' | b'\r' | b'<' | b'/'));
        if !is_boundary {
            out.push_str(&body[i..pos + 4]);
            i = pos + 4;
            continue;
        }
        out.push_str(&body[i..pos + 4]);
        let val_start = pos + 4;
        let Some(end_rel) = body[val_start..].find('"') else {
            out.push_str(&body[val_start..]);
            break;
        };
        out.push_str(prefix);
        out.push_str(&body[val_start..val_start + end_rel]);
        i = val_start + end_rel;
    }
    out
}

/// Replace every `<slot/>` / `<slot></slot>` / `<slot default="..."/>` in
/// `body` with `children` (template caller's inner XML). If `children` is
/// empty, fall back to the slot's `default=""` attribute, then to the
/// `<slot>...</slot>` inner content, then to empty.
fn substitute_slot(body: &str, children: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let Some(rel) = body[i..].find("<slot") else {
            out.push_str(&body[i..]);
            break;
        };
        let lt = i + rel;
        // Boundary check - must be `<slot` followed by space, `/`, or `>`.
        let after = body.as_bytes().get(lt + 5).copied();
        if !matches!(after, Some(b' ' | b'\t' | b'\n' | b'/' | b'>')) {
            out.push_str(&body[i..lt + 5]);
            i = lt + 5;
            continue;
        }
        out.push_str(&body[i..lt]);
        let Some(gt_rel) = body[lt..].find('>') else {
            out.push_str(&body[lt..]);
            break;
        };
        let gt = lt + gt_rel;
        let opening = &body[lt..=gt];
        let opening_inner = &opening[1..opening.len() - 1];
        let self_closing = opening_inner.ends_with('/');
        let default_val = extract_attr(opening, "default").unwrap_or_default();

        let (slot_default_inner, advance_to) = if self_closing {
            (String::new(), gt + 1)
        } else if let Some(close_rel) = body[gt + 1..].find("</slot>") {
            let close_start = gt + 1 + close_rel;
            (
                body[gt + 1..close_start].trim().to_string(),
                close_start + "</slot>".len(),
            )
        } else {
            (String::new(), gt + 1)
        };

        if !children.is_empty() {
            out.push_str(children);
        } else if !default_val.is_empty() {
            out.push_str(&default_val);
        } else {
            out.push_str(&slot_default_inner);
        }
        i = advance_to;
    }
    out
}

/// Pull a single attribute value out of a raw tag string (`<foo a="b" />`).
fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end_rel = tag[start..].find('"')?;
    Some(tag[start..start + end_rel].to_string())
}

/// Yield every `key="value"` pair in the raw tag string in source order.
fn extract_all_attrs(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let inner = tag.trim_start_matches('<').trim_end_matches('>');
    let inner = inner.trim_end_matches('/');
    let mut chars = inner.char_indices().peekable();
    // skip tag name
    while let Some((_, c)) = chars.peek() {
        if c.is_whitespace() {
            break;
        }
        chars.next();
    }
    let mut buf = String::new();
    let mut state = 0u8; // 0 = looking for name, 1 = inside name, 2 = inside value
    let mut current_name = String::new();
    while let Some((_, c)) = chars.next() {
        match state {
            0 if !c.is_whitespace() && c != '=' => {
                buf.push(c);
                state = 1;
            }
            1 => {
                if c == '=' {
                    current_name = std::mem::take(&mut buf);
                    state = 2;
                } else if c.is_whitespace() {
                    buf.clear();
                    state = 0;
                } else {
                    buf.push(c);
                }
            }
            2 if c == '"' || c == '\'' => {
                let quote = c;
                let mut val = String::new();
                for (_, vc) in chars.by_ref() {
                    if vc == quote {
                        break;
                    }
                    val.push(vc);
                }
                out.push((current_name.clone(), val));
                state = 0;
            }
            _ => {}
        }
    }
    out
}

/// Normalise `$`-prefixed signal references inside `{interpolation}`
/// placeholders.
///
/// - `{$name}` -> `{name}` (the `$` is opt-in sugar; the bare form keeps
///   working). The bare form has been the legacy default; new code is
///   encouraged to write `{$name}` so reviewers can grep for signal
///   sites.
/// - `{$self.<field>}` -> currently emitted as a literal empty string
///   (the per-entity consumer system lands in a follow-up commit; we
///   keep the substitution lossless on the parser side and let the
///   runtime resolve the marker).
/// - `{$parent.<field>}` -> same fallback as `$self.` above.
///
/// Unknown / unmatched placeholders are passed through verbatim so
/// misuse fails loudly downstream.
///
/// Round-8 wave-B: also walks the body for *bare* `{name}` sites and
/// pushes a `LintKind::BareInterpolation` info-level finding for each
/// one. `body_offset` is the byte position of `body` within the
/// expanded source so the finding's line/col is accurate. `src` is
/// the full expanded source.
///
/// Round-8 wave-C: also collects [`InterpolationSlot`]s into `slots` so
/// the spawner can resolve each placeholder against the right scope
/// (global signals, the current iteration record, the row index, ...)
/// without re-classifying the brace body at runtime.
/// `in_for` toggles row-aware lint messaging: bare `{name}` inside a
/// `<for>` body now suggests both `{row.name}` and `{$name}` because
/// the iteration / global ambiguity is real. Outside `<for>`, the
/// wave-B message stands.
/// Also recognizes the legacy `{idx}` alias (= `{$index}`) inside a
/// `<for>` body and routes it to [`InterpolationSlot::RowIndex`]
/// while emitting a bare-interp finding that suggests `{$index}`.
fn normalize_dollar_interpolation(
    body: &str,
    body_offset: usize,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    slots: &mut Vec<InterpolationSlot>,
    in_for: bool,
) -> String {
    // Walk every `{...}` placeholder, classify, optionally rewrite, and
    // emit a deprecation finding for bare `{name}` shapes. We can't
    // shortcut on `body.contains("{$")` any more - bare-form bodies
    // also need the walk.
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        // Find the next `{` opener (any flavour).
        let Some(rel) = body[i..].find('{') else {
            out.push_str(&body[i..]);
            break;
        };
        let lt = i + rel;
        out.push_str(&body[i..lt]);
        // Locate the matching `}`. We use a single-line scan since
        // `{...}` placeholders never span newlines in markup.
        let Some(end_rel) = body[lt..].find('}') else {
            // No close - emit the rest verbatim.
            out.push_str(&body[lt..]);
            break;
        };
        let gt = lt + end_rel;
        let inner = &body[lt + 1..gt];
        let trimmed = inner.trim();
        if let Some(rest) = trimmed.strip_prefix('$') {
            // `{$...}` family - rewrite or preserve verbatim, no lint
            // since the author already used the explicit form.
            if rest.starts_with("self.") || rest.starts_with("parent.") {
                // Preserve verbatim until the per-entity consumer lands.
                push_unique(slots, InterpolationSlot::from(trimmed));
                out.push('{');
                out.push_str(inner);
                out.push('}');
            } else if rest == "index" {
                // `{$index}` - iteration row index. Preserve verbatim
                // so the substitution pass sees the same token at
                // runtime. Only meaningful inside `<for>` bodies but
                // we record the slot unconditionally - the spawner
                // resolves to empty string with a trace outside of
                // iteration scope.
                push_unique(slots, InterpolationSlot::RowIndex);
                out.push('{');
                out.push_str(inner);
                out.push('}');
            } else {
                // Plain `$name` -> `name`. Record a Global slot.
                push_unique(slots, InterpolationSlot::Global(rest.to_string()));
                out.push('{');
                out.push_str(rest);
                out.push('}');
            }
        } else if let Some(field) = trimmed.strip_prefix("row.") {
            // `{row.field}` - iteration-scope field, only meaningful
            // inside a `<for>` body. Record the slot and preserve the
            // placeholder verbatim so the spawner's substitution pass
            // recognises it at reconcile time.
            push_unique(slots, InterpolationSlot::Row(field.to_string()));
            out.push('{');
            out.push_str(inner);
            out.push('}');
        } else if trimmed == "idx" && in_for {
            // Legacy `{idx}` alias for `{$index}` - only treated as a
            // row-index slot inside `<for>` so the rest of the codebase
            // can still use `idx` as a normal global-signal name when
            // there's no iteration scope.
            push_unique(slots, InterpolationSlot::RowIndex);
            let (line, col) = line_col_of(src, body_offset + lt);
            lint_findings.push(LintFinding {
                kind: LintKind::BareInterpolation,
                severity: LintSeverity::Info,
                message:
                    "`{idx}` is the legacy alias for the iteration index - prefer `{$index}` so reviewers can grep for the iteration-scope reference"
                        .to_string(),
                line,
                col,
                suggest: Some("{$index}".to_string()),
            });
            out.push('{');
            out.push_str(inner);
            out.push('}');
        } else if is_bare_interpolation_token(trimmed) {
            // Bare `{name}` - emit the deprecation finding and keep
            // the placeholder verbatim (the IR / runtime path is
            // unchanged). Resolution stays Global to preserve back-
            // compat even inside `<for>` bodies; the row-aware
            // suggestion only nudges the author to disambiguate.
            push_unique(slots, InterpolationSlot::Global(trimmed.to_string()));
            let (line, col) = line_col_of(src, body_offset + lt);
            let (message, suggest) = if in_for {
                (
                    format!(
                        "Inside `<for>`: prefer `{{row.{trimmed}}}` (iteration field) or `{{${trimmed}}}` (global signal); bare `{{{trimmed}}}` is ambiguous and currently resolves to global"
                    ),
                    // The structured suggest points at the row form
                    // since iteration fields are the common case
                    // inside a `<for>`. `lumenc fix` consumers can
                    // surface both shapes via the message body.
                    Some(format!("{{row.{trimmed}}}")),
                )
            } else {
                (
                    format!(
                        "`{{{trimmed}}}` is the legacy shorthand for `{{${trimmed}}}` - prefer the explicit `$`-prefixed form so reviewers can grep for signal sites"
                    ),
                    Some(format!("{{${trimmed}}}")),
                )
            };
            lint_findings.push(LintFinding {
                kind: LintKind::BareInterpolation,
                severity: LintSeverity::Info,
                message,
                line,
                col,
                suggest,
            });
            out.push('{');
            out.push_str(inner);
            out.push('}');
        } else {
            // Not an interpolation token (CSS-token brace, etc.) -
            // pass through verbatim.
            out.push('{');
            out.push_str(inner);
            out.push('}');
        }
        i = gt + 1;
    }
    out
}

/// Append `slot` to `slots` only if no equal slot is already present.
/// Keeps the per-element slot list deduplicated so the spawner doesn't
/// re-resolve the same placeholder more than once per row.
fn push_unique(slots: &mut Vec<InterpolationSlot>, slot: InterpolationSlot) {
    if !slots.iter().any(|s| s == &slot) {
        slots.push(slot);
    }
}

/// Is `s` a single identifier-shaped interpolation token (no `$`
/// prefix)? Used to filter `{ k: v }` map literals and other non-
/// interpolation brace content out of the bare-shorthand lint.
fn is_bare_interpolation_token(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// Map a byte offset in `src` to a 1-based (line, col).
fn line_col_of(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    let offset = offset.min(src.len());
    for (i, c) in src.char_indices() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// The one truthiness rule every boolean attribute in the markup
/// follows. `true`, `yes`, `1` and an empty value (`disabled=""`,
/// the closest the XML shape gets to HTML's bare attribute) are true;
/// `false`, `no`, `0` are false. `None` means the value is
/// outside the set - callers report [`LintKind::BooleanAttribute`] and
/// read the attribute as false.
///
/// Matching is exact apart from surrounding whitespace: `True` is not
/// accepted.
fn parse_bool_value(value: &str) -> Option<bool> {
    match value.trim() {
        "" | "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// Push the finding a boolean attribute raises when its value is
/// outside [`parse_bool_value`]'s set. `offset` anchors at the value in
/// the expanded source so editors can point at the exact span.
fn push_bool_lint(
    tag: &str,
    name: &str,
    value: &str,
    src: &str,
    offset: usize,
    lint_findings: &mut Vec<LintFinding>,
) {
    let (line, col) = line_col_of(src, offset);
    lint_findings.push(LintFinding {
        kind: LintKind::BooleanAttribute,
        severity: LintSeverity::Warn,
        message: format!(
            "`<{tag} {name}=\"{value}\">` is not a boolean value; write `true`, `yes`, `1` or an empty value for true, or `false`, `no`, `0` for false. Reading it as false."
        ),
        line,
        col,
        // No machine-applicable fix: which of true / false the author
        // meant is exactly what the value failed to say.
        suggest: None,
    });
}

/// Read a boolean attribute straight off `node`. Absent = false. Used
/// by the desugar passes (`<tab disabled>`, `<option disabled>`,
/// `<menuitem disabled>`, `<root frameless>`), which read child
/// attributes without going through [`apply_attribute`].
fn bool_attribute_of(
    node: roxmltree::Node<'_, '_>,
    name: &str,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
) -> bool {
    let Some(attr) = node.attributes().find(|a| a.name() == name) else {
        return false;
    };
    parse_bool_value(attr.value()).unwrap_or_else(|| {
        push_bool_lint(
            node.tag_name().name(),
            name,
            attr.value(),
            src,
            attr.range_value().start,
            lint_findings,
        );
        false
    })
}

/// Source anchor plus lint sink for [`apply_attribute`]. Carrying the
/// offset (rather than a resolved line/col) keeps the per-attribute
/// cost at zero: the source scan only runs when a value is rejected.
struct AttrCtx<'a> {
    /// Expanded markup source, for line/col resolution.
    src: &'a str,
    /// Byte offset of the current attribute's value in `src`.
    value_offset: usize,
    /// Findings collected for the whole document.
    lint_findings: &'a mut Vec<LintFinding>,
}

impl AttrCtx<'_> {
    /// Resolve a boolean attribute value, reporting the shared
    /// truthiness rule when it does not apply.
    fn bool_value(&mut self, tag: &str, name: &str, value: &str) -> bool {
        parse_bool_value(value).unwrap_or_else(|| {
            push_bool_lint(
                tag,
                name,
                value,
                self.src,
                self.value_offset,
                self.lint_findings,
            );
            false
        })
    }
}

/// Replace every `{name}` token in `body` with the matching attribute
/// value. Unknown placeholders are left intact so misuse fails loudly
/// downstream rather than silently producing an attribute named `{x}`.
fn substitute_placeholders(body: &str, attrs: &[(String, String)]) -> String {
    let mut out = body.to_string();
    for (k, v) in attrs {
        if k == "template" {
            continue;
        }
        let pat = format!("{{{k}}}");
        if out.contains(&pat) {
            out = out.replace(&pat, v);
        }
    }
    out
}

/// Scan the top-level `<root>` children for a single `<menubar>`
/// block and parse its contents into a `MenuBarSpec`. The block is
/// stripped from the layout tree by `build_element` (it skips
/// `<menubar>` children during traversal). Returns `None` when no
/// `<menubar>` is present.
fn extract_menubar(
    root: roxmltree::Node,
) -> Result<Option<crate::layout_ir::MenuBarSpec>, ParseError> {
    use crate::layout_ir::{MenuBarSpec, MenuEntrySpec, MenuSpec};
    let mut iter = root
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "menubar");
    let Some(node) = iter.next() else {
        return Ok(None);
    };
    if iter.next().is_some() {
        return Err(ParseError::Xml(format!(
            "duplicate <menubar> at byte {}: at most one allowed per <root>",
            node.range().start
        )));
    }
    let mut bar = MenuBarSpec::default();
    for menu_node in node.children().filter(|c| c.is_element()) {
        if menu_node.tag_name().name() != "menu" {
            return Err(ParseError::Xml(format!(
                "<menubar> child at byte {} must be <menu>, got <{}>",
                menu_node.range().start,
                menu_node.tag_name().name()
            )));
        }
        let label = menu_node
            .attribute("label")
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ParseError::Xml(format!(
                    "<menu> at byte {} requires a label=\"...\" attribute",
                    menu_node.range().start
                ))
            })?;
        let mut items = Vec::new();
        for child in menu_node.children().filter(|c| c.is_element()) {
            match child.tag_name().name() {
                "menuitem" => {
                    let id = child
                        .attribute("id")
                        .map(|s| s.to_string())
                        .ok_or_else(|| {
                            ParseError::Xml(format!(
                                "<menuitem> at byte {} requires an id=\"...\" attribute",
                                child.range().start
                            ))
                        })?;
                    let item_label = child
                        .attribute("label")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| id.clone());
                    let accelerator = child.attribute("accel").map(|s| s.to_string());
                    items.push(MenuEntrySpec::Item {
                        id,
                        label: item_label,
                        accelerator,
                    });
                }
                "separator" => items.push(MenuEntrySpec::Separator),
                other => {
                    return Err(ParseError::Xml(format!(
                        "<menu> child at byte {} must be <menuitem> or <separator>, got <{}>",
                        child.range().start,
                        other
                    )));
                }
            }
        }
        bar.menus.push(MenuSpec { label, items });
    }
    Ok(Some(bar))
}

/// Maximum element-tree nesting depth. Past this, `build_element` returns a
/// `ParseError` instead of recursing - a deeply nested (even well-formed)
/// `.lmn` would otherwise overflow the stack (a SIGSEGV, not a catchable
/// panic). Kept consistent with the template-expansion cap.
const MAX_ELEMENT_DEPTH: u32 = 32;

fn build_element(
    node: roxmltree::Node,
    script_buf: &mut String,
    external_scripts: &mut Vec<String>,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    in_for_body: bool,
    depth: u32,
) -> Result<Element, ParseError> {
    if depth > MAX_ELEMENT_DEPTH {
        return Err(ParseError::Xml(format!(
            "element nesting exceeded depth {MAX_ELEMENT_DEPTH} at byte {} (too deeply nested)",
            node.range().start
        )));
    }
    let tag = node.tag_name().name().to_string();
    // Built-in tags first; fall back to the lumen-widget runtime
    // registry so `#[derive(Widget)] #[widget(tag="my-thing")]` widgets
    // that called `MyThing::register()` at startup are accepted instead
    // of rejected as `UnknownTag`. The Widget derive populates the
    // registry; lumenc itself only consults it.
    if !KNOWN_TAGS.contains(&tag.as_str()) && !lumen_widget::is_widget_tag_registered(tag.as_str())
    {
        return Err(ParseError::UnknownTag(tag, node.range().start));
    }

    // `<tooltip text="..." delay="...">...trigger...</tooltip>` flattens to
    // the trigger child with a `TooltipSpec` attached. The trigger
    // must be a single layout element; multi-child tooltips are
    // rejected here so authors get a clear parse error instead of a
    // silent first-child pickup.
    if tag == "tooltip" {
        let text = node.attribute("text").unwrap_or("").to_string();
        let delay_ms = node.attribute("delay").and_then(|s| s.parse::<u32>().ok());
        let offset = node.attribute("offset").and_then(|s| s.parse::<f32>().ok());
        let mut child_nodes = node
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() != "script");
        let Some(child) = child_nodes.next() else {
            return Err(ParseError::Xml(format!(
                "<tooltip text=\"{text}\"> at byte {} must wrap exactly one trigger element",
                node.range().start
            )));
        };
        if child_nodes.next().is_some() {
            return Err(ParseError::Xml(format!(
                "<tooltip text=\"{text}\"> at byte {} must wrap exactly one trigger element \
                 (got >1)",
                node.range().start
            )));
        }
        let mut elem = build_element(
            child,
            script_buf,
            external_scripts,
            src,
            lint_findings,
            in_for_body,
            depth + 1,
        )?;
        elem.attrs.tooltip = Some(crate::layout_ir::TooltipSpec {
            text,
            delay_ms,
            offset,
        });
        return Ok(elem);
    }

    // `<tabs bind-value="active_tab">` collapses into a column with a
    // tab strip on top + one `<if eq>` body per `<tab>`. The strip
    // buttons each carry a `tab_strip_button = (signal, value)` so
    // the runtime click->signal system can flip the active tab
    // without an author-side script.
    if tag == "tabs" {
        let signal_name = node
            .attribute("bind-value")
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ParseError::Xml(format!(
                    "<tabs> at byte {} requires bind-value=\"signal-name\"",
                    node.range().start
                ))
            })?;
        let mut tab_buttons: Vec<Element> = Vec::new();
        let mut tab_bodies: Vec<Element> = Vec::new();
        let mut first_name: Option<String> = None;
        for child in node
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() != "script")
        {
            if child.tag_name().name() != "tab" {
                return Err(ParseError::Xml(format!(
                    "<tabs> child at byte {} must be <tab>, got <{}>",
                    child.range().start,
                    child.tag_name().name()
                )));
            }
            let tab_name = child
                .attribute("name")
                .map(|s| s.to_string())
                .ok_or_else(|| {
                    ParseError::Xml(format!(
                        "<tab> at byte {} requires a name=\"...\" attribute",
                        child.range().start
                    ))
                })?;
            let tab_label = child
                .attribute("label")
                .map(|s| s.to_string())
                .unwrap_or_else(|| tab_name.clone());
            if first_name.is_none() {
                first_name = Some(tab_name.clone());
            }
            // Strip button - `<button>` with TabStripButton attrs so
            // the runtime knows to write `signal_name = tab_name` on
            // click.
            let mut btn = Element {
                tag: "button".to_string(),
                attrs: Attributes {
                    height: Some(LengthSpec::Px(36.0)),
                    grow: Some(1.0),
                    tab_index: Some(0),
                    padding: Some(crate::layout_ir::Edges {
                        left: 12.0,
                        right: 12.0,
                        top: 0.0,
                        bottom: 0.0,
                        ..crate::layout_ir::Edges::default()
                    }),
                    ..Attributes::default()
                },
                children: Vec::new(),
                interpolations: Vec::new(),
            };
            btn.attrs.text = Some(tab_label);
            btn.attrs.classes = vec!["tab-btn".to_string()];
            btn.attrs.tab_strip = Some((signal_name.clone(), tab_name.clone()));
            // `disabled="true"` on a `<tab>`: the strip button is
            // unclickable, skipped by arrow nav, `:disabled`-styled.
            // The body `<if>` still mounts if the tab is somehow
            // activated by a script write - parity with QTabBar, where
            // disabling a tab doesn't unmount its page.
            btn.attrs.disabled = bool_attribute_of(child, "disabled", src, lint_findings);
            tab_buttons.push(btn);
            // Body - wrap children in `<if signal=... mode="hide" eq=tab_name>...</if>`
            let mut body_children: Vec<Element> = Vec::new();
            for body_child in child
                .children()
                .filter(|c| c.is_element() && c.tag_name().name() != "script")
            {
                body_children.push(build_element(
                    body_child,
                    script_buf,
                    external_scripts,
                    src,
                    lint_findings,
                    in_for_body,
                    depth + 1,
                )?);
            }
            let mut if_block = Element {
                tag: "if".to_string(),
                attrs: Attributes::default(),
                children: body_children,
                interpolations: Vec::new(),
            };
            if_block.attrs.if_signal = Some(signal_name.clone());
            if_block.attrs.if_eq = Some(tab_name);
            if_block.attrs.if_mode = crate::layout_ir::IfModeSpec::Hide;
            tab_bodies.push(if_block);
        }
        // Seed the default active tab so the first body mounts at
        // startup. Stored on the synthetic strip via a marker the
        // spawn pass converts into a Signals seed.
        // Synthetic elements bypass `build_element`'s default-setting
        // pass, so we set `flex` explicitly here (otherwise children
        // stack at the same position because taffy gets no direction).
        let mut strip = Element {
            tag: "row".to_string(),
            attrs: Attributes {
                flex: Some(FlexAxis::Row),
                width: Some(LengthSpec::Percent(100.0)),
                ..Attributes::default()
            },
            children: tab_buttons,
            interpolations: Vec::new(),
        };
        strip.attrs.classes = vec!["tab-strip".to_string()];
        strip.attrs.gap = Some(4.0);
        let mut column = Element {
            tag: "column".to_string(),
            attrs: Attributes {
                flex: Some(FlexAxis::Column),
                width: Some(LengthSpec::Percent(100.0)),
                ..Attributes::default()
            },
            children: std::iter::once(strip).chain(tab_bodies).collect(),
            interpolations: Vec::new(),
        };
        column.attrs.classes = vec!["tabs".to_string()];
        if let Some(default) = first_name {
            column.attrs.signal_seed = Some((signal_name, default));
        }
        return Ok(column);
    }

    // `<tab>` outside of `<tabs>` is meaningless - collapsing it to
    // its first child would be silently surprising. Reject so authors
    // get a clear error.
    if tag == "tab" {
        return Err(ParseError::Xml(format!(
            "<tab> at byte {} may only appear inside <tabs>",
            node.range().start
        )));
    }

    // `<dropdown bind-value="signal">` + `<option value="x"
    // label="X"/>` children collapse to a column with a header button
    // The header text binds to the active value signal; clicking it toggles a synthetic
    // `__dropdown_open:<signal>` flag (managed by `lumen_primitives::DropdownButton`).
    // Each option button writes the value and clears the flag.
    if tag == "dropdown" {
        let signal_name = node
            .attribute("bind-value")
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ParseError::Xml(format!(
                    "<dropdown> at byte {} requires bind-value=\"signal-name\"",
                    node.range().start
                ))
            })?;
        let open_signal = format!("__dropdown_open:{signal_name}");
        // An authored `placeholder` means "start with nothing picked":
        // the header shows the placeholder until a click or a script
        // writes the value signal. Without one, the first `<option>`
        // seeds the signal, so the dropdown opens on a real selection
        // the way `<tabs>` opens on its first tab.
        let placeholder = node.attribute("placeholder");
        let seed_first_option = placeholder.is_none();
        let placeholder = placeholder.unwrap_or_default().to_string();
        let mut options: Vec<Element> = Vec::new();
        let mut option_specs: Vec<(String, String, bool)> = Vec::new();
        let mut default_value: Option<String> = None;
        for child in node
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() != "script")
        {
            if child.tag_name().name() != "option" {
                return Err(ParseError::Xml(format!(
                    "<dropdown> child at byte {} must be <option>, got <{}>",
                    child.range().start,
                    child.tag_name().name()
                )));
            }
            let value = child
                .attribute("value")
                .map(|s| s.to_string())
                .ok_or_else(|| {
                    ParseError::Xml(format!(
                        "<option> at byte {} requires value=\"...\"",
                        child.range().start
                    ))
                })?;
            let label = child
                .attribute("label")
                .map(|s| s.to_string())
                .unwrap_or_else(|| value.clone());
            // `disabled="true"` on an `<option>` - same truthiness rule
            // every boolean attribute follows.
            let disabled = bool_attribute_of(child, "disabled", src, lint_findings);
            option_specs.push((value.clone(), label.clone(), disabled));
            if default_value.is_none() {
                default_value = Some(value.clone());
            }
            let mut opt_btn = Element {
                tag: "button".to_string(),
                attrs: Attributes {
                    height: Some(LengthSpec::Px(32.0)),
                    width: Some(LengthSpec::Percent(100.0)),
                    padding: Some(crate::layout_ir::Edges {
                        left: 12.0,
                        right: 12.0,
                        top: 0.0,
                        bottom: 0.0,
                        ..crate::layout_ir::Edges::default()
                    }),
                    ..Attributes::default()
                },
                children: Vec::new(),
                interpolations: Vec::new(),
            };
            opt_btn.attrs.text = Some(label);
            opt_btn.attrs.classes = vec!["dropdown-option".to_string()];
            opt_btn.attrs.dropdown_option = Some((signal_name.clone(), value, open_signal.clone()));
            opt_btn.attrs.disabled = disabled;
            options.push(opt_btn);
        }
        // Header button - text bound to the active value (placeholder
        // when empty). Click flips the open-panel signal. Focusable
        // (`tab-index 0`, same explicit opt-in the `<tabs>` strip uses
        // - synthetic elements bypass the per-tag default pass) so the
        // closed combobox takes keyboard interaction like a
        // `QComboBox`: Up/Down value stepping, Alt+Down / Space /
        // Enter open, type-ahead.
        let mut header = Element {
            tag: "button".to_string(),
            attrs: Attributes {
                width: Some(LengthSpec::Percent(100.0)),
                height: Some(LengthSpec::Px(36.0)),
                tab_index: Some(0),
                padding: Some(crate::layout_ir::Edges {
                    left: 12.0,
                    right: 12.0,
                    top: 0.0,
                    bottom: 0.0,
                    ..crate::layout_ir::Edges::default()
                }),
                ..Attributes::default()
            },
            children: Vec::new(),
            interpolations: Vec::new(),
        };
        header.attrs.text = Some(placeholder);
        header.attrs.classes = vec!["dropdown-button".to_string()];
        header.attrs.bind = Some(crate::layout_ir::BindSpec {
            kind: BindKind::Text,
            name: signal_name.clone(),
        });
        header.attrs.dropdown_button = Some(crate::layout_ir::DropdownButtonSpec {
            open_signal: open_signal.clone(),
            value_signal: signal_name.clone(),
            options: option_specs,
        });
        // `Attributes` carries one `signal_seed` slot per element and
        // the wrapper column's is spent on the open-panel flag, so the
        // value seed rides on the header button. `spawn_element` reads
        // the slot on every element and seeds only when nothing has
        // written the signal yet, so a script's own initial value still
        // wins.
        if seed_first_option && let Some(default) = default_value {
            header.attrs.signal_seed = Some((signal_name.clone(), default));
        }

        // Panel - absolute-positioned overlay with the options stack.
        let mut panel = Element {
            tag: "column".to_string(),
            attrs: Attributes {
                flex: Some(FlexAxis::Column),
                position: Some(PositionSpec::Absolute),
                inset: Some(crate::layout_ir::Edges {
                    left: 0.0,
                    right: 0.0,
                    top: 40.0,
                    bottom: -1.0,
                    ..crate::layout_ir::Edges::default()
                }),
                ..Attributes::default()
            },
            children: options,
            interpolations: Vec::new(),
        };
        panel.attrs.classes = vec!["dropdown-panel".to_string()];
        // Tag the panel so the runtime can dismiss it on an outside
        // click and flip it above the trigger near the viewport bottom.
        panel.attrs.popup_panel = Some(open_signal.clone());

        // Wrap the panel in `<if eq="true" mode="hide">` keyed on the
        // open signal so it shows / hides without despawning options.
        let mut panel_if = Element {
            tag: "if".to_string(),
            attrs: Attributes::default(),
            children: vec![panel],
            interpolations: Vec::new(),
        };
        panel_if.attrs.if_signal = Some(open_signal.clone());
        panel_if.attrs.if_eq = Some("true".to_string());
        panel_if.attrs.if_mode = crate::layout_ir::IfModeSpec::Hide;

        let mut column = Element {
            tag: "column".to_string(),
            attrs: Attributes {
                flex: Some(FlexAxis::Column),
                position: Some(PositionSpec::Relative),
                ..Attributes::default()
            },
            children: vec![header, panel_if],
            interpolations: Vec::new(),
        };
        column.attrs.classes = vec!["dropdown".to_string()];
        // Seed the open-panel signal to "false" so the panel hides at
        // startup.
        column.attrs.signal_seed = Some((open_signal, "false".to_string()));
        return Ok(column);
    }

    // `<option>` outside of `<dropdown>` is meaningless - same
    // rejection rule as `<tab>` outside of `<tabs>`.
    if tag == "option" {
        return Err(ParseError::Xml(format!(
            "<option> at byte {} may only appear inside <dropdown>",
            node.range().start
        )));
    }

    // `<menu id="m">` collapses into an `<if eq="true">` keyed on `__menu_open:m`.
    // Body becomes an `<overlay class="menu-panel">` carrying one button per `<menuitem>` and a thin spacer per `<separator>`.
    // `<menubar>`-nested `<menu>` blocks are extracted upstream by `extract_menubar` and never reach `build_element`.
    if tag == "menu" {
        let menu_id = node.attribute("id").map(|s| s.to_string()).ok_or_else(|| {
            ParseError::Xml(format!(
                "<menu> at byte {} requires id=\"...\"",
                node.range().start
            ))
        })?;
        let open_signal = format!("__menu_open:{menu_id}");
        let mut body: Vec<Element> = Vec::new();
        for child in node
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() != "script")
        {
            match child.tag_name().name() {
                "menuitem" => {
                    let item_id =
                        child
                            .attribute("id")
                            .map(|s| s.to_string())
                            .ok_or_else(|| {
                                ParseError::Xml(format!(
                                    "<menuitem> at byte {} requires id=\"...\"",
                                    child.range().start
                                ))
                            })?;
                    let label = child
                        .attribute("label")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| item_id.clone());
                    let mut btn = Element {
                        tag: "button".to_string(),
                        attrs: Attributes {
                            height: Some(LengthSpec::Px(28.0)),
                            width: Some(LengthSpec::Percent(100.0)),
                            padding: Some(crate::layout_ir::Edges {
                                left: 12.0,
                                right: 12.0,
                                top: 0.0,
                                bottom: 0.0,
                                ..crate::layout_ir::Edges::default()
                            }),
                            ..Attributes::default()
                        },
                        children: Vec::new(),
                        interpolations: Vec::new(),
                    };
                    btn.attrs.text = Some(label);
                    btn.attrs.classes = vec!["menu-item".to_string()];
                    btn.attrs.menu_item = Some((open_signal.clone(), item_id));
                    // `disabled="true"` on a `<menuitem>`: unclickable,
                    // skipped by arrow nav, `:disabled`-styled.
                    btn.attrs.disabled = bool_attribute_of(child, "disabled", src, lint_findings);
                    body.push(btn);
                }
                "separator" => {
                    let mut sep = Element {
                        tag: "spacer".to_string(),
                        attrs: Attributes {
                            height: Some(LengthSpec::Px(1.0)),
                            width: Some(LengthSpec::Percent(100.0)),
                            ..Attributes::default()
                        },
                        children: Vec::new(),
                        interpolations: Vec::new(),
                    };
                    sep.attrs.classes = vec!["menu-separator".to_string()];
                    body.push(sep);
                }
                other => {
                    return Err(ParseError::Xml(format!(
                        "<menu> child at byte {} must be <menuitem> or <separator>, got <{}>",
                        child.range().start,
                        other
                    )));
                }
            }
        }
        // Synthetic elements bypass `build_element`'s per-tag default
        // pass (same caveat as the `<tabs>` strip), so the overlay
        // geometry must be set explicitly here. Without it the panel
        // spawned as a plain in-flow Row: menu items laid out inline
        // horizontally in the document flow instead of stacking in a
        // floating panel (spec section 8 - a menu panel is an overlay that
        // never participates in ancestor layout). `right` / `bottom`
        // stay `NaN` (= auto) so the panel shrink-wraps its items; the
        // items' `width: 100%` then equalises them to the widest.
        let mut panel = Element {
            tag: "overlay".to_string(),
            attrs: Attributes {
                flex: Some(FlexAxis::Column),
                position: Some(PositionSpec::Absolute),
                inset: Some(crate::layout_ir::Edges {
                    left: 0.0,
                    top: 0.0,
                    right: f32::NAN,
                    bottom: f32::NAN,
                    ..crate::layout_ir::Edges::default()
                }),
                min_width: Some(LengthSpec::Px(160.0)),
                ..Attributes::default()
            },
            children: body,
            interpolations: Vec::new(),
        };
        panel.attrs.classes = vec!["menu-panel".to_string()];
        panel.attrs.id = Some(menu_id);
        // Same popup wiring as the dropdown panel: outside-click
        // dismissal keys off the shared open signal.
        panel.attrs.popup_panel = Some(open_signal.clone());

        let mut if_block = Element {
            tag: "if".to_string(),
            attrs: Attributes::default(),
            children: vec![panel],
            interpolations: Vec::new(),
        };
        if_block.attrs.if_signal = Some(open_signal.clone());
        if_block.attrs.if_eq = Some("true".to_string());
        if_block.attrs.if_mode = crate::layout_ir::IfModeSpec::Hide;
        if_block.attrs.signal_seed = Some((open_signal, "false".to_string()));
        return Ok(if_block);
    }

    if matches!(tag.as_str(), "menuitem" | "separator") {
        return Err(ParseError::Xml(format!(
            "<{tag}> at byte {} may only appear inside <menu> or <menubar>",
            node.range().start
        )));
    }

    // Collapse `<date-picker>` and `<time-picker>` into a validated `<input>` with a built-in pattern that enforces the shape.
    // Form validation mirrors the value into `valid:<id>`; authors hook the existing `text-input` commit dispatch on submit.
    if matches!(tag.as_str(), "date-picker" | "time-picker") {
        let is_time = tag == "time-picker";
        let signal_name = node
            .attribute("bind-value")
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ParseError::Xml(format!(
                    "<{tag}> at byte {} requires bind-value=\"signal-name\"",
                    node.range().start
                ))
            })?;
        let id = node.attribute("id").map(|s| s.to_string());
        let placeholder = node
            .attribute("placeholder")
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                if is_time {
                    "HH:MM".to_string()
                } else {
                    "YYYY-MM-DD".to_string()
                }
            });
        // Structural patterns, checked by `lumen_primitives::validation`:
        // `shape:time` is 24-hour `HH:MM` (hour 00-23, minute 00-59),
        // `shape:date` is ISO 8601 `YYYY-MM-DD` (month 01-12, day
        // 01-31). Both are shape checks, not calendar checks - 2026-02-31
        // passes.
        let pattern = if is_time { "shape:time" } else { "shape:date" };
        let class = if is_time {
            "time-picker"
        } else {
            "date-picker"
        };
        let mut input = Element {
            tag: "input".to_string(),
            attrs: Attributes {
                width: Some(LengthSpec::Px(180.0)),
                ..Attributes::default()
            },
            children: Vec::new(),
            interpolations: Vec::new(),
        };
        input.attrs.placeholder = Some(placeholder);
        input.attrs.id = id;
        input.attrs.bind = Some(crate::layout_ir::BindSpec {
            kind: BindKind::Text,
            name: signal_name,
        });
        input.attrs.pattern = Some(pattern.to_string());
        input.attrs.classes = vec![class.to_string()];
        return Ok(input);
    }

    let mut attrs = Attributes {
        // Apply tag defaults that act like CSS user-agent styles.
        // `<scroll>` and `<root>` are columns by default; the horizontal
        // axis is opt-in via `scroll="x"` (handled in apply_attribute,
        // which then also flips `flex` so the children stack left->right).
        flex: match tag.as_str() {
            // `<for>` defaults to column because the common case is a
            // vertical list of rows - author opts out by wrapping the
            // body in an explicit `<row>` or setting another flex
            // direction on a parent.
            "column" | "scroll" | "root" | "overlay" | "for" | "dialog" => Some(FlexAxis::Column),
            "row" | "title-bar" | "checkbox" | "radio" | "progress" => Some(FlexAxis::Row),
            _ => None,
        },
        position: if matches!(tag.as_str(), "overlay" | "dialog") {
            Some(PositionSpec::Absolute)
        } else {
            None
        },
        inset: if matches!(tag.as_str(), "overlay" | "dialog") {
            Some(crate::layout_ir::Edges {
                left: 0.0,
                right: 0.0,
                top: 0.0,
                bottom: 0.0,
                ..crate::layout_ir::Edges::default()
            })
        } else {
            None
        },
        align: if tag == "dialog" {
            Some(FlexAlign::Center)
        } else {
            None
        },
        justify: if tag == "dialog" {
            Some(FlexJustify::Center)
        } else {
            None
        },
        // Per-tag default sizing (root/title-bar dimensions, control
        // minimums) is a true user-agent layer now: it lives in
        // `lumen_runtime::skins::UA` (`crates/runtime/src/skins/ua.css`),
        // an always-on stylesheet folded into the cascade beneath any
        // skin and beneath author CSS - not in these parse-time
        // `Attributes` fields, and not fully in Rust either. Two cases
        // that don't fit a plain CSS rule (a `<switch>` `min-width` that
        // only applies when `width` is also unset, and the `<input>` /
        // `<textarea>` `overflow: hidden` default backing off a shorthand
        // `overflow` authored anywhere) stay in
        // `lumen_runtime::spawn::apply_ua_style_defaults`, applied at
        // spawn time (D3 / task #29). The old parse-time width / height /
        // min-width tables that used to sit in these `Attributes` fields
        // are gone: they made the defaults inline-origin -
        // `restore_inline_origin` then re-applied them *over* author CSS
        // after the cascade, so rules like `button { min-width: 40px }`
        // could never win. The label / button height + min-width entries
        // were dropped outright: the W2.5 `CosmicShaper::measure` wiring
        // supplies real text intrinsic sizes, which those hardcoded
        // defaults overrode unhelpfully (e.g. a 4-line label forced to
        // 24px).
        scroll: if tag == "scroll" {
            Some(ScrollAxisSpec::Y)
        } else {
            None
        },
        // <input> seeds an empty editable buffer so the focus router
        // and the renderer have something to point at without the
        // author having to write `text=""`.
        text: if tag == "input" {
            Some(String::new())
        } else {
            None
        },
        // Make every <input> / <textarea> / <button> / <toggle> /
        // <slider> / <checkbox> focusable by default - implicit tabindex
        // 0, mirroring the web (form controls are in the sequential
        // focus order without an explicit `tabindex`) and Qt
        // (`QLineEdit`/`QTextEdit`/`QAbstractButton` default
        // `focusPolicy = StrongFocus`). Author can override with explicit
        // `tab-index="-1"`. `<radio>` seeds -1: the roving-tabindex
        // system (`lumen_primitives::sync_radio_tab_index`) promotes
        // exactly one group member to 0 at runtime, so Tab enters/leaves
        // the GROUP rather than each member.
        tab_index: match tag.as_str() {
            "input" | "textarea" | "button" | "toggle" | "switch" | "slider" | "checkbox" => {
                Some(0)
            }
            "radio" => Some(-1),
            _ => None,
        },
        // `<spacer />` defaults grow=1 so it eats the remaining axis.
        grow: if tag == "spacer" { Some(1.0) } else { None },
        ..Default::default()
    };

    // Track byte offsets (in expanded source) of attributes that may
    // carry `{interpolation}` placeholders so the wave-B
    // bare-interpolation lint can report accurate line/col. We collect
    // these BEFORE `apply_attribute` mutates `attrs` because the
    // roxmltree `Attribute` handle owns the offset info.
    let mut text_off: Option<usize> = None;
    let mut placeholder_off: Option<usize> = None;
    let mut src_off: Option<usize> = None;
    let mut id_off: Option<usize> = None;
    let mut class_off: Option<usize> = None;
    for a in node.attributes() {
        let off = a.range_value().start;
        match a.name() {
            "text" => text_off = Some(off),
            "placeholder" => placeholder_off = Some(off),
            "src" => src_off = Some(off),
            "id" => id_off = Some(off),
            "class" => class_off = Some(off),
            _ => {}
        }
        let mut ctx = AttrCtx {
            src,
            value_offset: off,
            // Reborrow: the sink outlives every attribute in the loop.
            lint_findings: &mut *lint_findings,
        };
        apply_attribute(&tag, a.name(), a.value(), &mut attrs, &mut ctx)?;
    }

    // Normalise `{$name}` -> `{name}` in string-valued attrs that may
    // carry interpolation placeholders. Keeps the IR backwards-
    // compatible with the legacy bare-name form while letting authors
    // write the preferred `$`-prefixed shape. See `normalize_dollar_interpolation`.
    //
    // Wave-C: collect [`InterpolationSlot`]s into `slots` so the
    // spawner / for-block reconciler can resolve each placeholder
    // against the right scope (global signals, the current iteration
    // record, the row index, ...) without re-classifying the brace body
    // at runtime. Order preserves first-appearance across the
    // text/placeholder/src/id/classes scan.
    let mut slots: Vec<InterpolationSlot> = Vec::new();
    if let Some(t) = attrs.text.take() {
        let off = text_off.unwrap_or(0);
        attrs.text = Some(normalize_dollar_interpolation(
            &t,
            off,
            src,
            lint_findings,
            &mut slots,
            in_for_body,
        ));
    }
    if let Some(p) = attrs.placeholder.take() {
        let off = placeholder_off.unwrap_or(0);
        attrs.placeholder = Some(normalize_dollar_interpolation(
            &p,
            off,
            src,
            lint_findings,
            &mut slots,
            in_for_body,
        ));
    }
    if let Some(src_val) = attrs.src.take() {
        let off = src_off.unwrap_or(0);
        attrs.src = Some(normalize_dollar_interpolation(
            &src_val,
            off,
            src,
            lint_findings,
            &mut slots,
            in_for_body,
        ));
    }
    if let Some(id) = attrs.id.take() {
        let off = id_off.unwrap_or(0);
        attrs.id = Some(normalize_dollar_interpolation(
            &id,
            off,
            src,
            lint_findings,
            &mut slots,
            in_for_body,
        ));
    }
    if !attrs.classes.is_empty() {
        let off = class_off.unwrap_or(0);
        attrs.classes = attrs
            .classes
            .into_iter()
            .map(|c| {
                normalize_dollar_interpolation(&c, off, src, lint_findings, &mut slots, in_for_body)
            })
            .collect();
    }

    // Recurse into children. When the current element is a `<for>`,
    // flip `in_for_body` on so descendants get row-aware
    // interpolation handling (`{row.field}`, `{$index}`, the
    // row-aware bare-interp lint message). The `<for>` element
    // itself doesn't see `in_for_body=true` for its own attrs - the
    // `each=` attribute holds the array signal name, not an
    // iteration-scope reference.
    let child_in_for = in_for_body || tag == "for";
    let mut children = Vec::new();
    for child in node.children() {
        if child.is_text() {
            // Element text content (e.g. `<label>Hi</label>`) becomes the
            // element's `text` attribute when no explicit attribute is set.
            let raw_text = child.text().unwrap_or("");
            let t = raw_text.trim();
            if !t.is_empty() && attrs.text.is_none() {
                // `child.range().start` covers the text node including
                // any leading whitespace `trim()` discards; offset by
                // the lead-skip so the finding points at the `{` rather
                // than the indent before it.
                let lead_skip = raw_text.len() - raw_text.trim_start().len();
                let off = child.range().start + lead_skip;
                attrs.text = Some(normalize_dollar_interpolation(
                    t,
                    off,
                    src,
                    lint_findings,
                    &mut slots,
                    in_for_body,
                ));
            }
        } else if child.is_element() {
            // Special-case <script>: not a layout node.
            // - `<script src="file.rhai"/>` records the path; the runtime
            //   reads + concatenates the file.
            // - `<script>body</script>` collects the inline text into the
            //   shared script buffer (still useful for tiny snippets that
            //   don't contain XML-illegal characters).
            // `<menubar>` is stripped from the layout tree by
            // `extract_menubar` before `build_element` runs; skipping
            // here keeps unknown-tag checks happy and prevents the
            // menubar's authoring nesting from being layout-spawned.
            if child.tag_name().name() == "menubar" {
                continue;
            }
            if child.tag_name().name() == "script" {
                if let Some(script_src) = child.attribute("src") {
                    external_scripts.push(script_src.to_string());
                } else if let Some(s) = child.text() {
                    if !script_buf.is_empty() {
                        script_buf.push('\n');
                    }
                    script_buf.push_str(s);
                }
                continue;
            }
            children.push(build_element(
                child,
                script_buf,
                external_scripts,
                src,
                lint_findings,
                child_in_for,
                depth + 1,
            )?);
        }
    }

    // `<button default="true">` gains the `default` class here - after
    // the whole attribute loop, so a later `class="..."` attr can't
    // clobber it - letting skins style `button.default` through the
    // compile-time cascade.
    if attrs.default_button && !attrs.classes.iter().any(|c| c == "default") {
        attrs.classes.push("default".to_string());
    }

    // `<checkbox>` / `<radio>` desugar (W5): the tag itself stays the
    // root (a centred row carrying the control component at spawn);
    // the parser synthesizes the visual part children so every visual
    // is CSS-reachable - `.checkbox-box` / `.radio-dot` for the
    // indicator tile, `.checkbox-label` / `.radio-label` for the
    // caption. Runs AFTER the generic attribute pass so the full attr
    // surface (id, class, bind-*, disabled, sizing...) applies to the
    // root unchanged.
    if tag == "checkbox" || tag == "radio" {
        if tag == "radio" {
            let (Some(group), Some(value)) = (&attrs.radio_group, &attrs.radio_value) else {
                return Err(ParseError::Xml(format!(
                    "<radio> at byte {} requires group=\"signal\" and value=\"...\"",
                    node.range().start
                )));
            };
            // `checked="true"` seeds the group signal (first spawned
            // checked member wins; the runtime falls back to the first
            // enabled member when no one is checked).
            if attrs.checked == Some(true) {
                attrs.signal_seed = Some((group.clone(), value.clone()));
            }
        }
        if attrs.align.is_none() {
            attrs.align = Some(FlexAlign::Center);
        }
        if attrs.gap.is_none() {
            attrs.gap = Some(8.0);
        }
        let (part, box_class, label_class) = if tag == "checkbox" {
            (
                crate::layout_ir::WidgetPart::CheckboxBox,
                "checkbox-box",
                "checkbox-label",
            )
        } else {
            (
                crate::layout_ir::WidgetPart::RadioDot,
                "radio-dot",
                "radio-label",
            )
        };
        let mut indicator = Element {
            tag: "tile".to_string(),
            attrs: Attributes {
                part: Some(part),
                shrink: Some(0.0),
                ..Attributes::default()
            },
            children: Vec::new(),
            interpolations: Vec::new(),
        };
        indicator.attrs.classes = vec![box_class.to_string()];
        let mut synthesized = vec![indicator];
        if let Some(label) = attrs.text.take() {
            let mut lbl = Element {
                tag: "label".to_string(),
                attrs: Attributes::default(),
                children: Vec::new(),
                interpolations: Vec::new(),
            };
            lbl.attrs.text = Some(label);
            lbl.attrs.classes = vec![label_class.to_string()];
            synthesized.push(lbl);
        }
        // Synthesized parts paint first (indicator leads the caption);
        // any authored children follow.
        synthesized.append(&mut children);
        children = synthesized;
    }

    // `<progress>` desugar (W5): the track stays the root; a single
    // absolute-positioned `.progress-fill` tile child is synthesized.
    // The runtime (`lumen_primitives::sync_progress_fill`) drives its
    // width (determinate) or sweep offset (indeterminate); everything
    // else - colors, radius, track height, sweep period - is CSS.
    if tag == "progress" {
        let mut fill = Element {
            tag: "tile".to_string(),
            attrs: Attributes {
                part: Some(crate::layout_ir::WidgetPart::ProgressFill),
                position: Some(PositionSpec::Absolute),
                inset: Some(crate::layout_ir::Edges {
                    left: 0.0,
                    top: 0.0,
                    // NaN = auto: the explicit width / 100% height must
                    // not be over-constrained by the far edges (same
                    // convention as the toggle-knob seed style).
                    right: f32::NAN,
                    bottom: f32::NAN,
                    ..crate::layout_ir::Edges::default()
                }),
                ..Attributes::default()
            },
            children: Vec::new(),
            interpolations: Vec::new(),
        };
        fill.attrs.classes = vec!["progress-fill".to_string()];
        children.insert(0, fill);
    }

    Ok(Element {
        tag,
        attrs,
        children,
        interpolations: slots,
    })
}

fn apply_attribute(
    tag: &str,
    name: &str,
    value: &str,
    attrs: &mut Attributes,
    ctx: &mut AttrCtx<'_>,
) -> Result<(), ParseError> {
    match name {
        "width" => attrs.width = Some(parse_length(tag, name, value)?),
        "height" => attrs.height = Some(parse_length(tag, name, value)?),
        "bg" => attrs.bg = Some(parse_bg(tag, name, value)?),
        "radius" => {
            attrs.radius = Some(parse_f32(tag, name, value)?);
        }
        "padding" => attrs.padding = Some(parse_edges(tag, name, value)?),
        "margin" => attrs.margin = Some(parse_edges(tag, name, value)?),
        "text" => attrs.text = Some(value.to_string()),
        "text-color" => attrs.text_color = Some(parse_color(tag, name, value)?),
        "selection-color" => attrs.selection_color = Some(parse_color(tag, name, value)?),
        "caret-color" => attrs.caret_color = Some(parse_color(tag, name, value)?),
        "selection-text-color" => attrs.selection_text_color = Some(parse_color(tag, name, value)?),
        "caret-width" => attrs.caret_width = Some(parse_f32(tag, name, value)?),
        "caret-blink" => attrs.caret_blink_ms = Some(parse_duration_ms(tag, name, value.trim())?),
        "password-character" => {
            let mut chars = value.chars();
            let c = chars
                .next()
                .ok_or_else(|| bad(tag, name, value, "expected a single character".into()))?;
            if chars.next().is_some() {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "expected exactly one character".into(),
                ));
            }
            attrs.password_character = Some(c);
        }
        "scroll" => {
            let axis = match value {
                "y" => ScrollAxisSpec::Y,
                "x" => ScrollAxisSpec::X,
                "both" => ScrollAxisSpec::Both,
                other => {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        format!("unknown scroll axis '{other}'"),
                    ));
                }
            };
            attrs.scroll = Some(axis);
            // Horizontal scrollers default their flex axis to row so
            // children stack left->right; vertical / both keep column
            // (the tag default).
            if matches!(axis, ScrollAxisSpec::X) && attrs.flex == Some(FlexAxis::Column) {
                attrs.flex = Some(FlexAxis::Row);
            }
        }
        "sensitivity" => attrs.sensitivity = Some(parse_f32(tag, name, value)?),
        "inertia" => attrs.inertia = Some(parse_f32(tag, name, value)?),
        "tab-index" => attrs.tab_index = Some(parse_i32(tag, name, value)?),
        "id" => attrs.id = Some(value.to_string()),
        "href" => attrs.href = Some(value.to_string()),
        "class" => {
            attrs.classes = value.split_whitespace().map(|s| s.to_string()).collect();
        }
        "hover-bg" => attrs.hover_bg = Some(parse_color(tag, name, value)?),
        "press-bg" => attrs.press_bg = Some(parse_color(tag, name, value)?),
        "focus-outline" => {
            let parts: Vec<&str> = value.split_whitespace().collect();
            if parts.len() != 2 {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "expected '<width-px> <#color>'".to_string(),
                ));
            }
            let width = parse_f32(tag, name, parts[0])?;
            let color = parse_color(tag, name, parts[1])?;
            attrs.focus_outline = Some(OutlineSpec {
                width,
                color,
                offset: 0.0,
            });
        }
        "border" => {
            let sh = crate::values::parse_border_shorthand(tag, name, value)?;
            attrs.border_style = sh.style;
            attrs.border_width = sh.width.map(crate::layout_ir::Edges::all);
            attrs.border_color = sh.color;
        }
        "shrink" => attrs.shrink = Some(parse_f32(tag, name, value)?),
        "z-index" => attrs.z_index = Some(parse_i32(tag, name, value)?),
        "placeholder" => attrs.placeholder = Some(value.to_string()),
        "drop" => {
            attrs.drop_target = ctx.bool_value(tag, name, value);
        }
        // HTML5-DnD parity: `<x drop-target>` / `drop-target="true"`
        // marks an in-app drop zone.
        "drop-target" => {
            attrs.drop_target = ctx.bool_value(tag, name, value);
        }
        // MIME filter for a drop target - mirrors HTML5 `dropzone` /
        // `DataTransfer` type filtering. Empty = accept any.
        "accept" if !value.is_empty() => attrs.drop_accept = Some(value.trim().to_string()),
        "drag" if tag == "title-bar" => {
            attrs.title_bar_drag = ctx.bool_value(tag, name, value);
        }
        "layout-boundary" => {
            attrs.layout_boundary = ctx.bool_value(tag, name, value);
        }
        "src" => attrs.src = Some(value.to_string()),
        "font-size" => attrs.font_size = Some(parse_f32(tag, name, value)?),
        "font-family" => attrs.font_family = Some(value.trim().to_string()),
        "font-weight" => {
            attrs.font_weight = Some(crate::values::parse_font_weight(tag, name, value)?);
        }
        // See `lumen_ir::css`'s `line-height` arm for why the unitless
        // and px forms are kept as distinct `LineHeightSpec` variants.
        "line-height" => {
            let v = value.trim();
            attrs.line_height = Some(if let Some(rest) = v.strip_suffix("px") {
                LineHeightSpec::Px(parse_f32(tag, name, rest)?)
            } else {
                let n = parse_f32(tag, name, v)?;
                if n < 0.0 {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        "line-height must be \u{2265} 0".into(),
                    ));
                }
                LineHeightSpec::Multiplier(n)
            });
        }
        "knob-color" => attrs.knob_color = Some(parse_color(tag, name, value)?),
        "knob-inset" => attrs.knob_inset = Some(parse_f32(tag, name, value)?),
        "thumb-size" => attrs.thumb_size = Some(parse_f32(tag, name, value)?),
        "popup-gap" => attrs.popup_gap = Some(parse_f32(tag, name, value)?),
        "style" => {
            attrs.style_role = Some(value.to_string());
        }
        "gap" => attrs.gap = Some(parse_f32(tag, name, value)?),
        "grow" => attrs.grow = Some(parse_f32(tag, name, value)?),
        "align" => {
            attrs.align = Some(match value {
                "start" => FlexAlign::Start,
                "end" => FlexAlign::End,
                "center" => FlexAlign::Center,
                "stretch" => FlexAlign::Stretch,
                other => {
                    return Err(bad(tag, name, value, format!("unknown align '{other}'")));
                }
            });
        }
        "justify" => {
            attrs.justify = Some(match value {
                "start" => FlexJustify::Start,
                "end" => FlexJustify::End,
                "center" => FlexJustify::Center,
                "between" | "space-between" => FlexJustify::SpaceBetween,
                "around" | "space-around" => FlexJustify::SpaceAround,
                "evenly" | "space-evenly" => FlexJustify::SpaceEvenly,
                other => {
                    return Err(bad(tag, name, value, format!("unknown justify '{other}'")));
                }
            });
        }
        "text-align" => {
            attrs.text_align = Some(match value {
                "start" | "left" => TextAlignSpec::Start,
                "center" => TextAlignSpec::Center,
                "end" | "right" => TextAlignSpec::End,
                other => {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        format!("unknown text-align '{other}'"),
                    ));
                }
            });
        }
        // Per-kind binding form `bind-<kind>="<signal>"`. Hyphen-separated
        // because roxmltree treats `bind:` as an XML namespace prefix
        // and rejects undeclared namespaces - so the Svelte-flavour
        // `bind:text="..."` syntax isn't available to us. The
        // hyphenated form matches the rest of Lumen's compound attrs
        // (`text-color`, `hover-bg`, `focus-outline`).
        //
        // The value may carry a leading `$` (preferred for new code):
        //   - `$<name>` -> same as `<name>` (named-signal binding).
        //   - `$self.<field>` -> per-entity binding marker.
        //   - `$parent.<field>` -> parent-entity binding marker.
        // See the crate-level docs at the top of this file.
        // One-way disabled binding. Handled apart from the generic
        // `bind-<kind>` arm because it drives a *marker* (the `Disabled`
        // component), not a value component, and must be able to coexist
        // with a `bind-checked` / `bind-value` / `bind-text` on the same
        // element (those share the single `attrs.bind` slot).
        "bind-disabled" => {
            let trimmed = value.trim();
            if trimmed.starts_with("$self.") || trimmed.starts_with("$parent.") {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "bind-disabled supports named signals only ($self./$parent. forms are not \
                     available for the disabled state)"
                        .to_string(),
                ));
            }
            let signal = trimmed.strip_prefix('$').unwrap_or(trimmed).to_string();
            if signal.is_empty() {
                return Err(bad(tag, name, value, "expected a signal name".to_string()));
            }
            attrs.bind_disabled = Some(signal);
        }
        // Two-way scroll-offset binding (W6 T6). Handled apart from the
        // generic `bind-<kind>` arm because it drives [`lumen_core::input::
        // ScrollOffset`] on a scroll container, not one of the value
        // components sharing the single `attrs.bind` slot.
        "bind-scroll" => {
            let trimmed = value.trim();
            if trimmed.starts_with("$self.") || trimmed.starts_with("$parent.") {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "bind-scroll supports named signals only ($self./$parent. forms are not \
                     available for the scroll offset)"
                        .to_string(),
                ));
            }
            let signal = trimmed.strip_prefix('$').unwrap_or(trimmed).to_string();
            if signal.is_empty() {
                return Err(bad(tag, name, value, "expected a signal name".to_string()));
            }
            attrs.bind_scroll = Some(signal);
        }
        n if n.starts_with("bind-") => {
            let kind_str = &n["bind-".len()..];
            let kind = match kind_str {
                "text" => BindKind::Text,
                "checked" => BindKind::Checked,
                "value" => BindKind::Value,
                other => {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        format!(
                            "unknown bind kind '{other}' (supported: text, checked, value, \
                             disabled, scroll)"
                        ),
                    ));
                }
            };
            let trimmed = value.trim();
            if let Some(rest) = trimmed.strip_prefix("$self.") {
                let field = rest.trim().to_string();
                if field.is_empty() {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        "expected a field name after '$self.'".to_string(),
                    ));
                }
                match kind {
                    BindKind::Text => attrs.bind_self_text = Some(field),
                    BindKind::Value => attrs.bind_self_value = Some(field),
                    BindKind::Checked => attrs.bind_self_checked = Some(field),
                }
            } else if let Some(rest) = trimmed.strip_prefix("$parent.") {
                let field = rest.trim().to_string();
                if field.is_empty() {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        "expected a field name after '$parent.'".to_string(),
                    ));
                }
                match kind {
                    BindKind::Text => attrs.bind_parent_text = Some(field),
                    BindKind::Value => attrs.bind_parent_value = Some(field),
                    BindKind::Checked => attrs.bind_parent_checked = Some(field),
                }
            } else {
                // Strip the optional leading `$` on plain named-signal
                // bindings - `$count` is sugar for `count`.
                let signal = trimmed.strip_prefix('$').unwrap_or(trimmed).to_string();
                attrs.bind = Some(BindSpec { kind, name: signal });
            }
        }
        "each" => {
            // Wave-A: strip the optional leading `$` so `each="$users"`
            // is identical to `each="users"`. The `ArraySignals` store
            // keys by bare name; the `$` prefix is a reviewer-facing
            // marker that the author is referencing a signal.
            let v = value.trim();
            let stripped = v.strip_prefix('$').unwrap_or(v).to_string();
            attrs.each = Some(stripped);
        }
        "key" => attrs.key = Some(value.trim().to_string()),
        "virtualized" => attrs.virtualized = ctx.bool_value(tag, name, value),
        "row-height" => attrs.row_height = Some(parse_f32(tag, name, value)?),
        "wrap" => {
            if value == "ellipsis" {
                // Single-line elision: lowered by the spawn layer onto
                // the runtime TextStyle (glyph wrap + max_lines = 1 +
                // trailing `...`). Kept out of TextWrapSpec so the plain
                // wrap policies stay a 1:1 mirror of the runtime enum.
                attrs.text_overflow = Some(crate::layout_ir::TextOverflowSpec::Ellipsis);
            } else {
                attrs.text_wrap = Some(match value {
                    "none" | "nowrap" => TextWrapSpec::None,
                    "word" | "normal" => TextWrapSpec::Word,
                    "glyph" | "char" => TextWrapSpec::Glyph,
                    other => {
                        return Err(bad(
                            tag,
                            name,
                            value,
                            format!(
                                "unknown wrap '{other}' (supported: none, word, glyph, ellipsis)"
                            ),
                        ));
                    }
                });
            }
        }
        "max-lines" => {
            let n: i64 = value
                .trim()
                .parse()
                .map_err(|e: std::num::ParseIntError| bad(tag, name, value, e.to_string()))?;
            if n < 0 {
                return Err(bad(tag, name, value, "max-lines must be >= 0".to_string()));
            }
            attrs.max_lines = Some(n as u32);
        }
        "position" => {
            attrs.position = Some(match value {
                "relative" => PositionSpec::Relative,
                "absolute" => PositionSpec::Absolute,
                other => {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        format!("unknown position '{other}' (supported: relative, absolute)"),
                    ));
                }
            });
        }
        "inset" => attrs.inset = Some(parse_edges(tag, name, value)?),
        "min-width" => attrs.min_width = Some(parse_length(tag, name, value)?),
        "min-height" => attrs.min_height = Some(parse_length(tag, name, value)?),
        "max-width" => attrs.max_width = Some(parse_length(tag, name, value)?),
        "max-height" => attrs.max_height = Some(parse_length(tag, name, value)?),
        "aspect-ratio" => attrs.aspect_ratio = Some(parse_f32(tag, name, value)?),
        "opacity" => {
            let v: f32 = value
                .trim()
                .parse()
                .map_err(|e: std::num::ParseFloatError| bad(tag, name, value, e.to_string()))?;
            attrs.opacity = Some(v.clamp(0.0, 1.0));
        }
        "shadow" => {
            // Markup form accepts a single shadow spec (no comma list);
            // CSS-side parsing handles stacked `box-shadow` entries.
            // Leading `inset` keyword flips drop -> inner.
            let mut toks: Vec<&str> = value.split_whitespace().collect();
            let inner = if toks.first().is_some_and(|t| *t == "inset") {
                toks.remove(0);
                true
            } else {
                false
            };
            if toks.len() != 4 && toks.len() != 5 {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "expected '[inset] <offset-x> <offset-y> <blur> [<spread>] <#color>'".into(),
                ));
            }
            let spread = if toks.len() == 5 {
                parse_f32(tag, name, toks[3])?
            } else {
                0.0
            };
            attrs.shadows = vec![crate::layout_ir::ShadowSpec {
                offset_x: parse_f32(tag, name, toks[0])?,
                offset_y: parse_f32(tag, name, toks[1])?,
                blur: parse_f32(tag, name, toks[2])?,
                spread,
                color: parse_color(tag, name, toks[if toks.len() == 5 { 4 } else { 3 }])?,
                inner,
            }];
        }
        "fit" => {
            attrs.image_fit = Some(match value {
                "fill" => ImageFitSpec::Fill,
                "cover" => ImageFitSpec::Cover,
                "contain" => ImageFitSpec::Contain,
                "none" => ImageFitSpec::None,
                "scale-down" => ImageFitSpec::ScaleDown,
                other => {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        format!(
                            "unknown fit '{other}' (supported: fill, cover, contain, none, scale-down)"
                        ),
                    ));
                }
            });
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            let o = match value {
                "visible" => OverflowSpec::Visible,
                "hidden" => OverflowSpec::Hidden,
                "scroll" => OverflowSpec::Scroll,
                other => {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        format!("unknown overflow '{other}' (supported: visible, hidden, scroll)"),
                    ));
                }
            };
            match name {
                "overflow" => attrs.overflow = Some(o),
                "overflow-x" => attrs.overflow_x = Some(o),
                "overflow-y" => attrs.overflow_y = Some(o),
                _ => unreachable!(),
            }
        }
        // Lumen-native overlay-scrollbar geometry/timing - markup mirror
        // of the `lumen_ir::css` cascade arms of the same names.
        "scrollbar-thickness" => attrs.scrollbar_thickness = Some(parse_f32(tag, name, value)?),
        "scrollbar-thickness-thin" => {
            attrs.scrollbar_thickness_thin = Some(parse_f32(tag, name, value)?);
        }
        "scrollbar-margin" => attrs.scrollbar_margin = Some(parse_f32(tag, name, value)?),
        "scrollbar-min-thumb" => attrs.scrollbar_min_thumb = Some(parse_f32(tag, name, value)?),
        "scrollbar-track-hover" => {
            attrs.scrollbar_track_hover = Some(parse_color(tag, name, value)?);
        }
        "scrollbar-hover-boost" => {
            attrs.scrollbar_hover_boost = Some(parse_f32(tag, name, value)?);
        }
        "scrollbar-fade-delay" => {
            attrs.scrollbar_fade_delay_ms = Some(parse_duration_ms(tag, name, value.trim())?);
        }
        "scrollbar-fade-duration" => {
            attrs.scrollbar_fade_duration_ms = Some(parse_duration_ms(tag, name, value.trim())?);
        }
        // `<if signal="name">` - `signal` is a reserved attribute name
        // only on the `<if>` tag. On any other tag it's silently ignored
        // (unknown attrs are tolerated for forward-compat).
        "signal" if tag == "if" => {
            attrs.if_signal = Some(value.trim().to_string());
        }
        // `<dialog open="signal">` is sugar for `<if signal="signal"
        // mode="hide">`. Preserving children state across show/hide is
        // the right default for modal forms.
        "open" if tag == "dialog" => {
            attrs.if_signal = Some(value.trim().to_string());
            attrs.if_mode = crate::layout_ir::IfModeSpec::Hide;
        }
        "mode" if tag == "if" => {
            attrs.if_mode = match value.trim() {
                "render" => crate::layout_ir::IfModeSpec::Render,
                "hide" => crate::layout_ir::IfModeSpec::Hide,
                other => {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        format!("unknown <if> mode '{other}' (expected 'render' or 'hide')"),
                    ));
                }
            };
        }
        "eq" if tag == "if" => {
            attrs.if_eq = Some(value.to_string());
        }
        "checked" => {
            attrs.checked = Some(ctx.bool_value(tag, name, value));
        }
        // `<radio value="apple">` carries a STRING member value; every
        // other tag's `value` (slider, progress) is numeric.
        "value" if tag == "radio" => {
            attrs.radio_value = Some(value.to_string());
        }
        "value" => attrs.value = Some(parse_f32(tag, name, value)?),
        "group" if tag == "radio" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "radio group requires a signal name".to_string(),
                ));
            }
            attrs.radio_group = Some(trimmed.to_string());
        }
        // `<checkbox label>` / `<radio label>` - the visible caption.
        // Routed through the shared `text` slot; the desugar moves it
        // onto the synthesized `.checkbox-label` / `.radio-label`
        // child.
        "label" if matches!(tag, "checkbox" | "radio") => {
            attrs.text = Some(value.to_string());
        }
        "indeterminate" if tag == "checkbox" => {
            attrs.indeterminate = ctx.bool_value(tag, name, value);
        }
        "duration" if tag == "progress" => {
            let n: u32 = value
                .trim()
                .parse()
                .map_err(|e: std::num::ParseIntError| bad(tag, name, value, e.to_string()))?;
            attrs.progress_duration = Some(n);
        }
        // Markup mirror of CSS `progress-chunk` - short attribute name
        // matches the existing `duration` convention above.
        "chunk" if tag == "progress" => {
            let v = parse_f32(tag, name, value)?;
            if !(0.0..=1.0).contains(&v) {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "progress-chunk must be between 0.0 and 1.0".to_string(),
                ));
            }
            attrs.progress_chunk = Some(v);
        }
        "min" => attrs.min = Some(parse_f32(tag, name, value)?),
        "max" => attrs.max = Some(parse_f32(tag, name, value)?),
        "step" => attrs.step = Some(parse_f32(tag, name, value)?),
        "required" => {
            attrs.required = ctx.bool_value(tag, name, value);
        }
        // Dialog contract (W5): `autofocus` marks the initial-focus
        // target when the containing <dialog> opens; `default` marks
        // the dialog's default button (Enter-anywhere activation +
        // accepted-path close). The `default` class is appended at
        // parse time so the compile-time cascade can style
        // `button.default`.
        "autofocus" => {
            attrs.autofocus = ctx.bool_value(tag, name, value);
        }
        "default" if tag == "button" => {
            attrs.default_button = ctx.bool_value(tag, name, value);
        }
        "disabled" => {
            attrs.disabled = ctx.bool_value(tag, name, value);
        }
        // CSS-authored replacement for the runtime's generic `:disabled`
        // dimming fallback - see `lumen_ir::css`'s arm for how this
        // differs from an explicit `:disabled { opacity }` override.
        "disabled-opacity" => {
            let v = parse_f32(tag, name, value)?;
            attrs.disabled_opacity_default = Some(v.clamp(0.0, 1.0));
        }
        "pattern" if !value.is_empty() => attrs.pattern = Some(value.to_string()),
        "multiline" => attrs.multiline = Some(ctx.bool_value(tag, name, value)),
        // In-app DnD source payload - mirrors HTML5 `dataTransfer.setData`.
        // Value may be a `{row.field}` placeholder (substituted per-row in
        // a `<for>`); empty string means "derive the payload from `id`".
        "drag-payload" => attrs.drag_payload = Some(value.to_string()),
        "draggable" => {
            attrs.draggable = ctx.bool_value(tag, name, value);
        }
        // W5.4: CSS Logical Properties direction attribute. Accepts
        // `ltr` / `rtl` / `auto`. Validated at parse time so typos
        // surface early; the resolved value rides through `Attributes`
        // and is stamped on the spawned entity as a
        // `lumen_core::components::LayoutDirection` component.
        "dir" => {
            let lowered = value.trim().to_ascii_lowercase();
            match lowered.as_str() {
                "ltr" | "rtl" | "auto" => {
                    attrs.dir = Some(lumen_core::components::LayoutDirection::from(
                        lowered.as_str(),
                    ));
                }
                other => {
                    return Err(bad(
                        tag,
                        name,
                        value,
                        format!("unknown dir '{other}' (supported: ltr, rtl, auto)"),
                    ));
                }
            }
        }
        // W5.4: BCP-47 language tag. We accept any non-empty value;
        // structural validation (via `unic_langid`) lives in
        // `lumen-i18n` and runs at runtime since the parser shouldn't
        // pull the full ICU stack. Empty value is an error. The
        // trimmed string lands in `Attributes::lang` and is converted
        // to `lumen_core::components::Lang` at spawn time.
        "lang" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "lang attribute requires a BCP-47 tag (e.g. \"en-US\")".to_string(),
                ));
            }
            attrs.lang = Some(trimmed.to_string());
        }
        // Translation marker. The value is a catalogue key, not display
        // text: `lumenc i18n extract` collects it into
        // `locale/<lang>.ftl`, and the runtime resolves it through the
        // loaded catalogue at spawn time. Empty is an error - an empty
        // key can never resolve, and silently dropping it would leave
        // the author wondering why nothing translated.
        "translatable" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "translatable attribute requires a catalogue key (e.g. \"app-title\")"
                        .to_string(),
                ));
            }
            attrs.translatable = Some(trimmed.to_string());
        }
        // Window / document metadata read straight off `<root>` in
        // `parse_html` before this walk: `skin` selects the user-agent
        // stylesheet, `frameless` drops the OS title bar. Neither is a
        // layout attribute, so neither has a field in `Attributes`;
        // they are listed here so the rule below does not call them
        // unknown.
        "skin" | "frameless" => {}
        // An attribute the vocabulary has no meaning for is dropped, and
        // dropping it silently is how `tect="hi"` or `on_click="inc"`
        // survives review: the markup parses, the app runs, and nothing
        // the author wrote takes effect. Warn and carry on; a future
        // strict mode can promote this to a ParseError.
        //
        // Custom widget tags are exempt: `#[derive(Widget)]` reads its
        // own `#[widget(prop)]` fields out of the raw attribute bag, so
        // the built-in table cannot say what is and is not meaningful
        // there.
        other => {
            if KNOWN_TAGS.contains(&tag) {
                let (line, col) = line_col_of(ctx.src, ctx.value_offset);
                ctx.lint_findings.push(LintFinding {
                    kind: LintKind::UnknownAttribute,
                    severity: LintSeverity::Warn,
                    message: format!(
                        "`<{tag}>` has no `{other}` attribute; it is ignored. Check the spelling \
                         against the tag reference."
                    ),
                    line,
                    col,
                    // Guessing which attribute was meant needs an edit-distance
                    // table the parser does not carry.
                    suggest: None,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod include_tests {
    use super::*;
    use crate::resolve::{FileLoader, normalize_path};
    use std::collections::HashMap;
    use std::path::Path;

    struct MockLoader(HashMap<String, String>);
    impl FileLoader for MockLoader {
        fn load(&self, path: &Path) -> std::io::Result<String> {
            let key = normalize_path(path).display().to_string();
            self.0
                .get(&key)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
        }
    }
    fn mock(files: &[(&str, &str)]) -> MockLoader {
        MockLoader(
            files
                .iter()
                .map(|(k, v)| {
                    (
                        normalize_path(Path::new(k)).display().to_string(),
                        v.to_string(),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn included_files_field_populated() {
        let loader = mock(&[("app/parts/header.lmn", "<tile class=\"hdr\"/>")]);
        let ir = parse_html_with_loader(
            "<root><include src=\"parts/header.lmn\"/></root>",
            Path::new("app/main.lmn"),
            &loader,
        )
        .unwrap();
        assert_eq!(ir.included_files.len(), 1);
        assert!(ir.included_files[0].ends_with("parts/header.lmn"));
        // Spliced element is present in the tree.
        assert_eq!(ir.root.children.len(), 1);
        assert_eq!(ir.root.children[0].tag, "tile");
    }

    #[test]
    fn template_from_include_expands() {
        let loader = mock(&[(
            "app/lib.lmn",
            "<template name=\"Card\"><tile class=\"card\"/></template>",
        )]);
        let ir = parse_html_with_loader(
            "<root><include src=\"lib.lmn\"/><Card/></root>",
            Path::new("app/main.lmn"),
            &loader,
        )
        .unwrap();
        // The <Card/> use-site expanded to the template body (a tile).
        assert_eq!(ir.root.children.len(), 1);
        assert_eq!(ir.root.children[0].tag, "tile");
    }

    #[test]
    fn plain_parse_html_ignores_include() {
        // String-only entry: include is dropped, no error, no files.
        let ir = parse_html("<root><include src=\"x.lmn\"/><label/></root>").unwrap();
        assert!(ir.included_files.is_empty());
        assert_eq!(ir.root.children.len(), 1);
        assert_eq!(ir.root.children[0].tag, "label");
    }

    #[test]
    fn find_tag_gt_skips_gt_in_quoted_attr() {
        // A `>` inside a quoted attribute value must not close the tag early.
        let s = r#"<Card label="a > b"/>rest"#;
        let gt = find_tag_gt(s).expect("closing gt");
        assert_eq!(&s[..=gt], r#"<Card label="a > b"/>"#);
    }

    #[test]
    fn find_tag_gt_swallows_comment_body() {
        let s = "<!-- a > \"b\" --><next>";
        let gt = find_tag_gt(s).expect("comment end");
        assert_eq!(&s[..=gt], "<!-- a > \"b\" -->");
    }

    #[test]
    fn template_use_with_gt_in_attr_expands_intact() {
        // The use-site `<Card c="a>b"/>` carries a `>` in a quoted attr; the
        // naive `find('>')` truncated the tag and lost the attribute.
        let ir = parse_html(concat!(
            "<root>",
            "<template name=\"Card\"><tile class=\"{c}\"/></template>",
            "<Card c=\"a>b\"/>",
            "</root>",
        ))
        .expect("parse");
        assert_eq!(ir.root.children.len(), 1);
        assert_eq!(ir.root.children[0].attrs.classes, vec!["a>b".to_string()]);
    }
}
