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
    emit_rect_cached, emit_shadow, emit_shadow_cached, emit_svg, folded,
};
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
        }
    }

    /// Attaches the painter registry that [`Node::Native`] leaves dispatch through. Without it
    /// every native leaf is skipped, which is how a backend with no registered painters stays
    /// portable rather than failing.
    pub fn with_native_painters(mut self, painters: &'a NativePainters) -> Self {
        self.natives = Some(painters);
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
        Node::Opacity { alpha, child } => {
            let prev = ctx.opacity;
            ctx.opacity = prev * alpha.clamp(0.0, 1.0);
            walk_node(ctx, child);
            ctx.opacity = prev;
        }
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
            },
            Node::Opacity {
                alpha: ca,
                child: cchild,
            },
        ) => {
            if (pa - ca).abs() < f32::EPSILON {
                diff_node(pchild, cchild, viewport, xform)
            } else {
                either_has_area(pchild, cchild, viewport, xform, xform)
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
/// field is comparable return `true`. Variants carrying an opaque payload we
/// cannot compare for equality ([`Node::Image`] blob, [`Node::Svg`]) and any
/// cross-variant pair fall through to `false`, i.e.
/// "assume changed" - a false *positive* damage only costs an unnecessary
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
        // Image / Svg carry `Arc<dyn Any>` payloads we cannot compare; any
        // cross-variant pair is also a real change. Assume changed.
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
        Node::Rect { bounds, .. } => apply_affine_to_rect(*bounds, xform),
        Node::Shadow {
            origin,
            size,
            blur,
            spread,
            ..
        } => {
            let pad = blur.max(0.0) + spread.max(0.0);
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
/// tree). Conservative: the diff assumes-changed for any leaf it cannot compare (images, SVGs), so it
/// never misses a change, and a render world missing the scene resources (a non-standard embed)
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
}
