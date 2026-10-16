//! Images whose pixels change while they are on screen.
//!
//! A [`peniko::ImageData`] is immutable: its bytes are a blob, and a
//! renderer keys its upload cache off the blob's identity, so changing one
//! pixel means a new blob and a new upload of every pixel. That is the right
//! shape for an image file and the wrong one for a paint program, where a
//! brush stroke touches a few hundred pixels of a picture with millions.
//!
//! A [`PixelImage`] is the other shape. Its straight RGBA8 pixels live in
//! square tiles, each shared until something writes it, and each stamped
//! when it was last written. Three things follow:
//!
//! - **A copy is cheap and stays a copy.** Cloning one shares every tile; a
//!   write afterwards copies only the tiles it touches. A drawing that keeps
//!   a clone keeps the pixels it was drawn with, however the original changes
//!   later.
//! - **A renderer uploads what changed.** An image keeps one identity
//!   ([`PixelImage::id`]) across every write, so a renderer keeps one resident
//!   copy for it and brings that copy up to date by comparing tile stamps,
//!   re-uploading only the tiles whose stamp moved.
//! - **Untouched pixels cost nothing.** A new image is one shared transparent
//!   tile; memory grows with what is written, not with the image's size.
//!
//! [`Painter::draw_pixels`](crate::Painter::draw_pixels) is how one reaches a
//! frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

/// The side of a tile, in pixels. Large enough that a frame's bookkeeping
/// stays small (a 4096x4096 image is 4096 tiles), small enough that a brush
/// dab re-uploads a few kilobytes rather than a row of the whole image.
pub const TILE: u32 = 64;

/// The bytes one tile holds: always a full tile, so every tile can share one
/// transparent allocation. The part of an edge tile past the image's edge is
/// never read.
const TILE_BYTES: usize = (TILE * TILE * 4) as usize;

/// Bytes per row inside a tile.
const TILE_STRIDE: usize = (TILE * 4) as usize;

/// Issues image identities. Starts at 1 so 0 can never name an image.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Issues tile stamps. Starts at 1: stamp 0 means "transparent and never
/// written", which is what a renderer's fresh resident copy already holds.
static NEXT_STAMP: AtomicU64 = AtomicU64::new(1);

fn fresh_stamp() -> u64 {
    NEXT_STAMP.fetch_add(1, Ordering::Relaxed)
}

/// One tile: its pixels and when they were last written.
#[derive(Clone)]
struct Tile {
    /// 0 until the first write; afterwards unique across the process, so
    /// equal stamps mean equal pixels.
    stamp: u64,
    pixels: Arc<Vec<u8>>,
}

/// A straight (not premultiplied) RGBA8 image that changes in place, tiled so
/// a write copies and a renderer re-uploads only what it touched. See the
/// module docs.
///
/// Coordinates are signed and sizes unsigned: anything outside the image is
/// clipped away before work is done, so a rectangle the caller did not bound
/// costs no more than the image itself.
#[derive(Clone)]
pub struct PixelImage {
    id: u64,
    width: u32,
    height: u32,
    /// Tiles across.
    cols: u32,
    /// Row-major, `cols * rows` of them.
    tiles: Arc<Vec<Tile>>,
    /// The stamp of the latest write, or 0 for none: two clones with the same
    /// version hold the same pixels.
    version: u64,
}

impl std::fmt::Debug for PixelImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelImage")
            .field("id", &self.id)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// One tile as a renderer reads it: where it sits in the image, its stamp,
/// and its pixels.
#[derive(Clone, Copy, Debug)]
pub struct TileView<'a> {
    /// The tile's index in [`PixelImage::tile_count`] order.
    pub index: usize,
    /// Left edge, in image pixels.
    pub x: u32,
    /// Top edge, in image pixels.
    pub y: u32,
    /// Width inside the image: [`TILE`], or less for a tile on the right edge.
    pub width: u32,
    /// Height inside the image: [`TILE`], or less for a tile on the bottom edge.
    pub height: u32,
    /// When the tile was last written; 0 when it never was and is transparent.
    pub stamp: u64,
    /// Straight RGBA8, [`TILE`] pixels per row whatever `width` is. Only the
    /// first `width` pixels of the first `height` rows are the image's.
    pub pixels: &'a [u8],
}

