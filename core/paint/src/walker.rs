//! Tree-walker for the retained [`lumen_core::node_ir::Node`] IR.
//!
//! Single source of truth for every render path: each backend hands its
//! [`Painter`] to [`walk_node`], and each [`Node`] variant maps onto the
//! matching painter calls - see the `Node` doc-comments for the 1:1 mapping
//! table to Qt SceneGraph and GTK GSK.
//!
//! ## Frame diff
//!
//! [`diff_retained_scenes`] compares the prior frame's tree (via `lumen_core::node_ir::PreviousScene`)
//! against this frame's and short-circuits via `Arc::ptr_eq` on identical subtrees. The answer gates
//! whether a backend repaints at all.

use crate::cache::FragmentCache;
use crate::emit::{
    draw_image, draw_text, emit_border, emit_outline, emit_outline_cached, emit_rect,
    emit_rect_cached, emit_shadow, emit_shadow_cached, emit_svg, folded, text_bounds,
};
use crate::layers::{LayerCache, Plan};
use crate::{PaintTarget, Shape};
use bevy_ecs::world::World;
use lumen_core::native::{NativePaintCtx, NativePainters};
use lumen_core::node_ir::{Affine2, ClipShape, Node, PreviousScene, RetainedScene};
use lumen_core::render_world::{
    ExtractedOutline, ExtractedRect, ExtractedShadow, Rect as LumenRect, Viewport,
};
use peniko::Fill;
use peniko::kurbo::{Affine, Rect, RoundedRect};
use std::sync::Arc;

/// Active clip stack maintained by [`walk_node`]; each entry is the `push_layer` count we owe.
///
/// `walk_node` tracks the number of `push_layer` calls outstanding so the corresponding number of
/// `pop_layer` calls can be issued when the [`Node::Clip`] subtree returns. Modelled as a `Vec<u32>` of
/// per-level pop counts; in practice every Clip pushes exactly one layer, but the abstraction lets future
/// Opacity / Transform variants push more than one (transform pushes a layer + a transform, etc.).
#[derive(Default, Debug)]
pub struct ClipStack {
    levels: Vec<u32>,
}

impl ClipStack {
    /// Pushes a fresh stack level expected to pop `pops` layers on exit.
    pub fn push(&mut self, pops: u32) {
        self.levels.push(pops);
    }

    /// Pops the top stack level and returns the number of `pop_layer` calls to emit.
    pub fn pop(&mut self) -> u32 {
        self.levels.pop().unwrap_or(0)
    }

    /// Returns the current nesting depth.
    pub fn depth(&self) -> usize {
        self.levels.len()
    }
}

/// Walker context shared across recursive `walk_node` calls. Carries the running transform stack and
/// opacity multiplier so `Node::Transform` / `Node::Opacity` compose along the recursion.
pub struct WalkContext<'a> {
    /// The backend's sink receiving the frame.
    pub painter: &'a mut PaintTarget,
    /// Optional fragment cache. When `Some`, leaf encoders go through the cached emitters; when
    /// `None`, leaves take the uncached path.
    pub cache: Option<&'a mut FragmentCache>,
    /// Optional text shaper. When `None`, [`Node::Text`] leaves are silently skipped (matches the legacy
    /// behaviour when the offscreen plugin is built without a shaper).
    pub shaper: Option<&'a mut dyn lumen_text::TextShaper>,
    /// Running transform pushed by ancestor [`Node::Transform`] frames. Composed by post-multiplication -
    /// children inherit ancestor coordinate frames, same as Qt SG / GSK.
    pub transform: Affine,
    /// Device pixel ratio. Multiplied into every leaf's origin / size /
    /// font size / radius / shadow blur before the emit helper hands the
    /// scaled values to the painter. Lumen layout-taffy outputs logical
    /// pixels; the target is sized in physical pixels. Without this
    /// scale, content draws only into the top-left `1/dpr x 1/dpr` of the
    /// surface on hi-DPI displays - the search-bar-not-visible bug.
    pub dpr: f32,
    /// Running alpha multiplier pushed by ancestor [`Node::Opacity`] frames; multiplies into leaf colours.
    pub opacity: f32,
    /// Active clip stack - see [`ClipStack`].
    pub clips: ClipStack,
    /// Painters for [`Node::Native`] leaves. When `None`, or when no painter is registered for a
    /// leaf's `extension_id`, that leaf paints nothing.
    pub natives: Option<&'a NativePainters>,
    /// Layers kept for faded groups across frames. When `None`, every group paints its layer
    /// afresh.
    pub layers: Option<&'a mut LayerCache>,
}

impl<'a> WalkContext<'a> {
    /// Builds a context with identity transform + full opacity + empty clip stack.
    ///
    /// Walker coordinates are LOGICAL pixels (matching `Viewport.size` /
    /// `Transform.absolute` from layout-taffy). For drawing into a
    /// physical-pixel-sized target, callers should use
    /// [`Self::new_with_dpr`] instead so logical coords scale to physical
    /// at the root.
    pub fn new(
        painter: &'a mut PaintTarget,
        cache: Option<&'a mut FragmentCache>,
        shaper: Option<&'a mut dyn lumen_text::TextShaper>,
    ) -> Self {
        Self {
            painter,
            cache,
            shaper,
            transform: Affine::IDENTITY,
            opacity: 1.0,
            clips: ClipStack::default(),
            dpr: 1.0,
            natives: None,
            layers: None,
        }
    }

    /// Same as [`Self::new`] but seeds the root transform with a
    /// `scale(dpr)` so logical-pixel Node IR coordinates map to physical
    /// pixels in the target. Pass the window's `scale_factor`
    /// (a.k.a. device-pixel ratio). Without this scale, hi-DPI surfaces
    /// only fill the top-left `1/dpr x 1/dpr` of the window - the
    /// logical 1280-wide layout lands at physical pixels 0..1280 of a
    /// 2560-wide surface, leaving the right + bottom quadrants dark.
    pub fn new_with_dpr(
        painter: &'a mut PaintTarget,
        cache: Option<&'a mut FragmentCache>,
        shaper: Option<&'a mut dyn lumen_text::TextShaper>,
        dpr: f32,
    ) -> Self {
        Self {
            painter,
            cache,
            shaper,
            transform: Affine::IDENTITY,
            opacity: 1.0,
            clips: ClipStack::default(),
            dpr: dpr.max(0.01),
            natives: None,
            layers: None,
        }
    }

    /// Attaches the painter registry that [`Node::Native`] leaves dispatch through. Without it
    /// every native leaf is skipped, which is how a backend with no registered painters stays
    /// portable rather than failing.
    pub fn with_native_painters(mut self, painters: &'a NativePainters) -> Self {
        self.natives = Some(painters);
        self
    }

    /// Attaches the cache faded groups keep their layers in across frames. Hand it only with a
    /// sink that paints offscreen ([`crate::Painter::begin_offscreen`]); the caller brackets the
    /// walk with [`LayerCache::begin_frame`] and [`LayerCache::end_frame`].
    pub fn with_layer_cache(mut self, layers: &'a mut LayerCache) -> Self {
        self.layers = Some(layers);
        self
    }
}

/// Converts the walker's running transform back into the IR's affine, which is what a native
/// painter is handed - native painters see logical coordinates and the device scale, not kurbo types.
fn affine_to_affine2(a: Affine) -> Affine2 {
    Affine2 {
        coeffs: a.as_coeffs(),
    }
}

fn lumen_rect_to_kurbo(r: LumenRect) -> Rect {
    Rect::new(
        r.origin.x as f64,
        r.origin.y as f64,
        (r.origin.x + r.size.x) as f64,
        (r.origin.y + r.size.y) as f64,
    )
}

fn lumen_rect_to_rounded(r: LumenRect, radii: [f32; 4]) -> RoundedRect {
    // Per-corner radii route straight through - CSS order
    // [top-left, top-right, bottom-right, bottom-left] matches
    // kurbo's `RoundedRectRadii::new(top_left, top_right, bottom_right,
    // bottom_left)` argument order.
    use peniko::kurbo::RoundedRectRadii;
    let rect = lumen_rect_to_kurbo(r);
    RoundedRect::from_rect(
        rect,
        RoundedRectRadii::new(
            radii[0] as f64,
            radii[1] as f64,
            radii[2] as f64,
            radii[3] as f64,
        ),
    )
}

