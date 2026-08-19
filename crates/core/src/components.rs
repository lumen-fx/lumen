//! ECS component primitives.
//!
//! Hierarchy components [`ChildOf`] and [`Children`] are re-exported from `bevy_ecs::hierarchy` via [`crate::prelude`].

use crate::time::{Duration, Instant};
use bevy_ecs::prelude::*;
use glam::Vec2;
use std::sync::Arc;

/// Layout-resolved absolute position and size in logical pixels.
///
/// - Written by the `LayoutSync` stage and read by `Render`.
/// - Mutate via [`Style`]; the layout engine recomputes `Transform`.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Transform {
    /// Absolute origin (top-left) in window coordinates.
    pub absolute: Vec2,
    /// Computed size.
    pub size: Vec2,
    /// First-line text baseline y-offset from `absolute.y` (W5.9).
    /// `Some(y)` for text leaves that report a baseline from the
    /// shaper / measure function. `None` for non-text leaves and for
    /// text that hasn't been measured yet.
    ///
    /// Consumed by [`FlexAlign::Baseline`] sibling alignment and by
    /// AccessKit's text-position reporting. The taffy backend reads this to
    /// wire its measure callback's baseline; the renderer uses it to align
    /// mixed-size inline runs.
    pub baseline_y: Option<f32>,
}

impl Transform {
    /// Construct a [`Transform`] with `baseline_y = None`. Most spawn
    /// sites use this; text leaves overwrite `baseline_y` later from
    /// the measure-fn output.
    pub fn new(absolute: Vec2, size: Vec2) -> Self {
        Self {
            absolute,
            size,
            baseline_y: None,
        }
    }
}

impl From<(Vec2, Vec2)> for Transform {
    fn from((absolute, size): (Vec2, Vec2)) -> Self {
        Self::new(absolute, size)
    }
}

/// Monotonic counter bumped whenever a class / palette / media-feature
/// change invalidates computed style for at least one entity. Downstream
/// consumers (the cascade re-resolver, the `Visuals` recompute) observe
/// `Changed<StyleVersion>` and re-walk only the entities the invalidation
/// cache flagged.
///
/// Here rather than with the cascade because the writers and the readers are
/// in different crates: a scripted class edit bumps it from the scene layer,
/// and the host that owns the cascade reads it. A counter both can name has
/// to sit under both.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleVersion(pub u64);

impl StyleVersion {
    /// Record that something a rule could match has changed.
    ///
    /// Takes the world rather than the resource: the writers are exclusive
    /// systems, and an app assembled without a cascade (a browser page, where
    /// the page's own CSS engine resolves rules) carries no counter until the
    /// first edit puts one there.
    pub fn bump(world: &mut bevy_ecs::world::World) {
        match world.get_resource_mut::<Self>() {
            Some(mut version) => version.0 = version.0.wrapping_add(1),
            None => world.insert_resource(Self(1)),
        }
    }
}

/// Framework-internal style record. Smaller and renderer-agnostic compared with `taffy::Style`; the layout impl crate translates it into its backend type.
/// New fields require a corresponding `dirty_mask` bit allocation in `lumen/src/style_mask.rs`.
///
/// W5.9 made this `Clone` (no longer `Copy`) because [`GridTemplate`]
/// owns track-list vectors. Hot paths that previously took `Style` by
/// value should switch to `&Style` - the layout backend already does.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct Style {
    /// CSS `display` mode. Selects the layout algorithm used for the
    /// element's children (W5.9). [`Display::Flex`] is the default
    /// flexbox container; [`Display::Grid`] enables CSS Grid layout;
    /// [`Display::None`] hides the element + its subtree.
    pub display: Display,
    /// Width along the cross axis.
    pub width: Length,
    /// Height along the main axis.
    pub height: Length,
    /// Flex direction.
    pub flex_direction: FlexDirection,
    /// Padding on each edge.
    pub padding: Edges,
    /// Margin on each edge.
    pub margin: Edges,
    /// Spacing inserted between adjacent rows / columns of a flex or
    /// grid container. CSS `gap` / `row-gap` / `column-gap`. W5.9
    /// split the previous single-scalar `gap: f32` into a per-axis
    /// `Gap { row, column }`; existing call sites use the
    /// `Gap::from(f32)` shorthand to keep the same number on both
    /// axes.
    pub gap: Gap,
    /// Flex-grow factor. Mirrors CSS `flex-grow`. 0 = don't grow.
    pub grow: f32,
    /// Cross-axis alignment (CSS `align-items`).
    pub align: FlexAlign,
    /// Main-axis distribution (CSS `justify-content`).
    pub justify: FlexJustify,
    /// Per-item override of the container's [`Self::align`]. `None`
    /// inherits. Mirrors CSS `align-self`. W5.9: includes
    /// [`FlexAlign::Baseline`] for mixed-size inline runs.
    pub align_self: Option<FlexAlign>,
    /// Grid-only: alignment of items along the inline axis. Mirrors
    /// CSS `justify-items`. `None` = `Stretch`. Ignored under
    /// [`Display::Flex`].
    pub justify_items: Option<FlexAlign>,
    /// Grid-only: per-item override of the parent's `justify_items`.
    /// Mirrors CSS `justify-self`.
    pub justify_self: Option<FlexAlign>,
    /// Grid template (rows / columns). `None` under
    /// [`Display::Flex`]; required under [`Display::Grid`]. The
    /// taffy backend lowers this into `grid_template_rows` /
    /// `grid_template_columns`.
    pub grid_template: Option<GridTemplate>,
    /// Grid-item: row line range `(start, end)` - CSS `grid-row`.
    /// Each is a 1-based positive integer line number; `0` =
    /// auto-placement.
    pub grid_row: (i16, i16),
    /// Grid-item: column line range. See [`Self::grid_row`].
    pub grid_column: (i16, i16),
    /// Positioning mode. `Relative` (default) participates in flex flow;
    /// `Absolute` lifts the entity out of the flow and offsets it by
    /// [`Self::inset`] against the nearest positioned ancestor.
    pub position: Position,
    /// Distance from each edge when [`Self::position`] is `Absolute`.
    pub inset: Edges,
    /// Minimum width (after content). `Auto` = unconstrained.
    pub min_width: Length,
    /// Minimum height.
    pub min_height: Length,
    /// Maximum width. `Auto` = unbounded.
    pub max_width: Length,
    /// Maximum height.
    pub max_height: Length,
    /// `width / height` ratio constraint. `None` = none.
    pub aspect_ratio: Option<f32>,
    /// Per-axis overflow control. `Visible` clips nothing; `Hidden` clips
    /// children to the box; `Scroll` clips + the entity becomes
    /// scrollable.
    pub overflow_x: Overflow,
    /// See [`Self::overflow_x`].
    pub overflow_y: Overflow,
    /// Flex-shrink factor. Mirrors CSS `flex-shrink`; default `1.0`
    /// (items shrink to fit their line, per CSS).
    pub shrink: f32,
    /// Flex-basis - the main-axis size before free space distribution.
    /// Mirrors CSS `flex-basis`; default [`Length::Auto`].
    pub basis: Length,
    /// Line-wrapping mode for flex containers. Mirrors CSS `flex-wrap`;
    /// default [`FlexWrap::NoWrap`].
    pub flex_wrap: FlexWrap,
    /// Cross-axis distribution of *lines* in a multi-line flex container
    /// (or of tracks in grid). Mirrors CSS `align-content`. `None` =
    /// backend default (`stretch`-like). Only observable when
    /// [`Self::flex_wrap`] allows multiple lines.
    pub align_content: Option<AlignContent>,
    /// Border widths per edge in logical pixels. Mirrors CSS
    /// `border-width` with the resolved `border-style` folded in: a side
    /// whose style is `none` carries width `0` here (per CSS, the
    /// computed border-width of a `none` side is zero). Consumes space
    /// per the CSS box model; paint lives in [`Visuals::border`].
    pub border: Edges,
    /// CSS `box-sizing`. Default [`BoxSizing::BorderBox`] (Lumen's UA
    /// default - explicit sizes include padding + border, matching what
    /// virtually every real-world stylesheet opts into).
    pub box_sizing: BoxSizing,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            display: Display::default(),
            width: Length::Auto,
            height: Length::Auto,
            flex_direction: FlexDirection::default(),
            padding: Edges::default(),
            margin: Edges::default(),
            gap: Gap::default(),
            grow: 0.0,
            align: FlexAlign::default(),
            justify: FlexJustify::default(),
            align_self: None,
            justify_items: None,
            justify_self: None,
            grid_template: None,
            grid_row: (0, 0),
            grid_column: (0, 0),
            position: Position::default(),
            inset: Edges::default(),
            min_width: Length::Auto,
            min_height: Length::Auto,
            max_width: Length::Auto,
            max_height: Length::Auto,
            aspect_ratio: None,
            overflow_x: Overflow::default(),
            overflow_y: Overflow::default(),
            // CSS initial value: items shrink by default.
            shrink: 1.0,
            basis: Length::Auto,
            flex_wrap: FlexWrap::default(),
            align_content: None,
            border: Edges::default(),
            box_sizing: BoxSizing::default(),
        }
    }
}

/// CSS `flex-wrap` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlexWrap {
    /// Single line (default).
    #[default]
    NoWrap,
    /// Wrap onto additional lines along the cross axis.
    Wrap,
    /// Wrap with reversed cross-axis line order.
    WrapReverse,
}

/// CSS `align-content` values - distribution of flex lines / grid
/// tracks along the cross axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignContent {
    /// Pack lines at the start.
    Start,
    /// Pack lines at the end.
    End,
    /// Pack lines at the center.
    Center,
    /// Stretch lines to fill the cross axis (CSS initial value).
    Stretch,
    /// Even gaps between lines, none at the edges.
    SpaceBetween,
    /// Half-size gaps at the edges.
    SpaceAround,
    /// Equal gaps everywhere including edges.
    SpaceEvenly,
}

/// CSS `box-sizing` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BoxSizing {
    /// `width` / `height` include padding and border (Lumen UA default;
    /// also taffy's default).
    #[default]
    BorderBox,
    /// `width` / `height` size the content box only (the CSS-spec
    /// initial value; opt back in with `box-sizing: content-box`).
    ContentBox,
}

/// CSS `z-index` - paint-order override among siblings. Higher values
/// paint later (on top). Missing component = `auto` (`0`, document
/// order). Consumed by `render_world::build_parent_map`, which
/// stable-sorts each entity's child list by `(z_index, document order)`
/// before assigning pre-order paint ranks - so an element with a higher
/// `z-index` (and its whole subtree) paints above its siblings, matching
/// CSS stacking behaviour within one parent stacking context.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ZIndex(pub i32);

/// CSS `display` value. Selects the layout algorithm for the
/// element's children (W5.9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Display {
    /// Flexbox layout (default).
    #[default]
    Flex,
    /// CSS Grid layout. Pairs with [`Style::grid_template`] /
    /// [`Style::grid_row`] / [`Style::grid_column`].
    Grid,
    /// Hidden - element + its subtree generate no boxes. Distinct
    /// from `Visible(false)` which keeps layout slots; `Display::None`
    /// collapses the box entirely.
    None,
}

/// CSS `gap` / `row-gap` / `column-gap` - per-axis spacing between
/// adjacent rows / columns of a flex or grid container (W5.9). The
/// previous single-scalar `gap: f32` is reachable via
/// `Gap::from(value)` for back-compat with existing call sites.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gap {
    /// Vertical spacing between adjacent rows (CSS `row-gap`).
    pub row: f32,
    /// Horizontal spacing between adjacent columns (CSS `column-gap`).
    pub column: f32,
    /// CSS percent unit for the row gap. When `Some(pct)` the row gap
    /// resolves as `pct%` of the container's content-box height (taffy
    /// receives `LengthPercentage::percent`); `row` is ignored.
    pub row_pct: Option<f32>,
    /// See [`Self::row_pct`]; resolves against content-box width.
    pub column_pct: Option<f32>,
}

impl From<f32> for Gap {
    /// CSS shorthand: `gap: <v>` sets both axes.
    fn from(v: f32) -> Self {
        Self::all(v)
    }
}

impl Gap {
    /// Uniform gap on both axes.
    pub const fn all(v: f32) -> Self {
        Self {
            row: v,
            column: v,
            row_pct: None,
            column_pct: None,
        }
    }
}

/// One track in a grid template - CSS Grid L1 subset.
///
/// Authored values lower to taffy's `MinMax<MinTrackSizingFunction,
/// MaxTrackSizingFunction>` at the layout boundary. The recursive
/// [`Self::MinMax`] arm boxes its inner pair so the enum's size stays
/// bounded.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum TrackSize {
    /// Fixed length in logical pixels (`<N>px`).
    Fixed(f32),
    /// CSS `auto` - sized by intrinsic content.
    #[default]
    Auto,
    /// Flex factor (`<N>fr`) - proportional share of free space.
    Fr(f32),
    /// `min-content` - narrowest non-overflowing size.
    MinContent,
    /// `max-content` - widest fitting all content on one line.
    MaxContent,
    /// `minmax(min, max)` - independent min / max sizing functions.
    MinMax(Box<TrackSize>, Box<TrackSize>),
}

