//! Glyph outlines, flattened by Lumen for every render backend.
//!
//! Each rasterizer flattens the curves it fills its own way: vello's GPU
//! stages with Euler spirals at a fixed quarter pixel, vello_cpu with its
//! own fixed tolerance. On a large shape the difference is invisible; on a
//! glyph at text sizes it moves edge coverage enough that two backends draw
//! the same text with visibly different edges. So an outline glyph never
//! reaches a rasterizer as curves: [`GlyphOutlines`] reads its outline with
//! skrifa and flattens it in target pixels at [`GLYPH_TOLERANCE`], and the
//! sink fills the polygons. Every backend then rasterizes the same lines.
//!
//! A flattened outline depends on the glyph, its size, the variation
//! instance, and the linear part of the transform it is drawn under, never
//! on where it lands. So it is cached under those and moved to each position
//! it is drawn at; static text is not reflattened from frame to frame. A
//! sink chooses the form it keeps each flattened glyph in ([`GlyphShape`]),
//! so a rasterizer that takes encoded paths stores them encoded once.
//!
//! Colour glyphs (COLR and bitmap faces) are not outlines.
//! [`GlyphOutlines::has_outlines`] says so for a run, and the sink draws
//! that run its own way.

use crate::{GlyphRun, Painter, Shape};
use peniko::kurbo::{Affine, BezPath, PathEl, Point, Rect, flatten};
use peniko::{BrushRef, Fill};
use rustc_hash::FxHashMap;
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// How far a flattened glyph edge may stray from the true outline, in
/// target pixels.
///
/// Chosen by comparing text drawn at a range of tolerances with the same
/// text supersampled from near-exact outlines, against what each costs to
/// draw: at this tolerance edge coverage stays within a few percent of the
/// true outline, and a frame of text costs no more on either backend than
/// the curves the rasterizers flattened themselves. Finer tolerances are
/// more accurate still, but cost the CPU backend time in proportion to the
/// extra lines.
pub const GLYPH_TOLERANCE: f64 = 0.05;

/// Frames an unused glyph stays cached.
const MAX_AGE: u64 = 64;

/// Frames between sweeps for unused glyphs.
const SWEEP_EVERY: u64 = 64;

/// One flattened glyph: closed polygons in target pixels, relative to the
/// glyph's origin.
#[derive(Clone, Copy, Debug)]
pub struct Contours<'a> {
    points: &'a [[f32; 2]],
    ends: &'a [u32],
}

impl<'a> Contours<'a> {
    /// Each closed polygon's points, in order. The last point connects back
    /// to the first.
    pub fn iter(&self) -> impl Iterator<Item = &'a [[f32; 2]]> + '_ {
        let points = self.points;
        let mut start = 0usize;
        self.ends.iter().map(move |&end| {
            let contour = &points[start..end as usize];
            start = end as usize;
            contour
        })
    }

    /// The bounding box of every point, relative to the glyph's origin;
    /// empty at the origin for a glyph with no outline.
    pub fn bounds(&self) -> Rect {
        self.points
            .iter()
            .map(|&[x, y]| Point::new(f64::from(x), f64::from(y)))
            .fold(None, |acc: Option<Rect>, p| {
                Some(acc.map_or(Rect::from_points(p, p), |r| r.union_pt(p)))
            })
            .unwrap_or_default()
    }
}

/// The form a sink keeps a flattened glyph in, built once from its
/// [`Contours`] when the glyph is first drawn.
pub trait GlyphShape: Send + 'static {
    /// This sink's form of `contours`.
    fn from_contours(contours: Contours<'_>) -> Self;
}

/// The polygons themselves: the form for a sink that fills paths.
#[derive(Clone, Debug, Default)]
pub struct Polygons {
    points: Box<[[f32; 2]]>,
    ends: Box<[u32]>,
    /// The points' bounding box, relative to the glyph's origin.
    bounds: Rect,
}

