//! In-memory RGBA8 software renderer.
//!
//! - Render-world impl that queries [`ExtractedRect`] each frame and software-rasterises into an RGBA8 framebuffer.
//! - Deterministic; no GPU or display dependencies.
//! - Supports filled rounded rectangles. [`ExtractedText`] is ignored here.
//! - Implements [`SurfaceRenderer`], so a window backend can drive it in place of a GPU renderer. There are no pixels to put on screen, but frames are still rasterised and screenshot requests are answered, which is what makes a windowed run reproducible in a test.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bevy_ecs::prelude::*;
use bevy_ecs::system::NonSendMut;
use glam::Vec2;
use lumen_core::prelude::*;
use lumen_core::traits::{FrameRequest, RenderTarget, SurfaceError, SurfaceRenderer};
use std::sync::Arc;

/// Headless software renderer producing an RGBA8 framebuffer.
pub struct HeadlessRenderer {
    width: u32,
    height: u32,
    buffer: Vec<u8>, // RGBA8, row-major, top-down.
}

impl HeadlessRenderer {
    /// Allocate a framebuffer of the given pixel dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let buffer = vec![0u8; (width as usize) * (height as usize) * 4];
        Self {
            width,
            height,
            buffer,
        }
    }

    /// Pixel dimensions.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Borrow the current RGBA8 framebuffer.
    pub fn framebuffer(&self) -> &[u8] {
        &self.buffer
    }

    fn resize_to(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.buffer = vec![0u8; (width as usize) * (height as usize) * 4];
    }

    fn clear(&mut self, color: Color) {
        let [r, g, b, a] = color.to_rgba8();
        for px in self.buffer.chunks_exact_mut(4) {
            px[0] = r;
            px[1] = g;
            px[2] = b;
            px[3] = a;
        }
    }

    fn put_px(&mut self, x: i32, y: i32, src: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let idx = ((y as usize) * self.width as usize + x as usize) * 4;
        let sa = src[3] as u32;
        if sa == 255 {
            self.buffer[idx..idx + 4].copy_from_slice(&src);
            return;
        }
        if sa == 0 {
            return;
        }
        let inv = 255 - sa;
        let blend = |s: u8, d: u8| -> u8 { ((s as u32 * sa + d as u32 * inv + 127) / 255) as u8 };
        let dst = &mut self.buffer[idx..idx + 4];
        dst[0] = blend(src[0], dst[0]);
        dst[1] = blend(src[1], dst[1]);
        dst[2] = blend(src[2], dst[2]);
        let da = dst[3] as u32;
        dst[3] = (sa + (da * inv + 127) / 255) as u8;
    }

    /// Rasterise one frame from the render world: resize to the viewport,
    /// clear, then paint every extracted rect in the order extraction
    /// spawned them, which is the painter order the render stage uses.
    ///
    /// Shared by the render-stage system and the [`SurfaceRenderer`]
    /// present path so both produce the same framebuffer.
    fn render_from_world(&mut self, world: &mut World) {
        let viewport = {
            let vp = world.resource::<Viewport>();
            (vp.size, vp.clear)
        };
        let (size, clear) = viewport;
        self.resize_to(size.x.max(1.0) as u32, size.y.max(1.0) as u32);
        self.clear(clear);
        let mut rects = world.query::<&ExtractedRect>();
        let painted: Vec<(Vec2, Vec2, Color, f32)> = rects
            .iter(world)
            .map(|rect| (rect.origin, rect.size, representative_color(rect), rect.radius))
            .collect();
        for (origin, size, color, radius) in painted {
            self.fill_rect(origin, size, color, radius);
        }
        // Text intentionally skipped - see crate-level docs.
    }

    fn fill_rect(&mut self, origin: Vec2, size: Vec2, fill: Color, radius: f32) {
        let x0 = origin.x.floor() as i32;
        let y0 = origin.y.floor() as i32;
        let x1 = (origin.x + size.x).ceil() as i32;
        let y1 = (origin.y + size.y).ceil() as i32;
        let src = fill.to_rgba8();
        let r = radius.max(0.0).min(size.x.min(size.y) / 2.0);

        for y in y0..y1 {
            for x in x0..x1 {
                if r > 0.0 && !inside_rounded_rect(x as f32, y as f32, origin, size, r) {
                    continue;
                }
                self.put_px(x, y, src);
            }
        }
    }
}

