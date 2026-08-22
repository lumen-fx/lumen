//! Render world definitions and the extract pipeline.
//!
//! ## Worlds
//!
//! - **Main world** (`App::world`): UI state, layout, scripts. Tick stages: `Input -> CommandDrain -> Systems -> LayoutSync -> A11ySync`.
//! - **Render world** (`App::render_world`): per-frame extracted draw data, GPU resource caches, renderer state. Tick stages: `Prepare -> Render`.
//!
//! ## Extract step
//!
//! After the main schedule and before the render schedule, the registered chain of [`ExtractFn`] entries runs.
//! Each function takes `(&mut World, &mut World)` and copies or upserts draw data from the main world into the render world.
//!
//! ## Drawable model
//!
//! Each render-world entity represents one drawable. Adding a primitive consists of: one `Extracted*` component, one extract fn, and one render system.

use crate::components::{
    CARET_WIDTH_PX, CaretBlink, CaretWidth, Color, EchoMode, Fill, ImeState, Opacity,
    PASSWORD_MASK_CHAR, PasswordCharacter, TextAlign, TextBlockOrigin, TextContent, TextInput,
    TextInputPaint, TextInputScroll, TextStyle, Transform, Visible, Visuals, resolve_line_height,
    text_baseline_in_line, text_block_top,
};
use crate::input::{Focused, ScrollOffset};
use bevy_ecs::prelude::*;
use bevy_ecs::query::Or;
use glam::Vec2;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Function pointer for an extract step. Stateless; closures are disallowed, so per-extract state lives in render-world resources.
///
/// Takes `&mut` on the main world to enable [`World::query`], which caches component-id resolution on first call.
pub type ExtractFn = fn(&mut World, &mut World);

/// Schedule label for the render schedule.
#[derive(bevy_ecs::schedule::ScheduleLabel, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Render;

/// Ordered stages inside the [`Render`] schedule.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum RenderStage {
    /// Build GPU buffers, vello scenes, and cache lookups for the upcoming submit.
    Prepare,
    /// Submit GPU draw work.
    Render,
}

/// Window viewport, inserted as a `Resource` into both the main and render worlds.
/// The window plugin writes both copies on every resize.
#[derive(Resource, Clone, Debug)]
pub struct Viewport {
    /// Logical-pixel window size. The window backend divides the raw
    /// `PhysicalSize` it receives from winit by [`scale_factor`] before
    /// writing here, so layout consumers see the same coordinate space
    /// CSS / pointer events do.
    ///
    /// [`scale_factor`]: Self::scale_factor
    pub size: Vec2,
    /// Background clear color.
    pub clear: Color,
    /// Current window scale factor (logical -> physical multiplier). Written
    /// by the window backend on startup, `Resized`, and `ScaleFactorChanged`.
    /// Defaults to `1.0` so headless / pre-window code paths can multiply
    /// through unconditionally.
    pub scale_factor: f32,
    /// HiDPI factor of the current monitor (e.g. `1.0`, `1.5`, `2.0`); written by the window backend on startup and `MonitorChanged`. `None` while monitor info is unavailable.
    pub monitor_scale: Option<f32>,
    /// Physical-pixel size of the current monitor; written by the window backend.
    pub monitor_size: Option<Vec2>,
    /// Monitor name reported by winit (for example `"DELL U2719D"`).
    pub monitor_name: Option<String>,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            size: Vec2::new(800.0, 600.0),
            clear: Color::rgba(0.0, 0.0, 0.0, 0.0),
            scale_factor: 1.0,
            monitor_scale: None,
            monitor_size: None,
            monitor_name: None,
        }
    }
}

/// Main-world resource shrinking the layout viewport of every normal root
/// while a panel is docked to a window edge (browser-devtools semantics: the
/// page reflows into the remaining space instead of being covered). Roots in
/// the top paint band ([`OverlayLayer`]) keep the full viewport - the docked
/// panel itself is one, so it can lay out along the edge it occupies.
///
/// Written by whoever docks the panel (the devtools overlay); read by the
/// layout backend. Absent or all-zero means no dock.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct DockInsets {
    /// Logical pixels taken from the right window edge.
    pub right: f32,
    /// Logical pixels taken from the bottom window edge.
    pub bottom: f32,
}

/// Per-frame flag indicating that the upcoming frame requires a fresh GPU encode.
///
/// - Set by [`roll_up_frame_dirty`] from `Changed<T>` filters on render-relevant components: [`Transform`], [`Visuals`], [`TextStyle`], [`TextContent`], [`TextInput`], [`TextInputScroll`], [`Opacity`], [`Visible`], [`Viewport`], [`crate::components::LumenClasses`], plus the [`crate::property_store::PropertyStore`] notify queue (any property write since the previous tick), plus child-set mutations (newly added [`Visible`] / removed `ChildOf`).
/// - Cleared by the window backend after submitting the frame.
/// - When unset, window backends skip GPU encode and submit in `RedrawRequested`.
///
/// Kept as a `bool` alias for legacy consumers (the wgpu render system).
/// Wave 2 lands [`FrameDamage`] as the typed replacement.
#[derive(Resource, Debug)]
pub struct FrameDirty {
    /// `true` when the upcoming frame needs a fresh GPU encode.
    pub dirty: bool,
}

impl Default for FrameDirty {
    fn default() -> Self {
        // Start dirty so the initial frame paints.
        Self { dirty: true }
    }
}

/// Per-tick flag raised by animation drivers (hover / press tweens,
/// opacity transitions, scroll inertia) while a value is still in motion.
///
/// Read by the window backend right after `App::tick()` to self-schedule a
/// follow-up frame (`RedrawScheduler.pending = true`) so an in-flight
/// animation keeps advancing without waiting for an unrelated OS event.
/// Without this, the first tween frame paints and then the loop parks -
/// the animation freezes mid-way until the next mouse move.
///
/// Idle-quiescence contract: the flag is stored in an [`AtomicBool`] so
/// several parallel animation systems can raise it via `&Res` without
/// serialising. [`reset_animations_active`] clears it at the *start* of
/// every tick (`TickStage::Input`, which is chained before
/// `TickStage::Systems` where the drivers run), and each driver re-raises
/// it *only while it still has motion left* (progress strictly short of its
/// target, non-zero velocity). The moment every animation settles, no
/// driver raises it, the flag stays `false`, and the scheduler parks - so
/// there is no permanent vsync spin.
#[derive(Resource, Debug, Default)]
pub struct AnimationsActive(std::sync::atomic::AtomicBool);

