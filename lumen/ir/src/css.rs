//! CSS data model (AST) + Cascade-5 application, extracted from `lumenc`'s
//! `parser_css` front-end so the parser-free runtime and every other IR
//! consumer share the same cascade without pulling in the markup parser.
//!
//! The hand-rolled front-end (`parse_css` and its lexer) still lives in
//! `lumenc::parser_css` and produces the [`Stylesheet`] type defined here;
//! this crate owns the *data* + the code that *applies* it.
//!
//! ## Cascade ordering
//!
//! Per CSS Cascade-5 section 6.4: origin -> importance -> specificity -> source
//! order, and **later** rules win at equal weight (section 6.4.4). Within
//! [`apply_css`], HTML inline attrs (origin: inline) beat CSS attrs
//! (origin: user/UA) - preserving the long-standing rule that
//! `<tile width="50px"/>` overrides `.t { width: 100px }`. Inline
//! `!important` is not authorable; user `!important` lifts a CSS
//! declaration above its origin's normal block.

use crate::layout_ir::{
    Attributes, BgSpec, DisplaySpec, EasingIr, Edges, Element, FlexAlign, FlexJustify,
    ImageFitSpec, LayoutIR, LengthSpec, OverflowSpec, ParseError, PositionSpec, Rgba,
    ScrollAxisSpec, ScrollbarWidthSpec, ShadowSpec, TextAlignSpec, TextWrapSpec, TrackSizeSpec,
    TransitionIr, TransitionPropertyIr,
};
use crate::values::{bad, parse_bg, parse_color, parse_edges, parse_f32, parse_i32, parse_length};
// ---------------------------------------------------------------------------
// Public AST
// ---------------------------------------------------------------------------

/// A parsed stylesheet.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Stylesheet {
    /// Rules in source order. Each rule may live inside a `@media`
    /// block (recorded via [`Rule::media`]).
    pub rules: Vec<Rule>,
}

impl Stylesheet {
    /// Returns the union of class names appearing in any selector.
    /// The runtime class-change reapply path uses this as a fast-rejection
    /// set: when none of the changed classes are in the returned set, the
    /// respawn is skipped.
    pub fn class_invalidation_set(&self) -> std::collections::HashSet<String> {
        let mut set = std::collections::HashSet::new();
        for rule in &self.rules {
            for sel in &rule.selectors {
                sel.collect_classes(&mut set);
            }
        }
        set
    }
}

/// Cascade origin per CSS Cascade 4 section 6.1. Lumen models the two
/// origins it actually mixes: the built-in skin sheet ships as the
/// **user-agent** origin, and the app's own CSS is the **author**
/// origin. For normal (non-`!important`) declarations author beats
/// user-agent regardless of specificity, so the variants are ordered
/// `UserAgent < Author` and the cascade sort keys on origin first.
///
/// (Lumen has no user origin, and `!important` origin inversion is not
/// modelled here.)
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum Origin {
    /// Built-in skin sheet. Loses to author CSS for normal declarations.
    UserAgent,
    /// The app's own CSS. Wins over the skin for normal declarations.
    #[default]
    Author,
}

/// One CSS rule (one `selector_list { decl* }` block).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Rule {
    /// Compound selector list (each entry is one selector in the
    /// comma-separated list).
    pub selectors: Vec<SelectorBuf>,
    /// `property: value` pairs in source order, with `!important`
    /// resolved at parse time.
    pub declarations: Vec<Declaration>,
    /// Cascade origin. Defaults to [`Origin::Author`]; the runtime tags
    /// the built-in skin sheet as [`Origin::UserAgent`] before the
    /// combined cascade pass. The first term of the cascade sort.
    #[serde(default)]
    pub origin: Origin,
    /// Source ordinal across the stylesheet (the last tiebreaker in
    /// the cascade sort).
    pub source_order: usize,
    /// `@media (...)` enclosing query, if any.
    pub media: Option<MediaQuery>,
    /// Back-compat shim that exposes the leftmost compound of the
    /// first selector under the old single-`Selector` shape - the
    /// `run::extract_root_vars` and `StyleInvalidationCache` paths
    /// still poke at `rule.selector.classes` / `rule.selector.tag`.
    pub selector: LegacySelectorShim,
}

/// Back-compat shim - see [`Rule::selector`].
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LegacySelectorShim {
    /// Tag of the leftmost compound in the first selector.
    pub tag: Option<String>,
    /// Classes of the leftmost compound in the first selector.
    pub classes: Vec<String>,
}

/// One `property: value` pair.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Declaration {
    /// Property name.
    pub name: String,
    /// Raw textual value, trimmed and var()-unresolved.
    pub value: String,
    /// `true` when the source ended with `!important`.
    pub important: bool,
}

// ---------------------------------------------------------------------------
// Selector AST
// ---------------------------------------------------------------------------

/// Compiled selector: a chain of compound selectors joined by combinators.
///
/// The chain is stored *left-to-right* in CSS source order - so
/// `.outer .inner > .x` becomes:
///
/// `[(Subject, .outer), (Descendant, .inner), (Child, .x)]`
///
/// The *subject* of the selector (the element that the rule applies
/// to) is the **last** compound in the chain. The first entry's
/// combinator is always [`Combinator::Subject`] and records that
/// there is nothing to its left.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SelectorBuf {
    /// Compound chain in CSS source order (subject is the last entry).
    pub chain: Vec<(Combinator, CompoundSelector)>,
}

impl SelectorBuf {
    /// Specificity per Selectors-4 section 17 - `(a, b, c)` where
    /// `a` = #ids, `b` = #classes + #pseudo-classes + #attr selectors,
    /// `c` = #tags + #pseudo-elements.
    pub fn specificity(&self) -> Specificity {
        let mut spec = Specificity::default();
        for (_, c) in &self.chain {
            spec = spec.add(c.specificity());
        }
        spec
    }

    fn collect_classes(&self, out: &mut std::collections::HashSet<String>) {
        for (_, c) in &self.chain {
            c.collect_classes(out);
        }
    }
}

/// `(a, b, c)` triple, lexicographically ordered for cascade sort.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Specificity {
    /// IDs.
    pub a: u32,
    /// Classes + pseudo-classes + attribute selectors.
    pub b: u32,
    /// Element types + pseudo-elements.
    pub c: u32,
}

impl Specificity {
    /// Component-wise sum.
    pub const fn add(self, o: Self) -> Self {
        Self {
            a: self.a + o.a,
            b: self.b + o.b,
            c: self.c + o.c,
        }
    }

    /// Promote to the larger of the two. Used by `:is()` / `:not()`
    /// argument specificity per Selectors-4 section 17.
    fn promote(self, o: Self) -> Self {
        if self > o { self } else { o }
    }
}

/// Connector between two adjacent compounds in a selector chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Combinator {
    /// The first compound - nothing to its left.
    Subject,
    /// `A B` - descendant.
    Descendant,
    /// `A > B` - direct child.
    Child,
    /// `A + B` - adjacent sibling.
    AdjacentSibling,
    /// `A ~ B` - general sibling.
    GeneralSibling,
}

/// One compound selector - `tag#id.cls:pseudo`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CompoundSelector {
    /// Tag name. `None` matches any tag (universal `*`).
    pub tag: Option<String>,
    /// `#id`.
    pub id: Option<String>,
    /// Required class names.
    pub classes: Vec<String>,
    /// Required pseudo-classes (each is a [`PseudoClass`]).
    pub pseudo_classes: Vec<PseudoClass>,
}

impl CompoundSelector {
    fn specificity(&self) -> Specificity {
        let mut spec = Specificity::default();
        if self.id.is_some() {
            spec.a += 1;
        }
        spec.b += self.classes.len() as u32;
        for p in &self.pseudo_classes {
            spec = spec.add(p.specificity());
        }
        if self.tag.is_some() {
            spec.c += 1;
        }
        spec
    }

    fn collect_classes(&self, out: &mut std::collections::HashSet<String>) {
        for c in &self.classes {
            out.insert(c.clone());
        }
        for p in &self.pseudo_classes {
            p.collect_classes(out);
        }
    }

    /// `true` when the compound has no tag, id, classes, or pseudo-classes
    /// (the empty universal). The parser front-end uses this to reject
    /// `.a  .b` with a dangling combinator.
    pub fn is_empty(&self) -> bool {
        self.tag.is_none()
            && self.id.is_none()
            && self.classes.is_empty()
            && self.pseudo_classes.is_empty()
    }
}

/// All pseudo-classes the engine recognises.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PseudoClass {
    /// `:hover` - pointer over the element.
    Hover,
    /// `:focus` - focus from any input source (pointer or keyboard).
    Focus,
    /// `:focus-visible` - keyboard-only focus (Tab / Shift-Tab, roving
    /// arrows, assistive tech). The runtime tracks how focus arrived
    /// via the `FocusVisible` marker; pointer clicks focus without it.
    FocusVisible,
    /// `:active` - pressed.
    Active,
    /// `:disabled` - routes `bg` to the disabled fill; the runtime
    /// `Disabled` marker gates input.
    Disabled,
    /// `:checked` - routes `bg` to the `<toggle>` checked track fill.
    Checked,
    /// `:selected` - the active member of a selection group (tab
    /// strip button). Routes `bg` to `Attributes::selected_bg`;
    /// `lumen_primitives::tabs::sync_tab_button_visuals` swaps the fill
    /// at runtime as the `Selected` marker moves between siblings.
    Selected,
    /// `:drag-over` - an in-app drag is hovering this drop target with an
    /// acceptable payload (HTML5 `dragover` parity). The runtime
    /// `DropHovered` marker, maintained by `lumen-os-dnd`, gates it.
    DragOver,
    /// `:root` - matches only the root element.
    Root,
    /// `:first-child`.
    FirstChild,
    /// `:last-child`.
    LastChild,
    /// `:only-child`.
    OnlyChild,
    /// `:empty` - no element children, no non-whitespace text.
    Empty,
    /// `:nth-child(an+b)`.
    NthChild(AnB),
    /// `:is(...)`.
    Is(Vec<SelectorBuf>),
    /// `:where(...)`.
    Where(Vec<SelectorBuf>),
    /// `:not(...)`.
    Not(Vec<SelectorBuf>),
}

impl PseudoClass {
    fn specificity(&self) -> Specificity {
        match self {
            Self::Hover
            | Self::Focus
            | Self::FocusVisible
            | Self::Active
            | Self::Disabled
            | Self::Checked
            | Self::Selected
            | Self::DragOver
            | Self::Root
            | Self::FirstChild
            | Self::LastChild
            | Self::OnlyChild
            | Self::Empty
            | Self::NthChild(_) => Specificity { a: 0, b: 1, c: 0 },
            Self::Is(args) | Self::Not(args) => {
                let mut acc = Specificity::default();
                for s in args {
                    acc = acc.promote(s.specificity());
                }
                acc
            }
            Self::Where(_) => Specificity::default(),
        }
    }

    fn collect_classes(&self, out: &mut std::collections::HashSet<String>) {
        match self {
            Self::Is(args) | Self::Where(args) | Self::Not(args) => {
                for s in args {
                    s.collect_classes(out);
                }
            }
            _ => {}
        }
    }
}

/// `:nth-child(an+b)` coefficients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnB {
    /// `a` (step).
    pub a: i32,
    /// `b` (offset).
    pub b: i32,
}

impl AnB {
    /// Test whether the 1-based child index matches `an + b`.
    pub fn matches(self, index: i32) -> bool {
        if self.a == 0 {
            return index == self.b;
        }
        // Widen to i64: `index - self.b` overflows when `b == i32::MIN`.
        let diff = index as i64 - self.b as i64;
        let a = self.a as i64;
        if (diff < 0 && a > 0) || (diff > 0 && a < 0) {
            return false;
        }
        diff % a == 0
    }
}

// ---------------------------------------------------------------------------
// @media - MQ-5 subset
// ---------------------------------------------------------------------------

/// `@media` condition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaQuery {
    /// `and`-joined feature list.
    pub features: Vec<MediaFeature>,
}

/// One `(feature: value)` test inside an `@media` query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum MediaFeature {
    /// `(prefers-color-scheme: dark | light | no-preference)`.
    PrefersColorScheme(ColorSchemePreference),
    /// `(prefers-reduced-motion: reduce | no-preference)`.
    PrefersReducedMotion(MotionPreference),
    /// `(prefers-contrast: more | less | custom | no-preference)`.
    PrefersContrast(ContrastPreference),
    /// `(min-width: <px>)`.
    MinWidth(f32),
    /// `(max-width: <px>)`.
    MaxWidth(f32),
    /// `(width: <px>)`.
    Width(f32),
}

/// `prefers-color-scheme` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ColorSchemePreference {
    /// `dark`.
    Dark,
    /// `light`.
    Light,
    /// `no-preference`.
    NoPreference,
}

/// `prefers-reduced-motion` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MotionPreference {
    /// `reduce`.
    Reduce,
    /// `no-preference`.
    NoPreference,
}

/// `prefers-contrast` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContrastPreference {
    /// `more`.
    More,
    /// `less`.
    Less,
    /// `custom`.
    Custom,
    /// `no-preference`.
    NoPreference,
}

/// Runtime view of the OS-level theme + viewport state. Passed to
/// [`apply_css_with_media`] so `@media` blocks resolve consistently.
#[derive(Debug, Clone, Copy)]
pub struct MediaContext {
    /// OS color scheme. `None` = unknown.
    pub color_scheme: Option<ColorSchemePreference>,
    /// User motion preference.
    pub reduced_motion: MotionPreference,
    /// User contrast preference.
    pub contrast: ContrastPreference,
    /// Viewport width in CSS pixels (used for width / min-width /
    /// max-width queries). `None` = no width-MQ matches.
    pub viewport_width: Option<f32>,
}

impl Default for MediaContext {
    fn default() -> Self {
        Self {
            color_scheme: None,
            reduced_motion: MotionPreference::NoPreference,
            contrast: ContrastPreference::NoPreference,
            viewport_width: None,
        }
    }
}

impl MediaQuery {
    /// `true` when every feature matches the given context.
    pub fn matches(&self, ctx: &MediaContext) -> bool {
        self.features.iter().all(|f| f.matches(ctx))
    }
}

impl MediaFeature {
    fn matches(&self, ctx: &MediaContext) -> bool {
        match self {
            Self::PrefersColorScheme(want) => match (ctx.color_scheme, *want) {
                (Some(cs), w) => cs == w,
                (None, ColorSchemePreference::NoPreference) => true,
                (None, _) => false,
            },
            Self::PrefersReducedMotion(want) => ctx.reduced_motion == *want,
            Self::PrefersContrast(want) => ctx.contrast == *want,
            Self::MinWidth(min) => ctx.viewport_width.is_some_and(|w| w >= *min),
            Self::MaxWidth(max) => ctx.viewport_width.is_some_and(|w| w <= *max),
            Self::Width(w) => ctx.viewport_width.is_some_and(|vp| (vp - *w).abs() < 0.5),
        }
    }
}
// ---------------------------------------------------------------------------
// Cascade application
// ---------------------------------------------------------------------------

/// Apply a stylesheet to a `LayoutIR`. Each property follows
/// `(origin, !important, specificity, source_order)` ordering and the
/// **last** matching declaration wins. HTML inline attrs always beat
/// CSS (origin precedence: inline > user/UA).
pub fn apply_css(ir: &mut LayoutIR, css: &Stylesheet) -> Result<Vec<CssWarning>, ParseError> {
    apply_css_with_media(ir, css, &MediaContext::default())
}

/// Same as [`apply_css`] but honours `@media` blocks against the given
/// [`MediaContext`].
///
/// A bad declaration (unknown property, unparseable value, unresolvable
/// `var()`) is skipped and reported as a [`CssWarning`] - it never takes
/// down the rest of the stylesheet, matching CSS error-recovery
/// semantics. The returned list is deduplicated, so a rule that matches
/// a thousand elements reports each defect once.
pub fn apply_css_with_media(
    ir: &mut LayoutIR,
    css: &Stylesheet,
    media: &MediaContext,
) -> Result<Vec<CssWarning>, ParseError> {
    let inherited: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let parents: Vec<ElementRef> = Vec::new();
    let mut warnings = Vec::new();
    apply_to_element(
        &mut ir.root,
        css,
        media,
        &inherited,
        &InheritedText::default(),
        &parents,
        1,
        1,
        &mut warnings,
    );
    let mut seen = std::collections::HashSet::new();
    warnings.retain(|w| seen.insert((w.selector.clone(), w.property.clone(), w.message.clone())));
    Ok(warnings)
}

/// A skipped declaration, surfaced to the CLI / hot-reload log / LSP
/// instead of aborting the stylesheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssWarning {
    /// Selector text of the rule the declaration lives in.
    pub selector: String,
    /// Property name as written.
    pub property: String,
    /// Human-readable reason the declaration was skipped.
    pub message: String,
}

