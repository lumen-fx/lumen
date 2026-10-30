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
    Attributes, BindKind, BindSpec, DeferredAttr, Element, FlexAlign, FlexJustify, FragmentUse,
    InterpolationSlot, LayoutIR, LineHeightSpec, LintFinding, LintKind, LintSeverity, OutlineSpec,
    OverflowSpec, ParseError, PositionSpec, ScriptRefs, ScrollAxisSpec, TextAlignSpec,
    TextWrapSpec, WidgetRole,
};
use crate::values::{
    bad, parse_bg, parse_color, parse_edges, parse_f32, parse_i32, parse_inset_edges, parse_length,
};
// `parse_duration_ms` lives on the CSS cascade side (`lumen_ir::css`)
// because it's shared with `transition-duration`; reused here rather
// than re-implementing the `Nms` / `Ns` unit handling for the inline
// markup mirror of `caret-blink` / `scrollbar-fade-*`.
use lumen_ir::css::{canonical_style_property, parse_duration_ms};
use lumen_ir::fragment::{
    Fragment, FragmentKind, FragmentOrigin, FragmentParam, FragmentTable, SLOT_TAG,
};
use lumen_ir::source_map::{SourceMap, line_at};
use std::collections::BTreeSet;
use std::path::Path;

/// Recognized layout tag names. Unknown tags produce
/// [`ParseError::UnknownTag`]. `script` is special-cased (collected into
/// `LayoutIR::script_source`, not a layout node).
pub const KNOWN_TAGS: &[&str] = &[
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
    // and inset=0 0 0 0 so it covers its parent's padding box
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
    // Menu widget. `<menu id="m">` holding `<menuitem id label/>`, `<separator/>` and `<for>` children stays a `menu` element, floating and shown while `__menu_open:<id>` is set. `<menu>` inside a `<menubar>` is consumed earlier by `extract_menubar`.
    "menu",
    "menuitem",
    "separator",
    // Date/time pickers. Author writes a date or time string into a validated `<input>`; the built-in pattern enforces the shape.
    "date-picker",
    "time-picker",
    // Resizable panes. `<split>` lays its children out along its
    // `flex-direction` with a draggable `.split-gutter` between each pair.
    "split",
];

/// A parsed markup file: the tree it describes, and the fragments that were
/// visible while building it.
#[derive(Debug)]
pub struct ParsedMarkup {
    /// The layout tree, with every fragment use already inlined.
    pub ir: LayoutIR,
    /// The fragments this file declared, folded over the ones it was handed.
    pub fragments: FragmentTable,
}

/// The inline `<script>` blocks a parse collects, joined in source order, and
/// where each one was written.
pub(crate) struct InlineScripts {
    text: String,
    map: SourceMap,
    /// The markup file, as an error names it; empty for a pathless parse.
    file: String,
}

impl InlineScripts {
    fn new(file: &Path) -> Self {
        Self {
            text: String::new(),
            map: SourceMap::default(),
            file: file.display().to_string(),
        }
    }

    /// Add one block whose text `body` starts at byte `at` of `markup`.
    fn push(&mut self, body: &str, markup: &str, at: usize) {
        self.map
            .append(&mut self.text, body, &self.file, line_at(markup, at));
    }
}

/// Parse a Lumen-markup string into a [`LayoutIR`].
///
/// `<script>` tags are collected into `LayoutIR::script_source` and
/// stripped from the element tree (they are not layout nodes).
///
/// `<template name="X">...body...</template>` blocks declare fragments, which
/// are instantiated as `<X k="v"/>` or `<use template="X" k="v"/>` and
/// inlined here. Use [`parse_markup`] to reach the fragments themselves, or
/// to instantiate ones another file declared.
pub fn parse_html(src: &str) -> Result<LayoutIR, ParseError> {
    parse_markup(src, Path::new(""), None, &FragmentTable::new()).map(|p| p.ir)
}

/// Same as [`parse_html`] but resolves `<include src="..."/>` directives
/// through `loader`, treating relative paths as relative to `self_path`'s
/// directory. `self_path` is the file `src` was read from; it seeds cycle
/// detection and error positions. The resolved include file paths land in
/// [`LayoutIR::included_files`] so the runtime can watch them.
pub fn parse_html_with_loader(
    src: &str,
    self_path: &Path,
    loader: &dyn crate::parse::resolve::FileLoader,
) -> Result<LayoutIR, ParseError> {
    parse_markup(src, self_path, Some(loader), &FragmentTable::new()).map(|p| p.ir)
}

/// A `roxmltree` error as a [`ParseError`] placed where it happened in
/// `text`, the text that was parsed, with the position taken out of its
/// message.
fn xml_error(e: &roxmltree::Error, text: &str) -> ParseError {
    let pos = e.pos();
    let message = e.to_string();
    let message = message
        .strip_suffix(&format!(" at {}:{}", pos.row, pos.col))
        .unwrap_or(&message)
        .to_string();
    ParseError::Xml(message).at(lumen_ir::source_map::offset_of(text, pos.row, pos.col))
}

/// Parse markup that may instantiate fragments declared elsewhere.
///
/// `external` holds those fragments; an app collects them from every one of
/// its `.lmn` files with [`collect_fragments`] so a shared layout is reachable
/// from any file. A key this file declares itself wins over the one it was
/// handed, which is what lets a file be both a contributor to the app-wide
/// table and a parse of its own.
pub fn parse_markup(
    src: &str,
    self_path: &Path,
    loader: Option<&dyn crate::parse::resolve::FileLoader>,
    external: &FragmentTable,
) -> Result<ParsedMarkup, ParseError> {
    let mut included_files = Vec::new();
    let spliced =
        crate::parse::resolve::resolve_includes(src, self_path, loader, &mut included_files)?;
    let doc = roxmltree::Document::parse(&spliced).map_err(|e| xml_error(&e, &spliced))?;
    let mut script_source = InlineScripts::new(self_path);
    let mut external_scripts = Vec::new();
    let mut lint_findings: Vec<LintFinding> = Vec::new();
    let fragments = collect_declarations(
        &doc,
        &spliced,
        self_path,
        external,
        &mut script_source,
        &mut external_scripts,
        &mut lint_findings,
    )?;
    // `<root skin="...">` is the opt-in surface for the embedded
    // user-agent stylesheet. Pulled before `build_element` because
    // `skin` is not a layout attribute and shouldn't survive in
    // `Attributes` - it's metadata for the runtime.
    let skin = doc.root_element().attribute("skin").map(|s| s.to_string());
    let frameless = bool_attribute_of(
        doc.root_element(),
        "frameless",
        &spliced,
        &mut lint_findings,
    );
    // Extract `<menubar>` blocks from the layout tree before
    // `build_element` walks the root - they live in `LayoutIR.menubar`
    // and the window backend builds an OS-native menu from them.
    let menubar = extract_menubar(doc.root_element())?;
    let known = fragments.names();
    let mut root = build_element(
        doc.root_element(),
        &mut script_source,
        &mut external_scripts,
        &spliced,
        &mut lint_findings,
        &FragCtx {
            known: &known,
            in_body: false,
        },
        false,
        0,
    )
    .map_err(|e| e.at(doc.root_element().range().start))?;
    crate::parse::fragments::inline(&mut root, &fragments, &mut lint_findings)?;
    // Lines were counted in the spliced markup; an error names the file the
    // author wrote, which has no include bodies in it.
    let mut script_map = script_source.map;
    script_map.place_in(&script_source.text, src);
    Ok(ParsedMarkup {
        ir: LayoutIR {
            root,
            script_source: script_source.text,
            script_map,
            external_scripts,
            skin,
            frameless,
            menubar,
            combined_stylesheet: None,
            lint_findings,
            included_files,
        },
        fragments,
    })
}

/// Read the `<script>` elements `src` names, without building its layout
/// tree.
///
/// Markup names a candela component by writing the function as a tag, and
/// that function lives in a script the markup itself points at, so the
/// scripts are read first and their blocks are in the fragment table before
/// the tree is built.
///
/// # Errors
///
/// [`ParseError`] when an `<include>` cannot be resolved or the markup is not
/// well-formed XML.
pub fn collect_script_refs(
    src: &str,
    self_path: &Path,
    loader: Option<&dyn crate::parse::resolve::FileLoader>,
) -> Result<ScriptRefs, ParseError> {
    let mut included_files = Vec::new();
    let spliced =
        crate::parse::resolve::resolve_includes(src, self_path, loader, &mut included_files)?;
    let doc = roxmltree::Document::parse(&spliced).map_err(|e| xml_error(&e, &spliced))?;
    let mut refs = ScriptRefs::default();
    for node in doc.descendants() {
        if !node.is_element() || node.tag_name().name() != "script" {
            continue;
        }
        if let Some(path) = node.attribute("src") {
            refs.external.push(path.to_string());
        } else if let Some(body) = node.text() {
            if !refs.inline.is_empty() {
                refs.inline.push('\n');
            }
            refs.inline.push_str(body);
        }
    }
    Ok(refs)
}