impl AnimationsActive {
    /// Raise the flag: at least one animation still has motion this tick.
    /// Interior-mutable so animation systems can take a shared `Res`.
    #[inline]
    pub fn request(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read the current flag without clearing it. The window backend calls
    /// this after the tick to decide whether to re-arm the redraw.
    #[inline]
    pub fn get(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Clear the flag. Called by [`reset_animations_active`] at tick start.
    #[inline]
    pub fn clear(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Clears [`AnimationsActive`] at the top of every tick (registered in
/// [`crate::tick::TickStage::Input`], which is chained before the
/// `Systems` stage where animation drivers run). Recomputing the flag
/// from scratch each tick is what guarantees idle-quiescence: a tick with
/// no live animation leaves it `false`.
pub fn reset_animations_active(flag: Res<AnimationsActive>) {
    flag.clear();
}

/// Axis-aligned rectangle in logical pixel coordinates. Used by [`FrameDamage`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Top-left corner.
    pub origin: Vec2,
    /// Width x height.
    pub size: Vec2,
}

impl Rect {
    /// Constructs a rect from origin and size.
    pub const fn new(origin: Vec2, size: Vec2) -> Self {
        Self { origin, size }
    }
}

/// Per-frame list of damage rectangles for partial-redraw / dirty-region rendering.
///
/// Will replace the boolean [`FrameDirty`] once wave 1.5 / wave 2 fills it. Foundation only installs the resource so
/// downstream producers and consumers have a stable type to target.
#[derive(Resource, Default, Debug)]
pub struct FrameDamage(pub Vec<Rect>);

impl FrameDamage {
    /// Clears the damage list.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Appends `r` to the damage list. The resource keeps duplicates and lets the consumer coalesce.
    pub fn push(&mut self, r: Rect) {
        self.0.push(r);
    }

    /// Returns `true` when no damage rectangles are recorded.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Per-extract-phase memo of the parent-derived maps that every extract fn
/// recomputes identically within a single frame.
///
/// The default extract pass runs six extract fns back-to-back
/// ([`extract_shadows`], [`extract_rects`], [`extract_borders`],
/// [`extract_text`], [`extract_clips`], [`extract_scrollbars`]) - plus the
/// image/SVG extractors registered by `lumen-assets` - and each one
/// independently rebuilds the same hierarchy-derived structures via
/// [`build_parent_map`], [`hidden_entities`], [`parent_scroll_offsets`],
/// [`parent_opacities`], and [`parent_scroll_clip_rects`]. Those depend
/// only on the main-world hierarchy / `ScrollOffset` / `Opacity` /
/// `Visible` / clip components, none of which mutate between extractors of
/// the same phase (extract fns only read the main world and write the
/// render world). Recomputing them six times is pure redundant traversal +
/// allocation on every dirty frame - the steady-state cost of any
/// animation, scroll, or state change.
///
/// [`crate::app::App::tick`] wraps the extract-fn loop with
/// [`Self::begin_phase`] / [`Self::end_phase`]. The first extractor of a
/// phase computes each map and stores a copy here; the rest clone it back
/// (an O(n) memcpy instead of the query + DFS + sort rebuild). Reuse is
/// gated on [`Self::active`], so Systems-stage callers that share these
/// helpers - hit-testing hover, which runs *before* the `<if>` / `<for>`
/// reconcilers have finalised the tree - never observe a cached, stale
/// hierarchy: with `active == false` they always recompute. `begin_phase`
/// clears every slot, so no data survives across phases either.
///
/// Mirrors the "compute the frame's scene-graph context once" pattern in
/// retained-scene toolkits (Qt Quick's `QSGRenderContext`, GTK4's snapshot
/// pass): shared per-frame derivations are built once and threaded through
/// the emitters, not rederived per draw-list.
#[derive(Resource, Default)]
pub struct ExtractContextCache {
    /// `true` only while [`crate::app::App::tick`] is running the extract
    /// fns. Consulted before any reuse so out-of-phase callers (hover
    /// hit-testing in `TickStage::Systems`) always recompute.
    active: bool,
    /// Memoised `(child -> parent-entity, entity -> paint-order)` pair.
    parent_map: Option<(HashMap<Entity, Entity>, HashMap<Entity, u32>)>,
    /// Memoised hidden-subtree set.
    hidden: Option<std::collections::HashSet<Entity>>,
    /// Memoised cumulative ancestor scroll offsets.
    scroll: Option<HashMap<Entity, Vec2>>,
    /// Memoised cumulative ancestor opacity products.
    opacities: Option<HashMap<Entity, f32>>,
    /// Memoised nearest-clip rect per entity.
    clip: Option<HashMap<Entity, (Vec2, Vec2)>>,
}

impl ExtractContextCache {
    /// Open the extract phase: enable reuse and drop any slots left from a
    /// prior phase so the first extractor recomputes against the current
    /// (post-reconcile, post-layout) hierarchy.
    pub fn begin_phase(&mut self) {
        self.active = true;
        self.parent_map = None;
        self.hidden = None;
        self.scroll = None;
        self.opacities = None;
        self.clip = None;
    }

    /// Close the extract phase: disable reuse and release the memoised
    /// maps so they cannot be observed by out-of-phase callers or pin
    /// memory between frames.
    pub fn end_phase(&mut self) {
        self.active = false;
        self.parent_map = None;
        self.hidden = None;
        self.scroll = None;
        self.opacities = None;
        self.clip = None;
    }
}

/// [`SystemSet`] label for render-world extract systems registered via [`crate::app::App::add_extract_systems`].
///
/// The default extract pass (the legacy [`Vec<ExtractFn>`]) runs in registration order before this set; downstream crates
/// that need `Changed<T>` extract semantics install their systems into this set on the [`ExtractSchedule`].
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ExtractSet {
    /// Extract draw data from already-extracted render-world entities.
    Extract,
}

/// Schedule label for the dedicated extract schedule on the render world.
///
/// Distinct from [`Render`] so extract systems and render systems can be reasoned about independently. Wave 2 migrates
/// the existing [`ExtractFn`] entries onto this schedule.
#[derive(bevy_ecs::schedule::ScheduleLabel, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct ExtractSchedule;

/// Rolls up render-relevant change signals into [`FrameDirty`].
/// Runs each tick in [`crate::tick::TickStage::A11ySync`]; the window backend reads the flag right before encoding and clears it after submission.
///
/// Sources, in order of preference:
/// 1. [`crate::property_store::PropertyStore::dirty_peek`] - the typed notify queue. Any global / entity property write since the last tick flips dirty. This is the long-term replacement for the per-component `Changed<T>` filters; wave 1 reads the queue so newly-installed bindable components (typed signals, theme, custom properties) flip dirty without needing a Query column here.
/// 2. `Changed<T>` filters on the legacy render-relevant components ([`Transform`], [`Visuals`], [`TextStyle`], [`TextContent`], [`TextInput`] (caret / selection moves), [`TextInputScroll`], [`Opacity`], [`Visible`], [`crate::components::LumenClasses`]). Kept until the wave 2 migration moves these onto [`crate::property_store::PropertyStore`] notify.
/// 3. [`Viewport`] resource change.
/// 4. Child-set mutations: `Added<Visible>` (newly-spawned / newly-hidden entities - fixes the `FrameDirty ignores child-set mutations` audit bug in `renderer.md:86`) and `RemovedComponents<ChildOf>` (despawn / reparent - same bug).
///
/// `dirty_peek` is non-destructive; downstream observer systems remain free to `drain_dirty` later in the tick.
///
/// Set `LUMEN_TRACE_FRAME_DIRTY=1` to log which source raised the flag each
/// tick (stderr) - the tool for hunting "app never idles" regressions.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn roll_up_frame_dirty(
    mut fd: ResMut<FrameDirty>,
    viewport: Res<Viewport>,
    property_store: Option<Res<crate::property_store::PropertyStore>>,
    component_changed: Query<
        (),
        Or<(
            bevy_ecs::query::Changed<Transform>,
            bevy_ecs::query::Changed<Visuals>,
            bevy_ecs::query::Changed<TextStyle>,
            bevy_ecs::query::Changed<TextContent>,
            bevy_ecs::query::Changed<TextInput>,
            bevy_ecs::query::Changed<TextInputScroll>,
            bevy_ecs::query::Changed<Opacity>,
            bevy_ecs::query::Changed<Visible>,
            bevy_ecs::query::Changed<crate::components::LumenClasses>,
            // Overlay-scrollbar fade: alpha steps must repaint even when
            // nothing else changed (the fade-out frames).
            bevy_ecs::query::Changed<crate::input::ScrollbarState>,
            // Runtime `type` / echo-mode flips (`bind-*`) must repaint even
            // when the underlying text is unchanged (mask <-> plaintext).
            bevy_ecs::query::Changed<EchoMode>,
            bevy_ecs::query::Added<EchoMode>,
            bevy_ecs::query::Added<Visible>,
        )>,
    >,
    mut removed_childof: RemovedComponents<bevy_ecs::hierarchy::ChildOf>,
    trace: (
        Query<Entity, bevy_ecs::query::Changed<Transform>>,
        Query<Entity, bevy_ecs::query::Changed<Visuals>>,
        Query<Entity, bevy_ecs::query::Changed<TextStyle>>,
        Query<Entity, bevy_ecs::query::Changed<TextContent>>,
        Query<Entity, bevy_ecs::query::Changed<TextInput>>,
        Query<Entity, bevy_ecs::query::Changed<TextInputScroll>>,
        Query<Entity, bevy_ecs::query::Changed<Opacity>>,
        Query<Entity, bevy_ecs::query::Changed<Visible>>,
        Query<Entity, bevy_ecs::query::Changed<crate::components::LumenClasses>>,
        Query<Entity, bevy_ecs::query::Changed<crate::input::ScrollbarState>>,
    ),
) {
    // Fully drain the `RemovedComponents` reader every tick. `.next()` would
    // consume only ONE entry: a tick that removes K `ChildOf` (closing a
    // `<for>` list, a page switch) would leave K-1 stale entries, each raising
    // `FrameDirty` on a later idle tick and preventing the app from parking.
    // `.count()` advances the cursor past every entry this tick while still
    // reporting whether any ChildOf was removed.
    let removed_any = removed_childof.read().count() > 0;
    if fd.dirty {
        return;
    }
    let property_dirty = property_store
        .as_ref()
        .is_some_and(|s| !s.dirty_peek().is_empty());
    if property_dirty || removed_any || viewport.is_changed() || !component_changed.is_empty() {
        fd.dirty = true;
        // Diagnostic: name the source(s). Env read is cached; disabled runs
        // pay one atomic load.
        static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let trace_on = *TRACE.get_or_init(|| std::env::var_os("LUMEN_TRACE_FRAME_DIRTY").is_some());
        if trace_on {
            let mut sources: Vec<String> = Vec::new();
            if property_dirty {
                let keys: Vec<String> = property_store
                    .as_ref()
                    .map(|s| s.dirty_peek().iter().map(|k| format!("{k:?}")).collect())
                    .unwrap_or_default();
                sources.push(format!("property_store{keys:?}"));
            }
            if removed_any {
                sources.push("removed ChildOf".into());
            }
            if viewport.is_changed() {
                sources.push("viewport".into());
            }
            let names = [
                "Transform",
                "Visuals",
                "TextStyle",
                "TextContent",
                "TextInput",
                "TextInputScroll",
                "Opacity",
                "Visible",
                "LumenClasses",
                "ScrollbarState",
            ];
            let counts = [
                trace.0.iter().count(),
                trace.1.iter().count(),
                trace.2.iter().count(),
                trace.3.iter().count(),
                trace.4.iter().count(),
                trace.5.iter().count(),
                trace.6.iter().count(),
                trace.7.iter().count(),
                trace.8.iter().count(),
                trace.9.iter().count(),
            ];
            for (name, count) in names.iter().zip(counts) {
                if count > 0 {
                    sources.push(format!("Changed<{name}>x{count}"));
                }
            }
            eprintln!("lumen-core: FrameDirty raised by {}", sources.join(", "));
        }
    }
}

/// Painter-algorithm sort key for `Extracted*` entries. Higher values paint later (closer to the viewer).
///
/// Encoded as `document_order_rank * 2`, where the rank is the pre-order DFS index over the entity
/// hierarchy forest computed by [`build_parent_map`]: ancestors paint before descendants and siblings
/// paint in `Children`-list order (markup / reconcile order - entity-id allocation order plays no part).
/// The `x2` stride keeps `order - 1` free so [`extract_shadows`] can slot shadows directly under their
/// source rect without colliding with the preceding leaf. Because pre-order ranks make every subtree a
/// contiguous range, an [`ExtractedClipBox`]'s `[start_order, end_order]` bracket covers exactly the
/// clipping entity's descendants.
///
/// The `u32` key space is partitioned into three bands, low to high:
///
/// 1. **Normal tree content** - `[0, OVERLAY_ORDER_BASE)`: the pre-order forest ranks described above,
///    excluding any subtree rooted at an [`OverlayLayer`] entity.
/// 2. **Top layer** - `[OVERLAY_ORDER_BASE, 0x8000_0000)`: subtrees rooted at an [`OverlayLayer`]
///    entity (dropdown / menu panels, tooltips, dialogs). Each subtree keeps contiguous internal
///    pre-order ranks; whole subtrees stack by open order (later-opened paints on top - see
///    [`OverlayOpenOrder`]).
/// 3. **Orphans** - `0x8000_0000 | (entity_index << 1)`: entities outside the hierarchy forest, ordered
///    by entity index - see [`paint_order_of`].
pub type PaintOrder = u32;

/// First [`PaintOrder`] of the top-layer band. Every rank in an [`OverlayLayer`] subtree is
/// `>= OVERLAY_ORDER_BASE`; every normal-tree rank is below it (real UI trees are nowhere near
/// `0x2000_0000` entities), so overlay content always paints after all normal content.
pub const OVERLAY_ORDER_BASE: PaintOrder = 0x4000_0000;

/// Marker component lifting an entity and its whole subtree into the top-layer paint band
/// (browser top-layer / Qt popup-window semantics): the subtree paints after ALL normal tree
/// content regardless of its document position, keeps its internal document order, and escapes
/// ancestor clip / scroll-cull rects (its own internal clips still apply).
///
/// Attach to popup roots: `<dropdown>` / `<menu>` floating panels, tooltips, `<dialog>` wrappers.
/// Painting only - hit-testing and layout are unaffected.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct OverlayLayer;

/// Main-world resource tracking the order in which [`OverlayLayer`] roots became visible, so
/// concurrently open popups stack later-opened-on-top (like OS popup windows).
///
/// Maintained by [`build_parent_map`]: a visible (not [`Visible(false)`]-hidden) overlay root gets a
/// monotonically increasing stamp on first sight; hiding or despawning the root drops its stamp, so
/// re-opening restamps it on top. Self-inserted on first use.
#[derive(Resource, Default, Debug)]
pub struct OverlayOpenOrder {
    /// Overlay root -> open stamp. Lower stamps paint first (under later-opened popups).
    pub stamps: HashMap<Entity, u64>,
    /// Next stamp to hand out.
    pub next: u64,
}

/// Fill brush for an [`ExtractedRect`]. Gradient variants store their stops in an `Arc<[...]>` so cloning the brush bumps the Arc instead of deep-copying.
///
/// `PartialEq` compares appearance so the retained Node-IR damage diff
/// ([`crate::node_ir`]) can tell an unchanged fill from a changed one - the
/// producer rebuilds a fresh brush every frame, so identity (`Arc::ptr_eq`)
/// alone never matches across frames.
#[derive(Clone, Debug, PartialEq)]
pub enum Brush {
    /// Solid fill.
    Solid(Color),
    /// Linear gradient. `angle_deg` follows the CSS convention (`0` = left->right, `90` = bottom->top, `180` = top->bottom); `stops` are sorted by ascending offset.
    Linear {
        /// Direction in degrees.
        angle_deg: f32,
        /// `(offset, color)` pairs with `offset` in `0..=1`.
        stops: std::sync::Arc<[(f32, Color)]>,
    },
    /// Radial gradient centred at 50% / 50% of the entity rect.
    Radial {
        /// Normalised radius in `0..=1` relative to half the rect's min dimension.
        radius: f32,
        /// `(offset, color)` pairs with `offset` in `0..=1`.
        stops: std::sync::Arc<[(f32, Color)]>,
    },
    /// Conic (sweep) gradient centred at 50% / 50%.
    Conic {
        /// Starting angle in degrees.
        from_deg: f32,
        /// `(offset, color)` pairs with `offset` in `0..=1`.
        stops: std::sync::Arc<[(f32, Color)]>,
    },
}

impl Brush {
    /// Returns a fresh brush with `alpha` multiplied into every color/stop.
    /// Gradient variants allocate a new stops Arc; the solid variant only updates the inner color.
    pub fn with_opacity(self, alpha: Opacity) -> Self {
        match self {
            Brush::Solid(c) => Brush::Solid(alpha.apply(c)),
            Brush::Linear { angle_deg, stops } => {
                let mapped: std::sync::Arc<[(f32, Color)]> =
                    stops.iter().map(|(o, c)| (*o, alpha.apply(*c))).collect();
                Brush::Linear {
                    angle_deg,
                    stops: mapped,
                }
            }
            Brush::Radial { radius, stops } => {
                let mapped: std::sync::Arc<[(f32, Color)]> =
                    stops.iter().map(|(o, c)| (*o, alpha.apply(*c))).collect();
                Brush::Radial {
                    radius,
                    stops: mapped,
                }
            }
            Brush::Conic { from_deg, stops } => {
                let mapped: std::sync::Arc<[(f32, Color)]> =
                    stops.iter().map(|(o, c)| (*o, alpha.apply(*c))).collect();
                Brush::Conic {
                    from_deg,
                    stops: mapped,
                }
            }
        }
    }
}

impl From<&Fill> for Brush {
    fn from(f: &Fill) -> Self {
        match f {
            Fill::Solid(c) => Brush::Solid(*c),
            Fill::Linear { angle_deg, stops } => Brush::Linear {
                angle_deg: *angle_deg,
                stops: stops.iter().copied().collect(),
            },
            Fill::Radial { radius, stops } => Brush::Radial {
                radius: *radius,
                stops: stops.iter().copied().collect(),
            },
            Fill::Conic { from_deg, stops } => Brush::Conic {
                from_deg: *from_deg,
                stops: stops.iter().copied().collect(),
            },
        }
    }
}

/// One filled rectangle to render this frame.
#[derive(Component, Clone, Debug)]
pub struct ExtractedRect {
    /// Top-left in window coordinates.
    pub origin: Vec2,
    /// Width x height.
    pub size: Vec2,
    /// Fill brush - solid or linear gradient.
    pub brush: Brush,
    /// Uniform corner radius in logical pixels (`0.0` = sharp).
    pub radius: f32,
    /// Per-corner radii `[top-left, top-right, bottom-right,
    /// bottom-left]`; `None` = uniform [`Self::radius`] everywhere.
    pub corner_radii: Option<[f32; 4]>,
    /// Global paint order (see [`PaintOrder`]).
    pub order: PaintOrder,
}

/// Shadow extracted for one entity. The renderer draws it at `order = rect.order - 1` so it appears underneath the source rect without bleeding onto unrelated siblings.
#[derive(Component, Clone, Copy, Debug)]
pub struct ExtractedShadow {
    /// Top-left in window coordinates, already including the per-shadow offset.
    pub origin: Vec2,
    /// Size of the source rect, used by vello to compute the blur bounding box.
    pub size: Vec2,
    /// Corner radius of the source rect.
    pub radius: f32,
    /// CSS spread radius: inflate (positive) / deflate (negative) the
    /// shadow rect on every side before blurring.
    pub spread: f32,
    /// Gaussian blur std-dev.
    pub blur: f32,
    /// Shadow color (alpha controls softness).
    pub color: Color,
    /// Global paint order; placed strictly below the source rect.
    pub order: PaintOrder,
    /// `true` for an inset shadow: the renderer clips to the entity's bbox and draws at the negated offset. `false` for a drop shadow.
    pub inner: bool,
    /// Source rect top-left (`origin` minus the per-shadow offset). Inset shadows use it for the clip boundary and offset flip.
    pub rect_origin: Vec2,
}

/// CSS border for one entity: per-side widths, one solid color, painted
/// between the outer border-box edge and the padding box (inside the
/// rect, unlike [`ExtractedOutline`] which strokes centered on the box
/// edge). Emitted by [`extract_borders`] at the entity's own
/// [`PaintOrder`]; the IR builder pushes borders after rects, so at the
/// shared order key the border paints above the background fill and
/// below all descendants - CSS's background -> border -> content order.
#[derive(Component, Clone, Copy, Debug)]
pub struct ExtractedBorder {
    /// Border-box top-left in window coordinates.
    pub origin: Vec2,
    /// Border-box size.
    pub size: Vec2,
    /// Per-side widths `[top, right, bottom, left]` in logical pixels.
    pub widths: [f32; 4],
    /// Solid border color (all four sides).
    pub color: Color,
    /// Per-side color overrides `[top, right, bottom, left]`; `None` =
    /// every side paints [`Self::color`].
    pub side_colors: Option<[Color; 4]>,
    /// Outer corner radius (matches the entity's [`Visuals::radius`]).
    pub radius: f32,
    /// Per-corner outer radii `[tl, tr, br, bl]`; `None` = uniform.
    pub corner_radii: Option<[f32; 4]>,
    /// Global paint order (see [`PaintOrder`]).
    pub order: PaintOrder,
}

/// Stroked outline ring rendered around an entity (typically when
/// [`crate::input::Focused`]).
#[derive(Component, Clone, Copy, Debug)]
pub struct ExtractedOutline {
    /// Top-left in window coordinates (matches the rect being outlined).
    pub origin: Vec2,
    /// Width x height of the box being outlined.
    pub size: Vec2,
    /// Stroke color.
    pub stroke: Color,
    /// Stroke width in logical pixels.
    pub width: f32,
    /// Uniform corner radius (matches the underlying box).
    pub radius: f32,
    /// Global paint order (see [`PaintOrder`]).
    pub order: PaintOrder,
}

/// One decoded image to render this frame in RGBA8 row-major pixels.
/// The renderer wraps `rgba` in a `peniko::Blob` whose stable identity keys the vello/wgpu upload cache across frames.
#[derive(Component, Clone, Debug)]
pub struct ExtractedImage {
    /// Top-left in window coordinates.
    pub origin: Vec2,
    /// Drawn size (the image is scaled into `size` according to [`Self::fit`]).
    pub size: Vec2,
    /// Source pixel width.
    pub width: u32,
    /// Source pixel height.
    pub height: u32,
    /// RGBA8 pixels, `width * height * 4` bytes.
    pub rgba: std::sync::Arc<[u8]>,
    /// How the source image fits into the drawn rectangle.
    pub fit: crate::components::ImageFit,
    /// Global paint order (see [`PaintOrder`]).
    pub order: PaintOrder,
    /// Alpha multiplier from [`crate::components::Opacity`] (1.0 when absent). Applied via `push_layer` at draw time so the whole image fades together.
    pub alpha: f32,
}

/// Single built-in fallback for the selection highlight when no
/// `selection-color` token is set - the platform "highlight" blue at ~40 %
/// alpha (mirrors macOS / web selection tints and Qt's `Highlight` role).
/// Translucent on purpose so the text underneath keeps its contrast, so no
/// `selection-text-color` is required for legibility. Skins override the
/// whole look via the `--lumen-selection` token; this is the one Rust
/// fallback.
pub const DEFAULT_SELECTION_BG: Color = Color::rgba(0.20, 0.51, 0.98, 0.40);

/// One text run to render this frame.
///
/// `PartialEq` compares every visual field so the retained Node-IR damage
/// diff can skip an unchanged label without re-shaping - deterministic
/// shaping means identical fields => identical glyphs => identical pixels.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct ExtractedText {
    /// Baseline origin in window coordinates at the container's leading edge. The renderer shifts by `(container_width - measured_width)` times an alignment fraction to honour [`Self::align`].
    pub origin: Vec2,
    /// Unshaped text passed to the shaper.
    pub text: String,
    /// Font size in logical pixels.
    pub size_px: f32,
    /// Fill color.
    pub fill: Color,
    /// `Some(byte_offset)` paints a vertical caret at the corresponding pixel position inside `text`.
    pub caret: Option<usize>,
    /// `Some((start, end))` paints a translucent selection highlight between the byte offsets; `start < end`.
    pub selection: Option<(usize, usize)>,
    /// Selection highlight background. `None` falls back to
    /// [`DEFAULT_SELECTION_BG`]. Sourced from
    /// [`TextStyle::selection_color`] (`selection-color` CSS property).
    pub selection_color: Option<Color>,
    /// Selected-glyph color (Qt `HighlightedText` / Slint
    /// `selection-foreground-color`). `None` keeps glyphs their normal
    /// [`Self::fill`]. Sourced from [`TextStyle::selection_foreground`].
    pub selection_foreground: Option<Color>,
    /// Caret color. `None` falls back to [`Self::fill`] (the text color).
    /// Sourced from [`TextStyle::caret_color`] (`caret-color`).
    pub caret_color: Option<Color>,
    /// Global paint order.
    pub order: PaintOrder,
    /// Container width within which the text is aligned. With `align == Start` the renderer draws at `origin` directly; other alignments measure the run and shift inside this width.
    pub container_width: f32,
    /// Horizontal alignment policy.
    pub align: TextAlign,
    /// Wrap policy passed to the shaper.
    pub wrap: crate::components::TextWrap,
    /// Hard cap on shaped line count; `None` is unbounded.
    pub max_lines: Option<u32>,
    /// CSS `font-family` fallback chain (`None` = platform sans-serif).
    pub family: Option<std::sync::Arc<str>>,
    /// CSS `font-weight` (1-1000; 400 = normal).
    pub weight: u16,
    /// Resolved CSS `line-height` in logical pixels (already resolved
    /// against [`Self::size_px`] via [`resolve_line_height`]). Drives
    /// inter-line spacing in the shaper and the newline-caret math.
    pub line_height_px: f32,
    /// Resolved text-input caret stroke width in logical pixels (already
    /// resolved against [`CARET_WIDTH_PX`] or a [`CaretWidth`] override).
    pub caret_width_px: f32,
}

/// One rectangular clip region constraining descendant paints.
///
/// - The renderer pushes a vello layer at `start_order` and pops it at `end_order`.
/// - Emitted for `<scroll>` containers and entities with `overflow: hidden`.
#[derive(Component, Clone, Copy, Debug)]
pub struct ExtractedClipBox {
    /// Top-left in window coordinates.
    pub origin: Vec2,
    /// Width x height in logical pixels.
    pub size: Vec2,
    /// Corner radius of the clip rect; matches the entity's visual radius for rounded clips.
    pub radius: f32,
    /// Paint-order key at which the layer is pushed - the clipping entity's own [`PaintOrder`].
    /// Because paint order is a pre-order document rank, everything in `[start_order, end_order]`
    /// is the clipping entity itself plus exactly its descendants.
    pub start_order: PaintOrder,
    /// Paint-order key at which the layer is popped - the maximum [`PaintOrder`] across the clipping
    /// entity and its descendants, so the pop trails every descendant and nothing else.
    pub end_order: PaintOrder,
}

/// One rounded solid rect of an overlay scrollbar (track or thumb).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarDrawRect {
    /// Top-left in window coordinates.
    pub origin: Vec2,
    /// Width x height in logical pixels.
    pub size: Vec2,
    /// Solid fill (fade alpha already folded in).
    pub color: Color,
    /// Corner radius (pill = half the bar thickness).
    pub radius: f32,
}

/// Overlay-scrollbar draw list for one scroll container (spec section 16.2 /
/// section 16.6). Emitted by [`extract_scrollbars`]; the IR builder appends the
/// rects - in `draws` order - AFTER every other leaf sharing the same
/// [`PaintOrder`], so bars always paint above the container's content
/// (the `order` is the container's `max descendant order + 1`, which
/// also places them outside the container's clip bracket - overlay bars
/// are never clipped by their own viewport).
#[derive(Component, Clone, Debug)]
pub struct ExtractedScrollbar {
    /// Track / thumb rects in back-to-front paint order.
    pub draws: Vec<ScrollbarDrawRect>,
    /// Global paint order shared by every rect in `draws`.
    pub order: PaintOrder,
}

/// Extract fn emitting one [`ExtractedScrollbar`] per [`crate::input::Scroll`]
/// container whose content overflows on an allowed axis and whose fade
/// alpha is above zero. Geometry comes from the shared
/// [`crate::input::vertical_scrollbar`] / [`crate::input::horizontal_scrollbar`]
/// math so painted pixels and hit regions always agree. All visuals
/// (colors, thickness, minimums) resolve through
/// [`crate::input::ScrollbarStyle`] - CSS `scrollbar-color` /
/// `scrollbar-width` per container, with the component's [`Default`] as
/// the no-stylesheet fallback.
pub fn extract_scrollbars(main: &mut World, render: &mut World) {
    use crate::input::{
        Scroll, ScrollbarAxisPick, ScrollbarInteraction, ScrollbarState, ScrollbarStyle,
        horizontal_scrollbar, vertical_scrollbar,
    };
    let (parents, mut depth_cache) = build_parent_map(main);
    let hidden = hidden_entities(main, &parents);
    let scroll_offsets = parent_scroll_offsets(main, &parents);
    let interaction = main
        .get_resource::<ScrollbarInteraction>()
        .copied()
        .unwrap_or_default();

    // Child lookup for content extents + descendant paint orders.
    let mut children: std::collections::HashMap<Entity, Vec<Entity>> =
        std::collections::HashMap::new();
    for (&e, &p) in parents.iter() {
        children.entry(p).or_default().push(e);
    }
    let transforms: std::collections::HashMap<Entity, Transform> = {
        let mut q = main.query::<(Entity, &Transform)>();
        q.iter(main).map(|(e, t)| (e, *t)).collect()
    };

    type Row<'a> = (
        Entity,
        &'a Transform,
        &'a Scroll,
        &'a ScrollOffset,
        &'a ScrollbarState,
        Option<&'a ScrollbarStyle>,
    );
    let mut q = main.query::<Row>();
    let mut pairs: Vec<(Entity, ExtractedScrollbar)> = Vec::new();
    for (e, tf, scroll, offset, state, style) in q.iter(main) {
        if hidden.contains(&e) || state.alpha <= 0.001 {
            continue;
        }
        let style = style.copied().unwrap_or_default();
        // `scrollbar-width: none` - content scrolls, bars never paint.
        let Some(metrics) = style.metrics() else {
            continue;
        };
        // Content extent: bbox of direct children relative to the
        // container - same rule `clamp_scroll_offsets` applies.
        let (mut content_w, mut content_h) = (0.0_f32, 0.0_f32);
        if let Some(kids) = children.get(&e) {
            for kid in kids {
                if let Some(kt) = transforms.get(kid) {
                    content_w = content_w.max((kt.absolute.x - tf.absolute.x) + kt.size.x);
                    content_h = content_h.max((kt.absolute.y - tf.absolute.y) + kt.size.y);
                }
            }
        }
        let allow_y = scroll.axis.allows_y();
        let allow_x = scroll.axis.allows_x();
        // The viewport box itself translates with ANCESTOR scrollers
        // (its own offset moves content, not its box).
        let anc_off = scroll_offsets.get(&e).copied().unwrap_or(Vec2::ZERO);
        let origin = tf.absolute - anc_off;
        let v_overflow = allow_y && content_h - tf.size.y > 0.5;
        let h_overflow = allow_x && content_w - tf.size.x > 0.5;
        let v_geo = if v_overflow {
            vertical_scrollbar(origin, tf.size, content_h, offset.0.y, h_overflow, metrics)
        } else {
            None
        };
        let h_geo = if h_overflow {
            horizontal_scrollbar(origin, tf.size, content_w, offset.0.x, v_overflow, metrics)
        } else {
            None
        };
        if v_geo.is_none() && h_geo.is_none() {
            continue;
        }

        let fade = state.alpha.clamp(0.0, 1.0);
        let radius = metrics.thickness / 2.0;

        let mut draws: Vec<ScrollbarDrawRect> = Vec::with_capacity(4);
        let mut push_bar = |geo: crate::input::ScrollbarGeometry, axis: ScrollbarAxisPick| {
            let hovered = interaction
                .drag
                .map(|d| d.entity == e && d.axis == axis)
                .unwrap_or(false)
                || interaction
                    .hover
                    .map(|(he, ha, _)| he == e && ha == axis)
                    .unwrap_or(false);
            // Track: an explicit `scrollbar-color` track paints whenever
            // the bar is visible (CSS semantics); the fallback track
            // shows on hover only (overlay convention).
            let track = match style.track {
                Some(c) => Some(c),
                None if hovered => Some(style.hover_track),
                None => None,
            };
            if let Some(mut track) = track {
                track.a *= fade;
                draws.push(ScrollbarDrawRect {
                    origin: geo.track_origin,
                    size: geo.track_size,
                    color: track,
                    radius,
                });
            }
            let mut thumb = style.thumb;
            thumb.a =
                (thumb.a * if hovered { style.hover_boost } else { 1.0 }).clamp(0.0, 1.0) * fade;
            draws.push(ScrollbarDrawRect {
                origin: geo.thumb_origin,
                size: geo.thumb_size,
                color: thumb,
                radius,
            });
        };
        if let Some(geo) = v_geo {
            push_bar(geo, ScrollbarAxisPick::Vertical);
        }
        if let Some(geo) = h_geo {
            push_bar(geo, ScrollbarAxisPick::Horizontal);
        }

        // Paint above every descendant: max-descendant-order + 1 (odd,
        // so it never collides with a document rank and sits after the
        // container's clip bracket pops).
        let mut end = paint_order_of(e, &parents, &mut depth_cache);
        let mut stack = vec![e];
        while let Some(n) = stack.pop() {
            let o = paint_order_of(n, &parents, &mut depth_cache);
            if o > end {
                end = o;
            }
            if let Some(kids) = children.get(&n) {
                stack.extend(kids.iter().copied());
            }
        }
        pairs.push((
            e,
            ExtractedScrollbar {
                draws,
                order: end.saturating_add(1),
            },
        ));
    }

    // Keyed-upsert against `RenderEntityMap.scrollbar` - same lifecycle
    // as `extract_rects`.
    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().scrollbar);
    let mut next: std::collections::HashMap<Entity, Entity> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (main_e, bar) in pairs {
        let reuse = prior
            .get(&main_e)
            .copied()
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert(bar);
                re
            }
            None => render.spawn(bar).id(),
        };
        next.insert(main_e, render_e);
    }
    for (main_e, render_e) in &prior {
        if !next.contains_key(main_e)
            && let Ok(em) = render.get_entity_mut(*render_e)
        {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().scrollbar = next;
}

/// Render-world snapshot of the set of MAIN-world entities that are hidden by
/// a [`Visible(false)`] on themselves or any ancestor (CSS `visibility:
/// hidden` semantics - the box keeps its layout space but paints nothing).
///
/// Written every extract phase by [`stash_hidden_entities`] and consumed by
/// the [`cull_hidden`] prepare system, which despawns any [`RenderEntityMap`]
/// entry whose owning main entity is hidden. This is the general guarantee
/// that a hidden subtree contributes ZERO paint nodes regardless of which
/// extractor emitted it: the per-extractor `hidden_entities` filters keep
/// hidden content from ever being extracted (the fast path), and this guard
/// catches anything an extractor forgets - so no extractor, core or plugin,
/// can leak a hidden subtree into the scene.
#[derive(Resource, Default, Debug)]
pub struct HiddenExtracts(pub std::collections::HashSet<Entity>);

/// Extract fn: recompute the hidden-subtree set from the main-world hierarchy
/// and mirror it into the render world for [`cull_hidden`].
///
/// Registered as the FIRST default extract so it primes the shared
/// [`ExtractContextCache`] hierarchy memos (`parents`, `hidden`) that every
/// following extractor reuses within the same phase.
pub fn stash_hidden_entities(main: &mut World, render: &mut World) {
    let (parents, _) = build_parent_map(main);
    let hidden = hidden_entities(main, &parents);
    render.resource_mut::<HiddenExtracts>().0 = hidden;
}

/// Prepare-stage guard that despawns every extracted render entity whose
/// owning main entity is hidden (per [`HiddenExtracts`]), then drops those
/// entries from [`RenderEntityMap`] so the next frame's keyed upserts start
/// clean. Runs before [`crate::node_ir::transform_extracted_to_nodes`] so no
/// hidden leaf reaches the retained tree.
///
/// Belt-and-suspenders behind the per-extractor `hidden_entities` filters:
/// with every current extractor filtering, this system finds nothing to do
/// on the common path; it exists so a future extractor that forgets the
/// filter still cannot paint a hidden subtree.
pub fn cull_hidden(
    mut commands: Commands,
    hidden: Res<HiddenExtracts>,
    mut map: ResMut<RenderEntityMap>,
) {
    if hidden.0.is_empty() {
        return;
    }
    let h = &hidden.0;
    let mut victims: Vec<Entity> = Vec::new();
    // Reborrow through `DerefMut` once so the per-field borrows below are seen
    // as disjoint (a `ResMut` smart pointer borrows the whole resource).
    let map = &mut *map;
    for m in [
        &mut map.rect,
        &mut map.text,
        &mut map.outline,
        &mut map.border,
        &mut map.image,
        &mut map.svg,
        &mut map.clip,
        &mut map.scrollbar,
        &mut map.native,
    ] {
        m.retain(|main_e, render_e| {
            if h.contains(main_e) {
                victims.push(*render_e);
                false
            } else {
                true
            }
        });
    }
    // Shadows map one main entity to a stack of render entities.
    map.shadow.retain(|main_e, render_es| {
        if h.contains(main_e) {
            victims.extend(render_es.iter().copied());
            false
        } else {
            true
        }
    });
    for e in victims {
        commands.entity(e).despawn();
    }
}

/// Despawns extracted entities whose AABB lies fully outside the [`Viewport`].
///
/// - Runs in [`RenderStage::Prepare`] (registered automatically by `App::new`).
/// - Performs only an AABB test against the viewport; partial-overlap clipping is the renderer's responsibility.
pub fn cull_offscreen(
    mut commands: Commands,
    viewport: Res<Viewport>,
    rects: Query<(Entity, &ExtractedRect)>,
    texts: Query<(Entity, &ExtractedText)>,
) {
    let vw = viewport.size.x;
    let vh = viewport.size.y;
    for (e, r) in &rects {
        if r.origin.x + r.size.x <= 0.0
            || r.origin.y + r.size.y <= 0.0
            || r.origin.x >= vw
            || r.origin.y >= vh
        {
            commands.entity(e).despawn();
        }
    }
    for (e, t) in &texts {
        // Approximate the text bounds with `size_px * char_count + 1` for width and `size_px` for height.
        let h = t.size_px;
        let w = t.size_px * (t.text.chars().count() as f32 + 1.0);
        if t.origin.x + w <= 0.0
            || t.origin.y + h <= 0.0
            || t.origin.x >= vw
            // Text uses the same top-left AABB convention as rects: cull below
            // when the top edge is at/under the viewport bottom. Previously
            // `origin.y - h >= vh` treated `origin` as a baseline only for the
            // below test, keeping text just past the bottom alive and reshaping
            // it every frame during a scroll.
            || t.origin.y >= vh
        {
            commands.entity(e).despawn();
        }
    }
}

// `lumen-assets::extract_loaded_images` provides the image extract fn and is registered externally.

/// Persistent map from a main-world entity to its render-world entity for each `Extracted*` type.
/// Upserting extracts read the prior map, update render entities in place, and write the new map back; legacy despawn-and-respawn extracts leave their slot untouched.
#[derive(Resource, Default, Debug)]
pub struct RenderEntityMap {
    /// `main_entity -> render_entity` for [`ExtractedRect`].
    pub rect: std::collections::HashMap<Entity, Entity>,
    /// `main_entity -> render_entity` for [`ExtractedText`].
    pub text: std::collections::HashMap<Entity, Entity>,
    /// `main_entity -> Vec<render_entity>` for [`ExtractedShadow`]; one main entity can carry multiple stacked shadows.
    pub shadow: std::collections::HashMap<Entity, Vec<Entity>>,
    /// `main_entity -> render_entity` for [`ExtractedOutline`].
    pub outline: std::collections::HashMap<Entity, Entity>,
    /// `main_entity -> render_entity` for [`ExtractedBorder`].
    pub border: std::collections::HashMap<Entity, Entity>,
    /// `main_entity -> render_entity` for [`ExtractedImage`].
    pub image: std::collections::HashMap<Entity, Entity>,
    /// `main_entity -> render_entity` for `lumen_assets::ExtractedSvg`. Stores only `Entity` ids so the core crate avoids a vello dependency.
    pub svg: std::collections::HashMap<Entity, Entity>,
    /// `main_entity -> render_entity` for [`ExtractedClipBox`]; one entry per scrollable or overflow-hidden container.
    pub clip: std::collections::HashMap<Entity, Entity>,
    /// `main_entity -> render_entity` for [`ExtractedScrollbar`]; one
    /// entry per scroll container with visible overlay bars.
    pub scrollbar: std::collections::HashMap<Entity, Entity>,
    /// `main_entity -> render_entity` for [`crate::native::ExtractedNative`]; one entry per
    /// plugin-painted leaf.
    pub native: std::collections::HashMap<Entity, Entity>,
}

/// Despawns transient render-world entities carrying `Extracted*` components, called once per frame before any [`ExtractFn`] runs.
///
/// - Entities listed in [`RenderEntityMap`] are preserved; their owning extract upserts them in place.
/// - Resources and non-send resources (vello scenes, GPU caches) are unaffected.
pub fn clear_extracted(render: &mut World) {
    let upserted: std::collections::HashSet<Entity> = {
        let map = render.resource::<RenderEntityMap>();
        let mut set: std::collections::HashSet<Entity> =
            std::collections::HashSet::with_capacity(map.rect.len() + map.shadow.len());
        set.extend(map.rect.values().copied());
        set.extend(map.shadow.values().flatten().copied());
        set.extend(map.text.values().copied());
        set.extend(map.outline.values().copied());
        set.extend(map.border.values().copied());
        set.extend(map.clip.values().copied());
        set.extend(map.image.values().copied());
        set.extend(map.svg.values().copied());
        set.extend(map.scrollbar.values().copied());
        set.extend(map.native.values().copied());
        set
    };
    let to_despawn: Vec<Entity> = render
        .query_filtered::<Entity, Or<(
            With<ExtractedRect>,
            With<ExtractedText>,
            With<ExtractedImage>,
            With<ExtractedOutline>,
            With<ExtractedBorder>,
            With<ExtractedShadow>,
            With<ExtractedScrollbar>,
            With<crate::native::ExtractedNative>,
        )>>()
        .iter(render)
        .filter(|e| !upserted.contains(e))
        .collect();
    for e in to_despawn {
        render.despawn(e);
    }
}

/// Extract fn that emits one [`ExtractedShadow`] per entry in [`Visuals::shadows`].
/// Each shadow is placed at `rect.order - 1 + idx` so it paints under the source rect and stacked shadows keep source order.
pub fn extract_shadows(main: &mut World, render: &mut World) {
    let (parents, mut depth_cache) = build_parent_map(main);
    let hidden = hidden_entities(main, &parents);
    let scroll = parent_scroll_offsets(main, &parents);
    let inherited_alpha = parent_opacities(main, &parents);
    let mut q = main.query::<(Entity, &Transform, &Visuals, Option<&Opacity>)>();
    // Group shadows by main entity so the upsert can grow or shrink each entity's render-side set.
    let mut groups: std::collections::HashMap<Entity, Vec<ExtractedShadow>> =
        std::collections::HashMap::new();
    for (e, t, v, opacity) in q.iter(main) {
        if hidden.contains(&e) || v.shadows.is_empty() {
            continue;
        }
        let alpha = effective_opacity(opacity, &inherited_alpha, e);
        let off = scroll.get(&e).copied().unwrap_or(Vec2::ZERO);
        let base_order = paint_order_of(e, &parents, &mut depth_cache).saturating_sub(1);
        let mut entries = Vec::with_capacity(v.shadows.len());
        for (idx, s) in v.shadows.iter().enumerate() {
            let order = base_order.saturating_add(idx as u32);
            entries.push(ExtractedShadow {
                origin: Vec2::new(t.absolute.x + s.offset_x, t.absolute.y + s.offset_y) - off,
                size: t.size,
                radius: v.radius,
                spread: s.spread,
                blur: s.blur,
                color: alpha.apply(s.color),
                order,
                inner: s.inner,
                rect_origin: t.absolute - off,
            });
        }
        groups.insert(e, entries);
    }

    // Keyed-upsert against `RenderEntityMap.shadow` (`main -> Vec<render>`).
    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().shadow);
    let mut next: std::collections::HashMap<Entity, Vec<Entity>> =
        std::collections::HashMap::with_capacity(groups.len());
    for (main_e, shadows) in groups {
        // Filter prior slots by current render-world validity to drop recycled entity indices.
        let mut slots: Vec<Entity> = prior
            .get(&main_e)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|&re| render.get_entity(re).is_ok())
            .collect();
        while slots.len() < shadows.len() {
            slots.push(
                render
                    .spawn(ExtractedShadow {
                        origin: Vec2::ZERO,
                        size: Vec2::ZERO,
                        radius: 0.0,
                        spread: 0.0,
                        blur: 0.0,
                        color: Color::rgba(0.0, 0.0, 0.0, 0.0),
                        order: 0,
                        inner: false,
                        rect_origin: Vec2::ZERO,
                    })
                    .id(),
            );
        }
        while slots.len() > shadows.len() {
            let drop = slots.pop().unwrap();
            if let Ok(em) = render.get_entity_mut(drop) {
                em.despawn();
            }
        }
        for (re, s) in slots.iter().copied().zip(shadows) {
            render.entity_mut(re).insert(s);
        }
        next.insert(main_e, slots);
    }
    // Despawn the entire render-side stack for main entities not present in `next`.
    for (main_e, slots) in &prior {
        if !next.contains_key(main_e) {
            for re in slots {
                if let Ok(em) = render.get_entity_mut(*re) {
                    em.despawn();
                }
            }
        }
    }
    render.resource_mut::<RenderEntityMap>().shadow = next;
}