/// Scales a logical-pixel [`ClipShape`] into physical pixels. Clip shapes come out of
/// `transform_extracted_to_nodes` in LOGICAL coordinates (same space as every leaf), and the walker
/// scales leaves by `ctx.dpr` at emit time (see [`walk_node`]'s `Node::Rect` arm and commits
/// 46acc97 / eb47c3d for the chosen convention). Clips must follow the same convention - an unscaled
/// clip at dpr 1.5 covers only the top-left `1/1.5 x 1/1.5` of its subtree, chopping off the right /
/// bottom third of every clipped region (the "dialog text cut mid-word" bug).
fn scale_clip_shape(shape: ClipShape, dpr: f32) -> ClipShape {
    match shape {
        ClipShape::Rect(r) => ClipShape::Rect(LumenRect {
            origin: r.origin * dpr,
            size: r.size * dpr,
        }),
        ClipShape::RoundedRect { rect, radii } => ClipShape::RoundedRect {
            rect: LumenRect {
                origin: rect.origin * dpr,
                size: rect.size * dpr,
            },
            radii: [
                radii[0] * dpr,
                radii[1] * dpr,
                radii[2] * dpr,
                radii[3] * dpr,
            ],
        },
    }
}

/// Pushes a layer matching the given [`ClipShape`]. Returns the number of pops the caller owes.
fn push_clip_layer(painter: &mut PaintTarget, shape: ClipShape, opacity: f32) -> u32 {
    let alpha = opacity.clamp(0.0, 1.0);
    let clip = match shape {
        ClipShape::Rect(r) => Shape::Rect(lumen_rect_to_kurbo(r)),
        ClipShape::RoundedRect { rect, radii } => {
            Shape::RoundedRect(lumen_rect_to_rounded(rect, radii))
        }
    };
    painter.push_layer(
        Fill::NonZero,
        peniko::BlendMode::default(),
        alpha,
        Affine::IDENTITY,
        &clip,
    );
    1
}

/// The bounds an opacity group's layer falls back to for a leaf whose extent cannot be read: wide
/// enough to cover any target.
const GROUP_EXTENT: LumenRect = LumenRect {
    origin: glam::Vec2::new(-1.0e6, -1.0e6),
    size: glam::Vec2::new(2.0e6, 2.0e6),
};

/// Walks a single [`Node`] subtree onto the painter.
///
/// Each leaf variant maps onto painter calls through the matching emitter, and the
/// Container/Transform/Opacity/Clip variants compose along the recursion. Leaves go through the
/// supplied [`FragmentCache`] when one is present.
pub fn walk_node(ctx: &mut WalkContext<'_>, node: &Node) {
    match node {
        Node::Container { children } => {
            for child in children {
                walk_node(ctx, child);
            }
        }
        Node::Transform { matrix, child } => {
            // Push the transform onto the running matrix - a stack push, not a per-leaf transform,
            // matching Qt's `QSGTransformNode`.
            let prev = ctx.transform;
            let c = matrix.coeffs;
            let t = Affine::new([c[0], c[1], c[2], c[3], c[4], c[5]]);
            ctx.transform = prev * t;
            walk_node(ctx, child);
            ctx.transform = prev;
        }
        Node::Opacity { alpha, child, key } => {
            // Group opacity: the subtree paints into a layer of its own and the layer fades once
            // when it closes, so a child over its parent hides it as it would at full opacity.
            let alpha = alpha.clamp(0.0, 1.0);
            if alpha <= 0.0 {
                return;
            }
            if alpha >= 1.0 {
                walk_node(ctx, child);
                return;
            }
            walk_group(ctx, child, alpha, *key);
        }
        Node::Mask {
            region,
            mask,
            child,
        } => walk_mask(ctx, *region, mask, child),
        Node::Clip { shape, child } => {
            // Scale the logical clip rect to physical pixels - leaves below scale themselves by
            // ctx.dpr at emit time, so the clip must live in the same (physical) space.
            let pops = push_clip_layer(ctx.painter, scale_clip_shape(*shape, ctx.dpr), ctx.opacity);
            ctx.clips.push(pops);
            walk_node(ctx, child);
            let to_pop = ctx.clips.pop();
            for _ in 0..to_pop {
                ctx.painter.pop_layer();
            }
        }
        Node::Rect {
            bounds,
            brush,
            corner,
            corners,
        } => {
            // Synthesise a transient ExtractedRect so the same emit helper
            // drives both legacy and tree paths. Pre-multiply by ctx.dpr so
            // logical coords from layout-taffy land at the right physical
            // pixels in the target.
            let cmd = ExtractedRect {
                origin: bounds.origin * ctx.dpr,
                size: bounds.size * ctx.dpr,
                brush: if ctx.opacity < 1.0 {
                    brush
                        .clone()
                        .with_opacity(lumen_core::components::Opacity(ctx.opacity.clamp(0.0, 1.0)))
                } else {
                    brush.clone()
                },
                radius: *corner * ctx.dpr,
                corner_radii: corners.map(|cs| cs.map(|c| c * ctx.dpr)),
                order: 0,
            };
            if let Some(cache) = ctx.cache.as_deref_mut() {
                emit_rect_cached(&mut **ctx.painter, cache, &cmd);
            } else {
                emit_rect(&mut **ctx.painter, &cmd);
            }
        }
        Node::Shadow {
            origin,
            size,
            radius,
            spread,
            blur,
            color,
            inner,
            rect_origin,
        } => {
            let color = folded(*color, ctx.opacity);
            let cmd = ExtractedShadow {
                origin: *origin * ctx.dpr,
                size: *size * ctx.dpr,
                radius: *radius * ctx.dpr,
                spread: *spread * ctx.dpr,
                blur: *blur * ctx.dpr,
                color,
                order: 0,
                inner: *inner,
                rect_origin: *rect_origin * ctx.dpr,
            };
            if let Some(cache) = ctx.cache.as_deref_mut() {
                emit_shadow_cached(&mut **ctx.painter, cache, &cmd);
            } else {
                emit_shadow(&mut **ctx.painter, &cmd);
            }
        }
        Node::Border {
            origin,
            size,
            widths,
            color,
            side_colors,
            radius,
            corners,
        } => {
            let color = folded(*color, ctx.opacity);
            let side_colors = side_colors.map(|cs| cs.map(|c| folded(c, ctx.opacity)));
            let cmd = lumen_core::render_world::ExtractedBorder {
                origin: *origin * ctx.dpr,
                size: *size * ctx.dpr,
                widths: [
                    widths[0] * ctx.dpr,
                    widths[1] * ctx.dpr,
                    widths[2] * ctx.dpr,
                    widths[3] * ctx.dpr,
                ],
                color,
                side_colors,
                radius: *radius * ctx.dpr,
                corner_radii: corners.map(|cs| cs.map(|c| c * ctx.dpr)),
                order: 0,
            };
            emit_border(&mut **ctx.painter, &cmd);
        }
        Node::Outline {
            origin,
            size,
            stroke,
            width,
            radius,
        } => {
            let stroke = folded(*stroke, ctx.opacity);
            let cmd = ExtractedOutline {
                origin: *origin * ctx.dpr,
                size: *size * ctx.dpr,
                stroke,
                width: *width * ctx.dpr,
                radius: *radius * ctx.dpr,
                order: 0,
            };
            if let Some(cache) = ctx.cache.as_deref_mut() {
                emit_outline_cached(&mut **ctx.painter, cache, &cmd);
            } else {
                emit_outline(&mut **ctx.painter, &cmd);
            }
        }
        Node::Text { run } => {
            // Pass the run by reference; `draw_text` folds in
            // ctx.dpr (origin / font size / container width) and ctx.opacity
            // (fill alpha) locally, so no per-node clone of the run - and its
            // owned String - is needed. caret + selection stay byte offsets
            // into the source string and are not scaled.
            if let Some(shaper) = ctx.shaper.as_deref_mut() {
                draw_text(shaper, &mut **ctx.painter, run, ctx.dpr, ctx.opacity);
            }
        }
        Node::Image { image, blob } => {
            // Apply opacity to the image alpha multiplier. Pre-multiply
            // origin + size by ctx.dpr for hi-DPI.
            let mut img = image.clone();
            if ctx.opacity < 1.0 {
                img.alpha *= ctx.opacity.clamp(0.0, 1.0);
            }
            img.origin *= ctx.dpr;
            img.size *= ctx.dpr;
            if let Some(blob) = blob
                && let Some(b) = blob.downcast_ref::<lumen_assets::ExtractedImageBlob>()
            {
                draw_image(&mut **ctx.painter, &img, b);
            }
        }
        Node::Svg { payload } => {
            if let Some(svg) = payload.downcast_ref::<lumen_assets::ExtractedSvg>() {
                let mut svg = svg.clone();
                if ctx.opacity < 1.0 {
                    svg.alpha *= ctx.opacity.clamp(0.0, 1.0);
                }
                svg.origin *= ctx.dpr;
                svg.size *= ctx.dpr;
                emit_svg(&mut **ctx.painter, ctx.cache.as_deref_mut(), &svg);
            }
        }
        Node::Native {
            extension_id,
            payload,
            bounds,
            clip_to_bounds,
            ..
        } => {
            // An id with no painter registered paints nothing. That is the portability story: a
            // scene carrying an extension this backend does not implement still renders the rest.
            let painter = ctx
                .natives
                .and_then(|registry| registry.get(extension_id))
                .cloned();
            let Some(painter) = painter else {
                return;
            };
            let transform = affine_to_affine2(ctx.transform);
            let (dpr, opacity) = (ctx.dpr, ctx.opacity);
            // Layers the walker owns right now. Everything the painter opens sits above this mark,
            // and nothing below it is the painter's to close.
            let depth_before = ctx.painter.layer_depth();
            if *clip_to_bounds {
                // Clip in the painter's own space: the same device transform the painter is handed,
                // over the same logical bounds. Pre-scaling the rect instead would put the clip in a
                // different space than the content as soon as an ancestor transform is non-identity.
                // Alpha stays at 1: opacity has one owner, and it is the painter, through
                // `ctx.opacity`. A clip that also composited would make the leaf's alpha depend on
                // whether it asked to be clipped.
                let device = Affine::scale(dpr as f64) * ctx.transform;
                ctx.painter.push_layer(
                    Fill::NonZero,
                    peniko::BlendMode::default(),
                    1.0,
                    device,
                    &Shape::Rect(lumen_rect_to_kurbo(*bounds)),
                );
            }
            let backend_id = ctx.painter.backend_id();
            let mut paint_ctx = NativePaintCtx::new(
                payload.as_ref(),
                &mut *ctx.painter,
                backend_id,
                *bounds,
                transform,
                dpr,
                opacity,
            );
            painter.paint(&mut paint_ctx);
            // Rebalance. A painter that left layers open would otherwise have the walker's later
            // pops close its layers instead of the walker's own; one that over-popped would already
            // have closed an ancestor's clip, and popping again here would close another. Closing
            // down to the mark and no further is the only move that is right in both directions.
            while ctx.painter.layer_depth() > depth_before {
                ctx.painter.pop_layer();
            }
        }
    }
}

