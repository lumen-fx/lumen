//! Retained Node IR - typed scene-graph tree produced from the flat `Extracted*` bag.
//!
//! ## Why a tree?
//!
//! The render side previously consumed a flat ECS of one-component-per-drawable, painter-sorted at submit
//! ([`crate::render_world::ExtractedRect`] / [`ExtractedText`] / [`ExtractedShadow`] / [`ExtractedOutline`]).
//! The flat shape lost the parent-driven invariants every modern scenegraph relies on - opacity composition,
//! transform stacks, and clip regions all have to be reconstructed at submit. This module replaces that with
//! a typed [`Node`] tree owned per-frame by the render world.
//!
//! ## 1:1 mapping to Qt SceneGraph + GTK GSK
//!
//! The variants below were chosen to map directly onto the two reference scenegraphs - Qt 6.8 Scene Graph
//! (`QSG*`) and GTK 4 / GSK (`gtk_snapshot_*` / `GskRenderNode`). The renderer back-end only needs to know how
//! to translate each variant to its native equivalent.
//!
//! | Lumen [`Node`]    | Qt SceneGraph                                          | GTK 4 / GSK                                                          |
//! |-------------------|--------------------------------------------------------|----------------------------------------------------------------------|
//! | [`Node::Container`] | `QSGNode` (parent of children)                       | implicit container via `gtk_snapshot_push_*` / `pop` bracket          |
//! | [`Node::Transform`] | `QSGTransformNode::setMatrix`                        | `gtk_snapshot_push_transform` -> `GskTransformNode`                    |
//! | [`Node::Opacity`]   | `QSGOpacityNode::setOpacity`                         | `gtk_snapshot_push_opacity` -> `GskOpacityNode`                        |
//! | [`Node::Clip`] (rect)   | `QSGClipNode { isRectangular = true }` (scissor) | `gtk_snapshot_push_clip` -> `GskClipNode`                              |
//! | [`Node::Clip`] (radii)  | `QSGClipNode` + custom geometry                  | `gtk_snapshot_push_rounded_clip` -> `GskRoundedClipNode`               |
//! | [`Node::Rect`] (solid)  | `QSGSimpleRectNode`                              | `gtk_snapshot_append_color` -> `GskColorNode`                          |
//! | [`Node::Rect`] (gradient) | `QSGGeometryNode` + gradient `QSGMaterial`     | `GskLinearGradientNode` / `GskRadialGradientNode` / `GskConicGradientNode` |
//! | [`Node::Shadow`] (outer) | `QSGGeometryNode` + blur material               | `gtk_snapshot_append_outset_shadow` -> `GskOutsetShadowNode`           |
//! | [`Node::Shadow`] (inner) | (custom material)                                | `gtk_snapshot_append_inset_shadow` -> `GskInsetShadowNode`             |
//! | [`Node::Outline`]   | `QSGGeometryNode` (line list)                       | `GskBorderNode` (4-side uniform) or composed `GskColorNode`s          |
//! | [`Node::Text`]      | `QSGTextNode` (via `QSGRendererInterface::createTextNode`) | `gtk_snapshot_append_layout` -> `GskTextNode`                   |
//! | [`Node::Image`]     | `QSGSimpleTextureNode::setTexture` + `setSourceRect`| `gtk_snapshot_append_texture` -> `GskTextureNode`                      |
//! | [`Node::Native`]    | `QSGRenderNode`                                     | `GskGLShaderNode` / `gtk_snapshot_push_gl_shader`                     |
//!
//! ## Content sharing
//!
//! Children are held in `Arc<Node>` so identical subtrees can share storage across frames - the diff can
//! short-circuit via `Arc::ptr_eq` and the leaf-encoding [`crate::render_world::SceneFragmentCache`] becomes a
//! content-addressed layer on top.
//!
//! ## Wave 2 status
//!
//! - W2.1 ships the types + a `transform_extracted_to_nodes` system that walks the existing extract output and
//!   produces a [`RetainedScene`] each frame. The legacy `Extracted*` components stay in place so the existing
//!   render systems keep compiling during the migration.
//! - W2.2 wires the renderer walker (`lumen_render_wgpu::walk_node`) to consume [`RetainedScene`].
//! - W2.3 puts overflow clipping back on the rails via the [`Node::Clip`] variant - see the [`Clip`] doc-comment.
//! - W2.4 lets the offscreen render path reuse the same walker (and hence the [`crate::render_world::SceneFragmentCache`]).

use crate::components::{Color, ImageBlob, SvgPayload};
use crate::native::ExtractedNative;
use crate::render_world::{
    Brush, ExtractedBorder, ExtractedClipBox, ExtractedImage, ExtractedOutline, ExtractedRect,
    ExtractedScrollbar, ExtractedShadow, ExtractedText, PaintOrder, Rect,
};
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::Resource;
use glam::Vec2;
use std::any::Any;
use std::sync::Arc;

/// A 2D affine transform stored as `[a, b, c, d, e, f]` in column-major order - same convention as
/// `vello::kurbo::Affine` so back-ends can construct without conversion glue. The default is the identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    /// Row-major 2x3 coefficients: `[m11, m12, m21, m22, tx, ty]`. The identity is `[1, 0, 0, 1, 0, 0]`.
    pub coeffs: [f64; 6],
}