/// Read the fragments `src` declares, without building its layout tree.
///
/// An app-wide table is the union of this over every `.lmn` file the app
/// ships, which is what makes a fragment declared in one file usable from
/// another.
pub fn collect_fragments(
    src: &str,
    self_path: &Path,
    loader: Option<&dyn crate::parse::resolve::FileLoader>,
) -> Result<FragmentTable, ParseError> {
    let mut included_files = Vec::new();
    let spliced =
        crate::parse::resolve::resolve_includes(src, self_path, loader, &mut included_files)?;
    let doc = roxmltree::Document::parse(&spliced).map_err(|e| xml_error(&e, &spliced))?;
    let mut script_source = InlineScripts::new(self_path);
    let mut external_scripts = Vec::new();
    let mut lint_findings = Vec::new();
    collect_declarations(
        &doc,
        &spliced,
        self_path,
        &FragmentTable::new(),
        &mut script_source,
        &mut external_scripts,
        &mut lint_findings,
    )
}

/// Lift every `<template name="X">` in `doc` into a fragment and fold the
/// result over `external`.
///
/// Names are gathered before any body is built: a body may instantiate a
/// fragment declared further down the file, and the element builder decides
/// what is a use site from the names it can see.
#[allow(clippy::too_many_arguments)]
fn collect_declarations(
    doc: &roxmltree::Document,
    src: &str,
    self_path: &Path,
    external: &FragmentTable,
    script_buf: &mut InlineScripts,
    external_scripts: &mut Vec<String>,
    lint_findings: &mut Vec<LintFinding>,
) -> Result<FragmentTable, ParseError> {
    let declarations: Vec<roxmltree::Node<'_, '_>> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "template")
        .collect();
    if declarations.is_empty() {
        return Ok(external.clone());
    }
    let mut known = external.names();
    for node in &declarations {
        let name = node.attribute("name").ok_or_else(|| {
            ParseError::Xml("<template> requires a name=\"...\" attribute".to_string())
                .at(node.range().start)
        })?;
        known.insert(name.to_string());
    }

    let file = self_path.display().to_string();
    let mut own = FragmentTable::new();
    for node in &declarations {
        let key = node.attribute("name").unwrap_or_default().to_string();
        let mut params: Vec<FragmentParam> = node
            .attributes()
            .filter(|a| a.name() != "name")
            .map(|a| FragmentParam {
                name: a.name().to_string(),
                default: Some(a.value().to_string()),
            })
            .collect();
        let ctx = FragCtx {
            known: &known,
            in_body: true,
        };
        let mut body = Vec::new();
        let mut attrs = Attributes::default();
        let mut slots = Vec::new();
        build_children(
            *node,
            &mut attrs,
            &mut body,
            &mut slots,
            script_buf,
            external_scripts,
            src,
            lint_findings,
            &ctx,
            false,
            1,
        )?;
        // A marker the body reads is a parameter whether or not the
        // declaration named it: the use site is free to bind any of them, and
        // one it leaves alone falls through to the global signal scope.
        for name in body_markers(&body) {
            if !params.iter().any(|p| p.name == name) {
                params.push(FragmentParam {
                    name,
                    default: None,
                });
            }
        }
        let names: BTreeSet<&str> = params.iter().map(|p| p.name.as_str()).collect();
        for el in &mut body {
            bind_markers(el, &names);
        }
        let (line, col) = line_col_of(src, node.range().start);
        own.insert(Fragment {
            key,
            params,
            body,
            origins: vec![FragmentOrigin {
                file: file.clone(),
                line: line as u32,
                col: col as u32,
            }],
            kind: FragmentKind::Template,
            // A `<template>` is reached by the name it declares, which is its
            // own key.
            components: Vec::new(),
        })
        .map_err(|e| ParseError::Xml(e.to_string()))?;
    }

    let mut table = own;
    for (key, fragment) in external.iter() {
        if table.get(key).is_none() {
            table
                .insert(fragment.clone())
                .map_err(|e| ParseError::Xml(e.to_string()))?;
        }
    }
    Ok(table)
}

/// Every global marker name the body reads, in first-appearance order.
fn body_markers(body: &[Element]) -> Vec<String> {
    fn walk(el: &Element, out: &mut Vec<String>) {
        for slot in &el.interpolations {
            if let InterpolationSlot::Global(name) = slot
                && !out.iter().any(|n| n == name)
            {
                out.push(name.clone());
            }
        }
        for child in &el.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    for el in body {
        walk(el, &mut out);
    }
    out
}

/// Re-scope every marker in `el` that names a parameter, so instantiation
/// resolves it from the arguments instead of the global signal map.
fn bind_markers(el: &mut Element, params: &BTreeSet<&str>) {
    for slot in &mut el.interpolations {
        if let InterpolationSlot::Global(name) = slot
            && params.contains(name.as_str())
        {
            *slot = InterpolationSlot::Arg(std::mem::take(name));
        }
    }
    for child in &mut el.children {
        bind_markers(child, params);
    }
}

/// What the element builder needs to know about fragments as it walks.
pub(crate) struct FragCtx<'a> {
    /// Every key a use site may name: what the app handed this parse, plus
    /// what the file declares itself.
    pub(crate) known: &'a BTreeSet<String>,
    /// Set while building a fragment body.
    pub(crate) in_body: bool,
}

impl FragCtx<'_> {
    /// The fragment `tag` instantiates, if it instantiates one.
    ///
    /// Inside a body an unrecognized tag is a use site rather than an error:
    /// the table a body instantiates against is assembled from the whole app,
    /// so a name this file has never seen may still be declared elsewhere.
    /// The lookup that decides is [`crate::parse::fragments::inline`], which runs
    /// with the full table in hand.
    fn use_key(&self, node: roxmltree::Node, tag: &str) -> Option<String> {
        if tag == "use" {
            return Some(node.attribute("template").unwrap_or_default().to_string());
        }
        if self.known.contains(tag) {
            return Some(tag.to_string());
        }
        if self.in_body
            && !KNOWN_TAGS.contains(&tag)
            && !matches!(tag, SLOT_TAG | "script" | "menubar" | "template")
            && !lumen_widget::is_widget_tag_registered(tag)
        {
            return Some(tag.to_string());
        }
        None
    }
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

/// The signal name a signal-valued attribute names.
///
/// Every attribute that takes a signal accepts `name` or `$name`; the `$`
/// marks a signal reference for the reader and is not part of the name the
/// signal stores key by.
fn signal_attr_name(value: &str) -> String {
    let trimmed = value.trim();
    trimmed.strip_prefix('$').unwrap_or(trimmed).to_string()
}

/// Reject a `bind-*` value naming a fragment argument.
///
/// A fragment argument substitutes once, when the instance is built, so
/// nothing drives a binding from one yet. The syntax is claimed here rather
/// than left to fall through to the named-signal form, where `$arg.title`
/// would silently read a global signal called `arg.title`.
fn reject_arg_binding(tag: &str, name: &str, value: &str) -> Result<(), ParseError> {
    if value.trim().starts_with("$arg.") {
        return Err(bad(
            tag,
            name,
            value,
            "binding to a fragment argument is not supported yet; arguments are substituted \
             once at instantiation, so bind to a signal instead"
                .to_string(),
        ));
    }
    Ok(())
}