impl lumen_core::traits::Renderer for HeadlessRenderer {}

impl SurfaceRenderer for HeadlessRenderer {
    /// Adopt the window's size. There is no swap chain to build, so this
    /// cannot fail.
    fn attach(&mut self, target: Arc<dyn RenderTarget>) -> Result<(), SurfaceError> {
        let (width, height) = target.physical_size();
        self.resize_to(width.max(1), height.max(1));
        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) -> bool {
        if self.width == width && self.height == height {
            return false;
        }
        self.resize_to(width.max(1), height.max(1));
        true
    }

    /// Paint whenever the tick reported a change, plus whenever a
    /// screenshot is waiting. There is no retained-scene diff here: the
    /// software rasteriser has no encoding to reuse, so the finer gate
    /// would only cost a tree walk.
    fn wants_present(&mut self, render_world: &mut World, request: FrameRequest) -> bool {
        request.dirty
            || request.force_full
            || render_world
                .get_resource::<SurfaceCapture>()
                .is_some_and(|c| c.is_requested())
    }

    /// Rasterise the frame and answer any pending screenshot request from
    /// the framebuffer.
    fn present(&mut self, render_world: &mut World) -> Result<(), SurfaceError> {
        self.render_from_world(render_world);
        if let Some(capture) = render_world.get_resource::<SurfaceCapture>().cloned()
            && capture.is_requested()
        {
            capture.write(SurfaceFrame {
                width: self.width,
                height: self.height,
                rgba8: self.buffer.clone(),
            });
            capture.clear_request();
        }
        Ok(())
    }

    /// Nothing is bound to the window, so there is nothing to release.
    fn detach(&mut self) {}
}

/// The single color this renderer paints a rect in. Gradients sample their
/// first stop, which keeps golden images deterministic; apps that need the
/// full gradient appearance use the WGPU renderer.
fn representative_color(rect: &ExtractedRect) -> Color {
    match &rect.brush {
        Brush::Solid(c) => *c,
        Brush::Linear { stops, .. } | Brush::Radial { stops, .. } | Brush::Conic { stops, .. } => {
            stops.first().map(|(_, c)| *c).unwrap_or_default()
        }
    }
}

fn inside_rounded_rect(x: f32, y: f32, origin: Vec2, size: Vec2, r: f32) -> bool {
    let max = origin + size;
    if x < origin.x || y < origin.y || x >= max.x || y >= max.y {
        return false;
    }
    let cx = if x < origin.x + r {
        origin.x + r
    } else if x > max.x - r - 1.0 {
        max.x - r - 1.0
    } else {
        return true;
    };
    let cy = if y < origin.y + r {
        origin.y + r
    } else if y > max.y - r - 1.0 {
        max.y - r - 1.0
    } else {
        return true;
    };
    let dx = x - cx;
    let dy = y - cy;
    dx * dx + dy * dy <= r * r
}

/// Plugin: installs a [`HeadlessRenderer`] in the render world and registers
/// the rasterizer system in [`RenderStage::Render`].
pub struct HeadlessRendererPlugin {
    /// Initial framebuffer width.
    pub width: u32,
    /// Initial framebuffer height.
    pub height: u32,
}

impl Default for HeadlessRendererPlugin {
    fn default() -> Self {
        Self {
            width: 800,
            height: 600,
        }
    }
}

impl Plugin for HeadlessRendererPlugin {
    fn build(self, app: &mut App) {
        app.render_world
            .insert_non_send(HeadlessRenderer::new(self.width, self.height));
        app.add_render_systems(RenderStage::Render, headless_render_system);
    }
}

