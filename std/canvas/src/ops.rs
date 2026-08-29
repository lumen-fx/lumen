//! What a canvas call records, and the drawing state it plays back against.
//!
//! A script function body does no drawing. It appends one [`Op`] to the
//! surface's journal and returns, because the body runs on the script host's
//! stack with no world access at all, while drawing needs the app's fonts and
//! its own scene. One system per tick replays the journal into a retained
//! vello scene ([`crate::encode`]); that is also what makes a canvas cheap
//! when nothing changed, since a tick with an empty journal re-encodes
//! nothing.
//!
//! The state a replay carries lives in [`GfxState`] and persists across
//! ticks, so a script that sets a fill in one handler and draws in the next
//! gets the fill it set.

use lumen_module::lumen_render_wgpu::vello::peniko::kurbo::{Affine, BezPath, Point};

use crate::color::Rgba;

/// How a stroked line ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineCap {
    /// Cut flat at the endpoint.
    #[default]
    Butt,
    /// Half-disc past the endpoint.
    Round,
    /// Half-square past the endpoint.
    Square,
}

impl LineCap {
    /// Parse the CSS `lineCap` spelling, or `None`.
    #[must_use]
    pub fn parse(name: &str) -> Option<LineCap> {
        match name.trim().to_ascii_lowercase().as_str() {
            "butt" => Some(LineCap::Butt),
            "round" => Some(LineCap::Round),
            "square" => Some(LineCap::Square),
            _ => None,
        }
    }
}

/// How two stroked segments meet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineJoin {
    /// Extend the outer edges to a point.
    #[default]
    Miter,
    /// Arc across the corner.
    Round,
    /// Cut straight across the corner.
    Bevel,
}

impl LineJoin {
    /// Parse the CSS `lineJoin` spelling, or `None`.
    #[must_use]
    pub fn parse(name: &str) -> Option<LineJoin> {
        match name.trim().to_ascii_lowercase().as_str() {
            "miter" => Some(LineJoin::Miter),
            "round" => Some(LineJoin::Round),
            "bevel" => Some(LineJoin::Bevel),
            _ => None,
        }
    }
}

/// One recorded canvas call.
#[derive(Clone, Debug)]
pub enum Op {
    /// Drop the current path and start a new one.
    BeginPath,
    /// Start a new subpath at a point.
    MoveTo(f64, f64),
    /// Straight segment to a point.
    LineTo(f64, f64),
    /// Quadratic segment through a control point.
    QuadTo(f64, f64, f64, f64),
    /// Cubic segment through two control points.
    BezierTo(f64, f64, f64, f64, f64, f64),
    /// Circular arc, angles in radians, sweeping the shorter way from
    /// `start` to `end` in the increasing direction.
    Arc {
        /// Center x.
        x: f64,
        /// Center y.
        y: f64,
        /// Radius.
        radius: f64,
        /// Start angle, radians.
        start: f64,
        /// End angle, radians.
        end: f64,
    },
    /// Closed rectangle subpath.
    Rect(f64, f64, f64, f64),
    /// Close the current subpath back to its start.
    ClosePath,
    /// Fill the current path with the fill color.
    Fill,
    /// Stroke the current path with the stroke color and width.
    Stroke,
    /// Fill one rectangle without touching the current path.
    FillRect(f64, f64, f64, f64),
    /// Stroke one rectangle without touching the current path.
    StrokeRect(f64, f64, f64, f64),
    /// Set the fill color.
    SetFill(Rgba),
    /// Set the stroke color.
    SetStroke(Rgba),
    /// Set the stroke width, in canvas units.
    SetLineWidth(f64),
    /// Set the stroke's end shape.
    SetLineCap(LineCap),
    /// Set the stroke's corner shape.
    SetLineJoin(LineJoin),
    /// Set the alpha every later draw is multiplied by.
    SetGlobalAlpha(f64),
    /// Push the drawing state.
    Save,
    /// Pop the drawing state.
    Restore,
    /// Translate the transform.
    Translate(f64, f64),
    /// Rotate the transform, radians.
    Rotate(f64),
    /// Scale the transform.
    Scale(f64, f64),
    /// Replace the transform with the identity.
    ResetTransform,
    /// Replace the transform with `[a, b, c, d, e, f]`.
    SetTransform([f64; 6]),
    /// Set the font `fill_text` shapes with.
    SetFont(FontSpec),
    /// Draw text with the fill color, `(x, y)` on the alphabetic baseline.
    FillText {
        /// The text to shape and draw.
        text: String,
        /// Baseline start x.
        x: f64,
        /// Baseline y.
        y: f64,
    },
    /// Draw a pixel buffer at its own size.
    DrawBuffer {
        /// Buffer handle.
        buffer: u32,
        /// Top-left x.
        x: f64,
        /// Top-left y.
        y: f64,
    },
    /// Erase everything the canvas holds and start over.
    Clear,
    /// Set the drawing space, which erases the canvas the way writing
    /// `width` on an HTML canvas does.
    Resize(f32, f32),
    /// Draw a pixel buffer stretched into a box.
    DrawBufferScaled {
        /// Buffer handle.
        buffer: u32,
        /// Top-left x.
        x: f64,
        /// Top-left y.
        y: f64,
        /// Drawn width.
        width: f64,
        /// Drawn height.
        height: f64,
    },
}

