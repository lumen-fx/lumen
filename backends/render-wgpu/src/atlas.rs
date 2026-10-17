//! Living within vello's image atlas.
//!
//! Every image a vello render samples is copied into one atlas texture, an
//! [`ATLAS_SIDE`] pixel square, and stays placed there across renders until
//! the space is needed and two renders have passed without it. Two things
//! about it lose images without a word, and this module is how the renderer
//! keeps them.
//!
//! **A render that samples nothing empties the atlas.** Vello skips its image
//! bookkeeping for a scene with no images, gradients or glyph runs and
//! shrinks the atlas texture to a pixel, while every image it had placed stays
//! recorded as placed. The next render that draws one of them reads an atlas
//! that no longer holds it, and draws nothing there. [`keep_atlas`] gives such
//! a scene one invisible gradient, so every render keeps the atlas.
//!
//! **An image that finds no room is left out of its render.** A frame drawing
//! four 4096 x 4096 images fills the atlas, and a fifth goes missing; a paint
//! program's stack of large layers is the frame that hits this first. A frame
//! whose images would not fit is painted a second time, through a
//! [`SplitFrame`], into as many renders as it takes, none of which holds more
//! than a share of the atlas. Two things keep each render small:
//!
//! - **An image drawn smaller than it is draws through a copy of the size it
//!   lands at.** The draw alone renders into a texture covering the part of
//!   the frame it reaches, and the frame draws that texture one texel per
//!   pixel. Sampling happens once, in that render, so the pixels are the ones
//!   the direct draw would have made. The copy is kept from frame to frame
//!   while the image, its state and its placement stay the same, so an
//!   unchanged layer costs nothing after the first frame.
//! - **A layer whose content outgrows the share renders what it holds so
//!   far** into a texture over its bounds, and carries on from a draw of that
//!   texture. Each open layer keeps its own content apart until it closes, so
//!   the layer's opacity, blend mode or mask then applies to the whole of it,
//!   exactly as it would have to content drawn in one render.
//!
//! Each of these renders is a job the renderer runs, in order, before the
//! frame (see [`Job`]).

use std::collections::{HashMap, HashSet};

use vello::Scene;
use vello::kurbo::{self, Affine, BezPath, Rect, RoundedRect, Shape as _};
use vello::peniko::{
    BlendMode, Blob, Color, Fill, Gradient, ImageAlphaType, ImageBrush, ImageData, ImageFormat,
    ImageQuality, ImageSampler,
};
use vello_encoding::Patch;

use lumen_paint::Shape;

/// The side of vello's image atlas at its largest.
pub(crate) const ATLAS_SIDE: u32 = 8192;

/// The pixels the atlas holds.
pub(crate) const ATLAS_AREA: u64 = ATLAS_SIDE as u64 * ATLAS_SIDE as u64;

/// What one render of a split frame may hold, as a share of the atlas: room
/// for an image placed awkwardly by the atlas's allocator beside the ones the
/// render already holds.
pub(crate) const RENDER_BUDGET: u64 = ATLAS_AREA / 2;

/// The images a frame may hold in one render before it is split: the atlas
/// with room to spare for its allocator, which packs large images
/// imperfectly.
pub(crate) const FRAME_BUDGET: u64 = ATLAS_AREA / 4 * 3;

/// Pixels in an image.
fn area(image: &ImageData) -> u64 {
    u64::from(image.width) * u64::from(image.height)
}

/// The images `scene` samples, by blob id, with their sizes in pixels: what
/// rendering it puts in the atlas.
pub(crate) fn scene_images(scene: &Scene) -> HashMap<u64, u64> {
    scene
        .encoding()
        .resources
        .patches
        .iter()
        .filter_map(|patch| match patch {
            Patch::Image { image, .. } => Some((image.data.id(), area(image))),
            _ => None,
        })
        .collect()
}

/// Give a scene that samples nothing one invisible gradient, so rendering it
/// keeps vello's atlas and the images placed in it (see the module docs).
pub(crate) fn keep_atlas(scene: &mut Scene) {
    if !scene.encoding().resources.patches.is_empty() {
        return;
    }
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &Gradient::new_linear((0.0, 0.0), (1.0, 0.0))
            .with_stops([Color::TRANSPARENT, Color::TRANSPARENT]),
        None,
        &Rect::new(0.0, 0.0, 1.0, 1.0),
    );
}

