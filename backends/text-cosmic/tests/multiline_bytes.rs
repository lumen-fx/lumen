//! Byte offsets in a shaped multi-line run must be offsets into the string
//! that was shaped.
//!
//! cosmic-text reports `LayoutGlyph::start` / `end` relative to the
//! *buffer line* the glyph sits on, so every line after the first restarts
//! at zero. Everything downstream (`TextGeometry::x_to_byte`,
//! `byte_to_caret`, selection rects) treats those numbers as offsets into
//! the whole run, so a line-local offset silently aliases onto the first
//! line: clicking line two lands the caret on line one.

use lumen_text::{ShapeOptions, TextShaper};
use lumen_text_cosmic::CosmicShaper;

/// Two four-byte lines separated by one newline: "AAAA\nBBBB". Line 0
/// covers bytes 0..4, line 1 covers bytes 5..9.
const TWO_LINE: &str = "AAAA\nBBBB";

#[test]
fn multiline_glyph_offsets_span_the_whole_string() {
    let mut shaper = CosmicShaper::new();
    let run = shaper
        .shape(TWO_LINE, 16.0, ShapeOptions::default())
        .expect("shape");

    let max_end = run.glyphs.iter().map(|g| g.byte_end).max().unwrap_or(0);
    assert_eq!(
        max_end as usize,
        TWO_LINE.len(),
        "the last glyph must end at the end of the shaped string, not at \
         the end of its own line; glyph offsets are line-local"
    );

    // The second line's glyphs must not alias onto the first line's bytes.
    let second_line_glyphs: Vec<_> = run
        .glyphs
        .iter()
        .filter(|g| g.y > 1.0)
        .map(|g| (g.byte_start, g.byte_end))
        .collect();
    assert!(
        !second_line_glyphs.is_empty(),
        "expected glyphs on a second baseline"
    );
    for (lo, hi) in &second_line_glyphs {
        assert!(
            *lo >= 5,
            "second-line glyph claims byte {lo}..{hi}, which belongs to \
             line one"
        );
    }
}

/// Every glyph's byte range must index the shaped string, and consecutive
/// glyphs must not repeat a range already used by an earlier line.
#[test]
fn glyph_offsets_are_unique_across_lines() {
    let mut shaper = CosmicShaper::new();
    let run = shaper
        .shape(TWO_LINE, 16.0, ShapeOptions::default())
        .expect("shape");
    let mut seen = std::collections::HashSet::new();
    for g in &run.glyphs {
        assert!(
            TWO_LINE
                .get(g.byte_start as usize..g.byte_end as usize)
                .is_some(),
            "glyph range {}..{} does not index {TWO_LINE:?}",
            g.byte_start,
            g.byte_end
        );
        assert!(
            seen.insert((g.byte_start, g.byte_end)),
            "byte range {}..{} shaped twice; lines are aliasing",
            g.byte_start,
            g.byte_end
        );
    }
}
