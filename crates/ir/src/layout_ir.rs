//! `LayoutIR` - Lumen markup as a tree of styled elements, decoupled from
//! ECS spawning so multiple backends can consume it (runtime spawn, AOT
//! codegen, typed Rust struct emission, FFI bytecode).

use std::path::{Path, PathBuf};

use thiserror::Error;

// `From` impls below convert each IR spec type into its lumen_core
// runtime counterpart. They live here (not in lumen-core) because the
// orphan rule requires the IR types to be in the impl-defining crate;
// keeping them here also means `.into()` Just Works in spawn.rs without
// per-call match expressions. Each conversion is total - every spec
// variant maps to exactly one core variant.

/// Markup parse errors.
#[derive(Debug, Error)]
pub enum ParseError {
    /// Malformed XML/HTML at the lexer level.
    #[error("xml parse error: {0}")]
    Xml(String),
    /// Unknown tag name (not in the supported set).
    #[error(
        "unknown tag '{0}' at position {1}; a tag a module brings needs \
         `tags = [\"{0}\"]` on that module's [dependencies] entry"
    )]
    UnknownTag(String, usize),
    /// Attribute value didn't match its expected form.
    #[error("bad attribute '{name}=\"{value}\"' on <{tag}>: {reason}")]
    BadAttribute {
        /// Attribute name.
        name: String,
        /// Raw value seen.
        value: String,
        /// Owning tag.
        tag: String,
        /// Why the parser rejected it.
        reason: String,
    },
    /// A `<include>` or `@import` directive could not be resolved: a
    /// missing file, a malformed directive, or a cycle. The message
    /// carries the include-site position and, for cycles, the full chain.
    #[error("{0}")]
    Include(String),
}

/// serde adapter for [`Attributes::dir`]. `lumen-core` carries no serde
/// dependency, so `LayoutDirection` cannot derive `Serialize`/`Deserialize`
/// itself; this module maps `Option<LayoutDirection>` to a stable, compact
/// `Option<u8>` (`0 = Auto`, `1 = Ltr`, `2 = Rtl`) for the AOT artifact.
/// Keeping the mapping here (rather than adding serde to lumen-core) keeps
/// the parser-independent runtime free of an extra core dependency.
pub mod layout_direction_serde {
    use lumen_core::components::LayoutDirection;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serialize `Option<LayoutDirection>` as `Option<u8>`.
    pub fn serialize<S: Serializer>(v: &Option<LayoutDirection>, s: S) -> Result<S::Ok, S::Error> {
        let tag: Option<u8> = v.map(|d| match d {
            LayoutDirection::Auto => 0,
            LayoutDirection::Ltr => 1,
            LayoutDirection::Rtl => 2,
        });
        tag.serialize(s)
    }

    /// Deserialize `Option<u8>` back into `Option<LayoutDirection>`.
    /// Unknown tags degrade to [`LayoutDirection::Auto`], mirroring the
    /// `From<&str>` parser fallback.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<LayoutDirection>, D::Error> {
        let tag = Option::<u8>::deserialize(d)?;
        Ok(tag.map(|t| match t {
            1 => LayoutDirection::Ltr,
            2 => LayoutDirection::Rtl,
            _ => LayoutDirection::Auto,
        }))
    }
}

/// The `<script>` elements one markup source carries, read without building
/// its tree.
///
/// The blocks a candela script writes become fragments, and markup names one
/// of those by writing the function as a tag, so the scripts have to be known
/// before the tree is built. Reading them is a walk of the `<script>`
/// elements, which is all this carries; the tree build collects the same set
/// into [`LayoutIR::script_source`] and [`LayoutIR::external_scripts`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptRefs {
    /// Body text of every inline block, in source order, newline separated.
    pub inline: String,
    /// `src` of every external block, in source order.
    pub external: Vec<String>,
}

/// Top of the IR. Single root element; multi-rooted markup is illegal.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LayoutIR {
    /// The single root element.
    pub root: Element,
    /// Concatenated body text of every inline `<script>` tag found in
    /// the markup, in source order, separated by `\n`. Empty if none.
    ///
    /// Use `<script src="foo.rhai"/>` instead when the script contains
    /// characters that confuse the XML parser (`<`, `<=`, `&`) - those
    /// references land in [`external_scripts`].
    pub script_source: String,
    /// Paths from every `<script src="..."/>` reference, in source
    /// order. The runtime (`lumenc::run::load_ir`) resolves them
    /// relative to the app directory and concatenates with
    /// `script_source` before handing the combined source to the
    /// scripting host.
    pub external_scripts: Vec<String>,
    /// Opt-in skin requested by the root element via `<root skin="...">`.
    /// `None` means the bare framework - no default styling, fully
    /// honouring the SDD "no opinionated default theme" principle. When
    /// `Some("default")` (the only recognised value today), the runtime
    /// prepends an embedded `lumen-skin-default` stylesheet before
    /// parsing the user's `main.css`.
    pub skin: Option<String>,
    /// `<root frameless="true">` - suppress OS window chrome (title
    /// bar, borders, close/min/max buttons). The window backend reads
    /// this once at startup to set `WindowAttributes::decorations(false)`.
    /// Custom title bars are the app's responsibility.
    pub frameless: bool,
    /// Native menu bar, collected from the top-level `<menubar>`
    /// element (if any) and stripped from the layout tree. `None` =
    /// no menu bar. Each entry maps to a `muda::Submenu` /
    /// `MenuItem` / `PredefinedMenuItem::separator`.
    pub menubar: Option<MenuBarSpec>,
    /// Concatenated skin + user CSS, kept around so the for-block
    /// reconciler can re-apply matching rules to virtualized template
    /// instances *after* `{kind}`/`{level}` etc. placeholders have
    /// been substituted into class names at runtime. Without this,
    /// `<label class="md-h{level}">` never picks up `.md-h1` because
    /// the parse-time apply pass only saw the literal placeholder.
    pub combined_stylesheet: Option<crate::css::Stylesheet>,
    /// Parse-time lint findings. Populated by the markup walker for
    /// problems that don't invalidate the IR: `{name}` bare
    /// interpolation, a boolean attribute with an off-list value, an
    /// attribute nothing reads. The runtime / IR consumers ignore this;
    /// every compile path (`lumenc check`, `run`, `build`) prints it to
    /// stderr at the finding's own severity, and `lumenc lint --signals`
    /// folds it into the finding stream.
    pub lint_findings: Vec<LintFinding>,
    /// Normalized paths of every `.lmn` file pulled in via
    /// `<include src="..."/>` (transitively, in resolution order). Empty
    /// when the markup used no includes or was parsed without a file
    /// loader. The runtime (`lumenc::run`) adds these to the hot-reload
    /// watch set so editing an included file re-triggers a reload, exactly
    /// like [`external_scripts`].
    pub included_files: Vec<PathBuf>,
}

/// Severity tier for a parse-time [`LintFinding`].
///
/// Mirrors the lint-CLI severity ladder. Kept here (rather than
/// importing the CLI's enum) so the parser can populate findings
/// without pulling the lint module into the IR's dependency graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LintSeverity {
    /// Definitive bug - would fail a build in strict mode.
    Error,
    /// Probably wrong - flagged in `--strict`, advisory otherwise.
    Warn,
    /// Stylistic / migration nudge.
    Info,
    /// Lowest priority - likely dead code.
    Hint,
}

/// Category bucket for a parse-time [`LintFinding`]. New variants
/// land here as the parser learns more rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LintKind {
    /// `{name}` interpolation without the explicit `$` prefix; the
    /// preferred form is `{$name}` (global) or `{$self.field}` /
    /// `{$parent.field}` (per-entity).
    BareInterpolation,
    /// A boolean attribute carrying a value outside the shared
    /// truthiness set (`true` / `yes` / `1` / bare for true,
    /// `false` / `no` / `0` for false). The attribute reads as false.
    BooleanAttribute,
    /// An attribute the markup vocabulary has no meaning for. It is
    /// dropped: nothing reads it at spawn time, so a typo (`tect=` for
    /// `text=`) or a web-only attribute silently does nothing.
    UnknownAttribute,
}

impl From<LintKind> for &'static str {
    fn from(k: LintKind) -> &'static str {
        match k {
            LintKind::BareInterpolation => "bare-interpolation",
            LintKind::BooleanAttribute => "boolean-attribute",
            LintKind::UnknownAttribute => "unknown-attribute",
        }
    }
}

impl From<LintSeverity> for &'static str {
    fn from(s: LintSeverity) -> &'static str {
        match s {
            LintSeverity::Error => "error",
            LintSeverity::Warn => "warn",
            LintSeverity::Info => "info",
            LintSeverity::Hint => "hint",
        }
    }
}

/// One parse-time finding. Carries a 1-based (line, col) anchor in
/// the source so downstream tools can surface a diagnostic at the
/// exact byte where the legacy shape lives.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LintFinding {
    /// Category bucket.
    pub kind: LintKind,
    /// Severity tier.
    pub severity: LintSeverity,
    /// Human-readable description.
    pub message: String,
    /// 1-based line number in the source.
    pub line: usize,
    /// 1-based column.
    pub col: usize,
    /// Suggested replacement text (`Some("{$count}")` for the bare
    /// interpolation rule). `None` when no machine-applicable fix is
    /// available.
    pub suggest: Option<String>,
}

impl LintFinding {
    /// Render the finding as the stderr diagnostic the compile paths
    /// print: one severity-prefixed line anchored at `file:line:col`,
    /// plus a `hint:` line when the rule carries a machine-applicable
    /// fix. Every path that compiles markup from source calls this, so
    /// `check`, `run` and `build` cannot drift apart on the wording.
    pub fn render(&self, file: &Path) -> String {
        let mut out = format!(
            "{sev:<5} {file}:{line}:{col}  [{kind}] {msg}",
            sev = <&'static str>::from(self.severity),
            file = file.display(),
            line = self.line,
            col = self.col,
            kind = <&'static str>::from(self.kind),
            msg = self.message,
        );
        if let Some(s) = &self.suggest {
            out.push_str(&format!("\n      hint: replace with `{s}`"));
        }
        out
    }
}

/// Parsed `<menubar>` content. Top-level submenus + their items.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MenuBarSpec {
    /// Top-level submenus, in source order.
    pub menus: Vec<MenuSpec>,
}

/// One `<menu label="File">...</menu>` submenu.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MenuSpec {
    /// Display label shown in the menu bar.
    pub label: String,
    /// Items inside the submenu, in source order.
    pub items: Vec<MenuEntrySpec>,
}

/// One entry inside a submenu - either a clickable item or a
/// separator line.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MenuEntrySpec {
    /// `<menuitem id="open" label="Open" accel="Cmd+O" />`.
    Item {
        /// `id="..."` - passed back to scripts as `on_menu(id)`.
        id: String,
        /// Display label.
        label: String,
        /// Optional accelerator string in muda format
        /// (`"CommandOrControl+S"`, `"Alt+F4"`, ...).
        accelerator: Option<String>,
    },
    /// `<separator />`.
    Separator,
}

