//! The system clipboard: text, and images as RGBA8 pixels in memory.
//!
//! [`Clipboard`] is a system parameter over the clipboard the input plugin
//! opens, the same one text fields copy and paste through and the script
//! functions `copy_image` and `save_clipboard_image` reach. A system that
//! takes it runs on the main thread, because the platform ties the clipboard
//! to the thread that opened it.
//!
//! ```no_run
//! # extern crate lumenui_engine as lumenui;
//! use lumenui::prelude::*;
//!
//! fn copy_selection(clipboard: Clipboard) {
//!     let pixels = vec![255; 4 * 16 * 16];
//!     if let Some(image) = RgbaImage::new(16, 16, pixels) {
//!         clipboard.set_image(&image);
//!     }
//!     if let Some(pasted) = clipboard.image() {
//!         println!("{}x{}", pasted.width(), pasted.height());
//!     }
//! }
//! ```
//!
//! With no clipboard (a machine with no desktop session, a build that left
//! it out, a web page for images) the reads find nothing and the writes
//! report `false`.

use crate::bevy_ecs;
pub use crate::lumen_os_clipboard::ClipboardHost;
use bevy_ecs::system::{NonSend, SystemParam};

/// An image as RGBA8 pixels: four bytes per pixel, rows top to bottom, no
/// padding, straight (not premultiplied) alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl RgbaImage {
    /// An image of `width` by `height` from its pixels; `None` unless
    /// `pixels` holds exactly `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        let len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (pixels.len() == len).then_some(Self {
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

    /// The pixels, `width * height * 4` bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The pixels, owned.
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

/// The system clipboard, for a system to take as a parameter.
#[derive(SystemParam)]
pub struct Clipboard<'w> {
    host: Option<NonSend<'w, ClipboardHost>>,
}

impl Clipboard<'_> {
    /// Whether there is a clipboard to read and write.
    pub fn is_available(&self) -> bool {
        self.host.is_some()
    }

    /// The clipboard's text; empty when it holds none.
    pub fn text(&self) -> String {
        self.host
            .as_ref()
            .map(|h| h.read_text())
            .unwrap_or_default()
    }

    /// Put `text` on the clipboard. `false` when there is no clipboard or
    /// the platform refused.
    pub fn set_text(&self, text: &str) -> bool {
        self.host.as_ref().is_some_and(|h| h.write_text(text))
    }

    /// The clipboard's image, when it holds one.
    pub fn image(&self) -> Option<RgbaImage> {
        let (width, height, pixels) = self.host.as_ref()?.get_rgba8_image()?;
        RgbaImage::new(width, height, pixels)
    }

    /// Put `image` on the clipboard. `false` when there is no clipboard or
    /// the platform refused.
    pub fn set_image(&self, image: &RgbaImage) -> bool {
        self.host
            .as_ref()
            .is_some_and(|h| h.set_rgba8_image(image.width, image.height, image.pixels.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bevy_ecs::prelude::*;
    use crate::bevy_ecs::system::RunSystemOnce;

    #[test]
    fn an_image_must_hold_every_pixel() {
        assert!(RgbaImage::new(2, 2, vec![0; 16]).is_some());
        assert!(RgbaImage::new(2, 2, vec![0; 15]).is_none());
        assert!(RgbaImage::new(u32::MAX, u32::MAX, Vec::new()).is_none());
        let image = RgbaImage::new(1, 1, vec![1, 2, 3, 4]).unwrap();
        assert_eq!((image.width(), image.height()), (1, 1));
        assert_eq!(image.pixels(), &[1, 2, 3, 4]);
    }

    /// With no clipboard installed the parameter still resolves: reads find
    /// nothing and writes report failure.
    #[test]
    fn no_clipboard_reads_nothing_and_writes_fail() {
        let mut world = World::new();
        world
            .run_system_once(|clipboard: Clipboard| {
                assert!(!clipboard.is_available());
                assert_eq!(clipboard.text(), "");
                assert!(!clipboard.set_text("x"));
                assert!(clipboard.image().is_none());
                let image = RgbaImage::new(1, 1, vec![0; 4]).unwrap();
                assert!(!clipboard.set_image(&image));
            })
            .expect("the system runs");
    }
}
