//! A blank line between paragraphs must not shift the hit test.
//!
//! `TextGeometry` records one line entry per line that produced glyphs, so
//! a blank line has no entry. Deriving the entry's index from
//! `y / line_height` counts every visual line and therefore lands one
//! entry too far down for each blank line above the pointer - the caret
//! jumps to a lower line than the one clicked, and a drag highlights a
//! different line than the one under the pointer.

use lumen_text::{ShapeOptions, TextGeometry, TextShaper, WrapMode};
use lumen_text_cosmic::CosmicShaper;

const SIZE: f32 = 16.0;
const LINE_H: f32 = SIZE * 1.2;

/// "AAAA", blank, "BBBB", blank, "CCCC" - the shape of a markdown note.
const TEXT: &str = "AAAA\n\nBBBB\n\nCCCC";

fn geometry() -> TextGeometry {
    let mut shaper = CosmicShaper::new();
    let run = shaper
        .shape(
            TEXT,
            SIZE,
            ShapeOptions {
                width: Some(400.0),
                wrap: WrapMode::None,
                ..ShapeOptions::default()
            },
        )
        .expect("shaped");
    TextGeometry::from(&run).with_size(SIZE)
}

/// Pressing on a visual line resolves to a byte on THAT line.
#[test]
fn a_press_lands_on_the_line_under_the_pointer() {
    let g = geometry();
    // (visual line index, expected byte range on that line)
    for (line, lo, hi) in [(0usize, 0usize, 4usize), (2, 6, 10), (4, 12, 16)] {
        // Middle of the line's band, one glyph in from the left edge.
        let y = line as f32 * LINE_H + LINE_H * 0.5;
        let byte = g.x_to_byte(1.0, y);
        assert!(
            (lo..=hi).contains(&byte),
            "a press on visual line {line} (y={y}) resolved to byte {byte}, \
             which is not on that line ({lo}..={hi}); text {TEXT:?}"
        );
    }
}

/// The last line stays reachable, and a press below the text clamps to it
/// rather than wrapping back up.
#[test]
fn a_press_below_the_text_clamps_to_the_last_line() {
    let g = geometry();
    let byte = g.x_to_byte(1000.0, LINE_H * 40.0);
    assert_eq!(
        byte,
        TEXT.len(),
        "press past the end should land at the end"
    );
}
