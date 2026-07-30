use super::*;

/// Margin kept between the caret and the field edge while scrolling the
/// content to keep the caret visible (logical pixels).
const CARET_VISIBLE_MARGIN: f32 = 8.0;

/// Pure caret-keep-visible math for one axis.
///
/// Returns the content offset that keeps the caret span
/// `[caret_lo, caret_hi]` inside the `inner`-sized window with `margin`
/// slack, moving `current` as little as possible (Qt line-edit
/// behavior: the view only scrolls when the caret would leave it).
/// `content` is the full content extent on this axis; when it fits the
/// window the offset is always `0`.
fn caret_scroll_axis(
    caret_lo: f32,
    caret_hi: f32,
    inner: f32,
    margin: f32,
    current: f32,
    content: f32,
) -> f32 {
    if content <= inner {
        return 0.0;
    }
    let max_off = content - inner;
    // Never let the margin eat the whole window on tiny fields.
    let m = margin.min((inner / 4.0).max(0.0));
    let mut off = current.clamp(0.0, max_off);
    if caret_hi - off > inner - m {
        off = caret_hi - (inner - m);
    }
    if caret_lo - off < m {
        off = caret_lo - m;
    }
    off.clamp(0.0, max_off)
}

/// Caret-keep-visible (W2 Qt-polish item 6): maintain
/// [`lumen_core::components::TextInputScroll`] on the focused input so
/// the caret stays inside the field box with a small margin. The
/// extractor subtracts the offset from the emitted run origin, shifting
/// glyphs + caret + selection together; `<input>`/`<textarea>` boxes
/// clip via their UA-default `overflow: hidden`.
///
/// Horizontal: the caret x is the measured width of the caret line's
/// prefix (same cosmic shaper the layout measure uses, so it agrees
/// with the renderer's glyph positions). Vertical (multiline): logical
/// line index x the shaper's line height (`size_px * 1.2`).
#[allow(clippy::type_complexity)]
pub(crate) fn scroll_caret_into_view(
    mut commands: Commands,
    shaper: Option<NonSendMut<lumen_text_cosmic::CosmicShaper>>,
    mut q: Query<
        (
            Entity,
            &lumen_core::components::Transform,
            &lumen_core::components::TextContent,
            &lumen_core::components::TextInput,
            Option<&lumen_core::components::TextStyle>,
            Option<&lumen_core::components::Style>,
            Option<&mut lumen_core::components::TextInputScroll>,
        ),
        bevy_ecs::prelude::With<lumen_core::input::Focused>,
    >,
) {
    use lumen_text::WrapMode;
    let Some(mut shaper) = shaper else {
        return;
    };
    for (e, t, tc, ti, ts, style, scroll) in &mut q {
        let size_px = ts.map(|s| s.size_px).unwrap_or(16.0);
        let (pad_l, pad_r, pad_t, pad_b) = style
            .map(|s| {
                (
                    s.padding.left,
                    s.padding.right,
                    s.padding.top,
                    s.padding.bottom,
                )
            })
            .unwrap_or((0.0, 0.0, 0.0, 0.0));
        let inner_w = (t.size.x - pad_l - pad_r).max(1.0);
        // Clamp the byte cursor to a char boundary before slicing.
        let mut cur = ti.cursor.min(tc.0.len());
        while cur > 0 && !tc.0.is_char_boundary(cur) {
            cur -= 1;
        }
        let line_start = tc.0[..cur].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = tc.0[cur..]
            .find('\n')
            .map(|i| cur + i)
            .unwrap_or(tc.0.len());
        let caret_x = shaper
            .measure(&tc.0[line_start..cur], size_px, None, WrapMode::None, None)
            .0;
        let line_w = shaper
            .measure(
                &tc.0[line_start..line_end],
                size_px,
                None,
                WrapMode::None,
                None,
            )
            .0;
        const CARET_W: f32 = 2.0;
        let off_x = caret_scroll_axis(
            caret_x,
            caret_x + CARET_W,
            inner_w,
            CARET_VISIBLE_MARGIN,
            scroll.as_ref().map(|s| s.offset.x).unwrap_or(0.0),
            line_w + CARET_W,
        );
        let off_y = if ti.multiline {
            let line_h = size_px * 1.2;
            let inner_h = (t.size.y - pad_t - pad_b).max(1.0);
            let line_idx = tc.0[..cur].matches('\n').count() as f32;
            let total_h = (tc.0.matches('\n').count() + 1) as f32 * line_h;
            caret_scroll_axis(
                line_idx * line_h,
                (line_idx + 1.0) * line_h,
                inner_h,
                0.0,
                scroll.as_ref().map(|s| s.offset.y).unwrap_or(0.0),
                total_h,
            )
        } else {
            0.0
        };
        let next = glam::Vec2::new(off_x, off_y);
        match scroll {
            Some(mut s) => {
                // Sub-half-pixel churn is invisible; skipping the write
                // keeps Changed<TextInputScroll> (and FrameDirty) quiet.
                if (s.offset - next).length_squared() > 0.25 {
                    s.offset = next;
                }
            }
            None => {
                if next != glam::Vec2::ZERO {
                    commands
                        .entity(e)
                        .insert(lumen_core::components::TextInputScroll { offset: next });
                }
            }
        }
    }
}