/// Append `slot` to `slots` only if no equal slot is already present.
/// Keeps the per-element slot list deduplicated so the spawner doesn't
/// re-resolve the same placeholder more than once per row.
pub(crate) fn push_unique(slots: &mut Vec<InterpolationSlot>, slot: InterpolationSlot) {
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

/// Build the fragment one `lmn!` block declares.
///
/// `markup` is the block body rewritten for the markup parser: an argument
/// site is a `{name}` marker and every component element is a `<slot>`.
/// `params` names the arguments, so a marker the block did not write with `$`
/// stays a signal reference the global scope resolves.
///
/// This is the `<template>` path's other authoring form, and it ends in the
/// same [`Fragment`]: the body compiles through the same element builder, and
/// [`FragmentKind::Markup`] is all that distinguishes the result.
///
/// # Errors
///
/// [`ParseError`] when the body is not well-formed markup, or when it has
/// anything other than one root element to instantiate.
pub fn fragment_from_markup(
    markup: &str,
    key: &str,
    params: &[String],
    origin: FragmentOrigin,
) -> Result<Fragment, ParseError> {
    let wrapped = format!("<template>{markup}</template>");
    let doc = roxmltree::Document::parse(&wrapped).map_err(|e| ParseError::Xml(e.to_string()))?;
    let known = BTreeSet::new();
    let ctx = FragCtx {
        known: &known,
        in_body: true,
    };
    let mut body = Vec::new();
    let mut attrs = Attributes::default();
    let mut slots = Vec::new();
    let mut script_buf = InlineScripts::new(Path::new(""));
    let mut external_scripts = Vec::new();
    let mut lint_findings = Vec::new();
    build_children(
        doc.root_element(),
        &mut attrs,
        &mut body,
        &mut slots,
        &mut script_buf,
        &mut external_scripts,
        &wrapped,
        &mut lint_findings,
        &ctx,
        false,
        1,
    )?;
    let names: BTreeSet<&str> = params.iter().map(String::as_str).collect();
    for el in &mut body {
        bind_markers(el, &names);
    }
    match body.as_slice() {
        [root] if root.tag != SLOT_TAG => {}
        [_] => {
            return Err(ParseError::Xml(
                "an lmn! block whose root is a slot leaves nothing to return".to_string(),
            ));
        }
        other => {
            return Err(ParseError::Xml(format!(
                "an lmn! block has one root element; this one has {}",
                other.len()
            )));
        }
    }
    Ok(Fragment {
        key: key.to_string(),
        params: params
            .iter()
            .map(|name| FragmentParam {
                name: name.clone(),
                default: None,
            })
            .collect(),
        body,
        origins: vec![origin],
        kind: FragmentKind::Markup,
        // Filled by the extractor, which is what knows the function the block
        // was written in.
        components: Vec::new(),
    })
}

/// Map a byte offset in `src` to a 1-based (line, col).
pub(crate) fn line_col_of(src: &str, offset: usize) -> (usize, usize) {
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
    if let Some(second) = iter.next() {
        return Err(ParseError::Xml(
            "duplicate <menubar>: at most one allowed per <root>".to_string(),
        )
        .at(second.range().start));
    }
    let mut bar = MenuBarSpec::default();
    for menu_node in node.children().filter(|c| c.is_element()) {
        if menu_node.tag_name().name() != "menu" {
            return Err(ParseError::Xml(format!(
                "<menubar> child must be <menu>, got <{}>",
                menu_node.tag_name().name()
            ))
            .at(menu_node.range().start));
        }
        let label = menu_node
            .attribute("label")
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ParseError::Xml("<menu> requires a label=\"...\" attribute".to_string())
                    .at(menu_node.range().start)
            })?;
        let mut items = Vec::new();
        for child in menu_node.children().filter(|c| c.is_element()) {
            match child.tag_name().name() {
                "menuitem" => {
                    let id = child
                        .attribute("id")
                        .map(|s| s.to_string())
                        .ok_or_else(|| {
                            ParseError::Xml(
                                "<menuitem> requires an id=\"...\" attribute".to_string(),
                            )
                            .at(child.range().start)
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
                        "<menu> child must be <menuitem> or <separator>, got <{}>",
                        other
                    ))
                    .at(child.range().start));
                }
            }
        }
        bar.menus.push(MenuSpec { label, items });
    }
    Ok(Some(bar))
}

/// Give an element one of the styles the parser decides for it, as if the
/// markup had written it on the element; see
/// [`lumen_ir::css::apply_markup_declaration`] for why it goes through the
/// markup record rather than straight into a field.
fn set_built_style(attrs: &mut Attributes, property: &str, value: &str) {
    match lumen_ir::css::apply_markup_declaration(property, value, attrs) {
        Ok(true) => {}
        other => unreachable!(
            "the parser wrote a style it cannot apply: {property}: {value} ({other:?})"
        ),
    }
}

/// [`set_built_style`] for every declaration in `decls`, in order.
fn set_built_styles(attrs: &mut Attributes, decls: &[(&str, &str)]) {
    for (property, value) in decls {
        set_built_style(attrs, property, value);
    }
}

/// Collapse an authoring tag that stands for a composed widget - `<tooltip>`,
/// `<dropdown>`, `<menu>`, `<date-picker>`, `<time-picker>` - into the
/// primitive subtree it means, and reject the child tags that are only legal
/// inside one of them. `None` means the tag spawns as itself and the caller
/// builds it directly, as a `<tabs>` and its `<tab>`s do.
///
/// Separate from [`build_element`], and each widget in its own function,
/// because the parser recurses once per level of markup and `<tooltip>`
/// recurses from inside its builder. An unoptimized build gives every local
/// in a function its own stack slot whether or not the branch that uses it
/// runs, so a builder holding the whole widget catalog would put all of it on
/// the stack at every depth a `<tooltip>` is nested.
#[allow(clippy::too_many_arguments)]
fn build_composed_widget(
    node: roxmltree::Node,
    tag: &str,
    script_buf: &mut InlineScripts,
    external_scripts: &mut Vec<String>,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
    depth: u32,
) -> Result<Option<Element>, ParseError> {
    match tag {
        "tooltip" => build_tooltip(
            node,
            script_buf,
            external_scripts,
            src,
            lint_findings,
            frag,
            in_for_body,
            depth,
        )
        .map(Some),
        // `<tab>` outside of `<tabs>` is meaningless - collapsing it to
        // its first child would be silently surprising. Reject so authors
        // get a clear error. Inside one it is an element like any other.
        "tab" if node.parent_element().map(|p| p.tag_name().name()) != Some("tabs") => Err(
            ParseError::Xml("<tab> may only appear inside <tabs>".to_string())
                .at(node.range().start),
        ),
        "dropdown" => build_dropdown(node, tag, src, lint_findings, frag, in_for_body).map(Some),
        // `<option>` outside of `<dropdown>` is meaningless - same
        // rejection rule as `<tab>` outside of `<tabs>`.
        "option" => Err(
            ParseError::Xml("<option> may only appear inside <dropdown>".to_string())
                .at(node.range().start),
        ),
        "menu" => build_menu(
            node,
            script_buf,
            external_scripts,
            src,
            lint_findings,
            frag,
            in_for_body,
            depth,
        )
        .map(Some),
        // A menu's rows, written in it or in a `<for>` in it, are elements
        // of their own; anywhere else these are a mistake.
        "menuitem" | "separator" if in_menu(node) => Ok(None),
        "menuitem" | "separator" => Err(ParseError::Xml(format!(
            "<{tag}> may only appear inside <menu> or <menubar>"
        ))
        .at(node.range().start)),
        "date-picker" | "time-picker" => {
            build_picker(node, tag, src, lint_findings, frag, in_for_body).map(Some)
        }
        _ => Ok(None),
    }
}

/// `<tooltip text="..." delay="...">...trigger...</tooltip>` flattens to
/// the trigger child with a `TooltipSpec` attached. The trigger
/// must be a single layout element; multi-child tooltips are
/// rejected here so authors get a clear parse error instead of a
/// silent first-child pickup.
#[allow(clippy::too_many_arguments)]
fn build_tooltip(
    node: roxmltree::Node,
    script_buf: &mut InlineScripts,
    external_scripts: &mut Vec<String>,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
    depth: u32,
) -> Result<Element, ParseError> {
    let text = node.attribute("text").unwrap_or("").to_string();
    // The wrapper authors the popup body, so it carries the key for it.
    // Empty is rejected here for the same reason the generic attribute
    // rejects it: an empty key can never resolve.
    let translatable = match node.attribute("translatable").map(str::trim) {
        Some("") => {
            return Err(ParseError::Xml(
                "<tooltip>: translatable attribute requires a \
                 catalogue key (e.g. \"app-title\")"
                    .to_string(),
            )
            .at(node.range().start));
        }
        other => other.map(str::to_string),
    };
    let delay_ms = node.attribute("delay").and_then(|s| s.parse::<u32>().ok());
    let offset = node.attribute("offset").and_then(|s| s.parse::<f32>().ok());
    let mut child_nodes = node
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() != "script");
    let Some(child) = child_nodes.next() else {
        return Err(ParseError::Xml(format!(
            "<tooltip text=\"{text}\"> must wrap exactly one trigger element"
        ))
        .at(node.range().start));
    };
    if child_nodes.next().is_some() {
        return Err(ParseError::Xml(format!(
            "<tooltip text=\"{text}\"> must wrap exactly one trigger element \
             (got >1)"
        ))
        .at(node.range().start));
    }
    let mut elem = build_element(
        child,
        script_buf,
        external_scripts,
        src,
        lint_findings,
        frag,
        in_for_body,
        depth + 1,
    )
    .map_err(|e| e.at(child.range().start))?;
    elem.attrs.tooltip = Some(crate::layout_ir::TooltipSpec {
        text,
        translatable,
        delay_ms,
        offset,
    });
    elem.attrs.widget = Some(WidgetRole::Tooltip);
    Ok(elem)
}