/// Paints `child` through the alpha of what `mask` paints, inside `region`.
///
/// The child paints into a layer of its own clipped to the region, so the mask touches nothing
/// under it; the mask then paints in a nested destination-in layer, which keeps the child where the
/// mask has alpha and clears it where it has none, when it closes. An empty mask shows nothing.
fn walk_mask(ctx: &mut WalkContext<'_>, region: LumenRect, mask: &Arc<Node>, child: &Arc<Node>) {
    if matches!(mask.as_ref(), Node::Container { children } if children.is_empty()) {
        return;
    }
    let ClipShape::Rect(device) = scale_clip_shape(ClipShape::Rect(region), ctx.dpr) else {
        return;
    };
    let clip = Shape::Rect(lumen_rect_to_kurbo(device));
    let depth = ctx.painter.layer_depth();
    ctx.painter.push_layer(
        Fill::NonZero,
        peniko::BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &clip,
    );
    walk_node(ctx, child);
    while ctx.painter.layer_depth() > depth + 1 {
        ctx.painter.pop_layer();
    }
    ctx.painter.push_layer(
        Fill::NonZero,
        peniko::BlendMode::new(peniko::Mix::Normal, peniko::Compose::DestIn),
        1.0,
        Affine::IDENTITY,
        &clip,
    );
    // The mask's alpha is its own: an ancestor's fade reaches the child's paints, not the mask's.
    let opacity = std::mem::replace(&mut ctx.opacity, 1.0);
    walk_node(ctx, mask);
    ctx.opacity = opacity;
    while ctx.painter.layer_depth() > depth {
        ctx.painter.pop_layer();
    }
}

/// Paints the subtree of a group fading at `alpha` (in `(0, 1)`), into one layer that composites at
/// `alpha`.
///
/// With a [`LayerCache`] and a `key`, the layer is the kept one while the subtree paints the same
/// as when it was kept, so the subtree itself paints only when it changed; see
/// [`crate::layers`]. Otherwise, or for a subtree holding a native leaf (whose painter may draw
/// past the sink, straight into the backend's own target), the subtree paints into a layer that
/// lives for this frame only.
fn walk_group(ctx: &mut WalkContext<'_>, child: &Arc<Node>, alpha: f32, key: Option<u64>) {
    let (bounds, native) = group_bounds(ctx, child);
    // Bounds come out in logical pixels like a clip's, so they scale the same way; a pixel of
    // slack keeps the antialiased edges of the subtree inside, and whole-pixel edges let a kept
    // layer composite one layer pixel per target pixel.
    let slack = glam::Vec2::splat(1.0);
    let device = Rect::new(
        ((bounds.origin.x - slack.x) * ctx.dpr).floor() as f64,
        ((bounds.origin.y - slack.y) * ctx.dpr).floor() as f64,
        ((bounds.origin.x + bounds.size.x + slack.x) * ctx.dpr).ceil() as f64,
        ((bounds.origin.y + bounds.size.y + slack.y) * ctx.dpr).ceil() as f64,
    );
    if let (Some(key), false) = (key, native)
        && let Some(layers) = ctx.layers.as_deref_mut()
    {
        let device = device.intersect(layers.target_rect());
        if device.width() <= 0.0 || device.height() <= 0.0 {
            // Nothing of the group lands on the target.
            return;
        }
        match layers.plan(key, child, device) {
            Plan::Composite(layer) => {
                if ctx.painter.draw_offscreen(&layer, alpha) {
                    return;
                }
            }
            Plan::Render => {
                if ctx.painter.begin_offscreen(device) {
                    walk_node(ctx, child);
                    if let Some(layer) = ctx.painter.end_offscreen() {
                        ctx.painter.draw_offscreen(&layer, alpha);
                        if let Some(layers) = ctx.layers.as_deref_mut() {
                            layers.store(key, layer);
                        }
                    }
                    return;
                }
            }
            Plan::Direct => {}
        }
    }
    ctx.painter.push_layer(
        Fill::NonZero,
        peniko::BlendMode::default(),
        alpha,
        Affine::IDENTITY,
        &Shape::Rect(device),
    );
    walk_node(ctx, child);
    ctx.painter.pop_layer();
}

