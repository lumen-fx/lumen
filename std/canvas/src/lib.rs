//! Immediate-mode drawing for Lumen apps, as a self-contained module.
//!
//! The engine has no drawing surface; this crate is the whole capability.
//! Install [`CanvasPlugin`] and an app gains the `<canvas>` element and the
//! `canvas` script namespace that draws on it: paths, rectangles, arcs,
//! strokes and fills, transforms, text, and CPU pixel buffers. Rhai and
//! candela spell a call `canvas::fill_rect(..)`; Lua spells it
//! `canvas.fill_rect(..)`.
//!
//! Without the module there is no `<canvas>` tag and a script calling
//! `canvas::fill_rect` gets its host's ordinary unknown-function error.
//!
//! One implementation, two link shapes:
//!
//! - **Runtime module.** The `cdylib` target is the bundled `lumen-canvas`
//!   module; an app opts in from `lumen.toml`:
//!
//!   ```toml
//!   [dependencies]
//!   lumen-canvas = { bundled = true, tags = ["canvas"] }
//!   ```
//!
//!   The `tags` key is what lets `lumenc build` parse `<canvas>` markup: a
//!   compile loads no module, so the declaration is the app's claim that the
//!   element exists.
//!
//! - **Compiled in.** A statically linked app (or a test) adds this crate as
//!   an ordinary dependency and installs [`CanvasPlugin`] itself.
//!
//! # Drawing
//!
//! An element's `id` is the canvas's name, and every call takes it first:
//!
//! ```lmn
//! <canvas id="chart" width="300" height="150" />
//! ```
//!
//! ```rhai
//! canvas::set_fill_style("chart", "#3b82f6");
//! canvas::fill_rect("chart", 10.0, 10.0, 80.0, 40.0);
//! ```
//!
//! `width` and `height` are the drawing space, in canvas units. They are also
//! the element's default size; when layout gives it a different box, the
//! drawing is scaled onto that box, so one script draws the same picture at
//! any size. An axis with no declaration gets 300 across or 150 down, which
//! is what the HTML canvas has always defaulted to.
//!
//! A `width` in CSS is the same declaration as a `width` in markup by the
//! time an element exists, so CSS changes the drawing space rather than
//! scaling it. That is the one place this parts company with the HTML
//! canvas.
//!
//! A canvas is retained: what a script draws stays until `canvas::clear` (or
//! a `canvas::resize`, which empties it the same way). Drawing nothing on a
//! tick costs nothing.
//!
//! # Pixels
//!
//! The canvas itself is write-only, because what it holds is a list of
//! drawing calls bound for the GPU rather than an image. Reading and writing
//! pixels is what the buffer functions are for: `buffer_new`,
//! `buffer_set_pixel`, `buffer_get_region`, `buffer_load_png`, and
//! `draw_buffer` to put one on a canvas. Pixels are packed `0xRRGGBBAA`
//! integers with straight (not premultiplied) alpha, so what a script writes
//! is what it reads back.
//!
//! Three settings bound what one call can ask for:
//!
//! ```toml
//! [dependencies]
//! lumen-canvas = { bundled = true, tags = ["canvas"], config = {
//!     region_cap = 1048576,
//!     buffer_pixel_cap = 16777216,
//!     buffer_count_cap = 256,
//! } }
//! ```
//!
//! # Not here yet
//!
//! `clear_rect`, clipping, blend and composite modes, gradients, patterns,
//! shadows, `measure_text`, `stroke_text`, text alignment and baselines,
//! `ellipse` and `arc_to`, dashed strokes, image smoothing, drawing an image
//! file or another canvas directly, reading a composed canvas back, and a
//! per-canvas device pixel ratio. Canvas does not exist on the web target,
//! which refuses `[dependencies]` outright.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod buffer;
pub mod color;
pub mod ops;
pub mod store;

mod encode;
mod paint;
mod plugin;

/// The geometry types this module's public state is spelled in: the
/// transform on a [`ops::GfxState`] and the path on a [`ops::Gfx`] are
/// kurbo's, taken through the engine's own vello so there is one of each.
pub use lumen_module::lumen_render_wgpu::vello::peniko::kurbo;
pub use paint::{CanvasLeaf, CanvasPainter, EXTENSION_ID, extract_canvases};
pub use plugin::{Canvas, CanvasPlugin, TAG};
pub use store::{
    Caps, DEFAULT_BUFFER_COUNT_CAP, DEFAULT_BUFFER_PIXEL_CAP, DEFAULT_REGION_CAP,
    MAX_BUFFER_COUNT_CAP, MAX_BUFFER_PIXEL_CAP, MAX_REGION_CAP, MIN_BUFFER_COUNT_CAP,
    MIN_BUFFER_PIXEL_CAP, MIN_REGION_CAP, UA_SIZE,
};

// The module entry: the loader constructs the shipping plugin from the app's
// `config` table, whether it opened this crate's library or found it linked
// in.
lumen_module::lumen_module!("lumen-canvas", |config: lumen_module::ModuleConfig| {
    CanvasPlugin::new(config)
});