impl std::fmt::Display for CssWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "css warning: {} {{ {} }} - {}",
            self.selector, self.property, self.message
        )
    }
}

/// Lightweight ancestor reference for parent/sibling matching. Avoids
/// cloning whole subtrees during cascade resolution.
#[derive(Debug, Clone)]
struct ElementRef {
    tag: String,
    classes: Vec<String>,
    id: Option<String>,
    /// 1-based position among element siblings.
    child_index: i32,
    /// Total siblings (including self).
    sibling_count: i32,
}

impl ElementRef {
    fn from_element(el: &Element, child_index: i32, sibling_count: i32) -> Self {
        Self {
            tag: el.tag.clone(),
            classes: el.attrs.classes.clone(),
            id: el.attrs.id.clone(),
            child_index,
            sibling_count,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_to_element(
    el: &mut Element,
    css: &Stylesheet,
    media: &MediaContext,
    inherited_vars: &std::collections::HashMap<String, String>,
    inherited_text: &InheritedText,
    parents: &[ElementRef],
    child_index: i32,
    sibling_count: i32,
    warnings: &mut Vec<CssWarning>,
) {
    let me = ElementRef::from_element(el, child_index, sibling_count);
    let is_root = parents.is_empty();
    let has_element_children = !el.children.is_empty();

    // Snapshot pre-CSS (inline-set) attrs so origin precedence still
    // beats user CSS on a per-field basis.
    let inline_snapshot = el.attrs.clone();

    // Inheritance base (CSS-inherited text props): apply the parent's
    // computed value as a specificity-0 base, only where this element
    // has no inline value yet. Any matching rule (applied below) or the
    // inline origin (restored below) overrides it.
    inherited_text.apply_base(&mut el.attrs);

    // Collect matching rules, cascade-sorted (ascending - later wins).
    let matched = collect_matching_rules(
        css,
        media,
        &me,
        parents,
        has_element_children,
        el.attrs.text.as_deref(),
        is_root,
    );

    // Var pass: cascade-ordered, per-declaration importance, last-wins.
    // Custom-property declarations are ordered by their own `!important`
    // flag (a stable sort floats important decls after normal ones), so
    // an `!important` var beats a normal one at equal position.
    let mut vars = inherited_vars.clone();
    {
        let mut var_units: Vec<&Declaration> = Vec::new();
        for m in &matched {
            for decl in &css.rules[m.rule_idx].declarations {
                if decl.name.starts_with("--") {
                    var_units.push(decl);
                }
            }
        }
        var_units.sort_by_key(|d| d.important);
        for decl in var_units {
            if let Some(name) = decl.name.strip_prefix("--") {
                vars.insert(name.to_string(), decl.value.clone());
            }
        }
    }

    // Apply pass: per-declaration cascade. CSS Cascade section 6.4 makes
    // `!important` participate at the DECLARATION level, not the rule
    // level - a normal declaration sitting next to an important sibling
    // must NOT be promoted with it. We flatten (rule, decl) units in
    // cascade order (specificity, source_order, selector_idx) then
    // STABLE-sort by importance so all `!important` decls float to the
    // end (last-wins) while normal decls keep their relative order.
    let mut units: Vec<(&MatchedRule, &Declaration)> = Vec::new();
    for m in &matched {
        for decl in &css.rules[m.rule_idx].declarations {
            if decl.name.starts_with("--") {
                continue;
            }
            units.push((m, decl));
        }
    }
    units.sort_by_key(|(_, decl)| decl.important);

    // A bad declaration is skipped with a warning - never fatal.
    for (m, decl) in units {
        let ctx = describe_selector_for_error(&css.rules[m.rule_idx].selectors, m.selector_idx);
        let resolved = match resolve_vars(&ctx, &decl.name, &decl.value, &vars) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(CssWarning {
                    selector: ctx.clone(),
                    property: decl.name.clone(),
                    message: e.to_string(),
                });
                continue;
            }
        };
        match apply_decl_for_pseudo(
            &ctx,
            &decl.name,
            &resolved,
            &m.matched_pseudo,
            &mut el.attrs,
        ) {
            Ok(true) => {}
            Ok(false) => warnings.push(CssWarning {
                selector: ctx.clone(),
                property: decl.name.clone(),
                message: "unknown property (declaration ignored)".to_string(),
            }),
            Err(e) => warnings.push(CssWarning {
                selector: ctx.clone(),
                property: decl.name.clone(),
                message: e.to_string(),
            }),
        }
    }

    // Origin precedence: inline > CSS. Restore any field the inline
    // pass had populated.
    restore_inline_origin(&mut el.attrs, &inline_snapshot);

    // Tooltip skin tokens: `<tooltip>` collapses onto its trigger at
    // parse time, so no selector can reach it - the dwell / cursor-gap
    // metrics are instead routed through the `--lumen-tooltip-delay` /
    // `--lumen-tooltip-offset` custom properties (declared in the
    // built-in skins' `:root`, overridable by any app `:root` block).
    // Inline `delay=` / `offset=` attrs parsed as `Some` and win; the
    // runtime default (500 ms / 12 px) is the single Rust-side
    // fallback when neither the author nor a skin supplied a value.
    if let Some(tooltip) = el.attrs.tooltip.as_mut() {
        if tooltip.delay_ms.is_none() {
            tooltip.delay_ms = vars
                .get("lumen-tooltip-delay")
                .and_then(|v| v.trim().parse::<u32>().ok());
        }
        if tooltip.offset.is_none() {
            tooltip.offset = vars
                .get("lumen-tooltip-offset")
                .and_then(|v| v.trim().parse::<f32>().ok());
        }
    }

    // Compute the inherited text scope for children from this element's
    // now-resolved values (which already fold in the inherited base).
    let child_text = InheritedText::from_computed(&el.attrs, inherited_text);

    // Recurse into children with the up-to-date var + text scope.
    let mut new_parents: Vec<ElementRef> = parents.to_vec();
    new_parents.push(me);
    let n = el.children.len() as i32;
    for (i, child) in el.children.iter_mut().enumerate() {
        apply_to_element(
            child,
            css,
            media,
            &vars,
            &child_text,
            &new_parents,
            (i as i32) + 1,
            n,
            warnings,
        );
    }
}

/// CSS-inherited text properties carried down the element tree. Per the
/// CSS cascade, `color`/`font-size`/`text-align`/`text-wrap`/`max-lines`
/// inherit from parent to child; custom properties inherit separately
/// via the var map. Only slots that [`Attributes`] actually supports are
/// modelled (no `line-height`/`font-family` slot exists yet).
#[derive(Debug, Clone, Default)]
struct InheritedText {
    text_color: Option<Rgba>,
    selection_color: Option<Rgba>,
    caret_color: Option<Rgba>,
    selection_text_color: Option<Rgba>,
    font_size: Option<f32>,
    font_family: Option<String>,
    font_weight: Option<u16>,
    text_align: Option<TextAlignSpec>,
    text_wrap: Option<TextWrapSpec>,
    max_lines: Option<u32>,
}

impl InheritedText {
    /// Write inherited values as a specificity-0 base - only into fields
    /// the element hasn't already set inline. Matching rules (applied
    /// afterwards) and the inline-origin restore both override this.
    fn apply_base(&self, attrs: &mut Attributes) {
        if attrs.text_color.is_none() {
            attrs.text_color = self.text_color;
        }
        if attrs.selection_color.is_none() {
            attrs.selection_color = self.selection_color;
        }
        if attrs.caret_color.is_none() {
            attrs.caret_color = self.caret_color;
        }
        if attrs.selection_text_color.is_none() {
            attrs.selection_text_color = self.selection_text_color;
        }
        if attrs.font_size.is_none() {
            attrs.font_size = self.font_size;
        }
        if attrs.font_family.is_none() {
            attrs.font_family = self.font_family.clone();
        }
        if attrs.font_weight.is_none() {
            attrs.font_weight = self.font_weight;
        }
        if attrs.text_align.is_none() {
            attrs.text_align = self.text_align;
        }
        if attrs.text_wrap.is_none() {
            attrs.text_wrap = self.text_wrap;
        }
        if attrs.max_lines.is_none() {
            attrs.max_lines = self.max_lines;
        }
    }

