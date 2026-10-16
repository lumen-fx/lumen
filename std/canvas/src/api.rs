//! The canvas from Rust: what the `canvas::` script functions are written on.
//!
//! [`Context2d`] draws on one `<canvas>` element, named by its `id`, the way
//! the HTML canvas's `CanvasRenderingContext2D` does. [`PixelBuffer`] is a
//! CPU pixel buffer the app writes, reads, and draws onto a canvas. Both are
//! the same store and the same buffers the script functions use, so a Rust
//! app and a script can draw on one canvas and share one buffer.
//!
//! Every call is safe from anywhere: a system, an event handler, another
//! thread. A drawing call is recorded and replayed into the canvas once per
//! tick, by the module's encode system ([`CanvasSet::Encode`]); a system that
//! draws runs before it to land in the same frame:
//!
//! ```
//! # use bevy_ecs::prelude::*;
//! # use lumen_module::lumen_core::app::App;
//! # use lumen_module::lumen_core::prelude::TickStage;
//! use lumen_canvas::{CanvasSet, Context2d, PixelBuffer};
//!
//! #[derive(Resource)]
//! struct Layer(PixelBuffer);
//!
//! fn paint(layer: Res<Layer>) {
//!     let ctx = Context2d::new("art");
//!     ctx.clear();
//!     ctx.set_image_smoothing(false);
//!     ctx.draw_buffer_scaled(&layer.0, 0.0, 0.0, 512.0, 512.0);
//! }
//!
//! # let mut app = App::new();
//! app.add_systems(TickStage::Systems, paint.before(CanvasSet::Encode));
//! ```
//!
//! Reading a canvas back takes the app's text shaper and the renderer, so it
//! is a system parameter, [`CanvasReader`], rather than a context call; it
//! answers on the spot with everything drawn so far. A script, which has
//! neither, reads through [`Context2d::get_image_data`], which fills a buffer
//! when the canvas next encodes.

use std::fmt;
use std::path::Path;

use bevy_ecs::schedule::SystemSet;
use bevy_ecs::system::{NonSendMut, SystemParam};
use lumen_module::lumen_paint::PixelImage;
use lumen_module::lumen_text::{ShaperService, TextShaper};

use crate::buffer::PixBuf;
use crate::color::{self, Rgba};
use crate::encode;
use crate::ops::{CompositeOperation, FillRule, FontSpec, LineCap, LineJoin, Op};
use crate::plugin::Canvas;
use crate::store;

/// The module's systems, for ordering an app's own against them.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CanvasSet {
    /// Adopts the `<canvas>` elements that appeared this tick. Runs before
    /// any script handler.
    Adopt,
    /// Replays what was drawn since the last tick into each canvas. A system
    /// that draws before it lands in this frame; one after it, in the next.
    Encode,
}

/// Why a canvas call did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CanvasError {
    /// The buffer handle names no buffer: it was freed, or never issued.
    UnknownBuffer(BufferId),
    /// The request is over a cap the app set (or the default one): a buffer
    /// too large, too many buffers, a region too large.
    OverCap(String),
    /// A color string this module does not understand.
    NotAColor(String),
    /// A line cap other than butt, round, or square.
    NotALineCap(String),
    /// A line join other than miter, round, or bevel.
    NotALineJoin(String),
    /// A font string with no size in it.
    NotAFont(String),
    /// A name that is not one of the HTML canvas's composite operations.
    NotACompositeOperation(String),
    /// An image file that could not be read, decoded, or written.
    Image(String),
    /// A read of a canvas before the renderer has drawn a frame, or in a run
    /// with no renderer at all: there is nothing to rasterize with yet.
    NoRenderer,
    /// The renderer could not rasterize the canvas.
    Read(String),
}

