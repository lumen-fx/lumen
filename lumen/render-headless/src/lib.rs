//! In-memory RGBA8 software renderer.
//!
//! - Render-world impl that queries [`ExtractedRect`] each frame and software-rasterises into an RGBA8 framebuffer.
//! - Deterministic; no GPU or display dependencies.
//! - Supports filled rounded rectangles. [`ExtractedText`] is ignored here.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bevy_ecs::prelude::*;
use bevy_ecs::system::NonSendMut;
use glam::Vec2;
use lumen_core::prelude::*;

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
            .insert_non_send_resource(HeadlessRenderer::new(self.width, self.height));
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
        // Headless renderer paints a single representative color per
        // rect; for gradients it samples the first stop (deterministic
        // baseline for golden-image tests). Apps that need the full
        // gradient appearance use the WGPU renderer.
        let color = match &rect.brush {
            lumen_core::render_world::Brush::Solid(c) => *c,
            lumen_core::render_world::Brush::Linear { stops, .. }
            | lumen_core::render_world::Brush::Radial { stops, .. }
            | lumen_core::render_world::Brush::Conic { stops, .. } => {
                stops.first().map(|(_, c)| *c).unwrap_or_default()
            }
        };
        renderer.fill_rect(rect.origin, rect.size, color, rect.radius);
    }
    // Text intentionally skipped - see crate-level docs.
}