    /// The inherited scope handed to children: the element's computed
    /// value for each prop, falling back to what this element itself
    /// inherited when it set nothing.
    fn from_computed(attrs: &Attributes, parent: &InheritedText) -> Self {
        Self {
            text_color: attrs.text_color.or(parent.text_color),
            selection_color: attrs.selection_color.or(parent.selection_color),
            caret_color: attrs.caret_color.or(parent.caret_color),
            selection_text_color: attrs.selection_text_color.or(parent.selection_text_color),
            font_size: attrs.font_size.or(parent.font_size),
            font_family: attrs
                .font_family
                .clone()
                .or_else(|| parent.font_family.clone()),
            font_weight: attrs.font_weight.or(parent.font_weight),
            text_align: attrs.text_align.or(parent.text_align),
            text_wrap: attrs.text_wrap.or(parent.text_wrap),
            max_lines: attrs.max_lines.or(parent.max_lines),
        }
    }
}

#[derive(Debug, Clone)]
struct MatchedRule {
    rule_idx: usize,
    selector_idx: usize,
    origin: Origin,
    source_order: usize,
    specificity: Specificity,
    matched_pseudo: SubjectPseudo,
}

#[derive(Debug, Clone, Default)]
struct SubjectPseudo {
    hover: bool,
    focus: bool,
    focus_visible: bool,
    active: bool,
    disabled: bool,
    checked: bool,
    selected: bool,
    drag_over: bool,
}

impl SubjectPseudo {
    fn any(&self) -> bool {
        self.hover
            || self.focus
            || self.focus_visible
            || self.active
            || self.disabled
            || self.checked
            || self.selected
            || self.drag_over
    }
}

fn collect_matching_rules(
    css: &Stylesheet,
    media: &MediaContext,
    me: &ElementRef,
    parents: &[ElementRef],
    has_element_children: bool,
    text_body: Option<&str>,
    is_root: bool,
) -> Vec<MatchedRule> {
    let mut matched: Vec<MatchedRule> = Vec::new();
    for (rule_idx, rule) in css.rules.iter().enumerate() {
        if let Some(mq) = &rule.media {
            if !mq.matches(media) {
                continue;
            }
        }
        for (sel_idx, sel) in rule.selectors.iter().enumerate() {
            if let Some(subject) =
                match_selector(sel, me, parents, has_element_children, text_body, is_root)
            {
                matched.push(MatchedRule {
                    rule_idx,
                    selector_idx: sel_idx,
                    origin: rule.origin,
                    source_order: rule.source_order,
                    specificity: sel.specificity(),
                    matched_pseudo: subject,
                });
            }
        }
    }
    // Position order: (origin, specificity, source_order, selector_idx),
    // ascending, so the last write wins. Origin dominates - a
    // user-agent (skin) rule loses to an author rule regardless of
    // specificity, per CSS Cascade section 6.1 (so an author `.editor` beats a
    // skin `textarea:hover`). Importance is NOT folded in here - per
    // CSS Cascade section 6.4 it participates at the DECLARATION level, and the
    // apply/var passes stable-sort each declaration by its own
    // `!important` flag on top of this base ordering.
    matched.sort_by(|a, b| {
        a.origin
            .cmp(&b.origin)
            .then_with(|| a.specificity.cmp(&b.specificity))
            .then_with(|| a.source_order.cmp(&b.source_order))
            .then_with(|| a.selector_idx.cmp(&b.selector_idx))
    });
    matched
}

/// Try to match `sel` against `me` (the subject element), consulting
/// `parents` (outer-first) for ancestor matching. Returns the subject
/// compound's pseudo classification.
fn match_selector(
    sel: &SelectorBuf,
    me: &ElementRef,
    parents: &[ElementRef],
    has_element_children: bool,
    text_body: Option<&str>,
    is_root: bool,
) -> Option<SubjectPseudo> {
    let last = sel.chain.last()?;
    let subject_compound = &last.1;
    let subject_pseudo = extract_subject_pseudo(subject_compound);
    if !match_compound(
        subject_compound,
        me,
        has_element_children,
        text_body,
        is_root,
    ) {
        return None;
    }
    // Walk leftwards through the chain. `anc_cursor` is the index of
    // the next ancestor to try (counting from the IMMEDIATE parent =
    // parents.len() - 1 and going up).
    let mut anc_cursor: isize = parents.len() as isize - 1;
    for i in (0..sel.chain.len().saturating_sub(1)).rev() {
        // The combinator that linked the *previous* compound on the
        // right side (sel.chain[i+1]) to this compound on the left
        // (sel.chain[i]) is stored on sel.chain[i+1].0.
        let link = sel.chain[i + 1].0;
        let compound = &sel.chain[i].1;
        match link {
            Combinator::Subject => unreachable!("only leftmost is Subject"),
            Combinator::Descendant => {
                let mut found = false;
                while anc_cursor >= 0 {
                    let a = &parents[anc_cursor as usize];
                    let a_is_root = anc_cursor == 0;
                    anc_cursor -= 1;
                    if match_compound(compound, a, true, None, a_is_root) {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return None;
                }
            }
            Combinator::Child => {
                if anc_cursor < 0 {
                    return None;
                }
                let a = &parents[anc_cursor as usize];
                let a_is_root = anc_cursor == 0;
                anc_cursor -= 1;
                if !match_compound(compound, a, true, None, a_is_root) {
                    return None;
                }
            }
            Combinator::AdjacentSibling | Combinator::GeneralSibling => {
                // v1 limitation: sibling combinators require the
                // sibling's class/tag info, which isn't in our
                // ancestor-only context. Fail conservatively so we
                // don't silently mis-apply rules. Tracked in
                // theming.md follow-ups.
                let _ = compound;
                return None;
            }
        }
    }
    Some(subject_pseudo)
}

fn extract_subject_pseudo(c: &CompoundSelector) -> SubjectPseudo {
    let mut sp = SubjectPseudo::default();
    for p in &c.pseudo_classes {
        match p {
            PseudoClass::Hover => sp.hover = true,
            PseudoClass::Focus => sp.focus = true,
            PseudoClass::FocusVisible => sp.focus_visible = true,
            PseudoClass::Active => sp.active = true,
            PseudoClass::Disabled => sp.disabled = true,
            PseudoClass::Checked => sp.checked = true,
            PseudoClass::Selected => sp.selected = true,
            PseudoClass::DragOver => sp.drag_over = true,
            _ => {}
        }
    }
    sp
}

fn match_compound(
    compound: &CompoundSelector,
    el: &ElementRef,
    has_element_children: bool,
    text_body: Option<&str>,
    is_root: bool,
) -> bool {
    if let Some(t) = &compound.tag {
        if t != &el.tag {
            return false;
        }
    }
    if let Some(id) = &compound.id {
        if el.id.as_deref() != Some(id.as_str()) {
            return false;
        }
    }
    for c in &compound.classes {
        if !el.classes.iter().any(|x| x == c) {
            return false;
        }
    }
    for p in &compound.pseudo_classes {
        if !matches_pseudo(p, el, has_element_children, text_body, is_root) {
            return false;
        }
    }
    true
}

fn matches_pseudo(
    p: &PseudoClass,
    el: &ElementRef,
    has_element_children: bool,
    text_body: Option<&str>,
    is_root: bool,
) -> bool {
    match p {
        // State-conditional pseudos: matched as true at parse time;
        // the runtime ECS attaches the appropriate state component.
        // The subject side feeds `SubjectPseudo` for property routing.
        PseudoClass::Hover
        | PseudoClass::Focus
        | PseudoClass::FocusVisible
        | PseudoClass::Active
        | PseudoClass::Disabled
        | PseudoClass::Checked
        | PseudoClass::Selected
        | PseudoClass::DragOver => true,
        PseudoClass::Root => is_root || el.tag == "root",
        PseudoClass::FirstChild => el.child_index == 1,
        PseudoClass::LastChild => el.child_index == el.sibling_count,
        PseudoClass::OnlyChild => el.sibling_count == 1,
        PseudoClass::Empty => {
            let no_text = text_body.is_none_or(|s| s.trim().is_empty());
            !has_element_children && no_text
        }
        PseudoClass::NthChild(anb) => anb.matches(el.child_index),
        PseudoClass::Is(args) => args.iter().any(|s| {
            s.chain.len() == 1
                && match_compound(&s.chain[0].1, el, has_element_children, text_body, is_root)
        }),
        PseudoClass::Where(args) => args.iter().any(|s| {
            s.chain.len() == 1
                && match_compound(&s.chain[0].1, el, has_element_children, text_body, is_root)
        }),
        PseudoClass::Not(args) => !args.iter().any(|s| {
            s.chain.len() == 1
                && match_compound(&s.chain[0].1, el, has_element_children, text_body, is_root)
        }),
    }
}

// ---------------------------------------------------------------------------
// Public query surface (read side of the dynamic DOM API)
// ---------------------------------------------------------------------------

/// Parse a CSS selector list (`"#save, .row > .cell"`) into the same
/// [`SelectorBuf`] chain the stylesheet front-end builds. The runtime
/// query engine (`query(sel)`) calls this so a script selector runs
/// through the one selector grammar Lumen already speaks; there is no
/// second engine.
///
/// The grammar is the Selectors-4 subset the CSS parser accepts:
/// tag / `#id` / `.class` / pseudo-classes, descendant / `>` / `+` / `~`
/// combinators, and `:is()` / `:where()` / `:not()` / `:nth-child()`
/// nesting. Errors return a message string.
pub fn parse_selector_list(src: &str) -> Result<Vec<SelectorBuf>, String> {
    sel_parse_list(src, 0)
}

/// Test whether `sel` matches `subject`, consulting `ancestors`
/// (root-first, the same order [`reapply_with_ancestors`] expects) for
/// descendant / child combinators. Wraps the cascade matcher
/// ([`match_selector`]) so `query()` and traversal share the exact match
/// semantics of the live cascade.
///
/// Sibling combinators (`+`, `~`) fail conservatively here, the same as
/// in the cascade: the matcher has no sibling identity in an ancestor-only
/// context, so a selector that ends in a sibling step never matches. Query
/// inherits that limitation until sibling context is threaded through.
///
/// `has_element_children` is assumed true and inline text is assumed
/// absent for the subject, matching the ancestor-pass convention; `:empty`
/// therefore matches conservatively.
pub fn selector_matches(
    sel: &SelectorBuf,
    subject: &AncestorInfo,
    ancestors: &[AncestorInfo],
) -> bool {
    let me = subject.to_ref();
    let parents: Vec<ElementRef> = ancestors.iter().map(AncestorInfo::to_ref).collect();
    let is_root = ancestors.is_empty();
    match_selector(sel, &me, &parents, true, None, is_root).is_some()
}

/// Selector-nesting depth cap for `:is()` / `:where()` / `:not()`,
/// mirroring the CSS parser's own guard.
const SELECTOR_MAX_NEST_DEPTH: u32 = 8;

fn sel_parse_list(src: &str, depth: u32) -> Result<Vec<SelectorBuf>, String> {
    if depth > SELECTOR_MAX_NEST_DEPTH {
        return Err(format!(
            "css: selector nesting (:not/:is/:where) exceeded depth {SELECTOR_MAX_NEST_DEPTH}"
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
        out.push(sel_parse_one(p, depth)?);
    }
    Ok(out)
}

fn sel_parse_one(src: &str, depth: u32) -> Result<SelectorBuf, String> {
    let tokens = sel_tokenize(src)?;
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
        let (compound, consumed) = sel_read_compound(&tokens[i..], depth)?;
        if compound.tag.is_none()
            && compound.id.is_none()
            && compound.classes.is_empty()
            && compound.pseudo_classes.is_empty()
        {
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

fn sel_tokenize(src: &str) -> Result<Vec<SelTok>, String> {
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
                let s = sel_read_ident(bytes, &mut i)?;
                out.push(SelTok::Class(s));
            }
            b'#' => {
                i += 1;
                let s = sel_read_ident(bytes, &mut i)?;
                out.push(SelTok::Id(s));
            }
            b':' => {
                i += 1;
                if i < bytes.len() && bytes[i] == b':' {
                    return Err(format!(
                        "css: pseudo-elements not supported (got '::') in '{src}'"
                    ));
                }
                let name = sel_read_ident(bytes, &mut i)?;
                let args = if i < bytes.len() && bytes[i] == b'(' {
                    let close = sel_find_matching_paren(&src[i..])
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
            _ if sel_is_ident_start(c) => {
                let s = sel_read_ident(bytes, &mut i)?;
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

fn sel_is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'-'
}

fn sel_is_ident_cont(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

fn sel_read_ident(bytes: &[u8], i: &mut usize) -> Result<String, String> {
    let start = *i;
    while *i < bytes.len() && sel_is_ident_cont(bytes[*i]) {
        *i += 1;
    }
    if *i == start {
        return Err(format!("css: expected identifier at byte {start}"));
    }
    Ok(std::str::from_utf8(&bytes[start..*i])
        .map_err(|e| format!("css: non-utf8 selector: {e}"))?
        .to_string())
}

fn sel_find_matching_paren(s: &str) -> Option<usize> {
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

fn sel_read_compound(toks: &[SelTok], depth: u32) -> Result<(CompoundSelector, usize), String> {
    let mut out = CompoundSelector::default();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            SelTok::Tag(t) => {
                if out.tag.is_some() || !out.classes.is_empty() || out.id.is_some() {
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
                let p = sel_parse_pseudo(name, args.as_deref(), depth)?;
                out.pseudo_classes.push(p);
                i += 1;
            }
            _ => break,
        }
    }
    Ok((out, i))
}

fn sel_parse_pseudo(name: &str, args: Option<&str>, depth: u32) -> Result<PseudoClass, String> {
    match name {
        "hover" => sel_no_args(name, args).map(|_| PseudoClass::Hover),
        "focus" => sel_no_args(name, args).map(|_| PseudoClass::Focus),
        "focus-visible" => sel_no_args(name, args).map(|_| PseudoClass::FocusVisible),
        "active" => sel_no_args(name, args).map(|_| PseudoClass::Active),
        "disabled" => sel_no_args(name, args).map(|_| PseudoClass::Disabled),
        "checked" => sel_no_args(name, args).map(|_| PseudoClass::Checked),
        "selected" => sel_no_args(name, args).map(|_| PseudoClass::Selected),
        "drag-over" => sel_no_args(name, args).map(|_| PseudoClass::DragOver),
        "root" => sel_no_args(name, args).map(|_| PseudoClass::Root),
        "first-child" => sel_no_args(name, args).map(|_| PseudoClass::FirstChild),
        "last-child" => sel_no_args(name, args).map(|_| PseudoClass::LastChild),
        "only-child" => sel_no_args(name, args).map(|_| PseudoClass::OnlyChild),
        "empty" => sel_no_args(name, args).map(|_| PseudoClass::Empty),
        "nth-child" => {
            let a = args.ok_or_else(|| "css: :nth-child requires '(an+b)'".to_string())?;
            Ok(PseudoClass::NthChild(sel_parse_anb(a)?))
        }
        "is" => {
            let a = args.ok_or_else(|| "css: :is requires '(selector-list)'".to_string())?;
            Ok(PseudoClass::Is(sel_parse_list(a, depth + 1)?))
        }
        "where" => {
            let a = args.ok_or_else(|| "css: :where requires '(selector-list)'".to_string())?;
            Ok(PseudoClass::Where(sel_parse_list(a, depth + 1)?))
        }
        "not" => {
            let a = args.ok_or_else(|| "css: :not requires '(selector-list)'".to_string())?;
            Ok(PseudoClass::Not(sel_parse_list(a, depth + 1)?))
        }
        other => Err(format!(
            "css: unknown pseudo-class ':{other}' (supported: :hover, :focus, :focus-visible, :active, :disabled, :checked, :selected, :drag-over, :root, :first-child, :last-child, :only-child, :empty, :nth-child, :is, :where, :not)"
        )),
    }
}

fn sel_no_args(name: &str, args: Option<&str>) -> Result<(), String> {
    if args.is_some() {
        Err(format!("css: ':{name}' takes no arguments"))
    } else {
        Ok(())
    }
}

fn sel_parse_anb(src: &str) -> Result<AnB, String> {
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

/// Inline-set fields (origin: inline) beat CSS. Restore any field the
/// inline snapshot had populated.
fn restore_inline_origin(target: &mut Attributes, inline: &Attributes) {
    if inline.width.is_some() {
        target.width = inline.width;
    }
    if inline.height.is_some() {
        target.height = inline.height;
    }
    if inline.bg.is_some() {
        target.bg = inline.bg.clone();
    }
    if inline.radius.is_some() {
        target.radius = inline.radius;
    }
    if inline.padding.is_some() {
        target.padding = inline.padding;
    }
    if inline.margin.is_some() {
        target.margin = inline.margin;
    }
    if inline.text_color.is_some() {
        target.text_color = inline.text_color;
    }
    if inline.selection_color.is_some() {
        target.selection_color = inline.selection_color;
    }
    if inline.caret_color.is_some() {
        target.caret_color = inline.caret_color;
    }
    if inline.selection_text_color.is_some() {
        target.selection_text_color = inline.selection_text_color;
    }
    if inline.hover_bg.is_some() {
        target.hover_bg = inline.hover_bg;
    }
    if inline.scroll.is_some() {
        target.scroll = inline.scroll;
    }
    if inline.sensitivity.is_some() {
        target.sensitivity = inline.sensitivity;
    }
    if inline.inertia.is_some() {
        target.inertia = inline.inertia;
    }
    if inline.tab_index.is_some() {
        target.tab_index = inline.tab_index;
    }
    if inline.press_bg.is_some() {
        target.press_bg = inline.press_bg;
    }
    if inline.font_size.is_some() {
        target.font_size = inline.font_size;
    }
    if inline.font_family.is_some() {
        target.font_family = inline.font_family.clone();
    }
    if inline.font_weight.is_some() {
        target.font_weight = inline.font_weight;
    }
    if inline.knob_color.is_some() {
        target.knob_color = inline.knob_color;
    }
    if inline.radius_corners.is_some() {
        target.radius_corners = inline.radius_corners;
    }
    if inline.gap.is_some() {
        target.gap = inline.gap;
    }
    if inline.gap_row.is_some() {
        target.gap_row = inline.gap_row;
    }
    if inline.gap_column.is_some() {
        target.gap_column = inline.gap_column;
    }
    if inline.display.is_some() {
        target.display = inline.display;
    }
    if inline.grid_template.is_some() {
        target.grid_template = inline.grid_template.clone();
    }
    if inline.grid_row.is_some() {
        target.grid_row = inline.grid_row;
    }
    if inline.grid_column.is_some() {
        target.grid_column = inline.grid_column;
    }
    if inline.align_self.is_some() {
        target.align_self = inline.align_self;
    }
    if inline.justify_items.is_some() {
        target.justify_items = inline.justify_items;
    }
    if inline.justify_self.is_some() {
        target.justify_self = inline.justify_self;
    }
    if inline.grow.is_some() {
        target.grow = inline.grow;
    }
    if inline.align.is_some() {
        target.align = inline.align;
    }
    if inline.justify.is_some() {
        target.justify = inline.justify;
    }
    if inline.text_align.is_some() {
        target.text_align = inline.text_align;
    }
    if inline.focus_outline.is_some() {
        target.focus_outline = inline.focus_outline;
    }
    if inline.border_width.is_some() {
        target.border_width = inline.border_width;
    }
    if inline.border_color.is_some() {
        target.border_color = inline.border_color;
    }
    if inline.border_style.is_some() {
        target.border_style = inline.border_style;
    }
    if inline.box_sizing.is_some() {
        target.box_sizing = inline.box_sizing;
    }
    if inline.hover_border.is_some() {
        target.hover_border = inline.hover_border;
    }
    if inline.focus_border.is_some() {
        target.focus_border = inline.focus_border;
    }
    if inline.shrink.is_some() {
        target.shrink = inline.shrink;
    }
    if inline.basis.is_some() {
        target.basis = inline.basis;
    }
    if inline.flex_wrap.is_some() {
        target.flex_wrap = inline.flex_wrap;
    }
    if inline.align_content.is_some() {
        target.align_content = inline.align_content;
    }
    if inline.z_index.is_some() {
        target.z_index = inline.z_index;
    }
    if inline.gap_pct.is_some() {
        target.gap_pct = inline.gap_pct;
    }
    if inline.gap_row_pct.is_some() {
        target.gap_row_pct = inline.gap_row_pct;
    }
    if inline.gap_column_pct.is_some() {
        target.gap_column_pct = inline.gap_column_pct;
    }
    if inline.text_wrap.is_some() {
        target.text_wrap = inline.text_wrap;
    }
    if inline.max_lines.is_some() {
        target.max_lines = inline.max_lines;
    }
    if inline.position.is_some() {
        target.position = inline.position;
    }
    if inline.inset.is_some() {
        target.inset = inline.inset;
    }
    if inline.min_width.is_some() {
        target.min_width = inline.min_width;
    }
    if inline.min_height.is_some() {
        target.min_height = inline.min_height;
    }
    if inline.max_width.is_some() {
        target.max_width = inline.max_width;
    }
    if inline.max_height.is_some() {
        target.max_height = inline.max_height;
    }
    if inline.aspect_ratio.is_some() {
        target.aspect_ratio = inline.aspect_ratio;
    }
    if inline.opacity.is_some() {
        target.opacity = inline.opacity;
    }
    if inline.image_fit.is_some() {
        target.image_fit = inline.image_fit;
    }
    if inline.style_role.is_some() {
        target.style_role = inline.style_role.clone();
    }
    if inline.draggable {
        target.draggable = true;
    }
    if !inline.shadows.is_empty() {
        target.shadows = inline.shadows.clone();
    }
    if !inline.transitions.is_empty() {
        target.transitions = inline.transitions.clone();
    }
    if inline.transition_property.is_some() {
        target.transition_property = inline.transition_property.clone();
    }
    if inline.transition_duration.is_some() {
        target.transition_duration = inline.transition_duration.clone();
    }
    if inline.transition_timing.is_some() {
        target.transition_timing = inline.transition_timing.clone();
    }
    if inline.scrollbar_color.is_some() {
        target.scrollbar_color = inline.scrollbar_color;
    }
    if inline.scrollbar_width.is_some() {
        target.scrollbar_width = inline.scrollbar_width;
    }
}

/// Re-apply CSS rules to a single substituted element with overwrite
/// semantics - used by the `<for>` reconciler against runtime-
/// substituted template elements where classes are computed from
/// per-row data. Resolves `@media` blocks against the default (empty)
/// [`MediaContext`]; use [`reapply_single_with_media`] to honour the
/// live OS theme / viewport.
pub fn reapply_single(el: &mut Element, css: &Stylesheet) -> Result<(), ParseError> {
    reapply_single_with_media(el, css, &MediaContext::default())
}

/// Same as [`reapply_single`] but resolves `@media` blocks against a
/// live [`MediaContext`]. Used by the runtime theme-flip re-resolver so
/// a `prefers-color-scheme` / viewport-width flip restyles already-
/// spawned entities without a respawn.
///
/// Copies back the cascade result for the visual + box props that a
/// theme token scope realistically flips: text (`font_size`,
/// `text_color`, `text_align`, `text_wrap`, `max_lines`, `style_role`),
/// box (`padding`, `margin`, `width`, `height`), paint (`bg`, `radius`,
/// `shadows`, `opacity`), and interaction tints (`hover_bg`,
/// `press_bg`). A property the cascade didn't set is left untouched, so
/// inline authoring values on non-flipped props survive.
pub fn reapply_single_with_media(
    el: &mut Element,
    css: &Stylesheet,
    media: &MediaContext,
) -> Result<(), ParseError> {
    // No ancestor context here - flatten every rule's `--*` into a
    // first-value-wins seed so a lone element's `var()` still resolves
    // against globals it can't reach through the (empty) chain. The
    // `<for>` reconciler is the caller.
    let mut inherited: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for rule in &css.rules {
        for decl in &rule.declarations {
            if let Some(name) = decl.name.strip_prefix("--") {
                inherited
                    .entry(name.to_string())
                    .or_insert_with(|| decl.value.clone());
            }
        }
    }
    reapply_probe(el, css, media, &[], &inherited, &InheritedText::default());
    Ok(())
}

/// Public identity of one ancestor in the cascade chain handed to
/// [`reapply_with_ancestors`]. Mirrors the internal `ElementRef` but is
/// part of the crate's public surface so the runtime can build a chain
/// from ECS `ChildOf` + `LumenTag` / `LumenClasses` / `LumenId`
/// components without reaching into cascade internals.
#[derive(Debug, Clone)]
pub struct AncestorInfo {
    /// Markup tag (`root`, `tile`, ...). Empty for a plain layout
    /// container that carries no `LumenTag`; such an ancestor then
    /// matches only tagless compound selectors, which is correct.
    pub tag: String,
    /// Class list assigned in markup via `class="..."`.
    pub classes: Vec<String>,
    /// Stable `id="..."`, if any.
    pub id: Option<String>,
    /// 1-based position among element siblings. Defaults to 1 when the
    /// caller can't cheaply supply it - `:first-child` / `:nth-child`
    /// on ancestors then match conservatively.
    pub child_index: i32,
    /// Total sibling count including self. Defaults to 1.
    pub sibling_count: i32,
}

impl AncestorInfo {
    /// Construct from tag / classes / id with sibling position defaulted
    /// to `1 of 1`. Use [`Self::with_position`] to thread real positions
    /// when they're cheaply available.
    pub fn new(tag: impl Into<String>, classes: Vec<String>, id: Option<String>) -> Self {
        Self {
            tag: tag.into(),
            classes,
            id,
            child_index: 1,
            sibling_count: 1,
        }
    }

    /// Builder: set the 1-based sibling position, for `:nth-child` /
    /// `:first-child` ancestor matching.
    pub fn with_position(mut self, child_index: i32, sibling_count: i32) -> Self {
        self.child_index = child_index;
        self.sibling_count = sibling_count;
        self
    }

    fn to_ref(&self) -> ElementRef {
        ElementRef {
            tag: self.tag.clone(),
            classes: self.classes.clone(),
            id: self.id.clone(),
            child_index: self.child_index,
            sibling_count: self.sibling_count,
        }
    }
}

/// Ancestor-aware sibling of [`reapply_single_with_media`]. Re-runs the
/// cascade for one already-spawned element against a real ancestor chain
/// (root-first ordering), so a runtime theme flip re-resolves the cases
/// the empty-parent path can't:
///
/// * descendant / child combinators - `.theme-dark .card` matches only
///   when a `.theme-dark` ancestor is actually present, and `parent >
///   child` binds to the *immediate* parent only, and
/// * per-theme custom-property scopes toggled by an ancestor class -
///   `:root.theme-dark { --bg: ... }` consumed by `.card { bg: var(--bg) }`
///   re-resolves on the descendant because the ancestor's `--*` scope is
///   walked into the element's `var()` seed.
///
/// The var seed is computed by [`ancestor_var_scope`] (root vars first,
/// each ancestor overriding earlier ones in cascade order); the element's
/// own matched rules then fold their `--*` on top inside
/// [`apply_to_element`]. Copy-back uses the same whitelist as
/// [`reapply_single_with_media`].
pub fn reapply_with_ancestors(
    el: &mut Element,
    css: &Stylesheet,
    media: &MediaContext,
    ancestors: &[AncestorInfo],
) -> Result<(), ParseError> {
    let parents: Vec<ElementRef> = ancestors.iter().map(AncestorInfo::to_ref).collect();
    let (inherited, inherited_text) = ancestor_var_scope(css, media, &parents);
    reapply_probe(el, css, media, &parents, &inherited, &inherited_text);
    Ok(())
}

/// Apply one inline-style declaration (`element.style` layer) onto
/// `attrs`, overriding whatever the stylesheet cascade resolved. This is
/// the highest cascade tier for the dynamic DOM `set_style` surface; the
/// runtime calls it after [`reapply_with_ancestors`] so an inline value
/// beats every author / UA rule, mirroring DOM inline-style precedence.
/// Returns `Ok(true)` when the property was recognized and applied,
/// `Ok(false)` for an unknown property (ignored), `Err` for an
/// unparseable value.
pub fn apply_inline_declaration(
    name: &str,
    value: &str,
    attrs: &mut Attributes,
) -> Result<bool, ParseError> {
    apply_declaration("inline-style", name, value, attrs)
}

/// Read one resolved CSS property off `attrs` as a canonical string, for
/// the dynamic DOM `computed_style("prop")` getter. Covers the common
/// layout / paint / text properties; an unmodeled property returns `None`.
/// Colors render as `#rrggbb` / `#rrggbbaa`, lengths as `Npx` / `N%` /
/// `auto`, scalars as their decimal form.
pub fn computed_property(attrs: &Attributes, name: &str) -> Option<String> {
    fn hex(c: &Rgba) -> String {
        let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let (r, g, b, a) = (to(c.r), to(c.g), to(c.b), to(c.a));
        if a == 0xff {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }
    fn len(l: &LengthSpec) -> String {
        match l {
            LengthSpec::Auto => "auto".to_string(),
            LengthSpec::Px(v) => format!("{v}px"),
            LengthSpec::Percent(v) => format!("{v}%"),
        }
    }
    fn edges(e: &Edges) -> String {
        format!("{}px {}px {}px {}px", e.top, e.right, e.bottom, e.left)
    }
    match canonical_property_name(name) {
        "text-color" => attrs.text_color.as_ref().map(hex),
        "selection-color" => attrs.selection_color.as_ref().map(hex),
        "caret-color" => attrs.caret_color.as_ref().map(hex),
        "bg" => attrs.bg.as_ref().and_then(|b| match b {
            BgSpec::Solid(c) => Some(hex(c)),
            _ => None,
        }),
        "width" => attrs.width.as_ref().map(len),
        "height" => attrs.height.as_ref().map(len),
        "padding" => attrs.padding.as_ref().map(edges),
        "margin" => attrs.margin.as_ref().map(edges),
        "font-size" => attrs.font_size.map(|v| format!("{v}px")),
        "font-weight" => attrs.font_weight.map(|v| v.to_string()),
        "font-family" => attrs.font_family.clone(),
        "text-align" => attrs.text_align.map(|a| match a {
            TextAlignSpec::Start => "start".to_string(),
            TextAlignSpec::Center => "center".to_string(),
            TextAlignSpec::End => "end".to_string(),
        }),
        "opacity" => attrs.opacity.map(|v| v.to_string()),
        "radius" => attrs.radius.map(|v| format!("{v}px")),
        _ => None,
    }
}

/// Property names [`computed_style_map`] enumerates for the full-map
/// `computed_style()` getter. Covers the layout / paint / text properties
/// the cascade models; unmodeled CSS properties are absent from the map.
pub const COMPUTED_STYLE_PROPERTIES: &[&str] = &[
    "width",
    "height",
    "padding",
    "margin",
    "bg",
    "text-color",
    "selection-color",
    "caret-color",
    "font-size",
    "font-weight",
    "font-family",
    "text-align",
    "opacity",
    "radius",
];

/// Every resolved property in [`COMPUTED_STYLE_PROPERTIES`] that the
/// cascade set on `attrs`, as `(name, value)` pairs -- the full-map form
/// of [`computed_property`] backing the dynamic DOM `computed_style()`
/// getter. An inspection call, not a per-frame path.
pub fn computed_style_map(attrs: &Attributes) -> Vec<(String, String)> {
    COMPUTED_STYLE_PROPERTIES
        .iter()
        .filter_map(|&p| computed_property(attrs, p).map(|v| (p.to_string(), v)))
        .collect()
}

/// Serialize a compiled [`SelectorBuf`] back to CSS-ish text for the
/// dynamic DOM `matched_rules()` provenance view. Reconstructs
/// `tag#id.class:pseudo` compounds joined by their combinators. Pseudo
/// arguments (`:nth-child(2n)`, `:is(...)`) render their canonical form.
pub fn selector_to_css(sel: &SelectorBuf) -> String {
    fn pseudo(p: &PseudoClass) -> String {
        match p {
            PseudoClass::Hover => ":hover".into(),
            PseudoClass::Focus => ":focus".into(),
            PseudoClass::FocusVisible => ":focus-visible".into(),
            PseudoClass::Active => ":active".into(),
            PseudoClass::Disabled => ":disabled".into(),
            PseudoClass::Checked => ":checked".into(),
            PseudoClass::Selected => ":selected".into(),
            PseudoClass::DragOver => ":drag-over".into(),
            PseudoClass::Root => ":root".into(),
            PseudoClass::FirstChild => ":first-child".into(),
            PseudoClass::LastChild => ":last-child".into(),
            PseudoClass::OnlyChild => ":only-child".into(),
            PseudoClass::Empty => ":empty".into(),
            PseudoClass::NthChild(anb) => format!(":nth-child({}n+{})", anb.a, anb.b),
            PseudoClass::Is(args) => format!(":is({})", join_selectors(args)),
            PseudoClass::Where(args) => format!(":where({})", join_selectors(args)),
            PseudoClass::Not(args) => format!(":not({})", join_selectors(args)),
        }
    }
    fn compound(c: &CompoundSelector) -> String {
        let mut s = String::new();
        if let Some(tag) = &c.tag {
            s.push_str(tag);
        } else if c.id.is_none() && c.classes.is_empty() && c.pseudo_classes.is_empty() {
            s.push('*');
        }
        if let Some(id) = &c.id {
            s.push('#');
            s.push_str(id);
        }
        for class in &c.classes {
            s.push('.');
            s.push_str(class);
        }
        for p in &c.pseudo_classes {
            s.push_str(&pseudo(p));
        }
        s
    }
    let mut out = String::new();
    for (comb, comp) in &sel.chain {
        match comb {
            Combinator::Subject => {}
            Combinator::Descendant => out.push(' '),
            Combinator::Child => out.push_str(" > "),
            Combinator::AdjacentSibling => out.push_str(" + "),
            Combinator::GeneralSibling => out.push_str(" ~ "),
        }
        out.push_str(&compound(comp));
    }
    out
}

fn join_selectors(sels: &[SelectorBuf]) -> String {
    sels.iter()
        .map(selector_to_css)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One stylesheet rule that matched an element, with its cascade
/// provenance, for the dynamic DOM `matched_rules()` inspection getter.
#[derive(Debug, Clone)]
pub struct MatchedRuleInfo {
    /// The specific selector (of the rule's selector list) that matched,
    /// serialized back to CSS text.
    pub selector: String,
    /// Selectors-4 specificity of the matched selector.
    pub specificity: Specificity,
    /// Cascade origin (user-agent skin vs author sheet).
    pub origin: Origin,
    /// Source order of the rule within the stylesheet.
    pub source_order: usize,
    /// The rule's declarations as `(property, value)` pairs (raw,
    /// var()-unresolved), in source order.
    pub declarations: Vec<(String, String)>,
}

/// Rules that match `subject` given its `ancestors` (root-first), with
/// cascade provenance, ascending in cascade order (last wins). Reuses the
/// cascade matcher's own `collect_matching_rules`, so this is the same
/// match set the restyle pass resolves -- no second selector engine. An
/// inspection call backing `matched_rules()`.
pub fn matched_rules_for(
    subject: &AncestorInfo,
    css: &Stylesheet,
    media: &MediaContext,
    ancestors: &[AncestorInfo],
    has_element_children: bool,
    text_body: Option<&str>,
) -> Vec<MatchedRuleInfo> {
    let me = subject.to_ref();
    let parents: Vec<ElementRef> = ancestors.iter().map(AncestorInfo::to_ref).collect();
    let is_root = ancestors.is_empty();
    let matched = collect_matching_rules(
        css,
        media,
        &me,
        &parents,
        has_element_children,
        text_body,
        is_root,
    );
    matched
        .into_iter()
        .map(|m| {
            let rule = &css.rules[m.rule_idx];
            MatchedRuleInfo {
                selector: selector_to_css(&rule.selectors[m.selector_idx]),
                specificity: m.specificity,
                origin: m.origin,
                source_order: m.source_order,
                declarations: rule
                    .declarations
                    .iter()
                    .map(|d| (d.name.clone(), d.value.clone()))
                    .collect(),
            }
        })
        .collect()
}

/// Accumulate the custom-property (`--*`) scope contributed by an
/// ancestor chain, walking root-first. For each ancestor we collect the
/// var declarations of every rule that matches it (given that ancestor's
/// own parents), in cascade order, so a later / more-specific /
/// `!important` ancestor declaration overrides an earlier one. The
/// result seeds the subject element's cascade, so a var scope gated on an
/// ancestor class (`:root.theme-dark { --bg }`) is visible to a
/// descendant's `var(--bg)`.
fn ancestor_var_scope(
    css: &Stylesheet,
    media: &MediaContext,
    ancestors: &[ElementRef],
) -> (std::collections::HashMap<String, String>, InheritedText) {
    let mut vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut text = InheritedText::default();
    for (k, anc) in ancestors.iter().enumerate() {
        let anc_parents = &ancestors[..k];
        let is_root = k == 0;
        // Ancestors always have at least the subject below them, so
        // `has_element_children` is true; they never carry inline text.
        let matched = collect_matching_rules(css, media, anc, anc_parents, true, None, is_root);
        let mut var_units: Vec<&Declaration> = Vec::new();
        for m in &matched {
            for decl in &css.rules[m.rule_idx].declarations {
                if decl.name.starts_with("--") {
                    var_units.push(decl);
                }
            }
        }
        var_units.sort_by_key(|d| d.important);
        for decl in var_units {
            if let Some(name) = decl.name.strip_prefix("--") {
                vars.insert(name.to_string(), decl.value.clone());
            }
        }
        // Accumulate the CSS-inherited text props this ancestor's own
        // matched rules compute, so a theme flip re-resolves e.g.
        // `root { text-color: var(--lumen-text) }` down to a bare
        // `<label>` that matches no rule of its own (mirrors the
        // parse-time InheritedText walk).
        let mut probe_attrs = Attributes {
            classes: anc.classes.clone(),
            id: anc.id.clone(),
            ..Default::default()
        };
        let mut units: Vec<(&MatchedRule, &Declaration)> = Vec::new();
        for m in &matched {
            for decl in &css.rules[m.rule_idx].declarations {
                if decl.name.starts_with("--") {
                    continue;
                }
                units.push((m, decl));
            }
        }
        units.sort_by_key(|(_, decl)| decl.important);
        for (m, decl) in units {
            if m.matched_pseudo.any() {
                continue;
            }
            let name = canonical_property_name(&decl.name);
            if !matches!(
                name,
                "text-color"
                    | "font-size"
                    | "font-family"
                    | "font-weight"
                    | "text-align"
                    | "wrap"
                    | "max-lines"
            ) {
                continue;
            }
            let Ok(resolved) = resolve_vars("ancestor", name, &decl.value, &vars) else {
                continue;
            };
            let _ = apply_declaration("ancestor", name, &resolved, &mut probe_attrs);
        }
        text = InheritedText::from_computed(&probe_attrs, &text);
    }
    (vars, text)
}

/// Shared body of the two runtime re-apply entry points: cascade a
/// probe (an attr-stripped clone carrying only the element's own
/// classes / id) against `parents` + a pre-seeded `inherited` var scope,
/// then copy the whitelisted result back onto `el`.
fn reapply_probe(
    el: &mut Element,
    css: &Stylesheet,
    media: &MediaContext,
    parents: &[ElementRef],
    inherited: &std::collections::HashMap<String, String>,
    inherited_text: &InheritedText,
) {
    let mut probe = el.clone();
    probe.attrs = Attributes {
        classes: el.attrs.classes.clone(),
        id: el.attrs.id.clone(),
        ..Default::default()
    };
    let mut warnings = Vec::new();
    apply_to_element(
        &mut probe,
        css,
        media,
        inherited,
        inherited_text,
        parents,
        1,
        1,
        &mut warnings,
    );
    copy_back_reapplied(el, &probe);
}

/// Copy the whitelisted cascade result from `probe` back onto `el`, only
/// where the cascade produced a value so inline authoring values on
/// non-flipped props survive. The whitelist is the visual + box + text +
/// interaction set a theme token scope realistically flips.
fn copy_back_reapplied(el: &mut Element, probe: &Element) {
    // Text.
    if probe.attrs.font_size.is_some() {
        el.attrs.font_size = probe.attrs.font_size;
    }
    if probe.attrs.font_family.is_some() {
        el.attrs.font_family = probe.attrs.font_family.clone();
    }
    if probe.attrs.font_weight.is_some() {
        el.attrs.font_weight = probe.attrs.font_weight;
    }
    if probe.attrs.text_color.is_some() {
        el.attrs.text_color = probe.attrs.text_color;
    }
    if probe.attrs.selection_color.is_some() {
        el.attrs.selection_color = probe.attrs.selection_color;
    }
    if probe.attrs.caret_color.is_some() {
        el.attrs.caret_color = probe.attrs.caret_color;
    }
    if probe.attrs.selection_text_color.is_some() {
        el.attrs.selection_text_color = probe.attrs.selection_text_color;
    }
    if probe.attrs.text_align.is_some() {
        el.attrs.text_align = probe.attrs.text_align;
    }
    if probe.attrs.text_wrap.is_some() {
        el.attrs.text_wrap = probe.attrs.text_wrap;
    }
    if probe.attrs.max_lines.is_some() {
        el.attrs.max_lines = probe.attrs.max_lines;
    }
    if probe.attrs.style_role.is_some() {
        el.attrs.style_role = probe.attrs.style_role.clone();
    }
    // Box.
    if probe.attrs.padding.is_some() {
        el.attrs.padding = probe.attrs.padding;
    }
    if probe.attrs.margin.is_some() {
        el.attrs.margin = probe.attrs.margin;
    }
    if probe.attrs.width.is_some() {
        el.attrs.width = probe.attrs.width;
    }
    if probe.attrs.height.is_some() {
        el.attrs.height = probe.attrs.height;
    }
    // D8: layout-affecting props a theme / media flip can change. The
    // runtime consumer (`run::apply_reapplied_attrs`) mirrors this set.
    if probe.attrs.min_width.is_some() {
        el.attrs.min_width = probe.attrs.min_width;
    }
    if probe.attrs.min_height.is_some() {
        el.attrs.min_height = probe.attrs.min_height;
    }
    if probe.attrs.max_width.is_some() {
        el.attrs.max_width = probe.attrs.max_width;
    }
    if probe.attrs.max_height.is_some() {
        el.attrs.max_height = probe.attrs.max_height;
    }
    if probe.attrs.gap.is_some() {
        el.attrs.gap = probe.attrs.gap;
    }
    if probe.attrs.gap_row.is_some() {
        el.attrs.gap_row = probe.attrs.gap_row;
    }
    if probe.attrs.gap_column.is_some() {
        el.attrs.gap_column = probe.attrs.gap_column;
    }
    if probe.attrs.grow.is_some() {
        el.attrs.grow = probe.attrs.grow;
    }
    if probe.attrs.flex.is_some() {
        el.attrs.flex = probe.attrs.flex;
    }
    if probe.attrs.display.is_some() {
        el.attrs.display = probe.attrs.display;
    }
    // Paint.
    if probe.attrs.bg.is_some() {
        el.attrs.bg = probe.attrs.bg.clone();
    }
    if probe.attrs.radius.is_some() {
        el.attrs.radius = probe.attrs.radius;
    }
    if probe.attrs.radius_corners.is_some() {
        el.attrs.radius_corners = probe.attrs.radius_corners;
    }
    if probe.attrs.knob_color.is_some() {
        el.attrs.knob_color = probe.attrs.knob_color;
    }
    if !probe.attrs.shadows.is_empty() {
        el.attrs.shadows = probe.attrs.shadows.clone();
    }
    if probe.attrs.opacity.is_some() {
        el.attrs.opacity = probe.attrs.opacity;
    }
    // Interaction tints.
    if probe.attrs.hover_bg.is_some() {
        el.attrs.hover_bg = probe.attrs.hover_bg;
    }
    if probe.attrs.press_bg.is_some() {
        el.attrs.press_bg = probe.attrs.press_bg;
    }
    if probe.attrs.checked_bg.is_some() {
        el.attrs.checked_bg = probe.attrs.checked_bg;
    }
    if probe.attrs.selected_bg.is_some() {
        el.attrs.selected_bg = probe.attrs.selected_bg;
    }
    if probe.attrs.disabled_bg.is_some() {
        el.attrs.disabled_bg = probe.attrs.disabled_bg;
    }
    if probe.attrs.drag_over_bg.is_some() {
        el.attrs.drag_over_bg = probe.attrs.drag_over_bg;
    }
    if probe.attrs.drag_over_text_color.is_some() {
        el.attrs.drag_over_text_color = probe.attrs.drag_over_text_color;
    }
    if probe.attrs.drag_over_opacity.is_some() {
        el.attrs.drag_over_opacity = probe.attrs.drag_over_opacity;
    }
    if probe.attrs.drag_over_shadows.is_some() {
        el.attrs.drag_over_shadows = probe.attrs.drag_over_shadows.clone();
    }
    // State-routed text / opacity / shadow swaps (native-skin wave).
    if probe.attrs.hover_text_color.is_some() {
        el.attrs.hover_text_color = probe.attrs.hover_text_color;
    }
    if probe.attrs.active_text_color.is_some() {
        el.attrs.active_text_color = probe.attrs.active_text_color;
    }
    if probe.attrs.focus_text_color.is_some() {
        el.attrs.focus_text_color = probe.attrs.focus_text_color;
    }
    if probe.attrs.disabled_text_color.is_some() {
        el.attrs.disabled_text_color = probe.attrs.disabled_text_color;
    }
    if probe.attrs.hover_opacity.is_some() {
        el.attrs.hover_opacity = probe.attrs.hover_opacity;
    }
    if probe.attrs.active_opacity.is_some() {
        el.attrs.active_opacity = probe.attrs.active_opacity;
    }
    if probe.attrs.focus_opacity.is_some() {
        el.attrs.focus_opacity = probe.attrs.focus_opacity;
    }
    if probe.attrs.disabled_opacity.is_some() {
        el.attrs.disabled_opacity = probe.attrs.disabled_opacity;
    }
    if probe.attrs.hover_shadows.is_some() {
        el.attrs.hover_shadows = probe.attrs.hover_shadows.clone();
    }
    if probe.attrs.active_shadows.is_some() {
        el.attrs.active_shadows = probe.attrs.active_shadows.clone();
    }
    if probe.attrs.focus_shadows.is_some() {
        el.attrs.focus_shadows = probe.attrs.focus_shadows.clone();
    }
    if probe.attrs.disabled_shadows.is_some() {
        el.attrs.disabled_shadows = probe.attrs.disabled_shadows.clone();
    }
    if probe.attrs.focus_visible_text_color.is_some() {
        el.attrs.focus_visible_text_color = probe.attrs.focus_visible_text_color;
    }
    if probe.attrs.focus_visible_opacity.is_some() {
        el.attrs.focus_visible_opacity = probe.attrs.focus_visible_opacity;
    }
    if probe.attrs.focus_visible_shadows.is_some() {
        el.attrs.focus_visible_shadows = probe.attrs.focus_visible_shadows.clone();
    }
    // Borders + box model extensions (CSS-flexibility wave). A theme
    // flip commonly retints `border-color` / swaps `border` shorthand.
    if probe.attrs.border_width.is_some() {
        el.attrs.border_width = probe.attrs.border_width;
    }
    if probe.attrs.border_color.is_some() {
        el.attrs.border_color = probe.attrs.border_color;
    }
    if probe.attrs.border_color_top.is_some() {
        el.attrs.border_color_top = probe.attrs.border_color_top;
    }
    if probe.attrs.border_color_right.is_some() {
        el.attrs.border_color_right = probe.attrs.border_color_right;
    }
    if probe.attrs.border_color_bottom.is_some() {
        el.attrs.border_color_bottom = probe.attrs.border_color_bottom;
    }
    if probe.attrs.border_color_left.is_some() {
        el.attrs.border_color_left = probe.attrs.border_color_left;
    }
    if probe.attrs.border_style.is_some() {
        el.attrs.border_style = probe.attrs.border_style;
    }
    if probe.attrs.box_sizing.is_some() {
        el.attrs.box_sizing = probe.attrs.box_sizing;
    }
    if probe.attrs.hover_border.is_some() {
        el.attrs.hover_border = probe.attrs.hover_border;
    }
    if probe.attrs.focus_border.is_some() {
        el.attrs.focus_border = probe.attrs.focus_border;
    }
    if probe.attrs.focus_outline.is_some() {
        el.attrs.focus_outline = probe.attrs.focus_outline;
    }
    if probe.attrs.focus_visible_outline.is_some() {
        el.attrs.focus_visible_outline = probe.attrs.focus_visible_outline;
    }
    if probe.attrs.outline_offset.is_some() {
        el.attrs.outline_offset = probe.attrs.outline_offset;
    }
    // Flex completeness.
    if probe.attrs.shrink.is_some() {
        el.attrs.shrink = probe.attrs.shrink;
    }
    if probe.attrs.basis.is_some() {
        el.attrs.basis = probe.attrs.basis;
    }
    if probe.attrs.flex_wrap.is_some() {
        el.attrs.flex_wrap = probe.attrs.flex_wrap;
    }
    if probe.attrs.align_content.is_some() {
        el.attrs.align_content = probe.attrs.align_content;
    }
    if probe.attrs.z_index.is_some() {
        el.attrs.z_index = probe.attrs.z_index;
    }
    if probe.attrs.gap_pct.is_some() {
        el.attrs.gap_pct = probe.attrs.gap_pct;
    }
    if probe.attrs.gap_row_pct.is_some() {
        el.attrs.gap_row_pct = probe.attrs.gap_row_pct;
    }
    if probe.attrs.gap_column_pct.is_some() {
        el.attrs.gap_column_pct = probe.attrs.gap_column_pct;
    }
}

fn resolve_vars(
    ctx: &str,
    decl_name: &str,
    value: &str,
    vars: &std::collections::HashMap<String, String>,
) -> Result<String, ParseError> {
    let prefix = format!("{ctx} '{decl_name}' = '{value}': ");
    crate::css_vars::resolve(value, vars, &prefix).map_err(ParseError::Xml)
}

fn describe_selector_for_error(sels: &[SelectorBuf], idx: usize) -> String {
    let Some(sel) = sels.get(idx) else {
        return "*".into();
    };
    let mut out = String::new();
    for (i, (combinator, c)) in sel.chain.iter().enumerate() {
        if i > 0 {
            match combinator {
                Combinator::Descendant => out.push(' '),
                Combinator::Child => out.push_str(" > "),
                Combinator::AdjacentSibling => out.push_str(" + "),
                Combinator::GeneralSibling => out.push_str(" ~ "),
                Combinator::Subject => {}
            }
        }
        if let Some(t) = &c.tag {
            out.push_str(t);
        }
        if let Some(id) = &c.id {
            out.push('#');
            out.push_str(id);
        }
        for cls in &c.classes {
            out.push('.');
            out.push_str(cls);
        }
        for p in &c.pseudo_classes {
            out.push(':');
            out.push_str(pseudo_name(p));
        }
    }
    if out.is_empty() {
        out = "*".into();
    }
    out
}

fn pseudo_name(p: &PseudoClass) -> &'static str {
    match p {
        PseudoClass::Hover => "hover",
        PseudoClass::Focus => "focus",
        PseudoClass::FocusVisible => "focus-visible",
        PseudoClass::Active => "active",
        PseudoClass::Disabled => "disabled",
        PseudoClass::Checked => "checked",
        PseudoClass::Selected => "selected",
        PseudoClass::DragOver => "drag-over",
        PseudoClass::Root => "root",
        PseudoClass::FirstChild => "first-child",
        PseudoClass::LastChild => "last-child",
        PseudoClass::OnlyChild => "only-child",
        PseudoClass::Empty => "empty",
        PseudoClass::NthChild(_) => "nth-child",
        PseudoClass::Is(_) => "is",
        PseudoClass::Where(_) => "where",
        PseudoClass::Not(_) => "not",
    }
}

/// Route a CSS declaration to the correct [`Attributes`] field based on
/// the subject compound's matched pseudo state.
/// Returns `Ok(true)` when the declaration was recognised (applied or
/// deliberately consumed for the pseudo state), `Ok(false)` for an
/// unknown property so the caller can warn.
fn apply_decl_for_pseudo(
    ctx: &str,
    name: &str,
    value: &str,
    pseudo: &SubjectPseudo,
    attrs: &mut Attributes,
) -> Result<bool, ParseError> {
    let name = canonical_property_name(name);
    if !pseudo.any() {
        return apply_declaration(ctx, name, value, attrs);
    }
    // State-routed properties. `:focus-visible` shares the `:focus`
    // slots for everything except `outline` (which gets its own
    // keyboard-only slot); `:checked` / `:selected` route `bg` only.
    match name {
        "bg" => {
            let c = parse_color(ctx, name, value)?;
            if pseudo.active {
                attrs.press_bg = Some(c);
            } else if pseudo.checked {
                attrs.checked_bg = Some(c);
            } else if pseudo.selected {
                attrs.selected_bg = Some(c);
            } else if pseudo.disabled {
                attrs.disabled_bg = Some(c);
            } else if pseudo.drag_over {
                attrs.drag_over_bg = Some(c);
            } else {
                // hover / focus / focus-visible share one slot -
                // preserves the existing API surface.
                attrs.hover_bg = Some(c);
            }
            Ok(true)
        }
        "outline" if pseudo.focus || pseudo.focus_visible => {
            let parts: Vec<&str> = value.split_whitespace().collect();
            if parts.len() != 2 {
                return Err(crate::values::bad(
                    ctx,
                    name,
                    value,
                    "expected '<width-px> <#color>'".into(),
                ));
            }
            let spec = crate::layout_ir::OutlineSpec {
                width: parse_f32(ctx, name, parts[0])?,
                color: parse_color(ctx, name, parts[1])?,
                offset: attrs.outline_offset.unwrap_or(0.0),
            };
            if pseudo.focus_visible {
                attrs.focus_visible_outline = Some(spec);
            } else {
                attrs.focus_outline = Some(spec);
            }
            Ok(true)
        }
        "outline-offset" if pseudo.focus || pseudo.focus_visible => {
            let off = parse_f32(ctx, name, value)?;
            attrs.outline_offset = Some(off);
            // Retro-apply onto outlines already parsed in this or an
            // earlier rule (declaration order within a rule is free).
            if let Some(o) = attrs.focus_outline.as_mut() {
                o.offset = off;
            }
            if let Some(o) = attrs.focus_visible_outline.as_mut() {
                o.offset = off;
            }
            Ok(true)
        }
        "border" if pseudo.hover || pseudo.focus || pseudo.focus_visible => {
            // `:hover { border: ... }` / `:focus { border: ... }` - state
            // border swap, resolved to concrete widths + color here.
            let sh = crate::values::parse_border_shorthand(ctx, name, value)?;
            let spec = border_shorthand_to_paint(&sh);
            if pseudo.focus || pseudo.focus_visible {
                attrs.focus_border = spec;
            } else {
                attrs.hover_border = spec;
            }
            Ok(true)
        }
        "text-color" => {
            let c = parse_color(ctx, name, value)?;
            if pseudo.active {
                attrs.active_text_color = Some(c);
            } else if pseudo.disabled {
                attrs.disabled_text_color = Some(c);
            } else if pseudo.drag_over {
                attrs.drag_over_text_color = Some(c);
            } else if pseudo.focus_visible {
                attrs.focus_visible_text_color = Some(c);
            } else if pseudo.focus {
                attrs.focus_text_color = Some(c);
            } else if pseudo.hover {
                attrs.hover_text_color = Some(c);
            }
            Ok(true)
        }
        "opacity" => {
            let v = parse_f32(ctx, name, value)?.clamp(0.0, 1.0);
            if pseudo.active {
                attrs.active_opacity = Some(v);
            } else if pseudo.disabled {
                attrs.disabled_opacity = Some(v);
            } else if pseudo.drag_over {
                attrs.drag_over_opacity = Some(v);
            } else if pseudo.focus_visible {
                attrs.focus_visible_opacity = Some(v);
            } else if pseudo.focus {
                attrs.focus_opacity = Some(v);
            } else if pseudo.hover {
                attrs.hover_opacity = Some(v);
            }
            Ok(true)
        }
        "shadow" | "box-shadow" => {
            let shadows = parse_box_shadow(ctx, name, value)?;
            if pseudo.active {
                attrs.active_shadows = Some(shadows);
            } else if pseudo.disabled {
                attrs.disabled_shadows = Some(shadows);
            } else if pseudo.drag_over {
                attrs.drag_over_shadows = Some(shadows);
            } else if pseudo.focus_visible {
                attrs.focus_visible_shadows = Some(shadows);
            } else if pseudo.focus {
                attrs.focus_shadows = Some(shadows);
            } else if pseudo.hover {
                attrs.hover_shadows = Some(shadows);
            }
            Ok(true)
        }
        // Any other property under a state pseudo is consumed silently
        // (not routable yet) - matches the pre-existing behavior.
        _ => Ok(true),
    }
}

/// Map standard CSS property names onto Lumen's native slots so real-world
/// stylesheets work as written. The Lumen short names stay accepted.
fn canonical_property_name(name: &str) -> &str {
    match name {
        "color" => "text-color",
        "background" | "background-color" => "bg",
        "border-radius" => "radius",
        "flex-grow" => "grow",
        // `white-space: nowrap | normal` shares the wrap slot - the
        // `wrap` parser already accepts both spellings as values.
        "white-space" => "wrap",
        n => n,
    }
}

/// Resolve a parsed `border` shorthand into the state-border paint spec
/// used by `hover-border:` / `focus-border:` / `:hover { border }`.
/// `border: none` (or width 0) clears the state override.
fn border_shorthand_to_paint(
    sh: &crate::values::BorderShorthand,
) -> Option<crate::layout_ir::BorderPaintSpec> {
    use crate::layout_ir::{BorderStyleSpec, Edges, Rgba};
    match sh.style {
        Some(BorderStyleSpec::Solid) => {
            let w = sh.width.unwrap_or(3.0);
            if w <= 0.0 {
                return None;
            }
            Some(crate::layout_ir::BorderPaintSpec {
                widths: Edges::all(w),
                color: sh.color.unwrap_or(Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
            })
        }
        _ => None,
    }
}

/// Returns `Ok(true)` when the property is known, `Ok(false)` when it
/// isn't (caller warns; declaration is ignored).
#[allow(clippy::collapsible_match, clippy::collapsible_if)]
fn apply_declaration(
    ctx: &str,
    name: &str,
    value: &str,
    attrs: &mut Attributes,
) -> Result<bool, ParseError> {
    let name = canonical_property_name(name);
    // No more `is_none()` guards - cascade ordering at the caller has
    // already arranged last-wins per CSS Cascade-5 section 6.4.4.
    match name {
        "width" => attrs.width = Some(parse_length(ctx, name, value)?),
        "height" => attrs.height = Some(parse_length(ctx, name, value)?),
        "bg" => attrs.bg = Some(parse_bg(ctx, name, value)?),
        "radius" => {
            // CSS `border-radius` shorthand: 1 value = uniform; 2-4
            // values map onto per-corner radii in the standard
            // [tl, tr, br, bl] rotation. The uniform slot always
            // carries the max corner for uniform-only consumers.
            let parts: Vec<&str> = value.split_whitespace().collect();
            match parts.as_slice() {
                [one] => {
                    attrs.radius = Some(parse_f32(ctx, name, one)?);
                    attrs.radius_corners = None;
                }
                _ => {
                    let corners = crate::values::parse_corner_radii(ctx, name, value)?;
                    attrs.radius = Some(corners.iter().copied().fold(0.0_f32, f32::max));
                    attrs.radius_corners = Some(corners);
                }
            }
        }
        "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-right-radius"
        | "border-bottom-left-radius" => {
            let v = parse_f32(ctx, name, value)?;
            let base = attrs.radius.unwrap_or(0.0);
            let corners = attrs.radius_corners.get_or_insert([base; 4]);
            match name {
                "border-top-left-radius" => corners[0] = v,
                "border-top-right-radius" => corners[1] = v,
                "border-bottom-right-radius" => corners[2] = v,
                "border-bottom-left-radius" => corners[3] = v,
                _ => unreachable!(),
            }
            attrs.radius = Some(corners.iter().copied().fold(0.0_f32, f32::max));
        }
        "padding" => attrs.padding = Some(parse_edges(ctx, name, value)?),
        "margin" => attrs.margin = Some(parse_edges(ctx, name, value)?),
        "text-color" => attrs.text_color = Some(parse_color(ctx, name, value)?),
        "selection-color" => attrs.selection_color = Some(parse_color(ctx, name, value)?),
        "caret-color" => attrs.caret_color = Some(parse_color(ctx, name, value)?),
        "selection-text-color" => attrs.selection_text_color = Some(parse_color(ctx, name, value)?),
        "hover-bg" => attrs.hover_bg = Some(parse_color(ctx, name, value)?),
        "scroll" => {
            attrs.scroll = Some(match value {
                "y" => ScrollAxisSpec::Y,
                "x" => ScrollAxisSpec::X,
                "both" => ScrollAxisSpec::Both,
                other => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        format!("unknown scroll axis '{other}'"),
                    ));
                }
            });
        }
        "sensitivity" => attrs.sensitivity = Some(parse_f32(ctx, name, value)?),
        "inertia" => attrs.inertia = Some(parse_f32(ctx, name, value)?),
        "tab-index" => attrs.tab_index = Some(parse_i32(ctx, name, value)?),
        "draggable" => attrs.draggable = matches!(value, "true" | "yes"),
        "press-bg" => attrs.press_bg = Some(parse_color(ctx, name, value)?),
        "font-size" => attrs.font_size = Some(parse_f32(ctx, name, value)?),
        "font-family" => {
            attrs.font_family = Some(value.trim().to_string());
        }
        "font-weight" => {
            attrs.font_weight = Some(crate::values::parse_font_weight(ctx, name, value)?);
        }
        "gap" => {
            // W5.9: `gap: <r> <c>` shorthand. One value -> both axes via
            // the legacy `attrs.gap` slot. Two values -> split into
            // `gap_row` + `gap_column` so the per-axis path wins over
            // the shorthand in `From<&Attributes> for Style`. Each term
            // may be a percent (`gap: 5%`) which resolves against the
            // container's content box per CSS.
            let one_term = |attr_px: &mut Option<f32>, attr_pct: &mut Option<f32>, s: &str| {
                if let Some(rest) = s.trim().strip_suffix('%') {
                    let p = parse_f32(ctx, name, rest)?;
                    *attr_pct = Some(p);
                } else {
                    *attr_px = Some(parse_f32(ctx, name, s)?);
                }
                Ok::<(), ParseError>(())
            };
            let parts: Vec<&str> = value.split_whitespace().collect();
            match parts.as_slice() {
                [one] => one_term(&mut attrs.gap, &mut attrs.gap_pct, one)?,
                [r, c] => {
                    one_term(&mut attrs.gap_row, &mut attrs.gap_row_pct, r)?;
                    one_term(&mut attrs.gap_column, &mut attrs.gap_column_pct, c)?;
                }
                _ => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        "expected '<row+col>' or '<row> <col>'".into(),
                    ));
                }
            }
        }
        "row-gap" => {
            if let Some(rest) = value.trim().strip_suffix('%') {
                attrs.gap_row_pct = Some(parse_f32(ctx, name, rest)?);
            } else {
                attrs.gap_row = Some(parse_f32(ctx, name, value)?);
            }
        }
        "column-gap" => {
            if let Some(rest) = value.trim().strip_suffix('%') {
                attrs.gap_column_pct = Some(parse_f32(ctx, name, rest)?);
            } else {
                attrs.gap_column = Some(parse_f32(ctx, name, value)?);
            }
        }
        "grow" => attrs.grow = Some(parse_f32(ctx, name, value)?),
        "flex-shrink" => attrs.shrink = Some(parse_f32(ctx, name, value)?),
        "flex-basis" => attrs.basis = Some(parse_length(ctx, name, value)?),
        "flex-wrap" => {
            attrs.flex_wrap = Some(match value.trim() {
                "nowrap" => crate::layout_ir::FlexWrapSpec::NoWrap,
                "wrap" => crate::layout_ir::FlexWrapSpec::Wrap,
                "wrap-reverse" => crate::layout_ir::FlexWrapSpec::WrapReverse,
                other => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        format!("unknown flex-wrap '{other}'"),
                    ));
                }
            });
        }
        "align-content" => {
            use crate::layout_ir::AlignContentSpec as A;
            attrs.align_content = Some(match value.trim() {
                "start" | "flex-start" => A::Start,
                "end" | "flex-end" => A::End,
                "center" => A::Center,
                "stretch" | "normal" => A::Stretch,
                "space-between" => A::SpaceBetween,
                "space-around" => A::SpaceAround,
                "space-evenly" => A::SpaceEvenly,
                other => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        format!("unknown align-content '{other}'"),
                    ));
                }
            });
        }
        "flex-direction" => {
            attrs.flex = Some(match value.trim() {
                "row" => crate::layout_ir::FlexAxis::Row,
                "column" => crate::layout_ir::FlexAxis::Column,
                "row-reverse" => crate::layout_ir::FlexAxis::RowReverse,
                "column-reverse" => crate::layout_ir::FlexAxis::ColumnReverse,
                other => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        format!("unknown flex-direction '{other}'"),
                    ));
                }
            });
        }
        "flex" => {
            // CSS `flex: <grow> [<shrink>] [<basis>]` plus the keyword
            // forms. Per spec, a unitless single value sets grow with
            // shrink=1 and basis=0%.
            let v = value.trim();
            match v {
                "none" => {
                    attrs.grow = Some(0.0);
                    attrs.shrink = Some(0.0);
                    attrs.basis = Some(crate::layout_ir::LengthSpec::Auto);
                }
                "auto" => {
                    attrs.grow = Some(1.0);
                    attrs.shrink = Some(1.0);
                    attrs.basis = Some(crate::layout_ir::LengthSpec::Auto);
                }
                "initial" => {
                    attrs.grow = Some(0.0);
                    attrs.shrink = Some(1.0);
                    attrs.basis = Some(crate::layout_ir::LengthSpec::Auto);
                }
                _ => {
                    let parts: Vec<&str> = v.split_whitespace().collect();
                    match parts.as_slice() {
                        [g] => {
                            attrs.grow = Some(parse_f32(ctx, name, g)?);
                            attrs.shrink = Some(1.0);
                            attrs.basis = Some(crate::layout_ir::LengthSpec::Percent(0.0));
                        }
                        [g, s] => {
                            attrs.grow = Some(parse_f32(ctx, name, g)?);
                            // Second term is either <shrink> (number) or
                            // <basis> (length with unit / auto).
                            if let Ok(shrink) = s.parse::<f32>() {
                                attrs.shrink = Some(shrink);
                                attrs.basis = Some(crate::layout_ir::LengthSpec::Percent(0.0));
                            } else {
                                attrs.shrink = Some(1.0);
                                attrs.basis = Some(parse_length(ctx, name, s)?);
                            }
                        }
                        [g, s, b] => {
                            attrs.grow = Some(parse_f32(ctx, name, g)?);
                            attrs.shrink = Some(parse_f32(ctx, name, s)?);
                            attrs.basis = Some(parse_length(ctx, name, b)?);
                        }
                        _ => {
                            return Err(bad(
                                ctx,
                                name,
                                value,
                                "expected 'flex: <grow> [<shrink>] [<basis>]'".into(),
                            ));
                        }
                    }
                }
            }
        }
        "z-index" => {
            attrs.z_index = Some(if value.trim() == "auto" {
                0
            } else {
                parse_i32(ctx, name, value)?
            });
        }
        "border" => {
            let sh = crate::values::parse_border_shorthand(ctx, name, value)?;
            // CSS shorthand semantics: reset all three longhands, then
            // apply the authored terms (omitted width -> medium, omitted
            // color -> currentColor, resolved at spawn).
            attrs.border_style = sh.style;
            attrs.border_width = sh.width.map(crate::layout_ir::Edges::all);
            attrs.border_color = sh.color;
        }
        "border-width" => {
            attrs.border_width = Some(crate::values::parse_border_width_edges(ctx, name, value)?);
        }
        "border-color" => attrs.border_color = Some(parse_color(ctx, name, value)?),
        "border-top-color" => attrs.border_color_top = Some(parse_color(ctx, name, value)?),
        "border-right-color" => attrs.border_color_right = Some(parse_color(ctx, name, value)?),
        "border-bottom-color" => attrs.border_color_bottom = Some(parse_color(ctx, name, value)?),
        "border-left-color" => attrs.border_color_left = Some(parse_color(ctx, name, value)?),
        "border-top" | "border-right" | "border-bottom" | "border-left" => {
            // Per-side border shorthand: sets that side's width + color
            // (and the shared solid style). Approximation of CSS's
            // per-side border-style: Lumen has one style slot, so
            // `border-bottom: 1px solid X` on an otherwise borderless
            // element yields widths {bottom: 1, rest: 0}.
            let sh = crate::values::parse_border_shorthand(ctx, name, value)?;
            let is_none = matches!(sh.style, Some(crate::layout_ir::BorderStyleSpec::None));
            let w = if is_none {
                0.0
            } else {
                sh.width.unwrap_or(3.0)
            };
            let edges = attrs
                .border_width
                .get_or_insert_with(crate::layout_ir::Edges::default);
            let side_color = if is_none { None } else { sh.color };
            match name {
                "border-top" => {
                    edges.top = w;
                    if let Some(c) = side_color {
                        attrs.border_color_top = Some(c);
                    }
                }
                "border-right" => {
                    edges.right = w;
                    if let Some(c) = side_color {
                        attrs.border_color_right = Some(c);
                    }
                }
                "border-bottom" => {
                    edges.bottom = w;
                    if let Some(c) = side_color {
                        attrs.border_color_bottom = Some(c);
                    }
                }
                "border-left" => {
                    edges.left = w;
                    if let Some(c) = side_color {
                        attrs.border_color_left = Some(c);
                    }
                }
                _ => unreachable!(),
            }
            if !is_none {
                attrs.border_style = Some(crate::layout_ir::BorderStyleSpec::Solid);
            }
        }
        "border-style" => {
            attrs.border_style = Some(crate::values::parse_border_style(ctx, name, value)?);
        }
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            let px = crate::values::parse_border_width_term(ctx, name, value, value)?;
            let edges = attrs
                .border_width
                .get_or_insert_with(crate::layout_ir::Edges::default);
            match name {
                "border-top-width" => edges.top = px,
                "border-right-width" => edges.right = px,
                "border-bottom-width" => edges.bottom = px,
                "border-left-width" => edges.left = px,
                _ => unreachable!(),
            }
        }
        "box-sizing" => {
            attrs.box_sizing = Some(match value.trim() {
                "border-box" => crate::layout_ir::BoxSizingSpec::BorderBox,
                "content-box" => crate::layout_ir::BoxSizingSpec::ContentBox,
                other => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        format!("unknown box-sizing '{other}'"),
                    ));
                }
            });
        }
        // Lumen-native state-border / focus-ring properties (match the
        // markup attr spellings; also reachable via `:hover`/`:focus`
        // pseudo rules).
        "hover-border" => {
            let sh = crate::values::parse_border_shorthand(ctx, name, value)?;
            attrs.hover_border = border_shorthand_to_paint(&sh);
        }
        "focus-border" => {
            let sh = crate::values::parse_border_shorthand(ctx, name, value)?;
            attrs.focus_border = border_shorthand_to_paint(&sh);
        }
        "focus-outline" => {
            let parts: Vec<&str> = value.split_whitespace().collect();
            if parts.len() != 2 {
                return Err(crate::values::bad(
                    ctx,
                    name,
                    value,
                    "expected '<width-px> <#color>'".into(),
                ));
            }
            attrs.focus_outline = Some(crate::layout_ir::OutlineSpec {
                width: parse_f32(ctx, name, parts[0])?,
                color: parse_color(ctx, name, parts[1])?,
                offset: attrs.outline_offset.unwrap_or(0.0),
            });
        }
        "outline-offset" => {
            let off = parse_f32(ctx, name, value)?;
            attrs.outline_offset = Some(off);
            if let Some(o) = attrs.focus_outline.as_mut() {
                o.offset = off;
            }
            if let Some(o) = attrs.focus_visible_outline.as_mut() {
                o.offset = off;
            }
        }
        "knob-color" => {
            attrs.knob_color = Some(parse_color(ctx, name, value)?);
        }
        "display" => {
            attrs.display = Some(match value.trim() {
                "flex" => DisplaySpec::Flex,
                "grid" => DisplaySpec::Grid,
                "none" => DisplaySpec::None,
                other => {
                    return Err(bad(ctx, name, value, format!("unknown display '{other}'")));
                }
            });
        }
        "grid-template-rows" => {
            let rows = parse_track_list(ctx, name, value)?;
            let mut gt = attrs.grid_template.clone().unwrap_or_default();
            gt.rows = rows;
            attrs.grid_template = Some(gt);
        }
        "grid-template-columns" => {
            let cols = parse_track_list(ctx, name, value)?;
            let mut gt = attrs.grid_template.clone().unwrap_or_default();
            gt.columns = cols;
            attrs.grid_template = Some(gt);
        }
        "grid-row" => attrs.grid_row = Some(parse_grid_line_pair(ctx, name, value)?),
        "grid-column" => attrs.grid_column = Some(parse_grid_line_pair(ctx, name, value)?),
        "align" | "align-items" => {
            attrs.align = Some(parse_flex_align(ctx, name, value)?);
        }
        "align-self" => {
            attrs.align_self = Some(parse_flex_align(ctx, name, value)?);
        }
        "justify-items" => {
            attrs.justify_items = Some(parse_flex_align(ctx, name, value)?);
        }
        "justify-self" => {
            attrs.justify_self = Some(parse_flex_align(ctx, name, value)?);
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
                    return Err(bad(ctx, name, value, format!("unknown justify '{other}'")));
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
                        ctx,
                        name,
                        value,
                        format!("unknown text-align '{other}'"),
                    ));
                }
            });
        }
        "wrap" => {
            attrs.text_wrap = Some(match value {
                "none" | "nowrap" => TextWrapSpec::None,
                "word" | "normal" => TextWrapSpec::Word,
                "glyph" | "char" => TextWrapSpec::Glyph,
                other => return Err(bad(ctx, name, value, format!("unknown wrap '{other}'"))),
            });
        }
        "max-lines" => {
            let n: i64 = value
                .trim()
                .parse()
                .map_err(|e: std::num::ParseIntError| bad(ctx, name, value, e.to_string()))?;
            if n < 0 {
                return Err(bad(
                    ctx,
                    name,
                    value,
                    "max-lines must be \u{2265} 0".to_string(),
                ));
            }
            attrs.max_lines = Some(n as u32);
        }
        // Lumen-native analog property (like `knob-color`): the
        // indeterminate `<progress>` sweep period in ms. Skins route it
        // through `--lumen-progress-period`.
        "progress-duration" => {
            let n: i64 = value
                .trim()
                .parse()
                .map_err(|e: std::num::ParseIntError| bad(ctx, name, value, e.to_string()))?;
            if n <= 0 {
                return Err(bad(
                    ctx,
                    name,
                    value,
                    "progress-duration must be > 0 (ms)".to_string(),
                ));
            }
            attrs.progress_duration = Some(n as u32);
        }
        "text-overflow" => {
            attrs.text_overflow = Some(match value.trim() {
                "clip" => crate::layout_ir::TextOverflowSpec::Clip,
                "ellipsis" => crate::layout_ir::TextOverflowSpec::Ellipsis,
                other => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        format!("unknown text-overflow '{other}' (supported: clip, ellipsis)"),
                    ));
                }
            });
        }
        "position" => {
            attrs.position = Some(match value {
                "relative" => PositionSpec::Relative,
                "absolute" => PositionSpec::Absolute,
                other => {
                    return Err(bad(ctx, name, value, format!("unknown position '{other}'")));
                }
            });
        }
        "inset" => attrs.inset = Some(parse_edges(ctx, name, value)?),
        "min-width" => attrs.min_width = Some(parse_length(ctx, name, value)?),
        "min-height" => attrs.min_height = Some(parse_length(ctx, name, value)?),
        "max-width" => attrs.max_width = Some(parse_length(ctx, name, value)?),
        "max-height" => attrs.max_height = Some(parse_length(ctx, name, value)?),
        "aspect-ratio" => attrs.aspect_ratio = Some(parse_f32(ctx, name, value)?),
        "opacity" => {
            let v = parse_f32(ctx, name, value)?;
            attrs.opacity = Some(v.clamp(0.0, 1.0));
        }
        "shadow" | "box-shadow" => {
            attrs.shadows = parse_box_shadow(ctx, name, value)?;
        }
        "fit" => {
            attrs.image_fit = Some(match value {
                "fill" => ImageFitSpec::Fill,
                "cover" => ImageFitSpec::Cover,
                "contain" => ImageFitSpec::Contain,
                "none" => ImageFitSpec::None,
                "scale-down" => ImageFitSpec::ScaleDown,
                other => return Err(bad(ctx, name, value, format!("unknown fit '{other}'"))),
            });
        }
        "transition" => {
            attrs.transitions = parse_transition(ctx, name, value)?;
        }
        // Longhand trio - combined by `Attributes::effective_transitions`
        // (duration / timing cycle over the property list per CSS).
        "transition-property" => {
            let v = value.trim();
            if v == "none" {
                attrs.transition_property = Some(Vec::new());
            } else {
                let mut props = Vec::new();
                for entry in v.split(',') {
                    let entry = entry.trim();
                    if entry.is_empty() {
                        continue;
                    }
                    match TransitionPropertyIr::from_css_name(entry) {
                        Some(p) => props.push(p),
                        None => tracing::warn!(
                            target: "lumenc::css",
                            "transition-property: '{entry}' is not animatable - ignored"
                        ),
                    }
                }
                attrs.transition_property = Some(props);
            }
        }
        "transition-duration" => {
            let mut out = Vec::new();
            for entry in value.split(',') {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                out.push(parse_duration_ms(ctx, name, entry)?);
            }
            attrs.transition_duration = Some(out);
        }
        "transition-timing-function" => {
            let mut out = Vec::new();
            for entry in split_top_level_commas(value) {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                out.push(parse_easing(ctx, name, &[entry])?);
            }
            attrs.transition_timing = Some(out);
        }
        // CSS Scrollbars Styling Level 1 - overlay-bar styling for
        // `<scroll>` containers. `scrollbar-color: auto` clears back to
        // the runtime default.
        "scrollbar-color" => {
            let v = value.trim();
            if v == "auto" {
                attrs.scrollbar_color = None;
            } else {
                let parts: Vec<&str> = v.split_whitespace().collect();
                if parts.is_empty() || parts.len() > 2 {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        "expected 'auto' or '<thumb-color> [<track-color>]'".into(),
                    ));
                }
                let thumb = parse_color(ctx, name, parts[0])?;
                let track = match parts.get(1) {
                    Some(t) => Some(parse_color(ctx, name, t)?),
                    None => None,
                };
                attrs.scrollbar_color = Some((thumb, track));
            }
        }
        "scrollbar-width" => {
            attrs.scrollbar_width = Some(match value.trim() {
                "auto" => ScrollbarWidthSpec::Auto,
                "thin" => ScrollbarWidthSpec::Thin,
                "none" => ScrollbarWidthSpec::None,
                other => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        format!("unknown scrollbar-width '{other}' (supported: auto, thin, none)"),
                    ));
                }
            });
        }
        // W5.5: CSS Logical Properties Level 1 subset. Each value
        // parses as an f32 in px (so typos surface immediately) and
        // is written into the matching logical-edge field of the
        // owning `Edges` (padding / margin / inset) IR slot. The
        // lumen-core `Edges::resolved` then maps these onto the
        // physical sides per the entity's `ResolvedDirection` at
        // layout time.
        "padding-inline-start"
        | "padding-inline-end"
        | "padding-block-start"
        | "padding-block-end" => {
            let px = parse_f32(ctx, name, value)?;
            let edges = attrs
                .padding
                .get_or_insert_with(crate::layout_ir::Edges::default);
            match name {
                "padding-inline-start" => edges.inline_start = Some(px),
                "padding-inline-end" => edges.inline_end = Some(px),
                "padding-block-start" => edges.block_start = Some(px),
                "padding-block-end" => edges.block_end = Some(px),
                _ => unreachable!(),
            }
        }
        "margin-inline-start" | "margin-inline-end" | "margin-block-start" | "margin-block-end" => {
            let px = parse_f32(ctx, name, value)?;
            let edges = attrs
                .margin
                .get_or_insert_with(crate::layout_ir::Edges::default);
            match name {
                "margin-inline-start" => edges.inline_start = Some(px),
                "margin-inline-end" => edges.inline_end = Some(px),
                "margin-block-start" => edges.block_start = Some(px),
                "margin-block-end" => edges.block_end = Some(px),
                _ => unreachable!(),
            }
        }
        "inset-inline-start" | "inset-inline-end" | "inset-block-start" | "inset-block-end" => {
            let px = parse_f32(ctx, name, value)?;
            let edges = attrs
                .inset
                .get_or_insert_with(crate::layout_ir::Edges::default);
            match name {
                "inset-inline-start" => edges.inline_start = Some(px),
                "inset-inline-end" => edges.inline_end = Some(px),
                "inset-block-start" => edges.block_start = Some(px),
                "inset-block-end" => edges.block_end = Some(px),
                _ => unreachable!(),
            }
        }
        "border-inline-start-width"
        | "border-inline-end-width"
        | "border-block-start-width"
        | "border-block-end-width" => {
            let px = crate::values::parse_border_width_term(ctx, name, value, value)?;
            let edges = attrs
                .border_width
                .get_or_insert_with(crate::layout_ir::Edges::default);
            match name {
                "border-inline-start-width" => edges.inline_start = Some(px),
                "border-inline-end-width" => edges.inline_end = Some(px),
                "border-block-start-width" => edges.block_start = Some(px),
                "border-block-end-width" => edges.block_end = Some(px),
                _ => unreachable!(),
            }
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            let o = match value.trim() {
                "visible" => OverflowSpec::Visible,
                "hidden" => OverflowSpec::Hidden,
                "scroll" => OverflowSpec::Scroll,
                other => {
                    return Err(bad(
                        ctx,
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
        "layout-boundary" => attrs.layout_boundary = matches!(value.trim(), "true" | "yes"),
        _ => return Ok(false),
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// W5.9: grid / baseline parsers
// ---------------------------------------------------------------------------

/// Parse `align-items` / `align-self` / `justify-items` / `justify-self`
/// /  `align` values. CSS Grid + W5.9 adds `baseline` to the legacy
/// `start/end/center/stretch` set.
fn parse_flex_align(ctx: &str, name: &str, value: &str) -> Result<FlexAlign, ParseError> {
    Ok(match value.trim() {
        "start" | "flex-start" => FlexAlign::Start,
        "end" | "flex-end" => FlexAlign::End,
        "center" => FlexAlign::Center,
        "stretch" => FlexAlign::Stretch,
        "baseline" => FlexAlign::Baseline,
        other => return Err(bad(ctx, name, value, format!("unknown align '{other}'"))),
    })
}

/// Parse a single CSS Grid track size - `<N>px`, `<N>fr`, `auto`,
/// `min-content`, `max-content`, or `minmax(<min>, <max>)`. Returns
/// the IR-side [`TrackSizeSpec`].
fn parse_track_size(ctx: &str, name: &str, value: &str) -> Result<TrackSizeSpec, ParseError> {
    let v = value.trim();
    if let Some(inner) = v.strip_prefix("minmax(").and_then(|s| s.strip_suffix(')')) {
        // Split on first top-level comma. minmax() never nests deeper
        // than one level in CSS Grid L1, so a flat split suffices.
        let mut depth = 0_i32;
        let mut split: Option<usize> = None;
        for (i, c) in inner.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    split = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(comma) = split else {
            return Err(bad(
                ctx,
                name,
                value,
                "minmax() requires two arguments separated by ','".into(),
            ));
        };
        let lhs = parse_track_size(ctx, name, &inner[..comma])?;
        let rhs = parse_track_size(ctx, name, &inner[comma + 1..])?;
        return Ok(TrackSizeSpec::MinMax(Box::new(lhs), Box::new(rhs)));
    }
    if v == "auto" {
        return Ok(TrackSizeSpec::Auto);
    }
    if v == "min-content" {
        return Ok(TrackSizeSpec::MinContent);
    }
    if v == "max-content" {
        return Ok(TrackSizeSpec::MaxContent);
    }
    if let Some(num) = v.strip_suffix("fr") {
        let f = parse_f32(ctx, name, num.trim())?;
        return Ok(TrackSizeSpec::Fr(f));
    }
    if let Some(num) = v.strip_suffix("px") {
        let n = parse_f32(ctx, name, num.trim())?;
        return Ok(TrackSizeSpec::Fixed(n));
    }
    // Bare number -> pixels.
    if v.parse::<f32>().is_ok() {
        let n = parse_f32(ctx, name, v)?;
        return Ok(TrackSizeSpec::Fixed(n));
    }
    Err(bad(ctx, name, value, format!("unknown track size '{v}'")))
}

/// Parse a `grid-template-rows` / `grid-template-columns` value into
/// a vector of track-size terms. Whitespace-separated; the W5.9
/// minimal subset doesn't yet support named lines or `repeat()`.
fn parse_track_list(ctx: &str, name: &str, value: &str) -> Result<Vec<TrackSizeSpec>, ParseError> {
    // Tokenise on whitespace, but keep `minmax(a, b)` together so the
    // commas inside the function don't split the outer list. The
    // tokeniser walks chars and tracks paren depth.
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth = 0_i32;
    for ch in value.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth -= 1;
                cur.push(ch);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let mut out = Vec::with_capacity(tokens.len());
    for t in tokens {
        out.push(parse_track_size(ctx, name, &t)?);
    }
    Ok(out)
}

/// Parse `grid-row` / `grid-column` `<start>[/<end>]` values. A
/// single integer = start only, end auto-placed. A pair separated
/// by `/` = explicit start + end line numbers. CSS `span <N>` and
/// named lines are out of scope for the W5.9 subset.
///
/// `pub` so `lumenc`'s grid tests (which drive the parser front-end and
/// therefore stay in `lumenc`) can exercise it via the `parser_css`
/// re-export.
pub fn parse_grid_line_pair(ctx: &str, name: &str, value: &str) -> Result<(i16, i16), ParseError> {
    let v = value.trim();
    if let Some(idx) = v.find('/') {
        let lhs = v[..idx].trim();
        let rhs = v[idx + 1..].trim();
        let s = grid_line_to_i16(ctx, name, parse_i32(ctx, name, lhs)?);
        let e = grid_line_to_i16(ctx, name, parse_i32(ctx, name, rhs)?);
        return Ok((s, e));
    }
    let s = grid_line_to_i16(ctx, name, parse_i32(ctx, name, v)?);
    Ok((s, 0))
}

/// Grid line numbers are stored as `i16`. A value outside that range is
/// clamped (with a warning) rather than silently wrapping via `as i16`
/// (e.g. `grid-row: 40000` would have wrapped to a negative line).
fn grid_line_to_i16(ctx: &str, name: &str, v: i32) -> i16 {
    if v < i16::MIN as i32 || v > i16::MAX as i32 {
        let clamped = v.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        tracing::warn!(
            target: "lumenc::css",
            "{ctx} {{ {name} }}: grid line {v} out of range [{}, {}] - clamped to {clamped}",
            i16::MIN,
            i16::MAX,
        );
        clamped
    } else {
        v as i16
    }
}

// ---------------------------------------------------------------------------
// transition / shadow helpers (unchanged semantics)
// ---------------------------------------------------------------------------

fn parse_transition(ctx: &str, name: &str, value: &str) -> Result<Vec<TransitionIr>, ParseError> {
    let mut out = Vec::new();
    for entry in split_top_level_commas(value) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let parts: Vec<&str> = entry.split_whitespace().collect();
        if parts.len() < 2 {
            return Err(bad(
                ctx,
                name,
                value,
                format!("expected '<property> <duration> [<easing>]' in entry '{entry}'"),
            ));
        }
        let property = match TransitionPropertyIr::from_css_name(parts[0]) {
            Some(p) => p,
            None => {
                // CSS behavior: unanimatable / unknown properties in a
                // transition list are ignored, not errors. Geometry
                // props (width / height / padding ...) are deliberately
                // excluded in v1 - animating them would re-run layout
                // every frame.
                tracing::warn!(
                    target: "lumenc::css",
                    "transition: property '{}' is not animatable (v1 animates opacity, \
                     background-color, color, border-color) - entry ignored",
                    parts[0]
                );
                continue;
            }
        };
        let duration_ms = parse_duration_ms(ctx, name, parts[1])?;
        let easing = if parts.len() >= 3 {
            parse_easing(ctx, name, &parts[2..])?
        } else {
            EasingIr::EaseOut
        };
        out.push(TransitionIr {
            property,
            duration_ms,
            easing,
        });
    }
    Ok(out)
}

fn parse_box_shadow(ctx: &str, name: &str, value: &str) -> Result<Vec<ShadowSpec>, ParseError> {
    let mut out = Vec::new();
    for entry in split_top_level_commas(value) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut toks: Vec<&str> = entry.split_whitespace().collect();
        let inner = if toks.first().is_some_and(|t| *t == "inset") {
            toks.remove(0);
            true
        } else {
            false
        };
        if toks.len() != 4 && toks.len() != 5 {
            return Err(bad(
                ctx,
                name,
                value,
                format!("expected '[inset] <x> <y> <blur> [<spread>] <#color>' in entry '{entry}'"),
            ));
        }
        let spread = if toks.len() == 5 {
            parse_f32(ctx, name, toks[3])?
        } else {
            0.0
        };
        out.push(ShadowSpec {
            offset_x: parse_f32(ctx, name, toks[0])?,
            offset_y: parse_f32(ctx, name, toks[1])?,
            blur: parse_f32(ctx, name, toks[2])?,
            spread,
            color: parse_color(ctx, name, toks[if toks.len() == 5 { 4 } else { 3 }])?,
            inner,
        });
    }
    Ok(out)
}

fn parse_duration_ms(ctx: &str, name: &str, raw: &str) -> Result<u32, ParseError> {
    if let Some(stripped) = raw.strip_suffix("ms") {
        let v: f32 = stripped
            .parse()
            .map_err(|e: std::num::ParseFloatError| bad(ctx, name, raw, e.to_string()))?;
        if v < 0.0 {
            return Err(bad(ctx, name, raw, "duration must be \u{2265} 0".into()));
        }
        return Ok(v.round() as u32);
    }
    if let Some(stripped) = raw.strip_suffix('s') {
        let v: f32 = stripped
            .parse()
            .map_err(|e: std::num::ParseFloatError| bad(ctx, name, raw, e.to_string()))?;
        if v < 0.0 {
            return Err(bad(ctx, name, raw, "duration must be \u{2265} 0".into()));
        }
        return Ok((v * 1000.0).round() as u32);
    }
    Err(bad(
        ctx,
        name,
        raw,
        format!("expected 'Nms' or 'Ns'; got '{raw}'"),
    ))
}

fn parse_easing(ctx: &str, name: &str, parts: &[&str]) -> Result<EasingIr, ParseError> {
    let joined = parts.join(" ");
    let raw = joined.trim();
    match raw {
        "linear" => Ok(EasingIr::Linear),
        "ease" => Ok(EasingIr::CubicBezier(0.25, 0.1, 0.25, 1.0)),
        "ease-in" => Ok(EasingIr::EaseIn),
        "ease-out" => Ok(EasingIr::EaseOut),
        "ease-in-out" => Ok(EasingIr::EaseInOut),
        other if other.starts_with("cubic-bezier(") && other.ends_with(')') => {
            let inside = &other["cubic-bezier(".len()..other.len() - 1];
            let nums: Vec<f32> = inside
                .split(',')
                .map(|p| {
                    p.trim()
                        .parse::<f32>()
                        .map_err(|e| bad(ctx, name, raw, e.to_string()))
                })
                .collect::<Result<_, _>>()?;
            if nums.len() != 4 {
                return Err(bad(
                    ctx,
                    name,
                    raw,
                    "cubic-bezier expects 4 comma-separated numbers".into(),
                ));
            }
            Ok(EasingIr::CubicBezier(nums[0], nums[1], nums[2], nums[3]))
        }
        other => Err(bad(ctx, name, raw, format!("unknown easing '{other}'"))),
    }
}

// ---------------------------------------------------------------------------
// Lint mode helper - `lumenc lint --css-cascade` reports rules whose
// resolved value flips between legacy first-wins and new last-wins.
// ---------------------------------------------------------------------------

/// One diff finding emitted by [`cascade_lint`].
#[derive(Debug, Clone, PartialEq)]
pub struct CascadeDivergence {
    /// Selector text whose result flips between the two orderings.
    pub selector: String,
    /// Property name whose resolved value flipped.
    pub property: String,
    /// Value picked under legacy first-wins.
    pub first_wins: String,
    /// Value picked under conformant last-wins.
    pub last_wins: String,
}

/// Scan a parsed stylesheet and emit one [`CascadeDivergence`] per
/// (selector x property) pair whose resolved value differs between
/// the old first-wins ordering and the new last-wins ordering.
///
/// This is the audit's "compat regression escape valve": authors who
/// relied on the broken first-wins behaviour can run
/// `lumenc lint --css-cascade <dir>` and patch flagged rules ahead of
/// the next compiler upgrade.
pub fn cascade_lint(css: &Stylesheet) -> Vec<CascadeDivergence> {
    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for rule in &css.rules {
        for (sel_idx, _) in rule.selectors.iter().enumerate() {
            let sel_text = describe_selector_for_error(&rule.selectors, sel_idx);
            let entry = buckets.entry(sel_text).or_default();
            for decl in &rule.declarations {
                if decl.name.starts_with("--") {
                    continue;
                }
                entry
                    .entry(decl.name.clone())
                    .or_default()
                    .push(decl.value.clone());
            }
        }
    }
    let mut out = Vec::new();
    for (selector, props) in buckets {
        for (property, values) in props {
            if values.len() < 2 {
                continue;
            }
            let first = values.first().unwrap();
            let last = values.last().unwrap();
            if first != last {
                out.push(CascadeDivergence {
                    selector: selector.clone(),
                    property,
                    first_wins: first.clone(),
                    last_wins: last.clone(),
                });
            }
        }
    }
    out
}

// Split `s` on top-level commas (commas not nested inside parentheses),
// returning trimmed-inclusive slices. Duplicated from `lumenc`'s parser
// front-end (`parse_selector_list` needs it there); the value-list helpers
// (`parse_transition` / `parse_box_shadow`) and `apply_declaration` need it
// here.
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

#[cfg(test)]
mod transition_tests {
    use super::*;

    #[test]
    fn parses_single_opacity_transition() {
        let out = parse_transition("ctx", "transition", "opacity 200ms ease-out").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].property, TransitionPropertyIr::Opacity);
        assert_eq!(out[0].duration_ms, 200);
        assert_eq!(out[0].easing, EasingIr::EaseOut);
    }

    #[test]
    fn parses_comma_separated_entries() {
        let out = parse_transition(
            "ctx",
            "transition",
            "opacity 100ms linear, opacity 300ms ease-in",
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].easing, EasingIr::EaseIn);
        assert_eq!(out[1].duration_ms, 300);
    }

    #[test]
    fn drops_unsupported_property() {
        // Layout props are deliberately not animatable in v1 - they
        // parse-drop with a warn, never error.
        let out = parse_transition(
            "ctx",
            "transition",
            "width 100ms ease-out, opacity 200ms ease-out",
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].property, TransitionPropertyIr::Opacity);
    }

    #[test]
    fn parses_color_properties() {
        let out = parse_transition(
            "ctx",
            "transition",
            "bg 130ms ease, color 100ms linear, border-color 100ms linear",
        )
        .unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].property, TransitionPropertyIr::BackgroundColor);
        assert_eq!(out[1].property, TransitionPropertyIr::TextColor);
        assert_eq!(out[2].property, TransitionPropertyIr::BorderColor);
        // `ease` maps to the CSS ease cubic-bezier.
        assert!(matches!(out[0].easing, EasingIr::CubicBezier(..)));
    }

    #[test]
    fn longhands_combine_with_cycled_durations() {
        use crate::layout_ir::Attributes;
        let mut attrs = Attributes::default();
        apply_declaration(
            "ctx",
            "transition-property",
            "opacity, bg, border-color",
            &mut attrs,
        )
        .unwrap();
        apply_declaration("ctx", "transition-duration", "100ms, 200ms", &mut attrs).unwrap();
        apply_declaration("ctx", "transition-timing-function", "linear", &mut attrs).unwrap();
        let eff = attrs.effective_transitions();
        assert_eq!(eff.len(), 3);
        // Durations cycle over the property list (CSS repeat rule).
        assert_eq!(eff[0].duration_ms, 100);
        assert_eq!(eff[1].duration_ms, 200);
        assert_eq!(eff[2].duration_ms, 100);
        assert!(eff.iter().all(|t| t.easing == EasingIr::Linear));
    }

    #[test]
    fn shorthand_wins_over_longhands() {
        use crate::layout_ir::Attributes;
        let mut attrs = Attributes::default();
        apply_declaration("ctx", "transition-property", "bg", &mut attrs).unwrap();
        apply_declaration("ctx", "transition-duration", "999ms", &mut attrs).unwrap();
        apply_declaration("ctx", "transition", "opacity 150ms ease-out", &mut attrs).unwrap();
        let eff = attrs.effective_transitions();
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].property, TransitionPropertyIr::Opacity);
        assert_eq!(eff[0].duration_ms, 150);
    }

    #[test]
    fn accepts_seconds_duration() {
        let out = parse_transition("ctx", "transition", "opacity 0.25s").unwrap();
        assert_eq!(out[0].duration_ms, 250);
    }

    #[test]
    fn defaults_easing_to_ease_out() {
        let out = parse_transition("ctx", "transition", "opacity 200ms").unwrap();
        assert_eq!(out[0].easing, EasingIr::EaseOut);
    }

    #[test]
    fn rejects_missing_duration() {
        assert!(parse_transition("ctx", "transition", "opacity").is_err());
    }

    #[test]
    fn parses_cubic_bezier() {
        let out = parse_transition(
            "ctx",
            "transition",
            "opacity 200ms cubic-bezier(0.4, 0, 0.2, 1)",
        )
        .unwrap();
        assert_eq!(out[0].easing, EasingIr::CubicBezier(0.4, 0.0, 0.2, 1.0));
    }
}