impl fmt::Display for CanvasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CanvasError::UnknownBuffer(id) => write!(f, "no buffer {}", id.0),
            CanvasError::OverCap(message)
            | CanvasError::Image(message)
            | CanvasError::Read(message) => f.write_str(message),
            CanvasError::NotACompositeOperation(text) => write!(
                f,
                "'{text}' is not a composite operation; use one of the HTML canvas's, \
                 such as source-over, destination-out, or multiply"
            ),
            CanvasError::NoRenderer => {
                f.write_str("the canvas cannot be read before the renderer has drawn a frame")
            }
            CanvasError::NotAColor(text) => write!(
                f,
                "'{text}' is not a color this module understands; use a hex, rgb(), or rgba() value"
            ),
            CanvasError::NotALineCap(text) => {
                write!(f, "'{text}' is not a line cap; use butt, round, or square")
            }
            CanvasError::NotALineJoin(text) => {
                write!(f, "'{text}' is not a line join; use miter, round, or bevel")
            }
            CanvasError::NotAFont(text) => {
                write!(f, "'{text}' is not a font; it needs a size, as in '16px'")
            }
        }
    }
}

impl std::error::Error for CanvasError {}

/// The 2D drawing context of one `<canvas>` element, named by its `id`.
///
/// Cheap to make and to clone: it is the id and nothing else, so there is
/// nothing to keep alive and nothing to invalidate when the tree is rebuilt.
/// A canvas is created the first time anything names it, so drawing before
/// the element exists is kept and shown once it is mounted.
///
/// The state calls (`set_*`, `save`, `restore`, the transform) persist on the
/// canvas across ticks, as on the web, until a `clear` or a `resize` resets
/// them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Context2d {
    id: String,
}

