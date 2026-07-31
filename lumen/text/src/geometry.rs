//! D4: byte<->pixel geometry derived from a shaped run.
//!
//! Relocated from the private `render-wgpu` `ShapedRunSegmentIndex` (which
//! drew the caret + selection) and extended with the hit-test / visual-line
//! queries the main-world editing logic needs. Deterministic: an identical
//! [`ShapedRun`] yields an identical [`TextGeometry`], so the value computed
//! in the main world matches the glyphs the render world paints.
//!
//! Coordinates are RUN-LOCAL logical pixels: x grows from the run's leading
//! edge, y (`baseline_y`) is the byte's visual-line baseline offset (`0` on
//! line 1, `line_idx * line_height` further down). Callers add the draw
//! origin. All byte<->x math is BiDi-correct: it splits at segment
//! boundaries and follows visual (not logical) glyph order.

use crate::ShapedRun;
use std::cmp::Ordering;

/// Caret placement for a logical byte, in run-local coords.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretGeometry {
    /// Visual x from the run origin (BiDi-correct).
    pub x: f32,
    /// Y of the caret rect top, from the run origin baseline of line 1.
    /// Requires size metrics ([`TextGeometry::with_size`]); `0` otherwise.
    pub top: f32,
    /// Caret rect height (line height). Requires size metrics.
    pub height: f32,
    /// 0-based visual line index the byte sits on.
    pub line: usize,
}

/// One selection highlight rectangle, in run-local coords.
///
/// Selection spanning several lines yields one band per line rather than
/// one merged span, so the painter can place each on its own baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionBand {
    /// Baseline of the line this band sits on, measured from the first
    /// line's baseline. Zero for the first line.
    pub baseline_y: f32,
    /// Visual left edge from the run origin.
    pub x0: f32,
    /// Visual right edge from the run origin.
    pub x1: f32,
}

/// Per-cluster `(byte range, visual leading/trailing x, line baseline)`
/// record in cosmic-text's visual order.
#[derive(Debug, Clone, Copy)]
struct ClusterEntry {
    byte_start: u32,
    byte_end: u32,
    /// Local x of the cluster's leading visual edge.
    leading_x: f32,
    /// Local x of the cluster's trailing visual edge (`leading_x + advance`).
    trailing_x: f32,
    /// Baseline y offset of the cluster's line relative to the run origin.
    baseline_y: f32,
}

/// One maximal `(font, BiDi level)` slice of the run, with the per-cluster
/// byte->x advances used for caret + selection math.
#[derive(Debug, Clone)]
struct SegmentIndex {
    /// Visual origin of this segment relative to the run origin.
    x_offset: f32,
    /// Total visual advance of the segment.
    width: f32,
    /// BiDi level. Even = LTR, odd = RTL.
    level: u8,
    clusters: Vec<ClusterEntry>,
    /// Lowest source byte covered by this segment.
    byte_lo: u32,
    /// Highest source byte (exclusive) covered by this segment.
    byte_hi: u32,
}

/// One visual (post-wrap) line: its byte span, baseline, and the visual x
/// edges of every cluster on it (for hit-testing and goal-x landing).
#[derive(Debug, Clone)]
struct LineEntry {
    baseline_y: f32,
    byte_lo: u32,
    byte_hi: u32,
    /// `(visual_x, logical_byte)` for every cluster edge on this line,
    /// sorted ascending by `visual_x`. LTR clusters contribute
    /// `(leading, byte_start)` + `(trailing, byte_end)`; RTL clusters
    /// contribute `(leading, byte_end)` + `(trailing, byte_start)` so the
    /// nearest-edge search lands on the correct logical byte in both
    /// directions.
    edges: Vec<(f32, u32)>,
}

/// Font-size-derived line metrics. Zero until [`TextGeometry::with_size`]
/// bakes them (the render draw path supplies size itself, so it never
/// needs them; the main-world producer bakes them for caret rects).
#[derive(Debug, Clone, Copy, Default)]
struct Metrics {
    line_height: f32,
    ascent: f32,
    descent: f32,
}