/// An image with a blob of its own and no bytes: a unique image id the
/// renderer stands a texture in for.
pub(crate) fn stand_in(width: u32, height: u32) -> ImageData {
    ImageData {
        data: Blob::new(std::sync::Arc::new([0u8; 0])),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    }
}

/// A rectangle grown out to whole pixels.
fn whole_pixels(rect: Rect) -> Rect {
    rect.expand()
}

/// Whether `rect` covers no pixel.
fn is_empty(rect: Rect) -> bool {
    !(rect.width() > 0.0 && rect.height() > 0.0)
}

/// A render the renderer runs before the frame: `scene`, in the coordinates
/// of a texture the size of `image`, rendered over transparency into that
/// texture, which then stands in for `image` wherever a later render draws
/// it.
pub(crate) struct Job {
    pub(crate) scene: Scene,
    pub(crate) image: ImageData,
    /// The texture outlives the frame: it holds a kept copy of an image's
    /// draw (see [`Copies`]).
    pub(crate) kept: bool,
}

/// A shrunk draw's copy of the whole image, made where the draw lands up to
/// a whole-pixel shift, so the same copy serves the draw wherever it moves.
///
/// Moving a draw by whole pixels samples the same image texels at every
/// pixel, so a copy made under the draw's scale, rotation and fraction of a
/// pixel draws one texel per pixel at the shifted spot exactly as the image
/// itself would. A paint program panning over a stack of large layers then
/// reuses every layer's copy instead of making each again on every frame.
struct Placement {
    /// The draw's transform less its whole-pixel shift.
    local: Affine,
    /// The pixels the image covers under `local`.
    copy: Rect,
    /// Where the copy's corner lands in the frame.
    at: kurbo::Point,
}

impl Placement {
    /// The largest copy of a whole image worth keeping: one render's share of
    /// the atlas, in a quarter, so a frame holds several beside each other.
    const MAX_AREA: u64 = RENDER_BUDGET / 4;

    /// The whole-image copy for a draw under `transform`, or `None` when the
    /// image would cover too much to keep whole.
    fn whole(image: &ImageData, transform: Affine) -> Option<Placement> {
        let full = Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height));
        let [a, b, c, d, e, f] = transform.as_coeffs();
        let shift = kurbo::Vec2::new(e.floor(), f.floor());
        let local = Affine::new([a, b, c, d, e - shift.x, f - shift.y]);
        let copy = whole_pixels(local.transform_rect_bbox(full));
        let placement = Placement {
            local,
            copy,
            at: copy.origin() + shift,
        };
        (!is_empty(copy)
            && copy.width() <= f64::from(ATLAS_SIDE)
            && copy.height() <= f64::from(ATLAS_SIDE)
            && placement.area() <= Self::MAX_AREA)
            .then_some(placement)
    }

    fn area(&self) -> u64 {
        self.copy.width() as u64 * self.copy.height() as u64
    }
}

/// Which draw a kept copy is of: the image in the state it was drawn, how,
/// and the pixels it covers. Equal keys draw equal pixels.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CopyKey {
    image: u64,
    version: u64,
    transform: [u64; 6],
    region: [i64; 4],
    quality: u8,
    alpha: u32,
}

impl CopyKey {
    fn new(
        image: &ImageData,
        version: u64,
        sampler: &ImageSampler,
        transform: Affine,
        region: Rect,
    ) -> Self {
        CopyKey {
            image: image.data.id(),
            version,
            transform: transform.as_coeffs().map(f64::to_bits),
            region: [region.x0, region.y0, region.x1, region.y1].map(|v| v as i64),
            quality: match sampler.quality {
                ImageQuality::Low => 0,
                ImageQuality::Medium => 1,
                ImageQuality::High => 2,
            },
            alpha: sampler.alpha.to_bits(),
        }
    }
}

/// The copies of image draws kept from frame to frame, each with the image
/// a texture of it stands in for.
#[derive(Default)]
pub(crate) struct Copies {
    kept: HashMap<CopyKey, ImageData>,
    used: HashSet<CopyKey>,
}

impl Copies {
    /// Forget the copies the frame did not draw, handing back their images so
    /// the renderer can drop their textures.
    pub(crate) fn end_frame(&mut self) -> Vec<ImageData> {
        let used = std::mem::take(&mut self.used);
        let mut gone = Vec::new();
        self.kept.retain(|key, image| {
            let live = used.contains(key);
            if !live {
                gone.push(image.clone());
            }
            live
        });
        gone
    }
}

