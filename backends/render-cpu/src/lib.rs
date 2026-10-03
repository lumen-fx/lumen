//! CPU render backend.
//!
//! [`CpuRenderer`] paints the frame through the walker in `lumen-paint` into
//! a [`CpuPainter`] and rasterizes it with vello_cpu. No GPU, driver, or
//! graphics API is involved, so it runs on any machine and links none of
//! wgpu. The rasterized frame goes to whichever target the renderer
//! attaches to through [`lumen_core::traits::Renderer`]: an offscreen image
//! kept as RGBA8 for headless runs, screenshots, and tests, or a window,
//! through softbuffer.

#![warn(missing_docs)]

pub mod capability;
pub mod sink;
mod surface;
pub use sink::{BACKEND_ID, CpuPainter};
/// The vello_cpu version this backend draws with. A painter that needs it
/// downcasts [`lumen_paint::Painter::native`] to [`CpuPainter`] and draws
/// through [`CpuPainter::render_context`].
pub use vello_cpu;

use bevy_ecs::world::World;
use lumen_core::prelude::*;
use lumen_core::render_backend::install_offscreen;
use lumen_core::traits::{FrameRequest, FrameTarget, RenderError};
use lumen_paint::PaintTarget;
use surface::Presenter;
use vello_cpu::Pixmap;

/// The largest target the rasterizer takes on either axis, in pixels.
pub const MAX_DIMENSION: u32 = u16::MAX as u32;

/// CPU renderer: a [`CpuPainter`], the pixels of the last frame it
/// rasterized, and the window they are shown in, if any.
///
/// Construction is free: a window backend builds one before the window
/// exists and attaches the window once it does, and a headless launch
/// attaches an offscreen image (see [`Self::new_offscreen`]).
pub struct CpuRenderer {
    painter: PaintTarget,
    pixmap: Pixmap,
    /// `None` while detached.
    target: Option<Target>,
    /// Whether the pixmap holds a rasterized frame at its current size.
    holds_frame: bool,
    render_count: u64,
}

/// What a [`CpuRenderer`] shows its frames on.
enum Target {
    /// Nowhere: the pixmap is the result, read back on request.
    Offscreen,
    /// A window, through softbuffer.
    Window(Presenter),
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self {
            painter: Box::new(CpuPainter::new(1, 1)),
            pixmap: Pixmap::new(1, 1),
            target: None,
            holds_frame: false,
            render_count: 0,
        }
    }
}

impl CpuRenderer {
    /// A renderer bound to nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A renderer attached to a `width` x `height` offscreen image. Sizes
    /// clamp to `1..=`[`MAX_DIMENSION`]. The CPU always starts, so this
    /// cannot fail.
    pub fn new_offscreen(width: u32, height: u32) -> Self {
        let mut renderer = Self::new();
        renderer.bind_offscreen(width, height);
        renderer
    }

    /// Whether a target is currently bound.
    pub fn is_attached(&self) -> bool {
        self.target.is_some()
    }

    /// Pixel size of the target.
    pub fn size(&self) -> (u32, u32) {
        (
            u32::from(self.pixmap.width()),
            u32::from(self.pixmap.height()),
        )
    }

    /// Frames rasterized since construction. Frames the present gate
    /// skipped do not count, so a static UI leaves it flat.
    pub fn render_count(&self) -> u64 {
        self.render_count
    }

    /// The last frame as tightly packed, straight-alpha RGBA8.
    pub fn read_rgba8(&self) -> Vec<u8> {
        straight_rgba8(&self.pixmap)
    }

    fn bind_offscreen(&mut self, width: u32, height: u32) {
        self.target = Some(Target::Offscreen);
        self.set_size(width, height);
    }

    /// Size the pixmap to `width` x `height`, clamped. Returns `true` when
    /// the size changed; the new pixmap holds no frame.
    fn set_size(&mut self, width: u32, height: u32) -> bool {
        let (w, h) = clamp_size(width, height);
        if (u32::from(w), u32::from(h)) == self.size() {
            return false;
        }
        self.pixmap = Pixmap::new(w, h);
        self.holds_frame = false;
        true
    }
}