impl Affine2 {
    /// Identity transform.
    pub const IDENTITY: Affine2 = Affine2 {
        coeffs: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };

    /// Pure translation `(tx, ty)`.
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self {
            coeffs: [1.0, 0.0, 0.0, 1.0, tx, ty],
        }
    }

    /// Uniform scale by `s` about the origin.
    pub const fn scale(s: f64) -> Self {
        Self {
            coeffs: [s, 0.0, 0.0, s, 0.0, 0.0],
        }
    }
}

/// Composition: `outer * inner` applies `inner` first, matching `vello::kurbo::Affine`.
impl std::ops::Mul for Affine2 {
    type Output = Affine2;

    fn mul(self, rhs: Affine2) -> Affine2 {
        let a = self.coeffs;
        let b = rhs.coeffs;
        Affine2 {
            coeffs: [
                a[0] * b[0] + a[2] * b[1],
                a[1] * b[0] + a[3] * b[1],
                a[0] * b[2] + a[2] * b[3],
                a[1] * b[2] + a[3] * b[3],
                a[0] * b[4] + a[2] * b[5] + a[4],
                a[1] * b[4] + a[3] * b[5] + a[5],
            ],
        }
    }
}

impl Default for Affine2 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Clip-region shape for [`Node::Clip`]. Rectangular clips map onto scissor on backends that support it
/// (Qt `QSGClipNode { isRectangular = true }`, GSK `GskClipNode`); rounded clips require stencil / mask
/// (Qt `QSGClipNode` + geometry, GSK `GskRoundedClipNode`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClipShape {
    /// Axis-aligned rectangle.
    Rect(Rect),
    /// Rounded rectangle with per-corner radii in `[top_left, top_right, bottom_right, bottom_left]` order
    /// (CSS shorthand). A single uniform radius is represented by all four entries equal.
    RoundedRect {
        /// Bounding rect.
        rect: Rect,
        /// Per-corner radii.
        radii: [f32; 4],
    },
}

impl ClipShape {
    /// Builds a [`ClipShape::RoundedRect`] when `radius > 0.0`, otherwise [`ClipShape::Rect`].
    pub fn from_rect_radius(rect: Rect, radius: f32) -> Self {
        if radius > 0.0 {
            Self::RoundedRect {
                rect,
                radii: [radius; 4],
            }
        } else {
            Self::Rect(rect)
        }
    }
}

impl From<&ExtractedClipBox> for ClipShape {
    fn from(c: &ExtractedClipBox) -> Self {
        let rect = Rect::new(c.origin, c.size);
        Self::from_rect_radius(rect, c.radius)
    }
}

