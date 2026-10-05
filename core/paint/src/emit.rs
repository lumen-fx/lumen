//! Leaf emitters: one extracted primitive to calls on a [`Painter`].
//!
//! The walker builds a transient `Extracted*` value per leaf, already scaled
//! to target pixels, and hands it to the matching emitter here. The `_cached`
//! variants encode a position-independent fragment once per appearance and
//! replay it at every origin when the sink supports fragments.

use crate::cache::{FragmentCache, FragmentKey};
use crate::{GlyphRun, Painter, Shape};
use lumen_core::components::Color as LumenColor;
use lumen_core::render_world::{
    DEFAULT_SELECTION_BG, ExtractedImage, ExtractedOutline, ExtractedRect, ExtractedShadow,
    ExtractedText,
};
use lumen_text::{SelectionBand, ShapeOptions, ShapedRun, TextGeometry, TextShaper, WrapMode};
use peniko::color::{AlphaColor, Srgb};
use peniko::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, RoundedRectRadii, Stroke};
use peniko::{Blob, BrushRef, Color as PenikoColor, Fill, FontData};

/// The peniko color for a Lumen color.
pub fn peniko_color(c: LumenColor) -> PenikoColor {
    let [r, g, b, a] = c.to_rgba8();
    AlphaColor::<Srgb>::from_rgba8(r, g, b, a)
}

/// Folds an inherited `opacity` multiplier into a colour's alpha. A no-op
/// when `opacity >= 1.0`; otherwise multiplies `a` by the clamped opacity.
/// Centralises the alpha-fold idiom shared by every leaf emitter so no site
/// can forget the clamp.
pub fn folded(mut c: LumenColor, opacity: f32) -> LumenColor {
    if opacity < 1.0 {
        c.a *= opacity.clamp(0.0, 1.0);
    }
    c
}

/// Shared cache-lookup skeleton for the cached emitters. On a hit, replays
/// the stored fragment translated to `origin`. On a miss, records `encode` at
/// the local origin into a fresh fragment, replays it, and stores it under
/// `key`. A sink with no fragments gets `encode` at `origin` directly.
fn emit_cached(
    painter: &mut dyn Painter,
    cache: &mut FragmentCache,
    key: FragmentKey,
    origin: glam::Vec2,
    encode: impl FnOnce(&mut dyn Painter, f64, f64),
) {
    let at = Affine::translate((origin.x as f64, origin.y as f64));
    if let Some(fragment) = cache.get(key)
        && painter.append_fragment(&fragment, at)
    {
        return;
    }
    if !painter.begin_fragment() {
        encode(painter, origin.x as f64, origin.y as f64);
        return;
    }
    encode(painter, 0.0, 0.0);
    if let Some(fragment) = painter.end_fragment() {
        painter.append_fragment(&fragment, at);
        cache.insert(key, fragment);
    }
}

/// Emit one rect, origin baked into the geometry.
pub fn emit_rect(painter: &mut dyn Painter, cmd: &ExtractedRect) {
    emit_rect_at(painter, cmd, cmd.origin.x as f64, cmd.origin.y as f64);
}

/// Cache-aware rect emit: one encode per appearance, replayed at `cmd.origin`.
pub fn emit_rect_cached(painter: &mut dyn Painter, cache: &mut FragmentCache, cmd: &ExtractedRect) {
    emit_cached(
        painter,
        cache,
        FragmentKey::from(cmd),
        cmd.origin,
        |p, ox, oy| emit_rect_at(p, cmd, ox, oy),
    );
}

/// Maps Lumen gradient stops straight through to `peniko::ColorStop`,
/// converting each colour via [`peniko_color`]. Shared by the linear /
/// radial / conic arms of [`emit_rect_at`].
fn to_color_stops(stops: &[(f32, LumenColor)]) -> Vec<peniko::ColorStop> {
    stops
        .iter()
        .map(|(offset, color)| peniko::ColorStop {
            offset: *offset,
            color: peniko_color(*color).into(),
        })
        .collect()
}

fn emit_rect_at(painter: &mut dyn Painter, cmd: &ExtractedRect, ox: f64, oy: f64) {
    use lumen_core::render_world::Brush as LumenBrush;
    let x0 = ox;
    let y0 = oy;
    let x1 = x0 + cmd.size.x as f64;
    let y1 = y0 + cmd.size.y as f64;

    let brush: peniko::Brush = match &cmd.brush {
        LumenBrush::Solid(c) => peniko::Brush::Solid(peniko_color(*c)),
        LumenBrush::Linear { angle_deg, stops } => {
            // Convert CSS angle (0deg = left->right, increasing CCW) into
            // start/end points on the bounding box. CSS defines 0deg as
            // bottom-to-top - but we use the more common "0 = right" so
            // authors can think in compass terms. Each stop is mapped
            // straight through to peniko::ColorStop.
            let cx = (x0 + x1) / 2.0;
            let cy = (y0 + y1) / 2.0;
            let diag = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt() / 2.0;
            let theta = (*angle_deg as f64).to_radians();
            let dx = theta.cos() * diag;
            let dy = theta.sin() * diag;
            let start = Point::new(cx - dx, cy - dy);
            let end = Point::new(cx + dx, cy + dy);
            let color_stops = to_color_stops(stops);
            let g = peniko::Gradient::new_linear(start, end).with_stops(color_stops.as_slice());
            peniko::Brush::Gradient(g)
        }
        LumenBrush::Radial { radius, stops } => {
            // Centre on the rect midpoint; multiply the normalised radius by half the rect's min dimension so `1.0` reaches the nearest edge.
            let cx = (x0 + x1) / 2.0;
            let cy = (y0 + y1) / 2.0;
            let half_min = ((x1 - x0).min(y1 - y0) / 2.0).max(1.0);
            let r = (*radius as f64 * half_min).max(1.0) as f32;
            let color_stops = to_color_stops(stops);
            let g = peniko::Gradient::new_radial(Point::new(cx, cy), r)
                .with_stops(color_stops.as_slice());
            peniko::Brush::Gradient(g)
        }
        LumenBrush::Conic { from_deg, stops } => {
            // Centre on the rect midpoint; the sweep runs clockwise from `from_deg` (0 = north).
            let cx = (x0 + x1) / 2.0;
            let cy = (y0 + y1) / 2.0;
            let color_stops = to_color_stops(stops);
            let start = *from_deg;
            let end = start + 360.0;
            let g = peniko::Gradient::new_sweep(Point::new(cx, cy), start, end)
                .with_stops(color_stops.as_slice());
            peniko::Brush::Gradient(g)
        }
    };

    let shape = if let Some([tl, tr, br, bl]) = cmd.corner_radii {
        let radii = RoundedRectRadii::new(tl as f64, tr as f64, br as f64, bl as f64);
        Shape::RoundedRect(RoundedRect::from_rect(Rect::new(x0, y0, x1, y1), radii))
    } else if cmd.radius > 0.0 {
        Shape::RoundedRect(RoundedRect::new(x0, y0, x1, y1, cmd.radius as f64))
    } else {
        Shape::Rect(Rect::new(x0, y0, x1, y1))
    };
    painter.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        BrushRef::from(&brush),
        None,
        &shape,
    );
}