#[cfg(test)]
mod caret_scroll_tests {
    use super::caret_scroll_axis;

    const CARET_W: f32 = 2.0;

    #[test]
    fn content_that_fits_never_scrolls() {
        // 50px of text in a 200px field: offset pinned to 0 whatever
        // the caret / current offset say.
        assert_eq!(caret_scroll_axis(50.0, 52.0, 200.0, 8.0, 40.0, 52.0), 0.0);
        assert_eq!(caret_scroll_axis(0.0, 2.0, 200.0, 8.0, 40.0, 52.0), 0.0);
    }

    #[test]
    fn caret_past_right_edge_scrolls_right_with_margin() {
        // 400px of text, 200px window, caret at 300.
        let off = caret_scroll_axis(300.0, 300.0 + CARET_W, 200.0, 8.0, 0.0, 400.0);
        // Caret must sit inside [margin, inner - margin] of the window.
        let local = 300.0 + CARET_W - off;
        assert!(local <= 200.0 - 8.0, "caret inside the right margin");
        assert!(300.0 - off >= 8.0, "caret inside the left margin");
    }

    #[test]
    fn caret_at_end_pins_to_max_offset() {
        let off = caret_scroll_axis(400.0, 400.0 + CARET_W, 200.0, 8.0, 0.0, 400.0 + CARET_W);
        assert_eq!(
            off,
            400.0 + CARET_W - 200.0,
            "offset clamped to content end"
        );
    }

    #[test]
    fn caret_before_left_edge_scrolls_left() {
        // Scrolled to 250, caret back at 100 -> view follows leftward.
        let off = caret_scroll_axis(100.0, 100.0 + CARET_W, 200.0, 8.0, 250.0, 400.0);
        assert_eq!(off, 92.0, "caret - margin");
    }

    #[test]
    fn caret_inside_view_does_not_move_the_view() {
        // Qt behavior: no scrolling while the caret stays inside.
        let off = caret_scroll_axis(150.0, 150.0 + CARET_W, 200.0, 8.0, 100.0, 400.0);
        assert_eq!(off, 100.0);
    }

    #[test]
    fn vertical_line_span_scrolls_by_whole_lines() {
        // 5 lines of 19.2px in a 40px window; caret on line 4
        // (span 57.6..76.8) -> offset brings the line fully into view.
        let off = caret_scroll_axis(57.6, 76.8, 40.0, 0.0, 0.0, 96.0);
        assert!((off - (76.8 - 40.0)).abs() < 1e-4);
        // Moving back to line 0 scrolls back to the top.
        let off = caret_scroll_axis(0.0, 19.2, 40.0, 0.0, off, 96.0);
        assert_eq!(off, 0.0);
    }
}