/// CSS Grid template - explicit `grid-template-rows` + `-columns`
/// track lists (W5.9). Implicit-grid sizing is taffy's default
/// behaviour for cells placed past the explicit grid.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridTemplate {
    /// `grid-template-rows`.
    pub rows: Vec<TrackSize>,
    /// `grid-template-columns`.
    pub columns: Vec<TrackSize>,
}

/// CSS `position` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Position {
    /// In-flow positioning (default).
    #[default]
    Relative,
    /// Out-of-flow; offset by `inset` against the nearest positioned
    /// ancestor (or the viewport if none).
    Absolute,
}

/// CSS `overflow` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    /// Children paint outside the box (default).
    #[default]
    Visible,
    /// Children clipped at the box edge.
    Hidden,
    /// Clipped + scrollable (paired with the `<scroll>` interaction
    /// primitive for now; declarative scroll-on-overflow lands later).
    Scroll,
}

/// Cross-axis alignment.
///
/// Authored via `align="..."` / `align-items` / `justify-items` /
/// `align-self` / `justify-self`. W5.9 added [`Self::Baseline`] for
/// CSS Grid + mixed-size inline flex runs (items' first text
/// baselines align across the cross axis).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlexAlign {
    /// Flex-start.
    Start,
    /// Flex-end.
    End,
    /// Centered.
    Center,
    /// Stretch - default.
    #[default]
    Stretch,
    /// Baseline alignment (W5.9). Items' first-line text baselines
    /// are aligned along the cross axis (flex) or the block axis
    /// (grid). Falls back to `Start` when none of the items expose a
    /// baseline.
    Baseline,
}

/// Main-axis distribution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlexJustify {
    /// Pack at the start - default.
    #[default]
    Start,
    /// Pack at the end.
    End,
    /// Pack at the center.
    Center,
    /// Space between siblings, no edge padding.
    SpaceBetween,
    /// Space around siblings, half-step at edges.
    SpaceAround,
    /// Even spacing, edges and gaps equal.
    SpaceEvenly,
}

/// One-dimensional length specifier.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Length {
    /// Computed by the layout engine.
    #[default]
    Auto,
    /// Fixed pixel length.
    Px(f32),
    /// Percentage of parent's resolved dimension.
    Percent(f32),
}

/// Flexbox main-axis direction. Includes the logical *Reverse variants so
/// the layout backend can flip the inline axis when [`ResolvedDirection`]
/// is [`LayoutDirection::Rtl`] (W5.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlexDirection {
    /// Children flow along the inline axis (LTR: left->right).
    #[default]
    Row,
    /// Children flow along the block axis (top->bottom).
    Column,
    /// Children flow along the inline axis in reverse (LTR: right->left).
    /// W5.5: a plain `Row` under RTL resolves into this at the
    /// layout-backend boundary so authors don't have to think about
    /// mirroring.
    RowReverse,
    /// Children flow along the block axis in reverse (bottom->top).
    ColumnReverse,
}

impl FlexDirection {
    /// Resolve the logical flex direction under a concrete writing
    /// direction. Authors keep writing `Row`; the backend calls this so
    /// `Row` flips to [`Self::RowReverse`] under
    /// [`LayoutDirection::Rtl`] (mirrors Qt / Web flex semantics).
    /// `Column` / `*Reverse` pass through unchanged because the block
    /// axis is not affected by writing direction.
    pub const fn resolved(self, dir: LayoutDirection) -> Self {
        match (self, dir) {
            (Self::Row, LayoutDirection::Rtl) => Self::RowReverse,
            (Self::RowReverse, LayoutDirection::Rtl) => Self::Row,
            // Auto resolves at the cascade resolver before this is
            // called; treat it like Ltr as a defensive fallback.
            _ => self,
        }
    }
}

/// Per-edge length values (padding, margin, border). Physical edges
/// (`left` / `right` / `top` / `bottom`) carry the authored values; the
/// optional `*_inline_*` / `*_block_*` fields override them per writing
/// direction (W5.5 - CSS Logical Properties Level 1 subset).
///
/// `Edges::resolved(dir)` collapses the logical fields onto the
/// physical ones for the layout backend. When a logical override is
/// `None` the physical field wins (back-compat for every existing
/// callsite).
///
/// `PartialEq` is hand-written with NaN-tolerant semantics: `NaN` is the
/// canonical "unset / auto" sentinel for inset edges (`edges_to_lpa`
/// NaN-checks), and IEEE `NaN != NaN` made any two identical auto-inset
/// styles compare unequal - which defeated every "did the Style actually
/// change?" gate downstream (the taffy style cache re-pushed all
/// virtualized rows on every dirty tick; equality-gated `Style` inserts
/// re-fired forever).
#[derive(Clone, Copy, Debug, Default)]
pub struct Edges {
    /// Left edge.
    pub left: f32,
    /// Right edge.
    pub right: f32,
    /// Top edge.
    pub top: f32,
    /// Bottom edge.
    pub bottom: f32,
    /// `*-inline-start` - maps to `left` under LTR, `right` under RTL.
    pub inline_start: Option<f32>,
    /// `*-inline-end` - maps to `right` under LTR, `left` under RTL.
    pub inline_end: Option<f32>,
    /// `*-block-start` - alias for `top` (no vertical writing modes yet).
    pub block_start: Option<f32>,
    /// `*-block-end` - alias for `bottom`.
    pub block_end: Option<f32>,
    /// CSS percent unit for the left edge. When `Some(pct)` the side
    /// resolves as `pct%` per CSS (padding/margin percentages resolve
    /// against the containing block's *width*; the layout backend hands
    /// taffy a `LengthPercentage::percent`) and the px field for the
    /// side is ignored. `None` = the px field is authoritative (fast
    /// path, unchanged behaviour).
    pub pct_left: Option<f32>,
    /// See [`Self::pct_left`].
    pub pct_right: Option<f32>,
    /// See [`Self::pct_left`].
    pub pct_top: Option<f32>,
    /// See [`Self::pct_left`].
    pub pct_bottom: Option<f32>,
}

/// NaN-tolerant float equality: two NaNs (the "auto" sentinel) are equal.
fn eq_nan(a: f32, b: f32) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// See [`eq_nan`]; `None == None`, `Some(a) == Some(b)` iff `eq_nan`.
fn eq_nan_opt(a: Option<f32>, b: Option<f32>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => eq_nan(a, b),
        _ => false,
    }
}

impl PartialEq for Edges {
    fn eq(&self, other: &Self) -> bool {
        eq_nan(self.left, other.left)
            && eq_nan(self.right, other.right)
            && eq_nan(self.top, other.top)
            && eq_nan(self.bottom, other.bottom)
            && eq_nan_opt(self.inline_start, other.inline_start)
            && eq_nan_opt(self.inline_end, other.inline_end)
            && eq_nan_opt(self.block_start, other.block_start)
            && eq_nan_opt(self.block_end, other.block_end)
            && eq_nan_opt(self.pct_left, other.pct_left)
            && eq_nan_opt(self.pct_right, other.pct_right)
            && eq_nan_opt(self.pct_top, other.pct_top)
            && eq_nan_opt(self.pct_bottom, other.pct_bottom)
    }
}

impl Edges {
    /// Uniform physical edges.
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

    /// Resolve logical overrides onto the physical sides under `dir`.
    /// Returns a fresh [`Edges`] whose `left` / `right` / `top` /
    /// `bottom` are the values the layout backend should use; the
    /// `Option` fields are cleared so a second call is idempotent.
    ///
    /// - `inline_start` writes `left` (LTR) or `right` (RTL).
    /// - `inline_end`   writes `right` (LTR) or `left` (RTL).
    /// - `block_start` writes `top`; `block_end` writes `bottom`.
    /// - [`LayoutDirection::Auto`] is treated as LTR (the cascade
    ///   resolver should have stamped a concrete direction before this
    ///   is reached; the fallback keeps this fn total).
    pub fn resolved(&self, dir: LayoutDirection) -> Self {
        let rtl = matches!(dir, LayoutDirection::Rtl);
        let mut out = *self;
        if let Some(v) = self.inline_start {
            if rtl {
                out.right = v;
            } else {
                out.left = v;
            }
        }
        if let Some(v) = self.inline_end {
            if rtl {
                out.left = v;
            } else {
                out.right = v;
            }
        }
        if let Some(v) = self.block_start {
            out.top = v;
        }
        if let Some(v) = self.block_end {
            out.bottom = v;
        }
        out.inline_start = None;
        out.inline_end = None;
        out.block_start = None;
        out.block_end = None;
        out
    }
}

/// Marker: this entity's layout (or one of its ancestors') has changed.
///
/// - Set on the dirty entity and propagated upward until the nearest [`RelayoutBoundary`] ancestor.
/// - Cleared by `LayoutSync` after recomputation.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DirtyLayout;

/// Per-cache memory caps in megabytes, honoured by per-tick LRU eviction across the image, shape, scene-fragment, and GPU-texture caches.
/// Each cache exposes `bytes_used` and `evict_until(target_bytes)`; a shared system reduces the live total below the cap.
/// Defaults target desktop-class machines; override via `lumen.toml [perf]`.
#[derive(Resource, Clone, Copy, Debug)]
pub struct MemoryBudget {
    /// Decoded image cache cap in MB (CPU-side RGBA8 bytes).
    pub images_mb: u32,
    /// Text shape-result cache cap measured in entries.
    pub shape_entries: u32,
    /// Vello scene-fragment cache cap measured in entries.
    pub scene_fragments: u32,
}

impl Default for MemoryBudget {
    fn default() -> Self {
        Self {
            images_mb: 64,
            shape_entries: 512,
            scene_fragments: 256,
        }
    }
}

/// Marker indicating that the entity's size is fully determined by parent-imposed constraints (a `<scroll>` clip box, fixed `width`/`height`, or explicit `layout-boundary` attribute).
///
/// - `propagate_dirty_layout` halts at the nearest such ancestor.
/// - `sync_layout` recomputes within the subtree rooted at the boundary instead of from the absolute root.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct RelayoutBoundary;

/// Tab-navigation boundary. While the carrier is visible (no [`Visible`] component or [`Visible(true)`]), Tab / Shift-Tab cycling stays within its descendants.
/// Applied by `<dialog>` to trap focus; cycling tolerates nested visible boundaries by keeping focus inside the active one.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FocusBoundary;

/// Render gate. When present and set to `false`, every extract fn skips the entity (no rect, text, image, outline, or shadow), while layout still allocates space for it.
///
/// - The absent component is equivalent to [`Visible(true)`].
/// - Used by `<if mode="hide">` to keep descendant state (focus, scroll, per-row signals) across a hide/show flip.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visible(pub bool);

impl Default for Visible {
    fn default() -> Self {
        Self(true)
    }
}

/// Marker: this entity's accessibility-relevant state changed.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DirtyA11y;

/// Text payload for text-bearing entities. Stored as its own component so high-frequency keystroke mutation does not bump change detection on the cold [`TextStyle`] fields.
#[derive(Component, Clone, Debug, Default)]
pub struct TextContent(pub String);

// Reference [`crate::traits::Bindable`] impl shipped as the foundation reference port (see plan section 2 acceptance bar).
// Wave 1 wires the auto-register call and migrates `apply_text_bindings` onto `PropertyStore::drain_dirty`.
impl crate::traits::Bindable for TextContent {
    const NAME: &'static str = "text";
    type Value = std::sync::Arc<str>;
    fn read(&self) -> Self::Value {
        std::sync::Arc::<str>::from(self.0.as_str())
    }
    fn write(&mut self, v: Self::Value) {
        self.0 = v.to_string();
    }
}

/// Tab navigation order. Lower values focus first. Negative = not in tab chain.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct TabIndex(pub i32);

/// In-progress IME composition state.
#[derive(Component, Clone, Debug, Default)]
pub struct ImeState {
    /// The current preedit string (composition buffer).
    pub preedit: String,
    /// Caret position within `preedit`, in bytes.
    pub cursor: usize,
}

/// Marker: this entity is an editable text input.
///
/// - Spawned by the `<input>` markup tag.
/// - Gates [`lumen_input::type_into_focused`]; entities without this marker do not receive typing input even when focused.
/// - `placeholder` is shown verbatim while [`TextContent`] is empty.
/// - `cursor` indexes [`TextContent`] in bytes; ArrowLeft / ArrowRight move on Unicode boundaries.
#[derive(Component, Clone, Debug, Default)]
pub struct TextInput {
    /// Hint text shown when the input is empty.
    pub placeholder: String,
    /// Caret byte offset within the entity's [`TextContent`]; clamped to `0..=text.len()` by the input router.
    pub cursor: usize,
    /// Selection anchor.
    ///
    /// - `None`: no selection; the cursor alone marks the insertion point.
    /// - `Some(a)`: `min(a, cursor)..max(a, cursor)` is selected and highlighted.
    /// - Populated by Shift+Arrow / Shift+Home / Shift+End / Ctrl+A; collapsed to `None` on any non-shifted cursor move.
    pub selection_anchor: Option<usize>,
    /// Whether bare Enter inserts `\n`.
    ///
    /// - `true` (e.g. `<textarea>`): Enter inserts; Shift+Enter still commits.
    /// - `false` (single-line `<input>`): Enter commits via [`crate::input::TextInputCommitted`].
    pub multiline: bool,
}