/// One node in the IR tree.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Element {
    /// Tag name (`root`, `column`, `tile`, `label`, ...).
    pub tag: String,
    /// Resolved attribute bag in normalized form.
    pub attrs: Attributes,
    /// Children, in source order.
    pub children: Vec<Element>,
    /// Round-8 wave-C: catalog of `{...}` placeholder slots that appear in
    /// this element's string-valued attributes (text, id, classes, ...).
    /// Populated by the parser so the spawner / for-block reconciler can
    /// look up which scope to resolve each placeholder from instead of
    /// falling back to substring replace against the global signal map.
    ///
    /// Order matches first-appearance source order across the scanned
    /// attribute set; duplicates inside the same element collapse to one
    /// entry. Empty for elements with no interpolation sites.
    pub interpolations: Vec<InterpolationSlot>,
    /// Set when this element instantiates a fragment rather than rendering
    /// itself. `None` on every ordinary element, which is nearly all of
    /// them; boxed so carrying the possibility costs one pointer on an
    /// element the `<for>` reconciler deep-clones per row.
    pub frag_use: Option<Box<FragmentUse>>,
}

/// A fragment instantiation, recorded on the element that stands in for the
/// fragment's body until the tree is expanded.
///
/// The declaration side is [`crate::fragment::Fragment`], looked up by
/// [`Self::key`] in the app's [`crate::fragment::FragmentTable`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FragmentUse {
    /// Key of the fragment being instantiated.
    pub key: String,
    /// Arguments the use site passes, as `(parameter name, value)` pairs in
    /// source order. A parameter absent here takes its declared default.
    pub args: Vec<(String, String)>,
    /// Whether the use site supplies children of its own for the fragment's
    /// slot. The children themselves stay on [`Element::children`]; this
    /// says they are slot content rather than the element's own subtree.
    pub slot_children: bool,
}

impl Default for Element {
    /// Empty element with no tag, default attrs, no children, no
    /// interpolation slots, no fragment use. Synthetic-element
    /// constructors in the parser (`<tabs>`, `<dropdown>`, ...) use
    /// `..Default::default()` to fill in the trailing fields
    /// automatically.
    fn default() -> Self {
        Self {
            tag: String::new(),
            attrs: Attributes::default(),
            children: Vec::new(),
            interpolations: Vec::new(),
            frag_use: None,
        }
    }
}

/// Rewrite every `<image src>` under `el` that points inside `root` to a
/// path relative to it, recording in `outside` the ones that point
/// elsewhere.
///
/// The inverse of the compiler's asset resolution, which makes every `src`
/// absolute against the app directory on the machine that compiles. That is
/// the right answer for running the app in place and the wrong one for an
/// artifact that travels, so anything shipping an IR elsewhere (a packaged
/// folder, a web build) puts the paths back first.
///
/// Rewritten paths are joined with forward slashes whatever the compiling
/// machine uses: every platform's loader accepts them, so an artifact built
/// for another platform names its files in a way that platform can follow.
pub fn relativize_asset_paths(el: &mut Element, root: &Path, outside: &mut Vec<String>) {
    if el.tag == "image"
        && let Some(path) = &el.attrs.src
    {
        let p = Path::new(path);
        if p.is_absolute() {
            match p.strip_prefix(root) {
                Ok(rel) => {
                    let parts: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect();
                    el.attrs.src = Some(parts.join("/"));
                }
                Err(_) => outside.push(path.clone()),
            }
        }
    }
    for child in &mut el.children {
        relativize_asset_paths(child, root, outside);
    }
}

/// Round-8 wave-C: classification of a `{...}` placeholder inside markup.
/// Carries the *resolution scope* the spawner consults when materializing
/// the slot into a concrete string.
///
/// - `Global("name")` - `{$name}` or the legacy bare `{name}`. Resolves
///   against [`lumen_core::signals::Signals`] (the global signal map).
/// - `Row("field")` - `{row.field}`, only meaningful inside a `<for>`
///   body. Resolves against the current iteration's
///   [`lumen_core::signals::ArrayItem`] record. Crucially does not fall
///   through to globals when the row record is missing the field - that
///   substitution emits empty string and a one-shot `tracing::warn!`.
/// - `RowIndex` - `{$index}` (preferred) or the legacy `{idx}` alias.
///   Resolves to the 0-based iteration index, stringified.
/// - `SelfField("f")` / `ParentField("f")` - `{$self.f}` / `{$parent.f}`.
///   Stub today; the per-entity consumer system lands in a follow-up
///   wave. Substitutes empty string and emits a debug trace.
/// - `Arg("name")` - a fragment parameter, resolved from the arguments the
///   use site passed. Only a fragment body carries these: the classifier
///   below cannot produce one, because telling a parameter from a global
///   needs the enclosing fragment's parameter list, which it cannot see.
///   The fragment builder rewrites `Global` to `Arg` for each declared
///   parameter once that scope is known.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InterpolationSlot {
    /// `{$name}` or `{name}` legacy shorthand - global signal lookup.
    Global(String),
    /// `{row.field}` - iteration-scope field lookup. `<for>` body only.
    Row(String),
    /// `{$index}` or `{idx}` legacy alias - 0-based iteration index.
    RowIndex,
    /// `{$self.field}` - per-entity field lookup. Stubbed today.
    SelfField(String),
    /// `{$parent.field}` - parent-entity field lookup. Stubbed today.
    ParentField(String),
    /// A fragment parameter, resolved from the instantiating
    /// [`FragmentUse::args`] or the parameter's declared default.
    Arg(String),
}

impl From<&str> for InterpolationSlot {
    /// Classify a placeholder's *trimmed inner body* (without the
    /// surrounding `{...}`). The parser's interpolation walker uses this
    /// to convert each placeholder it finds into a typed slot:
    ///
    /// - `"$index"` / `"idx"` -> [`InterpolationSlot::RowIndex`].
    /// - `"$self.<f>"` -> [`InterpolationSlot::SelfField`].
    /// - `"$parent.<f>"` -> [`InterpolationSlot::ParentField`].
    /// - `"row.<f>"` -> [`InterpolationSlot::Row`].
    /// - `"$name"` / `"name"` -> [`InterpolationSlot::Global`].
    ///
    /// [`InterpolationSlot::Arg`] is never produced here: a parameter
    /// reference is spelled the same as a global, and only the enclosing
    /// fragment's parameter list separates them.
    fn from(inner: &str) -> Self {
        let trimmed = inner.trim();
        // `idx` is the legacy row-index alias; only meaningful inside
        // a `<for>` body, but the From impl can't see that context so
        // we always route it to [`RowIndex`]. The parser still emits
        // a `BareInterpolation` finding nudging the author to write
        // `{$index}`.
        if trimmed == "$index" || trimmed == "idx" {
            return InterpolationSlot::RowIndex;
        }
        if let Some(rest) = trimmed.strip_prefix("$self.") {
            return InterpolationSlot::SelfField(rest.to_string());
        }
        if let Some(rest) = trimmed.strip_prefix("$parent.") {
            return InterpolationSlot::ParentField(rest.to_string());
        }
        if let Some(rest) = trimmed.strip_prefix("row.") {
            return InterpolationSlot::Row(rest.to_string());
        }
        if let Some(rest) = trimmed.strip_prefix('$') {
            return InterpolationSlot::Global(rest.to_string());
        }
        InterpolationSlot::Global(trimmed.to_string())
    }
}

/// Length specifier as it appears in markup.
#[derive(Debug, Default, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LengthSpec {
    /// `auto`.
    #[default]
    Auto,
    /// Pixels.
    Px(f32),
    /// Percentage of parent.
    Percent(f32),
}

/// CSS `line-height` value. Unlike [`LengthSpec`], the two forms carry
/// different meanings rather than different units of the same quantity:
/// a bare number (`line-height: 1.2`) scales with the element's own
/// font size, while a `px` value (`line-height: 19px`) is a fixed line
/// box height that does not track font-size changes. Kept as two
/// variants instead of collapsing to a single px number so a consumer
/// can tell which behavior the author asked for.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LineHeightSpec {
    /// Unitless multiplier of the element's font size (CSS `normal`-like
    /// scaling behavior).
    Multiplier(f32),
    /// Fixed line height in px, independent of font size.
    Px(f32),
}

/// Four edge values (in CSS order: left, right, top, bottom).
///
/// W5.5 adds CSS Logical Properties Level 1 fields (`inline_start`,
/// `inline_end`, `block_start`, `block_end`). When set, they ride along
/// to the lumen-core `Edges` whose `resolved(dir)` method maps them onto
/// the physical sides at layout time per [`ResolvedDirection`].
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Edges {
    /// Left edge in px.
    pub left: f32,
    /// Right edge in px.
    pub right: f32,
    /// Top edge in px.
    pub top: f32,
    /// Bottom edge in px.
    pub bottom: f32,
    /// `*-inline-start` in px - maps to `left` (LTR) / `right` (RTL).
    pub inline_start: Option<f32>,
    /// `*-inline-end` in px - maps to `right` (LTR) / `left` (RTL).
    pub inline_end: Option<f32>,
    /// `*-block-start` in px - alias for `top` (no vertical writing modes yet).
    pub block_start: Option<f32>,
    /// `*-block-end` in px - alias for `bottom`.
    pub block_end: Option<f32>,
    /// CSS percent unit for the left edge (`padding: 5%`). When `Some`
    /// the px field for the side is ignored; the layout backend hands
    /// taffy a `LengthPercentage::percent` which resolves against the
    /// containing block per CSS.
    pub pct_left: Option<f32>,
    /// See [`Self::pct_left`].
    pub pct_right: Option<f32>,
    /// See [`Self::pct_left`].
    pub pct_top: Option<f32>,
    /// See [`Self::pct_left`].
    pub pct_bottom: Option<f32>,
}

impl Edges {
    /// Uniform edges.
    pub const fn all(v: f32) -> Self {
        Self {
            left: v,
            right: v,
            top: v,
            bottom: v,
            inline_start: None,
            inline_end: None,
            block_start: None,
            block_end: None,
            pct_left: None,
            pct_right: None,
            pct_top: None,
            pct_bottom: None,
        }
    }
}

/// Parsed `<tooltip text="..." delay="...">` payload, propagated from
/// the wrapper to its single child as
/// [`Attributes::tooltip`](crate::layout_ir::Attributes::tooltip).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TooltipSpec {
    /// Body text shown in the popup.
    pub text: String,
    /// Hover dwell before the popup appears, in milliseconds. `None` =
    /// author didn't set `delay="..."` inline; the cascade fills it from
    /// the `--lumen-tooltip-delay` skin token when declared, and the
    /// spawn layer falls back to the runtime default (500) last.
    pub delay_ms: Option<u32>,
    /// Gap between the cursor hotspot and the popup's top-left corner,
    /// in logical pixels. Same resolution chain as [`Self::delay_ms`]:
    /// inline attr -> `--lumen-tooltip-offset` token -> runtime default.
    pub offset: Option<f32>,
}