/// A shape kept past the call that drew it, to open a layer again later.
pub(crate) enum OwnedShape {
    Rect(Rect),
    RoundedRect(RoundedRect),
    Path(BezPath),
}

impl OwnedShape {
    fn of(shape: &Shape<'_>) -> Self {
        match shape {
            Shape::Rect(r) => OwnedShape::Rect(*r),
            Shape::RoundedRect(r) => OwnedShape::RoundedRect(*r),
            Shape::Path(p) => OwnedShape::Path((*p).clone()),
        }
    }

    fn bounds(&self) -> Rect {
        match self {
            OwnedShape::Rect(r) => *r,
            OwnedShape::RoundedRect(r) => r.bounding_box(),
            OwnedShape::Path(p) => p.bounding_box(),
        }
    }
}

/// How a layer was opened, so it can open around its content when it closes.
pub(crate) enum Opened {
    Layer {
        clip_style: Fill,
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: OwnedShape,
    },
    LuminanceMask {
        clip_style: Fill,
        alpha: f32,
        transform: Affine,
        clip: OwnedShape,
    },
}

impl Opened {
    /// A layer clipped to `clip`, compositing at `alpha` with `blend`.
    pub(crate) fn layer(
        clip_style: Fill,
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: &Shape<'_>,
    ) -> Self {
        Opened::Layer {
            clip_style,
            blend,
            alpha,
            transform,
            clip: OwnedShape::of(clip),
        }
    }

    /// A luminance mask layer clipped to `clip`.
    pub(crate) fn luminance_mask(
        clip_style: Fill,
        alpha: f32,
        transform: Affine,
        clip: &Shape<'_>,
    ) -> Self {
        Opened::LuminanceMask {
            clip_style,
            alpha,
            transform,
            clip: OwnedShape::of(clip),
        }
    }

    /// The frame pixels the layer can reach.
    fn bounds(&self) -> Rect {
        let (transform, clip) = match self {
            Opened::Layer {
                transform, clip, ..
            }
            | Opened::LuminanceMask {
                transform, clip, ..
            } => (transform, clip),
        };
        transform.transform_rect_bbox(clip.bounds())
    }

    /// Open the layer in `scene`.
    fn open(&self, scene: &mut Scene) {
        match self {
            Opened::Layer {
                clip_style,
                blend,
                alpha,
                transform,
                clip,
            } => match clip {
                OwnedShape::Rect(r) => scene.push_layer(*clip_style, *blend, *alpha, *transform, r),
                OwnedShape::RoundedRect(r) => {
                    scene.push_layer(*clip_style, *blend, *alpha, *transform, r)
                }
                OwnedShape::Path(p) => scene.push_layer(*clip_style, *blend, *alpha, *transform, p),
            },
            Opened::LuminanceMask {
                clip_style,
                alpha,
                transform,
                clip,
            } => match clip {
                OwnedShape::Rect(r) => {
                    scene.push_luminance_mask_layer(*clip_style, *alpha, *transform, r)
                }
                OwnedShape::RoundedRect(r) => {
                    scene.push_luminance_mask_layer(*clip_style, *alpha, *transform, r)
                }
                OwnedShape::Path(p) => {
                    scene.push_luminance_mask_layer(*clip_style, *alpha, *transform, p)
                }
            },
        }
    }
}

/// The frame, or one layer open in it, with its content so far kept apart
/// from what encloses it.
struct Level {
    scene: Scene,
    /// How the layer was opened; `None` for the frame itself.
    opened: Option<Opened>,
    /// The frame pixels its content can reach, in whole pixels.
    bounds: Rect,
    /// The images its content samples, by blob id, with their sizes.
    images: HashMap<u64, u64>,
    /// The sum of `images`.
    demand: u64,
}

impl Level {
    fn new(opened: Option<Opened>, bounds: Rect) -> Self {
        Level {
            scene: Scene::new(),
            opened,
            bounds,
            images: HashMap::new(),
            demand: 0,
        }
    }

