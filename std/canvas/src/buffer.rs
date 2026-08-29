//! Pixel buffers: the read-write side of a canvas.
//!
//! A canvas surface is write-only. What it holds is a list of drawing calls
//! bound for the GPU, not an image, so there is nothing on the CPU to read a
//! pixel back from and asking would mean stalling the frame on a readback.
//!
//! A buffer is the other half: plain CPU pixels a script creates, writes,
//! reads, loads from a PNG, saves to one, and draws onto a canvas. Filters,
//! generated textures, and sprite compositing all live here, and the surface
//! stays a one-way pipe.
//!
//! Pixels are stored straight (not premultiplied) `RGBA8`, which is what a
//! script means when it writes `0xff000080`: half-transparent pure red, with
//! the red channel still reading `0xff` after the write. The renderer is told
//! the data is straight and does the multiply itself, so nothing here is
//! lossy.

use std::path::Path;

use image::ImageEncoder;

/// One CPU pixel buffer.
#[derive(Clone, Debug)]
pub struct PixBuf {
    width: u32,
    height: u32,
    /// Straight RGBA8, row-major, `width * height * 4` bytes.
    pixels: Vec<u8>,
    /// Bumped by every write. The renderer's upload cache is keyed by it, so
    /// a buffer drawn every frame and never edited uploads once.
    generation: u64,
}

impl PixBuf {
    /// A transparent buffer of that size.
    #[must_use]
    pub fn new(width: u32, height: u32) -> PixBuf {
        PixBuf {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
            generation: 0,
        }
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// How many times this buffer has been written to.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The straight RGBA8 bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.pixels
    }