/// Synthesised `<dropdown>` header payload: the open signal plus the
/// full option list mirrored from the `<option>` children (markup
/// order), so the runtime header component can run keyboard
/// interaction while the popup body is unmounted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DropdownButtonSpec {
    /// Open-panel signal (`__dropdown_open:<bind-value>`).
    pub open_signal: String,
    /// Author-bound value signal (`bind-value=`).
    pub value_signal: String,
    /// `(value, label, disabled)` per `<option>`, in markup order.
    pub options: Vec<(String, String, bool)>,
}

/// Parsed `bg=` value. Solid colors and gradients share one attribute
/// surface so authors don't pick between `bg=` and `bg-gradient=`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum BgSpec {
    /// Solid color.
    Solid(Rgba),
    /// CSS-style linear gradient.
    Linear {
        /// Angle in degrees (CSS convention: 0deg = bottom->top, 90 = left->right).
        angle_deg: f32,
        /// `(offset, color)` pairs in source order, after parse-time
        /// normalization (sorted by offset ascending).
        stops: Vec<(f32, Rgba)>,
    },
    /// CSS-style radial gradient centred on the rect.
    Radial {
        /// Normalized radius `0..=1` of half-min-dim.
        radius: f32,
        /// `(offset, color)` pairs ascending offset.
        stops: Vec<(f32, Rgba)>,
    },
    /// CSS-style conic (sweep) gradient.
    Conic {
        /// Starting angle in degrees (CSS: 0 = north, 90 = east).
        from_deg: f32,
        /// `(offset, color)` pairs ascending offset.
        stops: Vec<(f32, Rgba)>,
    },
}

/// RGBA color in `[0,1]^4`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rgba {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha.
    pub a: f32,
}

/// Scroll axis declared in markup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ScrollAxisSpec {
    /// Vertical scroll only.
    Y,
    /// Horizontal scroll only.
    X,
    /// Both axes.
    Both,
}