/// One node in the retained scene-graph tree. The tree is produced each tick by
/// [`transform_extracted_to_nodes`] and rendered by the back-end walker.
///
/// Children - wherever they appear - are held as `Arc<Node>` so identical subtrees share storage and the
/// inter-frame diff can short-circuit on `Arc::ptr_eq`. Leaf variants own their parameters by value (cheap
/// copies, the inner brush already shares its stop array via `Arc<[...]>`).
#[derive(Clone)]
pub enum Node {
    /// Ordered list of children painted back-to-front. The tree root is always a [`Container`].
    ///
    /// [`Container`]: Node::Container
    Container {
        /// Ordered children (paint order = vec order).
        children: Vec<Arc<Node>>,
    },
    /// Affine transform pushed onto the back-end's transform stack. Wraps a single child subtree.
    ///
    /// Maps to `QSGTransformNode::setMatrix` / `gtk_snapshot_push_transform`.
    Transform {
        /// Affine matrix.
        matrix: Affine2,
        /// Child subtree.
        child: Arc<Node>,
    },
    /// Opacity multiplier pushed onto the back-end's compositing stack. Multiplies into the alpha of every
    /// descendant - including nested [`Opacity`] groups. Wraps a single child subtree.
    ///
    /// Maps to `QSGOpacityNode::setOpacity` / `gtk_snapshot_push_opacity`.
    ///
    /// [`Opacity`]: Node::Opacity
    Opacity {
        /// `[0.0, 1.0]` multiplier applied to descendant alpha.
        alpha: f32,
        /// Child subtree.
        child: Arc<Node>,
    },
    /// Clip region pushed onto the back-end's clip stack. Descendants are masked to `shape`. Wraps a single
    /// child subtree.
    ///
    /// Maps to `QSGClipNode` (scissor when rectangular, stencil when rounded) /
    /// `gtk_snapshot_push_clip` / `gtk_snapshot_push_rounded_clip`. Authored from
    /// `overflow: hidden` containers and `<scroll>` viewports.
    Clip {
        /// Clip-region shape.
        shape: ClipShape,
        /// Child subtree.
        child: Arc<Node>,
    },
    /// Filled rectangle leaf - solid or gradient.
    ///
    /// Maps to `QSGSimpleRectNode` / `GskColorNode` / gradient nodes.
    Rect {
        /// Bounding rect in window coordinates.
        bounds: Rect,
        /// Fill brush.
        brush: Brush,
        /// Uniform corner radius. `0.0` = sharp.
        corner: f32,
        /// Per-corner radii `[tl, tr, br, bl]`; `None` = uniform `corner`.
        corners: Option<[f32; 4]>,
    },
    /// Shadow leaf - outer drop shadow or inset shadow.
    ///
    /// Outer maps to `gtk_snapshot_append_outset_shadow` / `QSGGeometryNode` + blur material;
    /// inner maps to `gtk_snapshot_append_inset_shadow`.
    Shadow {
        /// Top-left in window coordinates.
        origin: Vec2,
        /// Source rect size.
        size: Vec2,
        /// Corner radius.
        radius: f32,
        /// CSS spread radius (inflates / deflates the rect pre-blur).
        spread: f32,
        /// Gaussian blur std-dev.
        blur: f32,
        /// Shadow color.
        color: Color,
        /// `true` for an inset shadow (clipped to the source rect, drawn at the negated offset).
        inner: bool,
        /// Source rect top-left without the per-shadow offset. Used by inset shadows for the clip rect and
        /// to flip the offset; ignored for outer shadows.
        rect_origin: Vec2,
    },
    /// CSS border leaf - the ring between the border box and the padding
    /// box, filled with one solid color. Per-side widths supported;
    /// distinct from [`Node::Outline`], which strokes centered on the box
    /// edge and never affects layout.
    ///
    /// Maps to `GskBorderNode` / a `QSGGeometryNode` ring.
    Border {
        /// Border-box top-left in window coordinates.
        origin: Vec2,
        /// Border-box size.
        size: Vec2,
        /// Per-side widths `[top, right, bottom, left]`.
        widths: [f32; 4],
        /// Solid border color.
        color: Color,
        /// Per-side color overrides `[top, right, bottom, left]`.
        side_colors: Option<[Color; 4]>,
        /// Outer corner radius.
        radius: f32,
        /// Per-corner outer radii `[tl, tr, br, bl]`; `None` = uniform.
        corners: Option<[f32; 4]>,
    },
    /// Stroked outline leaf - typically a focus ring.
    ///
    /// Maps to `QSGGeometryNode` (line list) / `GskBorderNode`.
    Outline {
        /// Top-left in window coordinates.
        origin: Vec2,
        /// Box size being outlined.
        size: Vec2,
        /// Stroke color.
        stroke: Color,
        /// Stroke width.
        width: f32,
        /// Uniform corner radius (matches the outlined box).
        radius: f32,
    },
    /// Text leaf - one shaped run for now. Future BiDi rewrite (wave 5) lifts this to a `Vec<ShapedRunRef>`.
    ///
    /// Maps to `QSGTextNode` / `GskTextNode`. The leaf carries the *unshaped* string + style; the renderer
    /// drives the shaper because text shaping is `&mut TextShaper`-bound and can't sit in an immutable IR.
    Text {
        /// Wrapped legacy [`ExtractedText`] - keeps the field set stable while wave 2 ships the IR.
        /// Wave 5 BiDi rewrites this to a `Vec<ShapedRunRef>` + baseline.
        run: ExtractedText,
    },
    /// Raster image leaf.
    ///
    /// Maps to `QSGSimpleTextureNode` / `GskTextureNode`. Wraps the legacy [`ExtractedImage`] payload so the
    /// existing pipeline keeps its blob-identity GPU upload cache.
    Image {
        /// Wrapped legacy [`ExtractedImage`].
        image: ExtractedImage,
        /// Opaque blob carrier - typically `Arc<lumen_assets::ExtractedImageBlob>`. `None` when the renderer
        /// doesn't need a separate blob (e.g. headless).
        blob: Option<Arc<dyn Any + Send + Sync>>,
    },
    /// Vector image leaf (SVG pre-rendered into a vello sub-scene).
    ///
    /// Maps to `QQuickSvgItem` -> SG subtree / `GskCairoNode` (or a pre-baked `GskTextureNode`).
    /// Stored as an opaque `Arc<dyn Any + Send + Sync>` so `lumen-core` doesn't need to depend on `vello`.
    Svg {
        /// Type-erased SVG payload. The concrete type is typically `Arc<lumen_assets::ExtractedSvg>`.
        payload: Arc<dyn Any + Send + Sync>,
    },
    /// Backend-painted leaf contributed by a plugin - see [`crate::native`] for the seam.
    ///
    /// Maps to `QSGRenderNode` / `GskGLShaderNode`. The backend looks up a
    /// [`crate::native::NativePainter`] by `extension_id` and hands it the payload plus its own
    /// draw target; an unregistered id paints nothing.
    Native {
        /// String identifier for the native extension (e.g. `"lumen.native.wgpu"`).
        extension_id: Arc<str>,
        /// Opaque payload the back-end downcasts.
        payload: Arc<dyn Any + Send + Sync>,
        /// Bounding rect in window coordinates - encloses every pixel the painter touches.
        bounds: Rect,
        /// Content stamp; equal revisions mean identical pixels.
        revision: u64,
        /// Clip the painter to `bounds`.
        clip_to_bounds: bool,
    },
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Node::Container { children } => f
                .debug_struct("Container")
                .field("children", &children.len())
                .finish(),
            Node::Transform { matrix, .. } => {
                f.debug_struct("Transform").field("matrix", matrix).finish()
            }
            Node::Opacity { alpha, .. } => f.debug_struct("Opacity").field("alpha", alpha).finish(),
            Node::Clip { shape, .. } => f.debug_struct("Clip").field("shape", shape).finish(),
            Node::Rect { bounds, corner, .. } => f
                .debug_struct("Rect")
                .field("bounds", bounds)
                .field("corner", corner)
                .finish(),
            Node::Shadow {
                origin,
                size,
                inner,
                ..
            } => f
                .debug_struct("Shadow")
                .field("origin", origin)
                .field("size", size)
                .field("inner", inner)
                .finish(),
            Node::Border {
                origin,
                size,
                widths,
                ..
            } => f
                .debug_struct("Border")
                .field("origin", origin)
                .field("size", size)
                .field("widths", widths)
                .finish(),
            Node::Outline { origin, size, .. } => f
                .debug_struct("Outline")
                .field("origin", origin)
                .field("size", size)
                .finish(),
            Node::Text { run } => f
                .debug_struct("Text")
                .field("len", &run.text.len())
                .finish(),
            Node::Image { image, .. } => f
                .debug_struct("Image")
                .field("origin", &image.origin)
                .field("size", &image.size)
                .finish(),
            Node::Svg { .. } => f.debug_struct("Svg").finish(),
            Node::Native {
                extension_id,
                bounds,
                revision,
                ..
            } => f
                .debug_struct("Native")
                .field("ext", extension_id)
                .field("bounds", bounds)
                .field("revision", revision)
                .finish(),
        }
    }
}