impl Context2d {
    /// The context of the canvas whose element has this `id`.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Context2d {
        Context2d { id: id.into() }
    }

    /// The context of an adopted canvas element.
    #[must_use]
    pub fn of(canvas: &Canvas) -> Context2d {
        Context2d::new(canvas.id.clone())
    }

    /// The element id this context draws on.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    fn record(&self, op: Op) {
        store::store().record(&self.id, op);
    }

    // -- the canvas ---------------------------------------------------------

    /// The drawing space, in canvas units: the last size a pending
    /// [`Self::resize`] asked for, or the one the canvas has.
    ///
    /// A resize is recorded like every other call, so it has not been applied
    /// when the next line asks how big the canvas is; answering with the old
    /// size there would make `resize` followed by a loop over the size draw
    /// the wrong picture.
    #[must_use]
    pub fn size(&self) -> (f32, f32) {
        let mut store = store::store();
        let surface = store.surface(&self.id);
        surface
            .pending
            .iter()
            .rev()
            .find_map(|op| match op {
                Op::Resize(w, h) => Some((*w, *h)),
                _ => None,
            })
            .unwrap_or(surface.logical)
    }

    /// Set the drawing space, in canvas units. This empties the canvas and
    /// resets its state, as writing `width` on an HTML canvas does.
    pub fn resize(&self, width: f32, height: f32) {
        self.record(Op::Resize(width.max(0.0), height.max(0.0)));
    }

    /// Erase everything the canvas holds and reset its state.
    pub fn clear(&self) {
        self.record(Op::Clear);
    }

    // -- paths --------------------------------------------------------------

    /// Start a new path.
    pub fn begin_path(&self) {
        self.record(Op::BeginPath);
    }

    /// Start a subpath at a point.
    pub fn move_to(&self, x: f64, y: f64) {
        self.record(Op::MoveTo(x, y));
    }

    /// Straight segment to a point.
    pub fn line_to(&self, x: f64, y: f64) {
        self.record(Op::LineTo(x, y));
    }

    /// Quadratic segment through a control point.
    pub fn quad_to(&self, cx: f64, cy: f64, x: f64, y: f64) {
        self.record(Op::QuadTo(cx, cy, x, y));
    }

    /// Cubic segment through two control points.
    pub fn bezier_to(&self, c1x: f64, c1y: f64, c2x: f64, c2y: f64, x: f64, y: f64) {
        self.record(Op::BezierTo(c1x, c1y, c2x, c2y, x, y));
    }

    /// Circular arc around `(x, y)`, angles in radians.
    pub fn arc(&self, x: f64, y: f64, radius: f64, start: f64, end: f64) {
        self.record(Op::Arc {
            x,
            y,
            radius,
            start,
            end,
        });
    }

    /// Add a closed rectangle to the path.
    pub fn rect(&self, x: f64, y: f64, width: f64, height: f64) {
        self.record(Op::Rect(x, y, width, height));
    }

    /// Close the current subpath back to its start.
    pub fn close_path(&self) {
        self.record(Op::ClosePath);
    }

    /// Fill the current path, under the nonzero rule.
    pub fn fill(&self) {
        self.record(Op::Fill(FillRule::NonZero));
    }

    /// Fill the current path under the even-odd rule: a region inside it an
    /// even number of times is a hole. The HTML canvas's `fill("evenodd")`.
    pub fn fill_evenodd(&self) {
        self.record(Op::Fill(FillRule::EvenOdd));
    }

    /// Intersect the clip with the current path, under the nonzero rule.
    /// Every later draw shows only inside it; `save` / `restore` carry it,
    /// and only a `restore` (or a `clear` or `resize`) widens it again.
    pub fn clip(&self) {
        self.record(Op::Clip(FillRule::NonZero));
    }

    /// Intersect the clip with the current path, under the even-odd rule.
    /// The HTML canvas's `clip("evenodd")`.
    pub fn clip_evenodd(&self) {
        self.record(Op::Clip(FillRule::EvenOdd));
    }

    /// Stroke the current path.
    pub fn stroke(&self) {
        self.record(Op::Stroke);
    }

    /// Fill one rectangle, leaving the path alone.
    pub fn fill_rect(&self, x: f64, y: f64, width: f64, height: f64) {
        self.record(Op::FillRect(x, y, width, height));
    }

    /// Stroke one rectangle, leaving the path alone.
    pub fn stroke_rect(&self, x: f64, y: f64, width: f64, height: f64) {
        self.record(Op::StrokeRect(x, y, width, height));
    }

    /// Make a rectangle transparent, through the transform and the clip.
    /// The global alpha and the composite operation do not apply, and the
    /// path and the state are left alone, as on the web.
    pub fn clear_rect(&self, x: f64, y: f64, width: f64, height: f64) {
        self.record(Op::ClearRect(x, y, width, height));
    }

    // -- state --------------------------------------------------------------

    /// Set the fill color.
    pub fn set_fill_color(&self, color: Rgba) {
        self.record(Op::SetFill(color));
    }

    /// Set the fill color from CSS text: a hex, `rgb()`, or `rgba()` value.
    pub fn set_fill_style(&self, css: &str) -> Result<(), CanvasError> {
        let color = color::parse_css(css).ok_or_else(|| CanvasError::NotAColor(css.to_string()))?;
        self.set_fill_color(color);
        Ok(())
    }

    /// Set the stroke color.
    pub fn set_stroke_color(&self, color: Rgba) {
        self.record(Op::SetStroke(color));
    }

    /// Set the stroke color from CSS text: a hex, `rgb()`, or `rgba()` value.
    pub fn set_stroke_style(&self, css: &str) -> Result<(), CanvasError> {
        let color = color::parse_css(css).ok_or_else(|| CanvasError::NotAColor(css.to_string()))?;
        self.set_stroke_color(color);
        Ok(())
    }

    /// Set the stroke width, in canvas units.
    pub fn set_line_width(&self, width: f64) {
        self.record(Op::SetLineWidth(width));
    }

    /// Set the stroke's ends.
    pub fn set_line_cap(&self, cap: LineCap) {
        self.record(Op::SetLineCap(cap));
    }

    /// Set the stroke's corners.
    pub fn set_line_join(&self, join: LineJoin) {
        self.record(Op::SetLineJoin(join));
    }

    /// Multiply every later draw by this alpha, 0..1.
    pub fn set_global_alpha(&self, alpha: f64) {
        self.record(Op::SetGlobalAlpha(alpha));
    }

    /// Whether a buffer drawn at another size is smoothed: on (the default,
    /// as on the web) samples it bilinearly; off samples the nearest pixel,
    /// which keeps zoomed pixel art sharp. The HTML canvas's
    /// `imageSmoothingEnabled`.
    pub fn set_image_smoothing(&self, enabled: bool) {
        self.record(Op::SetImageSmoothing(enabled));
    }

    /// How later draws combine with what the canvas holds. The HTML canvas's
    /// `globalCompositeOperation`; `source-over` by default.
    pub fn set_global_composite_operation(&self, operation: CompositeOperation) {
        self.record(Op::SetComposite(operation));
    }

    /// Push the drawing state.
    pub fn save(&self) {
        self.record(Op::Save);
    }

    /// Pop the drawing state; an unmatched pop leaves the state alone.
    pub fn restore(&self) {
        self.record(Op::Restore);
    }

    // -- the transform ------------------------------------------------------

    /// Move the origin.
    pub fn translate(&self, x: f64, y: f64) {
        self.record(Op::Translate(x, y));
    }

    /// Rotate the transform, in radians.
    pub fn rotate(&self, radians: f64) {
        self.record(Op::Rotate(radians));
    }

    /// Scale the transform.
    pub fn scale(&self, x: f64, y: f64) {
        self.record(Op::Scale(x, y));
    }

    /// Drop back to the identity transform.
    pub fn reset_transform(&self) {
        self.record(Op::ResetTransform);
    }

    /// Replace the transform with `[a, b, c, d, e, f]`, the HTML canvas's
    /// `setTransform` order.
    pub fn set_transform(&self, matrix: [f64; 6]) {
        self.record(Op::SetTransform(matrix));
    }

    // -- text ---------------------------------------------------------------

    /// Set the font as `"[weight] <size>px [family]"`.
    pub fn set_font(&self, font: &str) -> Result<(), CanvasError> {
        let spec = FontSpec::parse(font).ok_or_else(|| CanvasError::NotAFont(font.to_string()))?;
        self.record(Op::SetFont(spec));
        Ok(())
    }

    /// Draw text in the fill color, with `(x, y)` on the baseline. Shaped with
    /// the app's own fonts.
    pub fn fill_text(&self, text: &str, x: f64, y: f64) {
        self.record(Op::FillText {
            text: text.to_string(),
            x,
            y,
        });
    }

    // -- buffers ------------------------------------------------------------

    /// Draw a buffer at its own size, its top-left corner at `(x, y)`.
    ///
    /// The canvas shows the buffer as it is at this call, as an HTML canvas's
    /// `drawImage` does: a write after it, even in the same tick, shows once
    /// the buffer is drawn again. A handle naming no buffer draws nothing.
    pub fn draw_buffer(&self, buffer: &BufferId, x: f64, y: f64) {
        self.record_buffer(*buffer, |image| Op::DrawBuffer { image, x, y });
    }

    /// Draw a buffer stretched into a box, sampled as
    /// [`Self::set_image_smoothing`] says. Like [`Self::draw_buffer`], it
    /// draws the pixels the buffer holds at this call.
    pub fn draw_buffer_scaled(&self, buffer: &BufferId, x: f64, y: f64, width: f64, height: f64) {
        self.record_buffer(*buffer, |image| Op::DrawBufferScaled {
            image,
            x,
            y,
            width,
            height,
        });
    }

    /// Read a `width` x `height` region of the canvas, its top-left at
    /// `(x, y)` in canvas units, into a new buffer: the HTML canvas's
    /// `getImageData`. One canvas unit is one pixel, whatever size the
    /// element is shown at.
    ///
    /// The read sees every call made before it and none after, but it is
    /// answered when the canvas next encodes, after the handler that asked
    /// returns; until then the buffer is transparent. A read made before the
    /// renderer's first frame waits for it. [`CanvasReader`] answers on the
    /// spot, from a system.
    pub fn get_image_data(
        &self,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
    ) -> Result<PixelBuffer, CanvasError> {
        let buffer = PixelBuffer::new(width, height)?;
        self.record(Op::GetImageData {
            x,
            y,
            buffer: buffer.id().raw(),
        });
        Ok(buffer)
    }

    /// Record a draw of `buffer`'s pixels as they are now. The snapshot is
    /// taken under the same lock the call is recorded under, so no write can
    /// land between the two. Taking it costs a reference count per tile: the
    /// pixels are shared until the buffer is next written.
    fn record_buffer(&self, buffer: BufferId, op: impl FnOnce(PixelImage) -> Op) {
        let mut store = store::store();
        let Some(image) = store.buffers.get(&buffer.0).map(|b| b.image().clone()) else {
            return;
        };
        store.record(&self.id, op(image));
    }
}