/// CSS `object-fit` placement math shared by raster images ([`draw_image`])
/// and vector SVGs ([`emit_svg`]). Given the intrinsic size and the layout
/// box, returns the centred `(offset, drawn_size)` in the box's coordinate
/// space (top-left for `None`, centred otherwise). Both axes are guarded
/// against a zero intrinsic dimension; callers already reject zero-sized
/// content before calling, so the guard is a belt-and-braces no-op there.
pub fn fit_box(
    intrinsic: (f64, f64),
    box_size: (f64, f64),
    fit: lumen_core::components::ImageFit,
) -> ((f64, f64), (f64, f64)) {
    use lumen_core::components::ImageFit;
    let (iw, ih) = intrinsic;
    let (bw, bh) = box_size;
    let box_aspect = if bh > 0.0 { bw / bh } else { 1.0 };
    let img_aspect = if ih > 0.0 { iw / ih } else { 1.0 };
    let (dw, dh) = match fit {
        ImageFit::Fill => (bw, bh),
        ImageFit::None => (iw, ih),
        ImageFit::Contain => {
            if img_aspect > box_aspect {
                (bw, bw / img_aspect)
            } else {
                (bh * img_aspect, bh)
            }
        }
        ImageFit::Cover => {
            if img_aspect > box_aspect {
                (bh * img_aspect, bh)
            } else {
                (bw, bw / img_aspect)
            }
        }
        ImageFit::ScaleDown => {
            if iw <= bw && ih <= bh {
                (iw, ih)
            } else if img_aspect > box_aspect {
                (bw, bw / img_aspect)
            } else {
                (bh * img_aspect, bh)
            }
        }
    };
    let dx = match fit {
        ImageFit::None => 0.0,
        _ => (bw - dw) / 2.0,
    };
    let dy = match fit {
        ImageFit::None => 0.0,
        _ => (bh - dh) / 2.0,
    };
    ((dx, dy), (dw, dh))
}

/// Paint one SVG into its box with the same fit math raster images use, so
/// authors get consistent placement across `<image src="*.png">` and
/// `<image src="*.svg">`.
///
/// With a cache, the drawing is recorded once per asset as a fragment and
/// replayed under the placement transform; without one it is painted under
/// that transform directly.
pub fn emit_svg(
    painter: &mut dyn Painter,
    cache: Option<&mut FragmentCache>,
    cmd: &lumen_assets::ExtractedSvg,
) {
    let iw = cmd.intrinsic.x as f64;
    let ih = cmd.intrinsic.y as f64;
    let bw = cmd.size.x as f64;
    let bh = cmd.size.y as f64;
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let ((dx, dy), (dw, dh)) = fit_box((iw, ih), (bw, bh), cmd.fit);
    let sx = dw / iw;
    let sy = dh / ih;
    let transform = Affine::translate((cmd.origin.x as f64 + dx, cmd.origin.y as f64 + dy))
        * Affine::scale_non_uniform(sx, sy);
    // Wrap the drawing in a layer when opacity < 1.0 so the SVG fades
    // uniformly. Clip rect = drawn target rect.
    let alpha = cmd.alpha.clamp(0.0, 1.0);
    let needs_alpha = alpha < 1.0;
    if needs_alpha {
        let clip = Rect::new(
            cmd.origin.x as f64,
            cmd.origin.y as f64,
            cmd.origin.x as f64 + bw,
            cmd.origin.y as f64 + bh,
        );
        painter.push_layer(
            Fill::NonZero,
            peniko::BlendMode::default(),
            alpha,
            Affine::IDENTITY,
            &Shape::Rect(clip),
        );
    }
    let tree = &cmd.asset.tree;
    match cache {
        Some(cache) => {
            let key = FragmentKey::svg(cmd.asset.id);
            let replayed = match cache.get(key) {
                Some(fragment) => painter.append_fragment(&fragment, transform),
                None => false,
            };
            if !replayed {
                if painter.begin_fragment() {
                    crate::svg::paint_svg(painter, tree, Affine::IDENTITY);
                    if let Some(fragment) = painter.end_fragment() {
                        painter.append_fragment(&fragment, transform);
                        cache.insert(key, fragment);
                    }
                } else {
                    crate::svg::paint_svg(painter, tree, transform);
                }
            }
        }
        None => crate::svg::paint_svg(painter, tree, transform),
    }
    if needs_alpha {
        painter.pop_layer();
    }
}

/// Emit one drop shadow, origin baked into the geometry. One blurred
/// rounded rect per shadow, no stacked-clones approximation.
pub fn emit_shadow(painter: &mut dyn Painter, cmd: &ExtractedShadow) {
    emit_shadow_at(painter, cmd, cmd.origin.x as f64, cmd.origin.y as f64);
}

fn emit_shadow_at(painter: &mut dyn Painter, cmd: &ExtractedShadow, ox: f64, oy: f64) {
    // CSS spread: inflate (positive) / deflate (negative) the shadow
    // rect on every side before blurring; the corner radius grows /
    // shrinks with it (CSS Backgrounds & Borders section 7.1.1).
    let spread = cmd.spread as f64;
    let x1 = ox + cmd.size.x as f64;
    let y1 = oy + cmd.size.y as f64;
    if !cmd.inner {
        let rect = Rect::new(ox - spread, oy - spread, x1 + spread, y1 + spread);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        painter.draw_blurred_rounded_rect(
            Affine::IDENTITY,
            rect,
            peniko_color(cmd.color),
            (cmd.radius as f64 + spread).max(0.0),
            cmd.blur.max(0.0) as f64,
        );
        return;
    }
    // Inner (inset) shadow. Clip to the entity rect, then draw a
    // blurred rect at the *negated* offset so the dark edge lands on
    // the inside rim. Grow the inner rect outward by ~3x blur so the
    // gradient covers the whole interior; the clip hides the overflow.
    let rect_x0 = cmd.rect_origin.x as f64;
    let rect_y0 = cmd.rect_origin.y as f64;
    let rect_x1 = rect_x0 + cmd.size.x as f64;
    let rect_y1 = rect_y0 + cmd.size.y as f64;
    let clip = Rect::new(rect_x0, rect_y0, rect_x1, rect_y1);
    let grow = (cmd.blur.max(0.0) as f64 * 3.0).max(1.0);
    let dx = -(cmd.origin.x - cmd.rect_origin.x) as f64;
    let dy = -(cmd.origin.y - cmd.rect_origin.y) as f64;
    // Inset spread moves the shadow's inner edge inward: shrink the
    // blurred rect by `spread` per side (the clip still hides the
    // outer overflow).
    let inset_rect = Rect::new(
        rect_x0 + dx - grow + spread,
        rect_y0 + dy - grow + spread,
        rect_x1 + dx + grow - spread,
        rect_y1 + dy + grow - spread,
    );
    painter.push_layer(
        Fill::NonZero,
        peniko::BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &Shape::Rect(clip),
    );
    painter.draw_blurred_rounded_rect(
        Affine::IDENTITY,
        inset_rect,
        peniko_color(cmd.color),
        cmd.radius as f64,
        cmd.blur.max(0.0) as f64,
    );
    painter.pop_layer();
}

/// Cache-aware shadow emit. Identical appearance shares one encoded blurred
/// rect across every position.
pub fn emit_shadow_cached(
    painter: &mut dyn Painter,
    cache: &mut FragmentCache,
    cmd: &ExtractedShadow,
) {
    // Inner shadows depend on the entity's clip rect (not a
    // position-independent appearance), so they bypass the cache.
    if cmd.inner {
        emit_shadow_at(painter, cmd, cmd.origin.x as f64, cmd.origin.y as f64);
        return;
    }
    emit_cached(
        painter,
        cache,
        FragmentKey::from(cmd),
        cmd.origin,
        |p, ox, oy| emit_shadow_at(p, cmd, ox, oy),
    );
}

/// Emit one outline, origin baked into the geometry.
pub fn emit_outline(painter: &mut dyn Painter, cmd: &ExtractedOutline) {
    emit_outline_at(painter, cmd, cmd.origin.x as f64, cmd.origin.y as f64);
}

fn emit_outline_at(painter: &mut dyn Painter, cmd: &ExtractedOutline, ox: f64, oy: f64) {
    if cmd.width <= 0.0 {
        return;
    }
    let brush = peniko_color(cmd.stroke);
    let x1 = ox + cmd.size.x as f64;
    let y1 = oy + cmd.size.y as f64;
    let stroke = Stroke::new(cmd.width as f64);
    let shape = if cmd.radius > 0.0 {
        Shape::RoundedRect(RoundedRect::new(ox, oy, x1, y1, cmd.radius as f64))
    } else {
        Shape::Rect(Rect::new(ox, oy, x1, y1))
    };
    painter.stroke(&stroke, Affine::IDENTITY, brush.into(), None, &shape);
}