/// Resolved attribute bag; one struct so the parser fills it in a single
/// pass and the spawner / codegen don't have to re-parse strings.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Attributes {
    /// `width` attribute.
    pub width: Option<LengthSpec>,
    /// `height` attribute.
    pub height: Option<LengthSpec>,
    /// `flex` attribute (`row` / `column`).
    pub flex: Option<FlexAxis>,
    /// `bg` attribute (background color).
    pub bg: Option<BgSpec>,
    /// `radius` attribute in px (uniform).
    pub radius: Option<f32>,
    /// Per-corner radii `[top-left, top-right, bottom-right,
    /// bottom-left]` in px - the CSS `border-radius` 2-4 value
    /// shorthand and the `border-top-left-radius` ... longhands. When
    /// set, wins over the uniform [`Self::radius`] for paint; the
    /// uniform slot is kept in sync (max corner) for the runtime paths
    /// that only understand one number (knob geometry, hit rings).
    pub radius_corners: Option<[f32; 4]>,
    /// `padding` attribute.
    pub padding: Option<Edges>,
    /// `margin` attribute.
    pub margin: Option<Edges>,
    /// `text` content (also accepts text child node).
    pub text: Option<String>,
    /// `text-color`.
    pub text_color: Option<Rgba>,
    /// `selection-color` - text selection highlight. Skins default it
    /// via the `--lumen-selection` token; unset falls back to the
    /// renderer default (text fill at 32 % alpha).
    pub selection_color: Option<Rgba>,
    /// `caret-color` - text-input caret tint (CSS). Lands on
    /// [`lumen_core::components::TextInputPaint::caret_color`]; unset
    /// falls back to the text fill (web default). Only meaningful on
    /// `<input>` / `<textarea>`.
    pub caret_color: Option<Rgba>,
    /// `selection-text-color` - selected-glyph foreground (Qt
    /// `QPalette::HighlightedText` / Slint `selection-foreground-color`).
    /// Lands on
    /// [`lumen_core::components::TextInputPaint::selection_foreground`];
    /// unset => selected glyphs keep their normal fill. Only meaningful on
    /// `<input>` / `<textarea>`.
    pub selection_text_color: Option<Rgba>,
    /// `scroll` axis.
    pub scroll: Option<ScrollAxisSpec>,
    /// `sensitivity` (scroll containers only).
    pub sensitivity: Option<f32>,
    /// `inertia` (scroll containers only).
    pub inertia: Option<f32>,
    /// `tab-index`.
    pub tab_index: Option<i32>,
    /// `id="..."` - emits a `LumenId` marker.
    pub id: Option<String>,
    /// `class="a b"` - emits a `LumenClasses` marker (split on whitespace).
    pub classes: Vec<String>,
    /// `hover-bg` - when set, the entity gains a `HoverTint` component
    /// whose value the primitives crate swaps in on hover.
    pub hover_bg: Option<Rgba>,
    /// `draggable="true"` - entity gains a `Draggable` marker so drag
    /// moves translate its Transform automatically.
    pub draggable: bool,
    /// `drag-payload="..."` - makes the entity an in-app DnD
    /// [`DragSource`](lumen_os_dnd::DragSource) publishing this text
    /// payload (interpolated per-row inside a `<for>`). An empty value
    /// derives the payload from the element's `id`. Mirrors HTML5
    /// `dataTransfer.setData` / Qt `QMimeData`.
    pub drag_payload: Option<String>,
    /// `accept="text/plain"` on a drop target - MIME filter
    /// ([`DropAccept`](lumen_os_dnd::DropAccept)). Absent = accept any.
    pub drop_accept: Option<String>,
    /// `placeholder="..."` on `<input>` - shown when [`text`] is empty.
    pub placeholder: Option<String>,
    /// `drop="true"` - entity gains a `DropTarget` marker so OS file
    /// drags landing on its bounds fire `on_file_dropped(id, path)`.
    pub drop_target: bool,
    /// `drag="true"` on `<title-bar>` - entity gains a
    /// `TitleBarDraggable` marker. Pressing the bar requests a native
    /// window drag via the platform backend. Only meaningful in
    /// conjunction with `<root frameless="true">`.
    pub title_bar_drag: bool,
    /// `layout-boundary="true"` - entity gains a `RelayoutBoundary`
    /// marker so layout-dirty propagation stops here. Auto-set for
    /// `<scroll>` containers and entities with both fixed width and
    /// height; the explicit attribute lets authors override.
    pub layout_boundary: bool,
    /// Tooltip spec carried by the single child of a `<tooltip
    /// text="..." delay="...">` element after the parser flattens the
    /// wrapper away. `None` = no tooltip. Spawns a `TooltipSource`
    /// component at spawn time.
    pub tooltip: Option<TooltipSpec>,
    /// Synthetic tab-strip-button attachment generated by the
    /// `<tabs>` parser pass: `(signal_name, tab_value)`. When set,
    /// spawn attaches a `TabStripButton` component so clicking the
    /// button writes `signal_name = tab_value`.
    pub tab_strip: Option<(String, String)>,
    /// Synthetic "seed this signal on first spawn if absent"
    /// directive. `(signal_name, default_value)`. The `<tabs>` pass
    /// authors it for the default-active tab, and the `<dropdown>`
    /// pass for the open-panel flag plus (on the generated header
    /// button) the first option's value.
    pub signal_seed: Option<(String, String)>,
    /// `src="path/to/file.png"` on `<image>` - kicks off an async
    /// decode via `lumen-assets`. The entity gets a `LoadedImage`
    /// once the bytes are ready.
    pub src: Option<String>,
    /// `alt="a cat asleep on a keyboard"` on `<image>` - what the image
    /// shows, for a reader who is not looking at it. Screen readers announce
    /// it and the web target writes it out as the `alt` attribute. An
    /// `alt=""` an author wrote deliberately is kept as an empty string,
    /// which is how a decorative image is marked.
    pub alt: Option<String>,
    /// `href="settings"` on `<a>` - the target page path for file-based
    /// navigation. The spawner attaches an `Anchor` component so a click
    /// on the element navigates the active page. Resolved by longest
    /// existing `.lmn` prefix at navigation time, never here.
    pub href: Option<String>,
    /// `font-size="N"` - pixel size for rendered text. Inherited
    /// implicitly via the `TextStyle` component; absent = 16px.
    pub font_size: Option<f32>,
    /// CSS `font-family` - the raw family list as authored (commas
    /// preserved, quotes stripped per-name at spawn). Inherited like
    /// `font-size`. The shaper resolves the first available family in
    /// the list against the system font database, honouring the CSS
    /// generic keywords (`sans-serif`, `serif`, `monospace`, ...); an
    /// unresolvable list falls back to the platform sans-serif.
    pub font_family: Option<String>,
    /// CSS `font-weight` - `100..=1000`, `normal` (400), `bold` (700).
    /// Inherited. `None` = 400.
    pub font_weight: Option<u16>,
    /// `style="display-lg"` - Material 3-style typography role. The
    /// parser resolves the role to a concrete pixel size + sets
    /// [`font_size`] if it isn't already explicit.
    pub style_role: Option<String>,
    /// `gap="12"` - spacing inserted between every adjacent pair of
    /// children inside a flex container. Mirrors CSS `gap` (px).
    pub gap: Option<f32>,
    /// `row-gap="12"` - per-axis row gap (W5.9).
    pub gap_row: Option<f32>,
    /// `column-gap="12"` - per-axis column gap (W5.9).
    pub gap_column: Option<f32>,
    /// CSS `gap: 5%` - percent gap on both axes. Wins over [`Self::gap`]
    /// for the axis; resolves against the container's content box.
    pub gap_pct: Option<f32>,
    /// CSS `row-gap: 5%`.
    pub gap_row_pct: Option<f32>,
    /// CSS `column-gap: 5%`.
    pub gap_column_pct: Option<f32>,
    /// `display="grid|flex|none"` (W5.9). When `None` the spawner
    /// defaults to `Display::Flex`.
    pub display: Option<DisplaySpec>,
    /// Parsed `grid-template-rows` + `grid-template-columns` (W5.9).
    pub grid_template: Option<GridTemplateSpec>,
    /// `grid-row="<start> <end>"` (W5.9). `(0, 0)` = auto-place.
    pub grid_row: Option<(i16, i16)>,
    /// `grid-column="<start> <end>"` (W5.9).
    pub grid_column: Option<(i16, i16)>,
    /// `align-self="..."` (W5.9 - per-item override of container align).
    pub align_self: Option<FlexAlign>,
    /// `justify-items="..."` (W5.9 - grid-only inline-axis alignment).
    pub justify_items: Option<FlexAlign>,
    /// `justify-self="..."` (W5.9 - per-item override of justify-items).
    pub justify_self: Option<FlexAlign>,
    /// `grow="1"` - flex-grow factor. Mirrors CSS `flex-grow`. Default
    /// 0 (no grow). `<spacer />` sets this to 1 implicitly so it
    /// pushes neighbouring elements to opposite edges.
    pub grow: Option<f32>,
    /// `align="center|start|end|stretch"` - cross-axis alignment of children
    /// (CSS `align-items`).
    pub align: Option<FlexAlign>,
    /// `justify="center|start|end|between|around|evenly"` - main-axis
    /// distribution of children (CSS `justify-content`).
    pub justify: Option<FlexJustify>,
    /// `text-align="start|center|end"` - horizontal alignment of rendered
    /// text inside the element's content rectangle.
    pub text_align: Option<TextAlignSpec>,
    /// `press-bg="#color"` - color shown while the entity is pressed.
    pub press_bg: Option<Rgba>,
    /// `:hover { text-color }` - text color swapped in while hovered.
    pub hover_text_color: Option<Rgba>,
    /// `:active { text-color }` - text color while pressed.
    pub active_text_color: Option<Rgba>,
    /// `:focus { text-color }` - text color while focused.
    pub focus_text_color: Option<Rgba>,
    /// `:disabled { text-color }` - text color while disabled (applied
    /// at spawn alongside the `Disabled` marker).
    pub disabled_text_color: Option<Rgba>,
    /// `:hover { opacity }`.
    pub hover_opacity: Option<f32>,
    /// `:active { opacity }`.
    pub active_opacity: Option<f32>,
    /// `:focus { opacity }`.
    pub focus_opacity: Option<f32>,
    /// `:disabled { opacity }` - e.g. adwaita's documented 50 %-opacity
    /// disabled controls.
    pub disabled_opacity: Option<f32>,
    /// `:hover { box-shadow }` - shadow stack swapped in while hovered.
    pub hover_shadows: Option<Vec<ShadowSpec>>,
    /// `:active { box-shadow }`.
    pub active_shadows: Option<Vec<ShadowSpec>>,
    /// `:focus { box-shadow }` - e.g. the WinUI TextBox focus underline.
    pub focus_shadows: Option<Vec<ShadowSpec>>,
    /// `:disabled { box-shadow }` (applied at spawn).
    pub disabled_shadows: Option<Vec<ShadowSpec>>,
    /// `:focus-visible { text-color }` - keyboard-only focus text swap.
    pub focus_visible_text_color: Option<Rgba>,
    /// `:focus-visible { opacity }`.
    pub focus_visible_opacity: Option<f32>,
    /// `:focus-visible { box-shadow }` - e.g. the inner ring of the
    /// Windows keyboard-only double focus ring.
    pub focus_visible_shadows: Option<Vec<ShadowSpec>>,
    /// `:drag-over { bg }` - fill swapped in while an acceptable in-app
    /// drag hovers this drop target (HTML5 `dragover` parity). Gated by
    /// the runtime `DropHovered` marker via `StateVisuals::drag_over`.
    pub drag_over_bg: Option<Rgba>,
    /// `:drag-over { text-color }`.
    pub drag_over_text_color: Option<Rgba>,
    /// `:drag-over { opacity }`.
    pub drag_over_opacity: Option<f32>,
    /// `:drag-over { box-shadow }` - e.g. an inset ring lighting up the
    /// hovered drop zone.
    pub drag_over_shadows: Option<Vec<ShadowSpec>>,
    /// `focus-outline="<width> <#color>"` - stroke ring rendered while
    /// the entity has focus.
    pub focus_outline: Option<OutlineSpec>,
    /// `:focus-visible { outline: ... }` - stroke ring rendered only when
    /// focus arrived via the keyboard (Tab / Shift-Tab), mirroring the
    /// CSS `:focus-visible` heuristic. Pointer-driven focus does not
    /// paint it. When both this and [`Self::focus_outline`] are set the
    /// keyboard-only ring wins while the `FocusVisible` marker is
    /// present.
    pub focus_visible_outline: Option<OutlineSpec>,
    /// CSS `outline-offset` in px - gap between the border box edge and
    /// the focus outline. Folded into the outline specs at spawn.
    pub outline_offset: Option<f32>,
    /// `knob-color="#color"` / CSS `knob-color:` - fill of the toggle
    /// knob / slider thumb child. Lumen-native analog property (real
    /// CSS has no reachable pseudo-element for it in our subset);
    /// absent = the runtime `KNOB_FILL` fallback.
    pub knob_color: Option<Rgba>,
    /// CSS `border-width` (and the width part of the `border`
    /// shorthand): per-side widths in px. Participates in layout per the
    /// CSS box model and paints when [`Self::border_style`] is `solid`.
    pub border_width: Option<Edges>,
    /// CSS `border-color` (the uniform base color for all four sides).
    pub border_color: Option<Rgba>,
    /// CSS `border-top-color` - per-side override of [`Self::border_color`].
    pub border_color_top: Option<Rgba>,
    /// CSS `border-right-color`.
    pub border_color_right: Option<Rgba>,
    /// CSS `border-bottom-color` - the Windows elevation bottom edge.
    pub border_color_bottom: Option<Rgba>,
    /// CSS `border-left-color`.
    pub border_color_left: Option<Rgba>,
    /// CSS `border-style` (`none` | `solid`). Per CSS, no style => no
    /// border: the computed border-width is zero and nothing paints.
    /// The `border:` shorthand always sets this explicitly.
    pub border_style: Option<BorderStyleSpec>,
    /// CSS `box-sizing`. `None` = the Lumen UA default (`border-box`).
    pub box_sizing: Option<BoxSizingSpec>,
    /// `:hover { border: ... }` / `hover-border:` - border swapped in
    /// while the pointer hovers the entity.
    pub hover_border: Option<BorderPaintSpec>,
    /// `:focus { border: ... }` / `focus-border:` - border swapped in
    /// while the entity has keyboard focus. Wins over
    /// [`Self::hover_border`] when both states are active.
    pub focus_border: Option<BorderPaintSpec>,
    /// CSS `flex-shrink`. `None` = the CSS initial value `1`.
    pub shrink: Option<f32>,
    /// CSS `flex-basis`. `None` = `auto`.
    pub basis: Option<LengthSpec>,
    /// CSS `flex-wrap`. `None` = `nowrap`.
    pub flex_wrap: Option<FlexWrapSpec>,
    /// CSS `align-content` - cross-axis distribution of wrapped flex
    /// lines / grid tracks.
    pub align_content: Option<AlignContentSpec>,
    /// CSS `z-index` - sibling paint-order override. `None` = `auto`.
    pub z_index: Option<i32>,
    /// `checked="true|false"` on `<toggle>` - initial state.
    pub checked: Option<bool>,
    /// `autofocus="true"` - when the containing `<dialog>` opens, this
    /// element receives initial focus (HTML `autofocus` / Qt
    /// `setFocus` on show).
    pub autofocus: bool,
    /// `<button default="true">` - the dialog's DEFAULT button: Enter
    /// anywhere in the dialog (except on another button / an
    /// Enter-consuming input) activates it, and closing through it
    /// fires the `accepted` (not `rejected`) hook. Also appends the
    /// `default` class so skins can style `button.default`.
    pub default_button: bool,
    /// `<checkbox indeterminate="true">` - tri-state dash until the
    /// first user toggle clears it (web `indeterminate` IDL attr / Qt
    /// `PartiallyChecked`).
    pub indeterminate: bool,
    /// `<radio group="...">` - the PropertyStore global holding the
    /// radio group's selected value.
    pub radio_group: Option<String>,
    /// `<radio value="...">` - this member's (string) value. The shared
    /// `value` attribute parses as f32 for sliders/progress; radios
    /// route it here instead.
    pub radio_value: Option<String>,
    /// `<progress duration="...">` / CSS `progress-duration` - the
    /// indeterminate sweep period in ms (skin token
    /// `--lumen-progress-period`).
    pub progress_duration: Option<u32>,
    /// Synthetic widget-part marker set by the parser desugars so the
    /// spawn layer can attach the matching runtime marker component to
    /// a child element (`.checkbox-box` / `.radio-dot` /
    /// `.progress-fill`).
    pub part: Option<WidgetPart>,
    /// The authored widget this element came out of, for the desugars that
    /// replace a widget tag with plain boxes ([`WidgetRole`]). A surface that
    /// can express the widget directly reads this and writes the widget back;
    /// the runtime spawner ignores it, because the boxes beside it are already
    /// the widget it would build.
    pub widget: Option<WidgetRole>,
    /// `:checked { bg: ... }` CSS - track fill shown while a `<toggle>`
    /// is checked. Absent = the built-in accent fill.
    pub checked_bg: Option<Rgba>,
    /// `:selected { bg: ... }` CSS - fill shown on a `<tabs>` strip button
    /// while it carries the [`lumen_core::components::Selected`] marker.
    /// Absent = the built-in accent fill (same default-skin fallback
    /// pattern as [`Self::checked_bg`]).
    pub selected_bg: Option<Rgba>,
    /// `disabled="true"` - entity spawns with the `Disabled` marker;
    /// input routing skips it and the default render dims it.
    pub disabled: bool,
    /// `:disabled { bg: ... }` CSS - fill shown while the entity is
    /// disabled. Absent = reduced-opacity render of the normal fill.
    pub disabled_bg: Option<Rgba>,
    /// `value="0.5"` on `<slider>` - initial slider value.
    pub value: Option<f32>,
    /// `min="0"` on `<slider>` - lower bound.
    pub min: Option<f32>,
    /// `max="1"` on `<slider>` - upper bound.
    pub max: Option<f32>,
    /// `step="0.1"` on `<slider>` - keyboard / wheel increment.
    /// Absent = `(max - min) / 100`.
    pub step: Option<f32>,
    /// `bind="text:signal_name"` - declarative reactive binding to a named `Signals` entry. Only the `text:` prefix is recognised.
    pub bind: Option<BindSpec>,
    /// `bind-disabled="signal"` - one-way binding driving the `Disabled`
    /// marker from a boolean signal so scripts can enable / disable the
    /// widget live. Stored apart from [`Self::bind`] so it can coexist
    /// with `bind-checked` / `bind-value` / `bind-text` on one element.
    pub bind_disabled: Option<String>,
    /// `bind-scroll="signal"` on a scroll container - two-way binding
    /// between an f32 signal (logical px) and the container's vertical
    /// scroll offset (W6 T6). Stored apart from [`Self::bind`] (same
    /// coexistence rule as `bind_disabled`).
    pub bind_scroll: Option<String>,
    /// `each="rows"` on `<for>` - name of the `ArraySignals` entry to
    /// iterate. The element's inline children are the per-item body
    /// template; `{field}` placeholders inside their attrs / text get
    /// replaced at reconcile time with the matching record field.
    pub each: Option<String>,
    /// `key="id"` on `<for>` - record field used as the stable reconciliation key. Without it the reconciler keys rows by item index.
    pub key: Option<String>,
    /// `<for virtualized="true">` - spawn only rows in the visible scroll window. Requires a `<scroll>` ancestor for the reconciler to compute the visible band.
    pub virtualized: bool,
    /// `<for row-height="32">` - pixel height per virtualized row.
    /// Required when [`Self::virtualized`] is true; defaults to 32 px
    /// at spawn when the attr is absent.
    pub row_height: Option<f32>,
    /// `signal="loaded"` on `<if>` - name of the `Signals` entry whose
    /// truthiness gates the subtree. Truthy = non-empty AND not literal
    /// `"false"` / `"0"`. Toggle spawns / despawns body on next tick.
    pub if_signal: Option<String>,
    /// `eq="value"` on `<if>` - when set, the body mounts iff
    /// `Signals[if_signal] == eq`. Used by `<tabs>` to switch the
    /// active tab body; `None` falls back to truthiness.
    pub if_eq: Option<String>,
    /// `mode="render|hide"` on `<if>` - pick the reconciler policy.
    /// `render` (default) despawns/respawns the body each toggle;
    /// `hide` mounts once and flips a `Visible` flag, preserving focus
    /// / scroll / per-row signals across show-hide cycles.
    pub if_mode: IfModeSpec,
    /// `wrap="none|word|glyph"` - text wrap policy. Default is none
    /// (overflow clips horizontally).
    pub text_wrap: Option<TextWrapSpec>,
    /// `max-lines="N"` - hard cap on rendered line count for wrapped text.
    pub max_lines: Option<u32>,
    /// CSS `text-overflow: ellipsis | clip` (or `wrap="ellipsis"` in
    /// markup). `Ellipsis` elides overflowing single-line text with a
    /// trailing `...`; the spawn layer lowers it onto the runtime
    /// `TextStyle` as glyph-wrap + `max_lines = 1` unless the author
    /// supplied an explicit wrap / max-lines pair (multi-line clamp,
    /// which the shaper already ellipsizes). Not inherited (CSS
    /// `text-overflow` doesn't inherit either).
    pub text_overflow: Option<TextOverflowSpec>,
    /// `position="relative|absolute"` - CSS-style positioning mode.
    pub position: Option<PositionSpec>,
    /// `inset="t r b l"` (or 1/2/3-value shorthand) - offsets from each
    /// edge for `position="absolute"`.
    pub inset: Option<Edges>,
    /// `min-width="..."`.
    pub min_width: Option<LengthSpec>,
    /// `min-height="..."`.
    pub min_height: Option<LengthSpec>,
    /// `max-width="..."`.
    pub max_width: Option<LengthSpec>,
    /// `max-height="..."`.
    pub max_height: Option<LengthSpec>,
    /// `aspect-ratio="1.5"` - width / height constraint.
    pub aspect_ratio: Option<f32>,
    /// `overflow="visible|hidden|scroll"` - sets both axes. Use
    /// `overflow-x` / `overflow-y` to set per axis.
    pub overflow: Option<OverflowSpec>,
    /// `overflow-x`.
    pub overflow_x: Option<OverflowSpec>,
    /// `overflow-y`.
    pub overflow_y: Option<OverflowSpec>,
    /// `fit="fill|cover|contain|none|scale-down"` on `<image>` -
    /// CSS `object-fit`.
    pub image_fit: Option<ImageFitSpec>,
    /// CSS `scrollbar-color: <thumb> [<track>]` (Scrollbars Styling
    /// Level 1) - overlay-bar thumb + optional track fills for `<scroll>`
    /// containers. `None` = the runtime's `ScrollbarStyle` default.
    pub scrollbar_color: Option<(Rgba, Option<Rgba>)>,
    /// CSS `scrollbar-width: auto | thin | none` (Scrollbars Styling
    /// Level 1).
    pub scrollbar_width: Option<ScrollbarWidthSpec>,
    /// `shadow="<x> <y> <blur> <#color>"` - drop shadow.
    pub shadows: Vec<ShadowSpec>,
    /// `opacity="0.5"` - alpha multiplier in `[0, 1]`. Absent = 1.0.
    pub opacity: Option<f32>,
    /// CSS `transition: <property> <duration> [<easing>] [, ...]` declarations. Each entry tweens the property instead of snapping when the value changes.
    /// Animatable in v1: `opacity`, `background-color`/`bg`,
    /// `color`/`text-color`, `border-color`. Layout properties (width,
    /// height, ...) are rejected with a warn - see [`TransitionPropertyIr`].
    pub transitions: Vec<TransitionIr>,
    /// CSS `transition-property` longhand - comma list of property
    /// names. Combined with the duration / timing longhands by
    /// [`Attributes::effective_transitions`].
    pub transition_property: Option<Vec<TransitionPropertyIr>>,
    /// CSS `transition-duration` longhand - comma list of durations in
    /// ms, cycled over the property list per the CSS repeat rule.
    pub transition_duration: Option<Vec<u32>>,
    /// CSS `transition-timing-function` longhand - comma list of easing
    /// curves, cycled over the property list.
    pub transition_timing: Option<Vec<EasingIr>>,
    /// `required="true"` - form-field validity gate.
    pub required: bool,
    /// `pattern="<substring>"` - content must contain this literal
    /// substring to be valid. Values prefixed `shape:` are reserved for
    /// the built-in structural checks the parser attaches to
    /// `<date-picker>` (`shape:date`) and `<time-picker>`
    /// (`shape:time`); see `lumen_primitives::validation`.
    pub pattern: Option<String>,
    /// `multiline="true"` - text input accepts newlines. `<textarea>` defaults to true; `<input>` defaults to false.
    pub multiline: Option<bool>,
    /// Synthesised by the `<dropdown>` parser pass on the header
    /// button. The runtime attaches a [`lumen_primitives::DropdownButton`]
    /// whose click flips the open signal and whose option metadata
    /// drives *closed*-combobox keyboard interaction (Up/Down value
    /// stepping, Alt+Down open, type-ahead) before the lazily-mounted
    /// panel body exists.
    pub dropdown_button: Option<DropdownButtonSpec>,
    /// Synthesised per `<option>` inside `<dropdown>`: `(value_signal, value, open_signal)`. The runtime attaches a [`lumen_primitives::DropdownOptionButton`] whose click writes the value and closes the panel.
    pub dropdown_option: Option<(String, String, String)>,
    /// Synthesised per `<menuitem>` inside `<menu>`: `(open_signal, item_id)`. The runtime attaches a [`lumen_primitives::MenuItemButton`] whose click emits `MenuClicked { id: item_id }` and closes the menu.
    pub menu_item: Option<(String, String)>,
    /// Synthesised on the floating panel of a `<dropdown>` / `<menu>`:
    /// the open-state signal (`__dropdown_open:*` / `__menu_open:*`).
    /// The runtime attaches a [`lumen_primitives::PopupPanel`] so the
    /// outside-click dismissal and viewport edge-flip systems can find
    /// the panel and its bound open signal.
    pub popup_panel: Option<String>,
    /// `dir="ltr|rtl|auto"` - CSS Logical Properties writing direction
    /// (W5.4). When set, the spawn layer installs a
    /// [`lumen_core::components::LayoutDirection`] component; the
    /// `resolve_layout_direction` system then stamps a
    /// [`lumen_core::components::ResolvedDirection`] on every descendant.
    ///
    /// Serialized through [`layout_direction_serde`] because
    /// [`lumen_core::components::LayoutDirection`] lives in a crate that
    /// does not depend on serde; the proxy maps the three variants onto a
    /// stable `Option<u8>` so the AOT artifact stays parser-independent.
    #[serde(with = "layout_direction_serde")]
    pub dir: Option<lumen_core::components::LayoutDirection>,
    /// `translatable="<key>"` - marks the element's text for
    /// translation. The spawn layer resolves the key against the
    /// loaded catalogue and uses the result as the element's
    /// [`lumen_core::components::TextContent`], falling back to the
    /// authored `text` and then to the key itself. `lumenc i18n
    /// extract` collects these keys into `locale/<lang>.ftl`.
    pub translatable: Option<String>,
    /// `lang="<bcp47>"` - BCP-47 language tag (W5.4). When set, the
    /// spawn layer installs a [`lumen_core::components::Lang`] component
    /// consumed by text shaping (cosmic-text), AccessKit, and
    /// locale-aware formatters. Inherited at runtime from the nearest
    /// ancestor when absent.
    pub lang: Option<String>,
    /// `bind-text="$self.<field>"` - per-entity text binding. The
    /// field name (without the `$self.` prefix) lands here; the
    /// spawn layer installs a [`lumen_core::components::BindSelfText`]
    /// marker. Mutually exclusive with [`Self::bind`] for the same
    /// kind - the dollar-prefixed forms desugar to this field instead
    /// of the named-signal [`BindSpec`]. (W-signal-design step 1.)
    pub bind_self_text: Option<String>,
    /// `bind-value="$self.<field>"` - per-entity slider-value binding.
    /// See [`Self::bind_self_text`].
    pub bind_self_value: Option<String>,
    /// `bind-checked="$self.<field>"` - per-entity toggle binding.
    /// See [`Self::bind_self_text`].
    pub bind_self_checked: Option<String>,
    /// `bind-text="$parent.<field>"` - parent-entity text binding.
    /// The field name (without the `$parent.` prefix) lands here; the
    /// spawn layer installs a [`lumen_core::components::BindParentText`]
    /// marker. See [`Self::bind_self_text`].
    pub bind_parent_text: Option<String>,
    /// `bind-value="$parent.<field>"` - parent-entity slider-value binding.
    /// See [`Self::bind_parent_text`].
    pub bind_parent_value: Option<String>,
    /// `bind-checked="$parent.<field>"` - parent-entity toggle binding.
    /// See [`Self::bind_parent_text`].
    pub bind_parent_checked: Option<String>,
    /// `knob-inset="2"` / CSS `knob-inset` - gap in px between the knob
    /// child's edge and its track parent's edge on `<toggle>` /
    /// `<switch>`. Lumen-native analog property (mirrors [`Self::knob_color`]
    /// in having no real CSS pseudo-element target); absent = the
    /// runtime's own inset constant.
    pub knob_inset: Option<f32>,
    /// `thumb-size="20"` / CSS `thumb-size` - diameter in px of the
    /// `<slider>` thumb. Lumen-native analog property; absent = the
    /// runtime's own thumb-size constant.
    pub thumb_size: Option<f32>,
    /// `popup-gap="4"` / CSS `popup-gap` - offset in px between a
    /// `<dropdown>` / `<menu>` trigger and its floating panel. Absent =
    /// the runtime's own gap constant.
    pub popup_gap: Option<f32>,
    /// CSS `progress-chunk` - fraction of the track width (`0.0` to
    /// `1.0`) covered by the moving chunk of an indeterminate
    /// `<progress>` sweep. Absent = the runtime's own chunk-width
    /// constant.
    pub progress_chunk: Option<f32>,
    /// CSS `disabled-opacity` - alpha multiplier `[0, 1]` applied to a
    /// disabled entity when neither [`Self::disabled_bg`] nor an
    /// explicit `:disabled { opacity }` ([`Self::disabled_opacity`]) was
    /// authored. Distinct from `disabled_opacity`: that field is the
    /// per-element state-pseudo override, this field is the CSS-authored
    /// replacement for the runtime's own generic dimming fallback.
    pub disabled_opacity_default: Option<f32>,
    /// `caret-width="2"` / CSS `caret-width` - stroke width in px of the
    /// text-input caret. Absent = the runtime's own caret-width constant.
    /// Only meaningful on `<input>` / `<textarea>`.
    pub caret_width: Option<f32>,
    /// CSS `caret-blink` - full on/off blink period of the text-input
    /// caret, in milliseconds. Accepts `Nms` or `Ns`. Absent = the
    /// runtime's own blink-period constant. Only meaningful on
    /// `<input>` / `<textarea>`.
    pub caret_blink_ms: Option<u32>,
    /// CSS `password-character` - the glyph substituted for every
    /// character of a masked (`type="password"`-equivalent) text input.
    /// A single Unicode scalar value; absent = the runtime's own default
    /// mask glyph (typically `*` or a bullet).
    pub password_character: Option<char>,
    /// CSS `line-height` - see [`LineHeightSpec`] for why the unitless
    /// and px forms are kept distinct rather than folded into one number.
    /// Inherited like [`Self::font_size`]. Absent = the runtime's own
    /// line-height ratio.
    pub line_height: Option<LineHeightSpec>,
    /// CSS `scrollbar-thickness` - width in px of the overlay scrollbar
    /// track/thumb on `<scroll>` containers at `scrollbar-width: auto`.
    /// Absent = the runtime's own thickness constant.
    pub scrollbar_thickness: Option<f32>,
    /// CSS `scrollbar-thickness-thin` - width in px of the overlay
    /// scrollbar at `scrollbar-width: thin`. Absent = the runtime's own
    /// thin-thickness constant.
    pub scrollbar_thickness_thin: Option<f32>,
    /// CSS `scrollbar-margin` - gap in px between the scrollbar and the
    /// container's content edge. Absent = the runtime's own margin
    /// constant.
    pub scrollbar_margin: Option<f32>,
    /// CSS `scrollbar-min-thumb` - minimum thumb length in px, so a very
    /// long scrollable area still gets a grabbable thumb. Absent = the
    /// runtime's own minimum constant.
    pub scrollbar_min_thumb: Option<f32>,
    /// CSS `scrollbar-track-hover` - track fill shown while the pointer
    /// hovers the scrollbar. Lumen-native named property (mirrors
    /// [`Self::hover_bg`] in being a distinct property rather than a
    /// `:hover { ... }` pseudo rule, since the hover target is the
    /// scrollbar part, not the whole element). Absent = no track tint on
    /// hover.
    pub scrollbar_track_hover: Option<Rgba>,
    /// CSS `scrollbar-hover-boost` - multiplier applied to the thumb
    /// fill's brightness while hovered, paired with
    /// [`Self::scrollbar_track_hover`]. Absent = the runtime's own boost
    /// constant.
    pub scrollbar_hover_boost: Option<f32>,
    /// CSS `scrollbar-fade-delay` - idle time before an overlay
    /// scrollbar starts fading out, in milliseconds. Accepts `Nms` or
    /// `Ns`. Absent = the runtime's own delay constant.
    pub scrollbar_fade_delay_ms: Option<u32>,
    /// CSS `scrollbar-fade-duration` - length of the fade-out animation
    /// itself, in milliseconds. Accepts `Nms` or `Ns`. Absent = the
    /// runtime's own duration constant.
    pub scrollbar_fade_duration_ms: Option<u32>,
    /// Every styling attribute the element was given in markup, as
    /// `(property, value)` in the order they were written. The property is
    /// the one spelling the cascade files it under
    /// ([`crate::css::canonical_style_property`]); the value is the text the
    /// author wrote, unparsed.
    ///
    /// The fields above hold the *result* of the cascade and say nothing
    /// about where a value came from, so this is the record of which surface
    /// set what. It exists because the two surfaces do not rank the same way
    /// everywhere: in Lumen a styling attribute outranks any rule that
    /// targets the element, while on the web a rule outranks nothing but an
    /// inline declaration. A target with the web's ranking replays these as
    /// inline declarations so the author's markup keeps winning.
    pub markup_styles: Vec<(String, String)>,
    /// `name` on a `<slot>` inside a fragment body: which of the use site's
    /// children lands here. Absent on a `<slot>` names the default slot
    /// ([`crate::fragment::DEFAULT_SLOT`]), and absent on anything else
    /// means the element is not a slot at all.
    pub slot_name: Option<String>,
    /// Attributes whose value carries a fragment parameter, held back until
    /// a use site supplies the arguments.
    ///
    /// Most attribute values parse into typed layout data, which a
    /// `{parameter}` marker is not, so a fragment body keeps the authored
    /// text here instead and the compiler applies it once per instantiation.
    /// Empty on every element outside a fragment body, and emptied on the
    /// copy that reaches the tree.
    pub deferred: Vec<DeferredAttr>,
}