impl Polygons {
    /// The polygons, borrowed.
    pub fn contours(&self) -> Contours<'_> {
        Contours {
            points: &self.points,
            ends: &self.ends,
        }
    }

    /// The polygons' bounding box once moved to `origin`.
    pub fn bounds_at(&self, origin: Point) -> Rect {
        self.bounds + origin.to_vec2()
    }

    /// Append the polygons to `path`, moved to `origin`.
    pub fn append_to(&self, origin: Point, path: &mut BezPath) {
        for contour in self.contours().iter() {
            let mut points = contour
                .iter()
                .map(|&[x, y]| Point::new(origin.x + f64::from(x), origin.y + f64::from(y)));
            let Some(first) = points.next() else {
                continue;
            };
            path.move_to(first);
            for point in points {
                path.line_to(point);
            }
            path.close_path();
        }
    }
}

impl GlyphShape for Polygons {
    fn from_contours(contours: Contours<'_>) -> Self {
        Self {
            points: contours.points.into(),
            ends: contours.ends.into(),
            bounds: contours.bounds(),
        }
    }
}

/// Flattened glyphs, the cache every sink draws outline glyphs through.
///
/// A sink keeps one, draws each outline run through it from its
/// [`Painter::draw_glyphs`], and calls [`Self::end_frame`] once a frame so
/// glyphs no longer drawn are let go.
pub struct GlyphOutlines<S: GlyphShape = Polygons> {
    /// Whether a face draws through outlines, by (blob id, face index).
    faces: FxHashMap<(u64, u32), bool>,
    /// Flattened glyphs by everything a flattening depends on but the
    /// variation instance, which [`Instance`] adds.
    runs: FxHashMap<RunKey, Vec<Instance<S>>>,
    /// The path a run is assembled in, kept so its allocation is reused.
    path: BezPath,
    /// Frames ended so far: the clock glyph ages are measured on.
    frame: u64,
}

impl<S: GlyphShape> Default for GlyphOutlines<S> {
    fn default() -> Self {
        Self {
            faces: FxHashMap::default(),
            runs: FxHashMap::default(),
            path: BezPath::new(),
            frame: 0,
        }
    }
}

impl<S: GlyphShape> std::fmt::Debug for GlyphOutlines<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlyphOutlines")
            .field("glyphs", &self.len())
            .finish()
    }
}

/// What a flattening depends on, apart from the glyph and the instance.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct RunKey {
    blob: u64,
    index: u32,
    size: u32,
    /// The run transform's linear part, as bits.
    linear: [u64; 4],
}

/// The glyphs of one variation instance.
struct Instance<S> {
    coords: Box<[i16]>,
    glyphs: FxHashMap<u32, Cached<S>>,
}

/// A flattened glyph and the frame it was last drawn in.
struct Cached<S> {
    shape: S,
    used: u64,
}