#[cfg(test)]
mod cascade_origin_tests {
    use super::*;

    fn subject(compound: CompoundSelector) -> SelectorBuf {
        SelectorBuf {
            chain: vec![(Combinator::Subject, compound)],
        }
    }

    fn rule(selector: SelectorBuf, origin: Origin, source_order: usize) -> Rule {
        Rule {
            selectors: vec![selector],
            declarations: Vec::new(),
            origin,
            source_order,
            media: None,
            selector: LegacySelectorShim::default(),
        }
    }

    /// A user-agent (skin) `textarea:hover` rule has HIGHER specificity
    /// (0,1,1) than an author `.editor` rule (0,1,0), yet author origin
    /// dominates the cascade: the author rule must sort LAST (win). This
    /// is the notes-app "dark blue unreadable hover" fix -- author
    /// `.editor { bg: var(--surface) }` beats skin `textarea:hover`.
    #[test]
    fn author_class_beats_user_agent_pseudo_despite_specificity() {
        // Skin rule: `textarea:hover` at the user-agent origin.
        let ua = rule(
            subject(CompoundSelector {
                tag: Some("textarea".to_string()),
                id: None,
                classes: Vec::new(),
                pseudo_classes: vec![PseudoClass::Hover],
            }),
            Origin::UserAgent,
            0,
        );
        // Author rule: `.editor` at the author origin.
        let author = rule(
            subject(CompoundSelector {
                tag: None,
                id: None,
                classes: vec!["editor".to_string()],
                pseudo_classes: Vec::new(),
            }),
            Origin::Author,
            1,
        );

        // Sanity: the UA rule really is more specific, so without the
        // origin term it would (wrongly) win the sort.
        assert!(
            ua.selectors[0].specificity() > author.selectors[0].specificity(),
            "test premise: UA textarea:hover must out-specific author .editor"
        );

        let css = Stylesheet {
            rules: vec![ua, author],
        };
        // A hovered <textarea class="editor"> element.
        let me = ElementRef {
            tag: "textarea".to_string(),
            classes: vec!["editor".to_string()],
            id: None,
            child_index: 1,
            sibling_count: 1,
        };
        let matched =
            collect_matching_rules(&css, &MediaContext::default(), &me, &[], false, None, false);

        // Both rules match the element.
        assert_eq!(matched.len(), 2, "both rules should match");
        // Cascade sort is ascending -> last wins. Author (rule_idx 1)
        // must sort LAST despite the UA rule's higher specificity.
        assert_eq!(
            matched.last().unwrap().rule_idx,
            1,
            "author .editor must win over UA textarea:hover"
        );
        assert_eq!(matched.last().unwrap().origin, Origin::Author);
    }