/// One attribute of a fragment body element, kept as authored.
///
/// See [`Attributes::deferred`]. The position is the value's own place in
/// the file that declared the fragment, so a diagnostic raised while
/// instantiating still points at where the value was written.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeferredAttr {
    /// Attribute name as authored.
    pub name: String,
    /// Attribute value as authored, parameter markers included.
    pub value: String,
    /// 1-based line of the value in the declaring file.
    pub line: usize,
    /// 1-based column of the value in the declaring file.
    pub col: usize,
}

impl Attributes {
    /// Resolve the accumulated `border-*` longhands into the effective
    /// CSS border, following the real cascade rules:
    ///
    /// * no `border-style` (or `border-style: none`) => no border - the
    ///   computed width of a styleless side is `0` (CSS Backgrounds &
    ///   Borders section 3.2), so `border-width`/`border-color` alone paint
    ///   nothing and consume no space;
    /// * `border-style: solid` => widths default to `3px` (CSS `medium`)
    ///   when `border-width` is absent, and the color falls back to
    ///   `currentColor` - approximated by the element's `text-color`
    ///   when authored, else opaque black (the CSS initial `color`).
    ///
    /// Returns `(widths, color)` for the solid case; `None` otherwise.
    /// The color is the uniform base - per-side overrides
    /// (`border-top-color` ...) are resolved by
    /// [`Self::effective_border_colors`].
    pub fn effective_border(&self) -> Option<(Edges, Rgba)> {
        match self.border_style {
            Some(BorderStyleSpec::Solid) => {
                let widths = self.border_width.unwrap_or(Edges::all(3.0));
                let color = self.border_color.or(self.text_color).unwrap_or(Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                });
                Some((widths, color))
            }
            Some(BorderStyleSpec::None) | None => None,
        }
    }

    /// Per-side border colors `[top, right, bottom, left]`: each side
    /// takes its longhand (`border-top-color` ...) when authored, else
    /// the uniform `base`. Returns `None` when no side differs from the
    /// base, so the uniform fast path stays intact downstream.
    pub fn effective_border_colors(&self, base: Rgba) -> Option<[Rgba; 4]> {
        if self.border_color_top.is_none()
            && self.border_color_right.is_none()
            && self.border_color_bottom.is_none()
            && self.border_color_left.is_none()
        {
            return None;
        }
        Some([
            self.border_color_top.unwrap_or(base),
            self.border_color_right.unwrap_or(base),
            self.border_color_bottom.unwrap_or(base),
            self.border_color_left.unwrap_or(base),
        ])
    }

    /// Resolve the transition shorthand + longhands into the effective
    /// per-property list.
    ///
    /// - The `transition:` shorthand, when authored, wins outright (it
    ///   resets the longhands per CSS shorthand semantics).
    /// - Otherwise `transition-property` defines the entry list, with
    ///   `transition-duration` / `transition-timing-function` values
    ///   cycled over it (the CSS list-repeat rule). A duration list
    ///   without `transition-property` produces nothing: there is no
    ///   default property list, so name the properties or write `all`.
    pub fn effective_transitions(&self) -> Vec<TransitionIr> {
        if !self.transitions.is_empty() {
            return self.transitions.clone();
        }
        let Some(props) = &self.transition_property else {
            return Vec::new();
        };
        // An *empty* list (reachable via `transition-duration: ,`) must be
        // treated like an absent one - otherwise `durations[0]` below indexes
        // an empty slice and panics.
        let durations: &[u32] = match self.transition_duration.as_deref() {
            Some(d) if !d.is_empty() => d,
            _ => &[0],
        };
        // CSS `ease`, the initial `transition-timing-function`, same
        // default the `transition` shorthand applies.
        let default_timing = [EasingIr::CubicBezier(0.25, 0.1, 0.25, 1.0)];
        let timings: &[EasingIr] = match self.transition_timing.as_deref() {
            Some(t) if !t.is_empty() => t,
            _ => &default_timing,
        };
        props
            .iter()
            .enumerate()
            .map(|(i, p)| TransitionIr {
                property: *p,
                duration_ms: durations[i % durations.len().max(1)],
                easing: timings[i % timings.len().max(1)],
            })
            .collect()
    }
}