/// How a text input *renders* its content - Qt's
/// [`QLineEdit::EchoMode`](https://doc.qt.io/qt-6/qlineedit.html#EchoMode-enum).
///
/// The mode is a **display + clipboard** policy only: the underlying
/// [`TextContent`] / [`crate::text_model::TextBuffer`] always holds the
/// real plaintext so editing, caret motion, undo, and IME keep operating
/// on the true value. `extract_text` substitutes the display glyphs;
/// `lumen_input::type_into_focused` gates the clipboard.
///
/// - Spawned by `<input type="password">` markup (wired by the
///   reconciler onto the same entity that carries [`TextInput`]).
/// - Absent component => [`EchoMode::Normal`].
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EchoMode {
    /// Show the text verbatim (default). Copy / cut are allowed.
    #[default]
    Normal,
    /// Render every scalar as [`PASSWORD_MASK_CHAR`]; the plaintext stays
    /// in the buffer for editing. Copy / cut are **blocked** (Qt disables
    /// them for non-`Normal` echo modes so a password can't be lifted off
    /// the clipboard); paste and select-all still work.
    Password,
    /// Render nothing at all (not even length). Same clipboard block as
    /// [`EchoMode::Password`].
    NoEcho,
}

impl EchoMode {
    /// `true` when the mode conceals the content: copy must be suppressed
    /// and the glyphs masked.
    pub fn is_concealed(self) -> bool {
        matches!(self, EchoMode::Password | EchoMode::NoEcho)
    }

    /// The run actually drawn for the plaintext `plain`.
    ///
    /// Measuring, hit-testing, and drawing must all agree on one string, so
    /// this is what the shaping producer shapes for a concealed field.
    pub fn display_string(self, plain: &str) -> std::borrow::Cow<'_, str> {
        match self {
            EchoMode::Normal => std::borrow::Cow::Borrowed(plain),
            EchoMode::NoEcho => std::borrow::Cow::Borrowed(""),
            EchoMode::Password => std::borrow::Cow::Owned(
                PASSWORD_MASK_CHAR.to_string().repeat(plain.chars().count()),
            ),
        }
    }

    /// Byte offset into [`Self::display_string`] for a plaintext byte
    /// offset. Snaps `plain_byte` down to a code point boundary first.
    pub fn display_offset(self, plain: &str, plain_byte: usize) -> usize {
        match self {
            EchoMode::Normal => plain_byte,
            EchoMode::NoEcho => 0,
            EchoMode::Password => {
                let mut b = plain_byte.min(plain.len());
                while b > 0 && !plain.is_char_boundary(b) {
                    b -= 1;
                }
                plain[..b].chars().count() * PASSWORD_MASK_CHAR.len_utf8()
            }
        }
    }

    /// Inverse of [`Self::display_offset`]: plaintext byte offset for a byte
    /// offset into the displayed run.
    pub fn plain_offset(self, plain: &str, display_byte: usize) -> usize {
        match self {
            EchoMode::Normal => display_byte,
            EchoMode::NoEcho => 0,
            EchoMode::Password => {
                let scalars = display_byte / PASSWORD_MASK_CHAR.len_utf8();
                plain
                    .char_indices()
                    .nth(scalars)
                    .map(|(i, _)| i)
                    .unwrap_or(plain.len())
            }
        }
    }
}

/// Default glyph substituted for each scalar under [`EchoMode::Password`]:
/// U+2022 BULLET, the platform password convention Qt and the web use.
/// This is the single Rust fallback, used when no [`PasswordCharacter`]
/// override is present; the CSS `password-character` property authors
/// that override per skin.
pub const PASSWORD_MASK_CHAR: char = '\u{2022}';

/// Per-entity override for [`PASSWORD_MASK_CHAR`] (`password-character`
/// CSS property). Split off as its own tiny component - rather than a
/// field on [`TextInputPaint`] - so adding it never touches that
/// component's existing struct literals elsewhere in the tree (same
/// reasoning as `TextInputPaint`'s own doc comment). Absent =>
/// [`PASSWORD_MASK_CHAR`]. Only meaningful on `<input>` / `<textarea>`.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct PasswordCharacter(pub char);

impl Default for PasswordCharacter {
    fn default() -> Self {
        Self(PASSWORD_MASK_CHAR)
    }
}

/// Default text-input caret stroke width, in logical pixels (`caret-width`
/// CSS property). The single Rust fallback, used when no [`CaretWidth`]
/// override is present; render paths scale this by the active DPR
/// themselves.
pub const CARET_WIDTH_PX: f32 = 2.0;

/// Per-entity override for [`CARET_WIDTH_PX`] (`caret-width` CSS
/// property). Split off as its own tiny component for the same reason as
/// [`PasswordCharacter`]. Absent => [`CARET_WIDTH_PX`]. Only meaningful on
/// `<input>` / `<textarea>`.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct CaretWidth(pub f32);

impl Default for CaretWidth {
    fn default() -> Self {
        Self(CARET_WIDTH_PX)
    }
}

/// Default CSS `line-height: normal` multiplier - the single Rust
/// fallback used wherever no `line-height` value reaches the layout /
/// shaping / paint path. Common browsers use ~1.2; Lumen matches that.
/// This is the sole line-height ratio in the codebase; [`text_block_top`]
/// and [`text_baseline_in_line`] take the resolved line height (see
/// [`resolve_line_height`]) rather than re-deriving it from `size_px` and
/// a hardcoded factor, so an authored CSS `line-height` moves the text
/// block and baseline the same way it moves everything else.
pub const DEFAULT_LINE_HEIGHT_MULTIPLIER: f32 = 1.2;

/// Resolved CSS `line-height`: either a multiplier of the element's font
/// size (unitless, e.g. `line-height: 1.5`) or an absolute value in
/// logical pixels (`line-height: 24px`).
///
/// Mirrors `lumen_ir::layout_ir::LineHeightSpec` field-for-field;
/// duplicated here because `lumen-core` cannot depend on `lumen-ir`
/// (`lumen-ir` already depends on `lumen-core`, so the reverse edge would
/// cycle). The runtime converts between the two 1:1 at the IR/ECS
/// boundary (spawn, restyle).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeightSpec {
    /// Multiple of the resolved font size.
    Multiplier(f32),
    /// Absolute value in logical pixels.
    Px(f32),
}

impl LineHeightSpec {
    /// Resolve against a font size in logical pixels.
    pub fn resolve(self, size_px: f32) -> f32 {
        match self {
            LineHeightSpec::Multiplier(m) => size_px * m,
            LineHeightSpec::Px(px) => px,
        }
    }
}

/// Resolve a possibly-absent CSS `line-height` against `size_px`, falling
/// back to [`DEFAULT_LINE_HEIGHT_MULTIPLIER`] (`line-height: normal`) when
/// no value was authored. The single fallback-consumption point every
/// line-height-aware call site outside this module should route through,
/// rather than re-deriving `size_px * 1.2` locally.
pub fn resolve_line_height(spec: Option<LineHeightSpec>, size_px: f32) -> f32 {
    spec.map(|s| s.resolve(size_px))
        .unwrap_or(size_px * DEFAULT_LINE_HEIGHT_MULTIPLIER)
}

/// Cap height as a multiple of the font size, used to optically center a
/// line inside its line box. A font-metric approximation, not a CSS
/// `line-height` quantity, so it stays a fixed ratio rather than routing
/// through [`resolve_line_height`].
const TEXT_CAP_HEIGHT_FACTOR: f32 = 0.72;

/// Offset from the inner content box top to the top of the FIRST line box,
/// in logical pixels. `line_height` is the resolved CSS line height (see
/// [`resolve_line_height`]) - the caller passes
/// `resolve_line_height(style.line_height, size_px)` so an authored
/// `line-height` moves the block origin the same way it moves the line
/// box.
///
/// A lone line centers in the inner box, which is what `QLineEdit` does
/// with its `lineRect`. A stacked block starts at the top, as every
/// multi-line editor does, so line `i` occupies
/// `[top + i * line_height, top + (i + 1) * line_height)`; that is the
/// band `TextGeometry::x_to_byte` resolves a pointer y against.
///
/// `stacked` is true for a text area (which stays top-aligned however
/// little it holds, so the first newline does not make the content jump)
/// and for any run that already occupies more than one line.
///
/// This is the single origin the drawn baseline and the hit test share; the
/// layout producer evaluates it against the SHAPED (soft-wrap aware) line
/// count and publishes the result as [`TextBlockOrigin`].
pub fn text_block_top(inner_h: f32, line_height: f32, stacked: bool) -> f32 {
    if stacked {
        0.0
    } else {
        (inner_h - line_height) / 2.0
    }
}

/// Baseline offset of a line from the top of its own line box, in logical
/// pixels. Centers the cap height (a `size_px`-derived font metric) inside
/// the resolved `line_height` (see [`resolve_line_height`]).
pub fn text_baseline_in_line(size_px: f32, line_height: f32) -> f32 {
    (line_height + size_px * TEXT_CAP_HEIGHT_FACTOR) / 2.0
}

/// Published vertical origin of an entity's text block (see
/// [`text_block_top`]).
///
/// Written by the layout crate's shaping producer next to `ShapedText`, so
/// it reflects the soft-wrapped line count rather than the `\n` count. Read
/// by `extract_text` for the drawn baseline and by `lumen-input` for the
/// pointer hit test; both fall back to [`text_block_top`] over the logical
/// line count when the producer has not run.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct TextBlockOrigin {
    /// Offset from the inner content box top to the first line box top.
    pub top: f32,
}

/// Per-input content scroll offset that keeps the caret visible inside
/// the field box (W2 text-editing core).
///
/// - Written by the runtime's caret-keep-visible system (lumenc) from
///   the measured caret position; absent => text draws from the field
///   origin (legacy behavior).
/// - `offset.x` shifts the text run left by that many logical pixels;
///   `offset.y` does the same vertically for multiline inputs.
/// - Consumed by `extract_text`, which subtracts it from the emitted
///   run origin so caret / selection / glyphs all shift together.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct TextInputScroll {
    /// Content offset in logical pixels (positive = content scrolled
    /// left/up so later text is visible).
    pub offset: Vec2,
}

/// Caret blink phase, shared main-world resource (W2 text-editing core).
///
/// - Toggled by `lumen_text_edit::caret_blink` on a [`Self::period`]
///   cadence while a [`TextInput`] holds focus; reset to visible on any
///   edit or caret move.
/// - Read by `extract_text`: when `visible` is `false` the caret byte is
///   withheld from the extracted run, so the renderer paints no bar.
/// - Absent resource => caret always visible (headless / embedder path).
#[derive(bevy_ecs::resource::Resource, Clone, Copy, Debug)]
pub struct CaretBlink {
    /// Whether the caret is currently in the visible half of the phase.
    pub visible: bool,
    /// Start of the current blink phase; elapsed time against
    /// [`Self::period`] selects the half-cycle.
    pub phase: Instant,
    /// Half-cycle duration (visible for one period, hidden for the
    /// next). Qt's default is ~530 ms; this default is the single Rust
    /// fallback for the CSS `caret-blink` property, which overwrites this
    /// field directly (there is no per-entity blink state to route a
    /// per-element override through).
    pub period: Duration,
}

impl Default for CaretBlink {
    fn default() -> Self {
        Self {
            visible: true,
            phase: Instant::now(),
            period: Duration::from_millis(530),
        }
    }
}

impl CaretBlink {
    /// Restart the phase at "visible" (called on focus change and on
    /// every edit / caret move so the caret never blinks mid-keystroke).
    pub fn reset(&mut self) {
        self.visible = true;
        self.phase = Instant::now();
    }
}

/// Marker: this entity accepts file drops.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DropTarget;

/// Marker: an in-app drag is currently hovering this [`DropTarget`] and
/// its payload is acceptable. Maintained each tick by
/// `lumen-os-dnd`'s drag-gesture tracker while a drag is active, removed
/// the moment the pointer leaves or the drag ends. Drives the
/// `:drag-over` pseudo-class (HTML5 DnD `dragover` parity) so the hovered
/// drop zone can light up via design tokens.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DropHovered;

/// Marker: user input is rejected on this entity.
///
/// - Authored via `disabled="true"` on `<button>` / `<input>` /
///   `<toggle>` / `<slider>`.
/// - `lumen-input` skips Disabled entities in click dispatch and the
///   Tab focus cycle; CSS `:disabled` rules route their `bg` to the
///   entity's disabled fill at parse time.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Disabled;

/// Marker: this entity is the currently-selected member of a
/// single-selection group (active tab button today; dropdown
/// current-value button later). Maintained by the owning primitive's
/// sync system - inserted on the active member, removed from siblings.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Selected;