impl From<&ExtractedRect> for Node {
    fn from(r: &ExtractedRect) -> Self {
        Node::Rect {
            bounds: Rect::new(r.origin, r.size),
            brush: r.brush.clone(),
            corner: r.radius,
            corners: r.corner_radii,
        }
    }
}

impl From<&ExtractedShadow> for Node {
    fn from(s: &ExtractedShadow) -> Self {
        Node::Shadow {
            origin: s.origin,
            size: s.size,
            radius: s.radius,
            spread: s.spread,
            blur: s.blur,
            color: s.color,
            inner: s.inner,
            rect_origin: s.rect_origin,
        }
    }
}

impl From<&ExtractedBorder> for Node {
    fn from(b: &ExtractedBorder) -> Self {
        Node::Border {
            origin: b.origin,
            size: b.size,
            widths: b.widths,
            color: b.color,
            side_colors: b.side_colors,
            radius: b.radius,
            corners: b.corner_radii,
        }
    }
}

impl From<&ExtractedOutline> for Node {
    fn from(o: &ExtractedOutline) -> Self {
        Node::Outline {
            origin: o.origin,
            size: o.size,
            stroke: o.stroke,
            width: o.width,
            radius: o.radius,
        }
    }
}

impl From<&ExtractedText> for Node {
    fn from(t: &ExtractedText) -> Self {
        Node::Text { run: t.clone() }
    }
}

impl From<&ExtractedImage> for Node {
    fn from(i: &ExtractedImage) -> Self {
        Node::Image {
            image: i.clone(),
            blob: None,
        }
    }
}

impl From<(&ExtractedImage, &ImageBlob)> for Node {
    fn from((i, b): (&ExtractedImage, &ImageBlob)) -> Self {
        Node::Image {
            image: i.clone(),
            blob: Some(b.0.clone()),
        }
    }
}

impl From<&SvgPayload> for Node {
    fn from(s: &SvgPayload) -> Self {
        Node::Svg {
            payload: s.payload.clone(),
        }
    }
}

impl From<&ExtractedNative> for Node {
    fn from(n: &ExtractedNative) -> Self {
        Node::Native {
            extension_id: n.extension_id.clone(),
            payload: n.payload.clone(),
            bounds: n.bounds,
            revision: n.revision,
            clip_to_bounds: n.clip_to_bounds,
        }
    }
}

/// One drawable entry produced during extract, sorted by [`PaintOrder`] before tree assembly.
#[derive(Clone)]
pub enum DrawEntry {
    /// Rect leaf.
    Rect(Arc<Node>),
    /// Shadow leaf.
    Shadow(Arc<Node>),
    /// Border leaf.
    Border(Arc<Node>),
    /// Outline leaf.
    Outline(Arc<Node>),
    /// Text leaf.
    Text(Arc<Node>),
    /// Image leaf.
    Image(Arc<Node>),
    /// Svg leaf.
    Svg(Arc<Node>),
    /// Push-clip marker - pairs with a later [`DrawEntry::PopClip`] at the same logical depth.
    PushClip(ClipShape),
    /// Pop-clip marker - matches the most recent [`DrawEntry::PushClip`].
    PopClip,
}