/// IR-side parsed `transition` shorthand entry. `From<&TransitionIr> for
/// lumen_primitives::TransitionSpec` converts to the runtime form at
/// spawn time.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TransitionIr {
    /// Property name as authored (`"opacity"` ...). The spawn layer maps
    /// to `lumen_primitives::TransitionProperty`.
    pub property: TransitionPropertyIr,
    /// Duration in milliseconds.
    pub duration_ms: u32,
    /// Easing curve as authored.
    pub easing: EasingIr,
}

/// IR-side mirror of `lumen_primitives::TransitionProperty`. Kept local
/// to layout_ir so the parser doesn't need to depend on primitives.
///
/// v1 animatable set: colors + opacity only - geometry-free visual
/// props. Layout properties (`width`, `height`, padding, margins, ...)
/// are deliberately not transitionable in v1: animating them would
/// re-run layout every frame, and the parser warns + drops them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TransitionPropertyIr {
    /// CSS `opacity`.
    Opacity,
    /// CSS `background-color` (accepted spellings: `background-color`,
    /// `background`, `bg`).
    BackgroundColor,
    /// CSS `color` (accepted spellings: `color`, `text-color`).
    TextColor,
    /// CSS `border-color`.
    BorderColor,
}

impl TransitionPropertyIr {
    /// Parse a CSS property name into a transitionable property.
    /// Returns `None` for unknown / non-animatable names (the caller
    /// warns and drops per CSS "ignore unanimatable" behavior).
    pub fn from_css_name(name: &str) -> Option<Self> {
        match name {
            "opacity" => Some(Self::Opacity),
            "background-color" | "background" | "bg" => Some(Self::BackgroundColor),
            "color" | "text-color" => Some(Self::TextColor),
            "border-color" => Some(Self::BorderColor),
            _ => None,
        }
    }
}

/// IR-side mirror of `lumen_primitives::Easing`. Cubic-bezier carries
/// its four control points; named curves are zero-sized.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EasingIr {
    /// `linear`.
    Linear,
    /// `ease-in`.
    EaseIn,
    /// `ease-out`.
    EaseOut,
    /// `ease-in-out`.
    EaseInOut,
    /// `cubic-bezier(p1x, p1y, p2x, p2y)`.
    CubicBezier(f32, f32, f32, f32),
}

/// Parsed `shadow=` / `box-shadow=` value. Authors may stack multiple
/// shadows via comma separation in CSS; markup accepts a single entry.
/// `inner` flips drop -> inset.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShadowSpec {
    /// Horizontal offset in px.
    pub offset_x: f32,
    /// Vertical offset in px.
    pub offset_y: f32,
    /// Gaussian blur radius (std-dev) in px.
    pub blur: f32,
    /// CSS `box-shadow` spread radius in px - grows (positive) or
    /// shrinks (negative) the shadow rect before blurring. Enables the
    /// hard double-ring focus idiom (`box-shadow: 0 0 0 2px <color>`).
    pub spread: f32,
    /// Shadow color (alpha controls softness).
    pub color: Rgba,
    /// `true` = inset (inner) shadow. Default `false`.
    pub inner: bool,
}