/// Spawn-order tiebreak for focus cycling. `bevy_ecs` 0.19's `Entity: Ord`
/// is a niche-optimized row-index comparison, not a spawn-order one - for
/// entities recycled through a freed ECS row, a later-spawned entity can
/// sort *before* an earlier one. `lumenc::spawn` assigns this from a
/// monotonic per-document counter as it walks the parsed tree in markup
/// order, so entities with equal [`TabIndex`] cycle in the order they
/// appear in the source, not in whatever order their table rows landed.
///
/// Absent on entities not spawned through `lumenc` (hand-built ECS test
/// fixtures, primarily) - consumers should treat a missing value as "no
/// preference" and fall back to `Entity` ordering for those.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DocumentOrder(pub u32);

/// Marker: presses on this entity (and hit-bubbled descendants) trigger a native window drag.
/// Authored by the `<title-bar drag>` region; the window backend sets [`WindowDragRequest`] and calls `winit::Window::drag_window()`.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct TitleBarDraggable;

/// Window-backend request to begin a native window drag on the next tick.
/// Populated by the input layer on a press over a [`TitleBarDraggable`] entity; consumed and cleared by `lumen-window-winit`.
#[derive(bevy_ecs::resource::Resource, Default, Debug)]
pub struct WindowDragRequest(pub bool);

/// App-side intent for color-scheme resolution; mirrors libadwaita's
/// [`AdwColorScheme`](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/enum.ColorScheme.html).
///
/// Resolution rules (see [`StyleManager`]):
/// - [`ColorScheme::ForceLight`] -> always light, ignore system.
/// - [`ColorScheme::ForceDark`] -> always dark, ignore system.
/// - [`ColorScheme::PreferLight`] -> follow system; default light when unknown.
/// - [`ColorScheme::PreferDark`] -> follow system; default dark when unknown.
/// - [`ColorScheme::Default`] -> follow system; default light when unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColorScheme {
    /// Follow the OS-reported preference; fall back to light when no
    /// preference has been detected yet.
    #[default]
    Default,
    /// Force light regardless of OS preference.
    ForceLight,
    /// Force dark regardless of OS preference.
    ForceDark,
    /// Prefer light but follow OS overrides when reported.
    PreferLight,
    /// Prefer dark but follow OS overrides when reported.
    PreferDark,
}

impl From<bool> for ColorScheme {
    /// Legacy bridge: `true` -> [`ColorScheme::ForceDark`], `false` ->
    /// [`ColorScheme::ForceLight`]. Mirrors the pre-W4.6 `OsTheme.is_dark`
    /// bool, but lets old code lean on `Into<ColorScheme>` without a
    /// bespoke `convert_*` helper.
    fn from(is_dark: bool) -> Self {
        if is_dark {
            Self::ForceDark
        } else {
            Self::ForceLight
        }
    }
}

impl ColorScheme {
    /// Parse the Rhai / FFI / `@media` flavoured names: `"default"`,
    /// `"auto"`, `"force-light"`, `"force-dark"`, `"prefer-light"`,
    /// `"prefer-dark"`, `"light"`, `"dark"`. Case-insensitive. Returns
    /// `None` on unknown.
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "default" | "auto" | "follow" => Some(Self::Default),
            "force-light" | "light" => Some(Self::ForceLight),
            "force-dark" | "dark" => Some(Self::ForceDark),
            "prefer-light" => Some(Self::PreferLight),
            "prefer-dark" => Some(Self::PreferDark),
            _ => None,
        }
    }
}

/// Color-scheme arbiter mirroring `AdwStyleManager`. Combines the app's
/// stated intent (`scheme`) with the last-seen OS preference
/// (`system_dark`) to produce a single boolean (`effective_dark`) used
/// by the rest of the pipeline.
///
/// - Populated by the window backend from `winit::Theme` (`resumed` and `WindowEvent::ThemeChanged`).
/// - On Linux, an XDG desktop-portal `org.freedesktop.portal.Settings`
///   subscription pushes `set_system_dark` updates as the desktop's
///   color-scheme preference changes (best-effort; falls back to winit
///   when the portal is unavailable).
/// - [`crate::signals::style_manager_to_signal`] (W1.6) mirrors
///   `effective_dark` into `Signals["__theme__"]` as `"dark"` / `"light"`.
/// - [`crate::signals::apply_theme_signal_to_root_classes`] then writes
///   `theme-dark` / `theme-light` onto the root entity's
///   [`LumenClasses`].
#[derive(bevy_ecs::resource::Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleManager {
    /// Application-side intent. Default = follow OS.
    pub scheme: ColorScheme,
    /// Last-seen OS preference. Defaults to `false` (light) until the
    /// backend or portal listener writes through.
    pub system_dark: bool,
    /// Computed result of `scheme + system_dark`. Read by every theme
    /// consumer; written by [`Self::recompute`] from inside the setters.
    pub effective_dark: bool,
}

impl Default for StyleManager {
    fn default() -> Self {
        Self {
            scheme: ColorScheme::Default,
            system_dark: false,
            effective_dark: false,
        }
    }
}

impl StyleManager {
    /// Construct with a stated intent, recomputed against a fresh
    /// (light) system default. Useful at backend startup before any
    /// OS hint has arrived.
    pub fn with_scheme(scheme: ColorScheme) -> Self {
        let mut s = Self {
            scheme,
            system_dark: false,
            effective_dark: false,
        };
        s.recompute();
        s
    }

    /// Update the app's intent and recompute [`Self::effective_dark`].
    pub fn set_scheme(&mut self, scheme: ColorScheme) {
        self.scheme = scheme;
        self.recompute();
    }

    /// Update the last-seen OS preference and recompute
    /// [`Self::effective_dark`].
    pub fn set_system_dark(&mut self, system_dark: bool) {
        self.system_dark = system_dark;
        self.recompute();
    }

    /// Resolve `scheme + system_dark -> effective_dark` per the
    /// AdwColorScheme table.
    ///
    /// With `system_dark` modelled as a plain bool (no "unknown"
    /// state), `Default` / `PreferLight` / `PreferDark` all follow the
    /// reported system preference. The three variants still differ in
    /// the hint the app advertises back to the OS / desktop portal -
    /// the runtime layer relays the active variant via
    /// `WindowEvent::AppearanceRequested` (W4.x follow-up) so the
    /// system can switch its default.
    fn recompute(&mut self) {
        self.effective_dark = match self.scheme {
            ColorScheme::ForceLight => false,
            ColorScheme::ForceDark => true,
            ColorScheme::PreferLight | ColorScheme::PreferDark | ColorScheme::Default => {
                self.system_dark
            }
        };
    }
}

/// Backwards-compatible alias for the pre-W4.6 `OsTheme` resource.
/// New code should use [`StyleManager`] directly; the alias keeps
/// existing call sites that still read or write `is_dark` compiling
/// through the [`Deref`](std::ops::Deref) / [`DerefMut`](std::ops::DerefMut)
/// shim on the legacy wrapper.
#[deprecated(
    since = "0.0.1",
    note = "OsTheme was renamed to StyleManager (W4.6). Read `style_manager.effective_dark` in place of `os_theme.is_dark`."
)]
pub type OsTheme = StyleManager;

// ---------------------------------------------------------------------------
// W5.4 - LayoutDirection cascade + Lang
// ---------------------------------------------------------------------------

/// Per-entity layout direction (CSS `direction`). Tri-state:
///
/// - [`Self::Auto`] (default) inherits from the parent. The
///   `resolve_layout_direction` system walks the hierarchy and stamps
///   a concrete [`ResolvedDirection`] on every entity.
/// - [`Self::Ltr`] / [`Self::Rtl`] are explicit overrides.
///
/// Authored via `dir="ltr"|"rtl"|"auto"` on any markup element. Read
/// downstream by the layout backend (logical [`Edges`] resolver +
/// [`FlexDirection::resolved`]) and by AccessKit / text shaping.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LayoutDirection {
    /// Inherit from the parent (root falls back to
    /// [`DefaultLayoutDirection`]).
    #[default]
    Auto,
    /// Left-to-right writing direction.
    Ltr,
    /// Right-to-left writing direction.
    Rtl,
}

impl From<&str> for LayoutDirection {
    /// Parse the markup / CSS spellings. Unknown values map to
    /// [`Self::Auto`] so the caller can detect "no opinion" - the
    /// parser layer separately rejects malformed `dir=` attributes.
    fn from(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "ltr" => Self::Ltr,
            "rtl" => Self::Rtl,
            "auto" | "inherit" | "" => Self::Auto,
            _ => Self::Auto,
        }
    }
}

/// BCP-47 language tag (e.g. `"en-US"`, `"ar-EG"`). Drives text
/// shaping (`cosmic_text::Attrs::language`), AccessKit
/// (`Node::set_language`), and locale-aware formatters.
///
/// Authored via `lang="ar-EG"` on any element. Inherited from the
/// nearest ancestor when absent.
#[derive(Component, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Lang(pub Arc<str>);

impl From<&str> for Lang {
    fn from(s: &str) -> Self {
        Self(Arc::<str>::from(s.trim()))
    }
}

impl From<String> for Lang {
    fn from(s: String) -> Self {
        Self(Arc::<str>::from(s.as_str()))
    }
}

impl Default for Lang {
    fn default() -> Self {
        Self(Arc::<str>::from(""))
    }
}

/// Cascade output written by [`resolve_layout_direction`]. Either
/// [`LayoutDirection::Ltr`] or [`LayoutDirection::Rtl`] - never
/// [`LayoutDirection::Auto`] (the resolver folded the inheritance
/// chain). Downstream consumers (layout backend, text shaper,
/// AccessKit) read this instead of walking the hierarchy themselves.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ResolvedDirection(pub LayoutDirection);

impl ResolvedDirection {
    /// Resolved direction, guaranteed to be [`LayoutDirection::Ltr`] or
    /// [`LayoutDirection::Rtl`] - the resolver substitutes
    /// [`LayoutDirection::Auto`] with [`LayoutDirection::Ltr`] before
    /// stamping.
    pub const fn direction(self) -> LayoutDirection {
        self.0
    }

    /// Convenience: true when the resolved direction is RTL.
    pub const fn is_rtl(self) -> bool {
        matches!(self.0, LayoutDirection::Rtl)
    }
}

/// Default writing direction for the application root. The
/// [`resolve_layout_direction`] system uses this when the root entity
/// has no explicit [`LayoutDirection`]. It defaults to
/// [`LayoutDirection::Ltr`] and nothing sets it from the locale today,
/// so a right-to-left app still needs `dir="rtl"` in its markup.
#[derive(bevy_ecs::resource::Resource, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DefaultLayoutDirection(pub LayoutDirection);

impl Default for DefaultLayoutDirection {
    fn default() -> Self {
        Self(LayoutDirection::Ltr)
    }
}

/// Resolve every entity's [`LayoutDirection`] (defaulting to
/// [`LayoutDirection::Auto`] when the component is absent) against its
/// ancestor chain and stamp the answer into [`ResolvedDirection`].
///
/// Roots whose direction is `Auto` fall back to the
/// [`DefaultLayoutDirection`] resource. Runs in
/// [`crate::tick::TickStage::LayoutSync`] before the layout backend
/// reads `ResolvedDirection`.
///
/// Algorithm: one pass over every entity. For each entity, walk
/// `ChildOf` to the nearest ancestor that either (a) has an explicit
/// `Ltr` / `Rtl` direction, or (b) is the root. Honour the override or
/// the resource default. Time is `O(depth)` per entity; with shallow
/// UI trees (<= 16 levels) this is cheap and avoids any allocation.
///
/// D9: the pass is gated on its actual inputs - an explicit
/// [`LayoutDirection`] changing / appearing / disappearing, a hierarchy
/// edit (`ChildOf` changed or removed), the [`DefaultLayoutDirection`]
/// resource changing, or entities that have never been stamped. Steady
/// ticks cost a handful of empty-query checks. When the pass does run,
/// [`ResolvedDirection`] is only (re)inserted when the value actually
/// differs, so `Changed<ResolvedDirection>` downstream (the layout
/// backend's D8 hook) fires exclusively on real flips.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn resolve_layout_direction(
    mut commands: Commands,
    default_dir: Option<Res<DefaultLayoutDirection>>,
    dirs: Query<&LayoutDirection>,
    parents: Query<&bevy_ecs::hierarchy::ChildOf>,
    // `Without<IsResource>` keeps the sweep on real UI entities. Resources
    // live on their own entities, so an unfiltered `Query<Entity>` would
    // stamp `ResolvedDirection` onto every resource in the world.
    all: Query<Entity, Without<bevy_ecs::resource::IsResource>>,
    resolved_q: Query<&ResolvedDirection>,
    changed_inputs: Query<
        (),
        bevy_ecs::prelude::Or<(
            bevy_ecs::prelude::Changed<LayoutDirection>,
            bevy_ecs::prelude::Changed<bevy_ecs::hierarchy::ChildOf>,
        )>,
    >,
    unstamped: Query<(), Without<ResolvedDirection>>,
    mut removed_dirs: RemovedComponents<LayoutDirection>,
    mut removed_parents: RemovedComponents<bevy_ecs::hierarchy::ChildOf>,
) {
    // Drain both removal readers unconditionally so their bounded ring
    // buffers can't accumulate stale entries across gated ticks.
    let removed_dir_any = removed_dirs.read().next().is_some();
    let removed_parent_any = removed_parents.read().next().is_some();
    let default_changed = default_dir.as_ref().is_some_and(|r| r.is_changed());
    if !default_changed
        && !removed_dir_any
        && !removed_parent_any
        && changed_inputs.is_empty()
        && unstamped.is_empty()
    {
        return;
    }

    let fallback = default_dir.map(|r| r.0).unwrap_or(LayoutDirection::Ltr);
    let fallback = match fallback {
        LayoutDirection::Auto => LayoutDirection::Ltr,
        other => other,
    };

    for entity in &all {
        let resolved = resolve_one(entity, &dirs, &parents, fallback);
        // Insert only on a real change - a per-entity insert every tick
        // spams change detection and forces archetype churn (D9).
        if resolved_q.get(entity).map(|r| r.0) == Ok(resolved) {
            continue;
        }
        commands.entity(entity).insert(ResolvedDirection(resolved));
    }
}