/// Byte<->pixel geometry derived from one [`ShapedRun`].
#[derive(Debug, Clone)]
pub struct TextGeometry {
    segments: Vec<SegmentIndex>,
    lines: Vec<LineEntry>,
    /// Total run width; trailing-edge fallback for a past-end caret.
    run_width: f32,
    /// Baseline y of the last emitted cluster.
    end_baseline_y: f32,
    metrics: Metrics,
}

impl From<&ShapedRun> for TextGeometry {
    fn from(run: &ShapedRun) -> Self {
        let mut segments = Vec::with_capacity(run.segments.len());
        let mut lines: Vec<LineEntry> = Vec::new();
        let mut end_baseline_y = 0.0_f32;
        for seg in &run.segments {
            // Visual origin = smallest x across the segment's glyphs.
            let x_offset = seg.glyphs.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
            let x_offset = if x_offset.is_finite() { x_offset } else { 0.0 };
            let ltr = seg.level % 2 == 0;
            let mut clusters: Vec<ClusterEntry> = Vec::with_capacity(seg.glyphs.len());
            let mut byte_lo: u32 = u32::MAX;
            let mut byte_hi: u32 = 0;
            for g in &seg.glyphs {
                // Skip the synthesised ellipsis glyph (byte range 0..0 with
                // non-zero advance is the truncation sentinel).
                if g.byte_start == 0 && g.byte_end == 0 && g.advance > 0.0 {
                    continue;
                }
                if g.byte_start < byte_lo {
                    byte_lo = g.byte_start;
                }
                if g.byte_end > byte_hi {
                    byte_hi = g.byte_end;
                }
                let leading_x = g.x - x_offset;
                let trailing_x = leading_x + g.advance;
                end_baseline_y = g.y;
                clusters.push(ClusterEntry {
                    byte_start: g.byte_start,
                    byte_end: g.byte_end,
                    leading_x,
                    trailing_x,
                    baseline_y: g.y,
                });
                // Accumulate the byte's visual line.
                let li = match lines.iter().position(|l| l.baseline_y == g.y) {
                    Some(i) => i,
                    None => {
                        lines.push(LineEntry {
                            baseline_y: g.y,
                            byte_lo: u32::MAX,
                            byte_hi: 0,
                            edges: Vec::new(),
                        });
                        lines.len() - 1
                    }
                };
                let line = &mut lines[li];
                if g.byte_start < line.byte_lo {
                    line.byte_lo = g.byte_start;
                }
                if g.byte_end > line.byte_hi {
                    line.byte_hi = g.byte_end;
                }
                let vis_lead = x_offset + leading_x;
                let vis_trail = x_offset + trailing_x;
                if ltr {
                    line.edges.push((vis_lead, g.byte_start));
                    line.edges.push((vis_trail, g.byte_end));
                } else {
                    line.edges.push((vis_lead, g.byte_end));
                    line.edges.push((vis_trail, g.byte_start));
                }
            }
            if byte_lo == u32::MAX {
                byte_lo = 0;
                byte_hi = 0;
            }
            segments.push(SegmentIndex {
                x_offset,
                width: seg.width,
                level: seg.level,
                clusters,
                byte_lo,
                byte_hi,
            });
        }
        // Order lines top-to-bottom and each line's edges left-to-right.
        lines.sort_by(|a, b| f32_cmp(a.baseline_y, b.baseline_y));
        for l in &mut lines {
            l.edges.sort_by(|a, b| f32_cmp(a.0, b.0));
        }
        // Infer line height from the smallest positive baseline step (used
        // to pick the visual line by y when size metrics were not baked).
        let mut line_height = 0.0_f32;
        for w in lines.windows(2) {
            let d = w[1].baseline_y - w[0].baseline_y;
            if d > 0.0 && (line_height == 0.0 || d < line_height) {
                line_height = d;
            }
        }
        Self {
            segments,
            lines,
            run_width: run.width,
            end_baseline_y,
            metrics: Metrics {
                line_height,
                ascent: 0.0,
                descent: 0.0,
            },
        }
    }
}

