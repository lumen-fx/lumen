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
use lumen_core::render_backend::{DeviceKind, RendererInfo, install_offscreen};
use lumen_core::traits::{FrameRequest, FrameTarget, RenderError};
use lumen_paint::kurbo::Affine;
use lumen_paint::{LayerCache, LayerStats, PaintTarget, Rasterize, Rasterizer, Recording};
use std::collections::HashMap;
use surface::Presenter;
use vello_cpu::Pixmap;

/// The largest target the rasterizer takes on either axis, in pixels.
pub const MAX_DIMENSION: u32 = u16::MAX as u32;

/// CPU renderer: one [`CpuPainter`] shared by every window, and per window
/// the pixels of the last frame it rasterized and where they are shown.
///
/// Construction is free: a window backend builds one before the windows
/// exist and attaches each one once it does, and a headless launch attaches
/// an offscreen image per window (see [`Self::new_offscreen`]).
pub struct CpuRenderer {
    painter: PaintTarget,
    targets: HashMap<Entity, Target>,
    render_count: u64,
}

/// One window's share of the renderer.
struct Target {
    pixmap: Pixmap,
    /// Where the pixmap is shown: nowhere for an offscreen image, the window
    /// through softbuffer otherwise.
    presenter: Option<Presenter>,
    /// Whether the pixmap holds a rasterized frame at its current size.
    holds_frame: bool,
    /// Faded groups' layers, kept from frame to frame.
    layers: LayerCache,
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self {
            painter: Box::new(CpuPainter::new(1, 1)),
            targets: HashMap::new(),
            render_count: 0,
        }
    }
}

impl CpuRenderer {
    /// A renderer bound to nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A renderer with `target` attached to a `width` x `height` offscreen
    /// image. Sizes clamp to `1..=`[`MAX_DIMENSION`]. The CPU always starts,
    /// so this cannot fail.
    pub fn new_offscreen(target: Entity, width: u32, height: u32) -> Self {
        let mut renderer = Self::new();
        renderer.bind_offscreen(target, width, height);
        renderer
    }

    /// Whether any target is bound.
    pub fn is_attached(&self) -> bool {
        !self.targets.is_empty()
    }

    /// Pixel size of `target`'s image, or `None` when it is not attached.
    pub fn size(&self, target: Entity) -> Option<(u32, u32)> {
        self.targets.get(&target).map(|t| pixmap_size(&t.pixmap))
    }

    /// Frames rasterized since construction, across every target. Frames
    /// the present gate skipped do not count, so a static UI leaves it flat.
    pub fn render_count(&self) -> u64 {
        self.render_count
    }

    /// What `target`'s kept layers of faded groups hold and how often they
    /// were reused.
    pub fn layer_stats(&self, target: Entity) -> Option<LayerStats> {
        self.targets.get(&target).map(|t| t.layers.stats())
    }

    /// `target`'s last frame as tightly packed, straight-alpha RGBA8, empty
    /// when it is not attached.
    pub fn read_rgba8(&self, target: Entity) -> Vec<u8> {
        self.targets
            .get(&target)
            .map(|t| straight_rgba8(&t.pixmap))
            .unwrap_or_default()
    }

    fn bind_offscreen(&mut self, target: Entity, width: u32, height: u32) {
        let (w, h) = clamp_size(width, height);
        self.targets.insert(
            target,
            Target {
                pixmap: Pixmap::new(w, h),
                presenter: None,
                holds_frame: false,
                layers: LayerCache::default(),
            },
        );
    }
}

/// A pixmap's size as `u32`s.
fn pixmap_size(pixmap: &Pixmap) -> (u32, u32) {
    (u32::from(pixmap.width()), u32::from(pixmap.height()))
}

impl Renderer for CpuRenderer {
    fn info(&mut self) -> RendererInfo {
        RendererInfo {
            backend: capability::NAME,
            device: format!(
                "vello_cpu, {} worker threads",
                cpu_painter(&mut self.painter).worker_threads()
            ),
            device_kind: DeviceKind::Cpu,
            max_texture_side: MAX_DIMENSION,
            ..RendererInfo::default()
        }
    }