fn headless_render_system(
    mut renderer: NonSendMut<HeadlessRenderer>,
    viewport: Res<Viewport>,
    rects: Query<&ExtractedRect>,
) {
    let w = viewport.size.x.max(1.0) as u32;
    let h = viewport.size.y.max(1.0) as u32;
    renderer.resize_to(w, h);
    renderer.clear(viewport.clear);
    for rect in &rects {
        renderer.fill_rect(
            rect.origin,
            rect.size,
            representative_color(rect),
            rect.radius,
        );
    }
    // Text intentionally skipped - see crate-level docs.
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::traits::RenderTarget;
    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
    };

    /// A window that reports a size and refuses to hand out real handles.
    /// The software renderer never dereferences them, which is exactly the
    /// point: a window backend can drive it with no display attached.
    struct FakeWindow {
        size: (u32, u32),
    }

    impl HasWindowHandle for FakeWindow {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            Err(HandleError::Unavailable)
        }
    }

    impl HasDisplayHandle for FakeWindow {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            Err(HandleError::Unavailable)
        }
    }

    impl RenderTarget for FakeWindow {
        fn physical_size(&self) -> (u32, u32) {
            self.size
        }
    }

    fn render_world() -> World {
        let mut world = World::new();
        world.insert_resource(Viewport {
            size: Vec2::new(8.0, 4.0),
            clear: Color::rgba(0.0, 0.0, 0.0, 1.0),
            ..Viewport::default()
        });
        world
    }

    /// Attaching adopts the window's physical size, and a later resize is
    /// reported only when the size really changed - the same coalescing
    /// contract the GPU path has, so a window backend gets one repaint per
    /// distinct size from either renderer.
    #[test]
    fn attach_adopts_size_and_resize_coalesces() {
        let mut renderer = HeadlessRenderer::new(1, 1);
        renderer
            .attach(Arc::new(FakeWindow { size: (64, 32) }))
            .expect("software renderer attaches without a display");
        assert_eq!(renderer.size(), (64, 32));
        assert!(renderer.resize(80, 40));
        assert_eq!(renderer.size(), (80, 40));
        assert!(!renderer.resize(80, 40));
        renderer.detach();
    }

    /// The present gate: a clean tick paints nothing; a dirty tick, a
    /// recreated surface, or a waiting screenshot each ask for a frame.
    #[test]
    fn present_gate_answers_dirty_force_and_capture() {
        let mut renderer = HeadlessRenderer::new(8, 4);
        let mut world = render_world();
        assert!(!renderer.wants_present(&mut world, FrameRequest::default()));
        assert!(renderer.wants_present(
            &mut world,
            FrameRequest {
                dirty: true,
                force_full: false,
            }
        ));
        assert!(renderer.wants_present(
            &mut world,
            FrameRequest {
                dirty: false,
                force_full: true,
            }
        ));

        let capture = SurfaceCapture::default();
        capture.request();
        world.insert_resource(capture);
        assert!(renderer.wants_present(&mut world, FrameRequest::default()));
    }

    /// Presenting rasterises the extracted scene and hands the pixels to a
    /// waiting screenshot request, clearing it.
    #[test]
    fn present_rasterises_and_fulfils_capture() {
        let mut renderer = HeadlessRenderer::new(1, 1);
        let mut world = render_world();
        world.spawn(ExtractedRect {
            origin: Vec2::ZERO,
            size: Vec2::new(8.0, 4.0),
            brush: Brush::Solid(Color::rgba(1.0, 0.0, 0.0, 1.0)),
            radius: 0.0,
            corner_radii: None,
            order: 0,
        });
        let capture = SurfaceCapture::default();
        capture.request();
        world.insert_resource(capture.clone());

        renderer
            .present(&mut world)
            .expect("software present never fails");

        assert_eq!(renderer.size(), (8, 4));
        assert_eq!(&renderer.framebuffer()[0..4], &[255, 0, 0, 255]);
        assert!(!capture.is_requested());
        let frame = capture.read().expect("capture holds the presented frame");
        assert_eq!((frame.width, frame.height), (8, 4));
        assert_eq!(&frame.rgba8[0..4], &[255, 0, 0, 255]);
    }
}
