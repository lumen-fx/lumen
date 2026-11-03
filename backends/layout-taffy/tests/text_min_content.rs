//! Wrapping text's min-content width is its longest word.
//!
//! A flex item's automatic minimum size is its min-content size (CSS
//! flexbox section 4.5), and the min-content width of text that may wrap
//! is its widest unbreakable piece: the longest word. So a growing label
//! beside a button on a narrow row shrinks to what the button leaves and
//! wraps, rather than keeping its one-line width and pushing the button
//! off the row. The same holds when the label sits inside a growing
//! column, whose min-content width is its content's.

use bevy_ecs::hierarchy::ChildOf;
use lumen_core::components::*;
use lumen_core::prelude::*;
use lumen_layout_taffy::{LayoutResource, TaffyLayoutPlugin};
use lumen_text::{ShapeOptions, ShapedRun, ShaperService, TextShaper, WrapMode};

/// A fixed 10 px advance per char, wrapping on spaces, so a test knows
/// every width exactly. Like the real shapers, `measure_with_baseline`
/// reports at most the width it was given, while `shape` reports the
/// widest line it laid out.
struct GridShaper;

impl GridShaper {
    /// `(widest line, line count)` for `text` wrapped at `opts.width`.
    fn lines(text: &str, opts: &ShapeOptions) -> (f32, u32) {
        let cap = match (opts.wrap, opts.width) {
            (WrapMode::None, _) | (_, None) => f32::INFINITY,
            (_, Some(w)) => w,
        };
        let (mut lines, mut cur, mut widest) = (1u32, 0.0f32, 0.0f32);
        for word in text.split(' ') {
            let w = word.chars().count() as f32 * 10.0;
            let next = if cur == 0.0 { w } else { cur + 10.0 + w };
            if next > cap && cur > 0.0 {
                lines += 1;
                cur = w;
            } else {
                cur = next;
            }
            widest = widest.max(cur);
        }
        (widest, lines)
    }
}

impl TextShaper for GridShaper {
    fn shape(&mut self, text: &str, _size_px: f32, opts: ShapeOptions) -> Option<ShapedRun> {
        let (width, _) = Self::lines(text, &opts);
        Some(ShapedRun {
            font_data: Default::default(),
            font_index: 0,
            glyphs: Vec::new(),
            segments: Vec::new(),
            width,
        })
    }

    fn measure_with_baseline(
        &mut self,
        text: &str,
        size_px: f32,
        opts: &ShapeOptions,
    ) -> (f32, f32, f32) {
        let line_h = opts.resolved_line_height(size_px);
        let (widest, lines) = Self::lines(text, opts);
        let width = opts.width.map_or(widest, |w| widest.min(w));
        (width, lines as f32 * line_h, line_h * 0.8)
    }
}

const LINE_H: f32 = 20.0;

/// One 100 px word, then thirty 20 px words: 1000 px on one line.
fn sentence() -> String {
    let mut s = String::from("aaaaaaaaaa");
    for _ in 0..30 {
        s.push_str(" bb");
    }
    s
}

fn app() -> App {
    let mut app = App::new();
    app.world.insert_non_send(ShaperService::new(GridShaper));
    app.add_plugin(TaffyLayoutPlugin);
    app.world
        .get_non_send_mut::<LayoutResource>()
        .unwrap()
        .set_viewport(1200.0, 600.0);
    app
}

fn text(s: &str, wrap: TextWrap) -> (TextContent, TextStyle) {
    (
        TextContent(s.into()),
        TextStyle {
            wrap,
            line_height: Some(LineHeightSpec::Px(LINE_H)),
            ..TextStyle::default()
        },
    )
}

fn tf(app: &App, e: Entity) -> Transform {
    *app.world.get::<Transform>(e).unwrap()
}

/// Laid-out pieces of `<row width=row_w><label grow wrap/><button/></row>`,
/// with the label wrapped in a `<column grow>` when `in_column`.
struct Shape {
    app: App,
    item: Entity,
    label: Entity,
    button: Entity,
}

fn lay_out(row_w: f32, in_column: bool) -> Shape {
    let mut app = app();
    let row = app
        .world
        .spawn(Style {
            width: Length::Px(row_w),
            align: FlexAlign::Start,
            ..Style::default()
        })
        .id();
    let grow = Style {
        grow: 1.0,
        ..Style::default()
    };
    let (item, label) = if in_column {
        let column = app
            .world
            .spawn((
                Style {
                    flex_direction: FlexDirection::Column,
                    ..grow.clone()
                },
                ChildOf(row),
            ))
            .id();
        let label = app
            .world
            .spawn((
                Style::default(),
                text(&sentence(), TextWrap::Word),
                ChildOf(column),
            ))
            .id();
        (column, label)
    } else {
        let label = app
            .world
            .spawn((grow, text(&sentence(), TextWrap::Word), ChildOf(row)))
            .id();
        (label, label)
    };
    let button = app
        .world
        .spawn((Style::default(), text("Add", TextWrap::None), ChildOf(row)))
        .id();
    app.tick();
    Shape {
        app,
        item,
        label,
        button,
    }
}

/// The issue's shape: the label shrinks to what the 30 px button leaves on
/// a 380 px row and wraps there; the button stays on the row.
#[test]
fn a_wrapping_label_shrinks_beside_a_button() {
    let s = lay_out(380.0, false);
    let label = tf(&s.app, s.label);
    let button = tf(&s.app, s.button);
    assert_eq!(
        label.size.x, 350.0,
        "the label takes what the button leaves"
    );
    assert_eq!(button.absolute.x, 350.0, "the button stays on the row");
    assert_eq!(button.size.x, 30.0);
    // At 350 px the long word and eight short ones fill the first line,
    // twelve the second, and the last ten the third.
    assert_eq!(label.size.y, 3.0 * LINE_H, "the label wraps at its width");
}

/// The same through a growing column: the column's automatic minimum is
/// its label's min-content width, not the label's one-line width.
#[test]
fn a_wrapping_label_in_a_growing_column_shrinks_beside_a_button() {
    let s = lay_out(380.0, true);
    let column = tf(&s.app, s.item);
    let label = tf(&s.app, s.label);
    let button = tf(&s.app, s.button);
    assert_eq!(
        column.size.x, 350.0,
        "the column takes what the button leaves"
    );
    assert_eq!(label.size.x, 350.0);
    assert_eq!(label.size.y, 3.0 * LINE_H);
    assert_eq!(button.absolute.x, 350.0, "the button stays on the row");
}

/// The floor is the longest word, not zero: on a row too narrow for it the
/// label stops at 100 px and the overflow goes past the row's end.
#[test]
fn a_wrapping_label_stops_at_its_longest_word() {
    for in_column in [false, true] {
        let s = lay_out(100.0, in_column);
        let item = tf(&s.app, s.item);
        let button = tf(&s.app, s.button);
        assert_eq!(item.size.x, 100.0, "in_column={in_column}");
        assert_eq!(button.absolute.x, 100.0, "in_column={in_column}");
    }
}

/// Text that does not wrap keeps its one-line width as its floor.
#[test]
fn a_nowrap_label_keeps_its_line() {
    let mut app = app();
    let row = app
        .world
        .spawn(Style {
            width: Length::Px(380.0),
            align: FlexAlign::Start,
            ..Style::default()
        })
        .id();
    let label = app
        .world
        .spawn((
            Style {
                grow: 1.0,
                ..Style::default()
            },
            text(&sentence(), TextWrap::None),
            ChildOf(row),
        ))
        .id();
    app.tick();
    assert_eq!(tf(&app, label).size.x, 1000.0);
}