    fn attach(&mut self, target: Entity, frame: FrameTarget) -> Result<(), RenderError> {
        self.targets.remove(&target);
        match frame {
            FrameTarget::Offscreen { width, height } => self.bind_offscreen(target, width, height),
            FrameTarget::Window(window) => {
                let (width, height) = window.physical_size();
                let mut presenter = Presenter::new(window)?;
                let (w, h) = clamp_size(width, height);
                presenter.resize(u32::from(w), u32::from(h))?;
                self.targets.insert(
                    target,
                    Target {
                        pixmap: Pixmap::new(w, h),
                        presenter: Some(presenter),
                        holds_frame: false,
                        layers: LayerCache::default(),
                    },
                );
            }
        }
        Ok(())
    }

    fn resize(&mut self, target: Entity, width: u32, height: u32) -> bool {
        let Some(t) = self.targets.get_mut(&target) else {
            return false;
        };
        let (w, h) = clamp_size(width, height);
        if (u32::from(w), u32::from(h)) == pixmap_size(&t.pixmap) {
            return false;
        }
        t.pixmap = Pixmap::new(w, h);
        t.holds_frame = false;
        if let Some(presenter) = t.presenter.as_mut()
            && let Err(e) = presenter.resize(u32::from(w), u32::from(h))
        {
            tracing::warn!(target: "lumen::render", "softbuffer resize failed: {e}");
        }
        true
    }

    fn wants_present(
        &mut self,
        target: Entity,
        render_world: &mut World,
        request: FrameRequest,
    ) -> bool {
        self.targets
            .get(&target)
            .is_some_and(|t| lumen_paint::wants_frame(render_world, target, request, t.holds_frame))
    }

    fn present(&mut self, target: Entity, render_world: &mut World) -> Result<(), RenderError> {
        let Some(t) = self.targets.get_mut(&target) else {
            return Err(RenderError::Detached);
        };
        // What a drawing read back as pixels is rasterized with: this
        // renderer's own rasterizer, published once it draws.
        if !render_world.contains_resource::<Rasterizer>() {
            render_world.insert_resource(Rasterizer::new(CpuRasterizer));
        }
        let (w, h) = (t.pixmap.width(), t.pixmap.height());
        cpu_painter(&mut self.painter).begin_frame(
            w,
            h,
            lumen_paint::clear_color(render_world, target),
        );
        lumen_paint::paint_frame(
            render_world,
            target,
            &mut self.painter,
            None,
            Some(&mut t.layers),
        );
        stamp(render_world, RenderTimestamps::mark_encoded);
        cpu_painter(&mut self.painter).render_into(&mut t.pixmap);
        stamp(render_world, RenderTimestamps::mark_submitted);
        t.holds_frame = true;
        self.render_count += 1;
        let pixmap = &t.pixmap;
        lumen_paint::answer_capture(render_world, target, (u32::from(w), u32::from(h)), || {
            Ok(straight_rgba8(pixmap))
        });
        if let Some(presenter) = t.presenter.as_mut() {
            presenter.present(&t.pixmap)?;
            stamp(render_world, RenderTimestamps::mark_presented);
        }
        Ok(())
    }

    fn targets(&self) -> Vec<Entity> {
        let mut targets: Vec<Entity> = self.targets.keys().copied().collect();
        targets.sort();
        targets
    }

    fn detach(&mut self, target: Entity) {
        self.targets.remove(&target);
    }

    fn detach_all(&mut self) {
        self.targets.clear();
    }
}

/// Rasterizes a drawing on demand the way a frame is: a [`CpuPainter`] the
/// size of the request, replayed into, and rasterized with vello_cpu. What
/// [`CpuRenderer`] publishes as the render world's [`Rasterizer`].
///
/// Each call is a fresh painter and a fresh pixmap, so it shares nothing
/// with the frame and costs one rasterize of the drawing at that size.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuRasterizer;

impl Rasterize for CpuRasterizer {
    fn rasterize(
        &self,
        drawing: &Recording,
        transform: Affine,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        if width == 0 || height == 0 {
            return Ok(Vec::new());
        }
        if width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(format!(
                "a {width}x{height} image is larger than the {MAX_DIMENSION} pixels a side \
                 the CPU rasterizer takes"
            ));
        }
        let (w, h) = (width as u16, height as u16);
        let mut painter = CpuPainter::new(w, h);
        painter.begin_frame(w, h, lumen_paint::peniko::Color::TRANSPARENT);
        drawing.replay(&mut painter, transform);
        let mut pixmap = Pixmap::new(w, h);
        painter.render_into(&mut pixmap);
        Ok(straight_rgba8(&pixmap))
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
/// driven each frame by [`install_offscreen`]'s render-world system, which
/// gives every window an image of its own.
///
/// Text is painted through the render world's [`lumen_text::ShaperService`];
/// without one text is skipped.
pub struct CpuRendererPlugin {
    renderer: Option<CpuRenderer>,
    size: (u32, u32),
}

impl CpuRendererPlugin {
    /// A plugin rendering the primary window into a `width` x `height`
    /// image to begin with.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            renderer: None,
            size: (width, height),
        }
    }
}