    /// Within a single origin the existing (specificity, source_order)
    /// tie-break still applies -- origin only dominates ACROSS origins.
    #[test]
    fn specificity_still_wins_within_same_origin() {
        let low = rule(
            subject(CompoundSelector {
                tag: Some("textarea".to_string()),
                id: None,
                classes: Vec::new(),
                pseudo_classes: Vec::new(),
            }),
            Origin::Author,
            0,
        );
        let high = rule(
            subject(CompoundSelector {
                tag: None,
                id: None,
                classes: vec!["editor".to_string()],
                pseudo_classes: Vec::new(),
            }),
            Origin::Author,
            1,
        );
        let css = Stylesheet {
            rules: vec![low, high],
        };
        let me = ElementRef {
            tag: "textarea".to_string(),
            classes: vec!["editor".to_string()],
            id: None,
            child_index: 1,
            sibling_count: 1,
        };
        let matched =
            collect_matching_rules(&css, &MediaContext::default(), &me, &[], false, None, false);
        assert_eq!(matched.len(), 2);
        // Higher specificity `.editor` (rule_idx 1) wins within origin.
        assert_eq!(matched.last().unwrap().rule_idx, 1);
    }
}

#[cfg(test)]
mod query_surface_tests {
    use super::*;

    fn anc(tag: &str, classes: &[&str], id: Option<&str>) -> AncestorInfo {
        AncestorInfo::new(
            tag,
            classes.iter().map(|s| s.to_string()).collect(),
            id.map(|s| s.to_string()),
        )
    }