/// Default extract fn that emits one [`ExtractedRect`] per main-world entity carrying a [`Transform`] and a [`Visuals::fill`].
///
/// - Paints in deterministic order via [`PaintOrder`]: pre-order document/tree order.
/// - Skips entities whose AABB falls fully outside the nearest scroll / overflow-hidden ancestor.
pub fn extract_rects(main: &mut World, render: &mut World) {
    let (parents, mut depth_cache) = build_parent_map(main);
    let hidden = hidden_entities(main, &parents);
    let scroll = parent_scroll_offsets(main, &parents);
    let inherited_alpha = parent_opacities(main, &parents);
    let clip = parent_scroll_clip_rects(main, &parents);
    let mut q = main.query::<(Entity, &Transform, &Visuals, Option<&Opacity>)>();
    let pairs: Vec<(Entity, ExtractedRect)> = q
        .iter(main)
        .filter(|(e, _, _, _)| !hidden.contains(e))
        .filter_map(|(e, t, v, opacity)| {
            let alpha = effective_opacity(opacity, &inherited_alpha, e);
            let brush = Brush::from(v.fill.as_ref()?).with_opacity(alpha);
            let off = scroll.get(&e).copied().unwrap_or(Vec2::ZERO);
            let origin = t.absolute - off;
            // Drop entities whose AABB is fully outside the nearest scroll / overflow-hidden ancestor's clip rect.
            if let Some(clip_rect) = clip.get(&e)
                && aabb_outside(origin, t.size, *clip_rect)
            {
                return None;
            }
            Some((
                e,
                ExtractedRect {
                    origin,
                    size: t.size,
                    brush,
                    radius: v.radius,
                    corner_radii: v.corner_radii,
                    order: paint_order_of(e, &parents, &mut depth_cache),
                },
            ))
        })
        .collect();

    // Keyed-upsert against `RenderEntityMap.rect`.
    // `mem::take` releases the resource borrow so the render entities can be mutated below.
    // Each prior render entity is re-validated; a recycled id is treated as a miss and spawned fresh.
    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().rect);
    let mut next: std::collections::HashMap<Entity, Entity> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (main_e, rect) in pairs {
        let reuse = prior
            .get(&main_e)
            .copied()
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert(rect);
                re
            }
            None => render.spawn(rect).id(),
        };
        next.insert(main_e, render_e);
    }
    // Despawn render entities whose main entity is no longer in `next`; validate first to avoid panicking on recycled ids.
    for (main_e, render_e) in &prior {
        if !next.contains_key(main_e)
            && let Ok(em) = render.get_entity_mut(*render_e)
        {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().rect = next;
}

/// Extract fn that emits one [`ExtractedBorder`] per main-world entity
/// carrying a [`Transform`] and a [`Visuals::border`]. A border paints
/// even when the entity has no background fill (CSS: `background: none;
/// border: 1px solid ...` still draws the border). Logical border edges
/// (`border-inline-*`) are resolved against the entity's
/// [`crate::components::ResolvedDirection`].
pub fn extract_borders(main: &mut World, render: &mut World) {
    use crate::components::ResolvedDirection;
    let (parents, mut depth_cache) = build_parent_map(main);
    let hidden = hidden_entities(main, &parents);
    let scroll = parent_scroll_offsets(main, &parents);
    let inherited_alpha = parent_opacities(main, &parents);
    let clip = parent_scroll_clip_rects(main, &parents);
    type Row<'a> = (
        Entity,
        &'a Transform,
        &'a Visuals,
        Option<&'a Opacity>,
        Option<&'a ResolvedDirection>,
    );
    let mut q = main.query::<Row>();
    let pairs: Vec<(Entity, ExtractedBorder)> = q
        .iter(main)
        .filter(|(e, _, _, _, _)| !hidden.contains(e))
        .filter_map(|(e, t, v, opacity, dir)| {
            let border = v.border.as_ref()?;
            let widths = border
                .widths
                .resolved(dir.map(|d| d.direction()).unwrap_or_default());
            if widths.top <= 0.0
                && widths.right <= 0.0
                && widths.bottom <= 0.0
                && widths.left <= 0.0
            {
                return None;
            }
            let alpha = effective_opacity(opacity, &inherited_alpha, e);
            let off = scroll.get(&e).copied().unwrap_or(Vec2::ZERO);
            let origin = t.absolute - off;
            if let Some(clip_rect) = clip.get(&e)
                && aabb_outside(origin, t.size, *clip_rect)
            {
                return None;
            }
            Some((
                e,
                ExtractedBorder {
                    origin,
                    size: t.size,
                    widths: [widths.top, widths.right, widths.bottom, widths.left],
                    color: alpha.apply(border.color),
                    side_colors: border.side_colors.map(|cs| cs.map(|c| alpha.apply(c))),
                    radius: v.radius,
                    corner_radii: v.corner_radii,
                    order: paint_order_of(e, &parents, &mut depth_cache),
                },
            ))
        })
        .collect();

    // Keyed-upsert against `RenderEntityMap.border` - same lifecycle as
    // `extract_rects`.
    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().border);
    let mut next: std::collections::HashMap<Entity, Entity> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (main_e, border) in pairs {
        let reuse = prior
            .get(&main_e)
            .copied()
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert(border);
                re
            }
            None => render.spawn(border).id(),
        };
        next.insert(main_e, render_e);
    }
    for (main_e, render_e) in &prior {
        if !next.contains_key(main_e)
            && let Ok(em) = render.get_entity_mut(*render_e)
        {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().border = next;
}

/// Returns the nearest clipping-ancestor rect (origin and size in window coordinates) for every entity.
///
/// - A clipping ancestor is one carrying a [`crate::input::Scroll`] component or a [`crate::components::Style`] with `overflow_x` or `overflow_y` set to [`crate::components::Overflow::Hidden`].
/// - Entities without a clipping ancestor are absent from the returned map.
pub fn parent_scroll_clip_rects(
    main: &mut World,
    parents: &std::collections::HashMap<Entity, Entity>,
) -> std::collections::HashMap<Entity, (Vec2, Vec2)> {
    use crate::components::{Overflow, Style, Transform};
    use crate::input::Scroll;
    if let Some(c) = main.get_resource::<ExtractContextCache>()
        && c.active
        && let Some(v) = &c.clip
    {
        return v.clone();
    }
    // Per-clipper rect (origin, size) in window coords.
    let clippers: std::collections::HashMap<Entity, (Vec2, Vec2)> = {
        let mut q = main.query::<(Entity, &Transform, &Style, Option<&Scroll>)>();
        q.iter(main)
            .filter_map(|(e, t, style, scroll)| {
                let qualifies = scroll.is_some()
                    || matches!(style.overflow_y, Overflow::Hidden)
                    || matches!(style.overflow_x, Overflow::Hidden);
                qualifies.then_some((e, (t.absolute, t.size)))
            })
            .collect()
    };
    if clippers.is_empty() {
        if let Some(mut c) = main.get_resource_mut::<ExtractContextCache>()
            && c.active
        {
            c.clip = Some(std::collections::HashMap::new());
        }
        return std::collections::HashMap::new();
    }
    // Top-layer roots escape ancestor clips (browser top-layer semantics): the upward walk stops at
    // an [`OverlayLayer`] entity, so popup content is neither culled nor clipped by a scroll /
    // overflow-hidden ancestor outside the popup. Clippers inside the popup subtree still apply.
    let overlay: std::collections::HashSet<Entity> = {
        let mut oq = main.query_filtered::<Entity, With<OverlayLayer>>();
        oq.iter(main).collect()
    };
    let mut out: std::collections::HashMap<Entity, (Vec2, Vec2)> = std::collections::HashMap::new();
    let mut cache: std::collections::HashMap<Entity, Option<(Vec2, Vec2)>> =
        std::collections::HashMap::new();
    fn resolve(
        e: Entity,
        parents: &std::collections::HashMap<Entity, Entity>,
        clippers: &std::collections::HashMap<Entity, (Vec2, Vec2)>,
        overlay: &std::collections::HashSet<Entity>,
        cache: &mut std::collections::HashMap<Entity, Option<(Vec2, Vec2)>>,
    ) -> Option<(Vec2, Vec2)> {
        if let Some(v) = cache.get(&e) {
            return *v;
        }
        let result = if overlay.contains(&e) {
            // Top-layer root: no clip ancestor applies past this boundary.
            None
        } else if let Some(p) = parents.get(&e).copied() {
            if let Some(rect) = clippers.get(&p) {
                Some(*rect)
            } else {
                resolve(p, parents, clippers, overlay, cache)
            }
        } else {
            None
        };
        cache.insert(e, result);
        result
    }
    for &e in parents.keys() {
        if let Some(rect) = resolve(e, parents, &clippers, &overlay, &mut cache) {
            out.insert(e, rect);
        }
    }
    if let Some(mut c) = main.get_resource_mut::<ExtractContextCache>()
        && c.active
    {
        c.clip = Some(out.clone());
    }
    out
}

/// Returns `true` when the rect at `(origin, size)` lies fully outside `clip = (corigin, csize)`;
/// the two AABBs share no area at all.
///
/// Partially-visible content must not be culled here: the clip layer emitted by
/// [`extract_clips`] (vello push/pop) trims the overflowing part at paint time. The previous
/// any-part-outside test made every child that overhangs its scroll container by even one
/// pixel - e.g. `width: 100%` plus a horizontal margin, or a row straddling the container's
/// bottom edge - vanish entirely (W6 T2, the invisible counter tiles).
fn aabb_outside(origin: Vec2, size: Vec2, clip: (Vec2, Vec2)) -> bool {
    let (co, cs) = clip;
    origin.x + size.x <= co.x
        || origin.y + size.y <= co.y
        || origin.x >= co.x + cs.x
        || origin.y >= co.y + cs.y
}

/// Extracts one [`ExtractedClipBox`] per clipping entity (carrying [`crate::input::Scroll`] or `Style.overflow_x` / `overflow_y == Hidden`).
/// Each emission carries the containing rect plus the `(start_order, end_order)` range bracketing descendant paints so the renderer can push/pop a vello layer.
///
/// W2.3 wires this back into the default extract chain - the boxes feed [`crate::node_ir::Node::Clip`]
/// wrappers inside [`crate::node_ir::transform_extracted_to_nodes`].
pub fn extract_clips(main: &mut World, render: &mut World) {
    use crate::components::{Overflow, Style, Transform};
    use crate::input::Scroll;
    let (parents, mut depth_cache) = build_parent_map(main);
    let hidden = hidden_entities(main, &parents);
    // Invert the parent map into a child lookup so descendant orders can be computed.
    let mut children: std::collections::HashMap<Entity, Vec<Entity>> =
        std::collections::HashMap::new();
    for (&e, &p) in parents.iter() {
        children.entry(p).or_default().push(e);
    }
    // Collect candidate entities (scroll or overflow-hidden).
    let mut candidates: Vec<Entity> = Vec::new();
    {
        let mut q = main.query::<(Entity, &Style, Option<&Scroll>)>();
        for (e, style, scroll) in q.iter(main) {
            if hidden.contains(&e) {
                continue;
            }
            let clip_y = matches!(style.overflow_y, Overflow::Hidden) || scroll.is_some();
            let clip_x = matches!(style.overflow_x, Overflow::Hidden) || scroll.is_some();
            if clip_x || clip_y {
                candidates.push(e);
            }
        }
    }
    if candidates.is_empty() {
        // No candidates this frame; drop any leftover clip render entities.
        let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().clip);
        for re in prior.values() {
            if let Ok(em) = render.get_entity_mut(*re) {
                em.despawn();
            }
        }
        return;
    }
    // Materialise a `Entity -> Transform` lookup so the DFS below need not re-query the world.
    let transforms: std::collections::HashMap<Entity, Transform> = {
        let mut q = main.query::<(Entity, &Transform)>();
        q.iter(main).map(|(e, t)| (e, *t)).collect()
    };
    // Top-layer roots: their subtrees live in the overlay band and must not extend an outer clip's
    // bracket (the popup escapes ancestor clips; its own internal clips are separate candidates).
    let overlay: std::collections::HashSet<Entity> = {
        let mut oq = main.query_filtered::<Entity, With<OverlayLayer>>();
        oq.iter(main).collect()
    };
    // Compute the maximum [`PaintOrder`] across `root` and its descendants via DFS over `children`,
    // not descending into nested [`OverlayLayer`] roots (their ranks sit in the top-layer band and
    // would wrongly stretch the bracket across all content between).
    fn max_desc_order(
        root: Entity,
        children: &std::collections::HashMap<Entity, Vec<Entity>>,
        parents: &std::collections::HashMap<Entity, Entity>,
        overlay: &std::collections::HashSet<Entity>,
        depth_cache: &mut std::collections::HashMap<Entity, u32>,
    ) -> PaintOrder {
        let mut stack = vec![root];
        let mut best = paint_order_of(root, parents, depth_cache);
        while let Some(n) = stack.pop() {
            let order = paint_order_of(n, parents, depth_cache);
            if order > best {
                best = order;
            }
            if let Some(kids) = children.get(&n) {
                for &k in kids {
                    if !overlay.contains(&k) {
                        stack.push(k);
                    }
                }
            }
        }
        best
    }
    let pairs: Vec<(Entity, ExtractedClipBox)> = candidates
        .into_iter()
        .filter_map(|e| {
            let t = transforms.get(&e).copied()?;
            let own = paint_order_of(e, &parents, &mut depth_cache);
            let end = max_desc_order(e, &children, &parents, &overlay, &mut depth_cache);
            let radius = main
                .get::<crate::components::Visuals>(e)
                .map(|v| v.radius)
                .unwrap_or(0.0);
            Some((
                e,
                ExtractedClipBox {
                    origin: t.absolute,
                    size: t.size,
                    radius,
                    start_order: own,
                    end_order: end,
                },
            ))
        })
        .collect();
    // Keyed-upsert against `RenderEntityMap.clip`.
    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().clip);
    let mut next: std::collections::HashMap<Entity, Entity> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (main_e, clip) in pairs {
        let reuse = prior
            .get(&main_e)
            .copied()
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert(clip);
                re
            }
            None => render.spawn(clip).id(),
        };
        next.insert(main_e, render_e);
    }
    for (main_e, render_e) in &prior {
        if !next.contains_key(main_e)
            && let Ok(em) = render.get_entity_mut(*render_e)
        {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().clip = next;
}

/// Returns `(parent_map, document_order_map)` used by every extract fn to compute [`PaintOrder`] consistently.
///
/// - `parent_map`: `child -> parent` for every entity carrying [`bevy_ecs::hierarchy::ChildOf`].
/// - `document_order_map`: entity -> [`PaintOrder`], assigned by a pre-order DFS over the hierarchy
///   forest. Parents rank before their children; siblings rank in `Children`-list order (bevy keeps the
///   list in insertion order, which matches markup order for static spawns and reconcile order for
///   runtime `<if>` / `<for>` clones). Entity-id allocation order plays no part, so entities spawned out
///   of document order (children before parents, reconciler respawns) still paint in tree order and
///   [`ExtractedClipBox`] ranges bracket exactly the descendant set.
///
/// Ranks are multiplied by 2 (see [`PaintOrder`]) so [`extract_shadows`] can place shadows at
/// `order - 1` without colliding with the preceding leaf. Consume the maps via [`paint_order_of`].
///
/// Subtrees rooted at an [`OverlayLayer`] entity are excluded from the normal-band DFS and
/// re-ranked into the top-layer band (`>= OVERLAY_ORDER_BASE`), stacked among themselves by
/// [`OverlayOpenOrder`] stamp (later-opened on top), each keeping contiguous internal pre-order
/// ranks. Idempotent within a frame: repeated calls (one per extract fn) see the same visibility
/// state and hand out the same ranks.
pub fn build_parent_map(
    main: &mut World,
) -> (
    std::collections::HashMap<Entity, Entity>,
    std::collections::HashMap<Entity, u32>,
) {
    // Extract-phase reuse: the first extractor of a frame builds this,
    // the rest clone it back (see [`ExtractContextCache`]).
    if let Some(c) = main.get_resource::<ExtractContextCache>()
        && c.active
        && let Some(pm) = &c.parent_map
    {
        return pm.clone();
    }
    use bevy_ecs::hierarchy::{ChildOf, Children};
    let mut pq = main.query::<(Entity, &ChildOf)>();
    let parents: std::collections::HashMap<Entity, Entity> =
        pq.iter(main).map(|(e, p)| (e, p.parent())).collect();
    // CSS `z-index`: paint-order override among siblings. Entities
    // without the component are `auto` (= 0, document order).
    let z_of: std::collections::HashMap<Entity, i32> = {
        let mut zq = main.query::<(Entity, &crate::components::ZIndex)>();
        zq.iter(main).map(|(e, z)| (e, z.0)).collect()
    };
    // Ordered child lists straight from the bevy-maintained `Children` relationship target.
    // Each list is stable-sorted by `(z_index, document order)` so a
    // higher-z sibling (with its whole subtree) receives later pre-order
    // ranks and paints on top - CSS stacking within one parent context.
    let children: std::collections::HashMap<Entity, Vec<Entity>> = {
        let mut cq = main.query::<(Entity, &Children)>();
        cq.iter(main)
            .map(|(e, kids)| {
                let mut list: Vec<Entity> = kids.iter().collect();
                if !z_of.is_empty() {
                    list.sort_by_key(|c| z_of.get(c).copied().unwrap_or(0));
                }
                (e, list)
            })
            .collect()
    };
    // Top-layer roots: their subtrees are skipped by the normal DFS and re-banded below.
    let overlay_roots: Vec<Entity> = {
        let mut oq = main.query_filtered::<Entity, With<OverlayLayer>>();
        oq.iter(main).collect()
    };
    let overlay_set: std::collections::HashSet<Entity> = overlay_roots.iter().copied().collect();
    // Forest roots: entities that have children but no parent. Sorted by entity bits for determinism
    // (real apps have a single markup root; extra roots only occur in tests / embedder worlds).
    let mut roots: Vec<Entity> = children
        .keys()
        .copied()
        .filter(|e| !parents.contains_key(e))
        .collect();
    roots.sort_by_key(|e| e.to_bits());
    // Pre-order DFS assigning stride-2 document-order ranks. Overlay roots are neither ranked nor
    // descended into here - their subtrees land in the top-layer band instead.
    let mut order: std::collections::HashMap<Entity, u32> =
        std::collections::HashMap::with_capacity(parents.len() + roots.len());
    let mut stack: Vec<Entity> = roots
        .into_iter()
        .rev()
        .filter(|e| !overlay_set.contains(e))
        .collect();
    let mut rank: u32 = 0;
    while let Some(e) = stack.pop() {
        order.insert(e, rank.saturating_mul(2));
        rank = rank.saturating_add(1);
        if let Some(kids) = children.get(&e) {
            stack.extend(
                kids.iter()
                    .rev()
                    .copied()
                    .filter(|k| !overlay_set.contains(k)),
            );
        }
    }
    if !overlay_roots.is_empty() {
        rank_overlay_band(
            main,
            &parents,
            &children,
            &overlay_roots,
            &overlay_set,
            &mut order,
        );
    }
    let result = (parents, order);
    if let Some(mut c) = main.get_resource_mut::<ExtractContextCache>()
        && c.active
    {
        c.parent_map = Some(result.clone());
    }
    result
}

/// Returns each entity's cumulative ANCESTOR opacity product (own
/// [`Opacity`] excluded - extract fns already fold that in). CSS
/// semantics: `opacity` multiplies down the subtree, so fading a dialog
/// root fades every descendant. Returns an empty map when no entity
/// carries an [`Opacity`] (the overwhelmingly common case - extracts
/// then skip the lookup entirely).
pub fn parent_opacities(
    main: &mut World,
    parents: &HashMap<Entity, Entity>,
) -> HashMap<Entity, f32> {
    if let Some(c) = main.get_resource::<ExtractContextCache>()
        && c.active
        && let Some(v) = &c.opacities
    {
        return v.clone();
    }
    let direct: HashMap<Entity, f32> = {
        let mut q = main.query::<(Entity, &Opacity)>();
        q.iter(main).map(|(e, o)| (e, o.0)).collect()
    };
    let by_entity: HashMap<Entity, f32> = if direct.is_empty() {
        HashMap::new()
    } else {
        fn cumulative(
            e: Entity,
            parents: &HashMap<Entity, Entity>,
            direct: &HashMap<Entity, f32>,
            cache: &mut HashMap<Entity, f32>,
        ) -> f32 {
            if let Some(v) = cache.get(&e) {
                return *v;
            }
            let parent_alpha = parents
                .get(&e)
                .map(|p| cumulative(*p, parents, direct, cache))
                .unwrap_or(1.0);
            let own = direct.get(&e).copied().unwrap_or(1.0);
            let total = parent_alpha * own;
            cache.insert(e, total);
            total
        }
        let mut cache: HashMap<Entity, f32> = HashMap::new();
        let mut by_entity: HashMap<Entity, f32> = HashMap::new();
        for &e in parents.keys() {
            if let Some(p) = parents.get(&e) {
                let alpha = cumulative(*p, parents, &direct, &mut cache);
                if alpha < 1.0 {
                    by_entity.insert(e, alpha);
                }
            }
        }
        by_entity
    };
    if let Some(mut c) = main.get_resource_mut::<ExtractContextCache>()
        && c.active
    {
        c.opacities = Some(by_entity.clone());
    }
    by_entity
}

/// Combine an entity's own [`Opacity`] with its inherited ancestor
/// product from [`parent_opacities`].
pub(crate) fn effective_opacity(
    own: Option<&Opacity>,
    inherited: &HashMap<Entity, f32>,
    e: Entity,
) -> Opacity {
    let own = own.copied().unwrap_or_default().0;
    let anc = inherited.get(&e).copied().unwrap_or(1.0);
    Opacity(own * anc)
}

/// Ranks every [`OverlayLayer`] subtree into the top-layer band (`>= OVERLAY_ORDER_BASE`).
///
/// - Visible overlay roots are stamped via [`OverlayOpenOrder`] (first sighting = lowest stamp) and
///   ranked in stamp order, so a later-opened popup paints over an earlier one.
/// - Hidden overlay roots lose their stamp (re-opening restamps on top) and are ranked after all
///   stamped roots, by entity bits - deterministic, though never painted while hidden.
/// - Each subtree gets contiguous pre-order stride-2 ranks; nested overlay roots are skipped and
///   ranked by their own stamp (a submenu opened after its parent menu lands above it).
fn rank_overlay_band(
    main: &mut World,
    parents: &std::collections::HashMap<Entity, Entity>,
    children: &std::collections::HashMap<Entity, Vec<Entity>>,
    overlay_roots: &[Entity],
    overlay_set: &std::collections::HashSet<Entity>,
    order: &mut std::collections::HashMap<Entity, u32>,
) {
    let hidden = hidden_entities(main, parents);
    if main.get_resource::<OverlayOpenOrder>().is_none() {
        main.insert_resource(OverlayOpenOrder::default());
    }
    let ordered_roots: Vec<Entity> = {
        let mut oo = main.resource_mut::<OverlayOpenOrder>();
        let oo = &mut *oo;
        // Drop stamps for despawned or hidden roots so the next open lands on top.
        oo.stamps
            .retain(|e, _| overlay_set.contains(e) && !hidden.contains(e));
        // Stamp newly visible roots. Entity-bits order breaks the tie when several open on the
        // same tick (deterministic; simultaneous opens have no meaningful "later").
        let mut newly_open: Vec<Entity> = overlay_roots
            .iter()
            .copied()
            .filter(|e| !hidden.contains(e) && !oo.stamps.contains_key(e))
            .collect();
        newly_open.sort_by_key(|e| e.to_bits());
        for e in newly_open {
            let s = oo.next;
            oo.next += 1;
            oo.stamps.insert(e, s);
        }
        let mut stamped: Vec<(u64, Entity)> = oo.stamps.iter().map(|(&e, &s)| (s, e)).collect();
        stamped.sort_unstable_by_key(|&(s, e)| (s, e.to_bits()));
        let mut list: Vec<Entity> = stamped.into_iter().map(|(_, e)| e).collect();
        let mut closed: Vec<Entity> = overlay_roots
            .iter()
            .copied()
            .filter(|e| hidden.contains(e))
            .collect();
        closed.sort_by_key(|e| e.to_bits());
        list.extend(closed);
        list
    };
    // Contiguous pre-order ranks continuing across subtrees, starting at the band base.
    let mut orank: u32 = OVERLAY_ORDER_BASE / 2;
    for root in ordered_roots {
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            order.insert(e, orank.saturating_mul(2));
            orank = orank.saturating_add(1);
            if let Some(kids) = children.get(&e) {
                stack.extend(
                    kids.iter()
                        .rev()
                        .copied()
                        .filter(|k| !overlay_set.contains(k)),
                );
            }
        }
    }
}

