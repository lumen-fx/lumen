//! Images the render backend holds rather than the CPU.
//!
//! A [`PixelImage`] is pixels a frame carries to the backend. Some pixels
//! never exist on the CPU at all: a frame another library or another process
//! renders straight into GPU memory. Those reach a frame as a [`TextureId`]:
//! a painter draws the id with [`crate::Painter::draw_texture`], and the
//! backend looks up the texture registered under it when it renders.
//!
//! A backend shows such a texture beneath the frame, in place, rather than
//! copying it into the frame. Some effects cannot apply to a texture drawn
//! that way, and a draw under one of them is refused with a
//! [`TextureRefusal`] that says which.

use crate::PixelImage;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

/// Names one texture a render backend holds, process-wide.
///
/// Issued from a counter, so two ids never collide; whoever registers a
/// texture with the backend and whoever draws it agree on the id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureId(NonZeroU64);

impl TextureId {
    /// A fresh id no other call returns.
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(NonZeroU64::new(n).expect("the counter starts at 1"))
    }

    /// The id as a number, never 0.
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl Default for TextureId {
    fn default() -> Self {
        Self::new()
    }
}

/// One draw of a backend texture.
#[derive(Clone, Copy, Debug)]
pub struct BackendImage<'a> {
    /// The texture to draw.
    pub id: TextureId,
    /// Its size in texture pixels. A draw maps one texture pixel to one unit,
    /// as [`crate::Painter::draw_pixels`] does, and the transform scales it
    /// into place.
    pub width: u32,
    /// See [`Self::width`].
    pub height: u32,
    /// What a sink that cannot show the texture draws instead, at the same
    /// place and size; `None` draws nothing.
    pub fallback: Option<&'a PixelImage>,
}

/// Why a sink did not show a backend texture.
///
/// A sink shows a texture beneath the frame and leaves a hole in the frame
/// where it goes, so the texture is drawn as it is: opaque, upright, and
/// unblended. A draw that would need anything else is refused rather than
/// approximated, and the variant names what was in the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureRefusal {
    /// This sink holds no backend textures (a CPU renderer, a recording that
    /// was never replayed on a GPU), or none is registered under the id.
    Unavailable,
    /// The draw is rotated, skewed, or mirrored; only scaling and moving are
    /// shown.
    Transform,
    /// The draw is inside a group that fades (CSS `opacity` below 1).
    Opacity,
    /// The draw is inside a layer with a blend mode or composite operation
    /// other than plain source-over.
    Blend,
    /// The draw is inside a masked group (CSS `mask-image`).
    Mask,
    /// The draw is inside a layer the sink renders apart from the frame and
    /// composites later (a cached group, a recorded fragment).
    Layer,
}

impl TextureRefusal {
    /// A short name for the reason: `unavailable`, `transform`, `opacity`,
    /// `blend`, `mask` or `layer`.
    pub fn as_str(self) -> &'static str {
        match self {
            TextureRefusal::Unavailable => "unavailable",
            TextureRefusal::Transform => "transform",
            TextureRefusal::Opacity => "opacity",
            TextureRefusal::Blend => "blend",
            TextureRefusal::Mask => "mask",
            TextureRefusal::Layer => "layer",
        }
    }
}

impl std::fmt::Display for TextureRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TextureRefusal::Unavailable => "the renderer holds no such texture",
            TextureRefusal::Transform => "it is rotated, skewed or mirrored",
            TextureRefusal::Opacity => "it is inside an element with opacity below 1",
            TextureRefusal::Blend => "it is inside a layer that blends",
            TextureRefusal::Mask => "it is inside an element with a mask",
            TextureRefusal::Layer => "it is inside a layer painted apart from the frame",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids are distinct and never 0.
    #[test]
    fn ids_are_unique_and_nonzero() {
        let a = TextureId::new();
        let b = TextureId::default();
        assert_ne!(a, b);
        assert!(a.get() > 0 && b.get() > 0);
    }

    /// Every reason has a short name and a sentence.
    #[test]
    fn every_refusal_names_itself() {
        for reason in [
            TextureRefusal::Unavailable,
            TextureRefusal::Transform,
            TextureRefusal::Opacity,
            TextureRefusal::Blend,
            TextureRefusal::Mask,
            TextureRefusal::Layer,
        ] {
            assert!(!reason.as_str().is_empty());
            assert!(!reason.to_string().is_empty());
        }
    }
}