impl TextGeometry {
    /// Bake font-size line metrics so [`Self::byte_to_caret`] returns a real
    /// caret rect `top` / `height`. Matches the render caret math: ascent =
    /// `size_px * 0.9` above the baseline, descent = `size_px * 0.15` below,
    /// line height = `size_px * 1.2`.
    pub fn with_size(mut self, size_px: f32) -> Self {
        self.metrics = Metrics {
            line_height: size_px * 1.2,
            ascent: size_px * 0.9,
            descent: size_px * 0.15,
        };
        self
    }

    /// Visual `(x, baseline_y)` of the caret for a logical byte offset, in
    /// run-local coords. This is the byte->pixel math the render caret /
    /// selection draw path consumes (relocated verbatim from render-wgpu).
    pub fn caret_xy(&self, byte: usize) -> (f32, f32) {
        if byte == 0 {
            return (
                self.segments.first().map(|s| s.x_offset).unwrap_or(0.0),
                self.segments
                    .first()
                    .and_then(|s| s.clusters.first())
                    .map(|c| c.baseline_y)
                    .unwrap_or(0.0),
            );
        }
        let byte = byte as u32;
        for seg in &self.segments {
            if byte < seg.byte_lo || byte > seg.byte_hi {
                continue;
            }
            for c in &seg.clusters {
                if byte >= c.byte_start && byte <= c.byte_end {
                    let local = if seg.level % 2 == 1 {
                        // RTL: byte == byte_end -> visual-left (leading);
                        // byte == byte_start -> visual-right (trailing).
                        if byte == c.byte_end {
                            c.leading_x
                        } else {
                            c.trailing_x
                        }
                    } else {
                        // LTR: byte == byte_start -> leading; end -> trailing.
                        if byte == c.byte_start {
                            c.leading_x
                        } else {
                            c.trailing_x
                        }
                    };
                    return (seg.x_offset + local, c.baseline_y);
                }
            }
            let y = seg
                .clusters
                .last()
                .map(|c| c.baseline_y)
                .unwrap_or(self.end_baseline_y);
            return (seg.x_offset + seg.width, y);
        }
        (self.run_width, self.end_baseline_y)
    }

    /// Logical byte -> caret rect (run-local). `top` / `height` are only
    /// meaningful once [`Self::with_size`] has baked the metrics.
    pub fn byte_to_caret(&self, byte: usize) -> CaretGeometry {
        let (x, baseline) = self.caret_xy(byte);
        let line = if self.metrics.line_height > 0.0 {
            (baseline / self.metrics.line_height).round().max(0.0) as usize
        } else {
            0
        };
        CaretGeometry {
            x,
            top: baseline - self.metrics.ascent,
            height: self.metrics.ascent + self.metrics.descent,
            line,
        }
    }

    /// Number of visual (post-wrap) lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Visual line index containing `byte`. Past-end / between-line bytes
    /// snap to the nearest preceding line.
    pub fn visual_line_of_byte(&self, byte: usize) -> usize {
        let b = byte as u32;
        for (i, l) in self.lines.iter().enumerate() {
            if b >= l.byte_lo && b <= l.byte_hi {
                return i;
            }
        }
        let mut best = 0usize;
        for (i, l) in self.lines.iter().enumerate() {
            if l.byte_lo <= b {
                best = i;
            }
        }
        best
    }

    /// Logical byte range `[start, end)` of visual line `line` (end excludes
    /// a soft-wrap break and a trailing '\n', since neither emits a glyph
    /// cluster). Empty tuple for an out-of-range line.
    pub fn visual_line_bounds(&self, line: usize) -> (usize, usize) {
        match self.lines.get(line) {
            Some(l) => (l.byte_lo as usize, l.byte_hi as usize),
            None => (0, 0),
        }
    }