/// Returns each entity's cumulative ancestor-chain [`ScrollOffset`].
///
/// - The entity's own `ScrollOffset` is excluded; only its descendants translate.
/// - Returns an empty map when no [`ScrollOffset`] components are present.
/// - Consumed by [`extract_rects`], [`extract_text`], and [`extract_shadows`] to subtract the offset from rendered origins.
pub fn parent_scroll_offsets(
    main: &mut World,
    parents: &HashMap<Entity, Entity>,
) -> HashMap<Entity, Vec2> {
    if let Some(c) = main.get_resource::<ExtractContextCache>()
        && c.active
        && let Some(v) = &c.scroll
    {
        return v.clone();
    }
    let direct: HashMap<Entity, Vec2> = {
        let mut q = main.query::<(Entity, &ScrollOffset)>();
        q.iter(main).map(|(e, o)| (e, o.0)).collect()
    };
    let by_entity: HashMap<Entity, Vec2> = if direct.is_empty() {
        HashMap::new()
    } else {
        fn cumulative(
            e: Entity,
            parents: &HashMap<Entity, Entity>,
            direct: &HashMap<Entity, Vec2>,
            cache: &mut HashMap<Entity, Vec2>,
        ) -> Vec2 {
            if let Some(v) = cache.get(&e) {
                return *v;
            }
            let parent_off = parents
                .get(&e)
                .map(|p| cumulative(*p, parents, direct, cache))
                .unwrap_or(Vec2::ZERO);
            let own = direct.get(&e).copied().unwrap_or(Vec2::ZERO);
            let total = parent_off + own;
            cache.insert(e, total);
            total
        }
        let mut cache: HashMap<Entity, Vec2> = HashMap::new();
        let mut by_entity: HashMap<Entity, Vec2> = HashMap::new();
        for &e in parents.keys() {
            if let Some(p) = parents.get(&e) {
                let off = cumulative(*p, parents, &direct, &mut cache);
                if off != Vec2::ZERO {
                    by_entity.insert(e, off);
                }
            }
        }
        by_entity
    };
    if let Some(mut c) = main.get_resource_mut::<ExtractContextCache>()
        && c.active
    {
        c.scroll = Some(by_entity.clone());
    }
    by_entity
}