impl Renderer for CpuRenderer {
    fn attach(&mut self, target: FrameTarget) -> Result<(), RenderError> {
        self.target = None;
        self.holds_frame = false;
        match target {
            FrameTarget::Offscreen { width, height } => self.bind_offscreen(width, height),
            FrameTarget::Window(window) => {
                let (width, height) = window.physical_size();
                let mut presenter = Presenter::new(window)?;
                self.set_size(width, height);
                let (w, h) = self.size();
                presenter.resize(w, h)?;
                self.target = Some(Target::Window(presenter));
            }
        }
        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) -> bool {
        if self.target.is_none() || !self.set_size(width, height) {
            return false;
        }
        let (w, h) = self.size();
        if let Some(Target::Window(presenter)) = self.target.as_mut()
            && let Err(e) = presenter.resize(w, h)
        {
            tracing::warn!(target: "lumen::render", "softbuffer resize failed: {e}");
        }
        true
    }

    fn wants_present(&mut self, render_world: &mut World, request: FrameRequest) -> bool {
        self.target.is_some() && lumen_paint::wants_frame(render_world, request, self.holds_frame)
    }

    fn present(&mut self, render_world: &mut World) -> Result<(), RenderError> {
        let Some(target) = self.target.as_mut() else {
            return Err(RenderError::Detached);
        };
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        cpu_painter(&mut self.painter).begin_frame(w, h, lumen_paint::clear_color(render_world));
        lumen_paint::paint_frame(render_world, &mut self.painter, None);
        cpu_painter(&mut self.painter).render_into(&mut self.pixmap);
        self.holds_frame = true;
        self.render_count += 1;
        let pixmap = &self.pixmap;
        lumen_paint::answer_capture(render_world, (u32::from(w), u32::from(h)), || {
            Ok(straight_rgba8(pixmap))
        });
        match target {
            Target::Window(presenter) => presenter.present(pixmap),
            Target::Offscreen => Ok(()),
        }
    }

    fn detach(&mut self) {
        self.target = None;
    }
}

/// The sink a [`PaintTarget`] built by this crate always is.
fn cpu_painter(painter: &mut PaintTarget) -> &mut CpuPainter {
    painter
        .native()
        .downcast_mut::<CpuPainter>()
        .expect("the CPU renderer paints through a CpuPainter")
}

/// A requested size the rasterizer can take.
fn clamp_size(width: u32, height: u32) -> (u16, u16) {
    let clamp = |v: u32| v.clamp(1, MAX_DIMENSION) as u16;
    (clamp(width), clamp(height))
}

/// A premultiplied pixmap as straight-alpha RGBA8, the layout every readback
/// in Lumen hands out.
fn straight_rgba8(pixmap: &Pixmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixmap.data().len() * 4);
    for p in pixmap.data() {
        let unmultiply = |c: u8| -> u8 {
            match p.a {
                0 => 0,
                255 => c,
                a => ((u16::from(c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8,
            }
        };
        out.extend_from_slice(&[unmultiply(p.r), unmultiply(p.g), unmultiply(p.b), p.a]);
    }
    out
}

/// Plugin: installs an offscreen [`CpuRenderer`] into the render world,
/// driven each frame by [`install_offscreen`]'s render-world system.
///
/// Text is painted through the render world's [`lumen_text::ShaperService`];
/// without one text is skipped.
pub struct CpuRendererPlugin {
    renderer: CpuRenderer,
}

impl CpuRendererPlugin {
    /// A plugin rendering into a `width` x `height` image.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            renderer: CpuRenderer::new_offscreen(width, height),
        }
    }
}

impl From<CpuRenderer> for CpuRendererPlugin {
    /// A plugin installing an already-attached renderer.
    fn from(renderer: CpuRenderer) -> Self {
        Self { renderer }
    }
}