    /// The atlas pixels taking in `images` would add.
    fn added_by<'a>(&self, images: impl IntoIterator<Item = (&'a u64, &'a u64)>) -> u64 {
        images
            .into_iter()
            .filter(|(id, _)| !self.images.contains_key(id))
            .map(|(_, area)| area)
            .sum()
    }

    fn take_in(&mut self, id: u64, area: u64) {
        if self.images.insert(id, area).is_none() {
            self.demand += area;
        }
    }

    /// Render the content so far into a texture over the bounds, and carry on
    /// from a draw of that texture.
    fn flush(&mut self, jobs: &mut Vec<Job>) {
        let content = std::mem::take(&mut self.scene);
        self.images.clear();
        self.demand = 0;
        if is_empty(self.bounds) {
            // Nothing it draws can be seen.
            return;
        }
        let (width, height) = (self.bounds.width() as u32, self.bounds.height() as u32);
        if width > ATLAS_SIDE || height > ATLAS_SIDE {
            // Its copy could not be drawn back: keep the content as it was
            // and let the render hold what it can.
            self.scene = content;
            return;
        }
        let mut scene = Scene::new();
        scene.append(
            &content,
            Some(Affine::translate((-self.bounds.x0, -self.bounds.y0))),
        );
        let image = stand_in(width, height);
        self.scene.draw_image(
            &one_to_one(image.clone()),
            Affine::translate((self.bounds.x0, self.bounds.y0)),
        );
        self.take_in(image.data.id(), area(&image));
        jobs.push(Job {
            scene,
            image,
            kept: false,
        });
    }
}

/// An image drawn one texel per pixel.
fn one_to_one(image: ImageData) -> ImageBrush {
    ImageBrush {
        image,
        sampler: ImageSampler::default().with_quality(ImageQuality::Low),
    }
}

/// A frame painted into several renders. See the module docs.
pub(crate) struct SplitFrame {
    /// The frame, then each layer open in it, innermost last.
    levels: Vec<Level>,
    /// The renders to run before the frame, in order.
    jobs: Vec<Job>,
}

impl SplitFrame {
    /// A split frame over a target of `width` x `height` pixels.
    pub(crate) fn new(width: u32, height: u32) -> Self {
        SplitFrame {
            levels: vec![Level::new(
                None,
                Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            )],
            jobs: Vec::new(),
        }
    }

    fn top(&mut self) -> &mut Level {
        self.levels
            .last_mut()
            .expect("the frame level is never closed")
    }

    /// The scene the next draw lands in.
    pub(crate) fn scene(&mut self) -> &mut Scene {
        &mut self.top().scene
    }

    /// How many layers are open.
    pub(crate) fn depth(&self) -> usize {
        self.levels.len() - 1
    }

    /// Open a layer.
    pub(crate) fn open(&mut self, opened: Opened) {
        let bounds = whole_pixels(opened.bounds()).intersect(self.top().bounds);
        self.levels.push(Level::new(Some(opened), bounds));
    }

    /// Close the innermost layer: its content, inside the layer, joins the
    /// level around it. Either one renders what it holds first when the two
    /// together would not fit one render.
    pub(crate) fn close(&mut self) {
        if self.levels.len() < 2 {
            return;
        }
        let mut child = self.levels.pop().expect("checked above");
        let parent = self.top();
        let mut jobs = Vec::new();
        if parent.demand > 0 && parent.demand + parent.added_by(&child.images) > RENDER_BUDGET {
            parent.flush(&mut jobs);
        }
        if parent.demand + parent.added_by(&child.images) > RENDER_BUDGET {
            child.flush(&mut jobs);
        }
        if let Some(opened) = &child.opened {
            opened.open(&mut parent.scene);
        }
        parent.scene.append(&child.scene, None);
        parent.scene.pop_layer();
        for (id, area) in child.images {
            parent.take_in(id, area);
        }
        self.jobs.append(&mut jobs);
    }

    /// Make room for `images` in the innermost level: render what it holds
    /// first when they would not fit with it, then count them in.
    fn admit(&mut self, images: &HashMap<u64, u64>) {
        let mut jobs = Vec::new();
        let top = self.top();
        if top.demand > 0 && top.demand + top.added_by(images) > RENDER_BUDGET {
            top.flush(&mut jobs);
        }
        for (&id, &area) in images {
            top.take_in(id, area);
        }
        self.jobs.append(&mut jobs);
    }