/// The retained scene-graph root for the current frame.
///
/// Holds the root [`Node`] (always a [`Node::Container`]) plus an opportunistic content-sharing pool keyed
/// by appearance - wave 2 only wires this loosely; later waves add proper cache lookup on the producer side.
#[derive(Resource, Default)]
pub struct RetainedScene {
    /// Root container - `None` until the first [`transform_extracted_to_nodes`] tick.
    pub root: Option<Arc<Node>>,
}

impl std::fmt::Debug for RetainedScene {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetainedScene")
            .field("has_root", &self.root.is_some())
            .finish()
    }
}

/// Snapshot of the previous tick's [`RetainedScene`]. Stored on the render world so the renderer can diff
/// `Arc::ptr_eq` between corresponding subtrees and emit damage rects into [`crate::render_world::FrameDamage`].
///
/// Wave 2 stores the root only; the depth-first diff lives in the back-end walker.
#[derive(Resource, Default)]
pub struct PreviousScene {
    /// Root of the prior tick's tree. `None` for the first frame.
    pub root: Option<Arc<Node>>,
}

impl std::fmt::Debug for PreviousScene {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviousScene")
            .field("has_root", &self.root.is_some())
            .finish()
    }
}

/// Sort key for ordering [`DrawEntry`] before tree assembly.
type EntryOrder = PaintOrder;