/// Cache-aware outline emit.
pub fn emit_outline_cached(
    painter: &mut dyn Painter,
    cache: &mut FragmentCache,
    cmd: &ExtractedOutline,
) {
    emit_cached(
        painter,
        cache,
        FragmentKey::from(cmd),
        cmd.origin,
        |p, ox, oy| emit_outline_at(p, cmd, ox, oy),
    );
}

/// Emit a CSS border ring: the area between the outer border box (rounded
/// by `cmd.radius`) and the inner padding box (each side inset by its width,
/// inner corner radii reduced per CSS `border-radius` background-clip math),
/// filled even-odd with the border color. Handles both uniform and per-side
/// widths exactly.
pub fn emit_border(painter: &mut dyn Painter, cmd: &lumen_core::render_world::ExtractedBorder) {
    use peniko::kurbo::Shape as _;
    let [top, right, bottom, left] = cmd.widths;
    if top <= 0.0 && right <= 0.0 && bottom <= 0.0 && left <= 0.0 {
        return;
    }
    let x0 = cmd.origin.x as f64;
    let y0 = cmd.origin.y as f64;
    let x1 = x0 + cmd.size.x as f64;
    let y1 = y0 + cmd.size.y as f64;
    let r = cmd.radius.max(0.0) as f64;
    // Per-corner outer radii `[tl, tr, br, bl]` - uniform `radius`
    // when the entity has no per-corner override.
    let [rtl, rtr, rbr, rbl] = cmd
        .corner_radii
        .map(|cs| cs.map(|c| c.max(0.0) as f64))
        .unwrap_or([r; 4]);
    let rounded = rtl > 0.0 || rtr > 0.0 || rbr > 0.0 || rbl > 0.0;
    let (top, right, bottom, left) = (top as f64, right as f64, bottom as f64, left as f64);

    // Inner box = border box inset by the per-side widths. Degenerate
    // (fully-consumed) inner boxes fill the whole outer shape.
    let ix0 = x0 + left;
    let iy0 = y0 + top;
    let ix1 = (x1 - right).max(ix0);
    let iy1 = (y1 - bottom).max(iy0);

    let mut path = BezPath::new();
    let tol = 0.1;
    if rounded {
        let outer_radii = RoundedRectRadii::new(rtl, rtr, rbr, rbl);
        path.extend(
            RoundedRect::from_rect(Rect::new(x0, y0, x1, y1), outer_radii).path_elements(tol),
        );
    } else {
        path.extend(Rect::new(x0, y0, x1, y1).path_elements(tol));
    }
    if ix1 > ix0 && iy1 > iy0 {
        // CSS: inner corner radius = max(0, outer radius - the two
        // adjacent border widths' relevant component). With one circular
        // radius per corner we take the max of the two adjacent widths.
        let radii = RoundedRectRadii::new(
            (rtl - left.max(top)).max(0.0),
            (rtr - top.max(right)).max(0.0),
            (rbr - right.max(bottom)).max(0.0),
            (rbl - bottom.max(left)).max(0.0),
        );
        let inner = RoundedRect::from_rect(Rect::new(ix0, iy0, ix1, iy1), radii);
        path.extend(inner.path_elements(tol));
    }

    // Uniform-color fast path: one even-odd fill of the ring.
    let uniform = match cmd.side_colors {
        None => true,
        Some([t, rr, b, l]) => t == rr && rr == b && b == l,
    };
    if uniform {
        let color = cmd.side_colors.map(|cs| cs[0]).unwrap_or(cmd.color);
        painter.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            peniko_color(color).into(),
            None,
            &Shape::Path(&path),
        );
        return;
    }

    // Per-side colors: clip to the ring, then fill one mitred trapezoid
    // per side (outer edge -> the matching inner-box corner), exactly the
    // corner-diagonal split browsers paint. The clip keeps the rounded
    // corners correct.
    let side_colors = cmd.side_colors.unwrap_or([cmd.color; 4]);
    painter.push_layer(
        Fill::EvenOdd,
        peniko::BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &Shape::Path(&path),
    );
    let quads: [(f64, [(f64, f64); 4]); 4] = [
        // top: outer TL, outer TR, inner TR, inner TL
        (top, [(x0, y0), (x1, y0), (ix1, iy0), (ix0, iy0)]),
        // right: outer TR, outer BR, inner BR, inner TR
        (right, [(x1, y0), (x1, y1), (ix1, iy1), (ix1, iy0)]),
        // bottom: outer BR, outer BL, inner BL, inner BR
        (bottom, [(x1, y1), (x0, y1), (ix0, iy1), (ix1, iy1)]),
        // left: outer BL, outer TL, inner TL, inner BL
        (left, [(x0, y1), (x0, y0), (ix0, iy0), (ix0, iy1)]),
    ];
    for (i, (width, pts)) in quads.iter().enumerate() {
        if *width <= 0.0 {
            continue;
        }
        let mut quad = BezPath::new();
        quad.move_to(pts[0]);
        quad.line_to(pts[1]);
        quad.line_to(pts[2]);
        quad.line_to(pts[3]);
        quad.close_path();
        painter.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            peniko_color(side_colors[i]).into(),
            None,
            &Shape::Path(&quad),
        );
    }
    painter.pop_layer();
}

/// Draw one [`ExtractedImage`] honouring its [`lumen_core::components::ImageFit`].
/// `blob` is the pre-built peniko Blob from [`lumen_assets::LoadedImage`],
/// cloned (cheap, Arc-internal) each frame so a sink can key its upload
/// cache off the stable Blob identity rather than re-uploading every tick.
pub fn draw_image(
    painter: &mut dyn Painter,
    cmd: &ExtractedImage,
    blob: &lumen_assets::ExtractedImageBlob,
) {
    use lumen_core::components::ImageFit;
    if cmd.width == 0 || cmd.height == 0 {
        return;
    }
    let image_data = peniko::ImageData {
        data: blob.0.clone(),
        format: peniko::ImageFormat::Rgba8,
        alpha_type: peniko::ImageAlphaType::Alpha,
        width: cmd.width,
        height: cmd.height,
    };

    let iw = cmd.width as f64;
    let ih = cmd.height as f64;
    let bw = cmd.size.x as f64;
    let bh = cmd.size.y as f64;

    // Compute scaled (drawn) width + height + per-axis offset inside the
    // layout box according to the fit mode. Top-left default; centered
    // for cover / contain / scale-down because that's what CSS / Flutter
    // / SwiftUI all do.
    let ((dx, dy), (dw, dh)) = fit_box((iw, ih), (bw, bh), cmd.fit);

    let sx = if iw > 0.0 { dw / iw } else { 1.0 };
    let sy = if ih > 0.0 { dh / ih } else { 1.0 };
    let transform = Affine::translate((cmd.origin.x as f64 + dx, cmd.origin.y as f64 + dy))
        * Affine::scale_non_uniform(sx, sy);

    // Cover may overshoot the box - clip to the entity rect so the image
    // doesn't bleed onto sibling boxes. The same layer carries the
    // `Opacity` alpha so partially-transparent images fade as a whole.
    let needs_clip = matches!(cmd.fit, ImageFit::Cover | ImageFit::None);
    let alpha = cmd.alpha.clamp(0.0, 1.0);
    let needs_alpha = alpha < 1.0;
    let needs_layer = needs_clip || needs_alpha;
    if needs_layer {
        let clip = Rect::new(
            cmd.origin.x as f64,
            cmd.origin.y as f64,
            cmd.origin.x as f64 + bw,
            cmd.origin.y as f64 + bh,
        );
        painter.push_layer(
            Fill::NonZero,
            peniko::BlendMode::default(),
            alpha,
            Affine::IDENTITY,
            &Shape::Rect(clip),
        );
    }
    painter.draw_image(&peniko::ImageBrush::new(image_data), transform);
    if needs_layer {
        painter.pop_layer();
    }
}

