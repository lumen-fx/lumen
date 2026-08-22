//! Vello tree-walker for the retained [`lumen_core::node_ir::Node`] IR.
//!
//! Single source of truth for both render paths (offscreen `wgpu_render_system` and on-screen
//! window-winit `render_frame`). Each [`Node`] variant maps onto the appropriate vello scene call -
//! see the `Node` doc-comments for the 1:1 mapping table to Qt SceneGraph and GTK GSK.
//!
//! ## Damage-rect diff (W2.2)
//!
//! `walk_retained_scene` accepts the prior frame's tree (via `lumen_core::node_ir::PreviousScene`) and short-
//! circuits via `Arc::ptr_eq` on identical subtrees. Today the diff only gates whether to *re-encode*, not
//! what GPU region is touched - vello 0.5 has no scissor API. Wave 2 ships the structural diff scaffolding
//! and emits damage rects into `FrameDamage` for downstream consumers (e.g. region invalidation when vello
//! grows scissor support, or when the embedder drives a partial-redraw path).

use crate::{
    SceneFragmentCache, append_translated, draw_image_into_vello, draw_text_into_vello,
    emit_outline, emit_outline_cached, emit_outline_into_fragment, emit_rect, emit_rect_cached,
    emit_rect_into_fragment, emit_shadow, emit_shadow_cached, emit_shadow_into_fragment, emit_svg,
};
use lumen_core::native::{NativePaintCtx, NativePainters};
use lumen_core::node_ir::{Affine2, ClipShape, Node, RetainedScene};
use lumen_core::render_world::{
    ExtractedImage, ExtractedOutline, ExtractedRect, ExtractedShadow, FrameDamage,
    Rect as LumenRect,
};
use std::sync::Arc;
use vello::peniko;
use vello::peniko::Fill;
use vello::peniko::kurbo::{Affine, Rect, RoundedRect};