/// The logical-pixel box that holds everything `node` paints, and whether it holds a native leaf.
///
/// Text is measured the way it paints, through the context's shaper, so a run that wraps onto
/// more lines or overflows its container stays inside; without a shaper it falls back to the
/// frame diff's rough box. A clip bounds what it wraps.
fn group_bounds(ctx: &mut WalkContext<'_>, node: &Node) -> (LumenRect, bool) {
    match node {
        Node::Container { children } => {
            let mut acc: Option<LumenRect> = None;
            let mut native = false;
            for c in children {
                let (b, n) = group_bounds(ctx, c);
                native |= n;
                if has_area(b) {
                    acc = Some(acc.map_or(b, |a| union(a, b)));
                }
            }
            (acc.unwrap_or(EMPTY), native)
        }
        Node::Opacity { child, .. } => group_bounds(ctx, child),
        Node::Mask { region, child, .. } => {
            let (b, native) = group_bounds(ctx, child);
            (intersection(b, *region), native)
        }
        Node::Clip { shape, child } => {
            let (b, native) = group_bounds(ctx, child);
            let clip = match shape {
                ClipShape::Rect(r) | ClipShape::RoundedRect { rect: r, .. } => *r,
            };
            (intersection(b, clip), native)
        }
        Node::Transform { child, matrix } => {
            let (b, native) = group_bounds(ctx, child);
            (
                apply_affine_to_rect(b, compose_affine(Affine::IDENTITY, *matrix)),
                native,
            )
        }
        Node::Text { run } => match ctx.shaper.as_deref_mut() {
            Some(shaper) => {
                let r = text_bounds(shaper, run, ctx.dpr);
                let dpr = f64::from(ctx.dpr);
                let b = LumenRect {
                    origin: glam::Vec2::new((r.x0 / dpr) as f32, (r.y0 / dpr) as f32),
                    size: glam::Vec2::new((r.width() / dpr) as f32, (r.height() / dpr) as f32),
                };
                (b, false)
            }
            None => (node_bounds(node, GROUP_EXTENT, Affine::IDENTITY), false),
        },
        Node::Native { .. } => (node_bounds(node, GROUP_EXTENT, Affine::IDENTITY), true),
        _ => (node_bounds(node, GROUP_EXTENT, Affine::IDENTITY), false),
    }
}

/// A rect with no area.
const EMPTY: LumenRect = LumenRect {
    origin: glam::Vec2::ZERO,
    size: glam::Vec2::ZERO,
};

/// The overlap of two rects; [`EMPTY`] when they do not overlap.
fn intersection(a: LumenRect, b: LumenRect) -> LumenRect {
    let x0 = a.origin.x.max(b.origin.x);
    let y0 = a.origin.y.max(b.origin.y);
    let x1 = (a.origin.x + a.size.x).min(b.origin.x + b.size.x);
    let y1 = (a.origin.y + a.size.y).min(b.origin.y + b.size.y);
    if x1 <= x0 || y1 <= y0 {
        return EMPTY;
    }
    LumenRect {
        origin: glam::Vec2::new(x0, y0),
        size: glam::Vec2::new(x1 - x0, y1 - y0),
    }
}

/// Whether `curr` paints anything differently from `prev`, by the frame diff: the test a kept
/// group layer is reused under.
pub(crate) fn subtree_changed(prev: &Arc<Node>, curr: &Arc<Node>) -> bool {
    diff_node(prev, curr, GROUP_EXTENT, Affine::IDENTITY)
}

/// Walks the [`RetainedScene`] root into the context's painter.
///
/// Convenience entry point - equivalent to `walk_node(&ctx, &scene.root)` once the root is known to exist.
pub fn walk_retained_scene(ctx: &mut WalkContext<'_>, scene: &RetainedScene) {
    if let Some(root) = scene.root.as_ref() {
        walk_node(ctx, root);
    }
}

/// Whether `curr` paints differently from `prev`: the frame gate every renderer applies before
/// repainting.
///
/// Recursion proceeds in lockstep and short-circuits on `Arc::ptr_eq`, so identical Arc-shared
/// subtrees cost nothing, and stops at the first change it finds. A subtree that was deleted,
/// inserted, or changed counts when its bounds (old or new) cover any area; one with no area paints
/// nothing either way. Containers diff position-by-position, and an extra tail on either side
/// counts as a deletion or an insertion.
pub fn diff_retained_scenes(
    prev: Option<&Arc<Node>>,
    curr: Option<&Arc<Node>>,
    viewport: LumenRect,
) -> bool {
    match (prev, curr) {
        (None, None) => false,
        // Whole new tree appeared, or the whole tree was removed.
        (None, Some(node)) | (Some(node), None) => {
            has_area(node_bounds(node, viewport, Affine::IDENTITY))
        }
        (Some(p), Some(c)) => diff_node(p, c, viewport, Affine::IDENTITY),
    }
}

/// Lockstep recursive diff. `xform` is the running ancestor transform: for nested `Node::Transform`
/// frames the same matrix multiplies into both sides. When parameters diverge, both whole subtrees
/// count, each under its own matrix.
fn diff_node(prev: &Arc<Node>, curr: &Arc<Node>, viewport: LumenRect, xform: Affine) -> bool {
    if Arc::ptr_eq(prev, curr) {
        return false;
    }
    match (prev.as_ref(), curr.as_ref()) {
        (Node::Container { children: pc }, Node::Container { children: cc }) => {
            let common = pc.len().min(cc.len());
            (0..common).any(|i| diff_node(&pc[i], &cc[i], viewport, xform))
                || pc[common..]
                    .iter()
                    .chain(&cc[common..])
                    .any(|child| has_area(node_bounds(child, viewport, xform)))
        }
        (
            Node::Transform {
                matrix: pm,
                child: pchild,
            },
            Node::Transform {
                matrix: cm,
                child: cchild,
            },
        ) => {
            if pm == cm {
                diff_node(pchild, cchild, viewport, compose_affine(xform, *cm))
            } else {
                either_has_area(
                    pchild,
                    cchild,
                    viewport,
                    compose_affine(xform, *pm),
                    compose_affine(xform, *cm),
                )
            }
        }
        (
            Node::Opacity {
                alpha: pa,
                child: pchild,
                ..
            },
            Node::Opacity {
                alpha: ca,
                child: cchild,
                ..
            },
        ) => {
            if (pa - ca).abs() < f32::EPSILON {
                diff_node(pchild, cchild, viewport, xform)
            } else {
                either_has_area(pchild, cchild, viewport, xform, xform)
            }
        }
        (
            Node::Mask {
                region: pr,
                mask: pm,
                child: pchild,
            },
            Node::Mask {
                region: cr,
                mask: cm,
                child: cchild,
            },
        ) => {
            if pr == cr && !diff_node(pm, cm, viewport, xform) {
                diff_node(pchild, cchild, viewport, xform)
            } else {
                either_has_area(prev, curr, viewport, xform, xform)
            }
        }
        (
            Node::Clip {
                shape: ps,
                child: pchild,
            },
            Node::Clip {
                shape: cs,
                child: cchild,
            },
        ) => {
            if ps == cs {
                diff_node(pchild, cchild, viewport, xform)
            } else {
                either_has_area(pchild, cchild, viewport, xform, xform)
            }
        }
        // Leaves: compare appearance. The producer rebuilds every leaf as a
        // fresh `Arc` each frame, so `Arc::ptr_eq` never matches across frames
        // - a purely structural (ptr / bounds) diff would therefore mark every
        // leaf changed and defeat partial repaint entirely. Comparing the
        // leaves' visual fields lets an unchanged leaf count as unchanged.
        _ => !leaf_visually_eq(prev, curr) && either_has_area(prev, curr, viewport, xform, xform),
    }
}

/// Whether the old or the new side of a changed pair covers any area, each under its own transform.
fn either_has_area(
    prev: &Node,
    curr: &Node,
    viewport: LumenRect,
    prev_xform: Affine,
    curr_xform: Affine,
) -> bool {
    has_area(node_bounds(prev, viewport, prev_xform))
        || has_area(node_bounds(curr, viewport, curr_xform))
}