impl<S: GlyphShape> GlyphOutlines<S> {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many flattened glyphs are cached.
    pub fn len(&self) -> usize {
        self.runs
            .values()
            .flat_map(|instances| instances.iter())
            .map(|instance| instance.glyphs.len())
            .sum()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether `run`'s face draws its glyphs from outlines. A colour face
    /// (COLR layers or bitmap strikes) does not, and its runs are the
    /// sink's to draw.
    pub fn has_outlines(&mut self, run: &GlyphRun<'_>) -> bool {
        let key = (run.font.data.id(), run.font.index);
        *self.faces.entry(key).or_insert_with(|| {
            FontRef::from_index(run.font.data.as_ref(), run.font.index).is_ok_and(|font| {
                !((font.colr().is_ok() && font.cpal().is_ok()) || !font.bitmap_strikes().is_empty())
            })
        })
    }

    /// Hand each glyph of `run` to `each`, in run order, as its flattened
    /// form and the target-pixel point its origin lands on. Flattens the
    /// glyphs not cached yet. A glyph the face has no outline for comes
    /// through empty.
    pub fn for_each(&mut self, run: &GlyphRun<'_>, mut each: impl FnMut(Point, &S)) {
        let [a, b, c, d, _, _] = run.transform.as_coeffs();
        let key = RunKey {
            blob: run.font.data.id(),
            index: run.font.index,
            size: run.font_size.to_bits(),
            linear: [a.to_bits(), b.to_bits(), c.to_bits(), d.to_bits()],
        };
        let frame = self.frame;
        let instances = self.runs.entry(key).or_default();
        let at = match instances
            .iter()
            .position(|i| *i.coords == *run.normalized_coords)
        {
            Some(at) => at,
            None => {
                instances.push(Instance {
                    coords: run.normalized_coords.into(),
                    glyphs: FxHashMap::default(),
                });
                instances.len() - 1
            }
        };
        let glyphs = &mut instances[at].glyphs;
        // The outline reader, opened the first time this run misses.
        let mut reader: Option<Reader<'_>> = None;
        for glyph in run.glyphs {
            let cached = glyphs.entry(glyph.id).or_insert_with(|| {
                let reader = reader.get_or_insert_with(|| Reader::new(run));
                Cached {
                    shape: reader.flatten(glyph.id),
                    used: frame,
                }
            });
            cached.used = frame;
            let origin = run.transform * Point::new(f64::from(glyph.x), f64::from(glyph.y));
            each(origin, &cached.shape);
        }
    }

    /// Mark the end of a frame, and every so often let go of the glyphs no
    /// recent frame drew.
    pub fn end_frame(&mut self) {
        self.frame += 1;
        if !self.frame.is_multiple_of(SWEEP_EVERY) {
            return;
        }
        let frame = self.frame;
        self.runs.retain(|_, instances| {
            instances.retain_mut(|instance| {
                instance
                    .glyphs
                    .retain(|_, cached| frame - cached.used <= MAX_AGE);
                !instance.glyphs.is_empty()
            });
            !instances.is_empty()
        });
    }
}

impl GlyphOutlines<Polygons> {
    /// Fill `run` into `painter` as its flattened polygons, one fill for the
    /// whole run under the nonzero rule, the way a run's glyphs combine into
    /// one shape. Returns `false`, having painted nothing, when the run's
    /// face draws colour glyphs ([`Self::has_outlines`]); the caller then
    /// draws the run itself.
    ///
    /// Glyphs wholly outside `visible`, in target pixels, are left out of the
    /// fill; pass the target's bounds, outside which nothing shows anyway.
    pub fn draw(&mut self, painter: &mut dyn Painter, run: &GlyphRun<'_>, visible: Rect) -> bool {
        if !self.has_outlines(run) {
            return false;
        }
        let mut path = std::mem::take(&mut self.path);
        path.truncate(0);
        self.for_each(run, |origin, polygons| {
            if polygons.bounds_at(origin).overlaps(visible) {
                polygons.append_to(origin, &mut path);
            }
        });
        if !path.elements().is_empty() {
            painter.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                run.brush,
                brush_transform(run),
                &Shape::Path(&path),
            );
        }
        self.path = path;
        true
    }
}

/// The brush transform a run's fill takes when its path is in target pixels
/// already: the run transform, the space the brush was given in. `None` for
/// a solid colour, which has no geometry to map.
pub fn brush_transform(run: &GlyphRun<'_>) -> Option<Affine> {
    match run.brush {
        BrushRef::Solid(_) => None,
        _ => Some(run.transform),
    }
}

/// Reads and flattens the outlines of one run's glyphs.
struct Reader<'a> {
    font: Option<FontRef<'a>>,
    size: Size,
    coords: Vec<NormalizedCoord>,
    /// The run transform's linear part composed with the flip from the
    /// outline's y-up units to the run's y-down ones.
    linear: Affine,
    outline: BezPath,
    points: Vec<[f32; 2]>,
    ends: Vec<u32>,
}