/// `<dropdown bind-value="signal">` + `<option value="x"
/// label="X"/>` children collapse to a column with a header button
/// The header text binds to the active value signal; clicking it toggles a synthetic
/// `__dropdown_open:<signal>` flag (managed by `lumen_primitives::DropdownButton`).
/// Each option button writes the value and clears the flag.
fn build_dropdown(
    node: roxmltree::Node,
    tag: &str,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
) -> Result<Element, ParseError> {
    let signal_name = node
        .attribute("bind-value")
        .map(signal_attr_name)
        .ok_or_else(|| {
            ParseError::Xml("<dropdown> requires bind-value=\"signal-name\"".to_string())
                .at(node.range().start)
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
                "<dropdown> child must be <option>, got <{}>",
                child.tag_name().name()
            ))
            .at(child.range().start));
        }
        let value = child
            .attribute("value")
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ParseError::Xml("<option> requires value=\"...\"".to_string())
                    .at(child.range().start)
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
        // An option row carries no sizing of its own: how tall it is
        // and how far its text sits from the edge are theme metrics,
        // and they live on `.dropdown-option` in the user-agent sheet
        // where a skin or an app can overrule them.
        let mut opt_btn = Element {
            tag: "button".to_string(),
            attrs: Box::default(),
            children: Vec::new(),
            ..Default::default()
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
    // Its size is the control's: `.dropdown-button` fills the
    // `<dropdown>` box in the user-agent sheet, so what the author
    // sizes is what the closed combobox measures, and its height and
    // padding stay theme metrics a skin can overrule.
    let mut header = Element {
        tag: "button".to_string(),
        ..Default::default()
    };
    header.attrs.tab_index = Some(0);
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
        children: options,
        ..Default::default()
    };
    set_built_styles(
        &mut panel.attrs,
        &[("position", "absolute"), ("inset", "40 0 -1 0")],
    );
    panel.attrs.classes = vec!["dropdown-panel".to_string()];
    // Tag the panel so the runtime can dismiss it on an outside
    // click and flip it above the trigger near the viewport bottom.
    panel.attrs.popup_panel = Some(open_signal.clone());

    // Wrap the panel in `<if eq="true" mode="hide">` keyed on the
    // open signal so it shows / hides without despawning options.
    let mut panel_if = Element {
        tag: "if".to_string(),
        attrs: Box::default(),
        children: vec![panel],
        ..Default::default()
    };
    panel_if.attrs.if_signal = Some(open_signal.clone());
    panel_if.attrs.if_eq = Some("true".to_string());
    panel_if.attrs.if_mode = crate::layout_ir::IfModeSpec::Hide;

    // The box the author sizes and styles. It keeps the authored tag
    // and takes the attributes written on it, so `width="240"` and a
    // `dropdown { ... }` rule reach the control the same way they
    // reach a `<button>`; the parts it expands into are reachable by
    // class beneath it.
    // Its column direction comes from the `dropdown` rule in the
    // user-agent sheet, and like every element it anchors the floating
    // panel inside it.
    let mut attrs = Box::<Attributes>::default();
    let mut slots: Vec<InterpolationSlot> = Vec::new();
    apply_authored_attributes(
        node,
        tag,
        &mut attrs,
        &mut slots,
        src,
        lint_findings,
        frag,
        in_for_body,
    )?;
    // `bind-value` and `placeholder` are the widget's own vocabulary,
    // spent above on the header's text binding and its resting text.
    // Leaving them here would bind the box to the same signal and
    // give it a second copy of the hint.
    attrs.bind = None;
    attrs.placeholder = None;
    attrs.classes.push("dropdown".to_string());
    // Seed the open-panel signal to "false" so the panel hides at
    // startup.
    attrs.signal_seed = Some((open_signal, "false".to_string()));
    attrs.widget = Some(WidgetRole::Dropdown);
    Ok(Element {
        tag: tag.to_string(),
        attrs,
        children: vec![header, panel_if],
        interpolations: slots,
        ..Default::default()
    })
}

/// `<menu id="m">` becomes a floating `menu` element behind an `<if>` keyed
/// on `__menu_open:m`, holding its `<menuitem>` and `<separator>` elements,
/// and any `<for>` that builds them from a list, as they were written.
/// `<menubar>`-nested `<menu>` blocks are extracted upstream by
/// `extract_menubar` and never reach `build_element`.
#[allow(clippy::too_many_arguments)]
fn build_menu(
    node: roxmltree::Node,
    script_buf: &mut InlineScripts,
    external_scripts: &mut Vec<String>,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
    depth: u32,
) -> Result<Element, ParseError> {
    let menu_id = node.attribute("id").map(|s| s.to_string()).ok_or_else(|| {
        ParseError::Xml("<menu> requires id=\"...\"".to_string()).at(node.range().start)
    })?;
    let mut items = Vec::new();
    for child in node
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() != "script")
    {
        check_menu_row(child, false)?;
        items.push(
            build_element(
                child,
                script_buf,
                external_scripts,
                src,
                lint_findings,
                frag,
                in_for_body || child.tag_name().name() == "for",
                depth + 1,
            )
            .map_err(|e| e.at(child.range().start))?,
        );
    }
    // The popup an in-window menu bar builds its menus from too.
    Ok(lumen_ir::menu::popup_menu_of(&menu_id, items))
}

/// Refuse what a menu cannot hold: anything but `<menuitem>`,
/// `<separator>`, or (directly in the menu) a `<for>` whose rows are those,
/// and an item with no `id`.
fn check_menu_row(node: roxmltree::Node, in_for: bool) -> Result<(), ParseError> {
    match node.tag_name().name() {
        "menuitem" if node.attribute("id").is_none() => Err(ParseError::Xml(
            "<menuitem> requires id=\"...\"".to_string(),
        )
        .at(node.range().start)),
        "menuitem" | "separator" => Ok(()),
        "for" if !in_for => node
            .children()
            .filter(|c| c.is_element())
            .try_for_each(|c| check_menu_row(c, true)),
        other => Err(ParseError::Xml(format!(
            "<menu> child must be <menuitem>, <separator> or <for>, got <{other}>"
        ))
        .at(node.range().start)),
    }
}

/// Whether `node` is a row of a `<menu>` outside a menu bar: written in it,
/// or in a `<for>` written in it.
fn in_menu(node: roxmltree::Node) -> bool {
    let is_menu = |n: Option<roxmltree::Node>| {
        n.is_some_and(|n| {
            n.tag_name().name() == "menu"
                && n.parent_element().map(|p| p.tag_name().name()) != Some("menubar")
        })
    };
    let parent = node.parent_element();
    is_menu(parent)
        || (parent.is_some_and(|p| p.tag_name().name() == "for")
            && is_menu(parent.and_then(|p| p.parent_element())))
}

/// Collapse `<date-picker>` and `<time-picker>` into a validated `<input>` with a built-in pattern that enforces the shape.
/// Form validation mirrors the value into `valid:<id>`; authors hook the existing `text-input` commit dispatch on submit.
fn build_picker(
    node: roxmltree::Node,
    tag: &str,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
) -> Result<Element, ParseError> {
    let is_time = tag == "time-picker";
    let signal_name = node
        .attribute("bind-value")
        .map(signal_attr_name)
        .ok_or_else(|| {
            ParseError::Xml(format!("<{tag}> requires bind-value=\"signal-name\""))
                .at(node.range().start)
        })?;
    // The generated field is an `<input>`, so the attributes written on
    // the picker (`class`, `tab-index`, `required`, sizing, ...) reach it
    // the way they reach a written `<input>`.
    let mut attrs = Box::<Attributes>::default();
    let mut slots: Vec<InterpolationSlot> = Vec::new();
    apply_authored_attributes(
        node,
        "input",
        &mut attrs,
        &mut slots,
        src,
        lint_findings,
        frag,
        in_for_body,
    )?;
    if attrs.placeholder.is_none() {
        attrs.placeholder = Some(if is_time {
            "HH:MM".to_string()
        } else {
            "YYYY-MM-DD".to_string()
        });
    }
    // Its default width is `.date-picker` / `.time-picker` in the
    // user-agent sheet.
    // In the Tab order like every other text field, unless the author
    // took it out.
    if attrs.tab_index.is_none() {
        attrs.tab_index = Some(0);
    }
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
    // `bind-value` is the picker's own vocabulary: the field edits the
    // signal as text.
    attrs.bind = Some(crate::layout_ir::BindSpec {
        kind: BindKind::Text,
        name: signal_name,
    });
    attrs.pattern = Some(pattern.to_string());
    attrs.classes.insert(0, class.to_string());
    attrs.widget = Some(if is_time {
        WidgetRole::TimePicker
    } else {
        WidgetRole::DatePicker
    });
    let input = Element {
        tag: "input".to_string(),
        attrs,
        children: Vec::new(),
        interpolations: slots,
        ..Default::default()
    };
    Ok(input)
}

