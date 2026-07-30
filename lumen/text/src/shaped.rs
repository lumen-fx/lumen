//! D4: the shaped-once main-world store.
//!
//! [`ShapedText`] is a per-entity component produced ONCE per change in the
//! main world (see the runtime/layout producer) and read by the editing
//! logic ([`crate::TextGeometry`] queries) without any shaper dependency in
//! `lumen-input`. It also carries the [`crate::ShapedRun`] so the render
//! world can eventually CONSUME the same shape via the extract (O1) instead
//! of reshaping -- that render-consume step (D4-R) is separable and not yet
//! wired.
//!
//! Placement note: the doc (section 4.2) sketched `ShapedText` in
//! `lumen-core`, but `lumen-core` must not depend on `lumen-text` (that
//! would form a cycle -- `lumen-text` already depends on `lumen-core`).
//! The component therefore lives here in `lumen-text`, which owns
//! [`crate::ShapedRun`] / [`crate::TextGeometry`]; every consumer
//! (`lumen-input`, the layout producer) already depends on `lumen-text`.

use bevy_ecs::prelude::Component;

use crate::{ShapeOptions, ShapedRun, TextGeometry, TextShaper};

/// One shaped-once result for an entity's text, produced in the main world.
///
/// Consumed by BOTH the edit systems (via [`Self::geometry`]) and -- once
/// D4-R lands -- the extract (via [`Self::run`]). Invalidated by the
/// producer when [`Self::shape_version`] changes.
#[derive(Component, Clone, Debug)]
pub struct ShapedText {
    /// Glyphs + fonts, for the render draw path (D4-R consumer).
    pub run: ShapedRun,
    /// Byte<->pixel map, for the edit + IME consumers (D4a-e).
    pub geometry: TextGeometry,
    /// Version tag the shape was built from: [`crate::TextBuffer`]-style
    /// content version folded with size / width / wrap / family / weight.
    /// Lets the producer skip unchanged entities and lets a future
    /// `ExtractedText` damage diff compare a scalar rather than the glyph
    /// vec.
    pub shape_version: u64,
}

/// Companion viewport metrics written by the same producer alongside
/// [`ShapedText`]. Keeps [`TextGeometry`] a pure byte<->pixel map: the page
/// motion router (D7) reads `page_lines = (inner.y / line_h).floor()` from
/// here. (D7 itself is deferred -- its consumer lives in the locked
/// `lumen-runtime` -- but the producer still populates this so the data is
/// ready.)
#[derive(Component, Clone, Copy, Debug)]
pub struct TextViewport {
    /// Inner content box (box size minus padding), logical px.
    pub inner: glam::Vec2,
    /// Line height, logical px (`size_px * 1.2`).
    pub line_h: f32,
}

/// Pure, headless-testable shaping builder (O2): shape `display` once at
/// `size_px` under `opts`, bake the caret metrics, and package the result.
///
/// An empty / unshapeable `display` yields a ShapedText with an empty run
/// and empty geometry (rather than `None`), so an empty focused input still
/// carries a stable [`ShapedText`] the caret / IME consumers can read and
/// the producer can version-cache. The `Option` is retained for API
/// symmetry; it is presently always `Some`.
pub fn build_shaped_text(
    shaper: &mut dyn TextShaper,
    display: &str,
    size_px: f32,
    opts: ShapeOptions,
    shape_version: u64,
) -> Option<ShapedText> {
    let run = shaper
        .shape(display, size_px, opts)
        .unwrap_or_else(|| ShapedRun {
            font_data: std::sync::Arc::new(Vec::new()),
            font_index: 0,
            glyphs: Vec::new(),
            segments: Vec::new(),
            width: 0.0,
        });
    let geometry = TextGeometry::from(&run).with_size(size_px);
    Some(ShapedText {
        run,
        geometry,
        shape_version,
    })
}