impl<'a> Reader<'a> {
    fn new(run: &GlyphRun<'a>) -> Self {
        let [a, b, c, d, _, _] = run.transform.as_coeffs();
        Self {
            font: FontRef::from_index(run.font.data.as_ref(), run.font.index).ok(),
            size: Size::new(run.font_size),
            coords: run
                .normalized_coords
                .iter()
                .map(|&bits| NormalizedCoord::from_bits(bits))
                .collect(),
            linear: Affine::new([a, b, c, d, 0.0, 0.0]) * Affine::FLIP_Y,
            outline: BezPath::new(),
            points: Vec::new(),
            ends: Vec::new(),
        }
    }

    /// Glyph `id` flattened into the sink's form; empty when the face has no
    /// outline for it.
    fn flatten<S: GlyphShape>(&mut self, id: u32) -> S {
        self.outline.truncate(0);
        self.points.clear();
        self.ends.clear();
        let drawn = self.font.as_ref().and_then(|font| {
            let glyph = font.outline_glyphs().get(GlyphId::new(id))?;
            let settings = DrawSettings::unhinted(self.size, LocationRef::new(&self.coords));
            glyph.draw(settings, &mut Pen(&mut self.outline)).ok()
        });
        if drawn.is_some() {
            let linear = self.linear;
            let (points, ends) = (&mut self.points, &mut self.ends);
            let mut open = false;
            flatten(
                self.outline.iter().map(|el| linear * el),
                GLYPH_TOLERANCE,
                |el| match el {
                    PathEl::MoveTo(p) => {
                        if open {
                            ends.push(points.len() as u32);
                        }
                        points.push([p.x as f32, p.y as f32]);
                        open = true;
                    }
                    PathEl::LineTo(p) => points.push([p.x as f32, p.y as f32]),
                    PathEl::ClosePath => {
                        if open {
                            ends.push(points.len() as u32);
                            open = false;
                        }
                    }
                    // `flatten` emits lines only.
                    PathEl::QuadTo(..) | PathEl::CurveTo(..) => {}
                },
            );
            if open {
                ends.push(points.len() as u32);
            }
            let mut start = 0usize;
            for &end in self.ends.iter() {
                lean_vertical_edges(&mut self.points[start..end as usize]);
                start = end as usize;
            }
        }
        S::from_contours(Contours {
            points: &self.points,
            ends: &self.ends,
        })
    }
}

/// How far a glyph edge is leaned off the vertical, in target pixels.
const LEAN: f32 = 1.0 / 256.0;

/// Lean every exactly (or nearly) vertical edge of a closed polygon by
/// [`LEAN`], moving no point further than that.
///
/// vello's GPU stages work out the coverage of a pixel an edge crosses by
/// dividing by the edge's horizontal extent. For a vertical edge that
/// extent is a millionth of a pixel, and f32 rounding then costs the pixel
/// a few percent of its coverage: a glyph stem draws visibly heavier or
/// lighter than the same stem on the CPU. An edge leaned by a fraction of a
/// pixel divides by a width the rounding cannot touch, and the lean itself
/// changes no pixel's coverage by more than about one level in 255. Every
/// backend fills the same leaned polygon.
fn lean_vertical_edges(points: &mut [[f32; 2]]) {
    let Some(&[first, _]) = points.first() else {
        return;
    };
    let upright = |a: f32, b: f32| (a - b).abs() < LEAN / 2.0;
    let last = points.len() - 1;
    let mut prev = first;
    for (i, point) in points.iter_mut().enumerate().skip(1) {
        let x = point[0];
        // The last point's edge back to the first closes the polygon.
        let bad = |c: f32| upright(c, prev) || (i == last && upright(c, first));
        if bad(x)
            && let Some(leaned) = [x + LEAN, x - LEAN].into_iter().find(|&c| !bad(c))
        {
            point[0] = leaned;
        }
        prev = point[0];
    }
}

