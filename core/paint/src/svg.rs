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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::{Command, OwnedShape, Recording};

    fn parse(svg: &str) -> usvg::Tree {
        usvg::Tree::from_str(svg, &usvg::Options::default()).expect("valid svg")
    }

    fn paint(svg: &str, transform: Affine) -> Recording {
        let mut recording = Recording::default();
        paint_svg(&mut recording, &parse(svg), transform);
        recording
    }

    fn solid_alpha(brush: &Brush) -> [f32; 4] {
        match brush {
            Brush::Solid(c) => c.components,
            other => panic!("expected a solid brush, got {other:?}"),
        }
    }

    /// Fills and strokes come from the path's own paint: the fill rule, the
    /// stroke width, and each paint's opacity folded into its alpha. Curves
    /// keep their kind, so a quad stays a quad and a cubic a cubic.
    #[test]
    fn a_path_paints_its_fill_then_its_stroke() {
        let drawing = paint(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
              <path d="M0 0 L10 0 Q15 5 10 10 C5 15 0 15 0 10 Z" fill="#ff0000"
                    fill-rule="evenodd" stroke="#0000ff" stroke-width="2" stroke-opacity="0.5"/>
              <rect width="5" height="5" fill="none" stroke="#00ff00"/>
            </svg>"##,
            Affine::IDENTITY,
        );
        let commands = drawing.commands();
        assert_eq!(commands.len(), 3, "{commands:?}");
        let Command::Fill {
            style,
            brush,
            shape,
            ..
        } = &commands[0]
        else {
            panic!("expected the fill first, got {:?}", commands[0]);
        };
        assert_eq!(*style, Fill::EvenOdd);
        assert_eq!(solid_alpha(brush), [1.0, 0.0, 0.0, 1.0]);
        let OwnedShape::Path(path) = shape else {
            panic!("an svg path paints as a path, got {shape:?}");
        };
        use peniko::kurbo::PathEl;
        let kinds: Vec<&str> = path
            .elements()
            .iter()
            .map(|el| match el {
                PathEl::MoveTo(_) => "M",
                PathEl::LineTo(_) => "L",
                PathEl::QuadTo(..) => "Q",
                PathEl::CurveTo(..) => "C",
                PathEl::ClosePath => "Z",
            })
            .collect();
        assert_eq!(kinds, ["M", "L", "Q", "C", "Z"]);

        let Command::Stroke { style, brush, .. } = &commands[1] else {
            panic!("expected the stroke second, got {:?}", commands[1]);
        };
        assert_eq!(style.width, 2.0);
        let [r, g, b, a] = solid_alpha(brush);
        assert_eq!([r, g, b], [0.0, 0.0, 1.0]);
        assert!(
            (a - 0.5).abs() < 0.01,
            "stroke-opacity folds into alpha: {a}"
        );

        assert!(
            matches!(&commands[2], Command::Stroke { style, .. } if style.width == 1.0),
            "fill=none paints only the stroke: {:?}",
            commands[2],
        );
    }

    /// Linear and radial gradients keep their geometry and stops, with the
    /// element's fill opacity multiplied into every stop. A pattern paint
    /// has no brush and paints nothing.
    #[test]
    fn gradients_become_brushes_and_patterns_are_dropped() {
        let drawing = paint(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
              <defs>
                <linearGradient id="l" x1="0" y1="0" x2="20" y2="0" gradientUnits="userSpaceOnUse">
                  <stop offset="0" stop-color="#ff0000"/>
                  <stop offset="1" stop-color="#0000ff" stop-opacity="0.5"/>
                </linearGradient>
                <radialGradient id="r" cx="10" cy="10" r="8" gradientUnits="userSpaceOnUse">
                  <stop offset="0" stop-color="#ffffff"/>
                  <stop offset="1" stop-color="#000000"/>
                </radialGradient>
                <pattern id="p" width="4" height="4" patternUnits="userSpaceOnUse">
                  <rect width="2" height="2" fill="#000"/>
                </pattern>
              </defs>
              <rect width="20" height="10" fill="url(#l)" fill-opacity="0.5"/>
              <rect y="10" width="20" height="10" fill="url(#r)"/>
              <rect width="20" height="20" fill="url(#p)"/>
            </svg>"##,
            Affine::IDENTITY,
        );
        let commands = drawing.commands();
        assert_eq!(commands.len(), 2, "the pattern fill paints nothing");

        let Command::Fill {
            brush: Brush::Gradient(linear),
            ..
        } = &commands[0]
        else {
            panic!("expected a linear gradient fill, got {:?}", commands[0]);
        };
        let peniko::GradientKind::Linear(pos) = linear.kind else {
            panic!("expected a linear gradient, got {:?}", linear.kind);
        };
        assert_eq!((pos.start, pos.end), (Point::ZERO, Point::new(20.0, 0.0)));
        let alphas: Vec<f32> = linear.stops.iter().map(|s| s.color.components[3]).collect();
        assert!((alphas[0] - 0.5).abs() < 0.01, "{alphas:?}");
        assert!((alphas[1] - 0.25).abs() < 0.01, "{alphas:?}");

        let Command::Fill {
            brush: Brush::Gradient(radial),
            ..
        } = &commands[1]
        else {
            panic!("expected a radial gradient fill, got {:?}", commands[1]);
        };
        let peniko::GradientKind::Radial(pos) = radial.kind else {
            panic!("expected a radial gradient, got {:?}", radial.kind);
        };
        assert_eq!(pos.end_center, Point::new(10.0, 10.0));
        assert_eq!(pos.end_radius, 8.0);
        assert_eq!(radial.stops.len(), 2);
    }

    /// A group's transform composes under the caller's, and its clip path
    /// wraps the group's children in one clip layer that closes after them.
    #[test]
    fn a_clipped_group_paints_inside_a_layer_under_its_transform() {
        let drawing = paint(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
              <defs><clipPath id="c"><rect width="6" height="6"/></clipPath></defs>
              <g transform="translate(5,5)" clip-path="url(#c)">
                <rect width="10" height="10" fill="#00ff00"/>
              </g>
            </svg>"##,
            Affine::translate((100.0, 0.0)),
        );
        let commands = drawing.commands();
        assert_eq!(commands.len(), 3, "{commands:?}");
        let Command::PushLayer {
            transform, clip, ..
        } = &commands[0]
        else {
            panic!("expected the clip layer first, got {:?}", commands[0]);
        };
        assert_eq!(transform.translation(), (105.0, 5.0).into());
        let OwnedShape::Path(clip) = clip else {
            panic!("the clip is the clip path's outline, got {clip:?}");
        };
        use peniko::kurbo::Shape as _;
        assert_eq!(
            clip.bounding_box(),
            peniko::kurbo::Rect::new(0.0, 0.0, 6.0, 6.0)
        );
        let Command::Fill { transform, .. } = &commands[1] else {
            panic!("expected the clipped fill, got {:?}", commands[1]);
        };
        assert_eq!(transform.translation(), (105.0, 5.0).into());
        assert!(matches!(commands[2], Command::PopLayer));
        assert_eq!(drawing.layer_depth(), 0);
    }

    /// Paths usvg keeps but marks invisible, and raster images embedded in
    /// the document, paint nothing and do not stop the walk.
    #[test]
    fn invisible_paths_and_embedded_images_paint_nothing() {
        let drawing = paint(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
              <rect width="10" height="10" fill="#00ff00" visibility="collapse"/>
              <image width="1" height="1" href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="/>
              <rect x="10" width="10" height="10" fill="#0000ff"/>
            </svg>"##,
            Affine::IDENTITY,
        );
        assert_eq!(drawing.len(), 1, "{:?}", drawing.commands());
        assert!(matches!(
            &drawing.commands()[0],
            Command::Fill { brush, .. } if solid_alpha(brush) == [0.0, 0.0, 1.0, 1.0]
        ));
    }

    /// A clip path that reaches its shapes through `<use>` still clips to
    /// them: the referenced shape sits in a nested group of the clip body.
    #[test]
    fn a_clip_body_unions_shapes_from_nested_groups() {
        let drawing = paint(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20">
              <defs>
                <rect id="r" x="2" y="2" width="4" height="4"/>
                <clipPath id="c"><rect width="1" height="1"/><use xlink:href="#r"/></clipPath>
              </defs>
              <rect width="10" height="10" fill="#00ff00" clip-path="url(#c)"/>
            </svg>"##,
            Affine::IDENTITY,
        );
        let Some(Command::PushLayer {
            clip: OwnedShape::Path(clip),
            ..
        }) = drawing.commands().first()
        else {
            panic!("expected a clip layer, got {:?}", drawing.commands());
        };
        use peniko::kurbo::Shape as _;
        assert_eq!(
            clip.bounding_box(),
            peniko::kurbo::Rect::new(0.0, 0.0, 6.0, 6.0),
            "both the direct rect and the used one are in the clip",
        );
    }
}