/// A handle to a pixel buffer: the number a script holds.
///
/// Copyable and shareable with scripts ([`Self::raw`], [`Self::from_raw`]).
/// It does not keep the buffer alive; a [`PixelBuffer`] does, and frees it
/// when dropped. Every call on a handle whose buffer is gone does nothing and
/// says so ([`CanvasError::UnknownBuffer`]), or reads as empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(u32);

impl BufferId {
    /// The handle a script holds for this buffer.
    #[must_use]
    pub fn raw(self) -> u32 {
        self.0
    }

    /// The buffer a script's handle names. Handle 0 is never issued.
    #[must_use]
    pub fn from_raw(raw: u32) -> BufferId {
        BufferId(raw)
    }

    /// Create a transparent buffer, within the app's caps.
    pub fn create(width: u32, height: u32) -> Result<BufferId, CanvasError> {
        store::store()
            .new_buffer(width, height)
            .map(BufferId)
            .map_err(CanvasError::OverCap)
    }

    /// Read a PNG into a fresh buffer, within the app's caps. A relative
    /// path is resolved against the app, as every path an app names is.
    pub fn load_png(path: impl AsRef<Path>) -> Result<BufferId, CanvasError> {
        let path = lumen_module::lumen_core::app_paths::resolve(path.as_ref());
        // The cap is read under the lock and the decode happens without it:
        // reading a file is slow, and every other canvas call in the process
        // would queue behind it.
        let cap = store::store().caps.buffer_pixels;
        let loaded = PixBuf::load_png(&path, cap).map_err(CanvasError::Image)?;
        store::store()
            .adopt_buffer(loaded)
            .map(BufferId)
            .map_err(CanvasError::OverCap)
    }