/// Collects an outline skrifa draws into a path.
struct Pen<'a>(&'a mut BezPath);

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((f64::from(x), f64::from(y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((f64::from(x), f64::from(y)));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(x), f64::from(y)),
        );
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.curve_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(cx1), f64::from(cy1)),
            (f64::from(x), f64::from(y)),
        );
    }

    fn close(&mut self) {
        self.0.close_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::{Command, OwnedShape, Recording};
    use lumen_text::GlyphPosition;
    use peniko::{Blob, Color, FontData};
    use std::sync::Arc;

    fn font() -> FontData {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../public/lumenc/tests/goldens/fonts/NotoSans-Regular.ttf"
        );
        let bytes = std::fs::read(path).expect("the pinned golden font");
        FontData::new(Blob::new(Arc::new(bytes)), 0)
    }

    fn glyph(id: u32, x: f32) -> GlyphPosition {
        GlyphPosition {
            id,
            x,
            y: 0.0,
            advance: 10.0,
            byte_start: 0,
            byte_end: 1,
        }
    }

    fn run<'a>(font: &'a FontData, glyphs: &'a [GlyphPosition], at: (f64, f64)) -> GlyphRun<'a> {
        GlyphRun {
            font,
            font_size: 16.0,
            normalized_coords: &[],
            transform: Affine::translate(at),
            brush: Color::WHITE.into(),
            glyphs,
        }
    }

    const EVERYWHERE: Rect = Rect::new(-1e6, -1e6, 1e6, 1e6);

    /// The one path a drawn run filled.
    fn filled(sink: &Recording) -> &BezPath {
        let fills: Vec<&BezPath> = sink
            .commands()
            .iter()
            .filter_map(|c| match c {
                Command::Fill {
                    shape: OwnedShape::Path(p),
                    ..
                } => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(fills.len(), 1, "one fill per run");
        fills[0]
    }

    /// A run fills once, with straight lines only, in target pixels, and a
    /// solid brush needs no brush transform.
    #[test]
    fn a_run_fills_once_with_lines_in_target_pixels() {
        let font = font();
        let glyphs = [glyph(50, 0.0), glyph(51, 12.0)];
        let mut outlines = GlyphOutlines::new();
        let mut sink = Recording::default();
        assert!(outlines.draw(&mut sink, &run(&font, &glyphs, (40.0, 30.0)), EVERYWHERE));
        let path = filled(&sink);
        assert!(
            path.elements().iter().all(|el| matches!(
                el,
                PathEl::MoveTo(_) | PathEl::LineTo(_) | PathEl::ClosePath
            )),
            "flattened to lines"
        );
        let bounds = peniko::kurbo::Shape::bounding_box(path);
        assert!(bounds.x0 >= 40.0 && bounds.x1 <= 80.0, "{bounds:?}");
        assert!(
            bounds.y0 < 20.0,
            "glyphs rise above the baseline: {bounds:?}"
        );
        assert!(matches!(
            sink.commands()[0],
            Command::Fill {
                transform: Affine::IDENTITY,
                brush_transform: None,
                ..
            }
        ));
    }

    /// A glyph is flattened once: drawing it again, anywhere, reuses the
    /// cached polygons moved to the new spot.
    #[test]
    fn a_glyph_flattens_once_and_moves() {
        let font = font();
        let glyphs = [glyph(50, 0.0), glyph(50, 20.0)];
        let mut outlines = GlyphOutlines::new();
        let mut first = Recording::default();
        outlines.draw(&mut first, &run(&font, &glyphs, (10.0, 30.0)), EVERYWHERE);
        assert_eq!(outlines.len(), 1, "one glyph, cached once");
        let mut second = Recording::default();
        outlines.draw(&mut second, &run(&font, &glyphs, (13.25, 30.5)), EVERYWHERE);
        assert_eq!(outlines.len(), 1);
        let (a, b) = (filled(&first).elements(), filled(&second).elements());
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(b) {
            if let (PathEl::LineTo(p), PathEl::LineTo(q)) = (a, b) {
                assert!((q.x - p.x - 3.25).abs() < 1e-4 && (q.y - p.y - 0.5).abs() < 1e-4);
            }
        }
    }

    /// A different size is a different flattening.
    #[test]
    fn another_size_flattens_again() {
        let font = font();
        let glyphs = [glyph(50, 0.0)];
        let mut outlines = GlyphOutlines::new();
        let mut sink = Recording::default();
        outlines.draw(&mut sink, &run(&font, &glyphs, (0.0, 30.0)), EVERYWHERE);
        let mut bigger = run(&font, &glyphs, (0.0, 30.0));
        bigger.font_size = 32.0;
        outlines.draw(&mut sink, &bigger, EVERYWHERE);
        assert_eq!(outlines.len(), 2);
    }

    /// Glyphs no recent frame drew are let go.
    #[test]
    fn unused_glyphs_age_out() {
        let font = font();
        let glyphs = [glyph(50, 0.0)];
        let mut outlines = GlyphOutlines::new();
        outlines.draw(
            &mut Recording::default(),
            &run(&font, &glyphs, (0.0, 30.0)),
            EVERYWHERE,
        );
        for _ in 0..MAX_AGE {
            outlines.end_frame();
        }
        assert_eq!(outlines.len(), 1, "still recent");
        for _ in 0..=SWEEP_EVERY {
            outlines.end_frame();
        }
        assert!(outlines.is_empty(), "aged out: {outlines:?}");
    }

    /// Glyphs wholly outside the visible area are left out, and a run with
    /// nothing visible fills nothing.
    #[test]
    fn glyphs_outside_the_visible_area_are_left_out() {
        let font = font();
        let glyphs = [glyph(50, 0.0)];
        let mut outlines = GlyphOutlines::new();
        let mut sink = Recording::default();
        let visible = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(outlines.draw(&mut sink, &run(&font, &glyphs, (500.0, 30.0)), visible));
        assert!(sink.is_empty());
        outlines.draw(&mut sink, &run(&font, &glyphs, (95.0, 30.0)), visible);
        assert_eq!(sink.len(), 1, "a glyph partly in view still draws");
    }

    /// A face that cannot be read is not drawn here: the sink draws the run
    /// its own way.
    #[test]
    fn an_unreadable_face_is_left_to_the_sink() {
        let font = FontData::new(Blob::new(Arc::new(vec![0u8; 16])), 0);
        let glyphs = [glyph(1, 0.0)];
        let mut outlines = GlyphOutlines::new();
        let mut sink = Recording::default();
        assert!(!outlines.draw(&mut sink, &run(&font, &glyphs, (0.0, 0.0)), EVERYWHERE));
        assert!(sink.is_empty());
    }

    /// No edge of a flattened glyph is vertical, and leaning one moves no
    /// point further than the lean.
    #[test]
    fn no_flattened_edge_is_vertical() {
        let font = font();
        let glyphs: Vec<GlyphPosition> = (36..90).map(|id| glyph(id, 0.0)).collect();
        let mut outlines = GlyphOutlines::new();
        let mut contours = 0;
        outlines.for_each(
            &run(&font, &glyphs, (0.0, 0.0)),
            |_, polygons: &Polygons| {
                for contour in polygons.contours().iter() {
                    contours += 1;
                    for (i, a) in contour.iter().enumerate() {
                        let b = contour[(i + 1) % contour.len()];
                        assert!(
                            (b[0] - a[0]).abs() >= LEAN / 2.0,
                            "a vertical edge {a:?} -> {b:?}"
                        );
                    }
                }
            },
        );
        assert!(contours > 50, "{contours}");
        let original = [[0.0, 0.0], [0.0, 4.0], [4.0, 4.0], [4.0, 0.0]];
        let mut square = original;
        lean_vertical_edges(&mut square);
        for (point, original) in square.iter().zip(original) {
            assert!((point[0] - original[0]).abs() <= LEAN && point[1] == original[1]);
        }
        for i in 0..4 {
            let (a, b) = (square[i], square[(i + 1) % 4]);
            assert!(
                a[1] == b[1] || (b[0] - a[0]).abs() >= LEAN / 2.0,
                "{square:?}"
            );
        }
    }
}
