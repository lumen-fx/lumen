//! CPU render backend.
//!
//! Paints the frame through the walker in `lumen-paint` into a [`CpuPainter`]
//! and rasterizes it with vello_cpu. No GPU, driver, or graphics API is
//! involved, so it runs on any machine and links none of wgpu. Two entry
//! points:
//!
//! - [`CpuRenderer`], an offscreen renderer that keeps the last frame as RGBA8
//!   for headless runs, screenshots, and tests.
//! - [`CpuSurfaceRenderer`], the on-screen path. It presents through softbuffer
//!   into whatever window a window backend attaches, through
//!   [`lumen_core::traits::SurfaceRenderer`].

#![warn(missing_docs)]

pub mod capability;
pub mod sink;
pub mod surface;
pub use sink::{BACKEND_ID, CpuPainter};
pub use surface::CpuSurfaceRenderer;
/// The vello_cpu version this backend draws with. A painter that needs it
/// downcasts [`lumen_paint::Painter::native`] to [`CpuPainter`] and draws
/// through [`CpuPainter::render_context`].
pub use vello_cpu;

use bevy_ecs::prelude::*;
use bevy_ecs::system::{NonSendMut, SystemParam};
use lumen_core::components::Color as LumenColor;
use lumen_core::node_ir::{PreviousScene, RetainedScene};
use lumen_core::prelude::*;
use lumen_core::render_world::{SurfaceCapture, SurfaceFrame};
use lumen_paint::{PaintTarget, WalkContext};
use lumen_text::{ShaperService, TextShaper};
use vello_cpu::Pixmap;

/// The largest target the rasterizer takes on either axis, in pixels.
pub const MAX_DIMENSION: u32 = u16::MAX as u32;

/// Offscreen CPU renderer: a [`CpuPainter`] and the pixels of the last frame
/// it rasterized.
pub struct CpuRenderer {
    painter: PaintTarget,
    pixmap: Pixmap,
    render_count: u64,
}

impl CpuRenderer {
    /// A renderer with a `width` x `height` target. Sizes clamp to
    /// `1..=`[`MAX_DIMENSION`].
    pub fn new(width: u32, height: u32) -> Self {
        let (w, h) = clamp_size(width, height);
        Self {
            painter: Box::new(CpuPainter::new(w, h)),
            pixmap: Pixmap::new(w, h),
            render_count: 0,
        }
    }

    /// Pixel size of the target.
    pub fn size(&self) -> (u32, u32) {
        (
            u32::from(self.pixmap.width()),
            u32::from(self.pixmap.height()),
        )
    }

    /// Resize the target. The pixels are lost until the next frame.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = clamp_size(width, height);
        if (u32::from(w), u32::from(h)) != self.size() {
            self.pixmap = Pixmap::new(w, h);
        }
    }

    /// Frames rasterized since construction. Frames the damage gate skipped
    /// do not count, so a static UI leaves it flat.
    pub fn render_count(&self) -> u64 {
        self.render_count
    }

    /// The sink frames are painted into, as the walker takes it.
    pub fn painter_mut(&mut self) -> &mut PaintTarget {
        &mut self.painter
    }

    /// The sink frames are painted into, as its concrete type.
    pub fn cpu_painter(&mut self) -> &mut CpuPainter {
        cpu_painter(&mut self.painter)
    }

    /// Start a frame: forget the last one's drawing and fill the target with
    /// `clear`. Paint through [`Self::painter_mut`], then call
    /// [`Self::render_current`].
    pub fn begin_frame(&mut self, clear: LumenColor) {
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        cpu_painter(&mut self.painter).begin_frame(w, h, lumen_paint::peniko_color(clear));
    }

    /// Rasterize the painted frame into the target.
    pub fn render_current(&mut self) {
        self.render_count += 1;
        cpu_painter(&mut self.painter).render_into(&mut self.pixmap);
    }

    /// The last frame as tightly packed, straight-alpha RGBA8.
    pub fn read_rgba8(&self) -> Vec<u8> {
        straight_rgba8(&self.pixmap)
    }
}

impl lumen_core::traits::Renderer for CpuRenderer {}

/// The sink a [`PaintTarget`] built by this crate always is.
pub(crate) fn cpu_painter(painter: &mut PaintTarget) -> &mut CpuPainter {
    painter
        .native()
        .downcast_mut::<CpuPainter>()
        .expect("the CPU renderer paints through a CpuPainter")
}

/// A requested size the rasterizer can take.
pub(crate) fn clamp_size(width: u32, height: u32) -> (u16, u16) {
    let clamp = |v: u32| v.clamp(1, MAX_DIMENSION) as u16;
    (clamp(width), clamp(height))
}