/// A parsed `set_font` string: `"[weight] <size>px [family]"`.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    /// Size in canvas units.
    pub size: f32,
    /// CSS weight, 100..900.
    pub weight: u16,
    /// Family name, empty for the app's default.
    pub family: String,
}

impl Default for FontSpec {
    fn default() -> Self {
        FontSpec {
            size: 10.0,
            weight: 400,
            family: String::new(),
        }
    }
}

impl FontSpec {
    /// Parse the shorthand, or `None`.
    ///
    /// The subset is the part of the CSS `font` shorthand a canvas actually
    /// uses: an optional weight (a number, or `bold` / `normal`), a size in
    /// `px`, and an optional family. The size is the one required piece,
    /// because without it there is nothing to shape.
    #[must_use]
    pub fn parse(text: &str) -> Option<FontSpec> {
        let mut spec = FontSpec::default();
        let mut size = None;
        let mut family: Vec<&str> = Vec::new();
        for word in text.split_whitespace() {
            if let Some(px) = word.strip_suffix("px")
                && let Ok(v) = px.parse::<f32>()
                && v > 0.0
            {
                size = Some(v);
                continue;
            }
            if size.is_none() {
                match word.to_ascii_lowercase().as_str() {
                    "bold" => {
                        spec.weight = 700;
                        continue;
                    }
                    "normal" => {
                        spec.weight = 400;
                        continue;
                    }
                    _ => {}
                }
                if let Ok(w) = word.parse::<u16>()
                    && (100..=900).contains(&w)
                {
                    spec.weight = w;
                    continue;
                }
            }
            family.push(word);
        }
        spec.size = size?;
        spec.family = family.join(" ").trim_matches(['"', '\'']).to_string();
        Some(spec)
    }
}

/// The drawing state a replay carries, and what `save` / `restore` move.
#[derive(Clone, Debug)]
pub struct GfxState {
    /// The current transform.
    pub transform: Affine,
    /// The fill color.
    pub fill: Rgba,
    /// The stroke color.
    pub stroke: Rgba,
    /// The stroke width.
    pub line_width: f64,
    /// The stroke's end shape.
    pub line_cap: LineCap,
    /// The stroke's corner shape.
    pub line_join: LineJoin,
    /// The alpha every draw is multiplied by.
    pub global_alpha: f32,
    /// The font `fill_text` shapes with.
    pub font: FontSpec,
}

impl Default for GfxState {
    fn default() -> Self {
        GfxState {
            transform: Affine::IDENTITY,
            fill: Rgba::BLACK,
            stroke: Rgba::BLACK,
            line_width: 1.0,
            line_cap: LineCap::default(),
            line_join: LineJoin::default(),
            global_alpha: 1.0,
            font: FontSpec::default(),
        }
    }
}

/// Everything a replay mutates: the state, the state stack, and the path
/// being built. Kept together so a canvas can be reset in one assignment.
#[derive(Clone, Debug, Default)]
pub struct Gfx {
    /// The live state.
    pub state: GfxState,
    /// What `save` pushed and `restore` pops.
    pub stack: Vec<GfxState>,
    /// The path under construction, in user space.
    pub path: BezPath,
    /// Where the current subpath started, for `close_path`.
    pub subpath_start: Option<Point>,
    /// Where the pen is, so an arc or a curve with no `move_to` before it
    /// still starts somewhere.
    pub pen: Option<Point>,
}