thread_local! {
    /// Per-font-id `Blob` cache. Keyed on the shaper's stable `font_id`
    /// hash so the same face reuses one `Blob` (hence one glyph-cache entry
    /// in a sink that keys on blob identity) across frames. Rendering is
    /// single-threaded per surface, so a thread-local keeps the cache
    /// lock-free; a second render thread simply warms its own copy. Fonts
    /// are few and long-lived - no eviction.
    static FONT_BLOBS: std::cell::RefCell<rustc_hash::FxHashMap<u64, Blob<u8>>> =
        std::cell::RefCell::new(rustc_hash::FxHashMap::default());
}

/// Fetch (or mint + cache) the `Blob` for a font id. The clone returned is
/// an `Arc` bump - the underlying face bytes are shared, not copied.
fn font_blob(font_id: u64, font_data: &std::sync::Arc<Vec<u8>>) -> Blob<u8> {
    FONT_BLOBS.with(|c| {
        c.borrow_mut()
            .entry(font_id)
            .or_insert_with(|| Blob::new(font_data.clone()))
            .clone()
    })
}

/// Draws one [`ExtractedText`] using the supplied shaper.
///
/// The un-scaled run is passed by reference alongside the running device-
/// pixel ratio (`dpr`) and `opacity`; both are folded in locally so the
/// walker never has to deep-clone the run (String and all) per node.
///
/// Shapes the run once per frame; caret and selection x positions come from
/// the same shape via [`TextGeometry`]. A run that mixes scripts issues one
/// glyph run per segment so each font is bound to the right glyph slice;
/// selection highlights emit one rectangle per maximal contiguous-level slice
/// the range intersects (HTML / Qt / macOS convention).
pub fn draw_text<S: TextShaper + ?Sized>(
    shaper: &mut S,
    painter: &mut dyn Painter,
    text: &ExtractedText,
    dpr: f32,
    opacity: f32,
) {
    use lumen_core::components::TextAlign;
    let origin = text.origin * dpr;
    let size_px = text.size_px * dpr;
    let container_width = text.container_width * dpr;
    // `ExtractedText::line_height_px` is already resolved (CSS `line-height`
    // or the `DEFAULT_LINE_HEIGHT_MULTIPLIER` fallback) in logical px;
    // scale it by the same `dpr` factor as `size_px` so the shaper's
    // `Metrics` and the newline-caret math below stay in the same space.
    let line_height_px = text.line_height_px * dpr;
    let fill = folded(text.fill, opacity);
    // Shape the full run for glyph painting using the text's configured wrap and `max_lines`.
    let wrap = WrapMode::from(text.wrap);
    let shape_opts = ShapeOptions {
        width: Some(container_width),
        wrap,
        max_lines: text.max_lines,
        family: text.family.clone(),
        weight: text.weight,
        line_height: Some(line_height_px),
    };
    let shaped = shaper.shape(&text.text, size_px, shape_opts);
    let measured = shaped.as_ref().map(|r| r.width).unwrap_or(0.0);
    let align_dx = match text.align {
        TextAlign::Start => 0.0,
        TextAlign::Center => ((container_width - measured) / 2.0).max(0.0),
        TextAlign::End => (container_width - measured).max(0.0),
    };
    let draw_x = origin.x + align_dx;
    // The geometry index is only consumed by the caret + selection branches;
    // building it for every text node (the common no-caret label) wasted a
    // per-node pass. Build it lazily only when one of those is present.
    let run_index = if text.caret.is_some() || text.selection.is_some() {
        shaped.as_ref().map(TextGeometry::from)
    } else {
        None
    };
    // Selection bands, shared by the highlight fill below and the
    // selected-glyph foreground over-paint further down. BiDi-correct:
    // [`TextGeometry::selection_bands`] returns one band per
    // line-portion of each maximal contiguous-level slice - matches HTML /
    // Qt / macOS selection visualisation. Each band carries its own
    // baseline, so a selection running across a line break paints on both
    // lines instead of collapsing onto the first one.
    let sel_bands: Vec<SelectionBand> = match (text.selection, &run_index) {
        (Some((s, e)), Some(idx)) if e > s => idx.selection_bands(s, e),
        _ => Vec::new(),
    };
    let band_y = |b: &SelectionBand| {
        let base = origin.y as f64 + b.baseline_y as f64;
        (base - size_px as f64 * 0.9, base + size_px as f64 * 0.15)
    };
    // Selection highlight paints first so glyphs sit on top. Styleable
    // via `selection-color` (default skin: `--lumen-selection`); the
    // single built-in fallback is the platform highlight blue
    // ([`DEFAULT_SELECTION_BG`]) - visible on any field color, unlike the
    // old text-fill-at-32%-alpha fallback which vanished on light fields.
    if !sel_bands.is_empty() {
        let sel = folded(
            text.selection_color.unwrap_or(DEFAULT_SELECTION_BG),
            opacity,
        );
        let brush = peniko_color(sel);
        for b in &sel_bands {
            let (y0, y1) = band_y(b);
            let x0 = draw_x as f64 + b.x0 as f64;
            let x1 = draw_x as f64 + b.x1 as f64;
            painter.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                brush.into(),
                None,
                &Shape::Rect(Rect::new(x0, y0, x1, y1)),
            );
        }
    }
    if let Some(run) = &shaped {
        draw_glyph_run(painter, run, size_px, draw_x, origin.y, peniko_color(fill));
    }
    // Selected-glyph foreground (Qt `QPalette::HighlightedText` / Slint
    // `selection-foreground-color`): re-paint the glyphs that fall inside
    // each selection rect in the override color, clipping to the rect so
    // only the selected span inverts. Opt-in - the default translucent
    // highlight preserves unselected contrast, so most skins never set it.
    if let (Some(run), Some(fg)) = (&shaped, text.selection_foreground)
        && !sel_bands.is_empty()
    {
        let fg = folded(fg, opacity);
        let brush = peniko_color(fg);
        for b in &sel_bands {
            let (y0, y1) = band_y(b);
            let clip = Rect::new(
                draw_x as f64 + b.x0 as f64,
                y0,
                draw_x as f64 + b.x1 as f64,
                y1,
            );
            painter.push_layer(
                Fill::NonZero,
                peniko::BlendMode::default(),
                1.0,
                Affine::IDENTITY,
                &Shape::Rect(clip),
            );
            draw_glyph_run(painter, run, size_px, draw_x, origin.y, brush);
            painter.pop_layer();
        }
    }
    // Caret position computed from the same TextGeometry - no extra
    // shape pass. `caret_xy` also yields the baseline offset of
    // the byte's line so multiline carets land on the right line.
    if let Some(byte_offset) = text.caret {
        let line_height = line_height_px as f64;
        let (caret_x, caret_y) =
            if byte_offset > 0 && text.text.as_bytes().get(byte_offset - 1) == Some(&b'\n') {
                // Caret sits at the start of a (possibly empty) line right
                // after a newline - newlines emit no glyph cluster, so
                // derive the line index from the text instead of the shape.
                let line_idx = text.text[..byte_offset.min(text.text.len())]
                    .matches('\n')
                    .count();
                (0.0, line_idx as f64 * line_height)
            } else if let Some(idx) = &run_index {
                let (x, y) = idx.caret_xy(byte_offset);
                (x as f64, y as f64)
            } else {
                // No shape (empty text or shaper missing): caret at origin.
                (0.0, 0.0)
            };
        // Ascent / descent stay fixed font-metric ratios of `size_px`,
        // independent of `line-height` - real CSS line-height changes the
        // spacing between lines, not the glyph box a caret hugs.
        let h = size_px as f64 * 0.9;
        let x0 = draw_x as f64 + caret_x;
        let y0 = origin.y as f64 + caret_y - h;
        // Caret width is `ExtractedText::caret_width_px` (CSS `caret-width`,
        // else `CARET_WIDTH_PX`) scaled to physical pixels (a bare fixed
        // width would render a sliver on hidpi and be easy to lose against
        // the field). Floor at 1 physical px so it never sub-pixels away
        // entirely.
        let x1 = x0 + (text.caret_width_px as f64 * dpr as f64).max(1.0);
        let y1 = origin.y as f64 + caret_y + size_px as f64 * 0.15;
        // Caret color: `caret-color` token, else the text fill (web
        // default). Alpha-folds the inherited opacity like the glyphs.
        let caret_col = folded(text.caret_color.unwrap_or(text.fill), opacity);
        painter.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            peniko_color(caret_col).into(),
            None,
            &Shape::Rect(Rect::new(x0, y0, x1, y1)),
        );
    }
}