fn resolve_one(
    entity: Entity,
    dirs: &Query<&LayoutDirection>,
    parents: &Query<&bevy_ecs::hierarchy::ChildOf>,
    fallback: LayoutDirection,
) -> LayoutDirection {
    let mut cur = entity;
    // Cap the walk so a pathological cycle (shouldn't happen - bevy_ecs
    // hierarchy guards against it) doesn't spin forever.
    for _ in 0..256 {
        match dirs.get(cur) {
            Ok(LayoutDirection::Ltr) => return LayoutDirection::Ltr,
            Ok(LayoutDirection::Rtl) => return LayoutDirection::Rtl,
            // Auto (or no component) -> continue up.
            _ => {}
        }
        match parents.get(cur) {
            Ok(p) => cur = p.parent(),
            Err(_) => return fallback,
        }
    }
    fallback
}

/// Shared hidden-check for every path that must honour visibility (spec
/// section 17.4: one visibility story). True when `entity` or any ancestor is
/// hidden by either mechanism:
///
/// * [`Visible(false)`](Visible): render-gate hide (keep-space variant,
///   and the flag `<if mode="hide">` stamps on its subtree root), or
/// * [`Style::display`] `== `[`Display::None`]: space-releasing hide.
///
/// The `Style` query is generic over a [`QueryFilter`](bevy_ecs::query::QueryFilter)
/// `F` so callers that already hold a conflicting `Style` view (e.g. the
/// `Without<ProgressFill>` split in `lumen_primitives`'s progress sync,
/// or the unfiltered pointer/keyboard paths in `lumen_input`) can pass a
/// disjoint query without a second archetype conflict.
pub fn hidden_via_ancestors<F: bevy_ecs::query::QueryFilter>(
    entity: Entity,
    parents: &Query<&bevy_ecs::hierarchy::ChildOf>,
    visibles: &Query<&Visible>,
    styles: &Query<&Style, F>,
) -> bool {
    let mut cur = entity;
    loop {
        if visibles.get(cur).is_ok_and(|v| !v.0) {
            return true;
        }
        if styles
            .get(cur)
            .is_ok_and(|s| matches!(s.display, Display::None))
        {
            return true;
        }
        match parents.get(cur) {
            Ok(co) => cur = co.parent(),
            Err(_) => return false,
        }
    }
}

/// Defines an `Arc<str>`-newtype binding component together with the
/// `From<String>` / `From<&str>` conversions every such binding shares.
///
/// The per-type doc comment is passed through verbatim (via the captured
/// `#[doc]` attributes) because each records a substantive markup
/// contract that must not be flattened.
macro_rules! arc_str_binding {
    ($(#[doc = $doc:expr])+ $name:ident) => {
        $(#[doc = $doc])+
        #[derive(Component, Clone, Debug)]
        pub struct $name(pub Arc<str>);

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s.into())
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.into())
            }
        }
    };
}

arc_str_binding! {
    /// Binds this entity's [`TextContent`] to a named entry in [`crate::signals::Signals`]; markup `bind-text="counter"`.
    ///
    /// - `apply_text_bindings` copies `Signals[name]` into `TextContent` each tick.
    /// - When the signal is absent, the existing text is preserved.
    /// - The signal name is stored as `Arc<str>` and shared across all entities binding the same name.
    BindText
}

/// Two-way binding for `<toggle bind-checked="signal">`.
/// - Signal -> [`Toggleable`] via [`crate::signals::apply_checked_bindings`].
/// - [`Toggleable`] -> signal via [`crate::signals::push_toggle_to_signal`] on user flip.
#[derive(Component, Clone, Debug)]
pub struct BindChecked(pub String);

/// Two-way binding for `<slider bind-value="signal">`.
/// - Signal -> [`SliderValue`] via [`crate::signals::apply_value_bindings`].
/// - [`SliderValue`] -> signal via [`crate::signals::push_slider_to_signal`] on user drag.
#[derive(Component, Clone, Debug)]
pub struct BindValue(pub String);

/// One-way binding for `<button bind-disabled="signal">` (any tag).
/// - Signal -> [`Disabled`] marker via
///   [`crate::signals::apply_disabled_bindings`]: a truthy signal value
///   inserts the marker, a falsy one removes it, letting scripts and
///   derived signals enable / disable widgets live.
///
/// There is no push half - `Disabled` is never mutated by user input.
#[derive(Component, Clone, Debug)]
pub struct BindDisabled(pub String);

/// Two-way binding for `<scroll bind-scroll="signal">` (W6 T6).
/// - Signal (f32, logical px, vertical offset) -> [`crate::input::ScrollOffset`]
///   via [`crate::signals::apply_scroll_bindings`] - reactive scroll
///   control with NO per-frame script hook; a script writes the signal
///   once and the dirty-gated reader applies it.
/// - [`crate::input::ScrollOffset`] -> signal via
///   [`crate::signals::push_scroll_to_signal`], throttled to
///   scroll-settle (offset stopped changing and the fling velocity
///   slept) so user scrolling doesn't spam the store per frame.
#[derive(Component, Clone, Debug)]
pub struct BindScroll(pub String);

arc_str_binding! {
    /// Per-entity text binding: `bind-text="$self.field"` lowers to this
    /// marker. The follow-up consumer reads the named field from the
    /// owning entity's `ArrayItem` (or other per-entity property bag) each
    /// tick and writes it into [`TextContent`]. The field name is stored as
    /// `Arc<str>` and shared across instances that bind the same field.
    ///
    /// W-signal-design step 1 placeholder: the systems that consume this
    /// component land in a follow-up commit - installing the marker today
    /// just records authoring intent in the spawned entity.
    BindSelfText
}

arc_str_binding! {
    /// Per-entity slider-value binding: `bind-value="$self.field"`.
    /// Stub component; consumer lands in the follow-up commit.
    BindSelfValue
}

arc_str_binding! {
    /// Per-entity toggle binding: `bind-checked="$self.field"`.
    /// Stub component; consumer lands in the follow-up commit.
    BindSelfChecked
}

arc_str_binding! {
    /// Parent-entity text binding: `bind-text="$parent.field"`. The
    /// follow-up consumer walks one [`ChildOf`] step up the tree and reads
    /// the named field from the parent's per-entity property bag.
    BindParentText
}

arc_str_binding! {
    /// Parent-entity slider-value binding: `bind-value="$parent.field"`.
    /// Stub component; consumer lands in the follow-up commit.
    BindParentValue
}

arc_str_binding! {
    /// Parent-entity toggle binding: `bind-checked="$parent.field"`.
    /// Stub component; consumer lands in the follow-up commit.
    BindParentChecked
}

/// No-op consumer stub for [`BindSelfText`]. Registered so plugin
/// scheduling can already wire it in; the follow-up commit populates the
/// query and reads from the per-entity property bag. Today this is a
/// pure no-op to keep the system graph stable without behavioural
/// change.
pub fn apply_bind_self_text() {}

/// No-op consumer stub for [`BindSelfValue`]. See [`apply_bind_self_text`].
pub fn apply_bind_self_value() {}

/// No-op consumer stub for [`BindSelfChecked`]. See [`apply_bind_self_text`].
pub fn apply_bind_self_checked() {}

/// No-op consumer stub for [`BindParentText`]. See [`apply_bind_self_text`].
pub fn apply_bind_parent_text() {}

/// No-op consumer stub for [`BindParentValue`]. See [`apply_bind_self_text`].
pub fn apply_bind_parent_value() {}

/// No-op consumer stub for [`BindParentChecked`]. See [`apply_bind_self_text`].
pub fn apply_bind_parent_checked() {}

/// On/off state for `<toggle>` entities. Click flips `checked` and the runtime emits `on_toggle(id, checked)`.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Toggleable {
    /// Current checked state.
    pub checked: bool,
}

/// Bounded scalar state for `<slider>` entities. `value` is held in `[min, max]`; the runtime emits `on_slider(id, value)` on drag or track click.
#[derive(Component, Clone, Copy, Debug)]
pub struct SliderValue {
    /// Current value, clamped to `[min, max]`.
    pub value: f32,
    /// Lower bound.
    pub min: f32,
    /// Upper bound.
    pub max: f32,
    /// Authored `step="..."` increment for keyboard arrows and wheel
    /// notches. `None` falls back to `(max - min) / 100` - the
    /// `<input type=range>` browser default of 100 discrete positions
    /// (see [`Self::step_size`]).
    pub step: Option<f32>,
}

impl SliderValue {
    /// Effective step increment: the authored [`Self::step`], or
    /// `(max - min) / 100` when unset.
    pub fn step_size(&self) -> f32 {
        self.step.unwrap_or((self.max - self.min) / 100.0)
    }
}

impl Default for SliderValue {
    fn default() -> Self {
        Self {
            value: 0.0,
            min: 0.0,
            max: 1.0,
            step: None,
        }
    }
}

/// How an image fits its layout rectangle; mirrors CSS `object-fit`. Defaults to [`Self::Fill`] (stretch to the entity's `Transform.size`).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageFit {
    /// Stretch to fill, ignoring aspect ratio.
    #[default]
    Fill,
    /// Scale to cover the box; aspect-preserved; overflow clipped.
    Cover,
    /// Scale to fit inside the box; aspect-preserved; may leave empty
    /// space on one axis.
    Contain,
    /// Draw at intrinsic pixel size, top-left aligned. May overflow.
    None,
    /// `min(None, Contain)` - never enlarges, may shrink.
    ScaleDown,
}

/// An image with a backing GPU texture (uploaded asynchronously).
#[derive(Component, Clone, Debug, Default)]
pub struct ImageComponent {
    /// Source asset path or URL.
    pub source: String,
    /// Logical pixel size once decoded.
    pub natural_size: Option<Vec2>,
}

/// Type-erased blob sidecar for an image render entity.
///
/// Attached to render-world entities alongside [`crate::render_world::ExtractedImage`] so
/// [`crate::node_ir::transform_extracted_to_nodes`] can splice the payload straight into
/// [`crate::node_ir::Node::Image::blob`] without `lumen-core` depending on the concrete blob type
/// (today: `lumen_assets::ExtractedImageBlob` wrapping a `vello::peniko::Blob<u8>`).
///
/// The inner `Arc<dyn Any + Send + Sync>` is downcast back to its concrete type by the renderer
/// walker. This lets the asset crate own the vello-typed payload while the core crate stays free of
/// the vello dependency.
#[derive(Component, Clone)]
pub struct ImageBlob(pub Arc<dyn std::any::Any + Send + Sync>);

impl std::fmt::Debug for ImageBlob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageBlob").finish_non_exhaustive()
    }
}

/// Type-erased payload sidecar for an SVG render entity.
///
/// Same shape as [`ImageBlob`] but for the SVG path: the renderer walker downcasts to
/// `lumen_assets::ExtractedSvg` to drive the cached `vello::Scene`. Attached to render-world
/// entities alongside the SVG's own components so [`crate::node_ir::transform_extracted_to_nodes`]
/// can splice the payload straight into [`crate::node_ir::Node::Svg::payload`].
///
/// `order` mirrors the `ExtractedSvg.order` so the IR builder can sort SVG leaves into painter
/// order without depending on the assets crate's concrete `ExtractedSvg` type.
#[derive(Component, Clone)]
pub struct SvgPayload {
    /// Opaque scene payload; the renderer walker downcasts to its concrete type.
    pub payload: Arc<dyn std::any::Any + Send + Sync>,
    /// Global paint order (mirrors `lumen_assets::ExtractedSvg::order`).
    pub order: u32,
}

impl std::fmt::Debug for SvgPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SvgPayload")
            .field("order", &self.order)
            .finish_non_exhaustive()
    }
}