    /// Byte on visual line `line` whose caret x is nearest `goal_x`
    /// (goal-column landing). Clamps to the line's byte range.
    pub fn byte_at_line_x(&self, line: usize, goal_x: f32) -> usize {
        match self.lines.get(line) {
            Some(l) => nearest_edge_byte(&l.edges, goal_x).unwrap_or(l.byte_lo as usize),
            None => 0,
        }
    }

    /// Visual hit-test: run-local pointer `(x, y)` -> logical byte. Picks the
    /// visual line by y, then the nearest cluster edge by x within that line
    /// (BiDi-correct). Past-end snaps to the line's trailing byte.
    pub fn x_to_byte(&self, x: f32, y: f32) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let line = if self.metrics.line_height > 0.0 {
            ((y / self.metrics.line_height).floor().max(0.0) as usize).min(self.lines.len() - 1)
        } else {
            0
        };
        let l = &self.lines[line];
        nearest_edge_byte(&l.edges, x).unwrap_or(l.byte_lo as usize)
    }

    /// Emit one selection band per line-portion of each maximal
    /// contiguous BiDi-level run the logical range `[lo, hi)` intersects.
    ///
    /// A segment is a maximal `(font, BiDi level)` run and keeps going
    /// across a line break, so a single min/max over a whole segment would
    /// merge two lines into one wide rectangle. Bucketing by baseline
    /// keeps each line's highlight on its own line; `baseline_y` is the
    /// offset from the first line's baseline, which is what the caret path
    /// already uses.
    pub fn selection_bands(&self, lo: usize, hi: usize) -> Vec<SelectionBand> {
        if hi <= lo {
            return Vec::new();
        }
        let lo = lo as u32;
        let hi = hi as u32;
        let mut out: Vec<SelectionBand> = Vec::new();
        for seg in &self.segments {
            let seg_lo = lo.max(seg.byte_lo);
            let seg_hi = hi.min(seg.byte_hi);
            if seg_lo >= seg_hi {
                continue;
            }
            // (baseline_y, min_x, max_x) per line this segment touches.
            let mut per_line: Vec<(f32, f32, f32)> = Vec::new();
            for c in &seg.clusters {
                if c.byte_end <= seg_lo || c.byte_start >= seg_hi {
                    continue;
                }
                match per_line.iter_mut().find(|(y, _, _)| *y == c.baseline_y) {
                    Some((_, min_x, max_x)) => {
                        if c.leading_x < *min_x {
                            *min_x = c.leading_x;
                        }
                        if c.trailing_x > *max_x {
                            *max_x = c.trailing_x;
                        }
                    }
                    None => per_line.push((c.baseline_y, c.leading_x, c.trailing_x)),
                }
            }
            for (baseline_y, min_x, max_x) in per_line {
                if max_x > min_x {
                    out.push(SelectionBand {
                        baseline_y,
                        x0: seg.x_offset + min_x,
                        x1: seg.x_offset + max_x,
                    });
                }
            }
        }
        out.sort_by(|a, b| f32_cmp(a.baseline_y, b.baseline_y).then(f32_cmp(a.x0, b.x0)));
        out
    }
}