    /// The byte offset of a pixel, or `None` when it is outside.
    fn offset(&self, x: i64, y: i64) -> Option<usize> {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return None;
        }
        Some((y as usize * self.width as usize + x as usize) * 4)
    }

    /// One pixel as `0xRRGGBBAA`; a pixel outside the buffer reads 0, which
    /// is the transparent black a sampler outside an image would give.
    #[must_use]
    pub fn get_pixel(&self, x: i64, y: i64) -> u32 {
        match self.offset(x, y) {
            Some(i) => u32::from_be_bytes([
                self.pixels[i],
                self.pixels[i + 1],
                self.pixels[i + 2],
                self.pixels[i + 3],
            ]),
            None => 0,
        }
    }

    /// Write one pixel; a pixel outside the buffer is dropped.
    pub fn set_pixel(&mut self, x: i64, y: i64, rgba: u32) {
        if let Some(i) = self.offset(x, y) {
            self.pixels[i..i + 4].copy_from_slice(&rgba.to_be_bytes());
            self.generation += 1;
        }
    }

    /// A rectangle of pixels, row-major, one `0xRRGGBBAA` per pixel. Pixels
    /// outside the buffer read 0, so a region that hangs over an edge comes
    /// back the size it asked for.
    #[must_use]
    pub fn get_region(&self, x: i64, y: i64, width: u32, height: u32) -> Vec<u32> {
        let mut out = Vec::with_capacity(width as usize * height as usize);
        for row in 0..i64::from(height) {
            for col in 0..i64::from(width) {
                out.push(self.get_pixel(x + col, y + row));
            }
        }
        out
    }

    /// Write a rectangle of pixels back. Extra values are ignored and
    /// missing ones leave their pixels alone, so a short array is a partial
    /// write rather than a refusal.
    pub fn put_region(&mut self, x: i64, y: i64, width: u32, height: u32, pixels: &[u32]) {
        for row in 0..i64::from(height) {
            for col in 0..i64::from(width) {
                let Some(value) = pixels.get(row as usize * width as usize + col as usize) else {
                    return;
                };
                self.set_pixel(x + col, y + row, *value);
            }
        }
    }

    /// Fill a rectangle with one color.
    pub fn fill_rect(&mut self, x: i64, y: i64, width: u32, height: u32, rgba: u32) {
        for row in 0..i64::from(height) {
            for col in 0..i64::from(width) {
                self.set_pixel(x + col, y + row, rgba);
            }
        }
    }

    /// Decode a PNG into a fresh buffer, or say why not.
    pub fn load_png(path: &Path, pixel_cap: u64) -> Result<PixBuf, String> {
        let reader = image::ImageReader::open(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?
            .with_guessed_format()
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let (width, height) = reader
            .into_dimensions()
            .map_err(|e| format!("{} is not an image: {e}", path.display()))?;
        let pixels = u64::from(width) * u64::from(height);
        if pixels > pixel_cap {
            return Err(format!(
                "{} is {width}x{height}, over the {pixel_cap}-pixel cap",
                path.display()
            ));
        }
        let decoded = image::open(path)
            .map_err(|e| format!("cannot decode {}: {e}", path.display()))?
            .to_rgba8();
        Ok(PixBuf {
            width,
            height,
            pixels: decoded.into_raw(),
            generation: 0,
        })
    }

    /// Write this buffer out as a PNG, or say why not.
    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        let file = std::fs::File::create(path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file))
            .write_image(
                &self.pixels,
                self.width,
                self.height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("cannot encode {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pixel_reads_back_exactly_at_any_alpha() {
        let mut buf = PixBuf::new(4, 4);
        // Half-transparent pure red. A premultiplied store would round the
        // red channel down and never give it back.
        buf.set_pixel(1, 2, 0xff000080);
        assert_eq!(buf.get_pixel(1, 2), 0xff000080);
        assert_eq!(buf.get_pixel(0, 0), 0, "an untouched pixel is transparent");
    }

    #[test]
    fn a_pixel_outside_the_buffer_is_transparent_and_unwritable() {
        let mut buf = PixBuf::new(2, 2);
        buf.set_pixel(-1, 0, 0xffffffff);
        buf.set_pixel(0, 9, 0xffffffff);
        assert_eq!(buf.get_pixel(-1, 0), 0);
        assert_eq!(buf.get_pixel(0, 9), 0);
        assert_eq!(buf.generation(), 0, "a dropped write is not a write");
    }

    #[test]
    fn a_region_round_trips() {
        let mut buf = PixBuf::new(4, 4);
        buf.put_region(1, 1, 2, 2, &[1, 2, 3, 4]);
        assert_eq!(buf.get_region(1, 1, 2, 2), [1, 2, 3, 4]);
        // A region hanging over the edge comes back the size it asked for,
        // padded with transparent.
        assert_eq!(buf.get_region(3, 3, 2, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn filling_and_the_generation_track_the_writes() {
        let mut buf = PixBuf::new(3, 3);
        buf.fill_rect(0, 0, 2, 2, 0x00ff00ff);
        assert_eq!(buf.get_pixel(1, 1), 0x00ff00ff);
        assert_eq!(buf.get_pixel(2, 2), 0);
        assert_eq!(buf.generation(), 4);
    }

    #[test]
    fn a_png_round_trips_byte_for_byte() {
        let dir = std::env::temp_dir().join(format!("lumen-canvas-png-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("round-trip.png");

        let mut buf = PixBuf::new(3, 2);
        buf.set_pixel(0, 0, 0xff000080);
        buf.set_pixel(2, 1, 0x0000ffff);
        buf.save_png(&path).expect("save");

        let back = PixBuf::load_png(&path, 1_000_000).expect("load");
        assert_eq!((back.width(), back.height()), (3, 2));
        assert_eq!(back.bytes(), buf.bytes());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_image_over_the_cap_is_refused_by_size() {
        let dir = std::env::temp_dir().join(format!("lumen-canvas-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("big.png");
        PixBuf::new(64, 64).save_png(&path).expect("save");

        let err = PixBuf::load_png(&path, 1024).expect_err("over the cap");
        assert!(err.contains("over the"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_png_reports_rather_than_panics() {
        let err = PixBuf::load_png(Path::new("no-such-file.png"), 1024).expect_err("missing");
        assert!(err.contains("cannot read"), "{err}");
    }
}