/// A premultiplied pixmap as straight-alpha RGBA8, the layout every readback
/// in Lumen hands out.
pub(crate) fn straight_rgba8(pixmap: &Pixmap) -> Vec<u8> {
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

/// Plugin: installs the offscreen [`CpuRenderer`] into the render world and
/// registers the render-world system in [`RenderStage::Render`].
///
/// Text is painted through the render world's [`ShaperService`]; without one
/// text is skipped.
pub struct CpuRendererPlugin {
    renderer: CpuRenderer,
}

impl CpuRendererPlugin {
    /// A plugin rendering into a `width` x `height` target.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            renderer: CpuRenderer::new(width, height),
        }
    }
}

impl From<CpuRenderer> for CpuRendererPlugin {
    /// A plugin installing an already-built renderer.
    fn from(renderer: CpuRenderer) -> Self {
        Self { renderer }
    }
}

impl Plugin for CpuRendererPlugin {
    fn build(self, app: &mut App) {
        // The walker reads the retained tree; this is what builds it.
        lumen_core::render_world::install_extract_pipeline(app);
        app.render_world.insert_non_send(self.renderer);
        app.add_render_systems(RenderStage::Render, cpu_render_system);
    }
}

/// The scene state a frame reads and leaves behind: this tick's tree, the
/// last painted one, and the damage between them.
#[derive(SystemParam)]
struct SceneState<'w> {
    retained: Res<'w, RetainedScene>,
    previous: ResMut<'w, PreviousScene>,
    damage: ResMut<'w, FrameDamage>,
}

/// Render-world system that paints the retained scene into the offscreen
/// [`CpuRenderer`] when it changed, and answers a pending screenshot request
/// from the last frame.
fn cpu_render_system(
    mut renderer: NonSendMut<CpuRenderer>,
    shaper: Option<NonSendMut<ShaperService>>,
    viewport: Res<Viewport>,
    scene: SceneState,
    natives: Option<Res<lumen_core::native::NativePainters>>,
    capture: Option<Res<SurfaceCapture>>,
) {
    let SceneState {
        retained,
        mut previous,
        mut damage,
    } = scene;
    // The target is sized in physical pixels; the walker scales every leaf
    // from logical to physical at emit time.
    let dpr = viewport.scale_factor.max(0.01);
    let w = (viewport.size.x * dpr).max(1.0) as u32;
    let h = (viewport.size.y * dpr).max(1.0) as u32;
    let resized = (w, h) != renderer.size();
    renderer.resize(w, h);

    damage.clear();
    lumen_paint::diff_retained_scenes(
        previous.root.as_ref(),
        retained.root.as_ref(),
        lumen_core::render_world::Rect {
            origin: glam::Vec2::ZERO,
            size: viewport.size,
        },
        &mut damage,
    );

    // Partial-repaint gate: an unchanged tree keeps the last frame.
    let first_frame = previous.root.is_none();
    if first_frame || resized || !damage.is_empty() {
        renderer.begin_frame(viewport.clear);
        {
            let mut shaper_opt = shaper;
            let shaper_ref: Option<&mut dyn TextShaper> = shaper_opt
                .as_deref_mut()
                .map(|s| &mut **s as &mut dyn TextShaper);
            let mut ctx = WalkContext::new_with_dpr(renderer.painter_mut(), None, shaper_ref, dpr);
            if let Some(painters) = natives.as_deref() {
                ctx = ctx.with_native_painters(painters);
            }
            lumen_paint::walk_retained_scene(&mut ctx, &retained);
        }
        renderer.render_current();
    }
    previous.root = retained.root.clone();

    if let Some(capture) = capture
        && capture.is_requested()
    {
        let (width, height) = renderer.size();
        capture.write(SurfaceFrame {
            width,
            height,
            rgba8: renderer.read_rgba8(),
        });
        capture.clear_request();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// new size, and a screenshot request is answered from the last frame
    /// and then cleared.
    #[test]
    fn the_plugin_repaints_on_change_and_answers_a_capture() {
        let mut renderer = CpuRenderer::new(8, 8);
        assert_eq!(renderer.cpu_painter().size(), (8, 8));
        let mut app = App::new();
        app.add_plugin(CpuRendererPlugin::from(renderer));
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

    /// Resizing to the size the target already has keeps its pixels.
    #[test]
    fn resizing_to_the_same_size_keeps_the_frame() {
        let mut renderer = CpuRenderer::new(4, 4);
        renderer.begin_frame(LumenColor::rgb(1.0, 0.0, 0.0));
        renderer.render_current();
        renderer.resize(4, 4);
        assert_eq!(&renderer.read_rgba8()[..4], &[255, 0, 0, 255]);
        renderer.resize(2, 2);
        assert_eq!(renderer.size(), (2, 2));
        assert_eq!(&renderer.read_rgba8()[..4], &[0, 0, 0, 0], "a new target");
    }
}