/// Synthesize the part children a widget tag paints itself out of, after the
/// generic attribute pass so the parts see the finished attribute bag.
/// `<checkbox>` and `<radio>` gain an indicator tile and a caption label;
/// `<progress>` gains its fill tile. Every part is class-tagged so its
/// visuals stay CSS-reachable.
///
/// Separate from [`build_element`] for the reason [`build_composed_widget`]
/// is: these locals would otherwise hold stack in every recursive frame,
/// including the ones building a tag that has no parts.
fn synthesize_widget_parts(
    node: roxmltree::Node,
    tag: &str,
    attrs: &mut Attributes,
    children: &mut Vec<Element>,
) -> Result<(), ParseError> {
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
                return Err(ParseError::Xml(
                    "<radio> requires group=\"signal\" and value=\"...\"".to_string(),
                )
                .at(node.range().start));
            };
            // `checked="true"` seeds the group signal (first spawned
            // checked member wins; the runtime falls back to the first
            // enabled member when no one is checked).
            if attrs.checked == Some(true) {
                attrs.signal_seed = Some((group.clone(), value.clone()));
            }
        }
        // The row the box and the caption sit on. Written here rather than
        // in `ua.css` for the reason the `.dropdown-button` rule there
        // gives: a skin does not get to move the caption onto the box.
        if attrs.align.is_none() {
            set_built_style(attrs, "align", "center");
        }
        if attrs.gap.is_none() {
            set_built_style(attrs, "gap", "8");
        }
        let (part, label_class) = if tag == "checkbox" {
            (crate::layout_ir::WidgetPart::CheckboxBox, "checkbox-label")
        } else {
            (crate::layout_ir::WidgetPart::RadioDot, "radio-label")
        };
        let box_class = part.class();
        let mut indicator = Element {
            tag: "tile".to_string(),
            attrs: Box::new(Attributes {
                part: Some(part),
                ..Attributes::default()
            }),
            children: Vec::new(),
            ..Default::default()
        };
        set_built_style(&mut indicator.attrs, "flex-shrink", "0");
        indicator.attrs.classes = vec![box_class.to_string()];
        let mut synthesized = vec![indicator];
        if let Some(label) = attrs.text.take() {
            let mut lbl = Element {
                tag: "label".to_string(),
                attrs: Box::default(),
                children: Vec::new(),
                ..Default::default()
            };
            lbl.attrs.text = Some(label);
            // The key that names a string moves with the string. Left on the
            // root it would resolve against an element that no longer has any
            // text, and the caption the user reads would never reach the
            // catalogue.
            lbl.attrs.translatable = attrs.translatable.take();
            lbl.attrs.classes = vec![label_class.to_string()];
            synthesized.push(lbl);
        }
        // Synthesized parts paint first (indicator leads the caption);
        // any authored children follow.
        synthesized.append(children);
        *children = synthesized;
    }

    // `<input type="number">`: the field stays the element, holding the
    // number as text, and gains the steps box it floats along its right end
    // with a `.number-step-down` and a `.number-step-up` button. The runtime
    // keeps the box's width clear of the text. On the web the browser draws
    // its own number field, so none of the parts become elements there.
    if tag == "input" && attrs.input_type == Some(crate::layout_ir::InputType::Number) {
        if !attrs.classes.iter().any(|c| c == "number-input") {
            attrs.classes.insert(0, "number-input".to_string());
        }
        let button = |part: crate::layout_ir::WidgetPart, label: &str| {
            let mut b = Element {
                tag: "button".to_string(),
                attrs: Box::new(Attributes {
                    part: Some(part),
                    ..Attributes::default()
                }),
                ..Default::default()
            };
            b.attrs.text = Some(label.to_string());
            b.attrs.classes = vec![part.class().to_string()];
            b
        };
        let mut steps = Element {
            tag: "row".to_string(),
            attrs: Box::new(Attributes {
                part: Some(crate::layout_ir::WidgetPart::NumberSteps),
                ..Attributes::default()
            }),
            children: vec![
                button(crate::layout_ir::WidgetPart::StepDown, "-"),
                button(crate::layout_ir::WidgetPart::StepUp, "+"),
            ],
            ..Default::default()
        };
        set_built_styles(
            &mut steps.attrs,
            &[("position", "absolute"), ("inset", "0 0 0 auto")],
        );
        steps.attrs.classes = vec![
            crate::layout_ir::WidgetPart::NumberSteps
                .class()
                .to_string(),
        ];
        children.insert(0, steps);
    }

    // `<input type="color">`: the field stays the element, a face holding
    // the color as `#rrggbb`, and gains its parts: the `.color-swatch` that
    // shows the color, and the `.color-panel` popup with the saturation-value
    // area, the hue strip and the hex field. The runtime opens the panel,
    // places it and its thumbs, and paints the color into them; their looks
    // are CSS. On the web the browser draws its own color input, so none of
    // the parts become elements there.
    if tag == "input" && attrs.input_type == Some(crate::layout_ir::InputType::Color) {
        use crate::layout_ir::WidgetPart as P;
        // HTML's value sanitization: a value that names no color is opaque
        // black, and without `alpha` every color is made opaque.
        let mut rgba = attrs
            .text
            .as_deref()
            .and_then(lumen_core::components::parse_color_value)
            .unwrap_or([0, 0, 0, 255]);
        if !attrs.alpha {
            rgba[3] = 255;
        }
        attrs.text = Some(lumen_core::components::format_color_value(
            rgba,
            attrs.alpha,
        ));
        if !attrs.classes.iter().any(|c| c == "color-input") {
            attrs.classes.insert(0, "color-input".to_string());
        }
        let part = |tag: &str, part: Option<P>, class: &str, kids: Vec<Element>| {
            let mut el = Element {
                tag: tag.to_string(),
                attrs: Box::new(Attributes {
                    part,
                    ..Attributes::default()
                }),
                children: kids,
                ..Default::default()
            };
            el.attrs.classes = vec![class.to_string()];
            el
        };
        let floating = |mut el: Element| {
            set_built_styles(
                &mut el.attrs,
                &[("position", "absolute"), ("inset", "0 auto auto 0")],
            );
            el
        };
        let covering = |mut el: Element| {
            set_built_styles(&mut el.attrs, &[("position", "absolute"), ("inset", "0")]);
            el
        };
        let area = part(
            "tile",
            Some(P::ColorArea),
            P::ColorArea.class(),
            vec![
                covering(part("tile", None, "color-area-white", Vec::new())),
                covering(part("tile", None, "color-area-black", Vec::new())),
                floating(part(
                    "tile",
                    Some(P::ColorAreaThumb),
                    P::ColorAreaThumb.class(),
                    Vec::new(),
                )),
            ],
        );
        let hue = part(
            "tile",
            Some(P::ColorHue),
            P::ColorHue.class(),
            vec![floating(part(
                "tile",
                Some(P::ColorHueThumb),
                P::ColorHueThumb.class(),
                Vec::new(),
            ))],
        );
        let mut hex = part("input", Some(P::ColorHex), P::ColorHex.class(), Vec::new());
        hex.attrs.text = Some(lumen_core::components::format_hex_color(rgba, attrs.alpha));
        hex.attrs.tab_index = Some(0);
        let mut controls = vec![area, hue];
        // With `alpha` the panel gains the alpha strip: the clear-to-opaque
        // shading over whatever backdrop the skin gives the strip.
        if attrs.alpha {
            controls.push(part(
                "tile",
                Some(P::ColorAlpha),
                P::ColorAlpha.class(),
                vec![
                    covering(part(
                        "tile",
                        Some(P::ColorAlphaFill),
                        P::ColorAlphaFill.class(),
                        Vec::new(),
                    )),
                    floating(part(
                        "tile",
                        Some(P::ColorAlphaThumb),
                        P::ColorAlphaThumb.class(),
                        Vec::new(),
                    )),
                ],
            ));
        }
        controls.push(hex);
        let mut panel = floating(part(
            "column",
            Some(P::ColorPanel),
            P::ColorPanel.class(),
            controls,
        ));
        set_built_style(&mut panel.attrs, "display", "none");
        let swatch = part(
            "tile",
            Some(P::ColorSwatch),
            P::ColorSwatch.class(),
            Vec::new(),
        );
        children.splice(0..0, [swatch, panel]);
    }

    // `<tabs>`: the children stay the tabs, and a `.tab-strip` row of
    // `.tab-btn` buttons, one per tab in order, goes in front of them. The
    // runtime keeps the strip in step when tabs come, go or move, shows one
    // tab at a time, and builds the same strip for a `<tabs>` a script
    // assembles.
    if tag == "tabs" {
        use crate::layout_ir::{tab_button, tab_strip};
        let mut strip = tab_strip();
        for child in children.iter() {
            if child.tag != "tab" {
                return Err(ParseError::Xml(format!(
                    "<tabs> may hold only <tab> children, got <{}>",
                    child.tag
                ))
                .at(node.range().start));
            }
            let spec = child.attrs.tab.clone().unwrap_or_default();
            strip.children.push(tab_button(&spec, child.attrs.disabled));
        }
        children.insert(0, strip);
    }
    if tag == "tab" && attrs.tab.as_ref().is_none_or(|t| t.name.is_empty()) {
        return Err(
            ParseError::Xml("<tab> requires a name=\"...\" attribute".to_string())
                .at(node.range().start),
        );
    }

    // `<split>`: the children stay the panes, each marked `.split-pane`,
    // and a focusable `.split-gutter` goes between every two of them. The
    // runtime drags and steps the gutters; their looks are CSS. A `<for>`
    // or an `<if>` among the children is one pane.
    if tag == "split" && children.len() > 1 {
        use crate::layout_ir::{SPLIT_PANE_CLASS, split_gutter};
        let panes = std::mem::take(children);
        let count = panes.len();
        for (i, mut pane) in panes.into_iter().enumerate() {
            if !pane.attrs.classes.iter().any(|c| c == SPLIT_PANE_CLASS) {
                pane.attrs.classes.push(SPLIT_PANE_CLASS.to_string());
            }
            children.push(pane);
            if i + 1 < count {
                children.push(split_gutter());
            }
        }
    }

    // `<progress>` desugar (W5): the track stays the root; a single
    // absolute-positioned `.progress-fill` tile child is synthesized.
    // The runtime (`lumen_primitives::sync_progress_fill`) drives its
    // width (determinate) or sweep offset (indeterminate); everything
    // else - colors, radius, track height, sweep period - is CSS.
    if tag == "progress" {
        let mut fill = Element {
            tag: "tile".to_string(),
            attrs: Box::new(Attributes {
                part: Some(crate::layout_ir::WidgetPart::ProgressFill),
                ..Attributes::default()
            }),
            children: Vec::new(),
            ..Default::default()
        };
        // The far edges stay `auto`: the explicit width and 100% height
        // must not be over-constrained by them (same convention as the
        // toggle-knob seed style).
        set_built_styles(
            &mut fill.attrs,
            &[("position", "absolute"), ("inset", "0 auto auto 0")],
        );
        fill.attrs.classes = vec![
            crate::layout_ir::WidgetPart::ProgressFill
                .class()
                .to_string(),
        ];
        children.insert(0, fill);
    }

    Ok(())
}

