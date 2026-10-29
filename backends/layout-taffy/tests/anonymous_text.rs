//! An element's own text beside element children lays out in an anonymous
//! box placed first among them, as CSS does, and paints there.
//!
//! taffy measures leaves only, so before the anonymous box an element that
//! gained a child was sized by the child alone and its text drew past its
//! own edge, over whatever came next.

use bevy_ecs::hierarchy::ChildOf;
use glam::Vec2;
use lumen_core::components::*;
use lumen_core::prelude::*;
use lumen_core::render_world::{ExtractedText, extract_text};
use lumen_layout_taffy::{LayoutResource, TaffyLayoutPlugin};
use lumen_text::{ShapeOptions, ShapedRun, ShaperService, TextShaper, WrapMode};

/// A fixed 10 px advance per char, wrapping on spaces, so a test knows
/// every width exactly.
struct GridShaper;

impl TextShaper for GridShaper {
    fn shape(&mut self, _text: &str, _size_px: f32, _opts: ShapeOptions) -> Option<ShapedRun> {
        None
    }

    fn measure_with_baseline(
        &mut self,
        text: &str,
        size_px: f32,
        opts: &ShapeOptions,
    ) -> (f32, f32, f32) {
        let line_h = opts.resolved_line_height(size_px);
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
        (widest, lines as f32 * line_h, line_h * 0.8)
    }
}

const LINE_H: f32 = 20.0;

fn app() -> App {
    let mut app = App::new();
    app.world.insert_non_send(ShaperService::new(GridShaper));
    app.add_plugin(TaffyLayoutPlugin);
    app.world
        .get_non_send_mut::<LayoutResource>()
        .unwrap()
        .set_viewport(800.0, 600.0);
    app
}

fn label(text: &str) -> (TextContent, TextStyle) {
    (
        TextContent(text.into()),
        TextStyle {
            line_height: Some(LineHeightSpec::Px(LINE_H)),
            ..TextStyle::default()
        },
    )
}

/// A button-like element: 4 px of padding, a 6 px gap, its own text.
fn button_style(direction: FlexDirection) -> Style {
    Style {
        flex_direction: direction,
        padding: Edges {
            left: 4.0,
            right: 4.0,
            top: 4.0,
            bottom: 4.0,
            ..Edges::default()
        },
        gap: Gap {
            row: 6.0,
            column: 6.0,
            ..Gap::default()
        },
        ..Style::default()
    }
}

fn icon_style() -> Style {
    Style {
        width: Length::Px(16.0),
        height: Length::Px(16.0),
        ..Style::default()
    }
}

/// `<row align="start"><button text="Save"><column 16x16/></button><button text="Open"/></row>`.
struct Strip {
    app: App,
    save: Entity,
    icon: Entity,
    open: Entity,
}

fn strip(direction: FlexDirection) -> Strip {
    let mut app = app();
    let row = app
        .world
        .spawn(Style {
            align: FlexAlign::Start,
            ..Style::default()
        })
        .id();
    let save = app
        .world
        .spawn((button_style(direction), label("Save"), ChildOf(row)))
        .id();
    let icon = app.world.spawn((icon_style(), ChildOf(save))).id();
    let open = app
        .world
        .spawn((button_style(direction), label("Open"), ChildOf(row)))
        .id();
    app.tick();
    Strip {
        app,
        save,
        icon,
        open,
    }
}

fn tf(app: &App, e: Entity) -> Transform {
    *app.world.get::<Transform>(e).unwrap()
}

#[test]
fn text_beside_a_child_takes_room_before_it() {
    let s = strip(FlexDirection::Row);
    let save = tf(&s.app, s.save);
    let icon = tf(&s.app, s.icon);
    let open = tf(&s.app, s.open);

    // padding + "Save" (40) + gap + icon (16) + padding.
    assert_eq!(save.size.x, 4.0 + 40.0 + 6.0 + 16.0 + 4.0);
    // The text box comes first, the gap after it, then the icon.
    let text = *s.app.world.get::<AnonymousTextBox>(s.save).unwrap();
    assert_eq!(text.offset, Vec2::new(4.0, 4.0));
    assert_eq!(text.size.x, 40.0);
    assert_eq!(icon.absolute.x - save.absolute.x, 4.0 + 40.0 + 6.0);
    // The next button starts where this one ends, so nothing overlaps.
    assert_eq!(open.absolute.x, save.absolute.x + save.size.x);
    // A leaf keeps its text in its content box, with no anonymous box.
    assert_eq!(open.size.x, 4.0 + 40.0 + 4.0);
    assert!(s.app.world.get::<AnonymousTextBox>(s.open).is_none());

    // The box is not an element: the children list holds the icon alone.
    let kids: Vec<Entity> = s
        .app
        .world
        .get::<Children>(s.save)
        .unwrap()
        .iter()
        .collect();
    assert_eq!(kids, vec![s.icon]);
}

#[test]
fn column_places_the_text_box_above_the_child() {
    let s = strip(FlexDirection::Column);
    let save = tf(&s.app, s.save);
    let icon = tf(&s.app, s.icon);
    let text = *s.app.world.get::<AnonymousTextBox>(s.save).unwrap();
    assert_eq!(text.offset, Vec2::new(4.0, 4.0));
    assert_eq!(text.size.y, LINE_H);
    assert_eq!(icon.absolute.y - save.absolute.y, 4.0 + LINE_H + 6.0);
    assert_eq!(save.size.y, 4.0 + LINE_H + 6.0 + 16.0 + 4.0);
    // Stretched across the column, wide enough for the text.
    assert_eq!(save.size.x, 4.0 + 40.0 + 4.0);
}