impl From<CpuRenderer> for CpuRendererPlugin {
    /// A plugin installing an already-attached renderer.
    fn from(renderer: CpuRenderer) -> Self {
        Self {
            renderer: Some(renderer),
            size: (1, 1),
        }
    }
}

impl Plugin for CpuRendererPlugin {
    fn build(self, app: &mut App) {
        let renderer = self.renderer.unwrap_or_else(|| {
            let primary =
                lumen_core::window::primary_window(&mut app.world).expect("a primary window");
            CpuRenderer::new_offscreen(primary, self.size.0, self.size.1)
        });
        install_offscreen(app, renderer);
    }
}

/// Stamp one of the frame's [`RenderTimestamps`], when the render world keeps
/// them.
fn stamp(render_world: &mut World, mark: fn(&mut RenderTimestamps)) {
    if let Some(mut stamps) = render_world.get_resource_mut::<RenderTimestamps>() {
        mark(&mut stamps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::components::Color as LumenColor;
    use lumen_core::render_world::SurfaceCapture;

    fn primary_window(app: &mut App) -> Entity {
        lumen_core::window::primary_window(&mut app.world).expect("a primary window")
    }

    /// #528: after a frame the app can read that the CPU renderer drew it,
    /// and when it finished rasterizing.
    #[test]
    fn the_app_reads_that_the_cpu_drew_it() {
        let mut app = App::new();
        app.add_plugin(CpuRendererPlugin::new(16, 16));
        app.tick();
        let info = app.world.resource::<RendererInfo>();
        assert_eq!(info.backend, "cpu");
        assert_eq!(info.device_kind, DeviceKind::Cpu);
        assert_eq!(info.memory, None, "nothing on a device to report");
        let timing = app.world.resource::<FrameTiming>();
        assert!(timing.encoded.is_some() && timing.submitted.is_some());
    }

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
        let primary = primary_window(&mut app);
        {
            let mut vp = primary_viewport_mut(&mut app.world);
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
        let rgba = renderer.read_rgba8(primary);
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
            primary_viewport_mut(&mut app.world).size = size;
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
        let target = Entity::PLACEHOLDER;
        let mut renderer = CpuRenderer::new_offscreen(target, 4, 4);
        let mut world = World::new();
        lumen_core::render_world::spawn_render_window(
            &mut world,
            target,
            WindowViewport {
                clear: LumenColor::rgb(1.0, 0.0, 0.0),
                ..Default::default()
            },
            true,
        );
        renderer
            .present(target, &mut world)
            .expect("offscreen present");
        assert!(!renderer.resize(target, 4, 4));
        assert_eq!(&renderer.read_rgba8(target)[..4], &[255, 0, 0, 255]);
        assert!(renderer.resize(target, 2, 2));
        assert_eq!(renderer.size(target), Some((2, 2)));
        assert_eq!(
            &renderer.read_rgba8(target)[..4],
            &[0, 0, 0, 0],
            "a new target"
        );
        assert!(renderer.layer_stats(target).is_some());
        assert_eq!(renderer.targets(), vec![target]);
        renderer.detach(target);
        assert!(renderer.read_rgba8(target).is_empty());
    }

    /// With no target bound the renderer answers every call without
    /// touching a display.
    #[test]
    fn a_detached_renderer_has_nothing_to_present() {
        let mut renderer = CpuRenderer::new();
        let target = Entity::PLACEHOLDER;
        assert!(!renderer.is_attached());
        assert!(!renderer.resize(target, 800, 600));
        assert_eq!(renderer.size(target), None);
        let mut world = World::new();
        let request = FrameRequest {
            dirty: true,
            force_full: true,
        };
        assert!(!renderer.wants_present(target, &mut world, request));
        assert!(matches!(
            renderer.present(target, &mut world),
            Err(RenderError::Detached)
        ));
        renderer.detach(target);
        renderer.detach_all();
    }
}