    fn one(src: &str) -> SelectorBuf {
        let list = parse_selector_list(src).unwrap();
        assert_eq!(list.len(), 1, "expected single selector for '{src}'");
        list.into_iter().next().unwrap()
    }

    #[test]
    fn parse_round_trips_basic_shapes() {
        assert_eq!(parse_selector_list("#save").unwrap().len(), 1);
        assert_eq!(parse_selector_list(".row").unwrap().len(), 1);
        assert_eq!(parse_selector_list("button").unwrap().len(), 1);
        assert_eq!(parse_selector_list("button.primary").unwrap().len(), 1);
        // Descendant and child chains yield multi-compound chains.
        assert_eq!(one(".card .row").chain.len(), 2);
        assert_eq!(one(".card > .row").chain.len(), 2);
        // Selector list splits on top-level commas.
        assert_eq!(parse_selector_list("#a, .b, c").unwrap().len(), 3);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_selector_list("").is_err());
        assert!(parse_selector_list("   ").is_err());
        assert!(parse_selector_list("#a,").is_err());
    }

    #[test]
    fn matches_id_class_tag() {
        let subject = anc("button", &["primary", "big"], Some("save"));
        assert!(selector_matches(&one("#save"), &subject, &[]));
        assert!(selector_matches(&one(".primary"), &subject, &[]));
        assert!(selector_matches(&one("button"), &subject, &[]));
        assert!(selector_matches(&one("button.primary"), &subject, &[]));
        assert!(selector_matches(&one(".primary.big"), &subject, &[]));
        assert!(!selector_matches(&one("#other"), &subject, &[]));
        assert!(!selector_matches(&one(".missing"), &subject, &[]));
        assert!(!selector_matches(&one("label"), &subject, &[]));
    }

    #[test]
    fn descendant_and_child_combinators() {
        // Tree: root.app > .card > button#save  (ancestors root-first).
        let ancestors = [anc("root", &["app"], None), anc("div", &["card"], None)];
        let subject = anc("button", &[], Some("save"));
        assert!(selector_matches(&one(".card button"), &subject, &ancestors));
        assert!(selector_matches(
            &one(".app .card #save"),
            &subject,
            &ancestors
        ));
        assert!(selector_matches(
            &one(".card > #save"),
            &subject,
            &ancestors
        ));
        // Child combinator binds to the immediate parent only.
        assert!(!selector_matches(
            &one(".app > #save"),
            &subject,
            &ancestors
        ));
        // Descendant that names a non-ancestor fails.
        assert!(!selector_matches(
            &one(".sidebar #save"),
            &subject,
            &ancestors
        ));
    }

    #[test]
    fn selector_matches_agrees_with_cascade_match_selector() {
        // Cross-check the public wrapper against the private matcher on
        // the same hand-built chain.
        let ancestors = [anc("root", &["theme-dark"], None)];
        let subject = anc("div", &["card"], None);
        let sel = one(".theme-dark .card");
        let me = subject.to_ref();
        let parents: Vec<ElementRef> = ancestors.iter().map(AncestorInfo::to_ref).collect();
        let via_private = match_selector(&sel, &me, &parents, true, None, false).is_some();
        let via_public = selector_matches(&sel, &subject, &ancestors);
        assert_eq!(via_private, via_public);
        assert!(via_public);
    }

    #[test]
    fn sibling_combinators_conservatively_fail() {
        let subject = anc("button", &["b"], None);
        let ancestors = [anc("root", &[], None)];
        // `+` and `~` end the chain on the subject; the matcher has no
        // sibling context, so these never match. Query inherits that.
        assert!(!selector_matches(&one(".a + .b"), &subject, &ancestors));
        assert!(!selector_matches(&one(".a ~ .b"), &subject, &ancestors));
    }

    #[test]
    fn root_pseudo_matches_only_root() {
        let root = anc("root", &[], None);
        assert!(selector_matches(&one(":root"), &root, &[]));
        let child = anc("div", &[], None);
        assert!(!selector_matches(
            &one(":root"),
            &child,
            &[anc("root", &[], None)]
        ));
    }
}

#[cfg(test)]
mod inline_style_tests {
    use super::*;

    #[test]
    fn inline_declaration_overrides_a_cascaded_value() {
        // Simulate the stylesheet cascade having resolved a blue color.
        let mut attrs = Attributes::default();
        apply_declaration("rule", "color", "#0000ff", &mut attrs).unwrap();
        assert_eq!(
            computed_property(&attrs, "color").as_deref(),
            Some("#0000ff")
        );
        // The inline layer (highest tier) beats it.
        assert!(apply_inline_declaration("color", "#ff0000", &mut attrs).unwrap());
        assert_eq!(
            computed_property(&attrs, "color").as_deref(),
            Some("#ff0000")
        );
    }

    #[test]
    fn computed_property_renders_common_props() {
        let mut attrs = Attributes::default();
        apply_inline_declaration("width", "120px", &mut attrs).unwrap();
        apply_inline_declaration("opacity", "0.5", &mut attrs).unwrap();
        assert_eq!(computed_property(&attrs, "width").as_deref(), Some("120px"));
        assert_eq!(computed_property(&attrs, "opacity").as_deref(), Some("0.5"));
        // An unmodeled property returns None rather than panicking.
        assert_eq!(computed_property(&attrs, "z-index"), None);
    }
}