/// Returns the set of entities hidden by a [`Visible(false)`] on themselves or any ancestor.
///
/// - Used by extract fns to skip subtrees of `<if mode="hide">` blocks without despawning.
/// - First pass collects every `Visible(false)`; second pass walks each `parents` chain upward, memoising the answer per entity.
pub fn hidden_entities(
    main: &mut World,
    parents: &std::collections::HashMap<Entity, Entity>,
) -> std::collections::HashSet<Entity> {
    if let Some(c) = main.get_resource::<ExtractContextCache>()
        && c.active
        && let Some(v) = &c.hidden
    {
        return v.clone();
    }
    let mut hide_roots = std::collections::HashSet::new();
    let mut q = main.query::<(Entity, &Visible)>();
    for (e, v) in q.iter(main) {
        if !v.0 {
            hide_roots.insert(e);
        }
    }
    let hidden: std::collections::HashSet<Entity> = if hide_roots.is_empty() {
        std::collections::HashSet::new()
    } else {
        let mut cache: std::collections::HashMap<Entity, bool> = std::collections::HashMap::new();
        let mut hidden = std::collections::HashSet::new();
        for &entity in parents.keys().chain(hide_roots.iter()) {
            if is_hidden_walk(entity, &hide_roots, parents, &mut cache) {
                hidden.insert(entity);
            }
        }
        hidden
    };
    if let Some(mut c) = main.get_resource_mut::<ExtractContextCache>()
        && c.active
    {
        c.hidden = Some(hidden.clone());
    }
    hidden
}

fn is_hidden_walk(
    e: Entity,
    roots: &std::collections::HashSet<Entity>,
    parents: &std::collections::HashMap<Entity, Entity>,
    cache: &mut std::collections::HashMap<Entity, bool>,
) -> bool {
    if let Some(v) = cache.get(&e) {
        return *v;
    }
    let v = if roots.contains(&e) {
        true
    } else {
        match parents.get(&e) {
            Some(p) => is_hidden_walk(*p, roots, parents, cache),
            None => false,
        }
    };
    cache.insert(e, v);
    v
}

/// Returns the [`PaintOrder`] for `e` - the document-order rank precomputed by [`build_parent_map`]
/// (whose second return value is the map passed here as `cache`).
///
/// Entities absent from the map (standalone drawables with no hierarchy links) fall back to
/// `0x8000_0000 | (entity_index << 1)`: they paint after all tree content in entity-allocation order,
/// and - because the high bit clears every tree rank - no [`ExtractedClipBox`] descendant range can
/// accidentally absorb them.
pub fn paint_order_of(
    e: Entity,
    _parents: &std::collections::HashMap<Entity, Entity>,
    cache: &mut std::collections::HashMap<Entity, u32>,
) -> PaintOrder {
    if let Some(o) = cache.get(&e) {
        return *o;
    }
    let idx = (e.to_bits() as u32) & 0x3FFF_FFFF;
    let o = 0x8000_0000 | (idx << 1);
    cache.insert(e, o);
    o
}

/// One CPU-side snapshot of the on-screen window surface produced by a GPU->CPU readback.
#[derive(Clone, Debug)]
pub struct SurfaceFrame {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Tightly-packed RGBA8 pixels, top-to-bottom, sRGB-encoded (no pre-multiplied alpha).
    pub rgba8: Vec<u8>,
}

/// Coordination handle for on-screen surface screenshots, inserted as a `Resource` into both worlds.
///
/// - The render backend inspects [`Self::is_requested`] each frame; when set, it performs a GPU->CPU readback, writes the result via [`Self::write`], and clears the flag.
/// - Both fields are `Arc`-wrapped (`Send + Sync`) so MCP-server worker threads can clone the handle and read filled frames.
#[derive(Resource, Clone, Default)]
pub struct SurfaceCapture {
    /// One-shot capture request flag; the renderer clears it after writing.
    pub request: Arc<AtomicBool>,
    /// Latest captured frame; the renderer replaces it wholesale.
    pub store: Arc<Mutex<Option<SurfaceFrame>>>,
    /// Optional handle to interrupt a parked platform event loop. The
    /// windowed backend wires this in [`crate::app::EventLoopWaker`] via
    /// [`Self::set_waker`] once its event-loop proxy exists; [`Self::request`]
    /// then nudges the loop so a screenshot request from the off-thread MCP
    /// server is serviced promptly instead of waiting for an unrelated OS
    /// event (the redraw scheduler otherwise leaves the loop parked, so the
    /// capture never runs and the request times out - the "no SurfaceCapture
    /// wired" failure). Shared `Arc<OnceLock>` so every clone - including the
    /// server thread's - observes a waker set on any one of them.
    pub waker: Arc<std::sync::OnceLock<crate::app::EventLoopWaker>>,
}