impl TileView<'_> {
    /// Bytes per row of [`Self::pixels`].
    pub const STRIDE: usize = TILE_STRIDE;
}

impl PixelImage {
    /// A transparent image of that size, with a fresh identity.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let cols = width.div_ceil(TILE);
        let rows = height.div_ceil(TILE);
        let blank = Arc::new(vec![0u8; TILE_BYTES]);
        let tiles = (0..cols as usize * rows as usize)
            .map(|_| Tile {
                stamp: 0,
                pixels: Arc::clone(&blank),
            })
            .collect();
        PixelImage {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            cols,
            tiles: Arc::new(tiles),
            version: 0,
        }
    }

    /// An image holding `rgba`, straight RGBA8 rows of `width` pixels. Short
    /// data leaves the rest transparent; extra data is ignored.
    #[must_use]
    pub fn from_rgba8(width: u32, height: u32, rgba: &[u8]) -> Self {
        let mut image = PixelImage::new(width, height);
        image.write_rgba8(0, 0, width, height, rgba);
        image
    }

    /// The identity a renderer keeps its resident copy under. Every clone and
    /// every later state of this image shares it.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
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

    /// Changes every time a write lands, and only then. Two clones of one
    /// image with the same version hold the same pixels.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// How many tiles the image is cut into.
    #[must_use]
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Every tile, row-major, for a renderer bringing a resident copy up to
    /// date: compare each [`TileView::stamp`] with the one it uploaded last.
    pub fn tiles(&self) -> impl Iterator<Item = TileView<'_>> + '_ {
        self.tiles.iter().enumerate().map(|(index, tile)| {
            let col = index as u32 % self.cols.max(1);
            let row = index as u32 / self.cols.max(1);
            let (x, y) = (col * TILE, row * TILE);
            TileView {
                index,
                x,
                y,
                width: (self.width - x).min(TILE),
                height: (self.height - y).min(TILE),
                stamp: tile.stamp,
                pixels: &tile.pixels,
            }
        })
    }

    /// The part of a requested rectangle inside the image, as `(left, top,
    /// right, bottom)`, or `None` when none of it is.
    fn clip(&self, x: i64, y: i64, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        let left = x.max(0);
        let top = y.max(0);
        let right = x
            .saturating_add(i64::from(width))
            .min(i64::from(self.width));
        let bottom = y
            .saturating_add(i64::from(height))
            .min(i64::from(self.height));
        (left < right && top < bottom).then_some((
            left as u32,
            top as u32,
            right as u32,
            bottom as u32,
        ))
    }

    /// Run `write` once per tile the clipped rectangle covers, with the
    /// tile's own pixels (copied first if anything else shares them) and the
    /// part of the rectangle inside it, in image pixels. Stamps every tile
    /// written.
    fn for_tiles_mut(
        &mut self,
        (left, top, right, bottom): (u32, u32, u32, u32),
        mut write: impl FnMut(&mut [u8], (u32, u32), (u32, u32, u32, u32)),
    ) {
        let cols = self.cols;
        let tiles = Arc::make_mut(&mut self.tiles);
        let stamp = fresh_stamp();
        for row in top / TILE..bottom.div_ceil(TILE) {
            for col in left / TILE..right.div_ceil(TILE) {
                let origin = (col * TILE, row * TILE);
                let part = (
                    left.max(origin.0),
                    top.max(origin.1),
                    right.min(origin.0 + TILE),
                    bottom.min(origin.1 + TILE),
                );
                let tile = &mut tiles[(row * cols + col) as usize];
                write(Arc::make_mut(&mut tile.pixels).as_mut_slice(), origin, part);
                tile.stamp = stamp;
            }
        }
        self.version = stamp;
    }

    /// Write straight RGBA8 rows of `width` pixels into the rectangle at
    /// `(x, y)`. What falls outside the image is dropped; short data writes
    /// what it has and leaves the rest alone.
    pub fn write_rgba8(&mut self, x: i64, y: i64, width: u32, height: u32, rgba: &[u8]) {
        let Some(clip) = self.clip(x, y, width, height) else {
            return;
        };
        if rgba.len() < 4 {
            return;
        }
        let stride = width as usize * 4;
        // Rows the data covers; a short buffer stops early rather
        // than reading past its end.
        let full_rows = rgba.len() / stride.max(1);
        let tail = rgba.len() % stride.max(1) / 4;
        self.for_tiles_mut(clip, |tile, (tx, ty), (l, t, r, b)| {
            for py in t..b {
                let src_row = (i64::from(py) - y) as usize;
                let src_col = (i64::from(l) - x) as usize;
                let mut count = (r - l) as usize;
                if src_row > full_rows || (src_row == full_rows && src_col >= tail) {
                    return;
                }
                if src_row == full_rows {
                    count = count.min(tail - src_col);
                }
                let src = src_row * stride + src_col * 4;
                let dst = (py - ty) as usize * TILE_STRIDE + (l - tx) as usize * 4;
                tile[dst..dst + count * 4].copy_from_slice(&rgba[src..src + count * 4]);
            }
        });
    }

    /// Read the rectangle at `(x, y)` into `out` as straight RGBA8 rows of
    /// `width` pixels. Pixels outside the image read as transparent black; a
    /// short `out` takes what fits.
    pub fn read_rgba8(&self, x: i64, y: i64, width: u32, height: u32, out: &mut [u8]) {
        let stride = width as usize * 4;
        let want = (stride * height as usize).min(out.len());
        out[..want].fill(0);
        let Some((left, top, right, bottom)) = self.clip(x, y, width, height) else {
            return;
        };
        for py in top..bottom {
            let dst_row = (i64::from(py) - y) as usize * stride;
            let row = py / TILE;
            let mut px = left;
            while px < right {
                let col = px / TILE;
                let end = right.min((col + 1) * TILE);
                let tile = &self.tiles[(row * self.cols + col) as usize].pixels;
                let src = (py - row * TILE) as usize * TILE_STRIDE + (px - col * TILE) as usize * 4;
                let dst = dst_row + (i64::from(px) - x) as usize * 4;
                let len = (end - px) as usize * 4;
                if dst >= want {
                    return;
                }
                let len = len.min(want - dst);
                out[dst..dst + len].copy_from_slice(&tile[src..src + len]);
                px = end;
            }
        }
    }

    /// One pixel, straight RGBA8; transparent black outside the image.
    #[must_use]
    pub fn pixel(&self, x: i64, y: i64) -> [u8; 4] {
        let mut out = [0u8; 4];
        self.read_rgba8(x, y, 1, 1, &mut out);
        out
    }

    /// Fill a rectangle with one straight RGBA8 color, clipped to the image.
    pub fn fill_rgba8(&mut self, x: i64, y: i64, width: u32, height: u32, rgba: [u8; 4]) {
        let Some(clip) = self.clip(x, y, width, height) else {
            return;
        };
        self.for_tiles_mut(clip, |tile, (tx, ty), (l, t, r, b)| {
            for py in t..b {
                let start = (py - ty) as usize * TILE_STRIDE + (l - tx) as usize * 4;
                for px in tile[start..start + (r - l) as usize * 4].chunks_exact_mut(4) {
                    px.copy_from_slice(&rgba);
                }
            }
        });
    }

    /// Change the size, keeping the pixels the old and new sizes share at
    /// the top left; new area is transparent. The identity stays.
    pub fn resize(&mut self, width: u32, height: u32) {
        if (width, height) == (self.width, self.height) {
            return;
        }
        let mut next = PixelImage::new(width, height);
        next.id = self.id;
        let keep_w = self.width.min(width);
        let keep_h = self.height.min(height);
        if keep_w > 0 && keep_h > 0 {
            let mut rows = vec![0u8; keep_w as usize * keep_h as usize * 4];
            self.read_rgba8(0, 0, keep_w, keep_h, &mut rows);
            next.write_rgba8(0, 0, keep_w, keep_h, &rows);
        }
        // A resize is a change even when nothing was kept.
        next.version = fresh_stamp();
        *self = next;
    }

    /// The whole image as contiguous straight RGBA8 rows.
    #[must_use]
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.width as usize * self.height as usize * 4];
        self.read_rgba8(0, 0, self.width, self.height, &mut out);
        out
    }

    /// The whole image as a fresh [`ImageData`]: a full copy, for a sink that
    /// keeps no resident copies and draws through
    /// [`Painter::draw_image`](crate::Painter::draw_image).
    #[must_use]
    pub fn to_image_data(&self) -> ImageData {
        ImageData {
            data: Blob::new(Arc::new(self.to_rgba8())),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: self.width,
            height: self.height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamps(image: &PixelImage) -> Vec<u64> {
        image.tiles().map(|t| t.stamp).collect()
    }

    #[test]
    fn a_region_round_trips_across_tile_edges() {
        let mut image = PixelImage::new(200, 150);
        let data: Vec<u8> = (0..100 * 90 * 4).map(|i| (i % 251) as u8).collect();
        image.write_rgba8(30, 40, 100, 90, &data);
        let mut back = vec![0u8; data.len()];
        image.read_rgba8(30, 40, 100, 90, &mut back);
        assert_eq!(back, data);
        assert_eq!(image.pixel(29, 40), [0; 4], "outside the write");
    }

    #[test]
    fn a_write_stamps_only_the_tiles_it_touches() {
        let mut image = PixelImage::new(256, 256);
        assert!(stamps(&image).iter().all(|s| *s == 0));
        image.fill_rgba8(70, 70, 8, 8, [255, 0, 0, 255]);
        let touched: Vec<usize> = image
            .tiles()
            .filter(|t| t.stamp != 0)
            .map(|t| t.index)
            .collect();
        assert_eq!(touched, [5], "one 8x8 dab inside tile (1, 1)");
    }

    #[test]
    fn a_clone_keeps_the_pixels_it_was_taken_with() {
        let mut image = PixelImage::new(128, 128);
        image.fill_rgba8(0, 0, 128, 128, [1, 2, 3, 4]);
        let snapshot = image.clone();
        image.fill_rgba8(0, 0, 1, 1, [9, 9, 9, 9]);
        assert_eq!(snapshot.pixel(0, 0), [1, 2, 3, 4]);
        assert_eq!(image.pixel(0, 0), [9, 9, 9, 9]);
        assert_eq!(snapshot.id(), image.id(), "one identity for every state");
        assert_ne!(snapshot.version(), image.version());
        // The untouched tiles are still the snapshot's allocation.
        assert!(Arc::ptr_eq(
            &snapshot.tiles[1].pixels,
            &image.tiles[1].pixels
        ));
    }

    #[test]
    fn hostile_coordinates_neither_panic_nor_write() {
        let mut image = PixelImage::new(2, 2);
        image.fill_rgba8(i64::MAX, i64::MAX, u32::MAX, u32::MAX, [255; 4]);
        image.write_rgba8(i64::MIN, i64::MIN, 2, 2, &[255; 16]);
        assert_eq!(image.version(), 0);
        let mut out = [7u8; 8];
        image.read_rgba8(i64::MAX, 0, 2, 1, &mut out);
        assert_eq!(out, [0; 8]);
    }

    #[test]
    fn short_data_writes_what_it_has() {
        let mut image = PixelImage::new(4, 4);
        image.write_rgba8(0, 0, 4, 4, &[255; 12]);
        assert_eq!(image.pixel(2, 0), [255; 4]);
        assert_eq!(image.pixel(3, 0), [0; 4]);
        assert_eq!(image.pixel(0, 1), [0; 4]);
    }

    #[test]
    fn a_resize_keeps_the_shared_corner_and_the_identity() {
        let mut image = PixelImage::new(100, 100);
        image.fill_rgba8(0, 0, 100, 100, [5, 5, 5, 255]);
        let id = image.id();
        image.resize(150, 50);
        assert_eq!((image.width(), image.height()), (150, 50));
        assert_eq!(image.pixel(99, 49), [5, 5, 5, 255]);
        assert_eq!(image.pixel(120, 10), [0; 4]);
        assert_eq!(image.id(), id);
    }

    #[test]
    fn the_flat_copy_is_the_image() {
        let mut image = PixelImage::new(70, 3);
        image.fill_rgba8(65, 1, 5, 1, [1, 2, 3, 4]);
        let flat = image.to_image_data();
        let bytes = flat.data.data();
        let at = (70 + 66) * 4;
        assert_eq!(&bytes[at..at + 4], &[1, 2, 3, 4]);
        assert_eq!(bytes.len(), 70 * 3 * 4);
    }
}