impl Gfx {
    /// Apply the state-machine half of an op. Returns `true` when the op was
    /// one of them, so the encoder knows it has nothing left to draw.
    ///
    /// This is where `save` / `restore` / the transform / the path live,
    /// which is the half that has no vello in it and can be exercised on its
    /// own.
    pub fn apply(&mut self, op: &Op) -> bool {
        match op {
            Op::BeginPath => {
                self.path = BezPath::new();
                self.subpath_start = None;
                self.pen = None;
            }
            Op::MoveTo(x, y) => {
                let p = Point::new(*x, *y);
                self.path.move_to(p);
                self.subpath_start = Some(p);
                self.pen = Some(p);
            }
            Op::LineTo(x, y) => {
                let p = Point::new(*x, *y);
                self.ensure_start(p);
                self.path.line_to(p);
                self.pen = Some(p);
            }
            Op::QuadTo(cx, cy, x, y) => {
                let p = Point::new(*x, *y);
                self.ensure_start(p);
                self.path.quad_to(Point::new(*cx, *cy), p);
                self.pen = Some(p);
            }
            Op::BezierTo(c1x, c1y, c2x, c2y, x, y) => {
                let p = Point::new(*x, *y);
                self.ensure_start(p);
                self.path
                    .curve_to(Point::new(*c1x, *c1y), Point::new(*c2x, *c2y), p);
                self.pen = Some(p);
            }
            Op::Arc {
                x,
                y,
                radius,
                start,
                end,
            } => self.arc(*x, *y, *radius, *start, *end),
            Op::Rect(x, y, w, h) => {
                let start = Point::new(*x, *y);
                self.path.move_to(start);
                self.path.line_to(Point::new(x + w, *y));
                self.path.line_to(Point::new(x + w, y + h));
                self.path.line_to(Point::new(*x, y + h));
                self.path.close_path();
                self.subpath_start = Some(start);
                self.pen = Some(start);
            }
            Op::ClosePath => {
                if self.subpath_start.is_some() {
                    self.path.close_path();
                    self.pen = self.subpath_start;
                }
            }
            Op::SetFill(c) => self.state.fill = *c,
            Op::SetStroke(c) => self.state.stroke = *c,
            Op::SetLineWidth(w) => self.state.line_width = w.max(0.0),
            Op::SetLineCap(cap) => self.state.line_cap = *cap,
            Op::SetLineJoin(join) => self.state.line_join = *join,
            Op::SetGlobalAlpha(a) => self.state.global_alpha = a.clamp(0.0, 1.0) as f32,
            Op::Save => self.stack.push(self.state.clone()),
            Op::Restore => {
                // An unmatched `restore` leaves the state alone, which is
                // what the HTML canvas does. A script that pops too far is
                // usually mid-refactor, and losing its brush over it would
                // hide the bug behind a blank canvas.
                if let Some(prev) = self.stack.pop() {
                    self.state = prev;
                }
            }
            Op::Translate(x, y) => {
                self.state.transform *= Affine::translate((*x, *y));
            }
            Op::Rotate(radians) => {
                self.state.transform *= Affine::rotate(*radians);
            }
            Op::Scale(x, y) => {
                self.state.transform *= Affine::scale_non_uniform(*x, *y);
            }
            Op::ResetTransform => self.state.transform = Affine::IDENTITY,
            Op::SetTransform(coeffs) => self.state.transform = Affine::new(*coeffs),
            Op::SetFont(spec) => self.state.font = spec.clone(),
            _ => return false,
        }
        true
    }

    /// A curve with no `move_to` before it starts where it was told to,
    /// rather than being dropped.
    fn ensure_start(&mut self, fallback: Point) {
        if self.pen.is_none() {
            self.path.move_to(fallback);
            self.subpath_start = Some(fallback);
            self.pen = Some(fallback);
        }
    }

    /// Append a circular arc, joining it to the current subpath if there is
    /// one, as the HTML canvas does.
    fn arc(&mut self, x: f64, y: f64, radius: f64, start: f64, end: f64) {
        use lumen_module::lumen_render_wgpu::vello::peniko::kurbo::Arc as KurboArc;
        use lumen_module::lumen_render_wgpu::vello::peniko::kurbo::Shape;

        let radius = radius.max(0.0);
        let sweep = end - start;
        let arc = KurboArc::new((x, y), (radius, radius), start, sweep, 0.0);
        let first = Point::new(x + radius * start.cos(), y + radius * start.sin());
        match self.pen {
            Some(_) => self.path.line_to(first),
            None => {
                self.path.move_to(first);
                self.subpath_start = Some(first);
            }
        }
        for el in arc.path_elements(0.1).skip(1) {
            self.path.push(el);
        }
        self.pen = Some(Point::new(x + radius * end.cos(), y + radius * end.sin()));
    }

    /// The brush color a fill uses: the fill color at the global alpha.
    #[must_use]
    pub fn fill_brush(&self) -> Rgba {
        self.state.fill.scaled_alpha(self.state.global_alpha)
    }

    /// The brush color a stroke uses: the stroke color at the global alpha.
    #[must_use]
    pub fn stroke_brush(&self) -> Rgba {
        self.state.stroke.scaled_alpha(self.state.global_alpha)
    }
}