/// Maximum element-tree nesting depth. Past this, `build_element` returns a
/// `ParseError` instead of recursing - a deeply nested (even well-formed)
/// `.lmn` would otherwise overflow the stack (a SIGSEGV, not a catchable
/// panic). Kept consistent with the template-expansion cap.
///
/// The cap only holds if a `build_element` frame stays small, which is why
/// the widget branches live in their own functions: an unoptimized build
/// reserves a slot for every local in the function, and a tag-specific branch
/// left inline would hold that stack at all 32 levels.
const MAX_ELEMENT_DEPTH: u32 = 32;

/// Write every attribute authored on `node` into `attrs`, and normalise the
/// `{...}` placeholders in the ones that carry them into `slots`.
///
/// Every element the parser produces comes through here, including the box a
/// composed widget expands into: `width` on a `<dropdown>` has to mean what it
/// means on any other tag, and a `dropdown` rule in a stylesheet has to reach
/// the same element the attribute did.
#[allow(clippy::too_many_arguments)]
fn apply_authored_attributes(
    node: roxmltree::Node,
    tag: &str,
    attrs: &mut Attributes,
    slots: &mut Vec<InterpolationSlot>,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
) -> Result<(), ParseError> {
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
            "label" if tag == "menuitem" => text_off = Some(off),
            "placeholder" => placeholder_off = Some(off),
            "src" => src_off = Some(off),
            "id" => id_off = Some(off),
            "class" => class_off = Some(off),
            _ => {}
        }
        // A fragment body writes markers into any attribute, and a value
        // like `tab-index="{n}"` is not an integer until an argument
        // replaces the marker. Hold the text and parse it per use site.
        if frag.in_body && a.value().contains('{') && !interpolated_attribute(a.name()) {
            let (line, col) = line_col_of(src, off);
            attrs.deferred.push(DeferredAttr {
                name: a.name().to_string(),
                value: a.value().to_string(),
                line,
                col,
            });
            continue;
        }
        let mut ctx = AttrCtx {
            src,
            value_offset: off,
            // Reborrow: the sink outlives every attribute in the loop.
            lint_findings: &mut *lint_findings,
        };
        // An attribute's error is placed at the attribute.
        let applied = apply_attribute(tag, a.name(), a.value(), attrs, &mut ctx)
            .map_err(|e| e.at(a.range().start))?;
        // Which surface set a value is not recoverable from the fields
        // `apply_attribute` just wrote, and a target that ranks a rule above
        // an attribute needs to know. Record the styling ones under the
        // spelling the cascade files them under; anything else on the element
        // (`id`, `text`, `bind-value`) is not a style and is skipped, as is an
        // attribute the vocabulary has no meaning for and dropped.
        if applied && let Some(property) = canonical_style_property(a.name()) {
            attrs
                .markup_styles
                .push((property.to_string(), a.value().to_string()));
        }
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
    if let Some(t) = attrs.text.take() {
        let off = text_off.unwrap_or(0);
        attrs.text = Some(normalize_dollar_interpolation(
            &t,
            off,
            src,
            lint_findings,
            slots,
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
            slots,
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
            slots,
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
            slots,
            in_for_body,
        ));
    }
    if !attrs.classes.is_empty() {
        let off = class_off.unwrap_or(0);
        attrs.classes = std::mem::take(&mut attrs.classes)
            .into_iter()
            .map(|c| {
                normalize_dollar_interpolation(&c, off, src, lint_findings, slots, in_for_body)
            })
            .collect();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_element(
    node: roxmltree::Node,
    script_buf: &mut InlineScripts,
    external_scripts: &mut Vec<String>,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
    depth: u32,
) -> Result<Element, ParseError> {
    if depth > MAX_ELEMENT_DEPTH {
        return Err(ParseError::Xml(format!(
            "element nesting exceeded depth {MAX_ELEMENT_DEPTH} (too deeply nested)"
        ))
        .at(node.range().start));
    }
    let tag = node.tag_name().name().to_string();
    // A use site stands in for the fragment it names until the tree is
    // inlined, so it never reaches the tag vocabulary below.
    if let Some(key) = frag.use_key(node, &tag) {
        if key.is_empty() {
            return Err(
                ParseError::Xml("<use> requires a template=\"...\" attribute".to_string())
                    .at(node.range().start),
            );
        }
        return build_fragment_use(
            node,
            tag,
            key,
            script_buf,
            external_scripts,
            src,
            lint_findings,
            frag,
            in_for_body,
            depth,
        );
    }
    if frag.in_body && tag == SLOT_TAG {
        return build_slot(
            node,
            script_buf,
            external_scripts,
            src,
            lint_findings,
            frag,
            in_for_body,
            depth,
        );
    }
    // Built-in tags first; fall back to the lumen-widget runtime
    // registry so `#[derive(Widget)] #[widget(tag="my-thing")]` widgets
    // that called `MyThing::register()` at startup are accepted instead
    // of rejected as `UnknownTag`. The Widget derive populates the
    // registry; lumenc itself only consults it.
    if !KNOWN_TAGS.contains(&tag.as_str()) && !lumen_widget::is_widget_tag_registered(tag.as_str())
    {
        return Err(ParseError::UnknownTag(tag, node.range().start));
    }

    // An authoring tag that stands for a composed widget collapses into the
    // primitive subtree it means before anything else looks at the node.
    if let Some(composed) = build_composed_widget(
        node,
        &tag,
        script_buf,
        external_scripts,
        src,
        lint_findings,
        frag,
        in_for_body,
        depth,
    )? {
        return Ok(composed);
    }

    // Built on the heap: this function recurses once per level of markup,
    // and an unoptimized build keeps a by-value attribute bag (and the
    // temporary a struct literal fills it from) in every frame.
    let mut attrs = Box::<Attributes>::default();
    // A tag's default styles (a `<column>`'s direction, an
    // `<overlay>`'s placement, a `<spacer>`'s grow, control sizing
    // floors) are user-agent rules in `core/runtime/src/skins/ua.css`,
    // not values written here. Written here they would be inline
    // origin, which outranks every rule, and a restyle that rebuilds
    // the element from the cascade could not rebuild them. The web
    // target keeps its own copy of them in web/src/reset.css. Two
    // cases that don't fit a plain CSS rule (a `<switch>` `min-width`
    // that only applies when `width` is also unset, and the `<input>`
    // / `<textarea>` `overflow: hidden` default backing off a
    // shorthand `overflow` authored anywhere) stay in
    // `lumen_scene::spawn::apply_ua_style_defaults`, which spawning
    // and a restyle both run.
    // <input> seeds an empty editable buffer so the focus router
    // and the renderer have something to point at without the
    // author having to write `text=""`.
    attrs.text = if tag == "input" {
        Some(String::new())
    } else {
        None
    };
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
    attrs.tab_index = match tag.as_str() {
        "input" | "textarea" | "button" | "toggle" | "switch" | "slider" | "checkbox" => Some(0),
        "radio" => Some(-1),
        _ => None,
    };
    // A registered custom tag keeps its attributes as authored, for the
    // widget the spawner hands them to; `apply_attribute` fills it.
    attrs.widget_attributes = if KNOWN_TAGS.contains(&tag.as_str()) {
        None
    } else {
        Some(Vec::new())
    };

    let mut slots: Vec<InterpolationSlot> = Vec::new();
    apply_authored_attributes(
        node,
        &tag,
        &mut attrs,
        &mut slots,
        src,
        lint_findings,
        frag,
        in_for_body,
    )?;
    // A menu item with no label shows its id.
    if tag == "menuitem" && attrs.text.is_none() {
        attrs.text = attrs.id.clone();
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
    build_children(
        node,
        &mut attrs,
        &mut children,
        &mut slots,
        script_buf,
        external_scripts,
        src,
        lint_findings,
        frag,
        child_in_for,
        depth,
    )?;

    // `<button default="true">` gains the `default` class here - after
    // the whole attribute loop, so a later `class="..."` attr can't
    // clobber it - letting skins style `button.default` through the
    // compile-time cascade.
    if attrs.default_button && !attrs.classes.iter().any(|c| c == "default") {
        attrs.classes.push("default".to_string());
    }

    synthesize_widget_parts(node, &tag, &mut attrs, &mut children)?;

    Ok(Element {
        tag,
        attrs,
        children,
        interpolations: slots,
        ..Default::default()
    })
}

/// Walk `node`'s children into `children`, taking the element's own text
/// content, its inline scripts, and the tags that never become layout nodes
/// on the way.
///
/// `in_for_body` is already the value the children see: the caller decides
/// whether it is opening a `<for>` body.
#[allow(clippy::too_many_arguments)]
fn build_children(
    node: roxmltree::Node,
    attrs: &mut Attributes,
    children: &mut Vec<Element>,
    slots: &mut Vec<InterpolationSlot>,
    script_buf: &mut InlineScripts,
    external_scripts: &mut Vec<String>,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
    depth: u32,
) -> Result<(), ParseError> {
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
                    slots,
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
            // `extract_menubar`, and `<template>` by the fragment collection
            // pass, both before the walk reaches them; skipping here keeps
            // unknown-tag checks happy and prevents their authoring nesting
            // from being layout-spawned.
            if matches!(child.tag_name().name(), "menubar" | "template") {
                continue;
            }
            if child.tag_name().name() == "script" {
                if let Some(script_src) = child.attribute("src") {
                    external_scripts.push(script_src.to_string());
                } else if let Some(s) = child.text() {
                    let at = child
                        .children()
                        .find(|n| n.is_text())
                        .map_or(child.range().start, |n| n.range().start);
                    script_buf.push(s, src, at);
                }
                continue;
            }
            // An error inside the child that names no place of its own is
            // placed at the child.
            children.push(
                build_element(
                    child,
                    script_buf,
                    external_scripts,
                    src,
                    lint_findings,
                    frag,
                    in_for_body,
                    depth + 1,
                )
                .map_err(|e| e.at(child.range().start))?,
            );
        }
    }
    Ok(())
}