    fn read<T>(self, read: impl FnOnce(&PixBuf) -> T) -> Result<T, CanvasError> {
        let store = store::store();
        store
            .buffers
            .get(&self.0)
            .map(read)
            .ok_or(CanvasError::UnknownBuffer(self))
    }

    fn write<T>(self, write: impl FnOnce(&mut PixBuf) -> T) -> Result<T, CanvasError> {
        let mut store = store::store();
        let out = store
            .buffers
            .get_mut(&self.0)
            .map(write)
            .ok_or(CanvasError::UnknownBuffer(self))?;
        // A written buffer is only on screen once a canvas draws it again,
        // and that draw wakes the loop; nothing to wake for here.
        Ok(out)
    }

    /// Release the buffer. Returns whether there was one.
    pub fn free(self) -> bool {
        store::store().buffers.remove(&self.0).is_some()
    }

    /// Whether the buffer still exists.
    #[must_use]
    pub fn exists(self) -> bool {
        self.read(|_| ()).is_ok()
    }

    /// `(width, height)` in pixels, or `None` when the buffer is gone.
    #[must_use]
    pub fn size(self) -> Option<(u32, u32)> {
        self.read(|b| (b.width(), b.height())).ok()
    }

    /// Width in pixels; 0 when the buffer is gone.
    #[must_use]
    pub fn width(self) -> u32 {
        self.size().map_or(0, |s| s.0)
    }

    /// Height in pixels; 0 when the buffer is gone.
    #[must_use]
    pub fn height(self) -> u32 {
        self.size().map_or(0, |s| s.1)
    }

    /// Change the size, keeping the pixels both sizes share at the top left;
    /// new area is transparent. The new size answers to the pixel cap.
    pub fn resize(self, width: u32, height: u32) -> Result<(), CanvasError> {
        let mut store = store::store();
        store
            .check_size(width, height)
            .map_err(CanvasError::OverCap)?;
        store
            .buffers
            .get_mut(&self.0)
            .map(|b| b.resize(width, height))
            .ok_or(CanvasError::UnknownBuffer(self))
    }