#[test]
fn the_text_paints_inside_its_box() {
    let mut s = strip(FlexDirection::Row);
    let save = tf(&s.app, s.save);
    let icon = tf(&s.app, s.icon);
    let app = &mut s.app;
    extract_text(&mut app.world, &mut app.render_world);
    let runs: Vec<ExtractedText> = app
        .render_world
        .query::<&ExtractedText>()
        .iter(&app.render_world)
        .cloned()
        .collect();
    let run = runs.iter().find(|r| r.text == "Save").unwrap();
    assert_eq!(run.origin.x, save.absolute.x + 4.0);
    assert_eq!(run.container_width, 40.0);
    // The run ends before the icon starts.
    assert!(run.origin.x + run.container_width <= icon.absolute.x);
    // Its baseline sits in the box's line.
    assert!(run.origin.y > save.absolute.y + 4.0);
    assert!(run.origin.y < save.absolute.y + 4.0 + LINE_H);
}

#[test]
fn text_edits_resize_the_box() {
    let mut s = strip(FlexDirection::Row);
    s.app.world.get_mut::<TextContent>(s.save).unwrap().0 = "Saved".into();
    s.app.tick();
    assert_eq!(tf(&s.app, s.save).size.x, 4.0 + 50.0 + 6.0 + 16.0 + 4.0);
    let text = *s.app.world.get::<AnonymousTextBox>(s.save).unwrap();
    assert_eq!(text.size.x, 50.0);
}

#[test]
fn losing_the_child_makes_the_element_a_leaf_again() {
    let mut s = strip(FlexDirection::Row);
    s.app.world.despawn(s.icon);
    s.app.tick();
    assert_eq!(tf(&s.app, s.save).size.x, 4.0 + 40.0 + 4.0);
    assert!(s.app.world.get::<AnonymousTextBox>(s.save).is_none());
    assert!(
        !s.app
            .world
            .get_non_send::<LayoutResource>()
            .unwrap()
            .has_anonymous_text_box(s.save)
    );

    // And gaining one back brings the box back.
    s.app.world.spawn((icon_style(), ChildOf(s.save)));
    s.app.tick();
    assert_eq!(tf(&s.app, s.save).size.x, 4.0 + 40.0 + 6.0 + 16.0 + 4.0);
    assert!(s.app.world.get::<AnonymousTextBox>(s.save).is_some());
}

#[test]
fn clearing_the_text_drops_the_box() {
    let mut s = strip(FlexDirection::Row);
    s.app.world.get_mut::<TextContent>(s.save).unwrap().0 = String::new();
    s.app.tick();
    assert_eq!(tf(&s.app, s.save).size.x, 4.0 + 16.0 + 4.0);
    assert!(s.app.world.get::<AnonymousTextBox>(s.save).is_none());
}

#[test]
fn white_space_alone_beside_a_child_makes_no_box() {
    let mut s = strip(FlexDirection::Row);
    s.app.world.get_mut::<TextContent>(s.save).unwrap().0 = " \n\t".into();
    s.app.tick();
    assert_eq!(tf(&s.app, s.save).size.x, 4.0 + 16.0 + 4.0);
    assert_eq!(
        tf(&s.app, s.icon).absolute.x - tf(&s.app, s.save).absolute.x,
        4.0
    );
    assert!(s.app.world.get::<AnonymousTextBox>(s.save).is_none());
}

#[test]
fn a_text_field_keeps_its_text_in_its_own_box() {
    let mut app = app();
    let field = app
        .world
        .spawn((
            button_style(FlexDirection::Row),
            label("Save"),
            TextInput::default(),
        ))
        .id();
    app.world.spawn((icon_style(), ChildOf(field)));
    app.tick();
    assert!(app.world.get::<AnonymousTextBox>(field).is_none());
    assert!(
        !app.world
            .get_non_send::<LayoutResource>()
            .unwrap()
            .has_anonymous_text_box(field)
    );
}

/// Stretched across a column, the box is as wide as the element's content
/// and `text-align` places the lines inside it.
#[test]
fn text_align_places_the_lines_inside_a_stretched_box() {
    let mut app = app();
    let card = app
        .world
        .spawn((
            Style {
                width: Length::Px(200.0),
                ..button_style(FlexDirection::Column)
            },
            TextContent("Title".into()),
            TextStyle {
                align: TextAlign::Center,
                line_height: Some(LineHeightSpec::Px(LINE_H)),
                ..TextStyle::default()
            },
        ))
        .id();
    app.world.spawn((icon_style(), ChildOf(card)));
    app.tick();
    let text = *app.world.get::<AnonymousTextBox>(card).unwrap();
    assert_eq!(text.size.x, 200.0 - 8.0);
    extract_text(&mut app.world, &mut app.render_world);
    let run = app
        .render_world
        .query::<&ExtractedText>()
        .iter(&app.render_world)
        .find(|r| r.text == "Title")
        .cloned()
        .unwrap();
    assert_eq!(run.container_width, 192.0);
    assert_eq!(run.align, TextAlign::Center);
}
