//! A painted frame, as [`App::screenshot`](crate::App::screenshot) returns it.

use std::path::Path;

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::lumen_core;

use crate::error::{Error, Result};
use image::ImageEncoder;
use image::codecs::png::PngEncoder;
use lumen_core::render_world::SurfaceFrame;

/// One frame in RGBA8: four bytes per pixel, rows top to bottom, colours
/// sRGB-encoded with straight (not premultiplied) alpha. The size is in
/// physical pixels, the logical size times the run's dpr.
///
/// Two frames are equal when their sizes and every pixel are, which is the
/// comparison a golden test makes:
///
/// ```no_run
/// # extern crate lumenui_engine as lumenui;
/// # use lumenui::prelude::*;
/// # fn demo(frame: lumenui::Frame) -> lumenui::Result<()> {
/// let golden = lumenui::Frame::open_png("tests/golden/main.png")?;
/// assert!(frame == golden, "the main view drew differently");
/// # Ok(()) }
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Frame {
    /// A frame of `width` x `height` from its RGBA8 pixels, or `None` when
    /// there are not exactly four bytes for each of them.
    pub fn from_rgba8(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        (pixels.len() as u64 == u64::from(width) * u64::from(height) * 4).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Every pixel, four bytes each.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The pixel at `(x, y)` as `[r, g, b, a]`, or `None` outside the frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = ((y as usize) * (self.width as usize) + x as usize) * 4;
        self.pixels[at..at + 4].try_into().ok()
    }

    /// The frame encoded as a PNG file.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(
                &self.pixels,
                self.width,
                self.height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| Error::Setup(format!("encode the frame as PNG: {e}")))?;
        Ok(out)
    }

    /// Write the frame to `path` as a PNG file.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::write(path, self.to_png()?)
            .map_err(|e| Error::Setup(format!("write {}: {e}", path.display())))
    }

    /// Read a PNG file into a frame, converting it to RGBA8 when it is
    /// stored another way.
    pub fn open_png(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|e| Error::Setup(format!("read {}: {e}", path.display())))?;
        let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .map_err(|e| Error::Setup(format!("{} is not a PNG file: {e}", path.display())))?
            .into_rgba8();
        let (width, height) = image.dimensions();
        Ok(Self {
            width,
            height,
            pixels: image.into_raw(),
        })
    }
}

impl From<SurfaceFrame> for Frame {
    fn from(frame: SurfaceFrame) -> Self {
        Self {
            width: frame.width,
            height: frame.height,
            pixels: frame.rgba8,
        }
    }
}

/// The size, not the pixels: a frame holds millions of bytes.
impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_survives_a_png_round_trip() {
        let frame = Frame::from_rgba8(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        let path = std::env::temp_dir().join(format!("lumenui-frame-{}.png", std::process::id()));
        frame.save_png(&path).unwrap();
        let back = Frame::open_png(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(back, frame);
        assert_eq!(back.pixel(1, 0), Some([0, 0, 255, 128]));
        assert_eq!(back.pixel(2, 0), None);
    }

    #[test]
    fn pixels_must_fill_the_size() {
        assert!(Frame::from_rgba8(2, 2, vec![0; 12]).is_none());
    }
}