/// CSS `object-fit` analogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImageFitSpec {
    /// Stretch to fill.
    Fill,
    /// Aspect-preserved cover (overflow clipped).
    Cover,
    /// Aspect-preserved fit (may leave space).
    Contain,
    /// Native pixel size, top-left aligned.
    None,
    /// Smaller of None / Contain.
    ScaleDown,
}

/// CSS-style position mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PositionSpec {
    /// In-flow positioning.
    Relative,
    /// Out-of-flow, offset by `inset`.
    Absolute,
}

/// CSS-style overflow control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OverflowSpec {
    /// Children paint outside (default).
    Visible,
    /// Children clipped at the box edge.
    Hidden,
    /// Clipped + scrollable.
    Scroll,
}

/// CSS `scrollbar-width` keyword (Scrollbars Styling Level 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ScrollbarWidthSpec {
    /// Platform-default overlay thickness.
    Auto,
    /// Narrow rail.
    Thin,
    /// Bars hidden entirely (content still scrolls).
    None,
}

impl From<ScrollbarWidthSpec> for lumen_core::input::ScrollbarWidthMode {
    fn from(s: ScrollbarWidthSpec) -> Self {
        match s {
            ScrollbarWidthSpec::Auto => Self::Auto,
            ScrollbarWidthSpec::Thin => Self::Thin,
            ScrollbarWidthSpec::None => Self::None,
        }
    }
}

/// Text wrap policy attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextWrapSpec {
    /// No automatic wrap.
    None,
    /// Word-break wrap.
    Word,
    /// Glyph-level wrap.
    Glyph,
}

/// Synthetic widget-part roles attached by the parser desugars - see
/// [`Attributes::part`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WidgetPart {
    /// The `.checkbox-box` tile inside `<checkbox>`.
    CheckboxBox,
    /// The `.radio-dot` tile inside `<radio>`.
    RadioDot,
    /// The `.progress-fill` tile inside `<progress>`.
    ProgressFill,
}

/// A widget tag that the parser replaces with plain boxes - see
/// [`Attributes::widget`].
///
/// These seven tags never reach the IR under their own name: the parser
/// expands each into the rows, buttons and gates that draw it, because the
/// runtime has no widget layer below the box layer. The expansion is lossy in
/// one direction only, and this is what records the loss, so a surface with a
/// widget of its own to offer can rebuild the authored one instead of drawing
/// the boxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WidgetRole {
    /// `<tabs>` - a strip of buttons over one panel per tab.
    Tabs,
    /// `<dropdown>` - a header button over a panel of options.
    Dropdown,
    /// `<menu>` - a panel of items shown while its signal is set.
    Menu,
    /// `<date-picker>` - a text entry holding a date.
    DatePicker,
    /// `<time-picker>` - a text entry holding a time of day.
    TimePicker,
    /// `<tooltip>` - text shown beside whatever it wraps. The marker lands
    /// on the wrapped element, which is what survives the expansion.
    Tooltip,
}

/// CSS `text-overflow` - what to do with text that overflows its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextOverflowSpec {
    /// Hard clip at the box edge (CSS default).
    Clip,
    /// Elide with a trailing `...` (Qt `elideMode`, CSS `ellipsis`).
    Ellipsis,
}

/// `<if mode="render|hide">` policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IfModeSpec {
    /// Despawn the body subtree on falsy -> truthy transitions (default).
    #[default]
    Render,
    /// Mount the body once and toggle `Visible(bool)` for show / hide.
    Hide,
}

impl From<EasingIr> for lumen_primitives::Easing {
    fn from(e: EasingIr) -> Self {
        use lumen_primitives::Easing as E;
        match e {
            EasingIr::Linear => E::Linear,
            EasingIr::EaseIn => E::EaseIn,
            EasingIr::EaseOut => E::EaseOut,
            EasingIr::EaseInOut => E::EaseInOut,
            EasingIr::CubicBezier(a, b, c, d) => E::CubicBezier(a, b, c, d),
        }
    }
}

impl From<TransitionPropertyIr> for lumen_primitives::TransitionProperty {
    fn from(p: TransitionPropertyIr) -> Self {
        use lumen_primitives::TransitionProperty as P;
        match p {
            TransitionPropertyIr::Opacity => P::Opacity,
            TransitionPropertyIr::BackgroundColor => P::BackgroundColor,
            TransitionPropertyIr::TextColor => P::TextColor,
            TransitionPropertyIr::BorderColor => P::BorderColor,
        }
    }
}

impl From<&TransitionIr> for lumen_primitives::TransitionSpec {
    fn from(ir: &TransitionIr) -> Self {
        lumen_primitives::TransitionSpec {
            property: ir.property.into(),
            duration: std::time::Duration::from_millis(ir.duration_ms as u64),
            easing: ir.easing.into(),
        }
    }
}

/// Parsed `bind="<kind>:<name>"`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BindSpec {
    /// Which component to drive.
    pub kind: BindKind,
    /// Signal name to read from.
    pub name: String,
}

/// What `bind=` targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BindKind {
    /// Drive `TextContent` from the signal value (stringified).
    Text,
    /// Drive `Toggleable.checked` from the signal (truthy = on).
    Checked,
    /// Drive `SliderValue.value` from the signal (parsed as f32).
    Value,
}

/// Parsed `focus-outline="<width> <#color>"` (plus the CSS
/// `outline-offset` folded in at spawn).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OutlineSpec {
    /// Stroke width in pixels.
    pub width: f32,
    /// Stroke color.
    pub color: Rgba,
    /// Gap between the border box edge and the inner edge of the ring
    /// (CSS `outline-offset`). Default 0.
    pub offset: f32,
}

/// Horizontal text alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextAlignSpec {
    /// Left.
    Start,
    /// Centered.
    Center,
    /// Right.
    End,
}

/// Cross-axis alignment (CSS `align-items`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FlexAlign {
    /// Flex-start.
    Start,
    /// Flex-end.
    End,
    /// Centered.
    Center,
    /// Stretch to fill cross size.
    Stretch,
    /// Baseline alignment (W5.9).
    Baseline,
}

/// CSS `display` value (W5.9). Mirrors lumen-core's `Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DisplaySpec {
    /// Flexbox container (default).
    Flex,
    /// CSS Grid container.
    Grid,
    /// Hidden.
    None,
}

/// One CSS Grid track-size term (W5.9). IR-side mirror of
/// [`lumen_core::components::TrackSize`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TrackSizeSpec {
    /// `<N>px`.
    Fixed(f32),
    /// `auto`.
    Auto,
    /// `<N>fr`.
    Fr(f32),
    /// `min-content`.
    MinContent,
    /// `max-content`.
    MaxContent,
    /// `minmax(<min>, <max>)`.
    MinMax(Box<TrackSizeSpec>, Box<TrackSizeSpec>),
}

/// Parsed `grid-template-rows` + `grid-template-columns` lists (W5.9).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GridTemplateSpec {
    /// `grid-template-rows`.
    pub rows: Vec<TrackSizeSpec>,
    /// `grid-template-columns`.
    pub columns: Vec<TrackSizeSpec>,
}

/// Main-axis distribution (CSS `justify-content`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FlexJustify {
    /// Pack at the start.
    Start,
    /// Pack at the end.
    End,
    /// Pack at the center.
    Center,
    /// Space between siblings, no edge padding.
    SpaceBetween,
    /// Space around siblings, half-step at edges.
    SpaceAround,
    /// Even spacing including edges.
    SpaceEvenly,
}

/// Flex direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FlexAxis {
    /// Horizontal.
    Row,
    /// Vertical.
    Column,
    /// Horizontal, reversed (CSS `row-reverse`).
    RowReverse,
    /// Vertical, reversed (CSS `column-reverse`).
    ColumnReverse,
}

/// CSS `flex-wrap` values (IR-side mirror of
/// [`lumen_core::components::FlexWrap`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FlexWrapSpec {
    /// Single line (CSS initial value).
    NoWrap,
    /// Wrap onto additional lines.
    Wrap,
    /// Wrap with reversed line order.
    WrapReverse,
}

/// CSS `align-content` values (IR-side mirror of
/// [`lumen_core::components::AlignContent`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlignContentSpec {
    /// Pack lines at the start.
    Start,
    /// Pack lines at the end.
    End,
    /// Pack lines at the center.
    Center,
    /// Stretch lines (CSS initial value).
    Stretch,
    /// Even gaps between lines.
    SpaceBetween,
    /// Half gaps at the edges.
    SpaceAround,
    /// Equal gaps everywhere.
    SpaceEvenly,
}

/// CSS `border-style` subset. v1 recognises `none` and `solid`; other
/// keywords are rejected at parse time with a per-declaration warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BorderStyleSpec {
    /// No border - computed border-width is zero on every side (CSS).
    None,
    /// Solid stroke.
    Solid,
}

/// CSS `box-sizing` values (IR-side mirror of
/// [`lumen_core::components::BoxSizing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BoxSizingSpec {
    /// Sizes include padding + border (Lumen UA default).
    BorderBox,
    /// Sizes cover the content box only.
    ContentBox,
}

/// Resolved border paint for a state override (`:hover { border: ... }`,
/// `hover-border:` / `focus-border:`). Unlike the base border - which
/// accumulates from the `border-width` / `border-color` / `border-style`
/// longhands - a state border is authored via the shorthand only, so it
/// resolves to concrete widths + color at parse time.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BorderPaintSpec {
    /// Per-side widths in px.
    pub widths: Edges,
    /// Border color.
    pub color: Rgba,
}

// -- IR -> core type conversions. spawn.rs uses `.into()` instead of
// inline `match` expressions for every attribute. ---------------------

impl From<LengthSpec> for lumen_core::components::Length {
    fn from(l: LengthSpec) -> Self {
        match l {
            LengthSpec::Auto => lumen_core::components::Length::Auto,
            LengthSpec::Px(v) => lumen_core::components::Length::Px(v),
            LengthSpec::Percent(v) => lumen_core::components::Length::Percent(v),
        }
    }
}

impl From<FlexAxis> for lumen_core::components::FlexDirection {
    fn from(a: FlexAxis) -> Self {
        match a {
            FlexAxis::Row => lumen_core::components::FlexDirection::Row,
            FlexAxis::Column => lumen_core::components::FlexDirection::Column,
            FlexAxis::RowReverse => lumen_core::components::FlexDirection::RowReverse,
            FlexAxis::ColumnReverse => lumen_core::components::FlexDirection::ColumnReverse,
        }
    }
}

impl From<FlexWrapSpec> for lumen_core::components::FlexWrap {
    fn from(w: FlexWrapSpec) -> Self {
        match w {
            FlexWrapSpec::NoWrap => lumen_core::components::FlexWrap::NoWrap,
            FlexWrapSpec::Wrap => lumen_core::components::FlexWrap::Wrap,
            FlexWrapSpec::WrapReverse => lumen_core::components::FlexWrap::WrapReverse,
        }
    }
}