    /// Draw an image. `version` tells the states of one image apart when its
    /// blob is kept across writes, as a pixel image's stand-in is.
    ///
    /// An image drawn at well under its own size, or too large for one render,
    /// draws through a copy the size of the pixels it covers, made by a job of
    /// its own and kept in `copies` while the same draw repeats.
    pub(crate) fn draw_image(
        &mut self,
        brush: &ImageBrush,
        transform: Affine,
        version: u64,
        copies: &mut Copies,
    ) {
        let image = &brush.image;
        let full = Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height));
        let region = whole_pixels(transform.transform_rect_bbox(full)).intersect(self.top().bounds);
        if is_empty(region) {
            // Nothing of it can be seen.
            return;
        }
        let covered = region.width() as u64 * region.height() as u64;
        let fits =
            region.width() <= f64::from(ATLAS_SIDE) && region.height() <= f64::from(ATLAS_SIDE);
        if fits && (covered * 2 <= area(image) || area(image) > RENDER_BUDGET) {
            let (copy, at, held) = match Placement::whole(image, transform) {
                Some(whole) => {
                    let key = CopyKey::new(image, version, &brush.sampler, whole.local, whole.copy);
                    let copy = self.copy(brush, key, whole.local, whole.copy, copies);
                    (copy, whole.at, whole.area())
                }
                None => {
                    let key = CopyKey::new(image, version, &brush.sampler, transform, region);
                    let copy = self.copy(brush, key, transform, region, copies);
                    (copy, region.origin(), covered)
                }
            };
            self.admit(&HashMap::from([(copy.data.id(), held)]));
            self.scene()
                .draw_image(&one_to_one(copy), Affine::translate(at.to_vec2()));
            return;
        }
        self.admit(&HashMap::from([(image.data.id(), area(image))]));
        self.scene().draw_image(brush, transform);
    }

    /// The kept copy `key` names, or a new one: `region` of the image drawn
    /// under `transform`, rendered by a job of its own.
    fn copy(
        &mut self,
        brush: &ImageBrush,
        key: CopyKey,
        transform: Affine,
        region: Rect,
        copies: &mut Copies,
    ) -> ImageData {
        copies.used.insert(key);
        if let Some(copy) = copies.kept.get(&key) {
            return copy.clone();
        }
        let copy = stand_in(region.width() as u32, region.height() as u32);
        let mut scene = Scene::new();
        scene.draw_image(
            brush,
            Affine::translate((-region.x0, -region.y0)) * transform,
        );
        self.jobs.push(Job {
            scene,
            image: copy.clone(),
            kept: true,
        });
        copies.kept.insert(key, copy.clone());
        copy
    }

    /// Append a recorded fragment, counting the images it samples.
    pub(crate) fn append(&mut self, fragment: &Scene, transform: Affine) {
        self.admit(&scene_images(fragment));
        self.scene().append(fragment, Some(transform));
    }

    /// Close every layer left open and hand back the frame's own scene and
    /// the renders to run before it, in order.
    pub(crate) fn finish(mut self) -> (Scene, Vec<Job>) {
        while self.levels.len() > 1 {
            self.close();
        }
        let root = self.levels.pop().expect("the frame level");
        (root.scene, self.jobs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(side: u32) -> ImageData {
        stand_in(side, side)
    }

    fn brush(image: &ImageData) -> ImageBrush {
        ImageBrush {
            image: image.clone(),
            sampler: ImageSampler::default(),
        }
    }

    fn square(side: f64) -> Shape<'static> {
        Shape::Rect(Rect::new(0.0, 0.0, side, side))
    }

    /// Every render a split frame makes holds at most its share of the atlas.
    fn assert_renders_fit(scene: &Scene, jobs: &[Job]) {
        for job in jobs {
            let held: u64 = scene_images(&job.scene).values().sum();
            assert!(held <= RENDER_BUDGET, "a job holds {held} atlas pixels");
        }
        let held: u64 = scene_images(scene).values().sum();
        assert!(held <= RENDER_BUDGET, "the frame holds {held} atlas pixels");
    }

    /// Large images drawn at their own size, more than one render holds,
    /// spread over several renders, each holding what fits.
    #[test]
    fn full_size_images_spread_over_renders() {
        let mut frame = SplitFrame::new(4096, 4096);
        let mut copies = Copies::default();
        for _ in 0..6 {
            frame.draw_image(&brush(&image(4096)), Affine::IDENTITY, 0, &mut copies);
        }
        let (scene, jobs) = frame.finish();
        assert!(!jobs.is_empty());
        assert!(jobs.iter().all(|job| !job.kept), "no draw was shrunk");
        assert_renders_fit(&scene, &jobs);
    }

    /// An image drawn at a fraction of its size draws through a copy the size
    /// it lands at, kept while the same draw repeats and let go when it stops.
    #[test]
    fn a_shrunk_draw_goes_through_a_kept_copy() {
        let big = image(4096);
        let shrink = Affine::translate((10.0, 10.0)) * Affine::scale(0.0625);
        let mut copies = Copies::default();

        let mut frame = SplitFrame::new(512, 512);
        frame.draw_image(&brush(&big), shrink, 1, &mut copies);
        let (scene, jobs) = frame.finish();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].kept);
        assert_eq!((jobs[0].image.width, jobs[0].image.height), (256, 256));
        let drawn = scene_images(&scene);
        assert_eq!(drawn.len(), 1);
        assert!(drawn.contains_key(&jobs[0].image.data.id()));
        assert!(copies.end_frame().is_empty());

        // The same draw again: no new job.
        let mut frame = SplitFrame::new(512, 512);
        frame.draw_image(&brush(&big), shrink, 1, &mut copies);
        assert!(frame.finish().1.is_empty());
        assert!(copies.end_frame().is_empty());

        // A new state of the image is a new copy, and the old one goes.
        let mut frame = SplitFrame::new(512, 512);
        frame.draw_image(&brush(&big), shrink, 2, &mut copies);
        assert_eq!(frame.finish().1.len(), 1);
        assert_eq!(copies.end_frame().len(), 1);
    }

    /// Moving a shrunk draw by whole pixels keeps its copy: a pan over a
    /// stack of large layers makes no new copies. A move by a fraction of a
    /// pixel samples other texels, so it makes one.
    #[test]
    fn a_shrunk_draw_keeps_its_copy_while_it_pans() {
        let big = image(4096);
        let at = |x: f64, y: f64| Affine::translate((x, y)) * Affine::scale(0.0625);
        let mut copies = Copies::default();

        let mut frame = SplitFrame::new(512, 512);
        frame.draw_image(&brush(&big), at(10.0, 10.0), 1, &mut copies);
        assert_eq!(frame.finish().1.len(), 1);
        assert!(copies.end_frame().is_empty());

        // The same draw seven pixels across and three down, half off the
        // frame's right edge: the same copy, drawn where the draw now lands.
        let mut frame = SplitFrame::new(512, 512);
        frame.draw_image(&brush(&big), at(17.0, 13.0), 1, &mut copies);
        frame.draw_image(&brush(&big), at(400.0, 13.0), 1, &mut copies);
        let (scene, jobs) = frame.finish();
        assert!(jobs.is_empty(), "a whole-pixel pan reuses the copy");
        assert!(copies.end_frame().is_empty());
        assert_eq!(scene_images(&scene).len(), 1, "both draws sample one copy");

        // A quarter of a pixel is a new copy, and the old one goes.
        let mut frame = SplitFrame::new(512, 512);
        frame.draw_image(&brush(&big), at(17.25, 13.0), 1, &mut copies);
        assert_eq!(frame.finish().1.len(), 1);
        assert_eq!(copies.end_frame().len(), 1);
    }

    /// A draw outside the frame draws nothing and makes no job.
    #[test]
    fn a_draw_outside_the_frame_is_left_out() {
        let mut frame = SplitFrame::new(64, 64);
        let mut copies = Copies::default();
        frame.draw_image(
            &brush(&image(16)),
            Affine::translate((100.0, 0.0)),
            0,
            &mut copies,
        );
        let (scene, jobs) = frame.finish();
        assert!(jobs.is_empty());
        assert!(scene.encoding().is_empty());
    }

    /// Layers keep their content apart while open, so a layer's content
    /// outgrowing a render renders inside the layer, and the layer closes
    /// around a draw of it.
    #[test]
    fn a_full_layer_renders_its_content_inside_the_layer() {
        let mut frame = SplitFrame::new(4096, 4096);
        let mut copies = Copies::default();
        frame.open(Opened::layer(
            Fill::NonZero,
            BlendMode::default(),
            0.5,
            Affine::IDENTITY,
            &square(4096.0),
        ));
        assert_eq!(frame.depth(), 1);
        for _ in 0..3 {
            frame.draw_image(&brush(&image(4096)), Affine::IDENTITY, 0, &mut copies);
        }
        frame.close();
        assert_eq!(frame.depth(), 0);
        let (scene, jobs) = frame.finish();
        assert!(!jobs.is_empty());
        assert_eq!(scene.encoding().n_open_clips, 0);
        assert_renders_fit(&scene, &jobs);
    }
}