/// Returns `true` when two leaf nodes are visually identical - same
/// appearance and same position - so a diff between them contributes no
/// damage.
///
/// Conservative by construction: only the leaf variants whose every visual
/// field is comparable return `true`. An image or SVG payload of a type this
/// crate does not know, and any cross-variant pair, fall through to `false`,
/// i.e. "assume changed" - a false *positive* damage only costs an unnecessary
/// repaint, whereas a false *negative* would drop a real change on the floor.
/// This is the same safety bias as GTK's / Qt's damage bookkeeping: never
/// under-report the dirty region.
fn leaf_visually_eq(a: &Node, b: &Node) -> bool {
    match (a, b) {
        (
            Node::Rect {
                bounds: ab,
                brush: abr,
                corner: ac,
                corners: acr,
            },
            Node::Rect {
                bounds: bb,
                brush: bbr,
                corner: bc,
                corners: bcr,
            },
        ) => ab == bb && ac == bc && acr == bcr && abr == bbr,
        (
            Node::Shadow {
                origin: ao,
                size: asz,
                radius: ar,
                spread: asp,
                blur: abl,
                color: acl,
                inner: ain,
                rect_origin: aro,
            },
            Node::Shadow {
                origin: bo,
                size: bsz,
                radius: br,
                spread: bsp,
                blur: bbl,
                color: bcl,
                inner: bin,
                rect_origin: bro,
            },
        ) => {
            ao == bo
                && asz == bsz
                && ar == br
                && asp == bsp
                && abl == bbl
                && acl == bcl
                && ain == bin
                && aro == bro
        }
        (
            Node::Border {
                origin: ao,
                size: asz,
                widths: aw,
                color: acl,
                side_colors: asc,
                radius: ar,
                corners: acr,
            },
            Node::Border {
                origin: bo,
                size: bsz,
                widths: bw,
                color: bcl,
                side_colors: bsc,
                radius: br,
                corners: bcr,
            },
        ) => {
            ao == bo && asz == bsz && aw == bw && acl == bcl && asc == bsc && ar == br && acr == bcr
        }
        (
            Node::Outline {
                origin: ao,
                size: asz,
                stroke: ast,
                width: awi,
                radius: ar,
            },
            Node::Outline {
                origin: bo,
                size: bsz,
                stroke: bst,
                width: bwi,
                radius: br,
            },
        ) => ao == bo && asz == bsz && ast == bst && awi == bwi && ar == br,
        // Text runs compare by every shaped-input field (see the `ExtractedText`
        // `PartialEq` note): identical fields => identical shaping => identical
        // pixels.
        (Node::Text { run: ar }, Node::Text { run: br }) => ar == br,
        // Native leaves compare by the stamp their producer maintains. The
        // payload `Arc` is rebuilt every dirty frame, so identity would report
        // every leaf changed; the revision is the seam's contract instead -
        // equal revision at equal geometry means equal pixels.
        (
            Node::Native {
                extension_id: aid,
                bounds: ab,
                revision: arv,
                clip_to_bounds: ac,
                ..
            },
            Node::Native {
                extension_id: bid,
                bounds: bb,
                revision: brv,
                clip_to_bounds: bc,
                ..
            },
        ) => aid == bid && ab == bb && arv == brv && ac == bc,
        // Images compare by every placed field and by the identity of their
        // pixels: the producer shares one pixel buffer across frames for as
        // long as the picture is the same.
        (
            Node::Image {
                image: ai,
                blob: ab,
            },
            Node::Image {
                image: bi,
                blob: bb,
            },
        ) => {
            let same_blob = match (ab, bb) {
                (None, None) => true,
                (Some(a), Some(b)) => match (
                    a.downcast_ref::<lumen_assets::ExtractedImageBlob>(),
                    b.downcast_ref::<lumen_assets::ExtractedImageBlob>(),
                ) {
                    (Some(a), Some(b)) => a.0.id() == b.0.id(),
                    _ => false,
                },
                _ => false,
            };
            same_blob
                && ai.origin == bi.origin
                && ai.size == bi.size
                && ai.width == bi.width
                && ai.height == bi.height
                && std::sync::Arc::ptr_eq(&ai.rgba, &bi.rgba)
                && ai.fit == bi.fit
                && ai.rendering == bi.rendering
                && ai.alpha == bi.alpha
                && ai.background == bi.background
        }
        // SVGs compare by every placed field and by the identity of the
        // decoded drawing they paint.
        (Node::Svg { payload: ap }, Node::Svg { payload: bp }) => match (
            ap.downcast_ref::<lumen_assets::ExtractedSvg>(),
            bp.downcast_ref::<lumen_assets::ExtractedSvg>(),
        ) {
            (Some(a), Some(b)) => {
                a.asset.id() == b.asset.id()
                    && a.origin == b.origin
                    && a.size == b.size
                    && a.intrinsic == b.intrinsic
                    && a.fit == b.fit
                    && a.alpha == b.alpha
            }
            _ => false,
        },
        // Any cross-variant pair is a real change.
        _ => false,
    }
}

/// Returns the bounding rect of `node` in window coordinates, with `xform` applied. Containers recurse and
/// union; leaves report their own bounds. Conservative fallback: the viewport for a leaf whose
/// geometry cannot be read (an SVG payload this backend does not recognise).
fn node_bounds(node: &Node, viewport: LumenRect, xform: Affine) -> LumenRect {
    match node {
        Node::Container { children } => {
            let mut acc: Option<LumenRect> = None;
            for c in children {
                let b = node_bounds(c, viewport, xform);
                acc = Some(match acc {
                    None => b,
                    Some(a) => union(a, b),
                });
            }
            acc.unwrap_or(LumenRect {
                origin: glam::Vec2::ZERO,
                size: glam::Vec2::ZERO,
            })
        }
        Node::Transform { matrix, child } => {
            let composed = compose_affine(xform, *matrix);
            node_bounds(child, viewport, composed)
        }
        Node::Opacity { child, .. } | Node::Clip { child, .. } => {
            node_bounds(child, viewport, xform)
        }
        Node::Mask { region, child, .. } => intersection(
            node_bounds(child, viewport, xform),
            apply_affine_to_rect(*region, xform),
        ),
        Node::Rect { bounds, .. } => apply_affine_to_rect(*bounds, xform),
        Node::Shadow {
            origin,
            size,
            blur,
            spread,
            ..
        } => {
            // `blur` is the Gaussian's standard deviation, and the blurred rect reaches about three
            // of them past its edge.
            let pad = blur.max(0.0) * 3.0 + spread.max(0.0);
            let r = LumenRect {
                origin: *origin - glam::Vec2::splat(pad),
                size: *size + glam::Vec2::splat(pad * 2.0),
            };
            apply_affine_to_rect(r, xform)
        }
        Node::Border { origin, size, .. } => {
            let r = LumenRect {
                origin: *origin,
                size: *size,
            };
            apply_affine_to_rect(r, xform)
        }
        Node::Outline {
            origin,
            size,
            width,
            ..
        } => {
            let pad = width.max(0.0) * 0.5;
            let r = LumenRect {
                origin: *origin - glam::Vec2::splat(pad),
                size: *size + glam::Vec2::splat(pad * 2.0),
            };
            apply_affine_to_rect(r, xform)
        }
        Node::Text { run } => {
            // Rough vertical box from origin (the baseline) and the container width. Width is the container
            // width; height is two text sizes (ascender + descender padding).
            let h = run.size_px.max(1.0) * 1.6;
            let r = LumenRect {
                origin: glam::Vec2::new(run.origin.x, run.origin.y - run.size_px),
                size: glam::Vec2::new(run.container_width.max(run.size_px), h),
            };
            apply_affine_to_rect(r, xform)
        }
        Node::Image { image, .. } => {
            let r = LumenRect {
                origin: image.origin,
                size: image.size,
            };
            apply_affine_to_rect(r, xform)
        }
        Node::Svg { payload } => {
            if let Some(svg) = payload.downcast_ref::<lumen_assets::ExtractedSvg>() {
                let r = LumenRect {
                    origin: svg.origin,
                    size: svg.size,
                };
                apply_affine_to_rect(r, xform)
            } else {
                viewport
            }
        }
        // The seam requires bounds that enclose every pixel the painter touches, so a native leaf
        // reports them like any other leaf. Bounds with no area are the one exception: a change
        // with no area counts as no change, which would leave such a leaf frozen on screen no
        // matter how often its revision moved, so it falls back to the viewport - a repaint that
        // costs too much beats one that never happens.
        Node::Native { bounds, .. } => {
            if bounds.size.x <= 0.0 || bounds.size.y <= 0.0 {
                viewport
            } else {
                apply_affine_to_rect(*bounds, xform)
            }
        }
    }
}