impl From<AlignContentSpec> for lumen_core::components::AlignContent {
    fn from(a: AlignContentSpec) -> Self {
        use lumen_core::components::AlignContent as Core;
        match a {
            AlignContentSpec::Start => Core::Start,
            AlignContentSpec::End => Core::End,
            AlignContentSpec::Center => Core::Center,
            AlignContentSpec::Stretch => Core::Stretch,
            AlignContentSpec::SpaceBetween => Core::SpaceBetween,
            AlignContentSpec::SpaceAround => Core::SpaceAround,
            AlignContentSpec::SpaceEvenly => Core::SpaceEvenly,
        }
    }
}

impl From<BoxSizingSpec> for lumen_core::components::BoxSizing {
    fn from(b: BoxSizingSpec) -> Self {
        match b {
            BoxSizingSpec::BorderBox => lumen_core::components::BoxSizing::BorderBox,
            BoxSizingSpec::ContentBox => lumen_core::components::BoxSizing::ContentBox,
        }
    }
}

impl From<BorderPaintSpec> for lumen_core::components::Border {
    fn from(b: BorderPaintSpec) -> Self {
        lumen_core::components::Border {
            widths: b.widths.into(),
            color: b.color.into(),
            side_colors: None,
        }
    }
}

impl From<FlexAlign> for lumen_core::components::FlexAlign {
    fn from(a: FlexAlign) -> Self {
        match a {
            FlexAlign::Start => lumen_core::components::FlexAlign::Start,
            FlexAlign::End => lumen_core::components::FlexAlign::End,
            FlexAlign::Center => lumen_core::components::FlexAlign::Center,
            FlexAlign::Stretch => lumen_core::components::FlexAlign::Stretch,
            FlexAlign::Baseline => lumen_core::components::FlexAlign::Baseline,
        }
    }
}

impl From<DisplaySpec> for lumen_core::components::Display {
    fn from(d: DisplaySpec) -> Self {
        match d {
            DisplaySpec::Flex => lumen_core::components::Display::Flex,
            DisplaySpec::Grid => lumen_core::components::Display::Grid,
            DisplaySpec::None => lumen_core::components::Display::None,
        }
    }
}

impl From<&TrackSizeSpec> for lumen_core::components::TrackSize {
    fn from(t: &TrackSizeSpec) -> Self {
        use lumen_core::components::TrackSize as Core;
        match t {
            TrackSizeSpec::Fixed(v) => Core::Fixed(*v),
            TrackSizeSpec::Auto => Core::Auto,
            TrackSizeSpec::Fr(f) => Core::Fr(*f),
            TrackSizeSpec::MinContent => Core::MinContent,
            TrackSizeSpec::MaxContent => Core::MaxContent,
            TrackSizeSpec::MinMax(min, max) => Core::MinMax(
                Box::new(Core::from(min.as_ref())),
                Box::new(Core::from(max.as_ref())),
            ),
        }
    }
}

impl From<&GridTemplateSpec> for lumen_core::components::GridTemplate {
    fn from(g: &GridTemplateSpec) -> Self {
        lumen_core::components::GridTemplate {
            rows: g.rows.iter().map(Into::into).collect(),
            columns: g.columns.iter().map(Into::into).collect(),
        }
    }
}

impl From<FlexJustify> for lumen_core::components::FlexJustify {
    fn from(j: FlexJustify) -> Self {
        match j {
            FlexJustify::Start => lumen_core::components::FlexJustify::Start,
            FlexJustify::End => lumen_core::components::FlexJustify::End,
            FlexJustify::Center => lumen_core::components::FlexJustify::Center,
            FlexJustify::SpaceBetween => lumen_core::components::FlexJustify::SpaceBetween,
            FlexJustify::SpaceAround => lumen_core::components::FlexJustify::SpaceAround,
            FlexJustify::SpaceEvenly => lumen_core::components::FlexJustify::SpaceEvenly,
        }
    }
}

impl From<TextAlignSpec> for lumen_core::components::TextAlign {
    fn from(a: TextAlignSpec) -> Self {
        match a {
            TextAlignSpec::Start => lumen_core::components::TextAlign::Start,
            TextAlignSpec::Center => lumen_core::components::TextAlign::Center,
            TextAlignSpec::End => lumen_core::components::TextAlign::End,
        }
    }
}

impl From<PositionSpec> for lumen_core::components::Position {
    fn from(p: PositionSpec) -> Self {
        match p {
            PositionSpec::Relative => lumen_core::components::Position::Relative,
            PositionSpec::Absolute => lumen_core::components::Position::Absolute,
        }
    }
}

impl From<ImageFitSpec> for lumen_core::components::ImageFit {
    fn from(f: ImageFitSpec) -> Self {
        match f {
            ImageFitSpec::Fill => lumen_core::components::ImageFit::Fill,
            ImageFitSpec::Cover => lumen_core::components::ImageFit::Cover,
            ImageFitSpec::Contain => lumen_core::components::ImageFit::Contain,
            ImageFitSpec::None => lumen_core::components::ImageFit::None,
            ImageFitSpec::ScaleDown => lumen_core::components::ImageFit::ScaleDown,
        }
    }
}

impl From<OverflowSpec> for lumen_core::components::Overflow {
    fn from(o: OverflowSpec) -> Self {
        match o {
            OverflowSpec::Visible => lumen_core::components::Overflow::Visible,
            OverflowSpec::Hidden => lumen_core::components::Overflow::Hidden,
            OverflowSpec::Scroll => lumen_core::components::Overflow::Scroll,
        }
    }
}

impl From<TextWrapSpec> for lumen_core::components::TextWrap {
    fn from(w: TextWrapSpec) -> Self {
        match w {
            TextWrapSpec::None => lumen_core::components::TextWrap::None,
            TextWrapSpec::Word => lumen_core::components::TextWrap::Word,
            TextWrapSpec::Glyph => lumen_core::components::TextWrap::Glyph,
        }
    }
}

impl From<ScrollAxisSpec> for lumen_core::input::ScrollAxis {
    fn from(a: ScrollAxisSpec) -> Self {
        match a {
            ScrollAxisSpec::Y => lumen_core::input::ScrollAxis::Y,
            ScrollAxisSpec::X => lumen_core::input::ScrollAxis::X,
            ScrollAxisSpec::Both => lumen_core::input::ScrollAxis::Both,
        }
    }
}

impl From<Edges> for lumen_core::components::Edges {
    fn from(e: Edges) -> Self {
        // W5.5: physical sides forward 1:1; the four logical-edge
        // overrides ride along so the layout backend can resolve them
        // against `ResolvedDirection` per CSS Logical Properties L1.
        // Percent units (CSS `padding: 5%`) ride along the same way.
        lumen_core::components::Edges {
            left: e.left,
            right: e.right,
            top: e.top,
            bottom: e.bottom,
            inline_start: e.inline_start,
            inline_end: e.inline_end,
            block_start: e.block_start,
            block_end: e.block_end,
            pct_left: e.pct_left,
            pct_right: e.pct_right,
            pct_top: e.pct_top,
            pct_bottom: e.pct_bottom,
        }
    }
}

impl From<Rgba> for lumen_core::components::Color {
    fn from(c: Rgba) -> Self {
        lumen_core::components::Color::rgba(c.r, c.g, c.b, c.a)
    }
}

#[cfg(test)]
mod bughunt_tests {
    use super::*;

    #[test]
    fn effective_transitions_empty_duration_list_does_not_panic() {
        // A `Some(vec![])` (reachable via `transition-duration: ,`) used to
        // index an empty slice at `durations[0]` and panic. It must now be
        // treated like an absent list (default 0ms).
        let attrs = Attributes {
            transition_property: Some(vec![TransitionPropertyIr::Opacity]),
            transition_duration: Some(Vec::new()),
            transition_timing: Some(Vec::new()),
            ..Default::default()
        };
        let out = attrs.effective_transitions();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].duration_ms, 0);
        assert_eq!(out[0].easing, EasingIr::CubicBezier(0.25, 0.1, 0.25, 1.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asset paths come out of the compiler resolved against the machine
    /// that compiled and have to leave relative to the app, or a copied
    /// package looks for its images in a directory that only exists there.
    #[test]
    fn asset_paths_leave_relative_to_the_app() {
        // Absolute on every platform, and with each family's own separators:
        // a literal POSIX path is relative on Windows, where the rewrite would
        // have nothing to do and the test would pass without testing anything.
        let app = std::env::temp_dir().join("notes");
        let inside = app.join("icons").join("save.png");
        let elsewhere = std::env::temp_dir().join("pixmaps").join("other.png");

        let image = |path: &Path| Element {
            tag: "image".to_string(),
            attrs: Attributes {
                src: Some(path.to_string_lossy().into_owned()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut root = Element {
            tag: "root".to_string(),
            children: vec![image(&inside), image(&elsewhere)],
            ..Default::default()
        };

        let mut outside = Vec::new();
        relativize_asset_paths(&mut root, &app, &mut outside);

        // Compared as a path, not as text: the separator between `icons` and
        // the file name is the platform's own.
        let rewritten = root.children[0]
            .attrs
            .src
            .as_deref()
            .expect("the asset keeps a path");
        assert_eq!(Path::new(rewritten), Path::new("icons").join("save.png"));
        assert!(
            Path::new(rewritten).is_relative(),
            "a packaged asset path must be relative to the app: {rewritten}"
        );
        assert_eq!(
            outside,
            vec![elsewhere.to_string_lossy().into_owned()],
            "a file outside the app directory keeps the path it had"
        );
    }

    /// A placeholder's spelling alone decides its scope, and adding the
    /// fragment-parameter scope does not change that: `{$name}` and the bare
    /// `{name}` stay global lookups, because nothing in the text says which
    /// of the two a name is. Only a fragment's declared parameter list can
    /// turn one into [`InterpolationSlot::Arg`].
    #[test]
    fn placeholder_classification_is_by_spelling() {
        let cases = [
            ("$name", InterpolationSlot::Global("name".to_string())),
            ("name", InterpolationSlot::Global("name".to_string())),
            (" $name ", InterpolationSlot::Global("name".to_string())),
            ("$index", InterpolationSlot::RowIndex),
            ("idx", InterpolationSlot::RowIndex),
            ("row.title", InterpolationSlot::Row("title".to_string())),
            ("$self.x", InterpolationSlot::SelfField("x".to_string())),
            ("$parent.y", InterpolationSlot::ParentField("y".to_string())),
        ];
        for (text, expected) in cases {
            assert_eq!(InterpolationSlot::from(text), expected, "`{text}`");
        }
    }

    /// The slot list is part of the artifact, so every variant has to make
    /// the round trip, the new one included.
    #[test]
    fn placeholder_slots_round_trip() {
        let slots = vec![
            InterpolationSlot::Global("count".to_string()),
            InterpolationSlot::Row("title".to_string()),
            InterpolationSlot::RowIndex,
            InterpolationSlot::SelfField("x".to_string()),
            InterpolationSlot::ParentField("y".to_string()),
            InterpolationSlot::Arg("tone".to_string()),
        ];
        let bytes = bincode::serialize(&slots).expect("slots encode");
        let back: Vec<InterpolationSlot> = bincode::deserialize(&bytes).expect("slots decode");
        assert_eq!(back, slots);
    }
}