/// Visual record for one rect: optional fill (solid or gradient), uniform corner radius, and stacked shadows.
///
/// - `fill = None` emits no rect; the entity behaves as a layout-only container.
/// - Opacity is held in [`Opacity`] separately so it composes with text, image, and SVG paint too.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct Visuals {
    /// Background fill. `None` emits no rect.
    pub fill: Option<Fill>,
    /// Uniform corner radius in logical pixels (`0` = sharp).
    pub radius: f32,
    /// Per-corner radii `[top-left, top-right, bottom-right,
    /// bottom-left]` (CSS `border-radius` 2-4 value shorthand /
    /// per-corner longhands). When `Some`, the paint path uses these
    /// and [`Self::radius`] carries the max corner for uniform-only
    /// consumers (knob geometry, focus rings).
    pub corner_radii: Option<[f32; 4]>,
    /// Stacked shadows in source order. Each comma-separated CSS `box-shadow` entry produces one [`ShadowSpec`]; `inset` entries set `inner = true` and render clipped to the rect.
    pub shadows: Vec<ShadowSpec>,
    /// CSS border paint: per-side widths + one color, solid style.
    /// `None` = no border (style `none`). Painted inside the border box
    /// (between the outer edge and the padding box), above the
    /// background fill and below children - exactly CSS's
    /// background -> border -> content order. The matching layout-space
    /// widths live in [`Style::border`].
    pub border: Option<Border>,
}

/// Solid border paint record stored on [`Visuals::border`]. Supports
/// `border-style: solid` with per-side widths (including `0` = no
/// border on that side) and optional per-side colors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    /// Per-side stroke widths in logical pixels (`0` = that side absent).
    pub widths: Edges,
    /// Border color shared by all four sides (the uniform fast path).
    pub color: Color,
    /// Per-side color overrides `[top, right, bottom, left]` (CSS
    /// `border-top-color` ...). `None` = every side paints [`Self::color`].
    pub side_colors: Option<[Color; 4]>,
}

impl Border {
    /// Uniform border: one width, one color, no per-side overrides.
    pub fn uniform(widths: Edges, color: Color) -> Self {
        Self {
            widths,
            color,
            side_colors: None,
        }
    }
}

impl Visuals {
    /// Returns a reference to the first [`ShadowSpec`] in `shadows`, or `None` when the vector is empty.
    pub fn primary_shadow(&self) -> Option<&ShadowSpec> {
        self.shadows.first()
    }
}

/// Fill brush variants for a [`Visuals`] rect.
#[derive(Clone, Debug, PartialEq)]
pub enum Fill {
    /// Single uniform color.
    Solid(Color),
    /// Linear gradient. `angle_deg` uses the CSS convention (`0` = left->right, `90` = bottom->top, `180` = top->bottom). `stops` is sorted by ascending offset at parse time.
    Linear {
        /// Direction in degrees.
        angle_deg: f32,
        /// `(offset, color)` pairs with `offset` in `0..=1`, ascending.
        stops: Vec<(f32, Color)>,
    },
    /// Radial gradient centred at 50% / 50% of the entity rect. `radius` is normalised to `0..=1` of the rect's min dimension (`1.0` reaches the nearest edge).
    Radial {
        /// Normalised radius in `0..=1`.
        radius: f32,
        /// `(offset, color)` pairs with `offset` in `0..=1`, ascending.
        stops: Vec<(f32, Color)>,
    },
    /// Conic (sweep) gradient centred at 50% / 50%. `from_deg` rotates the sweep start using the CSS convention (`0` = north, `90` = east).
    Conic {
        /// Starting angle in degrees.
        from_deg: f32,
        /// `(offset, color)` pairs with `offset` in `0..=1`, ascending.
        stops: Vec<(f32, Color)>,
    },
}

impl Fill {
    /// Returns the inner [`Color`] when `self` is `Fill::Solid`; `None` otherwise.
    pub fn as_solid(&self) -> Option<Color> {
        if let Fill::Solid(c) = self {
            Some(*c)
        } else {
            None
        }
    }

    /// Constructs a `Fill::Solid(c)` shorthand.
    pub const fn solid(c: Color) -> Self {
        Fill::Solid(c)
    }
}

/// Shadow record stored on [`Visuals::shadows`].
///
/// - `offset_x` / `offset_y` move the shadow origin in logical pixels.
/// - `blur` is the Gaussian std-dev (`0` = sharp offset clone).
/// - `inner = true` renders an inset shadow clipped to the rect with the blurred draw at the negated offset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShadowSpec {
    /// Horizontal offset in logical pixels (positive = right).
    pub offset_x: f32,
    /// Vertical offset in logical pixels (positive = down).
    pub offset_y: f32,
    /// Gaussian blur radius (std-dev). `0` = sharp offset.
    pub blur: f32,
    /// CSS spread radius - inflates (positive) / deflates (negative)
    /// the shadow rect before blurring. Enables the hard double-ring
    /// idiom `box-shadow: 0 0 0 2 <color>`.
    pub spread: f32,
    /// Shadow color; alpha controls softness.
    pub color: Color,
    /// `true` renders as inset shadow; `false` (default) renders as drop shadow.
    pub inner: bool,
}

/// Alpha multiplier applied to every drawn aspect of this entity (background fill, gradient, image, SVG, text, shadow, outline).
///
/// - Value range: `[0, 1]`; absent component is equivalent to fully opaque.
/// - Applied at extract time by multiplying the alpha channel of each emitted color/brush.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Opacity(pub f32);

impl Default for Opacity {
    fn default() -> Self {
        Self(1.0)
    }
}

impl Opacity {
    /// Returns `c` with its alpha multiplied by `self.0` and clamped to `[0, 1]`.
    pub fn apply(&self, mut c: Color) -> Color {
        c.a = (c.a * self.0).clamp(0.0, 1.0);
        c
    }
}

/// Stable string id assigned in markup via `id="..."`. Apps query `Query<(Entity, &LumenId)>` and match by name.
#[derive(Component, Clone, Debug)]
pub struct LumenId(pub String);

/// Generic attribute overflow map for element attributes that have no typed
/// component of their own (`role`, `data-*`, `aria-*`, custom attrs). The
/// dynamic DOM API's `set_attr`/`get_attr`/`remove_attr` route KNOWN attrs
/// (src, id, class, text, ...) to their typed components and everything
/// else here. Attribute names are stored verbatim; values are strings.
#[derive(Component, Clone, Debug, Default)]
pub struct LumenAttributes(pub std::collections::HashMap<String, String>);

impl LumenAttributes {
    /// Read an attribute value.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// Set (or replace) an attribute value.
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        self.0.insert(name.to_string(), value.into());
    }

    /// Remove an attribute, returning its previous value if present.
    pub fn remove(&mut self, name: &str) -> Option<String> {
        self.0.remove(name)
    }
}

/// Per-element inline style overrides: the DOM `element.style` layer. Stored
/// as ordered `(property, value)` pairs so a later write wins and iteration
/// is deterministic. The runtime CSS re-apply reads this LAST (highest
/// cascade tier, above the stylesheet), mirroring how inline style beats
/// author rules in the browser. `set_style`/`style_get`/`style_remove`
/// mutate it; `computed_style` reflects it after the cascade.
#[derive(Component, Clone, Debug, Default)]
pub struct InlineStyle(pub Vec<(String, String)>);

impl InlineStyle {
    /// Read the inline value for `property`, if set.
    pub fn get(&self, property: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == property)
            .map(|(_, v)| v.as_str())
    }

    /// Set (or replace) an inline property, keeping first-seen order.
    pub fn set(&mut self, property: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| k == property) {
            slot.1 = value;
        } else {
            self.0.push((property.to_string(), value));
        }
    }

    /// Remove an inline property, returning its previous value if present.
    pub fn remove(&mut self, property: &str) -> Option<String> {
        let pos = self.0.iter().position(|(k, _)| k == property)?;
        Some(self.0.remove(pos).1)
    }
}

/// Form-field validation rules attached when `<input>`, `<toggle>`, or `<slider>` declares `required` / `pattern` / `min` / `max`.
/// The `validate_inputs` system in `lumen-primitives` recomputes [`Self::is_valid`] from the entity's content and mirrors the result into the `valid:<id>` reactive signal.
#[derive(Component, Clone, Debug, Default)]
pub struct Validation {
    /// When `true`, the trimmed content must be non-empty for `is_valid`.
    pub required: bool,
    /// Literal substring the content must contain. (Regex support is not provided; Rhai scripts cover broader matching.)
    pub pattern: Option<String>,
    /// Lower numeric bound when the content parses as a number.
    pub min: Option<f32>,
    /// Upper numeric bound when the content parses as a number.
    pub max: Option<f32>,
    /// Most recent validity result, recomputed by the validator system.
    pub is_valid: bool,
}

/// Class list assigned in markup via `class="a b c"`. Apps test membership with `LumenClasses::has("tile")`.
/// Storage is `Vec<Arc<str>>` so repeated class names share one allocation; cloning performs only Arc bumps.
#[derive(Component, Clone, Debug, Default)]
pub struct LumenClasses(pub Vec<std::sync::Arc<str>>);

impl LumenClasses {
    /// Returns `true` when any stored class equals `name`.
    pub fn has(&self, name: &str) -> bool {
        self.0.iter().any(|c| c.as_ref() == name)
    }
}

/// A use site the build could not finish, waiting for the script to fill it.
///
/// A component element names a script function. When instantiating the block
/// that function returns is the same as calling it, the build puts the block
/// in the tree and nothing is left to do. When the function has to run,
/// because it works a value out or picks between blocks, what the build leaves
/// is one element carrying this: the function's name and the arguments the use
/// site bound, in the order the call passes them.
///
/// The script runtime calls the function, replaces this element with the node
/// it returns, and drops the marker. That runs on the tick the element was
/// mounted, ahead of the command applier, so the tree is whole before it is
/// drawn.
#[derive(Component, Clone, Debug)]
pub struct PendingFill {
    /// Script function to call.
    pub function: String,
    /// Argument values, in the function's parameter order.
    pub args: Vec<String>,
}

/// Markup tag name (`tile`, `label`, `button`, ...) retained on entities
/// that carry a `class` / `id`, so the runtime can rebuild a minimal
/// selector target and re-run the CSS cascade in place on a theme /
/// media flip (see `lumenc`'s `reapply_computed_styles`). Only attached
/// to selector-reachable entities to keep archetype churn off the plain
/// layout containers that no rule can name.
#[derive(Component, Clone, Debug)]
pub struct LumenTag(pub std::sync::Arc<str>);

impl From<Vec<String>> for LumenClasses {
    fn from(v: Vec<String>) -> Self {
        Self(v.into_iter().map(Into::into).collect())
    }
}

impl From<&[String]> for LumenClasses {
    fn from(v: &[String]) -> Self {
        Self(v.iter().map(|s| s.as_str().into()).collect())
    }
}

/// Text wrap policy stored inside [`TextStyle`] and `ExtractedText`. Defaults to [`Self::None`] (no wrap, overflow clips).
/// Mirrors CSS `white-space: nowrap` (None), `word-wrap: break-word` (Word), and a CJK-style glyph-level break (Glyph).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextWrap {
    /// No automatic wrapping.
    #[default]
    None,
    /// Word-break wrap at the available width.
    Word,
    /// Glyph-level wrap.
    Glyph,
}

/// Horizontal text alignment inside the entity's content rectangle, stored inside [`TextStyle`] and `ExtractedText`.
/// Defaults to [`Self::Start`] (left in left-to-right reading order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    /// Left-aligned.
    #[default]
    Start,
    /// Center-aligned within the content rect.
    Center,
    /// Right-aligned.
    End,
}

/// Text style record carrying fill color, size, family/weight, alignment, wrap policy, and optional max-line cap.
/// [`TextContent`] is stored separately so keystrokes do not bump change detection on these cold fields.
/// Default: near-white at 16px, weight 400, platform sans-serif, left-aligned, no wrap, unbounded lines.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Fill color. Default is `Color::rgb(0.92, 0.92, 0.94)`.
    pub color: Color,
    /// Font size in logical pixels.
    pub size_px: f32,
    /// Horizontal alignment inside the content rect.
    pub align: TextAlign,
    /// Wrap policy passed to the shaper.
    pub wrap: TextWrap,
    /// Hard cap on lines after shaping; `None` is unbounded.
    pub max_lines: Option<u32>,
    /// CSS `font-family` fallback chain as authored (comma-separated;
    /// the shaper strips quotes and resolves the first available family
    /// against the system font database, honouring the CSS generic
    /// keywords). `None` = platform sans-serif. Shared `Arc` so clones
    /// (extract, cache keys) don't copy the string.
    pub family: Option<std::sync::Arc<str>>,
    /// CSS `font-weight` (1-1000; 400 = normal, 700 = bold).
    pub weight: u16,
    /// Selection highlight background (`selection-color` in CSS; the
    /// default skin routes it through the `--lumen-selection` token) -
    /// Qt's `QPalette::Highlight` / Slint's `selection-background-color`.
    /// `None` falls back to the renderer's single built-in highlight
    /// ([`crate::render_world::DEFAULT_SELECTION_BG`]).
    ///
    /// The paired caret color and selected-glyph color live on the
    /// separate [`TextInputPaint`] component so adding them never forces
    /// every `TextStyle` literal to change.
    pub selection_color: Option<Color>,
    /// CSS `line-height`. Inherits down the tree like [`Self::size_px`]
    /// (the IR resolves inheritance before this field is populated).
    /// `None` => [`DEFAULT_LINE_HEIGHT_MULTIPLIER`] (`line-height: normal`),
    /// resolved via [`resolve_line_height`].
    pub line_height: Option<LineHeightSpec>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: Color::rgb(0.92, 0.92, 0.94),
            size_px: 16.0,
            align: TextAlign::Start,
            wrap: TextWrap::None,
            max_lines: None,
            family: None,
            weight: 400,
            selection_color: None,
            line_height: None,
        }
    }
}