/// Write an attribute a fragment body held back, now that its markers are
/// resolved.
///
/// `line` / `col` are where the value was written, which is a different file
/// from the one being parsed whenever the fragment came from elsewhere, so
/// any finding the value raises is repositioned onto them.
pub(crate) fn apply_deferred_attribute(
    tag: &str,
    attr: &DeferredAttr,
    value: &str,
    attrs: &mut Attributes,
    lint_findings: &mut Vec<LintFinding>,
) -> Result<(), ParseError> {
    let mut raised = Vec::new();
    let mut ctx = AttrCtx {
        src: "",
        value_offset: 0,
        lint_findings: &mut raised,
    };
    let applied = apply_attribute(tag, &attr.name, value, attrs, &mut ctx)?;
    for mut finding in raised {
        finding.line = attr.line;
        finding.col = attr.col;
        lint_findings.push(finding);
    }
    if applied && let Some(property) = canonical_style_property(&attr.name) {
        attrs
            .markup_styles
            .push((property.to_string(), value.to_string()));
    }
    Ok(())
}

/// Classify the `{marker}` sites in a value the compiler produced rather
/// than read from a file, normalizing `{$name}` to `{name}` the same way an
/// authored value is normalized.
pub(crate) fn classify_markers(value: &str) -> (String, Vec<InterpolationSlot>) {
    let mut slots = Vec::new();
    let mut discarded = Vec::new();
    let text = normalize_dollar_interpolation(value, 0, "", &mut discarded, &mut slots, false);
    (text, slots)
}

/// Does this attribute hold text that `{marker}` resolution reads?
///
/// The rest of the vocabulary parses its value into typed layout data, so a
/// marker in one of those is held back and parsed per use site instead. See
/// [`Attributes::deferred`](crate::layout_ir::Attributes::deferred).
fn interpolated_attribute(name: &str) -> bool {
    matches!(name, "text" | "placeholder" | "src" | "id" | "class")
}

/// Build the element that stands in for a fragment until the tree is
/// inlined.
///
/// Every attribute except the `<use template=>` selector becomes an argument,
/// `id` included: it seeds the per-instance id prefix and is bindable like any
/// other. The use site's own children ride along as the content its `<slot>`
/// receives.
#[allow(clippy::too_many_arguments)]
fn build_fragment_use(
    node: roxmltree::Node,
    tag: String,
    key: String,
    script_buf: &mut InlineScripts,
    external_scripts: &mut Vec<String>,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
    depth: u32,
) -> Result<Element, ParseError> {
    let args: Vec<(String, String)> = node
        .attributes()
        .filter(|a| !(tag == "use" && a.name() == "template"))
        .map(|a| (a.name().to_string(), a.value().to_string()))
        .collect();
    let mut slots = Vec::new();
    let mut attrs = Box::<Attributes>::default();
    if let Some(a) = node.attributes().find(|a| a.name() == "id") {
        attrs.id = Some(normalize_dollar_interpolation(
            a.value(),
            a.range_value().start,
            src,
            lint_findings,
            &mut slots,
            in_for_body,
        ));
    }
    let mut children = Vec::new();
    build_children(
        node,
        &mut attrs,
        &mut children,
        &mut slots,
        script_buf,
        external_scripts,
        src,
        lint_findings,
        frag,
        in_for_body,
        depth,
    )?;
    let slot_children = !children.is_empty() || attrs.text.is_some();
    Ok(Element {
        tag,
        attrs,
        children,
        interpolations: slots,
        frag_use: Some(Box::new(FragmentUse {
            key,
            args,
            slot_children,
        })),
    })
}

/// Build the marker a fragment body writes where the use site's content
/// lands.
///
/// The fallback is resolved here rather than at instantiation: a non-empty
/// `default` wins over the marker's own content, and both are known now. A
/// default that carries tags is markup; one that does not is text for the
/// element holding the marker.
///
/// `name` says which slot this is; a marker without one is the default slot,
/// which is the only one markup can hand content to.
#[allow(clippy::too_many_arguments)]
fn build_slot(
    node: roxmltree::Node,
    script_buf: &mut InlineScripts,
    external_scripts: &mut Vec<String>,
    src: &str,
    lint_findings: &mut Vec<LintFinding>,
    frag: &FragCtx<'_>,
    in_for_body: bool,
    depth: u32,
) -> Result<Element, ParseError> {
    let mut attrs = Box::<Attributes>::default();
    attrs.slot_name = node.attribute("name").map(|n| n.to_string());
    let mut children = Vec::new();
    let mut slots = Vec::new();
    let default = node.attribute("default").unwrap_or_default();
    if default.is_empty() {
        build_children(
            node,
            &mut attrs,
            &mut children,
            &mut slots,
            script_buf,
            external_scripts,
            src,
            lint_findings,
            frag,
            in_for_body,
            depth,
        )?;
    } else if default.contains('<') {
        let wrapped = format!("<{SLOT_TAG}>{default}</{SLOT_TAG}>");
        let doc =
            roxmltree::Document::parse(&wrapped).map_err(|e| ParseError::Xml(e.to_string()))?;
        build_children(
            doc.root_element(),
            &mut attrs,
            &mut children,
            &mut slots,
            script_buf,
            external_scripts,
            &wrapped,
            lint_findings,
            frag,
            in_for_body,
            depth,
        )?;
    } else {
        attrs.text = Some(default.to_string());
    }
    Ok(Element {
        tag: SLOT_TAG.to_string(),
        attrs,
        children,
        interpolations: slots,
        ..Default::default()
    })
}