impl SurfaceCapture {
    /// Returns `true` while a capture has been requested but not yet fulfilled.
    pub fn is_requested(&self) -> bool {
        self.request.load(Ordering::Acquire)
    }

    /// Installs the platform event-loop waker. First write wins
    /// (`OnceLock`); later calls are ignored. Idempotent and thread-safe.
    pub fn set_waker(&self, waker: crate::app::EventLoopWaker) {
        let _ = self.waker.set(waker);
    }

    /// Sets the request flag with `Release` ordering, then nudges the
    /// platform event loop (if [`Self::set_waker`] wired one) so a parked
    /// windowed backend wakes to service the readback this frame instead of
    /// sitting idle until the request times out. Idempotent.
    pub fn request(&self) {
        self.request.store(true, Ordering::Release);
        if let Some(waker) = self.waker.get() {
            waker.wake();
        }
    }

    /// Clears the request flag with `Release` ordering.
    pub fn clear_request(&self) {
        self.request.store(false, Ordering::Release);
    }

    /// Replaces the stored frame with `frame`.
    pub fn write(&self, frame: SurfaceFrame) {
        if let Ok(mut slot) = self.store.lock() {
            *slot = Some(frame);
        }
    }

    /// Returns a clone of the latest stored frame, if any.
    pub fn read(&self) -> Option<SurfaceFrame> {
        self.store.lock().ok().and_then(|g| g.clone())
    }
}

/// Byte offset into a masked run that corresponds to `plain_byte` in the
/// plaintext `text`, where each scalar renders as one `mask` char.
/// Snaps `plain_byte` down to a char boundary first, then counts scalars
/// before it and scales by the mask char's UTF-8 width.
fn masked_offset(text: &str, plain_byte: usize, mask: char) -> usize {
    let mut b = plain_byte.min(text.len());
    while b > 0 && !text.is_char_boundary(b) {
        b -= 1;
    }
    text[..b].chars().count() * mask.len_utf8()
}

/// Rewrite a display run and its caret / selection byte offsets for a
/// concealed [`EchoMode`]:
/// - [`EchoMode::Password`] -> one `mask` glyph per scalar ([`PASSWORD_MASK_CHAR`]
///   unless a [`PasswordCharacter`] override is present), with caret /
///   selection remapped into the masked string.
/// - [`EchoMode::NoEcho`] -> empty run; caret collapses to the origin and
///   no selection is painted (there is nothing to highlight).
///
/// [`EchoMode::Normal`] is handled by the caller and never reaches here.
fn mask_echo(
    mode: EchoMode,
    text: &str,
    caret: Option<usize>,
    selection: Option<(usize, usize)>,
    mask: char,
) -> (String, Option<usize>, Option<(usize, usize)>) {
    match mode {
        EchoMode::NoEcho => (String::new(), caret.map(|_| 0), None),
        EchoMode::Password => {
            let scalars = text.chars().count();
            let masked: String = mask.to_string().repeat(scalars);
            let caret = caret.map(|c| masked_offset(text, c, mask));
            let selection = selection
                .map(|(s, e)| (masked_offset(text, s, mask), masked_offset(text, e, mask)));
            (masked, caret, selection)
        }
        EchoMode::Normal => (text.to_string(), caret, selection),
    }
}