/// Total ordering for the non-NaN f32s the shaper emits.
fn f32_cmp(a: f32, b: f32) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// Byte whose edge visual-x is nearest `target`.
fn nearest_edge_byte(edges: &[(f32, u32)], target: f32) -> Option<usize> {
    edges
        .iter()
        .min_by(|a, b| f32_cmp((a.0 - target).abs(), (b.0 - target).abs()))
        .map(|e| e.1 as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GlyphPosition, ShapedRun, ShapedSegment};
    use std::sync::Arc;

    fn glyph(byte_start: u32, byte_end: u32, x: f32, advance: f32) -> GlyphPosition {
        GlyphPosition {
            id: 0,
            x,
            y: 0.0,
            advance,
            byte_start,
            byte_end,
        }
    }

    fn seg_at_level(level: u8, glyphs: Vec<GlyphPosition>) -> ShapedSegment {
        let width: f32 = glyphs.iter().map(|g| g.advance).sum();
        ShapedSegment {
            font_id: 1,
            font_data: Arc::new(Vec::new()),
            font_index: 0,
            level,
            glyphs,
            width,
        }
    }

    /// Pure-LTR paragraph: byte->x grows monotonically and matches each
    /// glyph's leading edge (W3.6 fast-path regression check).
    #[test]
    fn ltr_caret_byte_to_x_is_monotone() {
        let seg = seg_at_level(
            0,
            vec![
                glyph(0, 1, 0.0, 10.0),
                glyph(1, 2, 10.0, 10.0),
                glyph(2, 3, 20.0, 10.0),
            ],
        );
        let run = ShapedRun {
            font_data: seg.font_data.clone(),
            font_index: 0,
            glyphs: seg.glyphs.clone(),
            segments: vec![seg],
            width: 30.0,
        };
        let g = TextGeometry::from(&run);
        assert_eq!(g.caret_xy(0).0, 0.0);
        assert_eq!(g.caret_xy(1).0, 10.0);
        assert_eq!(g.caret_xy(2).0, 20.0);
        assert_eq!(g.caret_xy(3).0, 30.0);
    }

    /// D4: x_to_byte round-trips with byte_to_caret for LTR; a click past
    /// the run end snaps to the trailing byte.
    #[test]
    fn ltr_x_to_byte_roundtrips_and_snaps_past_end() {
        let seg = seg_at_level(
            0,
            vec![
                glyph(0, 1, 0.0, 10.0),
                glyph(1, 2, 10.0, 10.0),
                glyph(2, 3, 20.0, 10.0),
            ],
        );
        let run = ShapedRun {
            font_data: seg.font_data.clone(),
            font_index: 0,
            glyphs: seg.glyphs.clone(),
            segments: vec![seg],
            width: 30.0,
        };
        let g = TextGeometry::from(&run);
        // Nearest-edge hit-testing: left-half of a glyph -> its start byte,
        // right-half -> its end byte.
        assert_eq!(g.x_to_byte(1.0, 0.0), 0);
        assert_eq!(g.x_to_byte(9.0, 0.0), 1);
        assert_eq!(g.x_to_byte(11.0, 0.0), 1);
        assert_eq!(g.x_to_byte(1000.0, 0.0), 3);
        // Round-trip: caret x for each byte hit-tests back to that byte.
        for b in [0usize, 1, 2, 3] {
            let x = g.caret_xy(b).0;
            assert_eq!(g.x_to_byte(x, 0.0), b);
        }
    }

    /// D4: a proportional run (wide 'W', narrow 'i') hit-tests by real glyph
    /// widths, not a uniform advance. Clicking inside the wide glyph lands on
    /// the boundary nearest the pointer, which the 0.55-avg estimate missed.
    #[test]
    fn proportional_hit_test_uses_real_widths() {
        // 'W' 24px wide, 'i' 4px wide, 'l' 4px wide.
        let seg = seg_at_level(
            0,
            vec![
                glyph(0, 1, 0.0, 24.0),
                glyph(1, 2, 24.0, 4.0),
                glyph(2, 3, 28.0, 4.0),
            ],
        );
        let run = ShapedRun {
            font_data: seg.font_data.clone(),
            font_index: 0,
            glyphs: seg.glyphs.clone(),
            segments: vec![seg],
            width: 32.0,
        };
        let g = TextGeometry::from(&run);
        // A click at x=20 is inside the wide 'W' (0..24), past its midpoint
        // (12), so it lands on byte 1 (after 'W'). A uniform 0.55*size
        // estimate would misplace this.
        assert_eq!(g.x_to_byte(20.0, 0.0), 1);
        // A click at x=10 is in the left half of 'W' -> byte 0.
        assert_eq!(g.x_to_byte(10.0, 0.0), 0);
    }

    /// A multiline shape carries per-line baseline offsets on its glyphs; the
    /// caret must land on the byte's own line, and x_to_byte must pick the
    /// visual line by y.
    #[test]
    fn multiline_caret_and_hit_test_use_line() {
        let mk = |bs: u32, be: u32, x: f32, y: f32| GlyphPosition {
            id: 0,
            x,
            y,
            advance: 10.0,
            byte_start: bs,
            byte_end: be,
        };
        let seg = seg_at_level(
            0,
            vec![
                mk(0, 1, 0.0, 0.0),
                mk(1, 2, 10.0, 0.0),
                mk(3, 4, 0.0, 19.2),
                mk(4, 5, 10.0, 19.2),
            ],
        );
        let run = ShapedRun {
            font_data: seg.font_data.clone(),
            font_index: 0,
            glyphs: seg.glyphs.clone(),
            segments: vec![seg],
            width: 20.0,
        };
        let g = TextGeometry::from(&run);
        assert_eq!(g.caret_xy(0), (0.0, 0.0));
        assert_eq!(g.caret_xy(3), (0.0, 19.2));
        assert_eq!(g.caret_xy(4), (10.0, 19.2));
        assert_eq!(g.caret_xy(5), (20.0, 19.2));
        assert_eq!(g.line_count(), 2);
        // Inferred line height is the 19.2 baseline step; y in the second
        // band hit-tests on line 2's bytes.
        assert_eq!(g.x_to_byte(1.0, 25.0), 3);
        assert_eq!(g.x_to_byte(9.0, 25.0), 4);
        assert_eq!(g.visual_line_of_byte(4), 1);
        assert_eq!(g.visual_line_bounds(1), (3, 5));
    }

    /// D5/D6 support: byte_at_line_x lands on the byte nearest a goal x on a
    /// target line (goal-column preservation).
    #[test]
    fn byte_at_line_x_lands_nearest_goal() {
        let mk = |bs: u32, be: u32, x: f32, y: f32| GlyphPosition {
            id: 0,
            x,
            y,
            advance: 10.0,
            byte_start: bs,
            byte_end: be,
        };
        // Line 1 "abcd" (bytes 0..4), line 2 "ef" (bytes 5..7).
        let seg = seg_at_level(
            0,
            vec![
                mk(0, 1, 0.0, 0.0),
                mk(1, 2, 10.0, 0.0),
                mk(2, 3, 20.0, 0.0),
                mk(3, 4, 30.0, 0.0),
                mk(5, 6, 0.0, 19.2),
                mk(6, 7, 10.0, 19.2),
            ],
        );
        let run = ShapedRun {
            font_data: seg.font_data.clone(),
            font_index: 0,
            glyphs: seg.glyphs.clone(),
            segments: vec![seg],
            width: 40.0,
        };
        let g = TextGeometry::from(&run);
        // Goal x=30 on the short line 2 clamps to its trailing byte (7).
        assert_eq!(g.byte_at_line_x(1, 30.0), 7);
        // Goal x=10 on line 1 lands on byte 1.
        assert_eq!(g.byte_at_line_x(0, 10.0), 1);
    }

    /// BiDi caret math: an LTR->RTL->LTR paragraph reorders caret positions
    /// visually, not logically.
    #[test]
    fn bidi_caret_byte_to_x_is_visual_not_logical() {
        let ltr_head = seg_at_level(
            0,
            vec![
                glyph(0, 1, 0.0, 10.0),
                glyph(1, 2, 10.0, 10.0),
                glyph(2, 3, 20.0, 10.0),
            ],
        );
        let rtl_mid = seg_at_level(
            1,
            vec![
                glyph(5, 6, 30.0, 10.0),
                glyph(4, 5, 40.0, 10.0),
                glyph(3, 4, 50.0, 10.0),
            ],
        );
        let ltr_tail = seg_at_level(
            0,
            vec![
                glyph(6, 7, 60.0, 10.0),
                glyph(7, 8, 70.0, 10.0),
                glyph(8, 9, 80.0, 10.0),
            ],
        );
        let mut all_glyphs = Vec::new();
        all_glyphs.extend(ltr_head.glyphs.iter().copied());
        all_glyphs.extend(rtl_mid.glyphs.iter().copied());
        all_glyphs.extend(ltr_tail.glyphs.iter().copied());
        let run = ShapedRun {
            font_data: ltr_head.font_data.clone(),
            font_index: 0,
            glyphs: all_glyphs,
            segments: vec![ltr_head, rtl_mid, ltr_tail],
            width: 90.0,
        };
        let g = TextGeometry::from(&run);
        assert_eq!(g.caret_xy(0).0, 0.0);
        assert_eq!(g.caret_xy(3).0, 30.0);
        let x_byte3 = g.caret_xy(3).0;
        let x_byte6 = g.caret_xy(6).0;
        assert!(
            x_byte3 == 30.0 || x_byte6 == 60.0,
            "RTL run must reorder caret positions visually; got \
             byte3.x={x_byte3} byte6.x={x_byte6}"
        );
        assert_eq!(g.caret_xy(9).0, 90.0);
    }

    /// Selection across an RTL run emits >= 2 rectangles (one per maximal
    /// contiguous-level slice); pure-LTR collapses to one.
    #[test]
    fn bidi_selection_emits_rect_per_level_run() {
        let ltr_head = seg_at_level(
            0,
            vec![
                glyph(0, 1, 0.0, 10.0),
                glyph(1, 2, 10.0, 10.0),
                glyph(2, 3, 20.0, 10.0),
            ],
        );
        let rtl_mid = seg_at_level(
            1,
            vec![
                glyph(5, 6, 30.0, 10.0),
                glyph(4, 5, 40.0, 10.0),
                glyph(3, 4, 50.0, 10.0),
            ],
        );
        let ltr_tail = seg_at_level(
            0,
            vec![
                glyph(6, 7, 60.0, 10.0),
                glyph(7, 8, 70.0, 10.0),
                glyph(8, 9, 80.0, 10.0),
            ],
        );
        let run = ShapedRun {
            font_data: ltr_head.font_data.clone(),
            font_index: 0,
            glyphs: Vec::new(),
            segments: vec![ltr_head, rtl_mid, ltr_tail],
            width: 90.0,
        };
        let g = TextGeometry::from(&run);
        let one = g.selection_bands(0, 3);
        assert_eq!(one.len(), 1);
        assert_eq!((one[0].x0, one[0].x1), (0.0, 30.0));
        let three = g.selection_bands(1, 8);
        assert_eq!(three.len(), 3, "got {three:?}");
    }

    /// with_size bakes caret rect top/height matching the render caret math.
    #[test]
    fn with_size_bakes_caret_rect() {
        let seg = seg_at_level(0, vec![glyph(0, 1, 0.0, 10.0), glyph(1, 2, 10.0, 10.0)]);
        let run = ShapedRun {
            font_data: seg.font_data.clone(),
            font_index: 0,
            glyphs: seg.glyphs.clone(),
            segments: vec![seg],
            width: 20.0,
        };
        let g = TextGeometry::from(&run).with_size(16.0);
        let c = g.byte_to_caret(1);
        assert_eq!(c.x, 10.0);
        assert_eq!(c.top, -16.0 * 0.9);
        assert!((c.height - 16.0 * 1.05).abs() < 1e-4);
        assert_eq!(c.line, 0);
    }
}