/// Active clip stack maintained by [`walk_node`]; each entry is the vello `push_layer` count we owe.
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
    /// Target vello scene receiving the encoding.
    pub scene: &'a mut vello::Scene,
    /// Optional sub-scene fragment cache (the shared T1.3 + W2.4 cache). When `Some`, leaf encoders go
    /// through the cached emitters; when `None`, leaves take the uncached path.
    pub cache: Option<&'a mut SceneFragmentCache>,
    /// Optional text shaper. When `None`, [`Node::Text`] leaves are silently skipped (matches the legacy
    /// behaviour when the offscreen plugin is built without a shaper).
    pub shaper: Option<&'a mut dyn lumen_text::TextShaper>,
    /// Running transform pushed by ancestor [`Node::Transform`] frames. Composed by post-multiplication -
    /// children inherit ancestor coordinate frames, same as Qt SG / GSK.
    pub transform: Affine,
    /// Device pixel ratio. Multiplied into every leaf's origin / size /
    /// font size / radius / shadow blur before the emit helper hands the
    /// scaled values to vello. Lumen layout-taffy outputs LOGICAL pixels;
    /// the vello surface texture is sized in PHYSICAL pixels. Without this
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
    /// physical-pixel-sized vello surface, callers should use
    /// [`Self::new_with_dpr`] instead so logical coords scale to physical
    /// at the root.
    pub fn new(
        scene: &'a mut vello::Scene,
        cache: Option<&'a mut SceneFragmentCache>,
        shaper: Option<&'a mut dyn lumen_text::TextShaper>,
    ) -> Self {
        Self {
            scene,
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
    /// pixels in the vello surface. Pass the window's `scale_factor`
    /// (a.k.a. device-pixel ratio). Without this scale, hi-DPI surfaces
    /// only fill the top-left `1/dpr x 1/dpr` of the window - the
    /// logical 1280-wide layout lands at physical pixels 0..1280 of a
    /// 2560-wide surface, leaving the right + bottom quadrants dark.
    pub fn new_with_dpr(
        scene: &'a mut vello::Scene,
        cache: Option<&'a mut SceneFragmentCache>,
        shaper: Option<&'a mut dyn lumen_text::TextShaper>,
        dpr: f32,
    ) -> Self {
        Self {
            scene,
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

/// Converts the walker's running vello transform back into the IR's affine, which is what a
/// painter is handed - painters see logical coordinates and the device scale, not vello types.
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
    use vello::peniko::kurbo::RoundedRectRadii;
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

/// Pushes a vello layer matching the given [`ClipShape`]. Returns the number of pops the caller owes.
fn push_clip_layer(scene: &mut vello::Scene, shape: ClipShape, opacity: f32) -> u32 {
    let alpha = opacity.clamp(0.0, 1.0);
    match shape {
        ClipShape::Rect(r) => {
            scene.push_layer(
                Fill::NonZero,
                peniko::BlendMode::default(),
                alpha,
                Affine::IDENTITY,
                &lumen_rect_to_kurbo(r),
            );
        }
        ClipShape::RoundedRect { rect, radii } => {
            scene.push_layer(
                Fill::NonZero,
                peniko::BlendMode::default(),
                alpha,
                Affine::IDENTITY,
                &lumen_rect_to_rounded(rect, radii),
            );
        }
    }
    1
}

/// Walks a single [`Node`] subtree onto the target scene.
///
/// Each variant maps onto vello primitives the same way the legacy `Extracted*` emitters did, plus the new
/// Container/Transform/Opacity/Clip composition primitives. Leaves go through the supplied
/// [`SceneFragmentCache`] when one is present; structural variants compose along the recursion.
pub fn walk_node(ctx: &mut WalkContext<'_>, node: &Node) {
    match node {
        Node::Container { children } => {
            for child in children {
                walk_node(ctx, child);
            }
        }
        Node::Transform { matrix, child } => {
            // Push the transform onto our running matrix; vello applies the running matrix lazily inside
            // the emitter helpers via Scene::push_layer + Affine. For wave 2 we re-route through a clip
            // layer with the identity-clip rect to honour the transform without re-encoding every leaf -
            // matches Qt's `QSGTransformNode` which is a stack push, not a per-leaf transform.
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
            let pops = push_clip_layer(ctx.scene, scale_clip_shape(*shape, ctx.dpr), ctx.opacity);
            ctx.clips.push(pops);
            walk_node(ctx, child);
            let to_pop = ctx.clips.pop();
            for _ in 0..to_pop {
                ctx.scene.pop_layer();
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
            // pixels in the vello surface.
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
                emit_rect_cached(ctx.scene, cache, &cmd);
            } else {
                emit_rect(ctx.scene, &cmd);
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
            let color = crate::folded(*color, ctx.opacity);
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
                emit_shadow_cached(ctx.scene, cache, &cmd);
            } else {
                emit_shadow(ctx.scene, &cmd);
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
            let color = crate::folded(*color, ctx.opacity);
            let side_colors = side_colors.map(|cs| cs.map(|c| crate::folded(c, ctx.opacity)));
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
            crate::emit_border(ctx.scene, &cmd);
        }
        Node::Outline {
            origin,
            size,
            stroke,
            width,
            radius,
        } => {
            let stroke = crate::folded(*stroke, ctx.opacity);
            let cmd = ExtractedOutline {
                origin: *origin * ctx.dpr,
                size: *size * ctx.dpr,
                stroke,
                width: *width * ctx.dpr,
                radius: *radius * ctx.dpr,
                order: 0,
            };
            if let Some(cache) = ctx.cache.as_deref_mut() {
                emit_outline_cached(ctx.scene, cache, &cmd);
            } else {
                emit_outline(ctx.scene, &cmd);
            }
        }
        Node::Text { run } => {
            // Pass the run by reference; `draw_text_into_vello` folds in
            // ctx.dpr (origin / font size / container width) and ctx.opacity
            // (fill alpha) locally, so no per-node clone of the run - and its
            // owned String - is needed. caret + selection stay byte offsets
            // into the source string and are not scaled.
            if let Some(shaper) = ctx.shaper.as_deref_mut() {
                draw_text_into_vello(shaper, ctx.scene, run, ctx.dpr, ctx.opacity);
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
            if let Some(blob) = blob {
                if let Some(b) = blob.downcast_ref::<lumen_assets::ExtractedImageBlob>() {
                    draw_image_into_vello(ctx.scene, &img, b);
                }
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
                emit_svg(ctx.scene, &svg);
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
            let pops = if *clip_to_bounds {
                let shape = scale_clip_shape(ClipShape::Rect(*bounds), ctx.dpr);
                push_clip_layer(ctx.scene, shape, ctx.opacity)
            } else {
                0
            };
            let transform = affine_to_affine2(ctx.transform);
            let (dpr, opacity) = (ctx.dpr, ctx.opacity);
            let mut paint_ctx = NativePaintCtx::new(
                payload.as_ref(),
                &mut *ctx.scene,
                crate::BACKEND_ID,
                *bounds,
                transform,
                dpr,
                opacity,
            );
            painter.paint(&mut paint_ctx);
            for _ in 0..pops {
                ctx.scene.pop_layer();
            }
        }
    }
}

/// Walks the [`RetainedScene`] root into the supplied scene.
///
/// Convenience entry point - equivalent to `walk_node(&ctx, &scene.root)` once the root is known to exist.
pub fn walk_retained_scene(ctx: &mut WalkContext<'_>, scene: &RetainedScene) {
    if let Some(root) = scene.root.as_ref() {
        walk_node(ctx, root);
    }
}

/// Wave 2 structural diff - emits damage rects into `damage` covering subtrees that differ between `prev`
/// and `curr`. Recursion proceeds in lockstep with `Arc::ptr_eq` short-circuit: identical Arc-shared subtrees
/// contribute no damage. When a subtree was deleted, the OLD bounds are accumulated; when inserted, the NEW
/// bounds are accumulated; when both sides exist but differ, the union of their bounds is recorded (the
/// changed region covers both the old paint and the new paint).
///
/// Containers diff position-by-position. When lengths differ, the tail (extra children on one side) is
/// added to damage as deletions/insertions. When child positions diverge structurally (e.g. siblings
/// reordered around an insertion), the per-position walk overestimates - that overestimate is bounded by the
/// involved subtrees' bounds and is still smaller than today's whole-viewport fallback.
///
/// Renderers can read [`FrameDamage::is_empty`] to skip submit entirely, or feed the rect list into a
/// scissor box when the backend supports it (vello 0.8 still lacks a scissor API; the rect list still gates
/// the encode pass).
pub fn diff_retained_scenes(
    prev: Option<&Arc<Node>>,
    curr: Option<&Arc<Node>>,
    viewport: LumenRect,
    damage: &mut FrameDamage,
) {
    match (prev, curr) {
        (None, None) => {}
        (None, Some(c)) => {
            // Whole new tree appeared; damage its bounds.
            push_rect(damage, node_bounds(c, viewport, Affine::IDENTITY));
        }
        (Some(p), None) => {
            // Whole tree removed; damage the prior bounds.
            push_rect(damage, node_bounds(p, viewport, Affine::IDENTITY));
        }
        (Some(p), Some(c)) => {
            diff_node(p, c, viewport, Affine::IDENTITY, damage);
        }
    }
}

/// Lockstep recursive diff. `xform` is the running ancestor transform - for nested `Node::Transform` frames
/// the same matrix multiplies into both sides (we only enter `diff_node` for ptr-different subtrees that share
/// transforms only if their parameters match). When parameters diverge we punt to whole-subtree damage on both
/// sides.
fn diff_node(
    prev: &Arc<Node>,
    curr: &Arc<Node>,
    viewport: LumenRect,
    xform: Affine,
    damage: &mut FrameDamage,
) {
    if Arc::ptr_eq(prev, curr) {
        return;
    }
    match (prev.as_ref(), curr.as_ref()) {
        (Node::Container { children: pc }, Node::Container { children: cc }) => {
            // Walk pair-wise; on length mismatch the extra tail damages on whichever side has it.
            let common = pc.len().min(cc.len());
            for i in 0..common {
                diff_node(&pc[i], &cc[i], viewport, xform, damage);
            }
            if pc.len() > common {
                for child in &pc[common..] {
                    push_rect(damage, node_bounds(child, viewport, xform));
                }
            }
            if cc.len() > common {
                for child in &cc[common..] {
                    push_rect(damage, node_bounds(child, viewport, xform));
                }
            }
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
                let composed = compose_affine(xform, *cm);
                diff_node(pchild, cchild, viewport, composed, damage);
            } else {
                // Matrix changed - both old and new bounds differ; damage both sides under their respective
                // matrices.
                push_rect(
                    damage,
                    node_bounds(pchild, viewport, compose_affine(xform, *pm)),
                );
                push_rect(
                    damage,
                    node_bounds(cchild, viewport, compose_affine(xform, *cm)),
                );
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
                diff_node(pchild, cchild, viewport, xform, damage);
            } else {
                push_rect(damage, node_bounds(pchild, viewport, xform));
                push_rect(damage, node_bounds(cchild, viewport, xform));
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
                diff_node(pchild, cchild, viewport, xform, damage);
            } else {
                push_rect(damage, node_bounds(pchild, viewport, xform));
                push_rect(damage, node_bounds(cchild, viewport, xform));
            }
        }
        // Leaves: compare appearance. The producer rebuilds every leaf as a
        // fresh `Arc` each frame, so `Arc::ptr_eq` never matches across frames
        // - a purely structural (ptr / bounds) diff would therefore mark every
        // leaf dirty and defeat partial repaint entirely. Comparing the leaves'
        // visual fields lets an unchanged leaf contribute no damage, so damage
        // is proportional to what actually changed (GTK `gtk_widget_queue_draw`
        // / Qt `QWidget::update()` region accumulation).
        _ => {
            if leaf_visually_eq(prev, curr) {
                // Identical appearance and position - no damage.
            } else {
                let pb = node_bounds(prev, viewport, xform);
                let cb = node_bounds(curr, viewport, xform);
                if rect_eq(pb, cb) {
                    push_rect(damage, pb);
                } else {
                    push_rect(damage, pb);
                    push_rect(damage, cb);
                }
            }
        }
    }
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
        // The seam requires bounds that enclose every pixel the painter touches, so damage from a
        // native leaf is confined to them like any other leaf.
        Node::Native { bounds, .. } => apply_affine_to_rect(*bounds, xform),
    }
}

/// Compose ancestor running affine with a child Affine2 from the IR. Mirrors the multiplication used inside
/// `walk_node`'s Transform branch but lives at the diff layer because we don't need a vello scene to compute
/// bounds.
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
        let p = xform * vello::peniko::kurbo::Point::new(px, py);
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

fn rect_eq(a: LumenRect, b: LumenRect) -> bool {
    a.origin == b.origin && a.size == b.size
}

fn push_rect(damage: &mut FrameDamage, r: LumenRect) {
    if r.size.x > 0.0 && r.size.y > 0.0 {
        damage.push(r);
    }
}

/// Returns the smallest rect that encloses every damage rect. Useful for renderers that only support a single
/// scissor box. Returns `None` when the damage list is empty.
pub fn damage_union(damage: &FrameDamage) -> Option<LumenRect> {
    let mut iter = damage.0.iter();
    let first = *iter.next()?;
    let mut acc = first;
    for r in iter {
        acc = union(acc, *r);
    }
    Some(acc)
}

// Silence unused-import lints when downstream features pare back the emit set.
#[allow(dead_code)]
fn _suppress_unused_imports() {
    let _ = append_translated;
    let _ = emit_rect_into_fragment;
    let _ = emit_shadow_into_fragment;
    let _ = emit_outline_into_fragment;
    let _: Option<&ExtractedImage> = None;
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