/// Builds a [`RetainedScene`] from the flat `Extracted*` components in the render world.
///
/// Walks every `ExtractedRect`/`Shadow`/`Outline`/`Text`/`Image`/`Svg`/`Native`/`ClipBox` once, sorts the leaves by
/// [`PaintOrder`], folds the clip pairs around the leaves they enclose, and produces an `Arc<Node::Container>`
/// containing the painter-ordered children. The previous frame's root is moved into [`PreviousScene`] so the
/// walker can diff against it.
///
/// Runs in [`crate::render_world::RenderStage::Prepare`] (registered by `App::new`).
#[allow(clippy::too_many_arguments)]
pub fn transform_extracted_to_nodes(
    mut retained: bevy_ecs::system::ResMut<RetainedScene>,
    mut previous: bevy_ecs::system::ResMut<PreviousScene>,
    rects: bevy_ecs::system::Query<&ExtractedRect>,
    borders: bevy_ecs::system::Query<&ExtractedBorder>,
    shadows: bevy_ecs::system::Query<&ExtractedShadow>,
    outlines: bevy_ecs::system::Query<&ExtractedOutline>,
    texts: bevy_ecs::system::Query<&ExtractedText>,
    images: bevy_ecs::system::Query<(&ExtractedImage, Option<&ImageBlob>)>,
    svgs: bevy_ecs::system::Query<&SvgPayload>,
    natives: bevy_ecs::system::Query<(bevy_ecs::entity::Entity, &ExtractedNative)>,
    clips: bevy_ecs::system::Query<&ExtractedClipBox>,
    scrollbars: bevy_ecs::system::Query<&ExtractedScrollbar>,
) {
    // Park the prior frame's root in PreviousScene so the walker / damage diff can compare ptr-equal subtrees.
    previous.root = retained.root.take();

    // Collect leaves sorted by PaintOrder. Each leaf is wrapped in an Arc<Node>.
    let mut entries: Vec<(EntryOrder, Arc<Node>)> = Vec::with_capacity(
        rects.iter().len()
            + borders.iter().len()
            + shadows.iter().len()
            + outlines.iter().len()
            + texts.iter().len()
            + images.iter().len()
            + svgs.iter().len()
            + natives.iter().len(),
    );
    for r in &rects {
        entries.push((r.order, Arc::new(Node::from(r))));
    }
    // Borders share the entity's own order key with its background rect;
    // pushing them after rects keeps `background -> border` paint order
    // through the stable sort below.
    for b in &borders {
        entries.push((b.order, Arc::new(Node::from(b))));
    }
    for s in &shadows {
        entries.push((s.order, Arc::new(Node::from(s))));
    }
    for o in &outlines {
        entries.push((o.order, Arc::new(Node::from(o))));
    }
    for t in &texts {
        entries.push((t.order, Arc::new(Node::from(t))));
    }
    for (i, maybe_blob) in &images {
        // Splice the type-erased blob payload (set by lumen-assets in its extract pass) directly
        // into Node::Image.blob - the renderer walker downcasts it back. Closes the loop from the
        // round-4 W36 / W39 deferral note: the on-screen path previously spliced blobs via a
        // window-winit auxiliary loop because lumen-core couldn't see lumen-assets' blob type.
        let node = match maybe_blob {
            Some(blob) => Node::from((i, blob)),
            None => Node::from(i),
        };
        entries.push((i.order, Arc::new(node)));
    }
    for s in &svgs {
        entries.push((s.order, Arc::new(Node::from(s))));
    }
    // Plugin-painted leaves. Query iteration follows archetype order, which
    // shifts as plugins add and drop components, so sort on a key that is
    // total: paint order, then extension id, then the render entity that
    // carries the leaf. The entity is stable across frames (the extract
    // helper reuses it), so one extension's several leaves at a shared order
    // also keep their relative order from frame to frame.
    let mut native_leaves: Vec<(Entity, &ExtractedNative)> = natives.iter().collect();
    native_leaves.sort_by(|(ae, a), (be, b)| {
        a.order
            .cmp(&b.order)
            .then_with(|| a.extension_id.cmp(&b.extension_id))
            .then_with(|| ae.cmp(be))
    });
    for (_, n) in native_leaves {
        entries.push((n.order, Arc::new(Node::from(n))));
    }
    // Overlay scrollbars: pushed LAST so the stable sort keeps them
    // after any other leaf sharing their paint-order key, and `draws`
    // order (track -> thumb) is preserved within the bar.
    for sb in &scrollbars {
        for d in &sb.draws {
            entries.push((
                sb.order,
                Arc::new(Node::Rect {
                    bounds: Rect::new(d.origin, d.size),
                    brush: Brush::Solid(d.color),
                    corner: d.radius,
                    corners: None,
                }),
            ));
        }
    }
    entries.sort_by_key(|(k, _)| *k);

    // Collect clip ranges sorted by start_order so the assembly loop can bracket leaves with the right
    // push/pop sequence. A clip wraps every leaf whose order is in `[start_order, end_order]`.
    let mut clip_ranges: Vec<(PaintOrder, PaintOrder, ClipShape)> = clips
        .iter()
        .map(|c| (c.start_order, c.end_order, ClipShape::from(c)))
        .collect();
    clip_ranges.sort_by_key(|(s, _, _)| *s);

    // Single pass: at each leaf, close any open clips whose end has passed, then open any clips whose
    // start matches the leaf's order. The result is a flat children Vec carrying the painter-ordered leaves
    // wrapped in Clip subtrees.
    let mut next_clip = 0usize;
    let mut open_clips: Vec<(PaintOrder, ClipShape, Vec<Arc<Node>>)> = Vec::new();
    let mut roots: Vec<Arc<Node>> = Vec::new();

    fn flush_open(
        open_clips: &mut Vec<(PaintOrder, ClipShape, Vec<Arc<Node>>)>,
        roots: &mut Vec<Arc<Node>>,
        until_order: PaintOrder,
    ) {
        while let Some((end, _, _)) = open_clips.last() {
            if *end >= until_order {
                break;
            }
            let (_, shape, children) = open_clips.pop().expect("checked above");
            let container = Arc::new(Node::Container { children });
            let clip_node = Arc::new(Node::Clip {
                shape,
                child: container,
            });
            push_into(open_clips, roots, clip_node);
        }
    }

    fn push_into(
        open_clips: &mut [(PaintOrder, ClipShape, Vec<Arc<Node>>)],
        roots: &mut Vec<Arc<Node>>,
        child: Arc<Node>,
    ) {
        if let Some(top) = open_clips.last_mut() {
            top.2.push(child);
        } else {
            roots.push(child);
        }
    }

    for (order, leaf) in entries {
        // Close finished clips first.
        flush_open(&mut open_clips, &mut roots, order);
        // Open any new clips that start at/before this leaf.
        while next_clip < clip_ranges.len() && clip_ranges[next_clip].0 <= order {
            let (_, end, shape) = clip_ranges[next_clip];
            next_clip += 1;
            // A range that already ended holds no leaf of its own: an empty
            // container clips nothing. Opening it here would wrap this leaf,
            // which sits past the range, in a clip it is not inside.
            if end < order {
                continue;
            }
            open_clips.push((end, shape, Vec::new()));
        }
        push_into(&mut open_clips, &mut roots, leaf);
    }
    // Drain any remaining clips.
    while let Some((_, shape, children)) = open_clips.pop() {
        let container = Arc::new(Node::Container { children });
        let clip_node = Arc::new(Node::Clip {
            shape,
            child: container,
        });
        if let Some(top) = open_clips.last_mut() {
            top.2.push(clip_node);
        } else {
            roots.push(clip_node);
        }
    }

    retained.root = Some(Arc::new(Node::Container { children: roots }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Color;
    use crate::render_world::{OVERLAY_ORDER_BASE, ScrollbarDrawRect};

    fn solid_rect(order: PaintOrder, origin: Vec2, size: Vec2) -> ExtractedRect {
        ExtractedRect {
            origin,
            size,
            brush: Brush::Solid(Color::rgba(1.0, 0.0, 0.0, 1.0)),
            radius: 0.0,
            corner_radii: None,
            order,
        }
    }

    #[test]
    fn node_from_rect_round_trips_fields() {
        let r = solid_rect(0, Vec2::new(1.0, 2.0), Vec2::new(10.0, 20.0));
        let node = Node::from(&r);
        match node {
            Node::Rect { bounds, corner, .. } => {
                assert_eq!(bounds.origin, Vec2::new(1.0, 2.0));
                assert_eq!(bounds.size, Vec2::new(10.0, 20.0));
                assert_eq!(corner, 0.0);
            }
            _ => panic!("expected Rect"),
        }
    }

    /// R-css-flex: at the shared paint-order key, the background rect
    /// assembles before the border leaf (CSS background -> border), and
    /// both come before higher-order (descendant) leaves.
    #[test]
    fn border_assembles_after_rect_at_same_order() {
        let mut retained = RetainedScene::default();
        let mut previous = PreviousScene::default();
        let mut world = bevy_ecs::world::World::new();
        world.spawn(solid_rect(4, Vec2::ZERO, Vec2::new(10.0, 10.0)));
        world.spawn(ExtractedBorder {
            origin: Vec2::ZERO,
            size: Vec2::new(10.0, 10.0),
            widths: [1.0; 4],
            color: Color::rgba(0.0, 0.0, 1.0, 1.0),
            side_colors: None,
            radius: 0.0,
            corner_radii: None,
            order: 4,
        });
        world.spawn(solid_rect(6, Vec2::ZERO, Vec2::new(4.0, 4.0)));

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(transform_extracted_to_nodes);
        world.insert_resource(std::mem::take(&mut retained));
        world.insert_resource(std::mem::take(&mut previous));
        schedule.run(&mut world);

        let retained = world.resource::<RetainedScene>();
        let root = retained.root.as_ref().expect("root");
        let Node::Container { children } = root.as_ref() else {
            panic!("root is a container");
        };
        assert_eq!(children.len(), 3);
        assert!(matches!(children[0].as_ref(), Node::Rect { .. }));
        assert!(matches!(children[1].as_ref(), Node::Border { .. }));
        assert!(matches!(children[2].as_ref(), Node::Rect { .. }));
    }

    fn native(
        order: PaintOrder,
        extension_id: &str,
        origin: Vec2,
        revision: u64,
    ) -> ExtractedNative {
        ExtractedNative {
            extension_id: extension_id.into(),
            payload: Arc::new(revision),
            bounds: Rect::new(origin, Vec2::new(30.0, 30.0)),
            order,
            revision,
            clip_to_bounds: false,
        }
    }

    fn assemble(world: &mut bevy_ecs::world::World) -> Arc<Node> {
        world.insert_resource(RetainedScene::default());
        world.insert_resource(PreviousScene::default());
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(transform_extracted_to_nodes);
        schedule.run(world);
        world
            .resource::<RetainedScene>()
            .root
            .as_ref()
            .expect("root")
            .clone()
    }

    fn children_of(node: &Arc<Node>) -> Vec<Arc<Node>> {
        match node.as_ref() {
            Node::Container { children } => children.clone(),
            other => panic!("expected a container, got {other:?}"),
        }
    }

    /// A plugin's leaf reaches the tree with the geometry and content stamp it
    /// was extracted with: the renderer positions and repaints it from those,
    /// not from anything inside the opaque payload.
    #[test]
    fn a_native_leaf_carries_its_bounds_and_revision_into_the_tree() {
        let mut world = bevy_ecs::world::World::new();
        let mut leaf = native(4, "demo.chart", Vec2::new(12.0, 8.0), 77);
        leaf.clip_to_bounds = true;
        world.spawn(leaf);

        let children = children_of(&assemble(&mut world));
        assert_eq!(children.len(), 1);
        match children[0].as_ref() {
            Node::Native {
                extension_id,
                bounds,
                revision,
                clip_to_bounds,
                ..
            } => {
                assert_eq!(&**extension_id, "demo.chart");
                assert_eq!(bounds.origin, Vec2::new(12.0, 8.0));
                assert_eq!(*revision, 77);
                assert!(clip_to_bounds);
            }
            other => panic!("expected Native, got {other:?}"),
        }
    }

    /// At one entity's paint-order key the leaf paints over that entity's own
    /// background and border, the way a canvas sits inside the box that styles
    /// it.
    #[test]
    fn a_native_leaf_paints_over_the_box_it_sits_in() {
        let mut world = bevy_ecs::world::World::new();
        world.spawn(solid_rect(4, Vec2::ZERO, Vec2::new(40.0, 40.0)));
        world.spawn(ExtractedBorder {
            origin: Vec2::ZERO,
            size: Vec2::new(40.0, 40.0),
            widths: [1.0; 4],
            color: Color::rgba(0.0, 0.0, 1.0, 1.0),
            side_colors: None,
            radius: 0.0,
            corner_radii: None,
            order: 4,
        });
        world.spawn(native(4, "demo.chart", Vec2::ZERO, 1));

        let children = children_of(&assemble(&mut world));
        assert_eq!(children.len(), 3);
        assert!(matches!(children[0].as_ref(), Node::Rect { .. }));
        assert!(matches!(children[1].as_ref(), Node::Border { .. }));
        assert!(matches!(children[2].as_ref(), Node::Native { .. }));
    }

    /// Two extensions contributing at the same paint-order key stack by
    /// `extension_id`, so the frame does not reorder itself as archetypes churn.
    #[test]
    fn native_leaves_at_one_order_stack_by_extension_id() {
        let mut world = bevy_ecs::world::World::new();
        world.spawn(native(4, "zeta.overlay", Vec2::ZERO, 1));
        world.spawn(native(4, "alpha.grid", Vec2::ZERO, 1));

        let children = children_of(&assemble(&mut world));
        let ids: Vec<String> = children
            .iter()
            .map(|c| match c.as_ref() {
                Node::Native { extension_id, .. } => extension_id.to_string(),
                other => panic!("expected Native, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, ["alpha.grid", "zeta.overlay"]);
    }

    /// An enclosing `overflow: hidden` container clips a plugin's leaf the same
    /// way it clips a rect, so a scrolled-out chart does not paint over its
    /// container's edge.
    #[test]
    fn an_enclosing_clip_brackets_a_native_leaf() {
        let mut world = bevy_ecs::world::World::new();
        world.spawn(ExtractedClipBox {
            origin: Vec2::new(5.0, 5.0),
            size: Vec2::new(50.0, 50.0),
            radius: 0.0,
            start_order: 4,
            end_order: 8,
        });
        world.spawn(native(6, "demo.chart", Vec2::ZERO, 1));

        let children = children_of(&assemble(&mut world));
        assert_eq!(children.len(), 1);
        let Node::Clip { child, .. } = children[0].as_ref() else {
            panic!("expected the leaf wrapped in a clip, got {:?}", children[0]);
        };
        let inner = children_of(child);
        assert!(matches!(inner[0].as_ref(), Node::Native { .. }));
    }

    /// An empty container's clip range covers no leaf, so the next leaf after
    /// it stays outside: a hidden or childless `overflow: hidden` box must not
    /// clip the sibling that paints after it.
    #[test]
    fn an_empty_clip_range_leaves_the_following_leaf_alone() {
        let mut world = bevy_ecs::world::World::new();
        world.spawn(ExtractedClipBox {
            origin: Vec2::new(5.0, 5.0),
            size: Vec2::new(10.0, 10.0),
            radius: 0.0,
            start_order: 4,
            end_order: 4,
        });
        world.spawn(solid_rect(6, Vec2::ZERO, Vec2::new(40.0, 40.0)));

        let children = children_of(&assemble(&mut world));
        assert_eq!(children.len(), 1);
        assert!(
            matches!(children[0].as_ref(), Node::Rect { .. }),
            "expected an unclipped rect, got {:?}",
            children[0]
        );
    }

    /// The same holds for a range nested inside a live one: the leaf keeps the
    /// enclosing clip and picks up nothing from the empty range beside it.
    #[test]
    fn an_empty_clip_range_nested_in_a_live_one_wraps_nothing() {
        let mut world = bevy_ecs::world::World::new();
        world.spawn(ExtractedClipBox {
            origin: Vec2::ZERO,
            size: Vec2::new(80.0, 80.0),
            radius: 0.0,
            start_order: 1,
            end_order: 10,
        });
        world.spawn(ExtractedClipBox {
            origin: Vec2::new(2.0, 2.0),
            size: Vec2::new(4.0, 4.0),
            radius: 0.0,
            start_order: 3,
            end_order: 4,
        });
        world.spawn(solid_rect(5, Vec2::ZERO, Vec2::new(40.0, 40.0)));

        let children = children_of(&assemble(&mut world));
        assert_eq!(children.len(), 1);
        let Node::Clip { child, .. } = children[0].as_ref() else {
            panic!("expected the outer clip, got {:?}", children[0]);
        };
        let inner = children_of(child);
        assert_eq!(inner.len(), 1);
        assert!(
            matches!(inner[0].as_ref(), Node::Rect { .. }),
            "expected the rect directly under the outer clip, got {:?}",
            inner[0]
        );
    }

    /// The top-layer band lifts a plugin's leaf over all normal content, and
    /// overlay scrollbars still paint last of all.
    #[test]
    fn the_overlay_band_and_scrollbars_keep_their_places_around_a_native_leaf() {
        let mut world = bevy_ecs::world::World::new();
        world.spawn(solid_rect(6, Vec2::ZERO, Vec2::new(40.0, 40.0)));
        world.spawn(native(
            OVERLAY_ORDER_BASE + 2,
            "demo.tooltip",
            Vec2::ZERO,
            1,
        ));
        world.spawn(ExtractedScrollbar {
            draws: vec![ScrollbarDrawRect {
                origin: Vec2::new(90.0, 0.0),
                size: Vec2::new(6.0, 40.0),
                color: Color::rgba(0.0, 0.0, 0.0, 0.4),
                radius: 3.0,
            }],
            order: OVERLAY_ORDER_BASE + 2,
        });

        let children = children_of(&assemble(&mut world));
        assert_eq!(children.len(), 3);
        assert!(matches!(children[0].as_ref(), Node::Rect { .. }));
        assert!(matches!(children[1].as_ref(), Node::Native { .. }));
        assert!(matches!(children[2].as_ref(), Node::Rect { .. }));
    }

    #[test]
    fn clipshape_from_extracted_clipbox_routes_radius() {
        let sharp = ExtractedClipBox {
            origin: Vec2::ZERO,
            size: Vec2::new(10.0, 10.0),
            radius: 0.0,
            start_order: 0,
            end_order: 1,
        };
        let rounded = ExtractedClipBox {
            origin: Vec2::ZERO,
            size: Vec2::new(10.0, 10.0),
            radius: 5.0,
            start_order: 0,
            end_order: 1,
        };
        assert!(matches!(ClipShape::from(&sharp), ClipShape::Rect(_)));
        assert!(matches!(
            ClipShape::from(&rounded),
            ClipShape::RoundedRect { .. }
        ));
    }
}