impl Plugin for CpuRendererPlugin {
    fn build(self, app: &mut App) {
        install_offscreen(app, self.renderer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::components::Color as LumenColor;
    use lumen_core::render_world::SurfaceCapture;

    /// A premultiplied pixel reads back straight: opaque pixels unchanged,
    /// transparent ones zero, and partial ones divided back out.
    #[test]
    fn readback_is_straight_alpha() {
        use vello_cpu::color::PremulRgba8;
        let mut pixmap = Pixmap::new(3, 1);
        pixmap.data_mut()[0] = PremulRgba8 {
            r: 10,
            g: 20,
            b: 30,
            a: 255,
        };
        pixmap.data_mut()[1] = PremulRgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        pixmap.data_mut()[2] = PremulRgba8 {
            r: 64,
            g: 0,
            b: 0,
            a: 128,
        };
        let rgba = straight_rgba8(&pixmap);
        assert_eq!(&rgba[0..4], &[10, 20, 30, 255]);
        assert_eq!(&rgba[4..8], &[0, 0, 0, 0]);
        assert_eq!(&rgba[8..12], &[128, 0, 0, 128]);
    }

    /// Sizes the rasterizer cannot take are clamped rather than wrapping.
    #[test]
    fn sizes_clamp_into_range() {
        assert_eq!(clamp_size(0, 10), (1, 10));
        assert_eq!(clamp_size(100_000, 1), (u16::MAX, 1));
    }

    /// The offscreen path end to end: one styled box on a clear colour,
    /// through extract, the walker, and the rasterizer, read back.
    #[test]
    fn the_plugin_renders_the_retained_scene() {
        let mut app = App::new();
        app.add_plugin(CpuRendererPlugin::new(32, 32));
        for world in [&mut app.world, &mut app.render_world] {
            let mut vp = world.resource_mut::<Viewport>();
            vp.size = glam::Vec2::new(32.0, 32.0);
            vp.clear = LumenColor::rgb(0.0, 0.0, 0.0);
        }
        app.world.spawn((
            Transform {
                absolute: glam::Vec2::new(8.0, 8.0),
                size: glam::Vec2::new(16.0, 16.0),
                baseline_y: None,
            },
            Visuals {
                fill: Some(Fill::Solid(LumenColor::rgb(1.0, 0.0, 0.0))),
                ..Default::default()
            },
        ));
        app.tick();
        let renderer = app.render_world.non_send::<CpuRenderer>();
        let rgba = renderer.read_rgba8();
        let at = |x: usize, y: usize| rgba[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4].to_vec();
        assert_eq!(at(16, 16), [255, 0, 0, 255]);
        assert_eq!(at(2, 2), [0, 0, 0, 255]);
        assert_eq!(renderer.render_count(), 1);
    }

    /// An unchanged scene keeps the last frame, a resize repaints at the
    /// new size, and a screenshot request is answered and then cleared.
    #[test]
    fn the_plugin_repaints_on_change_and_answers_a_capture() {
        let mut app = App::new();
        app.add_plugin(CpuRendererPlugin::new(8, 8));
        let capture = SurfaceCapture::default();
        app.render_world.insert_resource(capture.clone());
        let set_size = |app: &mut App, size: glam::Vec2| {
            for world in [&mut app.world, &mut app.render_world] {
                world.resource_mut::<Viewport>().size = size;
            }
        };
        set_size(&mut app, glam::Vec2::new(8.0, 8.0));

        app.tick();
        app.tick();
        let count = |app: &App| app.render_world.non_send::<CpuRenderer>().render_count();
        assert_eq!(count(&app), 1, "a static scene paints once");
        assert!(capture.read().is_none());

        set_size(&mut app, glam::Vec2::new(12.0, 6.0));
        capture.request();
        app.tick();
        assert_eq!(count(&app), 2, "a resize repaints");
        assert!(!capture.is_requested());
        let shot = capture.read().expect("the request was answered");
        assert_eq!((shot.width, shot.height), (12, 6));
        assert_eq!(shot.rgba8.len(), 12 * 6 * 4);
    }

    /// Resizing to the size the target already has keeps its frame; a new
    /// size is a new, empty image.
    #[test]
    fn resizing_to_the_same_size_keeps_the_frame() {
        let mut renderer = CpuRenderer::new_offscreen(4, 4);
        let mut world = World::new();
        world.insert_resource(Viewport {
            clear: LumenColor::rgb(1.0, 0.0, 0.0),
            ..Default::default()
        });
        renderer.present(&mut world).expect("offscreen present");
        assert!(!renderer.resize(4, 4));
        assert_eq!(&renderer.read_rgba8()[..4], &[255, 0, 0, 255]);
        assert!(renderer.resize(2, 2));
        assert_eq!(renderer.size(), (2, 2));
        assert_eq!(&renderer.read_rgba8()[..4], &[0, 0, 0, 0], "a new target");
    }

    /// With no target bound the renderer answers every call without
    /// touching a display.
    #[test]
    fn a_detached_renderer_has_nothing_to_present() {
        let mut renderer = CpuRenderer::new();
        assert!(!renderer.is_attached());
        assert!(!renderer.resize(800, 600));
        let mut world = World::new();
        world.insert_resource(Viewport::default());
        let request = FrameRequest {
            dirty: true,
            force_full: true,
        };
        assert!(!renderer.wants_present(&mut world, request));
        assert!(matches!(
            renderer.present(&mut world),
            Err(RenderError::Detached)
        ));
        renderer.detach();
    }
}