/// Default extract fn that emits one [`ExtractedText`] per entity with [`Transform`] and [`TextContent`].
///
/// - When an [`ImeState`] is present, its `preedit` is concatenated onto the committed text.
/// - For focused `<input>` entities, caret offset and selection range are propagated.
/// - Under a concealed [`EchoMode`] the display glyphs, caret, and selection are masked (see [`mask_echo`]); the buffer plaintext is untouched.
/// - Hidden subtrees and entities clipped fully outside their scroll/overflow ancestor are skipped.
pub fn extract_text(main: &mut World, render: &mut World) {
    use crate::components::Style;
    let (parents, mut depth_cache) = build_parent_map(main);
    let hidden = hidden_entities(main, &parents);
    let scroll = parent_scroll_offsets(main, &parents);
    let inherited_alpha = parent_opacities(main, &parents);
    let clip = parent_scroll_clip_rects(main, &parents);
    // Caret blink gate: when the blink resource says "hidden half of the
    // phase", withhold the caret byte so the renderer paints no bar.
    // Absent resource (headless / embedder without the blink system) =>
    // always visible.
    let caret_visible = main
        .get_resource::<CaretBlink>()
        .map(|b| b.visible)
        .unwrap_or(true);
    type RowFor<'a> = (
        Entity,
        &'a Transform,
        &'a TextContent,
        Option<&'a TextStyle>,
        Option<&'a ImeState>,
        Option<&'a TextInput>,
        Option<&'a Focused>,
        Option<&'a Style>,
        Option<&'a Opacity>,
        Option<&'a TextInputScroll>,
        Option<&'a EchoMode>,
        Option<&'a TextInputPaint>,
        Option<&'a TextBlockOrigin>,
        Option<&'a CaretWidth>,
        Option<&'a PasswordCharacter>,
    );
    let mut q = main.query::<RowFor>();
    let pairs: Vec<(Entity, ExtractedText)> = q
        .iter(main)
        .filter(|(e, ..)| !hidden.contains(e))
        .filter_map(
            |(
                e,
                t,
                text,
                ts,
                ime,
                input,
                focused,
                style,
                opacity,
                edit_scroll,
                echo,
                paint,
                block_origin,
                caret_width,
                password_char,
            )| {
                let ts = ts.cloned().unwrap_or_default();
                let size_px = ts.size_px;
                let line_height_px = resolve_line_height(ts.line_height, size_px);
                let caret_width_px = caret_width.map(|w| w.0).unwrap_or(CARET_WIDTH_PX);
                let mask_char = password_char.map(|c| c.0).unwrap_or(PASSWORD_MASK_CHAR);
                let preedit = ime.map(|i| i.preedit.as_str()).unwrap_or("");
                // Show the placeholder while the input holds neither committed
                // text nor preedit - focused or not (Qt shows the hint under a
                // blinking caret until the first keystroke).
                let placeholder = match input {
                    Some(i) if text.0.is_empty() && preedit.is_empty() => i.placeholder.as_str(),
                    _ => "",
                };
                if input.is_none() && text.0.is_empty() && preedit.is_empty() {
                    return None;
                }
                let caret = match (input, focused) {
                    (Some(i), Some(_)) if caret_visible => {
                        // `caret = TextInput.cursor (clamped) + preedit.len()` so the bar trails the composition buffer.
                        let base = i.cursor.min(text.0.len());
                        Some(base + preedit.len())
                    }
                    _ => None,
                };
                // While the placeholder is showing, the buffer is empty - pin
                // the caret to offset 0 so it doesn't index into hint text.
                let caret = caret.map(|c| if placeholder.is_empty() { c } else { 0 });
                let combined = if !placeholder.is_empty() {
                    placeholder.to_string()
                } else {
                    format!("{}{}", text.0, preedit)
                };
                // Emit a selection range only when the input is focused and the anchor differs from the cursor.
                let selection = match (input, focused) {
                    (Some(i), Some(_)) => i.selection_anchor.and_then(|a| {
                        let cur = i.cursor.min(text.0.len());
                        let a = a.min(text.0.len());
                        if a == cur {
                            None
                        } else {
                            Some((a.min(cur), a.max(cur)))
                        }
                    }),
                    _ => None,
                };
                // Password / no-echo masking (Qt `QLineEdit::EchoMode`).
                // The plaintext never leaves the buffer - only the display
                // run, caret, and selection offsets are rewritten against
                // the masked glyphs. Placeholder hint text is never masked
                // (it is not the secret). Char-based (one mask per Unicode
                // scalar) so `lumen-core` stays dependency-free; matches
                // Qt, whose password display is also per-code-unit.
                let (combined, caret, selection) = match echo {
                    Some(mode) if mode.is_concealed() && placeholder.is_empty() => {
                        mask_echo(*mode, &combined, caret, selection, mask_char)
                    }
                    _ => (combined, caret, selection),
                };
                // Vertical origin. `TextBlockOrigin` carries the producer's
                // soft-wrap-aware answer; without it, fall back to the same
                // rule over the logical line count so the drawn baseline
                // still agrees with the pointer hit test.
                let pad_left = style.map(|s| s.padding.left).unwrap_or(0.0);
                let pad_right = style.map(|s| s.padding.right).unwrap_or(0.0);
                let pad_top = style.map(|s| s.padding.top).unwrap_or(0.0);
                let pad_bottom = style.map(|s| s.padding.bottom).unwrap_or(0.0);
                let inner_h = (t.size.y - pad_top - pad_bottom).max(size_px);
                let block_top = block_origin.map(|b| b.top).unwrap_or_else(|| {
                    let stacked = input.is_some_and(|i| i.multiline) || combined.contains('\n');
                    text_block_top(inner_h, line_height_px, stacked)
                });
                let baseline_y = t.absolute.y
                    + pad_top
                    + block_top
                    + text_baseline_in_line(size_px, line_height_px);
                let container_width = (t.size.x - pad_left - pad_right).max(0.0);
                let alpha = effective_opacity(opacity, &inherited_alpha, e);
                let off = scroll.get(&e).copied().unwrap_or(Vec2::ZERO);
                // AABB-cull against the nearest scroll / overflow-hidden ancestor; matches the rule applied in `extract_rects`.
                if let Some(clip_rect) = clip.get(&e) {
                    let probe_origin = Vec2::new(t.absolute.x, t.absolute.y) - off;
                    if aabb_outside(probe_origin, t.size, *clip_rect) {
                        return None;
                    }
                }
                // Per-input caret-keep-visible offset: shift the whole run
                // (glyphs, caret, selection move together since the renderer
                // derives caret / selection x from the shifted origin).
                let edit_off = edit_scroll.map(|s| s.offset).unwrap_or(Vec2::ZERO);
                Some((
                    e,
                    ExtractedText {
                        origin: Vec2::new(t.absolute.x + pad_left, baseline_y) - off - edit_off,
                        text: combined,
                        size_px,
                        fill: alpha.apply(ts.color),
                        caret,
                        selection,
                        selection_color: ts.selection_color.map(|c| alpha.apply(c)),
                        selection_foreground: paint
                            .and_then(|p| p.selection_foreground)
                            .map(|c| alpha.apply(c)),
                        caret_color: paint.and_then(|p| p.caret_color).map(|c| alpha.apply(c)),
                        container_width,
                        align: ts.align,
                        wrap: ts.wrap,
                        max_lines: ts.max_lines,
                        family: ts.family.clone(),
                        weight: ts.weight,
                        line_height_px,
                        caret_width_px,
                        order: paint_order_of(e, &parents, &mut depth_cache),
                    },
                ))
            },
        )
        .collect();
    // Keyed-upsert against `RenderEntityMap.text`; reused render entities are validated to drop recycled ids.
    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().text);
    let mut next: std::collections::HashMap<Entity, Entity> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (main_e, et) in pairs {
        let reuse = prior
            .get(&main_e)
            .copied()
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert(et);
                re
            }
            None => render.spawn(et).id(),
        };
        next.insert(main_e, render_e);
    }
    for (main_e, render_e) in &prior {
        if !next.contains_key(main_e)
            && let Ok(em) = render.get_entity_mut(*render_e)
        {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().text = next;
}

#[cfg(test)]
mod echo_mask_tests {
    //! `EchoMode` display masking (Qt `QLineEdit::EchoMode`). The buffer
    //! plaintext is untouched; only the display run + caret / selection
    //! offsets are rewritten against the mask glyphs.
    use super::*;

    #[test]
    fn password_masks_each_scalar_and_remaps_caret() {
        // "abc" caret after 'b' (byte 2). Masked = three [`PASSWORD_MASK_CHAR`]
        // bullets, 3 bytes each, so the caret lands after the second -> byte 6.
        let (disp, caret, sel) =
            mask_echo(EchoMode::Password, "abc", Some(2), None, PASSWORD_MASK_CHAR);
        assert_eq!(disp.chars().count(), 3);
        assert!(disp.chars().all(|c| c == PASSWORD_MASK_CHAR));
        assert_eq!(caret, Some(6));
        assert_eq!(sel, None);
    }

    #[test]
    fn password_remaps_selection_range() {
        // Select "bc" (bytes 1..3) in "abc" -> masked bytes 3..9.
        let (_disp, _caret, sel) = mask_echo(
            EchoMode::Password,
            "abc",
            None,
            Some((1, 3)),
            PASSWORD_MASK_CHAR,
        );
        assert_eq!(sel, Some((3, 9)));
    }

    #[test]
    fn password_scalar_count_not_byte_count() {
        // "\u{e9}" is one scalar (2 bytes) -> exactly one mask glyph, and a
        // caret at end (byte 2) maps to one mask width (3 bytes).
        let (disp, caret, _) = mask_echo(
            EchoMode::Password,
            "\u{e9}",
            Some(2),
            None,
            PASSWORD_MASK_CHAR,
        );
        assert_eq!(disp.chars().count(), 1);
        assert_eq!(caret, Some(3));
    }

    /// A `password-character` override (here `*`, a 1-byte ASCII glyph vs
    /// the 3-byte default bullet) reaches `mask_echo` as a plain `char`
    /// parameter and both the display run and the remapped caret honour
    /// the override's own byte width, not the default's.
    #[test]
    fn password_character_override_changes_mask_glyph_and_width() {
        let (disp, caret, _) = mask_echo(EchoMode::Password, "abc", Some(2), None, '*');
        assert_eq!(disp, "***");
        assert_eq!(caret, Some(2), "1-byte mask -> caret byte == scalar count");
    }

    #[test]
    fn masked_offset_snaps_mid_codepoint_down() {
        // byte 1 is mid-'\u{e9}' -> snap to 0 -> zero mask widths.
        assert_eq!(EchoMode::Password.display_offset("\u{e9}", 1), 0);
    }

    #[test]
    fn masked_offset_round_trips_to_the_plaintext_byte() {
        // The pointer hit test resolves a masked byte and maps it back;
        // both directions must land on the same scalar edge.
        let plain = "a\u{e9}bc";
        for (i, _) in plain
            .char_indices()
            .chain(std::iter::once((plain.len(), 'x')))
        {
            let d = EchoMode::Password.display_offset(plain, i);
            assert_eq!(EchoMode::Password.plain_offset(plain, d), i);
        }
    }

    #[test]
    fn no_echo_hides_everything_and_collapses_caret() {
        let (disp, caret, sel) = mask_echo(
            EchoMode::NoEcho,
            "secret",
            Some(4),
            Some((0, 6)),
            PASSWORD_MASK_CHAR,
        );
        assert!(disp.is_empty());
        assert_eq!(caret, Some(0), "caret collapses to the origin");
        assert_eq!(sel, None, "nothing to highlight under no-echo");
    }

    /// End-to-end through the real `extract_text`: a focused password
    /// input emits a fully-masked run while the buffer keeps its
    /// plaintext, and the placeholder hint is never masked.
    #[test]
    fn extract_masks_password_run_but_not_placeholder() {
        use crate::components::{TextContent, TextInput, Transform};
        use crate::input::Focused;

        fn masked_text_for(content: &str, placeholder: &str) -> String {
            let mut main = World::new();
            let mut render = World::new();
            render.init_resource::<RenderEntityMap>();
            let e = main
                .spawn((
                    Transform::new(Vec2::ZERO, Vec2::new(120.0, 24.0)),
                    TextContent(content.to_string()),
                    TextInput {
                        placeholder: placeholder.to_string(),
                        cursor: content.len(),
                        ..Default::default()
                    },
                    EchoMode::Password,
                    Focused,
                ))
                .id();
            let _ = e;
            extract_text(&mut main, &mut render);
            let mut q = render.query::<&ExtractedText>();
            q.iter(&render).next().unwrap().text.clone()
        }

        // Non-empty buffer -> every scalar becomes a bullet.
        let masked = masked_text_for("hunter2", "");
        assert_eq!(masked.chars().count(), 7);
        assert!(masked.chars().all(|c| c == PASSWORD_MASK_CHAR));

        // Empty buffer -> the placeholder hint shows verbatim (a hint is
        // not the secret; Qt shows placeholder text under password mode).
        assert_eq!(masked_text_for("", "Password"), "Password");
    }

    /// A [`PasswordCharacter`] component (spawned from CSS
    /// `password-character`) overrides [`PASSWORD_MASK_CHAR`] end-to-end
    /// through `extract_text`; absent, the extract keeps using the
    /// built-in bullet.
    #[test]
    fn extract_password_character_override_replaces_default_mask() {
        use crate::components::{TextContent, TextInput, Transform};
        use crate::input::Focused;

        let mut main = World::new();
        let mut render = World::new();
        render.init_resource::<RenderEntityMap>();
        main.spawn((
            Transform::new(Vec2::ZERO, Vec2::new(120.0, 24.0)),
            TextContent("hunter2".to_string()),
            TextInput {
                cursor: 7,
                ..Default::default()
            },
            EchoMode::Password,
            PasswordCharacter('*'),
            Focused,
        ));
        extract_text(&mut main, &mut render);
        let mut q = render.query::<&ExtractedText>();
        let text = q.iter(&render).next().unwrap().text.clone();
        assert_eq!(text, "*******");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::hierarchy::ChildOf;

    /// A `Visible(false)` root (e.g. the dev-tools overlay, hidden until
    /// toggled) must suppress paint for its ENTIRE descendant subtree, not
    /// just the root entity. Regression for the hidden-overlay pixel leak:
    /// with the fix, an extract of a hidden root's subtree yields zero
    /// `ExtractedRect` / `ExtractedText`, matching the no-overlay scene.
    #[test]
    fn hidden_root_subtree_extracts_zero_paint_nodes() {
        use crate::components::{Fill, TextContent, Transform, Visible, Visuals};

        let mut main = World::new();
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());

        // Visible base scene: one filled rect.
        let base = main
            .spawn((
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(100.0, 100.0),
                    baseline_y: None,
                },
                Visuals {
                    fill: Some(Fill::Solid(Color::rgba(1.0, 0.0, 0.0, 1.0))),
                    ..Default::default()
                },
            ))
            .id();

        // Hidden second root + a subtree of rects and text under it.
        let overlay = main
            .spawn((
                Transform {
                    absolute: Vec2::ZERO,
                    size: Vec2::new(100.0, 100.0),
                    baseline_y: None,
                },
                Visuals {
                    fill: Some(Fill::Solid(Color::rgba(0.0, 1.0, 0.0, 1.0))),
                    ..Default::default()
                },
                Visible(false),
            ))
            .id();
        let panel = main
            .spawn((
                Transform {
                    absolute: Vec2::new(10.0, 10.0),
                    size: Vec2::new(50.0, 50.0),
                    baseline_y: None,
                },
                Visuals {
                    fill: Some(Fill::Solid(Color::rgba(0.0, 0.0, 1.0, 1.0))),
                    ..Default::default()
                },
                ChildOf(overlay),
            ))
            .id();
        let _label = main
            .spawn((
                Transform {
                    absolute: Vec2::new(12.0, 12.0),
                    size: Vec2::new(40.0, 16.0),
                    baseline_y: None,
                },
                TextContent("devtools".into()),
                ChildOf(panel),
            ))
            .id();

        extract_rects(&mut main, &mut render);
        extract_text(&mut main, &mut render);

        // Exactly one rect (the base), zero from the hidden subtree.
        let rects: Vec<(Entity, Vec2)> = {
            let mut q = render.query::<(Entity, &ExtractedRect)>();
            q.iter(&render).map(|(e, r)| (e, r.origin)).collect()
        };
        assert_eq!(
            rects.len(),
            1,
            "only the visible base rect extracts; the hidden subtree contributes none (got {rects:?})"
        );
        // The one rect must be the base, not the overlay/panel.
        let map = render.resource::<RenderEntityMap>();
        assert_eq!(map.rect.get(&base).copied(), Some(rects[0].0));
        assert!(!map.rect.contains_key(&overlay));
        assert!(!map.rect.contains_key(&panel));

        // Zero text: the label is inside the hidden subtree.
        let text_count = render.query::<&ExtractedText>().iter(&render).count();
        assert_eq!(text_count, 0, "hidden subtree text must not extract");
    }

    /// The `cull_hidden` guard is the safety net behind the per-extractor
    /// filters: even if some extractor leaks a render entity for a hidden
    /// main entity, the guard despawns it and prunes its `RenderEntityMap`
    /// slot before the retained tree is built.
    #[test]
    fn cull_hidden_despawns_leaked_render_entities() {
        use crate::components::{Transform, Visible};

        let mut main = World::new();
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());
        render.insert_resource(HiddenExtracts::default());

        let hidden_root = main.spawn((Transform::default(), Visible(false))).id();
        let child = main
            .spawn((Transform::default(), ChildOf(hidden_root)))
            .id();

        // Simulate an extractor that forgot the `hidden` filter: two render
        // entities keyed to the hidden main entities land in the map.
        let leaked_rect = render
            .spawn(ExtractedRect {
                origin: Vec2::ZERO,
                size: Vec2::new(10.0, 10.0),
                brush: Brush::Solid(Color::rgba(0.0, 1.0, 0.0, 1.0)),
                radius: 0.0,
                corner_radii: None,
                order: 0,
            })
            .id();
        let leaked_img = render
            .spawn(ExtractedRect {
                origin: Vec2::ZERO,
                size: Vec2::new(10.0, 10.0),
                brush: Brush::Solid(Color::rgba(0.0, 0.0, 1.0, 1.0)),
                radius: 0.0,
                corner_radii: None,
                order: 0,
            })
            .id();
        {
            let mut map = render.resource_mut::<RenderEntityMap>();
            map.rect.insert(hidden_root, leaked_rect);
            map.image.insert(child, leaked_img);
        }

        // Refresh the hidden snapshot as the extract phase would, then cull.
        stash_hidden_entities(&mut main, &mut render);
        let mut schedule = Schedule::default();
        schedule.add_systems(cull_hidden);
        schedule.run(&mut render);

        assert!(
            render.get_entity(leaked_rect).is_err(),
            "leaked rect for the hidden root must be despawned"
        );
        assert!(
            render.get_entity(leaked_img).is_err(),
            "leaked image for the hidden child must be despawned"
        );
        let map = render.resource::<RenderEntityMap>();
        assert!(map.rect.is_empty(), "hidden slot pruned from rect map");
        assert!(map.image.is_empty(), "hidden slot pruned from image map");
    }

    /// RC2 regression: paint order must follow document/tree order, not entity-id allocation order.
    /// Children are spawned BEFORE their parent so entity ids run opposite to document order.
    #[test]
    fn paint_order_follows_document_order_not_entity_ids() {
        let mut world = World::new();
        let child_a = world.spawn_empty().id();
        let child_b = world.spawn_empty().id();
        let grandchild = world.spawn_empty().id();
        let parent = world.spawn_empty().id();
        // Attach in document order: parent -> [child_a -> [grandchild], child_b].
        world.entity_mut(child_a).insert(ChildOf(parent));
        world.entity_mut(child_b).insert(ChildOf(parent));
        world.entity_mut(grandchild).insert(ChildOf(child_a));

        let (parents, mut cache) = build_parent_map(&mut world);
        let po_parent = paint_order_of(parent, &parents, &mut cache);
        let po_a = paint_order_of(child_a, &parents, &mut cache);
        let po_gc = paint_order_of(grandchild, &parents, &mut cache);
        let po_b = paint_order_of(child_b, &parents, &mut cache);

        assert!(
            po_parent < po_a && po_a < po_gc && po_gc < po_b,
            "expected pre-order parent < child_a < grandchild < child_b, got {po_parent} {po_a} {po_gc} {po_b}"
        );
    }

    /// R-css-flex: `z-index` overrides sibling paint order (higher paints
    /// later / on top), while equal z keeps document order.
    #[test]
    fn z_index_reorders_sibling_paint_order() {
        use crate::components::ZIndex;
        let mut world = World::new();
        let parent = world.spawn_empty().id();
        let a = world.spawn(ChildOf(parent)).id();
        let b = world.spawn((ChildOf(parent), ZIndex(-1))).id();
        let c = world.spawn(ChildOf(parent)).id();

        let (parents, mut cache) = build_parent_map(&mut world);
        let po_a = paint_order_of(a, &parents, &mut cache);
        let po_b = paint_order_of(b, &parents, &mut cache);
        let po_c = paint_order_of(c, &parents, &mut cache);
        assert!(
            po_b < po_a && po_a < po_c,
            "z:-1 sibling paints first; equal-z siblings keep document order (got {po_b} {po_a} {po_c})"
        );

        // Raise `a` above `c`.
        world.entity_mut(a).insert(ZIndex(5));
        let (parents, mut cache) = build_parent_map(&mut world);
        let po_a = paint_order_of(a, &parents, &mut cache);
        let po_c = paint_order_of(c, &parents, &mut cache);
        assert!(po_a > po_c, "z:5 sibling paints above z:auto");
    }

    /// W6 T2 regression (the invisible counter tiles): a child that
    /// PARTIALLY overhangs its scroll container's clip rect - e.g.
    /// `width: 100%` + margin pushing the right edge past the container,
    /// or a row straddling the container's bottom edge - must still be
    /// extracted (the vello clip layer trims it at paint time). Only a
    /// child with NO overlap at all may be culled.
    #[test]
    fn partially_clipped_child_is_extracted_fully_outside_is_culled() {
        use crate::components::Style;
        use crate::input::Scroll;
        let mut main = World::new();
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());

        // Scroll container: clip rect (0,0)-(960,504).
        let container = main
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(960.0, 504.0)),
                Style::default(),
                Scroll::default(),
            ))
            .id();
        // Tile shape from the counter app: margin shifts it to x=4 while
        // width:100% keeps it 960 wide -> right edge 964 > 960 (partial).
        let tile = main
            .spawn((
                Transform::new(Vec2::new(4.0, 8.0), Vec2::new(960.0, 80.0)),
                Visuals {
                    fill: Some(Fill::Solid(Color::rgb(1.0, 0.0, 0.0))),
                    radius: 0.0,
                    corner_radii: None,
                    shadows: Vec::new(),
                    border: None,
                },
                ChildOf(container),
            ))
            .id();
        // Straddles the container's bottom edge (480..560 vs clip 504).
        let straddler = main
            .spawn((
                Transform::new(Vec2::new(4.0, 480.0), Vec2::new(960.0, 80.0)),
                Visuals {
                    fill: Some(Fill::Solid(Color::rgb(0.0, 1.0, 0.0))),
                    radius: 0.0,
                    corner_radii: None,
                    shadows: Vec::new(),
                    border: None,
                },
                ChildOf(container),
            ))
            .id();
        // Fully below the clip rect: no overlap -> culled.
        let outside = main
            .spawn((
                Transform::new(Vec2::new(0.0, 600.0), Vec2::new(100.0, 50.0)),
                Visuals {
                    fill: Some(Fill::Solid(Color::rgb(0.0, 0.0, 1.0))),
                    radius: 0.0,
                    corner_radii: None,
                    shadows: Vec::new(),
                    border: None,
                },
                ChildOf(container),
            ))
            .id();

        extract_rects(&mut main, &mut render);

        let map = render.resource::<RenderEntityMap>();
        assert!(
            map.rect.contains_key(&tile),
            "width:100% + margin tile (4px overhang) must extract"
        );
        assert!(
            map.rect.contains_key(&straddler),
            "row straddling the clip bottom must extract"
        );
        assert!(
            !map.rect.contains_key(&outside),
            "zero-overlap child stays culled"
        );
        // Full size survives - the clip layer, not the extract, trims it.
        let re = map.rect[&tile];
        let rect = render.get::<ExtractedRect>(re).cloned().unwrap();
        assert_eq!(rect.size, Vec2::new(960.0, 80.0));
        assert_eq!(rect.origin, Vec2::new(4.0, 8.0));
    }

    /// R-css-flex: `extract_borders` emits one `ExtractedBorder` per
    /// entity with a `Visuals::border`, at the entity's own paint order,
    /// with widths in `[top, right, bottom, left]` order - and emits
    /// nothing for border-less visuals.
    #[test]
    fn extract_borders_emits_expected_widths_and_order() {
        use crate::components::{Border, Edges, Style};
        let mut main = World::new();
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());

        let bordered = main
            .spawn((
                Transform::new(Vec2::new(5.0, 6.0), Vec2::new(50.0, 40.0)),
                Style::default(),
                Visuals {
                    fill: None,
                    radius: 8.0,
                    corner_radii: None,
                    shadows: Vec::new(),
                    border: Some(Border {
                        widths: Edges {
                            top: 1.0,
                            right: 2.0,
                            bottom: 3.0,
                            left: 4.0,
                            ..Edges::default()
                        },
                        color: Color::rgb(1.0, 0.0, 0.0),
                        side_colors: None,
                    }),
                },
            ))
            .id();
        let _plain = main
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(10.0, 10.0)),
                Visuals {
                    fill: Some(Fill::Solid(Color::rgb(0.0, 1.0, 0.0))),
                    radius: 0.0,
                    corner_radii: None,
                    shadows: Vec::new(),
                    border: None,
                },
            ))
            .id();

        extract_borders(&mut main, &mut render);

        let borders: Vec<ExtractedBorder> = {
            let mut q = render.query::<&ExtractedBorder>();
            q.iter(&render).copied().collect()
        };
        assert_eq!(borders.len(), 1, "only the bordered entity extracts");
        let b = borders[0];
        assert_eq!(b.origin, Vec2::new(5.0, 6.0));
        assert_eq!(b.size, Vec2::new(50.0, 40.0));
        assert_eq!(b.widths, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(b.radius, 8.0);

        // Clearing the border removes the render entity on the next pass.
        main.get_mut::<Visuals>(bordered).unwrap().border = None;
        extract_borders(&mut main, &mut render);
        let count = {
            let mut q = render.query::<&ExtractedBorder>();
            q.iter(&render).count()
        };
        assert_eq!(count, 0);
    }

    /// CSS opacity semantics: an ancestor's `Opacity` multiplies into
    /// every descendant's painted alpha (this is what makes a fading
    /// dialog fade its content, not just its scrim).
    #[test]
    fn ancestor_opacity_multiplies_into_descendant_fill() {
        let mut main = World::new();
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());
        let parent = main
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(100.0, 100.0)),
                Opacity(0.5),
            ))
            .id();
        main.spawn((
            Transform::new(Vec2::ZERO, Vec2::new(50.0, 50.0)),
            Visuals {
                fill: Some(Fill::Solid(Color::rgba(1.0, 0.0, 0.0, 1.0))),
                radius: 0.0,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            },
            Opacity(0.5),
            ChildOf(parent),
        ));
        extract_rects(&mut main, &mut render);
        let rects: Vec<ExtractedRect> = {
            let mut q = render.query::<&ExtractedRect>();
            q.iter(&render).cloned().collect()
        };
        assert_eq!(rects.len(), 1);
        let Brush::Solid(c) = &rects[0].brush else {
            panic!("solid brush expected");
        };
        // own 0.5 x ancestor 0.5 = 0.25.
        assert!((c.a - 0.25).abs() < 1e-4, "expected 0.25, got {}", c.a);
    }

    /// Spec section 16.2: overlay bars extract only when content overflows, and
    /// paint ABOVE every descendant (order strictly greater than the
    /// deepest child's paint order).
    #[test]
    fn scrollbar_extracts_above_content_only_when_overflowing() {
        use crate::input::{Scroll, ScrollbarState};
        let mut main = World::new();
        main.insert_resource(crate::input::ScrollbarInteraction::default());
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());

        let scroller = main
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(200.0, 400.0)),
                Scroll::vertical(),
                ScrollOffset::default(),
                ScrollbarState::default(),
            ))
            .id();
        // Content taller than the viewport.
        let content = main
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(200.0, 900.0)),
                ChildOf(scroller),
            ))
            .id();
        extract_scrollbars(&mut main, &mut render);
        let bars: Vec<ExtractedScrollbar> = {
            let mut q = render.query::<&ExtractedScrollbar>();
            q.iter(&render).cloned().collect()
        };
        assert_eq!(bars.len(), 1, "overflowing scroller gets a bar");
        assert!(!bars[0].draws.is_empty());
        let (parents, mut cache) = build_parent_map(&mut main);
        let content_order = paint_order_of(content, &parents, &mut cache);
        assert!(
            bars[0].order > content_order,
            "bar order {} must sit above content order {content_order}",
            bars[0].order
        );

        // Shrink the content to fit - the bar disappears on next extract.
        main.get_mut::<Transform>(content).unwrap().size = Vec2::new(200.0, 300.0);
        extract_scrollbars(&mut main, &mut render);
        let count = {
            let mut q = render.query::<&ExtractedScrollbar>();
            q.iter(&render).count()
        };
        assert_eq!(count, 0, "as-needed visibility: no overflow, no bar");
    }

    /// `scrollbar-width: none` disables painting entirely while the
    /// container still scrolls.
    #[test]
    fn scrollbar_width_none_paints_nothing() {
        use crate::input::{Scroll, ScrollbarState, ScrollbarStyle, ScrollbarWidthMode};
        let mut main = World::new();
        main.insert_resource(crate::input::ScrollbarInteraction::default());
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());
        let scroller = main
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(200.0, 400.0)),
                Scroll::vertical(),
                ScrollOffset::default(),
                ScrollbarState::default(),
                ScrollbarStyle {
                    width: ScrollbarWidthMode::None,
                    ..Default::default()
                },
            ))
            .id();
        main.spawn((
            Transform::new(Vec2::ZERO, Vec2::new(200.0, 900.0)),
            ChildOf(scroller),
        ));
        extract_scrollbars(&mut main, &mut render);
        let count = {
            let mut q = render.query::<&ExtractedScrollbar>();
            q.iter(&render).count()
        };
        assert_eq!(count, 0);
    }

    /// RC2 regression: an entity outside the hierarchy forest must never land inside a clip bracket.
    #[test]
    fn orphan_entities_sort_after_tree_content() {
        let mut world = World::new();
        let orphan = world.spawn_empty().id();
        let child = world.spawn_empty().id();
        let parent = world.spawn_empty().id();
        world.entity_mut(child).insert(ChildOf(parent));

        let (parents, mut cache) = build_parent_map(&mut world);
        let po_orphan = paint_order_of(orphan, &parents, &mut cache);
        let po_child = paint_order_of(child, &parents, &mut cache);
        assert!(
            po_orphan > po_child,
            "orphan ({po_orphan}) must paint after tree content ({po_child})"
        );
    }

    /// RC2 regression: `ExtractedClipBox` `[start_order, end_order]` must bracket exactly the clip
    /// entity's descendants - a later sibling spawned with a LOWER entity id (the kanban failure
    /// mode: entity-id tiebreakers interleaved unrelated siblings into scroll/lane clips) must fall
    /// strictly outside the range.
    #[test]
    fn clip_ranges_bracket_exactly_descendants() {
        use crate::components::{Overflow, Style, Transform};
        let mut main = World::new();
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());

        // Allocate leaf ids first so document order and entity-id order disagree.
        let row1 = main.spawn(Transform::default()).id();
        let row2 = main.spawn(Transform::default()).id();
        let button = main.spawn(Transform::default()).id();
        let scrollbox = main
            .spawn((
                Style {
                    overflow_y: Overflow::Hidden,
                    ..Default::default()
                },
                Transform {
                    absolute: Vec2::new(10.0, 10.0),
                    size: Vec2::new(200.0, 100.0),
                    baseline_y: None,
                },
            ))
            .id();
        let root = main.spawn(Transform::default()).id();
        // Document order: root -> [scrollbox -> [row1, row2], button].
        main.entity_mut(scrollbox).insert(ChildOf(root));
        main.entity_mut(button).insert(ChildOf(root));
        main.entity_mut(row1).insert(ChildOf(scrollbox));
        main.entity_mut(row2).insert(ChildOf(scrollbox));

        extract_clips(&mut main, &mut render);

        let clip = {
            let mut q = render.query::<&ExtractedClipBox>();
            let boxes: Vec<ExtractedClipBox> = q.iter(&render).copied().collect();
            assert_eq!(boxes.len(), 1, "exactly one clip candidate");
            boxes[0]
        };

        let (parents, mut cache) = build_parent_map(&mut main);
        let po_scrollbox = paint_order_of(scrollbox, &parents, &mut cache);
        let po_row1 = paint_order_of(row1, &parents, &mut cache);
        let po_row2 = paint_order_of(row2, &parents, &mut cache);
        let po_button = paint_order_of(button, &parents, &mut cache);
        let po_root = paint_order_of(root, &parents, &mut cache);

        assert_eq!(clip.start_order, po_scrollbox);
        assert_eq!(clip.end_order, po_row1.max(po_row2));
        // Descendants inside the bracket...
        assert!(clip.start_order < po_row1 && po_row1 <= clip.end_order);
        assert!(clip.start_order < po_row2 && po_row2 <= clip.end_order);
        // ...non-descendants strictly outside, despite the button's lower entity id.
        assert!(
            po_button > clip.end_order,
            "later sibling (order {po_button}) must not be swallowed by clip range [{}, {}]",
            clip.start_order,
            clip.end_order
        );
        assert!(po_root < clip.start_order);
    }

    /// Overlay bug repro: a popup panel early in document order must paint AFTER later-document-order
    /// content (the widget-garden "Long dropdown over textarea" bleed-through). The whole overlay
    /// subtree lands in the top-layer band with contiguous internal pre-order ranks.
    #[test]
    fn overlay_subtree_paints_after_all_normal_content() {
        let mut world = World::new();
        let root = world.spawn_empty().id();
        // Document order: root -> [dropdown_row -> [wrapper -> [panel -> [opt1, opt2]]], textarea_row].
        let dropdown_row = world.spawn(ChildOf(root)).id();
        let wrapper = world.spawn(ChildOf(dropdown_row)).id();
        let panel = world.spawn((ChildOf(wrapper), OverlayLayer)).id();
        let opt1 = world.spawn(ChildOf(panel)).id();
        let opt2 = world.spawn(ChildOf(panel)).id();
        let textarea_row = world.spawn(ChildOf(root)).id();
        let textarea = world.spawn(ChildOf(textarea_row)).id();

        let (parents, mut cache) = build_parent_map(&mut world);
        let po_panel = paint_order_of(panel, &parents, &mut cache);
        let po_opt1 = paint_order_of(opt1, &parents, &mut cache);
        let po_opt2 = paint_order_of(opt2, &parents, &mut cache);
        for normal in [root, dropdown_row, wrapper, textarea_row, textarea] {
            let o = paint_order_of(normal, &parents, &mut cache);
            assert!(
                o < OVERLAY_ORDER_BASE,
                "normal content stays below the overlay band, got {o:#x}"
            );
            assert!(
                po_panel > o,
                "panel ({po_panel:#x}) must paint after normal content ({o:#x})"
            );
        }
        // Internal document order + stride-2 contiguity survive the re-banding.
        assert_eq!(po_opt1, po_panel + 2, "panel then first option");
        assert_eq!(po_opt2, po_opt1 + 2, "options keep sibling order");
        // Overlay band sits below the orphan fallback band.
        assert!((OVERLAY_ORDER_BASE..0x8000_0000).contains(&po_panel));
    }

    /// Two popups open on different ticks: the later-opened one must paint on top, and re-opening a
    /// popup must restamp it above a still-open one.
    #[test]
    fn overlays_stack_by_open_order() {
        let mut world = World::new();
        let root = world.spawn_empty().id();
        // Popup A is first in document order; both carry a child so the forest includes them.
        let a = world.spawn((ChildOf(root), OverlayLayer)).id();
        let _a_kid = world.spawn(ChildOf(a)).id();
        let b = world
            .spawn((ChildOf(root), OverlayLayer, Visible(false)))
            .id();
        let _b_kid = world.spawn(ChildOf(b)).id();

        // Tick 1: only A is open.
        let (parents, mut cache) = build_parent_map(&mut world);
        let po_a_t1 = paint_order_of(a, &parents, &mut cache);
        assert!(po_a_t1 >= OVERLAY_ORDER_BASE);

        // Tick 2: B opens later -> stacks above A.
        world.entity_mut(b).insert(Visible(true));
        let (parents, mut cache) = build_parent_map(&mut world);
        let po_a = paint_order_of(a, &parents, &mut cache);
        let po_b = paint_order_of(b, &parents, &mut cache);
        assert!(
            po_b > po_a,
            "later-opened popup B ({po_b:#x}) must paint over A ({po_a:#x})"
        );

        // Tick 3: A closes then re-opens -> restamped above B.
        world.entity_mut(a).insert(Visible(false));
        let _ = build_parent_map(&mut world);
        world.entity_mut(a).insert(Visible(true));
        let (parents, mut cache) = build_parent_map(&mut world);
        let po_a = paint_order_of(a, &parents, &mut cache);
        let po_b = paint_order_of(b, &parents, &mut cache);
        assert!(
            po_a > po_b,
            "re-opened popup A ({po_a:#x}) must now paint over B ({po_b:#x})"
        );
    }

    /// An ancestor scroll/overflow clip must not stretch its bracket over an overlay subtree inside
    /// it - otherwise the pushed layer would clip all content ranked between the bracket ends.
    #[test]
    fn clip_ranges_exclude_overlay_subtrees() {
        use crate::components::{Overflow, Style, Transform};
        let mut main = World::new();
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());

        let root = main.spawn(Transform::default()).id();
        let scrollbox = main
            .spawn((
                Style {
                    overflow_y: Overflow::Hidden,
                    ..Default::default()
                },
                Transform {
                    absolute: Vec2::new(0.0, 0.0),
                    size: Vec2::new(200.0, 100.0),
                    baseline_y: None,
                },
                ChildOf(root),
            ))
            .id();
        let row = main.spawn((Transform::default(), ChildOf(scrollbox))).id();
        let panel = main
            .spawn((Transform::default(), ChildOf(scrollbox), OverlayLayer))
            .id();
        let opt = main.spawn((Transform::default(), ChildOf(panel))).id();

        extract_clips(&mut main, &mut render);
        let clip = {
            let mut q = render.query::<&ExtractedClipBox>();
            let boxes: Vec<ExtractedClipBox> = q.iter(&render).copied().collect();
            assert_eq!(boxes.len(), 1, "exactly one clip candidate");
            boxes[0]
        };

        let (parents, mut cache) = build_parent_map(&mut main);
        let po_row = paint_order_of(row, &parents, &mut cache);
        let po_panel = paint_order_of(panel, &parents, &mut cache);
        let po_opt = paint_order_of(opt, &parents, &mut cache);
        assert_eq!(
            clip.end_order, po_row,
            "bracket ends at the last NORMAL descendant"
        );
        assert!(
            po_panel > clip.end_order && po_opt > clip.end_order,
            "overlay subtree ({po_panel:#x}, {po_opt:#x}) must fall outside the clip bracket [{:#x}, {:#x}]",
            clip.start_order,
            clip.end_order
        );
    }

    /// A clip owned INSIDE the overlay subtree (scrollable long dropdown) keeps a paired bracket in
    /// the overlay band covering exactly its descendants.
    #[test]
    fn overlay_internal_clip_brackets_intact() {
        use crate::components::{Overflow, Style, Transform};
        let mut main = World::new();
        let mut render = World::new();
        render.insert_resource(RenderEntityMap::default());

        let root = main.spawn(Transform::default()).id();
        let panel = main
            .spawn((
                Style {
                    overflow_y: Overflow::Hidden,
                    ..Default::default()
                },
                Transform {
                    absolute: Vec2::new(0.0, 0.0),
                    size: Vec2::new(200.0, 100.0),
                    baseline_y: None,
                },
                ChildOf(root),
                OverlayLayer,
            ))
            .id();
        let opt1 = main.spawn((Transform::default(), ChildOf(panel))).id();
        let opt2 = main.spawn((Transform::default(), ChildOf(panel))).id();

        extract_clips(&mut main, &mut render);
        let clip = {
            let mut q = render.query::<&ExtractedClipBox>();
            let boxes: Vec<ExtractedClipBox> = q.iter(&render).copied().collect();
            assert_eq!(boxes.len(), 1);
            boxes[0]
        };

        let (parents, mut cache) = build_parent_map(&mut main);
        let po_panel = paint_order_of(panel, &parents, &mut cache);
        let po_opt1 = paint_order_of(opt1, &parents, &mut cache);
        let po_opt2 = paint_order_of(opt2, &parents, &mut cache);
        assert_eq!(clip.start_order, po_panel);
        assert_eq!(clip.end_order, po_opt1.max(po_opt2));
        assert!(
            clip.start_order >= OVERLAY_ORDER_BASE,
            "bracket lives in the overlay band"
        );
        assert!(clip.start_order < po_opt1 && po_opt1 <= clip.end_order);
        assert!(clip.start_order < po_opt2 && po_opt2 <= clip.end_order);
    }

    /// Overlay content escapes ancestor scroll/overflow clip rects (top-layer semantics): the
    /// nearest-clip-ancestor map must stop at the overlay root, while normal siblings keep theirs.
    #[test]
    fn popup_content_escapes_ancestor_clip_rect() {
        use crate::components::{Overflow, Style, Transform};
        let mut main = World::new();

        let root = main.spawn(Transform::default()).id();
        let scrollbox = main
            .spawn((
                Style {
                    overflow_y: Overflow::Hidden,
                    ..Default::default()
                },
                Transform {
                    absolute: Vec2::new(0.0, 0.0),
                    size: Vec2::new(200.0, 50.0),
                    baseline_y: None,
                },
                ChildOf(root),
            ))
            .id();
        let row = main.spawn((Transform::default(), ChildOf(scrollbox))).id();
        let panel = main
            .spawn((Transform::default(), ChildOf(scrollbox), OverlayLayer))
            .id();
        let opt = main.spawn((Transform::default(), ChildOf(panel))).id();

        let (parents, _) = build_parent_map(&mut main);
        let clips = parent_scroll_clip_rects(&mut main, &parents);
        assert!(
            clips.contains_key(&row),
            "normal child keeps its scroll-ancestor clip"
        );
        assert!(
            !clips.contains_key(&panel) && !clips.contains_key(&opt),
            "overlay subtree escapes the ancestor clip rect"
        );
    }

    /// `ExtractedText::line_height_px` falls back to
    /// `size_px * DEFAULT_LINE_HEIGHT_MULTIPLIER` when the entity's
    /// `TextStyle::line_height` is absent (no `line-height` CSS reached
    /// this element) - preserves today's `1.2` behaviour exactly.
    #[test]
    fn line_height_px_falls_back_to_default_multiplier() {
        use crate::components::{
            DEFAULT_LINE_HEIGHT_MULTIPLIER, TextContent, TextStyle, Transform,
        };

        let mut main = World::new();
        let mut render = World::new();
        render.init_resource::<RenderEntityMap>();
        main.spawn((
            Transform::new(Vec2::ZERO, Vec2::new(120.0, 24.0)),
            TextContent("hi".to_string()),
            TextStyle {
                size_px: 20.0,
                ..Default::default()
            },
        ));
        extract_text(&mut main, &mut render);
        let mut q = render.query::<&ExtractedText>();
        let et = q.iter(&render).next().unwrap();
        assert_eq!(et.line_height_px, 20.0 * DEFAULT_LINE_HEIGHT_MULTIPLIER);
    }

    /// A CSS `line-height` value (here an explicit multiplier) overrides
    /// the default `1.2` ratio end-to-end through `extract_text`.
    #[test]
    fn line_height_px_honours_css_multiplier_override() {
        use crate::components::{LineHeightSpec, TextContent, TextStyle, Transform};

        let mut main = World::new();
        let mut render = World::new();
        render.init_resource::<RenderEntityMap>();
        main.spawn((
            Transform::new(Vec2::ZERO, Vec2::new(120.0, 24.0)),
            TextContent("hi".to_string()),
            TextStyle {
                size_px: 20.0,
                line_height: Some(LineHeightSpec::Multiplier(1.5)),
                ..Default::default()
            },
        ));
        extract_text(&mut main, &mut render);
        let mut q = render.query::<&ExtractedText>();
        let et = q.iter(&render).next().unwrap();
        assert_eq!(et.line_height_px, 30.0);
    }

    /// A CSS `line-height` value expressed in absolute pixels
    /// ([`LineHeightSpec::Px`]) does not scale with `size_px`.
    #[test]
    fn line_height_px_honours_css_absolute_override() {
        use crate::components::{LineHeightSpec, TextContent, TextStyle, Transform};

        let mut main = World::new();
        let mut render = World::new();
        render.init_resource::<RenderEntityMap>();
        main.spawn((
            Transform::new(Vec2::ZERO, Vec2::new(120.0, 24.0)),
            TextContent("hi".to_string()),
            TextStyle {
                size_px: 20.0,
                line_height: Some(LineHeightSpec::Px(19.0)),
                ..Default::default()
            },
        ));
        extract_text(&mut main, &mut render);
        let mut q = render.query::<&ExtractedText>();
        let et = q.iter(&render).next().unwrap();
        assert_eq!(et.line_height_px, 19.0);
    }

    /// `ExtractedText::caret_width_px` falls back to [`CARET_WIDTH_PX`]
    /// absent a [`CaretWidth`] override, and honours the override when
    /// present - the same override/fallback shape as every other
    /// CSS-supplied value.
    #[test]
    fn caret_width_px_falls_back_then_honours_override() {
        use crate::components::{CaretWidth, TextContent, Transform};

        let mut main = World::new();
        let mut render = World::new();
        render.init_resource::<RenderEntityMap>();
        let plain = main
            .spawn((
                Transform::new(Vec2::ZERO, Vec2::new(120.0, 24.0)),
                TextContent("a".to_string()),
            ))
            .id();
        let overridden = main
            .spawn((
                Transform::new(Vec2::new(0.0, 40.0), Vec2::new(120.0, 24.0)),
                TextContent("b".to_string()),
                CaretWidth(4.0),
            ))
            .id();
        extract_text(&mut main, &mut render);
        let map = render.resource::<RenderEntityMap>().text.clone();
        let plain_et = render.get::<ExtractedText>(map[&plain]).unwrap();
        let overridden_et = render.get::<ExtractedText>(map[&overridden]).unwrap();
        assert_eq!(plain_et.caret_width_px, CARET_WIDTH_PX);
        assert_eq!(overridden_et.caret_width_px, 4.0);
    }
}