    /// One pixel as `0xRRGGBBAA`; 0 outside the buffer or when it is gone.
    #[must_use]
    pub fn pixel(self, x: i64, y: i64) -> u32 {
        self.read(|b| b.get_pixel(x, y)).unwrap_or(0)
    }

    /// Write one pixel as `0xRRGGBBAA`; outside the buffer it is dropped.
    pub fn set_pixel(self, x: i64, y: i64, rgba: u32) -> Result<(), CanvasError> {
        self.write(|b| b.set_pixel(x, y, rgba))
    }

    /// Write a `width` x `height` rectangle at `(x, y)` from straight RGBA8
    /// rows, in one call. What falls outside the buffer is dropped; a short
    /// slice writes what it has.
    pub fn write_rgba8(
        self,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<(), CanvasError> {
        self.write(|b| b.write_rgba8(x, y, width, height, pixels))
    }

    /// Write a rectangle from `0xRRGGBBAA` values, row-major, in one call.
    pub fn write_packed(
        self,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
        pixels: &[u32],
    ) -> Result<(), CanvasError> {
        self.write(|b| b.put_region(x, y, width, height, pixels))
    }

    /// Read a rectangle into `out` as straight RGBA8 rows. Pixels outside
    /// the buffer read transparent; a short `out` takes what fits.
    pub fn read_rgba8(
        self,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
        out: &mut [u8],
    ) -> Result<(), CanvasError> {
        self.read(|b| b.read_rgba8(x, y, width, height, out))
    }

    /// Read a rectangle into `out` as `0xRRGGBBAA` values, row-major.
    pub fn read_packed(
        self,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
        out: &mut [u32],
    ) -> Result<(), CanvasError> {
        self.read(|b| b.read_packed(x, y, width, height, out))
    }

    /// Fill a rectangle with one `0xRRGGBBAA` color, clipped to the buffer.
    pub fn fill_rect(
        self,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
        rgba: u32,
    ) -> Result<(), CanvasError> {
        self.write(|b| b.fill_rect(x, y, width, height, rgba))
    }

    /// Write the buffer out as a PNG. A relative path is resolved against the
    /// app.
    pub fn save_png(self, path: impl AsRef<Path>) -> Result<(), CanvasError> {
        let path = lumen_module::lumen_core::app_paths::resolve(path.as_ref());
        self.read(|b| b.save_png(&path))?
            .map_err(CanvasError::Image)
    }
}

/// A pixel buffer the app owns: freed when dropped.
///
/// Derefs to its [`BufferId`], which is where every pixel call lives, and
/// which is what to hand a script that should draw it too.
#[derive(Debug)]
pub struct PixelBuffer {
    id: BufferId,
}

impl PixelBuffer {
    /// A transparent buffer, within the app's caps.
    pub fn new(width: u32, height: u32) -> Result<PixelBuffer, CanvasError> {
        BufferId::create(width, height).map(|id| PixelBuffer { id })
    }

    /// A buffer holding a PNG's pixels, within the app's caps.
    pub fn load_png(path: impl AsRef<Path>) -> Result<PixelBuffer, CanvasError> {
        BufferId::load_png(path).map(|id| PixelBuffer { id })
    }

    /// The handle, for a script or a [`Context2d`] draw.
    #[must_use]
    pub fn id(&self) -> BufferId {
        self.id
    }

    /// Give up ownership: the buffer outlives this value and is freed only
    /// by [`BufferId::free`] (or a script's `buffer_free`).
    #[must_use]
    pub fn into_id(self) -> BufferId {
        let id = self.id;
        std::mem::forget(self);
        id
    }
}

impl std::ops::Deref for PixelBuffer {
    type Target = BufferId;

    fn deref(&self) -> &BufferId {
        &self.id
    }
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        self.id.free();
    }
}