/// Paint one shaped run's glyphs at `(draw_x, baseline_y)` in `brush`, one
/// glyph run per segment so each font binds its own glyph slice; pure-Latin
/// runs degenerate to a single call. The per-font [`Blob`] is reused via
/// [`font_blob`] so a sink's glyph cache (keyed on `Blob` identity) stays
/// warm.
pub fn draw_glyph_run(
    painter: &mut dyn Painter,
    run: &ShapedRun,
    size_px: f32,
    draw_x: f32,
    baseline_y: f32,
    brush: PenikoColor,
) {
    for seg in &run.segments {
        if seg.glyphs.is_empty() {
            continue;
        }
        let blob = font_blob(seg.font_id, &seg.font_data);
        let font = FontData::new(blob, seg.font_index);
        painter.draw_glyphs(&GlyphRun {
            font: &font,
            font_size: size_px,
            // The instance the shaper measured this segment at. A variable
            // face otherwise paints its default instance, pairing one
            // weight's advances with another weight's strokes; a static
            // face reports no coordinates and is unaffected.
            normalized_coords: &seg.normalized_coords,
            transform: Affine::translate((draw_x as f64, baseline_y as f64)),
            brush: brush.into(),
            glyphs: &seg.glyphs,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::Recording;
    use lumen_text::NullShaper;

    /// A text run with the fields a plain label carries, so each test
    /// only states the ones it is about.
    fn label(text: &str) -> ExtractedText {
        ExtractedText {
            origin: glam::Vec2::new(10.0, 20.0),
            text: text.to_string(),
            size_px: 16.0,
            fill: LumenColor::rgba(0.0, 0.0, 0.0, 1.0),
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
        }
    }

    /// A build with no text backend shapes nothing, and a label then
    /// paints nothing rather than a placeholder box.
    #[test]
    fn a_label_with_no_shaper_paints_nothing() {
        let mut target = Recording::default();

        draw_text(&mut NullShaper, &mut target, &label("Save"), 1.0, 1.0);

        assert!(
            target.is_empty(),
            "no shaped glyphs means no geometry, not an empty rectangle",
        );
    }

    /// The caret does not come from the shaper, so a focused field still
    /// shows one when the run shapes to nothing: an empty input, or a
    /// build with no text backend. Without this the cursor disappears
    /// exactly where the user is about to type.
    #[test]
    fn a_caret_paints_without_a_shaped_run() {
        let mut target = Recording::default();
        let mut field = label("");
        field.caret = Some(0);
        field.selection = Some((0, 0));

        draw_text(&mut NullShaper, &mut target, &field, 2.0, 1.0);

        assert!(
            !target.is_empty(),
            "the caret is painted from the field origin, not from glyphs",
        );
    }

    /// Alpha folding is how an inherited `opacity` reaches every leaf
    /// emitter, so it must leave a fully opaque parent alone, scale the
    /// child's alpha otherwise, and clamp rather than produce a negative or
    /// over-bright alpha.
    #[test]
    fn opacity_folds_into_alpha() {
        let color = LumenColor::rgba(1.0, 0.0, 0.0, 0.8);
        assert_eq!(folded(color, 1.0).a, 0.8);
        assert!((folded(color, 0.5).a - 0.4).abs() < 1e-6);
        assert_eq!(folded(color, -1.0).a, 0.0);
        assert_eq!(folded(color, 2.0).a, 0.8);
    }

    /// A sink with no fragments still gets every cached leaf, painted in
    /// place at its origin rather than recorded and dropped.
    #[test]
    fn a_sink_without_fragments_paints_cached_leaves_in_place() {
        let mut target = Recording::default();
        let mut cache = FragmentCache::default();
        let rect = ExtractedRect {
            origin: glam::Vec2::new(5.0, 6.0),
            size: glam::Vec2::new(10.0, 10.0),
            brush: lumen_core::render_world::Brush::Solid(LumenColor::rgb(1.0, 0.0, 0.0)),
            radius: 0.0,
            corner_radii: None,
            order: 0,
        };

        emit_rect_cached(&mut target, &mut cache, &rect);

        assert_eq!(target.len(), 1);
        assert!(
            cache.is_empty(),
            "nothing was recorded, so nothing is cached"
        );
    }

    use crate::Fragment;
    use crate::recording::{Command, OwnedShape};
    use lumen_core::components::ImageFit;
    use lumen_core::render_world::{Brush as LumenBrush, ExtractedBorder};
    use lumen_text::{GlyphPosition, ShapedSegment};
    use std::sync::Arc;

    /// A sink with fragments: a fragment is a [`Recording`], and appending
    /// one replays it into the frame.
    #[derive(Default)]
    struct FragmentSink {
        frame: Recording,
        open: Option<Recording>,
        appended: usize,
    }

    impl FragmentSink {
        fn target(&mut self) -> &mut Recording {
            match &mut self.open {
                Some(open) => open,
                None => &mut self.frame,
            }
        }
    }

    impl Painter for FragmentSink {
        fn fill(
            &mut self,
            style: Fill,
            transform: Affine,
            brush: BrushRef<'_>,
            brush_transform: Option<Affine>,
            shape: &Shape<'_>,
        ) {
            self.target()
                .fill(style, transform, brush, brush_transform, shape);
        }
        fn stroke(
            &mut self,
            style: &Stroke,
            transform: Affine,
            brush: BrushRef<'_>,
            brush_transform: Option<Affine>,
            shape: &Shape<'_>,
        ) {
            self.target()
                .stroke(style, transform, brush, brush_transform, shape);
        }
        fn push_layer(
            &mut self,
            clip_style: Fill,
            blend: peniko::BlendMode,
            alpha: f32,
            transform: Affine,
            clip: &Shape<'_>,
        ) {
            self.target()
                .push_layer(clip_style, blend, alpha, transform, clip);
        }
        fn pop_layer(&mut self) {
            self.target().pop_layer();
        }
        fn layer_depth(&self) -> usize {
            self.frame.layer_depth()
        }
        fn draw_masked(
            &mut self,
            kind: crate::MaskKind,
            transform: Affine,
            region: &Shape<'_>,
            mask: &mut dyn FnMut(&mut dyn Painter),
            content: &mut dyn FnMut(&mut dyn Painter),
        ) {
            self.target()
                .draw_masked(kind, transform, region, mask, content);
        }
        fn draw_blurred_rounded_rect(
            &mut self,
            transform: Affine,
            rect: Rect,
            color: PenikoColor,
            radius: f64,
            std_dev: f64,
        ) {
            self.target()
                .draw_blurred_rounded_rect(transform, rect, color, radius, std_dev);
        }
        fn draw_image(&mut self, image: &peniko::ImageBrush, transform: Affine) {
            self.target().draw_image(image, transform);
        }
        fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
            self.target().draw_glyphs(run);
        }
        fn begin_fragment(&mut self) -> bool {
            self.open = Some(Recording::default());
            true
        }
        fn end_fragment(&mut self) -> Option<Fragment> {
            self.open.take().map(|r| Fragment::new(Arc::new(r)))
        }
        fn append_fragment(&mut self, fragment: &Fragment, transform: Affine) -> bool {
            let Some(recorded) = fragment.downcast::<Recording>() else {
                return false;
            };
            recorded.replay(&mut self.frame, transform);
            self.appended += 1;
            true
        }
        fn backend_id(&self) -> &'static str {
            "test.fragments"
        }
        fn native(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    fn red() -> LumenColor {
        LumenColor::rgb(1.0, 0.0, 0.0)
    }

    fn rect(brush: LumenBrush) -> ExtractedRect {
        ExtractedRect {
            origin: glam::Vec2::new(10.0, 20.0),
            size: glam::Vec2::new(40.0, 20.0),
            brush,
            radius: 0.0,
            corner_radii: None,
            order: 0,
        }
    }

    fn only_fill(target: &Recording) -> (&peniko::Brush, &OwnedShape) {
        assert_eq!(target.len(), 1, "{:?}", target.commands());
        match &target.commands()[0] {
            Command::Fill { brush, shape, .. } => (brush, shape),
            other => panic!("expected one fill, got {other:?}"),
        }
    }

    /// Each Lumen brush becomes the peniko brush of the same kind, centred
    /// on the rect, and the radius picks the rect's shape.
    #[test]
    fn a_rect_paints_its_brush_in_its_shape() {
        let stops: Arc<[(f32, LumenColor)]> =
            Arc::from([(0.0, red()), (1.0, LumenColor::rgb(0.0, 0.0, 1.0))].as_slice());

        let mut target = Recording::default();
        emit_rect(&mut target, &rect(LumenBrush::Solid(red())));
        let (brush, shape) = only_fill(&target);
        assert!(matches!(brush, peniko::Brush::Solid(_)));
        assert_eq!(*shape, OwnedShape::Rect(Rect::new(10.0, 20.0, 50.0, 40.0)));

        let mut target = Recording::default();
        let mut linear = rect(LumenBrush::Linear {
            angle_deg: 0.0,
            stops: stops.clone(),
        });
        linear.radius = 4.0;
        emit_rect(&mut target, &linear);
        let (brush, shape) = only_fill(&target);
        let peniko::Brush::Gradient(g) = brush else {
            panic!("expected a gradient, got {brush:?}");
        };
        let peniko::GradientKind::Linear(pos) = g.kind else {
            panic!("expected a linear gradient, got {:?}", g.kind);
        };
        assert_eq!(pos.start.y, 30.0, "a 0deg gradient runs through the middle");
        assert!(pos.start.x < pos.end.x, "0deg runs left to right");
        assert_eq!(g.stops.len(), 2);
        assert!(matches!(shape, OwnedShape::RoundedRect(r) if r.radii().top_left == 4.0));

        let mut target = Recording::default();
        let mut radial = rect(LumenBrush::Radial {
            radius: 1.0,
            stops: stops.clone(),
        });
        radial.corner_radii = Some([1.0, 2.0, 3.0, 4.0]);
        emit_rect(&mut target, &radial);
        let (brush, shape) = only_fill(&target);
        let peniko::Brush::Gradient(g) = brush else {
            panic!("expected a gradient, got {brush:?}");
        };
        let peniko::GradientKind::Radial(pos) = g.kind else {
            panic!("expected a radial gradient, got {:?}", g.kind);
        };
        assert_eq!(pos.end_center, Point::new(30.0, 30.0));
        assert_eq!(pos.end_radius, 10.0, "1.0 reaches the nearest edge");
        assert!(matches!(shape, OwnedShape::RoundedRect(r) if r.radii().bottom_left == 4.0));

        let mut target = Recording::default();
        emit_rect(
            &mut target,
            &rect(LumenBrush::Conic {
                from_deg: 90.0,
                stops,
            }),
        );
        let (brush, _) = only_fill(&target);
        let peniko::Brush::Gradient(g) = brush else {
            panic!("expected a gradient, got {brush:?}");
        };
        let peniko::GradientKind::Sweep(pos) = g.kind else {
            panic!("expected a sweep gradient, got {:?}", g.kind);
        };
        assert_eq!(pos.center, Point::new(30.0, 30.0));
        assert_eq!(pos.end_angle - pos.start_angle, 360.0);
    }

    /// On a sink with fragments a leaf is recorded once at the local origin
    /// and replayed at every origin after that; a second appearance at
    /// another place is a cache hit, not a second recording.
    #[test]
    fn a_sink_with_fragments_records_a_leaf_once_and_replays_it() {
        let mut sink = FragmentSink::default();
        let mut cache = FragmentCache::default();
        let first = rect(LumenBrush::Solid(red()));
        let mut second = first.clone();
        second.origin = glam::Vec2::new(100.0, 0.0);

        emit_rect_cached(&mut sink, &mut cache, &first);
        emit_rect_cached(&mut sink, &mut cache, &second);

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(sink.appended, 2);
        let origins: Vec<(f64, f64)> = sink
            .frame
            .commands()
            .iter()
            .map(|c| match c {
                Command::Fill { transform, .. } => transform.translation().into(),
                other => panic!("expected fills, got {other:?}"),
            })
            .collect();
        assert_eq!(origins, [(10.0, 20.0), (100.0, 0.0)]);
    }

    /// A cached fragment the sink cannot replay (another sink made it) is
    /// recorded again and replaced, rather than leaving the leaf unpainted.
    #[test]
    fn a_fragment_the_sink_cannot_replay_is_recorded_again() {
        let mut sink = FragmentSink::default();
        let mut cache = FragmentCache::default();
        let leaf = rect(LumenBrush::Solid(red()));
        cache.insert(FragmentKey::from(&leaf), Fragment::new(Arc::new(())));

        emit_rect_cached(&mut sink, &mut cache, &leaf);

        assert_eq!(sink.frame.len(), 1, "the leaf is painted");
        let replaced = cache.get(FragmentKey::from(&leaf)).expect("re-cached");
        assert!(replaced.downcast::<Recording>().is_some());
    }

    fn shadow() -> ExtractedShadow {
        ExtractedShadow {
            origin: glam::Vec2::new(12.0, 22.0),
            size: glam::Vec2::new(40.0, 20.0),
            radius: 4.0,
            spread: 2.0,
            blur: 3.0,
            color: LumenColor::rgba(0.0, 0.0, 0.0, 0.5),
            order: 0,
            inner: false,
            rect_origin: glam::Vec2::new(10.0, 20.0),
        }
    }

    /// An outer shadow is one blurred rect grown by its spread; a spread
    /// that swallows the box paints nothing.
    #[test]
    fn an_outer_shadow_is_one_blurred_rect_grown_by_its_spread() {
        let mut target = Recording::default();
        emit_shadow(&mut target, &shadow());
        assert_eq!(target.len(), 1);
        let Command::BlurredRoundedRect {
            rect,
            radius,
            std_dev,
            ..
        } = &target.commands()[0]
        else {
            panic!("expected a blurred rect, got {:?}", target.commands());
        };
        assert_eq!(*rect, Rect::new(10.0, 20.0, 54.0, 44.0));
        assert_eq!((*radius, *std_dev), (6.0, 3.0));

        let mut swallowed = shadow();
        swallowed.spread = -30.0;
        let mut target = Recording::default();
        emit_shadow(&mut target, &swallowed);
        assert!(target.is_empty());

        let mut sink = FragmentSink::default();
        let mut cache = FragmentCache::default();
        emit_shadow_cached(&mut sink, &mut cache, &shadow());
        emit_shadow_cached(&mut sink, &mut cache, &shadow());
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(sink.frame.len(), 2);
    }

    /// An inset shadow is clipped to its box and never cached, since it
    /// depends on where the box is.
    #[test]
    fn an_inner_shadow_paints_inside_its_box_and_skips_the_cache() {
        let mut inner = shadow();
        inner.inner = true;
        let mut sink = FragmentSink::default();
        let mut cache = FragmentCache::default();

        emit_shadow_cached(&mut sink, &mut cache, &inner);

        assert!(cache.is_empty());
        let commands = sink.frame.commands();
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert!(matches!(
            &commands[0],
            Command::PushLayer { clip: OwnedShape::Rect(r), .. }
                if *r == Rect::new(10.0, 20.0, 50.0, 40.0)
        ));
        assert!(matches!(commands[1], Command::BlurredRoundedRect { .. }));
        assert!(matches!(commands[2], Command::PopLayer));
    }

    /// An outline strokes its box, rounded when it has a radius, and a zero
    /// width draws nothing.
    #[test]
    fn an_outline_strokes_its_box() {
        let mut outline = ExtractedOutline {
            origin: glam::Vec2::new(1.0, 2.0),
            size: glam::Vec2::new(10.0, 10.0),
            stroke: red(),
            width: 2.0,
            radius: 3.0,
            order: 0,
        };
        let mut target = Recording::default();
        emit_outline(&mut target, &outline);
        assert!(matches!(
            &target.commands()[0],
            Command::Stroke { style, shape: OwnedShape::RoundedRect(_), .. } if style.width == 2.0
        ));

        outline.radius = 0.0;
        let mut sink = FragmentSink::default();
        let mut cache = FragmentCache::default();
        emit_outline_cached(&mut sink, &mut cache, &outline);
        assert!(matches!(
            &sink.frame.commands()[0],
            Command::Stroke {
                shape: OwnedShape::Rect(_),
                ..
            }
        ));

        outline.width = 0.0;
        let mut target = Recording::default();
        emit_outline(&mut target, &outline);
        assert!(target.is_empty());
    }

    fn border() -> ExtractedBorder {
        ExtractedBorder {
            origin: glam::Vec2::ZERO,
            size: glam::Vec2::new(20.0, 10.0),
            widths: [1.0, 2.0, 3.0, 4.0],
            color: red(),
            side_colors: None,
            radius: 0.0,
            corner_radii: None,
            order: 0,
        }
    }

    /// One border color is one even-odd fill of the ring, rounded or not; no
    /// width at all paints nothing.
    #[test]
    fn a_one_color_border_is_one_ring_fill() {
        let mut target = Recording::default();
        emit_border(&mut target, &border());
        let (_, shape) = only_fill(&target);
        let OwnedShape::Path(ring) = shape else {
            panic!("the ring is a path, got {shape:?}");
        };
        assert!(matches!(
            target.commands()[0],
            Command::Fill {
                style: Fill::EvenOdd,
                ..
            }
        ));
        use peniko::kurbo::Shape as _;
        assert_eq!(ring.winding(Point::new(2.0, 0.5)), 1, "on the ring");
        assert_eq!(ring.winding(Point::new(10.0, 5.0)) % 2, 0, "in the hole");

        let mut rounded = border();
        rounded.corner_radii = Some([6.0, 6.0, 0.0, 0.0]);
        rounded.side_colors = Some([red(); 4]);
        let mut target = Recording::default();
        emit_border(&mut target, &rounded);
        assert_eq!(target.len(), 1, "four equal side colors are one color");

        let mut none = border();
        none.widths = [0.0; 4];
        let mut target = Recording::default();
        emit_border(&mut target, &none);
        assert!(target.is_empty());
    }

    /// Per-side colors fill one trapezoid per side with a width, each in
    /// its own color, inside a clip to the ring.
    #[test]
    fn a_border_with_side_colors_fills_each_side_inside_the_ring() {
        let blue = LumenColor::rgb(0.0, 0.0, 1.0);
        let mut sides = border();
        sides.widths = [2.0, 2.0, 0.0, 2.0];
        sides.radius = 3.0;
        sides.side_colors = Some([red(), blue, red(), blue]);
        let mut target = Recording::default();

        emit_border(&mut target, &sides);

        let commands = target.commands();
        assert!(matches!(
            commands[0],
            Command::PushLayer {
                clip_style: Fill::EvenOdd,
                ..
            }
        ));
        let colors: Vec<peniko::Brush> = commands[1..commands.len() - 1]
            .iter()
            .map(|c| match c {
                Command::Fill { brush, .. } => brush.clone(),
                other => panic!("expected side fills, got {other:?}"),
            })
            .collect();
        let red = peniko::Brush::Solid(peniko_color(red()));
        let blue = peniko::Brush::Solid(peniko_color(blue));
        assert_eq!(colors, [red, blue.clone(), blue], "the bottom has no width");
        assert!(matches!(commands.last(), Some(Command::PopLayer)));
        assert_eq!(target.layer_depth(), 0);
    }

    /// The fit modes place content the way CSS `object-fit` does.
    #[test]
    fn fit_box_follows_object_fit() {
        let wide = (200.0, 100.0);
        let tall = (100.0, 200.0);
        let square = (100.0, 100.0);
        let small = (10.0, 10.0);
        assert_eq!(fit_box(wide, square, ImageFit::Fill), ((0.0, 0.0), square));
        assert_eq!(fit_box(wide, square, ImageFit::None), ((0.0, 0.0), wide));
        assert_eq!(
            fit_box(wide, square, ImageFit::Contain),
            ((0.0, 25.0), (100.0, 50.0))
        );
        assert_eq!(
            fit_box(tall, square, ImageFit::Contain),
            ((25.0, 0.0), (50.0, 100.0))
        );
        assert_eq!(
            fit_box(wide, square, ImageFit::Cover),
            ((-50.0, 0.0), (200.0, 100.0))
        );
        assert_eq!(
            fit_box(tall, square, ImageFit::Cover),
            ((0.0, -50.0), (100.0, 200.0))
        );
        assert_eq!(
            fit_box(small, square, ImageFit::ScaleDown),
            ((45.0, 45.0), small)
        );
        assert_eq!(
            fit_box(wide, square, ImageFit::ScaleDown),
            ((0.0, 25.0), (100.0, 50.0))
        );
        assert_eq!(
            fit_box(tall, square, ImageFit::ScaleDown),
            ((25.0, 0.0), (50.0, 100.0))
        );
        assert_eq!(
            fit_box((0.0, 0.0), (10.0, 0.0), ImageFit::Contain),
            ((5.0, 0.0), (0.0, 0.0)),
            "a zero dimension does not divide by zero",
        );
    }

    fn image(fit: ImageFit, alpha: f32) -> ExtractedImage {
        ExtractedImage {
            origin: glam::Vec2::new(5.0, 5.0),
            size: glam::Vec2::new(4.0, 4.0),
            width: 2,
            height: 2,
            rgba: Arc::from(vec![255u8; 16]),
            fit,
            order: 0,
            alpha,
            background: None,
        }
    }

    /// An image scales into its box; it gets a clip layer only when it can
    /// overflow the box or must fade, and an empty image paints nothing.
    #[test]
    fn an_image_clips_only_when_it_can_overflow_or_fades() {
        let blob = lumen_assets::ExtractedImageBlob(peniko::Blob::new(Arc::new(vec![255u8; 16])));

        let mut target = Recording::default();
        draw_image(&mut target, &image(ImageFit::Fill, 1.0), &blob);
        assert_eq!(target.len(), 1);
        let Command::Image {
            image: drawn,
            transform,
        } = &target.commands()[0]
        else {
            panic!("expected the image, got {:?}", target.commands());
        };
        assert_eq!((drawn.image.width, drawn.image.height), (2, 2));
        assert_eq!(transform.translation(), (5.0, 5.0).into());
        assert_eq!(transform.as_coeffs()[0], 2.0, "2 px scaled into a 4 px box");

        for (fit, alpha) in [(ImageFit::Cover, 1.0), (ImageFit::Fill, 0.5)] {
            let mut target = Recording::default();
            draw_image(&mut target, &image(fit, alpha), &blob);
            assert_eq!(target.len(), 3, "{fit:?} at {alpha}");
            assert!(matches!(
                target.commands()[0],
                Command::PushLayer { alpha: a, .. } if a == alpha
            ));
        }

        let mut empty = image(ImageFit::Fill, 1.0);
        empty.width = 0;
        let mut target = Recording::default();
        draw_image(&mut target, &empty, &blob);
        assert!(target.is_empty());
    }

    fn svg(alpha: f32) -> lumen_assets::ExtractedSvg {
        let tree = usvg::Tree::from_str(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
              <rect width="10" height="10" fill="#ff0000"/>
            </svg>"##,
            &usvg::Options::default(),
        )
        .expect("valid svg");
        lumen_assets::ExtractedSvg {
            origin: glam::Vec2::new(20.0, 0.0),
            size: glam::Vec2::new(20.0, 20.0),
            intrinsic: glam::Vec2::new(10.0, 10.0),
            asset: lumen_assets::SvgData {
                intrinsic: glam::Vec2::new(10.0, 10.0),
                tree,
                id: lumen_assets::SvgData::next_id(),
                source_bytes: 0,
            }
            .into(),
            fit: ImageFit::Fill,
            order: 0,
            alpha,
        }
    }

    /// An SVG paints scaled into its box. Without fragments it paints in
    /// place, cache or not; a fade wraps it in one layer.
    #[test]
    fn an_svg_paints_scaled_into_its_box() {
        let mut target = Recording::default();
        emit_svg(&mut target, None, &svg(1.0));
        let Command::Fill { transform, .. } = &target.commands()[0] else {
            panic!("expected the svg's fill, got {:?}", target.commands());
        };
        assert_eq!(transform.translation(), (20.0, 0.0).into());
        assert_eq!(transform.as_coeffs()[0], 2.0);

        let mut target = Recording::default();
        let mut cache = FragmentCache::default();
        emit_svg(&mut target, Some(&mut cache), &svg(0.5));
        assert!(cache.is_empty(), "no fragments, nothing to cache");
        let commands = target.commands();
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert!(matches!(commands[0], Command::PushLayer { alpha, .. } if alpha == 0.5));
        assert!(matches!(commands[2], Command::PopLayer));

        let mut empty = svg(1.0);
        empty.intrinsic = glam::Vec2::ZERO;
        let mut target = Recording::default();
        emit_svg(&mut target, None, &empty);
        assert!(target.is_empty());
    }

    /// On a sink with fragments an SVG asset is recorded once, by its id,
    /// and every later paint of it replays the recording.
    #[test]
    fn an_svg_is_recorded_once_per_asset() {
        let mut sink = FragmentSink::default();
        let mut cache = FragmentCache::default();
        let drawing = svg(1.0);

        emit_svg(&mut sink, Some(&mut cache), &drawing);
        emit_svg(&mut sink, Some(&mut cache), &drawing);

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(sink.appended, 2);
        assert_eq!(sink.frame.len(), 2);
        for command in sink.frame.commands() {
            let Command::Fill { transform, .. } = command else {
                panic!("expected fills, got {command:?}");
            };
            assert_eq!(transform.translation(), (20.0, 0.0).into());
        }
    }

    /// One glyph per byte, `advance` apart, on one baseline: enough of a
    /// shaper to place carets and selections.
    struct MonoShaper {
        advance: f32,
    }

    impl TextShaper for MonoShaper {
        fn shape(&mut self, text: &str, _size_px: f32, _opts: ShapeOptions) -> Option<ShapedRun> {
            if text.is_empty() {
                return None;
            }
            let glyphs: Vec<GlyphPosition> = (0..text.len())
                .map(|i| GlyphPosition {
                    id: i as u32 + 1,
                    x: i as f32 * self.advance,
                    y: 0.0,
                    advance: self.advance,
                    byte_start: i as u32,
                    byte_end: i as u32 + 1,
                })
                .collect();
            let width = glyphs.len() as f32 * self.advance;
            let font_data = Arc::new(Vec::new());
            Some(ShapedRun {
                font_data: font_data.clone(),
                font_index: 0,
                glyphs: glyphs.clone(),
                segments: vec![
                    ShapedSegment {
                        font_id: 1,
                        font_data,
                        font_index: 0,
                        normalized_coords: Vec::new(),
                        level: 0,
                        glyphs,
                        width,
                    },
                    ShapedSegment {
                        font_id: 2,
                        font_data: Arc::new(Vec::new()),
                        font_index: 0,
                        normalized_coords: Vec::new(),
                        level: 0,
                        glyphs: Vec::new(),
                        width: 0.0,
                    },
                ],
                width,
            })
        }
    }

    /// A shaped label paints one glyph run per non-empty segment, shifted by
    /// its alignment inside the container.
    #[test]
    fn a_label_paints_its_glyphs_where_its_alignment_puts_them() {
        use lumen_core::components::TextAlign;
        let mut shaper = MonoShaper { advance: 10.0 };
        for (align, x) in [
            (TextAlign::Start, 10.0),
            (TextAlign::Center, 10.0 + 80.0),
            (TextAlign::End, 10.0 + 160.0),
        ] {
            let mut text = label("Save");
            text.align = align;
            let mut target = Recording::default();
            draw_text(&mut shaper, &mut target, &text, 1.0, 1.0);
            assert_eq!(target.len(), 1, "the empty segment paints nothing");
            let Command::Glyphs {
                transform, glyphs, ..
            } = &target.commands()[0]
            else {
                panic!("expected a glyph run, got {:?}", target.commands());
            };
            assert_eq!(glyphs.len(), 4);
            assert_eq!(transform.translation(), (x, 20.0).into(), "{align:?}");
        }
    }

    /// A selection paints its highlight under the glyphs, re-paints the
    /// selected glyphs in the selection foreground inside a clip, and the
    /// caret lands after the byte it follows, in the caret color.
    #[test]
    fn a_selection_and_caret_paint_around_the_glyphs() {
        let mut shaper = MonoShaper { advance: 10.0 };
        let mut field = label("Save");
        field.selection = Some((1, 3));
        field.selection_foreground = Some(LumenColor::rgb(1.0, 1.0, 1.0));
        field.caret = Some(3);
        field.caret_color = Some(red());
        let mut target = Recording::default();

        draw_text(&mut shaper, &mut target, &field, 1.0, 0.5);

        let commands = target.commands();
        let kinds: Vec<&str> = commands
            .iter()
            .map(|c| match c {
                Command::Fill { .. } => "fill",
                Command::Glyphs { .. } => "glyphs",
                Command::PushLayer { .. } => "push",
                Command::PopLayer => "pop",
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["fill", "glyphs", "push", "glyphs", "pop", "fill"]);

        let Command::Fill {
            shape: OwnedShape::Rect(band),
            brush,
            ..
        } = &commands[0]
        else {
            panic!("expected the selection band, got {:?}", commands[0]);
        };
        assert_eq!((band.x0, band.x1), (20.0, 40.0), "bytes 1..3");
        let peniko::Brush::Solid(highlight) = brush else {
            panic!("the highlight is a solid fill, got {brush:?}");
        };
        assert!(
            highlight.components[3] < 1.0,
            "opacity folds into the highlight"
        );

        let Command::Fill {
            shape: OwnedShape::Rect(caret),
            brush,
            ..
        } = &commands[5]
        else {
            panic!("expected the caret, got {:?}", commands[5]);
        };
        assert_eq!(caret.x0, 40.0, "after byte 3");
        let peniko::Brush::Solid(caret_color) = brush else {
            panic!("the caret is a solid fill, got {brush:?}");
        };
        assert_eq!(caret_color.components[0], 1.0);
    }

    /// A caret right after a newline sits at the start of the next line,
    /// one line height down, even though a newline shapes no glyph.
    #[test]
    fn a_caret_after_a_newline_starts_the_next_line() {
        let mut shaper = MonoShaper { advance: 10.0 };
        let mut field = label("ab\n");
        field.caret = Some(3);
        let mut target = Recording::default();

        draw_text(&mut shaper, &mut target, &field, 1.0, 1.0);

        let Some(Command::Fill {
            shape: OwnedShape::Rect(caret),
            ..
        }) = target.commands().last()
        else {
            panic!("expected the caret last, got {:?}", target.commands());
        };
        assert_eq!(caret.x0, 10.0, "the line start is the field origin");
        assert!(caret.y1 > 20.0 + 20.0, "one line height below the first");
    }
}