/// Write one authored attribute into `attrs`, reporting whether the markup
/// vocabulary has a meaning for it. `false` means the value was dropped, which
/// is what keeps a dropped value out of the record of what the element was
/// styled with.
fn apply_attribute(
    tag: &str,
    name: &str,
    value: &str,
    attrs: &mut Attributes,
    ctx: &mut AttrCtx<'_>,
) -> Result<bool, ParseError> {
    // A custom widget reads its own attributes out of the authored bag, so
    // every one is kept, whatever the vocabulary below makes of it.
    if let Some(bag) = &mut attrs.widget_attributes {
        bag.push((name.to_string(), value.to_string()));
    }
    let mut recognised = true;
    match name {
        // `<tabs bind-value>` names the signal holding the shown tab's
        // name; without it the element keeps the choice itself.
        "bind-value" if tag == "tabs" => {
            attrs.active_signal = Some(signal_attr_name(value));
        }
        // `<tab name label>`: what picks the tab and what its strip button
        // says.
        "name" if tag == "tab" => {
            let label = attrs.tab.take().map(|t| t.label).filter(|l| !l.is_empty());
            attrs.tab = Some(crate::layout_ir::TabSpec::new(value, label));
        }
        "label" if tag == "tab" => {
            let mut spec = attrs.tab.take().unwrap_or_default();
            spec.label = value.to_string();
            attrs.tab = Some(spec);
        }
        // `<menuitem label accel>`: the item's text, and the shortcut it
        // shows beside it.
        "label" if tag == "menuitem" => attrs.text = Some(value.to_string()),
        "accel" if tag == "menuitem" => {
            attrs.shortcut = Some(value.to_string()).filter(|a| !a.trim().is_empty());
        }
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
            // A horizontal scroller stacks its children left to right;
            // vertical and both keep the direction the tag has. Written as
            // the element's own style, so it outranks the tag's `column`
            // from the user-agent sheet the way any attribute does.
            if matches!(axis, ScrollAxisSpec::X) {
                set_built_style(attrs, "flex-direction", "row");
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
        // Kept even when empty: `alt=""` is how an author marks an image as
        // decorative, and dropping it would leave the image unlabelled
        // instead of deliberately unlabelled.
        "alt" if tag == "image" => attrs.alt = Some(value.to_string()),
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
            reject_arg_binding(tag, name, value)?;
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
            let signal = signal_attr_name(trimmed);
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
            reject_arg_binding(tag, name, value)?;
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
            let signal = signal_attr_name(trimmed);
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
                // A custom tag's own binding (a module element's
                // `bind-window-title`) is the tag owner's to read from the
                // attribute bag, as any other attribute of its own.
                _ if attrs.widget_attributes.is_some() && !KNOWN_TAGS.contains(&tag) => {
                    return Ok(false);
                }
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
            reject_arg_binding(tag, name, value)?;
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
                let signal = signal_attr_name(trimmed);
                attrs.bind = Some(BindSpec { kind, name: signal });
            }
        }
        "each" => {
            // Wave-A: strip the optional leading `$` so `each="$users"`
            // is identical to `each="users"`. The `ArraySignals` store
            // keys by bare name; the `$` prefix is a reviewer-facing
            // marker that the author is referencing a signal.
            attrs.each = Some(signal_attr_name(value));
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
        "inset" => attrs.inset = Some(parse_inset_edges(tag, name, value)?),
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
        "fit" => attrs.image_fit = Some(crate::values::parse_image_fit(tag, name, value)?),
        "bg-fit" => attrs.bg_fit = Some(crate::values::parse_image_fit(tag, name, value)?),
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
            attrs.if_signal = Some(signal_attr_name(value));
        }
        // `<dialog open="signal">` is sugar for `<if signal="signal"
        // mode="hide">`. Preserving children state across show/hide is
        // the right default for modal forms.
        "open" if tag == "dialog" => {
            attrs.if_signal = Some(signal_attr_name(value));
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
        // An `<input>`'s value is the text it starts with, as in HTML: a
        // number for a number field, `#rrggbb` for a color field.
        "value" if tag == "input" => attrs.text = Some(value.to_string()),
        "value" => attrs.value = Some(parse_f32(tag, name, value)?),
        "group" if tag == "radio" => {
            // A signal name like every `bind-*`: `$cat` and `cat` are one signal.
            let group = signal_attr_name(value);
            if group.is_empty() {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "radio group requires a signal name".to_string(),
                ));
            }
            attrs.radio_group = Some(group);
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
        // `step="any"` on a number field accepts any number and steps by
        // one, as HTML's does; it is recorded as a step of 0.
        "step" if tag == "input" && value.trim().eq_ignore_ascii_case("any") => {
            attrs.step = Some(0.0);
        }
        "step" => attrs.step = Some(parse_f32(tag, name, value)?),
        // HTML's input types. `text` and every type Lumen does not implement
        // are a text field, which is also what a browser falls back to; the
        // ones it does not implement say so.
        "type" if tag == "input" => {
            attrs.input_type = crate::layout_ir::InputType::from_name(value);
            if attrs.input_type.is_none() && !value.trim().eq_ignore_ascii_case("text") {
                let (line, col) = line_col_of(ctx.src, ctx.value_offset);
                ctx.lint_findings.push(LintFinding {
                    kind: LintKind::UnknownAttribute,
                    severity: LintSeverity::Warn,
                    message: format!(
                        "`<input type=\"{value}\">` is not an input type Lumen implements; \
                         the field is a text field. The types are `text`, `number` and \
                         `color`."
                    ),
                    line,
                    col,
                    suggest: None,
                });
            }
        }
        "required" => {
            attrs.required = ctx.bool_value(tag, name, value);
        }
        // HTML's `alpha` on a color input: the alpha can be picked too.
        "alpha" if tag == "input" => {
            attrs.alpha = ctx.bool_value(tag, name, value);
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
        // Locale formatting. The value is a spec (`number`,
        // `currency:EUR`, `date`, `time`, `datetime`, `relative`) that
        // the runtime resolves against the app's locale. Which specs
        // exist is `lumen-i18n`'s to say and the compiler does not link
        // it on every path, so an unknown spec is not a parse error: it
        // leaves the text as written, the way an unparseable value does.
        // Empty is an error, the same as an empty catalogue key.
        "format" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(bad(
                    tag,
                    name,
                    value,
                    "format attribute requires a spec (e.g. \"number\" or \"currency:EUR\")"
                        .to_string(),
                ));
            }
            attrs.format = Some(trimmed.to_string());
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
            recognised = false;
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
    Ok(recognised)
}

#[cfg(test)]
mod include_tests {
    use super::*;
    use crate::parse::resolve::{FileLoader, normalize_path};
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

#[cfg(test)]
mod widget_tag_tests {
    use super::*;

    fn bag(el: &Element) -> Vec<(&str, &str)> {
        el.attrs
            .widget_attributes
            .as_ref()
            .expect("a custom tag keeps its attributes")
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    #[test]
    fn a_custom_tag_keeps_every_attribute_it_was_written_with() {
        lumen_widget::register_widget_tag("parse-bag-gauge");
        let ir = parse_html(concat!(
            "<root>",
            "<parse-bag-gauge id=\"g\" label=\"fuel\" width=\"40\"/>",
            "<parse-bag-gauge/>",
            "<label text=\"hi\"/>",
            "</root>",
        ))
        .expect("parse");
        let kids = &ir.root.children;
        // Attributes the vocabulary knows still take effect, and are kept too.
        assert_eq!(
            bag(&kids[0]),
            [("id", "g"), ("label", "fuel"), ("width", "40")]
        );
        assert_eq!(kids[0].attrs.id.as_deref(), Some("g"));
        assert!(bag(&kids[1]).is_empty());
        assert!(kids[2].attrs.widget_attributes.is_none());
        assert!(ir.root.attrs.widget_attributes.is_none());
    }

    #[test]
    fn a_custom_tags_own_binding_is_its_attribute() {
        lumen_widget::register_widget_tag("parse-bag-pane");
        let ir = parse_html(
            "<root><parse-bag-pane bind-window-title=\"doc.name\" bind-text=\"t\"/></root>",
        )
        .expect("a binding the tag owns parses");
        let pane = &ir.root.children[0];
        assert_eq!(
            bag(pane),
            [("bind-window-title", "doc.name"), ("bind-text", "t")]
        );
        assert_eq!(
            pane.attrs.bind.as_ref().map(|b| b.name.as_str()),
            Some("t"),
            "a binding the vocabulary knows still binds"
        );
        assert!(
            parse_html("<root><label bind-window-title=\"x\"/></root>").is_err(),
            "a built-in tag has no such binding"
        );
    }

    #[test]
    fn a_custom_tag_in_a_fragment_body_is_handed_the_use_sites_argument() {
        lumen_widget::register_widget_tag("parse-bag-meter");
        let ir = parse_html(concat!(
            "<root>",
            "<template name=\"Meter\"><parse-bag-meter label=\"{name}\"/></template>",
            "<Meter name=\"oil\"/>",
            "</root>",
        ))
        .expect("parse");
        assert_eq!(bag(&ir.root.children[0]), [("label", "oil")]);
    }
}

#[cfg(test)]
mod depth_tests {
    use super::*;

    /// Half the stack Windows gives a program's main thread, which is where
    /// `lumenc run` parses the app before it opens the window. The other half
    /// is for the frames above the parser.
    const PARSER_STACK_BUDGET: usize = 512 * 1024;

    /// A document nested `levels` deep below `<root>`, cycling through the
    /// tags with builders of their own (a `<tooltip>` trigger, and a `<tabs>`
    /// with its `<tab>`, which synthesizes a strip) as well as a plain
    /// `<column>`, with a `<checkbox>` at the bottom so the deepest frame also
    /// synthesizes widget parts.
    fn nested(levels: u32) -> String {
        let mut open = String::new();
        let mut close = String::new();
        let mut level = 1;
        while level < levels {
            match level % 3 {
                // A `<tabs>` and its `<tab>` are two levels of the tree.
                0 if level + 1 < levels => {
                    open.push_str(&format!(r#"<tabs bind-value="tab{level}"><tab name="a">"#));
                    close.insert_str(0, "</tab></tabs>");
                    level += 2;
                    continue;
                }
                1 => {
                    open.push_str(r#"<tooltip text="tip">"#);
                    close.insert_str(0, "</tooltip>");
                }
                _ => {
                    open.push_str("<column>");
                    close.insert_str(0, "</column>");
                }
            }
            level += 1;
        }
        format!(r#"<root>{open}<checkbox text="leaf"/>{close}</root>"#)
    }

    /// Parse on a thread with `stack` bytes, so a frame that grows past what
    /// the cap was sized for fails here as a stack overflow rather than in a
    /// user's debug build on Windows.
    fn parse_on_stack(markup: String, stack: usize) -> Result<LayoutIR, ParseError> {
        std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || parse_html(&markup))
            .expect("spawn parser thread")
            .join()
            .expect("parser thread")
    }

    #[test]
    fn the_deepest_document_the_cap_allows_parses_within_the_stack_budget() {
        let ir = parse_on_stack(nested(MAX_ELEMENT_DEPTH), PARSER_STACK_BUDGET)
            .expect("a document at the depth cap parses");
        assert_eq!(ir.root.tag, "root");
    }

    #[test]
    fn one_level_past_the_cap_is_a_parse_error() {
        let err = parse_on_stack(nested(MAX_ELEMENT_DEPTH + 1), PARSER_STACK_BUDGET)
            .expect_err("a document past the depth cap is rejected");
        assert!(err.to_string().contains("too deeply nested"), "{err}");
    }
}
