//! Painting a parsed SVG through a [`Painter`].
//!
//! - Walks groups and emits fills and strokes for paths.
//! - Honors `<clipPath>` with a clip layer.
//! - Supports linear and radial gradients; pattern paints are dropped with a `tracing::warn!`.
//! - Embedded raster images and SVG text nodes are dropped with a `tracing::warn!` rather than panicking.

use crate::{Painter, Shape};
use peniko::color::{AlphaColor, Srgb};
use peniko::kurbo::{Affine, BezPath, Point, Stroke};
use peniko::{Brush, BrushRef, Color, ColorStop, Fill, Gradient};

/// Paint every node of `tree` under `transform`.
pub fn paint_svg(painter: &mut dyn Painter, tree: &usvg::Tree, transform: Affine) {
    render_group(painter, tree.root(), transform);
}

fn render_group(painter: &mut dyn Painter, group: &usvg::Group, parent_xform: Affine) {
    let xform = parent_xform * to_affine(group.transform());

    // When the group carries a clip-path, wrap its children in a clip layer.
    // Masks are not handled here: alpha-mask composition needs a luminance
    // pass the painter does not expose.
    let clip_pushed = if let Some(clip) = group.clip_path() {
        let path = clip_path_to_bezpath(clip);
        painter.push_layer(
            Fill::NonZero,
            peniko::BlendMode::default(),
            1.0,
            xform,
            &Shape::Path(&path),
        );
        true
    } else {
        false
    };

    for node in group.children() {
        match node {
            usvg::Node::Group(g) => render_group(painter, g, xform),
            usvg::Node::Path(p) => render_path(painter, p, xform),
            usvg::Node::Image(_) | usvg::Node::Text(_) => {
                // Skip nested rasters and SVG text nodes, emitting one tracing warning per occurrence.
                tracing::warn!("svg: dropping unsupported node (Image / Text)");
            }
        }
    }

    if clip_pushed {
        painter.pop_layer();
    }
}

/// Walks every path inside a `<clipPath>` body and unions them into a single [`BezPath`].
/// Per-path fill rules are treated as NonZero and the clip path's own transform is ignored.
fn clip_path_to_bezpath(clip: &usvg::ClipPath) -> BezPath {
    let mut out = BezPath::new();
    gather_paths_into(&mut out, clip.root());
    if out.is_empty() {
        // Fall back to a large no-op rectangle when the clip body produced no recognisable paths.
        out.move_to(Point::new(-1.0e6, -1.0e6));
        out.line_to(Point::new(1.0e6, -1.0e6));
        out.line_to(Point::new(1.0e6, 1.0e6));
        out.line_to(Point::new(-1.0e6, 1.0e6));
        out.close_path();
    }
    out
}

fn gather_paths_into(out: &mut BezPath, group: &usvg::Group) {
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => gather_paths_into(out, g),
            usvg::Node::Path(p) => {
                let bez = tinyskia_path_to_bezpath(p.data());
                for el in bez.elements() {
                    out.push(*el);
                }
            }
            _ => {}
        }
    }
}

fn render_path(painter: &mut dyn Painter, path: &usvg::Path, xform: Affine) {
    if !path.is_visible() {
        return;
    }
    let bez = tinyskia_path_to_bezpath(path.data());
    if let Some(fill) = path.fill()
        && let Some(brush) = paint_to_brush(fill.paint(), fill.opacity().get())
    {
        let rule = match fill.rule() {
            usvg::FillRule::NonZero => Fill::NonZero,
            usvg::FillRule::EvenOdd => Fill::EvenOdd,
        };
        painter.fill(
            rule,
            xform,
            BrushRef::from(&brush),
            None,
            &Shape::Path(&bez),
        );
    }
    if let Some(stroke) = path.stroke()
        && let Some(brush) = paint_to_brush(stroke.paint(), stroke.opacity().get())
    {
        let style = Stroke::new(stroke.width().get() as f64);
        painter.stroke(
            &style,
            xform,
            BrushRef::from(&brush),
            None,
            &Shape::Path(&bez),
        );
    }
}

fn tinyskia_path_to_bezpath(path: &usvg::tiny_skia_path::Path) -> BezPath {
    let mut out = BezPath::new();
    for seg in path.segments() {
        use usvg::tiny_skia_path::PathSegment;
        match seg {
            PathSegment::MoveTo(p) => out.move_to(pt(p)),
            PathSegment::LineTo(p) => out.line_to(pt(p)),
            PathSegment::QuadTo(c, p) => out.quad_to(pt(c), pt(p)),
            PathSegment::CubicTo(c1, c2, p) => out.curve_to(pt(c1), pt(c2), pt(p)),
            PathSegment::Close => out.close_path(),
        }
    }
    out
}

fn pt(p: usvg::tiny_skia_path::Point) -> Point {
    Point::new(p.x as f64, p.y as f64)
}

fn to_affine(t: usvg::Transform) -> Affine {
    Affine::new([
        t.sx as f64,
        t.ky as f64,
        t.kx as f64,
        t.sy as f64,
        t.tx as f64,
        t.ty as f64,
    ])
}

/// Maps `usvg::Paint` to a `peniko::Brush`, handling solid color, linear, and radial gradients.
/// Pattern paints return `None` and emit a `tracing::warn!`.
fn paint_to_brush(paint: &usvg::Paint, opacity: f32) -> Option<Brush> {
    match paint {
        usvg::Paint::Color(c) => {
            let a = (opacity.clamp(0.0, 1.0) * 255.0) as u8;
            Some(Brush::Solid(Color::from(AlphaColor::<Srgb>::from_rgba8(
                c.red, c.green, c.blue, a,
            ))))
        }
        usvg::Paint::LinearGradient(g) => {
            let stops = stops_from(g.stops(), opacity);
            if stops.is_empty() {
                return None;
            }
            let start = Point::new(g.x1() as f64, g.y1() as f64);
            let end = Point::new(g.x2() as f64, g.y2() as f64);
            Some(Brush::Gradient(
                Gradient::new_linear(start, end).with_stops(stops.as_slice()),
            ))
        }
        usvg::Paint::RadialGradient(g) => {
            let stops = stops_from(g.stops(), opacity);
            if stops.is_empty() {
                return None;
            }
            let center = Point::new(g.cx() as f64, g.cy() as f64);
            let r = g.r().get();
            Some(Brush::Gradient(
                Gradient::new_radial(center, r).with_stops(stops.as_slice()),
            ))
        }
        usvg::Paint::Pattern(_) => {
            tracing::warn!("svg: dropping pattern paint (unsupported)");
            None
        }
    }
}

/// Maps `usvg::Stop` entries to `peniko::ColorStop`, multiplying each stop's alpha by the supplied `opacity`
/// so per-element transparency lands directly in the gradient.
fn stops_from(stops: &[usvg::Stop], opacity: f32) -> Vec<ColorStop> {
    stops
        .iter()
        .map(|s| {
            let alpha = (s.opacity().get() * opacity).clamp(0.0, 1.0);
            let c = s.color();
            let color =
                AlphaColor::<Srgb>::from_rgba8(c.red, c.green, c.blue, (alpha * 255.0) as u8);
            ColorStop {
                offset: s.offset().get(),
                color: color.into(),
            }
        })
        .collect()
}