/// Optional caret + selected-glyph paint overrides for a text input,
/// split from [`TextStyle`] so they can be added without touching every
/// `TextStyle` struct literal in the tree. Sourced from the `caret-color`
/// / `selection-text-color` CSS properties by the reconciler; absent =>
/// the renderer falls back (caret takes the text fill, selected glyphs
/// keep their fill on the translucent highlight).
///
/// - Caret: `caret-color` - Qt/web caret tint.
/// - Selected foreground: `selection-text-color` - Qt
///   `QPalette::HighlightedText` / Slint `selection-foreground-color`.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct TextInputPaint {
    /// Caret color; `None` => the text fill (web default).
    pub caret_color: Option<Color>,
    /// Selected-glyph color; `None` => glyphs keep their normal fill.
    pub selection_foreground: Option<Color>,
}

/// RGBA color, each channel in [0, 1].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    /// Red channel.
    pub r: f32,
    /// Green channel.
    pub g: f32,
    /// Blue channel.
    pub b: f32,
    /// Alpha channel.
    pub a: f32,
}

impl Color {
    /// Constructs a fully-opaque `Color` from `r`, `g`, `b` channels.
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// Constructs a `Color` from `r`, `g`, `b`, `a` channels.
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Constructs a `Color` from `[R, G, B, A]` bytes - the inverse of
    /// [`Self::to_rgba8`], and the readable spelling for hex palettes
    /// (`Color::from_rgba8([0x21, 0x25, 0x2c, 0xff])`).
    pub const fn from_rgba8(rgba: [u8; 4]) -> Self {
        Self {
            r: rgba[0] as f32 / 255.0,
            g: rgba[1] as f32 / 255.0,
            b: rgba[2] as f32 / 255.0,
            a: rgba[3] as f32 / 255.0,
        }
    }

    /// Packs into `[R, G, B, A]` bytes, clamping each channel to `[0, 1]` and rounding to `u8`.
    pub fn to_rgba8(self) -> [u8; 4] {
        let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        [q(self.r), q(self.g), q(self.b), q(self.a)]
    }
}

impl From<[u8; 4]> for Color {
    fn from(rgba: [u8; 4]) -> Self {
        Self::from_rgba8(rgba)
    }
}

impl From<Color> for [u8; 4] {
    fn from(c: Color) -> Self {
        c.to_rgba8()
    }
}

// --- Accessibility components (new, additive) --------------------------------
//
// These components feed the AccessKit tree-build system in `lumen-a11y-accesskit`.
// They are deliberately layered on top of the existing primitives (`Toggleable`,
// `SliderValue`, `TextInput`, `Validation`, `Visible`, `Focused`) so existing
// markup keeps working without explicit a11y annotation; explicit components
// override the defaults derived from those primitives.
//
// Mirrors the GTK 4 `update_state` / `update_property` / `update_relation`
// split: [`A11yState`] holds the boolean flags, [`A11yLabel`] /
// [`A11yDescription`] / [`A11yValue`] / [`A11yLevel`] / [`A11ySetSize`] /
// [`A11yLive`] hold the scalar/structured properties, and [`A11yRelations`]
// holds the cross-entity relations. See `docs/audits/a11y.md`.

/// Explicit accessibility role override. Maps to [`accesskit::Role`] through
/// the `From<A11yRole> for accesskit::Role` impl in `lumen-a11y-accesskit`.
///
/// - Absent component: role derived from primitives ([`TextInput`] -> text input,
///   [`Toggleable`] -> switch/checkbox, [`SliderValue`] -> slider, etc.).
/// - Present component: this value wins. Markup author can pin a role with
///   `role="dialog"`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum A11yRole {
    /// Push button.
    Button,
    /// Hyperlink.
    Link,
    /// Single-line text input. Distinct from [`Self::TextArea`] so screen
    /// readers can announce single- vs multi-line behaviour.
    TextInput,
    /// Multi-line text area.
    TextArea,
    /// Two-state checkbox.
    Checkbox,
    /// Two-state on/off switch (checkbox semantics, switch presentation).
    /// Distinct from [`Self::Checkbox`] so assistive tech announces it as a
    /// switch (`Role::Switch`) - the role `<switch>` pins explicitly.
    Switch,
    /// Single radio button. Pair with [`Self::RadioGroup`].
    Radio,
    /// Container for a set of [`Self::Radio`] options.
    RadioGroup,
    /// Continuous bounded scalar control.
    Slider,
    /// Read-only progress indicator.
    ProgressBar,
    /// Drop-down combobox.
    ComboBox,
    /// Selectable list.
    ListBox,
    /// One row in a [`Self::ListBox`] or [`Self::Tree`].
    ListItem,
    /// Top-level menu bar (typically a window's main menu).
    MenuBar,
    /// Sub-menu container.
    Menu,
    /// Action menu entry.
    MenuItem,
    /// Toggleable menu entry (checkbox-style).
    MenuItemCheckbox,
    /// Menu entry inside a radio group.
    MenuItemRadio,
    /// Tab strip entry.
    Tab,
    /// Container for [`Self::Tab`] entries.
    TabList,
    /// Panel revealed by an active [`Self::Tab`].
    TabPanel,
    /// Tree container.
    Tree,
    /// Single row inside a [`Self::Tree`]; use [`A11yLevel`] for depth.
    TreeItem,
    /// Toolbar (typically a horizontal strip of [`Self::Button`]s).
    Toolbar,
    /// Modal or modeless dialog.
    Dialog,
    /// Alert dialog (modal, error-style).
    AlertDialog,
    /// Tooltip surface.
    Tooltip,
    /// Polite status region (live-region default).
    Status,
    /// Assertive alert region (live-region default).
    Alert,
    /// Visible label (typically associated via [`A11yRelations`]`.labelled_by`).
    Label,
    /// Heading; pair with [`A11yLevel`] for `<h1>`..`<h6>` depth.
    Heading,
    /// Generic grouping container (`<fieldset>`, `<section>`).
    Group,
    /// Named landmark region (`<aside>`, `<section role=region>`).
    Region,
    /// Page landmark (`<header>`, `<footer>`, `<nav>`, `<main>`).
    Landmark,
    /// Default generic container with no semantic role.
    Generic,
}

bitflags::bitflags! {
    /// Boolean accessibility state flags.
    ///
    /// - Mirrors GTK 4 `GtkAccessibleState` and Qt 6 `QAccessible::State`.
    /// - Translated to AccessKit setters by `From<&A11yState>` in `lumen-a11y-accesskit`.
    /// - `HIDDEN` is derived from [`Visible(false)`] at translation time, but the
    ///   bit lives here so non-`Visible` carriers (popovers, off-screen drawer
    ///   panels) can still mark themselves hidden explicitly.
    #[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct A11yState: u32 {
        /// User input is currently rejected.
        const DISABLED  = 1 << 0;
        /// Content is read-only (still focusable / copyable).
        const READ_ONLY = 1 << 1;
        /// Field must be filled in for submission to succeed.
        const REQUIRED  = 1 << 2;
        /// Rendered as not visible to assistive tech (e.g. `<if mode="hide">`).
        const HIDDEN    = 1 << 3;
        /// Validation has failed.
        const INVALID   = 1 << 4;
        /// Disclosure / tree node is open.
        const EXPANDED  = 1 << 5;
        /// Currently selected within a multi-select container.
        const SELECTED  = 1 << 6;
        /// Toggle checkbox / switch is on.
        const CHECKED   = 1 << 7;
        /// Press-style button is currently pressed (toggle button).
        const PRESSED   = 1 << 8;
        /// Computation in progress; assistive tech may defer reads.
        const BUSY      = 1 << 9;
        /// Modal dialog overlay.
        const MODAL     = 1 << 10;
    }
}

/// Accessible label (`aria-label`). Distinct from [`TextContent`] so prose
/// markup does not collide with visible body text.
#[derive(Component, Clone, Debug, Default)]
pub struct A11yLabel(pub String);

/// Accessible description (`aria-description`). Distinct from
/// [`A11yLabel`]; screen readers announce label first, then description.
#[derive(Component, Clone, Debug, Default)]
pub struct A11yDescription(pub String);

/// Bounded numeric value carrier (slider / progress / spin). The
/// `From<&SliderValue> for A11yValue` impl converts existing
/// [`SliderValue`] state without callers having to set both.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct A11yValue {
    /// Current numeric reading.
    pub now: f64,
    /// Lower bound.
    pub min: f64,
    /// Upper bound.
    pub max: f64,
    /// Step granularity for `Action::Increment` / `Decrement`.
    /// `0.0` defaults to `(max - min) / 100` in the action handler.
    pub step: f64,
    /// Optional human-readable value (e.g. "Saturday" for a date picker).
    pub text: Option<String>,
}

impl From<&SliderValue> for A11yValue {
    fn from(s: &SliderValue) -> Self {
        Self {
            now: s.value as f64,
            min: s.min as f64,
            max: s.max as f64,
            // Authored step carries through; 0.0 keeps the action
            // handler's `(max - min) / 100` fallback - the same default
            // `SliderValue::step_size` applies.
            step: s.step.map(f64::from).unwrap_or(0.0),
            text: None,
        }
    }
}

impl From<&SliderValue> for A11yRole {
    fn from(_: &SliderValue) -> Self {
        A11yRole::Slider
    }
}

impl From<&Toggleable> for A11yRole {
    fn from(_: &Toggleable) -> Self {
        A11yRole::Checkbox
    }
}

impl From<&TextInput> for A11yRole {
    fn from(t: &TextInput) -> Self {
        if t.multiline {
            A11yRole::TextArea
        } else {
            A11yRole::TextInput
        }
    }
}

/// Hierarchy level for headings (1..6) and tree items (depth from root).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct A11yLevel(pub u8);

/// Position-in-set metadata for list / tree / grid items.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct A11ySetSize {
    /// Total number of items in the containing set.
    pub size: usize,
    /// 1-based index of this item within the set.
    pub position: usize,
}

/// Live-region politeness. Drives `accesskit::Live` on the carrier.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum A11yLive {
    /// No live announcements.
    #[default]
    Off,
    /// Announce after the current utterance completes.
    Polite,
    /// Interrupt the current utterance to announce immediately.
    Assertive,
}

/// Cross-entity accessibility relations. Mirrors GTK 4 `GtkAccessibleRelation`.
///
/// - Empty fields are heap-free thanks to [`smallvec::SmallVec`].
/// - Translation layer turns each [`Entity`] into a `NodeId` via the existing
///   `entity_to_node` mapping.
#[derive(Component, Clone, Debug, Default)]
pub struct A11yRelations {
    /// Entities whose labels describe this one (`aria-labelledby`).
    pub labelled_by: smallvec::SmallVec<[Entity; 2]>,
    /// Entities whose content elaborates on this one (`aria-describedby`).
    pub described_by: smallvec::SmallVec<[Entity; 2]>,
    /// Entities whose content this one controls (`aria-controls`).
    pub controls: smallvec::SmallVec<[Entity; 2]>,
    /// Entities this one owns logically (`aria-owns`); used for portal
    /// targets and ARIA re-parenting.
    pub owns: smallvec::SmallVec<[Entity; 2]>,
}

impl A11yRelations {
    /// `true` when every relation field is empty.
    pub fn is_empty(&self) -> bool {
        self.labelled_by.is_empty()
            && self.described_by.is_empty()
            && self.controls.is_empty()
            && self.owns.is_empty()
    }
}

/// One-shot live-region announcement. Drained by the a11y translation
/// system each tick and emitted as a transient AccessKit node so screen
/// readers speak the string and immediately forget it.
///
/// Mirrors GTK 4 `gtk_accessible_announce` and Qt 6
/// `QAccessibleAnnouncementEvent`.
#[derive(Component, Clone, Debug, Default)]
pub struct A11yAnnouncement(pub String);

/// Queue of pending one-shot announcements. Resource form of
/// [`A11yAnnouncement`] used by scripts (Rhai `announce(msg, "polite")`)
/// that have no entity handle.
#[derive(bevy_ecs::resource::Resource, Default, Debug)]
pub struct A11yAnnouncementQueue {
    /// `(message, politeness)` pairs drained each tick.
    pub pending: Vec<(String, A11yLive)>,
}