/// Compose ancestor running affine with a child Affine2 from the IR. Mirrors the multiplication used inside
/// `walk_node`'s Transform branch but lives at the diff layer because computing bounds needs no
/// painter.
fn compose_affine(parent: Affine, child: lumen_core::node_ir::Affine2) -> Affine {
    let c = child.coeffs;
    let m = Affine::new([c[0], c[1], c[2], c[3], c[4], c[5]]);
    parent * m
}

fn apply_affine_to_rect(r: LumenRect, xform: Affine) -> LumenRect {
    if xform == Affine::IDENTITY {
        return r;
    }
    let x0 = r.origin.x as f64;
    let y0 = r.origin.y as f64;
    let x1 = x0 + r.size.x as f64;
    let y1 = y0 + r.size.y as f64;
    let pts = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (px, py) in pts {
        let p = xform * peniko::kurbo::Point::new(px, py);
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    LumenRect {
        origin: glam::Vec2::new(min_x as f32, min_y as f32),
        size: glam::Vec2::new((max_x - min_x) as f32, (max_y - min_y) as f32),
    }
}

fn union(a: LumenRect, b: LumenRect) -> LumenRect {
    if a.size == glam::Vec2::ZERO {
        return b;
    }
    if b.size == glam::Vec2::ZERO {
        return a;
    }
    let ax0 = a.origin.x;
    let ay0 = a.origin.y;
    let ax1 = ax0 + a.size.x;
    let ay1 = ay0 + a.size.y;
    let bx0 = b.origin.x;
    let by0 = b.origin.y;
    let bx1 = bx0 + b.size.x;
    let by1 = by0 + b.size.y;
    let x0 = ax0.min(bx0);
    let y0 = ay0.min(by0);
    let x1 = ax1.max(bx1);
    let y1 = ay1.max(by1);
    LumenRect {
        origin: glam::Vec2::new(x0, y0),
        size: glam::Vec2::new(x1 - x0, y1 - y0),
    }
}

/// Whether a changed subtree's bounds cover any pixels. A change with no area paints nothing.
fn has_area(r: LumenRect) -> bool {
    r.size.x > 0.0 && r.size.y > 0.0
}

/// Whether the retained Node IR differs visually from the last painted frame.
///
/// Diffs `PreviousScene` (the last painted tree) against `RetainedScene` (this tick's freshly built
/// tree). Conservative: the diff assumes-changed for any leaf it cannot compare (an image or SVG
/// payload of a type it does not know), so it never misses a change, and a render world missing the scene resources (a non-standard embed)
/// always reports one.
pub fn scene_has_damage(render_world: &World) -> bool {
    let previous = render_world.get_resource::<PreviousScene>();
    let retained = render_world.get_resource::<RetainedScene>();
    let (Some(previous), Some(retained)) = (previous, retained) else {
        return true;
    };
    let size = render_world
        .get_resource::<Viewport>()
        .map(|v| v.size)
        .unwrap_or_default();
    let viewport_rect = LumenRect {
        origin: glam::Vec2::ZERO,
        size,
    };
    diff_retained_scenes(
        previous.root.as_ref(),
        retained.root.as_ref(),
        viewport_rect,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RC3 regression: clip shapes must scale by dpr exactly like leaves do, otherwise a clipped
    /// subtree at dpr 1.5 loses its right/bottom third (clip covers only logical-sized area of the
    /// physical surface).
    #[test]
    fn clip_rect_scales_to_full_physical_region_at_fractional_dpr() {
        let dpr = 1.5;
        let logical = ClipShape::Rect(LumenRect {
            origin: glam::Vec2::new(20.0, 10.0),
            size: glam::Vec2::new(800.0, 600.0),
        });
        match scale_clip_shape(logical, dpr) {
            ClipShape::Rect(r) => {
                assert_eq!(r.origin, glam::Vec2::new(30.0, 15.0));
                assert_eq!(r.size, glam::Vec2::new(1200.0, 900.0));
                // A leaf at the logical bottom-right corner scales to (1230, 915) physical - the
                // scaled clip must still contain it (the unscaled clip ended at 820x610).
                let leaf_br = glam::Vec2::new(820.0, 610.0) * dpr;
                assert!(r.origin.x + r.size.x >= leaf_br.x);
                assert!(r.origin.y + r.size.y >= leaf_br.y);
            }
            other => panic!("expected Rect, got {other:?}"),
        }
    }

    #[test]
    fn rounded_clip_scales_rect_and_radii() {
        let dpr = 2.0;
        let logical = ClipShape::RoundedRect {
            rect: LumenRect {
                origin: glam::Vec2::new(5.0, 5.0),
                size: glam::Vec2::new(100.0, 50.0),
            },
            radii: [4.0, 8.0, 12.0, 16.0],
        };
        match scale_clip_shape(logical, dpr) {
            ClipShape::RoundedRect { rect, radii } => {
                assert_eq!(rect.origin, glam::Vec2::new(10.0, 10.0));
                assert_eq!(rect.size, glam::Vec2::new(200.0, 100.0));
                assert_eq!(radii, [8.0, 16.0, 24.0, 32.0]);
            }
            other => panic!("expected RoundedRect, got {other:?}"),
        }
    }

    /// What a [`GroupSink`] was asked to do, in order.
    #[derive(Clone, Debug, PartialEq)]
    enum Call {
        Fill,
        Push { alpha: f32, clip: Rect },
        Pop,
        Begin(Rect),
        End,
        Draw { alpha: f32, bounds: Rect },
    }

    /// A sink that paints offscreen as the walker sees it: layers are their bounds, and each call
    /// is logged so a test reads what the walker asked for.
    #[derive(Default)]
    struct GroupSink {
        calls: Vec<Call>,
        depth: usize,
        open: Vec<Rect>,
        refuse_begin: bool,
        refuse_draw: bool,
    }

    impl crate::Painter for GroupSink {
        fn fill(
            &mut self,
            _: Fill,
            _: Affine,
            _: peniko::BrushRef<'_>,
            _: Option<Affine>,
            _: &Shape<'_>,
        ) {
            self.calls.push(Call::Fill);
        }
        fn stroke(
            &mut self,
            _: &peniko::kurbo::Stroke,
            _: Affine,
            _: peniko::BrushRef<'_>,
            _: Option<Affine>,
            _: &Shape<'_>,
        ) {
        }
        fn push_layer(
            &mut self,
            _: Fill,
            _: peniko::BlendMode,
            alpha: f32,
            _: Affine,
            clip: &Shape<'_>,
        ) {
            let clip = match clip {
                Shape::Rect(r) => *r,
                _ => Rect::ZERO,
            };
            self.calls.push(Call::Push { alpha, clip });
            self.depth += 1;
        }
        fn pop_layer(&mut self) {
            self.calls.push(Call::Pop);
            self.depth -= 1;
        }
        fn draw_masked(
            &mut self,
            _: crate::MaskKind,
            _: Affine,
            _: &Shape<'_>,
            _: &mut dyn FnMut(&mut dyn crate::Painter),
            content: &mut dyn FnMut(&mut dyn crate::Painter),
        ) {
            content(self);
        }
        fn layer_depth(&self) -> usize {
            self.depth
        }
        fn draw_blurred_rounded_rect(
            &mut self,
            _: Affine,
            _: Rect,
            _: peniko::Color,
            _: f64,
            _: f64,
        ) {
        }
        fn draw_image(&mut self, _: &peniko::ImageBrush, _: Affine) {}
        fn draw_glyphs(&mut self, _: &crate::GlyphRun<'_>) {}
        fn begin_offscreen(&mut self, bounds: Rect) -> bool {
            if self.refuse_begin {
                return false;
            }
            self.calls.push(Call::Begin(bounds));
            self.open.push(bounds);
            true
        }
        fn end_offscreen(&mut self) -> Option<crate::OffscreenLayer> {
            let bounds = self.open.pop()?;
            self.calls.push(Call::End);
            Some(crate::OffscreenLayer::new(Arc::new(bounds)))
        }
        fn draw_offscreen(&mut self, layer: &crate::OffscreenLayer, alpha: f32) -> bool {
            let Some(bounds) = layer.downcast::<Rect>() else {
                return false;
            };
            if self.refuse_draw {
                return false;
            }
            self.calls.push(Call::Draw {
                alpha,
                bounds: *bounds,
            });
            true
        }
        fn backend_id(&self) -> &'static str {
            "test.group"
        }
        fn native(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    fn solid(x: f32, y: f32, w: f32, h: f32) -> Arc<Node> {
        Arc::new(Node::Rect {
            bounds: LumenRect {
                origin: glam::Vec2::new(x, y),
                size: glam::Vec2::new(w, h),
            },
            brush: lumen_core::render_world::Brush::Solid(lumen_core::components::Color::rgb(
                0.0, 0.0, 1.0,
            )),
            corner: 0.0,
            corners: None,
        })
    }

    fn group(alpha: f32, key: Option<u64>, child: Arc<Node>) -> Arc<Node> {
        Arc::new(Node::Opacity { alpha, child, key })
    }

    /// Walks `scene` into `sink` as one frame of a 100 x 100 target and returns the calls.
    fn frame(
        sink: GroupSink,
        layers: Option<&mut LayerCache>,
        scene: &Node,
    ) -> (Vec<Call>, GroupSink) {
        let mut target: PaintTarget = Box::new(sink);
        match layers {
            Some(layers) => {
                layers.begin_frame(1.0, 100, 100);
                let mut ctx = WalkContext::new(&mut target, None, None).with_layer_cache(layers);
                walk_node(&mut ctx, scene);
                if let Some(layers) = ctx.layers.as_deref_mut() {
                    layers.end_frame();
                }
            }
            None => walk_node(&mut WalkContext::new(&mut target, None, None), scene),
        }
        let mut sink = target
            .native()
            .downcast_mut::<GroupSink>()
            .map(std::mem::take)
            .expect("the test sink");
        assert!(
            sink.open.is_empty() && sink.depth == 0,
            "every layer closed"
        );
        (std::mem::take(&mut sink.calls), sink)
    }

    fn fills(calls: &[Call]) -> usize {
        calls.iter().filter(|c| **c == Call::Fill).count()
    }

    /// A kept group paints directly the first frame, into a kept layer the second, and from the
    /// layer alone after that, at whatever alpha the frame asks.
    #[test]
    fn a_still_group_paints_once_then_composites_its_layer() {
        let mut layers = LayerCache::default();
        let scene = group(0.5, Some(7), solid(10.0, 10.0, 20.0, 20.0));
        let device = Rect::new(9.0, 9.0, 31.0, 31.0);

        let (calls, sink) = frame(GroupSink::default(), Some(&mut layers), &scene);
        assert_eq!(
            calls,
            vec![
                Call::Push {
                    alpha: 0.5,
                    clip: device
                },
                Call::Fill,
                Call::Pop
            ],
            "first seen: painted directly in a faded layer",
        );

        let (calls, sink) = frame(sink, Some(&mut layers), &scene);
        assert_eq!(
            calls,
            vec![
                Call::Begin(device),
                Call::Fill,
                Call::End,
                Call::Draw {
                    alpha: 0.5,
                    bounds: device
                },
            ],
            "held still: rasterized into a kept layer",
        );
        assert_eq!(layers.stats().renders, 1);
        assert_eq!(layers.stats().layers, 1);

        let faded = group(0.25, Some(7), solid(10.0, 10.0, 20.0, 20.0));
        let (calls, _) = frame(sink, Some(&mut layers), &faded);
        assert_eq!(
            calls,
            vec![Call::Draw {
                alpha: 0.25,
                bounds: device
            }],
            "a new alpha composites the kept pixels, painting none of the subtree",
        );
        assert_eq!(layers.stats().hits, 1);
    }

    /// A sink that turns down an offscreen layer, or cannot composite the kept one, still gets
    /// the group painted, directly.
    #[test]
    fn a_refused_layer_falls_back_to_direct_paint() {
        let scene = group(0.5, Some(1), solid(10.0, 10.0, 20.0, 20.0));

        let mut layers = LayerCache::default();
        let no_begin = GroupSink {
            refuse_begin: true,
            ..Default::default()
        };
        let (_, sink) = frame(no_begin, Some(&mut layers), &scene);
        let (calls, _) = frame(sink, Some(&mut layers), &scene);
        assert!(!calls.iter().any(|c| matches!(c, Call::Begin(_))));
        assert_eq!(fills(&calls), 1, "painted directly: {calls:?}");
        assert!(matches!(calls[0], Call::Push { alpha, .. } if alpha == 0.5));
        assert_eq!(layers.stats().renders, 0);

        let mut layers = LayerCache::default();
        let no_draw = GroupSink {
            refuse_draw: true,
            ..Default::default()
        };
        let (_, sink) = frame(no_draw, Some(&mut layers), &scene);
        let (_, sink) = frame(sink, Some(&mut layers), &scene);
        assert_eq!(layers.stats().layers, 1, "the layer was kept");
        let (calls, _) = frame(sink, Some(&mut layers), &scene);
        assert_eq!(layers.stats().hits, 1);
        assert_eq!(
            fills(&calls),
            1,
            "the kept layer did not draw, so the group paints: {calls:?}"
        );
    }

    /// A group wholly outside the target paints nothing, and one holding a native leaf is never
    /// kept: its painter may draw past the sink.
    #[test]
    fn off_target_and_native_groups_are_never_kept() {
        let mut layers = LayerCache::default();
        let away = group(0.5, Some(2), solid(500.0, 500.0, 20.0, 20.0));
        let (calls, _) = frame(GroupSink::default(), Some(&mut layers), &away);
        assert!(calls.is_empty(), "{calls:?}");

        let native = Arc::new(Node::Native {
            extension_id: "test.chart".into(),
            payload: Arc::new(()),
            bounds: LumenRect {
                origin: glam::Vec2::new(40.0, 40.0),
                size: glam::Vec2::new(10.0, 10.0),
            },
            revision: 0,
            clip_to_bounds: false,
        });
        let scene = group(
            0.5,
            Some(3),
            Arc::new(Node::Container {
                children: vec![solid(10.0, 10.0, 20.0, 20.0), native],
            }),
        );
        let mut sink = GroupSink::default();
        for _ in 0..3 {
            let (calls, next) = frame(sink, Some(&mut layers), &scene);
            assert!(
                !calls
                    .iter()
                    .any(|c| matches!(c, Call::Begin(_) | Call::Draw { .. }))
            );
            assert_eq!(
                calls.first(),
                Some(&Call::Push {
                    alpha: 0.5,
                    clip: Rect::new(9.0, 9.0, 51.0, 51.0)
                }),
                "the layer spans the rect and the native leaf",
            );
            sink = next;
        }
        assert_eq!(layers.stats().layers, 0);
    }

    /// Full alpha paints the subtree with no layer, zero alpha paints nothing, and without a layer
    /// cache a faded group paints in a layer of its own every frame.
    #[test]
    fn opacity_extremes_and_uncached_groups() {
        let (calls, _) = frame(
            GroupSink::default(),
            None,
            &group(1.0, Some(1), solid(0.0, 0.0, 4.0, 4.0)),
        );
        assert_eq!(calls, vec![Call::Fill]);
        let (calls, _) = frame(
            GroupSink::default(),
            None,
            &group(0.0, Some(1), solid(0.0, 0.0, 4.0, 4.0)),
        );
        assert!(calls.is_empty());
        let (calls, _) = frame(
            GroupSink::default(),
            None,
            &group(0.5, Some(1), solid(0.0, 0.0, 4.0, 4.0)),
        );
        assert_eq!(fills(&calls), 1);
        assert!(matches!(calls[0], Call::Push { .. }));
    }

    /// A group's layer covers what its subtree paints: a clip bounds it, a transform moves it, and
    /// a nested group adds nothing of its own.
    #[test]
    fn group_bounds_follow_clips_transforms_and_nested_groups() {
        let layer_of = |child: Arc<Node>| {
            let (calls, _) = frame(GroupSink::default(), None, &group(0.5, None, child));
            match calls.first() {
                Some(Call::Push { clip, .. }) => *clip,
                other => panic!("expected the group's layer first, got {other:?}"),
            }
        };
        let clipped = Arc::new(Node::Clip {
            shape: ClipShape::Rect(LumenRect {
                origin: glam::Vec2::new(10.0, 10.0),
                size: glam::Vec2::new(20.0, 20.0),
            }),
            child: solid(0.0, 0.0, 100.0, 100.0),
        });
        assert_eq!(layer_of(clipped), Rect::new(9.0, 9.0, 31.0, 31.0));

        let moved = Arc::new(Node::Transform {
            matrix: Affine2::translate(5.0, 0.0),
            child: solid(0.0, 0.0, 10.0, 10.0),
        });
        assert_eq!(layer_of(moved), Rect::new(4.0, -1.0, 16.0, 11.0));

        let nested = group(0.5, None, solid(20.0, 20.0, 10.0, 10.0));
        assert_eq!(layer_of(nested), Rect::new(19.0, 19.0, 31.0, 31.0));
    }

    /// Two rects overlap in their shared part and not at all when apart.
    #[test]
    fn intersection_is_the_shared_part_or_nothing() {
        let r = |x: f32, y: f32, w: f32, h: f32| LumenRect {
            origin: glam::Vec2::new(x, y),
            size: glam::Vec2::new(w, h),
        };
        let shared = intersection(r(0.0, 0.0, 10.0, 10.0), r(5.0, 5.0, 10.0, 10.0));
        assert_eq!(shared.origin, glam::Vec2::new(5.0, 5.0));
        assert_eq!(shared.size, glam::Vec2::new(5.0, 5.0));
        let apart = intersection(r(0.0, 0.0, 10.0, 10.0), r(20.0, 0.0, 10.0, 10.0));
        assert_eq!(apart.size, glam::Vec2::ZERO);
    }

    fn image(blob: Option<Arc<dyn std::any::Any + Send + Sync>>, rgba: &Arc<[u8]>) -> Arc<Node> {
        Arc::new(Node::Image {
            image: lumen_core::render_world::ExtractedImage {
                origin: glam::Vec2::new(5.0, 5.0),
                size: glam::Vec2::new(4.0, 4.0),
                width: 2,
                height: 2,
                rgba: rgba.clone(),
                fit: lumen_core::components::ImageFit::Fill,
                rendering: Default::default(),
                order: 0,
                alpha: 1.0,
                background: None,
            },
            blob,
        })
    }

    /// An image is unchanged while it shares its pixels and its blob; a new blob, a missing one,
    /// or a payload of another type is a change.
    #[test]
    fn an_image_changes_with_its_blob() {
        let rgba: Arc<[u8]> = Arc::from(vec![255u8; 16]);
        let blob = |b: &peniko::Blob<u8>| -> Option<Arc<dyn std::any::Any + Send + Sync>> {
            Some(Arc::new(lumen_assets::ExtractedImageBlob(b.clone())))
        };
        let pixels = peniko::Blob::new(Arc::new(vec![255u8; 16]));
        let other = peniko::Blob::new(Arc::new(vec![255u8; 16]));
        assert!(!subtree_changed(&image(None, &rgba), &image(None, &rgba)));
        assert!(!subtree_changed(
            &image(blob(&pixels), &rgba),
            &image(blob(&pixels), &rgba)
        ));
        assert!(subtree_changed(
            &image(blob(&pixels), &rgba),
            &image(blob(&other), &rgba)
        ));
        assert!(subtree_changed(
            &image(blob(&pixels), &rgba),
            &image(None, &rgba)
        ));
        let foreign: Option<Arc<dyn std::any::Any + Send + Sync>> = Some(Arc::new(0u8));
        assert!(subtree_changed(
            &image(foreign.clone(), &rgba),
            &image(foreign, &rgba)
        ));
    }

    fn svg(asset: &lumen_assets::Handle<lumen_assets::SvgData>, alpha: f32) -> Arc<Node> {
        Arc::new(Node::Svg {
            payload: Arc::new(lumen_assets::ExtractedSvg {
                origin: glam::Vec2::new(0.0, 0.0),
                size: glam::Vec2::new(20.0, 20.0),
                intrinsic: glam::Vec2::new(10.0, 10.0),
                asset: asset.clone(),
                fit: lumen_core::components::ImageFit::Fill,
                order: 0,
                alpha,
            }),
        })
    }

    /// An SVG is unchanged while it paints the same decode the same way; another decode, another
    /// alpha, or a payload of another type is a change.
    #[test]
    fn an_svg_changes_with_its_decode_and_placement() {
        let decode = || -> lumen_assets::Handle<lumen_assets::SvgData> {
            let tree = usvg::Tree::from_str(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#,
                &usvg::Options::default(),
            )
            .expect("valid svg");
            lumen_assets::SvgData {
                intrinsic: glam::Vec2::new(10.0, 10.0),
                tree,
                id: lumen_assets::SvgData::next_id(),
                source_bytes: 0,
            }
            .into()
        };
        let a = decode();
        let b = decode();
        assert!(!subtree_changed(&svg(&a, 1.0), &svg(&a, 1.0)));
        assert!(subtree_changed(&svg(&a, 1.0), &svg(&a, 0.5)));
        assert!(subtree_changed(&svg(&a, 1.0), &svg(&b, 1.0)));
        let foreign = || {
            Arc::new(Node::Svg {
                payload: Arc::new(0u8),
            })
        };
        assert!(subtree_changed(&foreign(), &foreign()));
    }

    fn label() -> Arc<Node> {
        Arc::new(Node::Text {
            run: lumen_core::render_world::ExtractedText {
                origin: glam::Vec2::new(10.0, 20.0),
                text: "Save".to_string(),
                size_px: 16.0,
                fill: lumen_core::components::Color::rgba(0.0, 0.0, 0.0, 1.0),
                caret: None,
                selection: None,
                selection_color: None,
                selection_foreground: None,
                caret_color: None,
                order: 0,
                container_width: 200.0,
                align: lumen_core::components::TextAlign::Start,
                wrap: lumen_core::components::TextWrap::None,
                max_lines: None,
                family: None,
                weight: 400,
                line_height_px: 20.0,
                caret_width_px: 1.0,
            },
        })
    }

    /// A faded label's layer holds the box its shaper measures, in the same logical pixels at any
    /// scale factor; without a shaper the layer falls back to the frame diff's rough box.
    #[test]
    fn a_faded_label_is_measured_by_its_shaper() {
        let scene = group(0.5, None, label());
        let mut shaper = lumen_text::NullShaper;
        let mut target: PaintTarget = Box::<GroupSink>::default();
        walk_node(
            &mut WalkContext::new_with_dpr(&mut target, None, Some(&mut shaper), 2.0),
            &scene,
        );
        let sink = target
            .native()
            .downcast_mut::<GroupSink>()
            .expect("the test sink");
        // The container from x 10 to 210 and the line at y 20, padded by the 16 px size (and the
        // caret, and 1.5 sizes above the baseline), then by a pixel, all at dpr 2.
        assert_eq!(
            sink.calls.first(),
            Some(&Call::Push {
                alpha: 0.5,
                clip: Rect::new(-14.0, -10.0, 456.0, 74.0)
            }),
        );

        let (calls, _) = frame(GroupSink::default(), None, &scene);
        let rough = node_bounds(&label(), GROUP_EXTENT, Affine::IDENTITY);
        let want = Rect::new(
            f64::from((rough.origin.x - 1.0).floor()),
            f64::from((rough.origin.y - 1.0).floor()),
            f64::from((rough.origin.x + rough.size.x + 1.0).ceil()),
            f64::from((rough.origin.y + rough.size.y + 1.0).ceil()),
        );
        assert!(
            matches!(calls.first(), Some(Call::Push { clip, .. }) if *clip == want),
            "{calls:?} vs {want:?}"
        );
    }
}