/// Reads canvases back as pixels, on the spot.
///
/// A system parameter, because a read rasterizes everything drawn so far,
/// and drawing text takes the app's shaper. Calls recorded since the canvas
/// last encoded are encoded first, exactly as the next tick would have, so
/// the read sees every call made before it.
///
/// The pixels are the renderer's: the CPU renderer rasterizes the canvas with
/// vello_cpu, and the GPU renderer renders it on the device and copies it
/// back, which waits for the GPU. Either way a read costs one rasterize of
/// the region, so a read every frame is a second render every frame.
///
/// ```
/// # use bevy_ecs::prelude::*;
/// use lumen_canvas::{CanvasReader, Context2d};
///
/// fn pick_color(mut reader: CanvasReader) {
///     let mut pixel = [0u8; 4];
///     if reader.read_rgba8(&Context2d::new("art"), 10, 10, 1, 1, &mut pixel).is_ok() {
///         // `pixel` is the straight RGBA under canvas unit (10, 10).
///     }
/// }
/// # let _ = IntoSystem::into_system(pick_color);
/// ```
#[derive(SystemParam)]
pub struct CanvasReader<'w> {
    shaper: Option<NonSendMut<'w, ShaperService>>,
}

impl CanvasReader<'_> {
    /// Read a `width` x `height` region, its top-left at `(x, y)` in canvas
    /// units, into `out` as straight RGBA8 rows. What lies outside the
    /// canvas reads transparent; a short `out` takes what fits.
    pub fn read_rgba8(
        &mut self,
        canvas: &Context2d,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
        out: &mut [u8],
    ) -> Result<(), CanvasError> {
        let pixels = self.read(canvas, x, y, width, height)?;
        let n = out.len().min(pixels.len());
        out[..n].copy_from_slice(&pixels[..n]);
        Ok(())
    }

    /// Read a region into `out` as `0xRRGGBBAA` values, row-major.
    pub fn read_packed(
        &mut self,
        canvas: &Context2d,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
        out: &mut [u32],
    ) -> Result<(), CanvasError> {
        let pixels = self.read(canvas, x, y, width, height)?;
        for (slot, px) in out.iter_mut().zip(pixels.chunks_exact(4)) {
            *slot = u32::from_be_bytes([px[0], px[1], px[2], px[3]]);
        }
        Ok(())
    }

    fn read(
        &mut self,
        canvas: &Context2d,
        x: i64,
        y: i64,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, CanvasError> {
        let mut store = store::store();
        let store = &mut *store;
        let rasterizer = store.rasterizer.clone().ok_or(CanvasError::NoRenderer)?;
        let surface = store.surfaces.entry(canvas.id.clone()).or_default();
        let ops = std::mem::take(&mut surface.pending);
        let drew = encode::encode(
            surface,
            ops,
            &mut store.blobs,
            self.shaper
                .as_deref_mut()
                .map(|s| &mut **s as &mut dyn TextShaper),
            &mut store.reads,
        );
        if drew {
            surface.revision += 1;
        }
        let drawing = surface.drawing.clone();
        // What was flushed reaches the screen through the next encode, which
        // sees the drawing moved; wake the loop so there is one.
        store.wake();
        store.serve_reads();
        store::read_pixels(&rasterizer, &drawing, x, y, width, height).map_err(CanvasError::Read)
    }
}

impl std::str::FromStr for CompositeOperation {
    type Err = CanvasError;

    /// The HTML canvas spelling, as in `destination-out`.
    fn from_str(name: &str) -> Result<CompositeOperation, CanvasError> {
        CompositeOperation::parse(name)
            .ok_or_else(|| CanvasError::NotACompositeOperation(name.to_string()))
    }
}

impl std::str::FromStr for LineCap {
    type Err = CanvasError;

    /// The HTML canvas spelling: `butt`, `round`, or `square`.
    fn from_str(name: &str) -> Result<LineCap, CanvasError> {
        LineCap::parse(name).ok_or_else(|| CanvasError::NotALineCap(name.to_string()))
    }
}

impl std::str::FromStr for LineJoin {
    type Err = CanvasError;

    /// The HTML canvas spelling: `miter`, `round`, or `bevel`.
    fn from_str(name: &str) -> Result<LineJoin, CanvasError> {
        LineJoin::parse(name).ok_or_else(|| CanvasError::NotALineJoin(name.to_string()))
    }
}