/// Latest AccessKit tree update produced by the `sync_a11y_tree` system.
///
/// - Written each `TickStage::A11ySync` tick.
/// - Consumed by `lumen-window-winit` inside `RedrawRequested`; that
///   consumer calls `Adapter::update_if_active(...)` with this payload
///   instead of re-walking the world.
/// - `take()` drains the value so the next tick must build a fresh one.
#[derive(bevy_ecs::resource::Resource, Default)]
pub struct PendingA11yUpdate {
    /// `None` after a consumer takes the update; `Some` after the system fills it.
    /// Type-erased as a `Box<dyn Any>` so `lumen-core` (which has no
    /// `accesskit` dep) can host the resource. Producers downcast to
    /// `Box<accesskit::TreeUpdate>` on push and pop. See
    /// `lumen-a11y-accesskit` for the typed wrapper helpers.
    pub boxed: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl std::fmt::Debug for PendingA11yUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingA11yUpdate")
            .field("present", &self.boxed.is_some())
            .finish()
    }
}

/// Window-root entity used by the a11y tree as the AccessKit tree root.
///
/// - Written once by the window backend after the first tick (when the
///   markup root has been spawned). The backend looks for the parent-less
///   entity carrying [`Transform`] and stores it here.
/// - `sync_a11y_tree` uses `entity_to_node(self.0)` as the AccessKit tree
///   root and the `focus = root` fallback maps to it.
/// - Without this resource the translation layer falls back to its legacy
///   synthetic `NodeId(u64::MAX)` root.
#[derive(bevy_ecs::resource::Resource, Clone, Copy, Debug)]
pub struct RootWindowEntity(pub Entity);

/// Optional human-readable label for the AccessKit tree root.
///
/// - Written by the window backend from `WindowOptions.title` once the
///   window exists.
/// - When absent the translation layer falls back to the legacy
///   `"Lumen app"` hard-coded string.
#[derive(bevy_ecs::resource::Resource, Clone, Debug)]
pub struct A11yRootLabel(pub String);

/// Queue of entities the assistive tech requested be scrolled into view.
///
/// - Written by `handle_a11y_action` in `lumen-window-winit` when an
///   `Action::ScrollIntoView` arrives.
/// - Drained by `lumen-primitives::scroll::apply_a11y_scroll_into_view`,
///   which walks each entry's [`ChildOf`] chain to its scroll ancestor
///   and updates [`ScrollOffset`] so the next layout/render pass brings
///   it into view.
#[derive(bevy_ecs::resource::Resource, Default, Debug)]
pub struct A11yScrollIntoViewRequests {
    /// Entities to scroll into view.
    pub targets: Vec<Entity>,
}

/// Queue of entities the assistive tech requested context menus for.
///
/// - Written by the accessibility bridge (`lumen-a11y-accesskit`) when a
///   context-menu request arrives from an assistive technology.
/// - Drained by a system in the application; mirrored to the
///   [`crate::input::ShowContextMenu`] message bus by
///   `forward_a11y_context_menu_requests` so script handlers and
///   `MessageReader<ShowContextMenu>` subscribers receive a typed event.
#[derive(bevy_ecs::resource::Resource, Default, Debug)]
pub struct A11yContextMenuRequests {
    /// Entities a context menu was requested on.
    pub targets: Vec<Entity>,
}

#[cfg(test)]
mod style_manager_tests {
    use super::{ColorScheme, StyleManager};

    #[test]
    fn force_light_ignores_system_dark() {
        let mut s = StyleManager::with_scheme(ColorScheme::ForceLight);
        s.set_system_dark(true);
        assert!(!s.effective_dark);
    }

    #[test]
    fn force_dark_ignores_system_light() {
        let mut s = StyleManager::with_scheme(ColorScheme::ForceDark);
        s.set_system_dark(false);
        assert!(s.effective_dark);
    }

    #[test]
    fn default_follows_system_dark() {
        let mut s = StyleManager::default();
        assert!(matches!(s.scheme, ColorScheme::Default));
        assert!(!s.effective_dark);
        s.set_system_dark(true);
        assert!(s.effective_dark);
        s.set_system_dark(false);
        assert!(!s.effective_dark);
    }

    #[test]
    fn prefer_light_and_dark_follow_system() {
        let mut s = StyleManager::with_scheme(ColorScheme::PreferLight);
        s.set_system_dark(true);
        assert!(s.effective_dark);
        s.set_scheme(ColorScheme::PreferDark);
        s.set_system_dark(false);
        assert!(!s.effective_dark);
    }

    #[test]
    fn set_scheme_recomputes_in_place() {
        let mut s = StyleManager::default();
        s.set_system_dark(true);
        assert!(s.effective_dark);
        s.set_scheme(ColorScheme::ForceLight);
        assert!(!s.effective_dark);
    }

    #[test]
    fn bool_bridge_maps_to_force_variants() {
        assert!(matches!(ColorScheme::from(true), ColorScheme::ForceDark));
        assert!(matches!(ColorScheme::from(false), ColorScheme::ForceLight));
        // Round-trip via `Into` so the From impl is exercised both ways
        // (this is the legacy `OsTheme.is_dark` callsite shape).
        let cs: ColorScheme = true.into();
        assert!(matches!(cs, ColorScheme::ForceDark));
    }

    #[test]
    fn from_name_accepts_canonical_and_legacy_spellings() {
        assert!(matches!(
            ColorScheme::from_name("default").unwrap(),
            ColorScheme::Default
        ));
        assert!(matches!(
            ColorScheme::from_name("auto").unwrap(),
            ColorScheme::Default
        ));
        assert!(matches!(
            ColorScheme::from_name("FORCE-DARK").unwrap(),
            ColorScheme::ForceDark
        ));
        assert!(matches!(
            ColorScheme::from_name("dark").unwrap(),
            ColorScheme::ForceDark
        ));
        assert!(matches!(
            ColorScheme::from_name("prefer-light").unwrap(),
            ColorScheme::PreferLight
        ));
        assert!(ColorScheme::from_name("nope").is_none());
    }
}

#[cfg(test)]
mod direction_tests {
    use super::*;
    use bevy_ecs::hierarchy::ChildOf;
    use bevy_ecs::schedule::Schedule;
    use bevy_ecs::world::World;

    #[test]
    fn layout_direction_from_str_round_trips() {
        assert!(matches!(LayoutDirection::from("ltr"), LayoutDirection::Ltr));
        assert!(matches!(LayoutDirection::from("RTL"), LayoutDirection::Rtl));
        assert!(matches!(
            LayoutDirection::from("auto"),
            LayoutDirection::Auto
        ));
        // Anything unknown defaults to Auto (the parser layer rejects
        // malformed inputs separately).
        assert!(matches!(
            LayoutDirection::from("???"),
            LayoutDirection::Auto
        ));
    }

    /// D9: the resolver stamps [`ResolvedDirection`] once, then stays
    /// quiet - a steady tick must not re-insert (each insert bumps
    /// `Changed<ResolvedDirection>`, which the layout backend's D8 hook
    /// turns into a relayout). A real flip re-stamps descendants.
    #[test]
    fn resolve_layout_direction_stamps_once_then_stays_quiet() {
        use bevy_ecs::system::RunSystemOnce;
        let mut world = World::new();
        let root = world.spawn(LayoutDirection::Rtl).id();
        let child = world.spawn(ChildOf(root)).id();

        world.run_system_once(resolve_layout_direction).unwrap();
        assert_eq!(
            world.get::<ResolvedDirection>(child).map(|r| r.0),
            Some(LayoutDirection::Rtl)
        );

        let tick = world
            .entity(child)
            .get_ref::<ResolvedDirection>()
            .unwrap()
            .last_changed();
        world.run_system_once(resolve_layout_direction).unwrap();
        assert_eq!(
            world
                .entity(child)
                .get_ref::<ResolvedDirection>()
                .unwrap()
                .last_changed(),
            tick,
            "steady run must not re-stamp ResolvedDirection"
        );

        // Flip the ancestor: descendants re-resolve.
        *world.get_mut::<LayoutDirection>(root).unwrap() = LayoutDirection::Ltr;
        world.run_system_once(resolve_layout_direction).unwrap();
        assert_eq!(
            world.get::<ResolvedDirection>(child).map(|r| r.0),
            Some(LayoutDirection::Ltr)
        );
    }

    #[test]
    fn edges_resolved_inline_start_rtl_writes_right() {
        let e = Edges {
            inline_start: Some(8.0),
            ..Edges::all(0.0)
        };
        let r = e.resolved(LayoutDirection::Rtl);
        assert_eq!(r.right, 8.0);
        assert_eq!(r.left, 0.0);
        // The logical override is cleared after resolution so a second
        // call is idempotent.
        assert!(r.inline_start.is_none());
    }

    #[test]
    fn edges_resolved_inline_start_ltr_writes_left() {
        let e = Edges {
            inline_start: Some(8.0),
            ..Edges::all(0.0)
        };
        let r = e.resolved(LayoutDirection::Ltr);
        assert_eq!(r.left, 8.0);
        assert_eq!(r.right, 0.0);
    }

    #[test]
    fn edges_resolved_inline_end_mirrors_under_rtl() {
        let e = Edges {
            inline_end: Some(12.0),
            ..Edges::all(0.0)
        };
        assert_eq!(e.resolved(LayoutDirection::Ltr).right, 12.0);
        assert_eq!(e.resolved(LayoutDirection::Rtl).left, 12.0);
    }

    #[test]
    fn edges_resolved_block_overrides_top_and_bottom() {
        let e = Edges {
            block_start: Some(4.0),
            block_end: Some(5.0),
            ..Edges::all(0.0)
        };
        let r = e.resolved(LayoutDirection::Ltr);
        assert_eq!(r.top, 4.0);
        assert_eq!(r.bottom, 5.0);
    }

    #[test]
    fn edges_physical_preserved_when_logical_absent() {
        let e = Edges {
            left: 1.0,
            right: 2.0,
            top: 3.0,
            bottom: 4.0,
            ..Edges::default()
        };
        let r = e.resolved(LayoutDirection::Rtl);
        assert_eq!(r.left, 1.0);
        assert_eq!(r.right, 2.0);
        assert_eq!(r.top, 3.0);
        assert_eq!(r.bottom, 4.0);
    }

    #[test]
    fn flex_direction_row_under_rtl_becomes_row_reverse() {
        assert!(matches!(
            FlexDirection::Row.resolved(LayoutDirection::Rtl),
            FlexDirection::RowReverse
        ));
        assert!(matches!(
            FlexDirection::Row.resolved(LayoutDirection::Ltr),
            FlexDirection::Row
        ));
        assert!(matches!(
            FlexDirection::Column.resolved(LayoutDirection::Rtl),
            FlexDirection::Column
        ));
        assert!(matches!(
            FlexDirection::RowReverse.resolved(LayoutDirection::Rtl),
            FlexDirection::Row
        ));
    }

    #[test]
    fn layout_direction_auto_inherits_from_parent() {
        let mut world = World::new();
        world.insert_resource(DefaultLayoutDirection::default());
        let root = world.spawn(LayoutDirection::Rtl).id();
        // Child has no explicit LayoutDirection -> should inherit Rtl.
        let child = world.spawn(ChildOf(root)).id();
        // Grandchild explicitly Auto -> still Rtl via cascade.
        let grandchild = world.spawn((LayoutDirection::Auto, ChildOf(child))).id();
        // Sibling root with no direction -> uses DefaultLayoutDirection (Ltr).
        let sibling_root = world.spawn(()).id();

        let mut sched = Schedule::default();
        sched.add_systems(resolve_layout_direction);
        sched.run(&mut world);

        assert_eq!(
            world.get::<ResolvedDirection>(root).copied(),
            Some(ResolvedDirection(LayoutDirection::Rtl))
        );
        assert_eq!(
            world.get::<ResolvedDirection>(child).copied(),
            Some(ResolvedDirection(LayoutDirection::Rtl))
        );
        assert_eq!(
            world.get::<ResolvedDirection>(grandchild).copied(),
            Some(ResolvedDirection(LayoutDirection::Rtl))
        );
        assert_eq!(
            world.get::<ResolvedDirection>(sibling_root).copied(),
            Some(ResolvedDirection(LayoutDirection::Ltr))
        );
    }

    #[test]
    fn resolved_falls_back_to_default_resource_under_rtl_locale() {
        let mut world = World::new();
        world.insert_resource(DefaultLayoutDirection(LayoutDirection::Rtl));
        let root = world.spawn(()).id();
        let child = world.spawn((LayoutDirection::Auto, ChildOf(root))).id();

        let mut sched = Schedule::default();
        sched.add_systems(resolve_layout_direction);
        sched.run(&mut world);

        assert_eq!(
            world.get::<ResolvedDirection>(root).copied(),
            Some(ResolvedDirection(LayoutDirection::Rtl))
        );
        assert_eq!(
            world.get::<ResolvedDirection>(child).copied(),
            Some(ResolvedDirection(LayoutDirection::Rtl))
        );
    }

    #[test]
    fn lang_from_str_preserves_bcp47_tag() {
        let l: Lang = "ar-EG".into();
        assert_eq!(&*l.0, "ar-EG");
        let l2: Lang = String::from("en-US").into();
        assert_eq!(&*l2.0, "en-US");
    }
}
