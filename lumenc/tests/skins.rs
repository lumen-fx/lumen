// Names `lumenc::spawn` / `lumenc::skins`, which lumenc only exposes under
// the `dev-run` feature. Gate the whole file so a thin
// (`--no-default-features`) `--all-targets` build compiles it out instead
// of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! Native-skin wave tests: the phase-1 CSS features the per-OS skins
//! rely on (font family/weight, extended state-pseudo routing, per-side
//! border colors, focus-ring fidelity, per-corner radii, knob-color),
//! plus skin loading (auto resolution, explicit force, author-CSS-wins,
//! zero warnings applying each embedded skin).

use lumenc::layout_ir::{BorderStyleSpec, Element, LayoutIR, Rgba};
use lumenc::parse_html;
use lumenc::parser_css::{
    ColorSchemePreference, MediaContext, apply_css, apply_css_with_media, parse_css,
};
use lumenc::spawn::SpawnIntoWorld;

fn tile(cls: &str) -> LayoutIR {
    parse_html(&format!(r#"<root><tile class="{cls}" /></root>"#)).expect("html")
}

fn first_child(ir: &LayoutIR) -> &Element {
    &ir.root.children[0]
}

fn assert_rgba(c: Rgba, r: f32, g: f32, b: f32) {
    assert!(
        (c.r - r).abs() < 0.01 && (c.g - g).abs() < 0.01 && (c.b - b).abs() < 0.01,
        "expected ({r},{g},{b}), got ({},{},{})",
        c.r,
        c.g,
        c.b
    );
}

// ---------------------------------------------------------------------------
// Phase-1 feature round-trips
// ---------------------------------------------------------------------------

#[test]
fn font_family_and_weight_parse_and_inherit() {
    let mut ir = parse_html(r#"<root><column class="o"><tile class="leaf" /></column></root>"#)
        .expect("html");
    let css = parse_css(
        r#".o { font-family: "Segoe UI Variable Text", "Segoe UI", sans-serif; font-weight: bold; }"#,
    )
    .expect("css");
    apply_css(&mut ir, &css).expect("apply");
    let leaf = &ir.root.children[0].children[0];
    assert_eq!(
        leaf.attrs.font_family.as_deref(),
        Some(r#""Segoe UI Variable Text", "Segoe UI", sans-serif"#),
        "font-family inherits to descendants"
    );
    assert_eq!(leaf.attrs.font_weight, Some(700), "bold = 700, inherited");
}

#[test]
fn font_weight_accepts_numbers_and_rejects_relative_keywords() {
    let mut ir = tile("t");
    let css = parse_css(".t { font-weight: 350; }").expect("css");
    apply_css(&mut ir, &css).expect("apply");
    assert_eq!(first_child(&ir).attrs.font_weight, Some(350));

    let mut ir2 = tile("t");
    let css2 = parse_css(".t { font-weight: bolder; }").expect("css");
    let warnings = apply_css(&mut ir2, &css2).expect("apply");
    assert!(
        !warnings.is_empty(),
        "relative keywords surface as a warning, not silent success"
    );
    assert_eq!(first_child(&ir2).attrs.font_weight, None);
}

#[test]
fn state_pseudos_route_text_color_opacity_and_box_shadow() {
    let mut ir = tile("b");
    let css = parse_css(
        r#"
        .b:hover { text-color: #ff0000; }
        .b:active { text-color: #00ff00; opacity: 0.9; }
        .b:disabled { opacity: 0.5; text-color: #0000ff; }
        .b:focus { box-shadow: inset 0 -2 0 #ff00ff; }
        "#,
    )
    .expect("css");
    let warnings = apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "no warnings: {warnings:?}");
    let a = &first_child(&ir).attrs;
    assert_rgba(a.hover_text_color.expect("hover text"), 1.0, 0.0, 0.0);
    assert_rgba(a.active_text_color.expect("active text"), 0.0, 1.0, 0.0);
    assert_eq!(a.active_opacity, Some(0.9));
    assert_eq!(a.disabled_opacity, Some(0.5));
    assert_rgba(a.disabled_text_color.expect("disabled text"), 0.0, 0.0, 1.0);
    let fs = a.focus_shadows.as_ref().expect("focus shadow stack");
    assert_eq!(fs.len(), 1);
    assert!(fs[0].inner, "focus underline is an inset shadow");
    assert_eq!(fs[0].offset_y, -2.0);
}

#[test]
fn focus_visible_outline_is_distinct_from_focus_outline() {
    let mut ir = tile("b");
    let css = parse_css(
        r#"
        .b:focus { outline: 2 #ff0000; }
        .b:focus-visible { outline: 4 #00ff00; outline-offset: 1; }
        "#,
    )
    .expect("css");
    let warnings = apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "no warnings: {warnings:?}");
    let a = &first_child(&ir).attrs;
    let focus = a.focus_outline.expect(":focus ring");
    let fv = a.focus_visible_outline.expect(":focus-visible ring");
    assert_eq!(focus.width, 2.0);
    assert_eq!(fv.width, 4.0);
    assert_eq!(fv.offset, 1.0, "outline-offset lands on the ring spec");
}

#[test]
fn per_side_border_colors_and_side_shorthand() {
    let mut ir = tile("b");
    let css = parse_css(r#".b { border: 1px solid #101010; border-bottom-color: #ff0000; }"#)
        .expect("css");
    apply_css(&mut ir, &css).expect("apply");
    let a = &first_child(&ir).attrs;
    assert_eq!(a.border_style, Some(BorderStyleSpec::Solid));
    let base = a.border_color.expect("uniform base");
    let sides = a.effective_border_colors(base).expect("per-side colors");
    assert_rgba(sides[2], 1.0, 0.0, 0.0); // bottom
    assert_rgba(sides[0], 0.063, 0.063, 0.063); // top falls back to base

    // Side shorthand on an otherwise borderless element.
    let mut ir2 = tile("u");
    let css2 = parse_css(".u { border-bottom: 2px solid #00ff00; }").expect("css");
    apply_css(&mut ir2, &css2).expect("apply");
    let a2 = &first_child(&ir2).attrs;
    let (widths, _) = a2.effective_border().expect("solid border");
    assert_eq!(widths.bottom, 2.0);
    assert_eq!(widths.top, 0.0, "only the authored side gets width");
    assert_rgba(a2.border_color_bottom.expect("bottom color"), 0.0, 1.0, 0.0);
}

#[test]
fn per_corner_radii_shorthand_and_longhands() {
    let mut ir = tile("t");
    let css = parse_css(".t { radius: 4 4 0 0; }").expect("css");
    apply_css(&mut ir, &css).expect("apply");
    let a = &first_child(&ir).attrs;
    assert_eq!(a.radius_corners, Some([4.0, 4.0, 0.0, 0.0]));
    assert_eq!(a.radius, Some(4.0), "uniform slot carries the max corner");

    let mut ir2 = tile("t");
    let css2 = parse_css(
        ".t { border-radius: 8; border-top-left-radius: 2; border-bottom-right-radius: 12; }",
    )
    .expect("css");
    apply_css(&mut ir2, &css2).expect("apply");
    let a2 = &first_child(&ir2).attrs;
    assert_eq!(a2.radius_corners, Some([2.0, 8.0, 12.0, 8.0]));
    assert_eq!(a2.radius, Some(12.0));
}

#[test]
fn box_shadow_spread_parses() {
    let mut ir = tile("t");
    let css = parse_css(".t { box-shadow: 0 0 0 2 #ff0000, 0 4 8 #00000040; }").expect("css");
    apply_css(&mut ir, &css).expect("apply");
    let shadows = &first_child(&ir).attrs.shadows;
    assert_eq!(shadows.len(), 2);
    assert_eq!(shadows[0].spread, 2.0, "5-token entry parses spread");
    assert_eq!(shadows[1].spread, 0.0, "4-token entry defaults spread 0");
}

#[test]
fn knob_color_parses_from_css_and_markup() {
    let mut ir = parse_html(r#"<root><toggle class="t" /></root>"#).expect("html");
    let css = parse_css(".t { knob-color: #123456; }").expect("css");
    let warnings = apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty());
    assert!(first_child(&ir).attrs.knob_color.is_some());

    let ir2 = parse_html(r##"<root><slider knob-color="#ffffff" /></root>"##).expect("html");
    assert!(first_child(&ir2).attrs.knob_color.is_some());
}

// ---------------------------------------------------------------------------
// Skin loading
// ---------------------------------------------------------------------------

/// A widget-garden-ish fixture exercising every selector the skins ship.
fn widget_fixture() -> LayoutIR {
    parse_html(
        r#"<root>
            <button text="Save" />
            <button class="primary" text="OK" />
            <input placeholder="name" />
            <textarea />
            <toggle />
            <slider />
            <tile />
            <scroll><tile /></scroll>
        </root>"#,
    )
    .expect("fixture html")
}

#[test]
fn auto_resolves_to_a_concrete_skin_for_this_os() {
    let auto = lumenc::skins::resolve_auto();
    assert!(
        lumenc::skins::NAMES.contains(&auto),
        "auto must resolve to a shipped skin, got '{auto}'"
    );
    assert!(
        lumenc::skins::lookup("auto").is_some(),
        "lookup(\"auto\") must return the resolved skin source"
    );
    // On this CI/dev host (Linux) auto = linux.
    if std::env::consts::OS == "linux" {
        assert_eq!(auto, "linux");
    }
}

#[test]
fn every_embedded_skin_parses_and_applies_with_zero_warnings() {
    for name in lumenc::skins::NAMES {
        let src = lumenc::skins::lookup(name).unwrap_or_else(|| panic!("skin '{name}' missing"));
        let sheet = parse_css(src).unwrap_or_else(|e| panic!("skin '{name}' parse error: {e}"));
        for scheme in [ColorSchemePreference::Light, ColorSchemePreference::Dark] {
            let mut ir = widget_fixture();
            let ctx = MediaContext {
                color_scheme: Some(scheme),
                ..Default::default()
            };
            let warnings = apply_css_with_media(&mut ir, &sheet, &ctx)
                .unwrap_or_else(|e| panic!("skin '{name}' apply error: {e}"));
            assert!(
                warnings.is_empty(),
                "skin '{name}' ({scheme:?}) produced warnings: {warnings:#?}"
            );
        }
    }
}

#[test]
fn per_os_skins_style_the_signature_metrics() {
    // macOS: 20px min-height buttons, pill 38x22 switch, no hover shift
    // (hover fill == resting fill).
    let macos = parse_css(lumenc::skins::MACOS).expect("macos css");
    let mut ir = widget_fixture();
    apply_css(&mut ir, &macos).expect("apply macos");
    let button = &ir.root.children[0].attrs;
    assert_eq!(
        button.min_height,
        Some(lumenc::layout_ir::LengthSpec::Px(20.0))
    );
    let resting = match button.bg.as_ref().expect("button bg") {
        lumenc::layout_ir::BgSpec::Solid(c) => *c,
        other => panic!("expected solid, got {other:?}"),
    };
    let hover = button.hover_bg.expect("hover slot authored");
    assert_eq!(resting, hover, "macOS buttons have no hover shift");
    let toggle = &ir.root.children[4].attrs;
    assert_eq!(toggle.width, Some(lumenc::layout_ir::LengthSpec::Px(38.0)));
    assert_eq!(toggle.height, Some(lumenc::layout_ir::LengthSpec::Px(22.0)));
    assert_eq!(toggle.radius, Some(11.0));
    assert!(toggle.knob_color.is_some(), "knob is CSS-reachable");

    // Windows: 4px radius, elevation bottom edge darker than the top,
    // accent primary, focus underline on inputs.
    let windows = parse_css(lumenc::skins::WINDOWS).expect("windows css");
    let mut ir = widget_fixture();
    apply_css(&mut ir, &windows).expect("apply windows");
    let button = &ir.root.children[0].attrs;
    assert_eq!(button.radius, Some(4.0));
    let base = button.border_color.expect("border base");
    let sides = button
        .effective_border_colors(base)
        .expect("per-side colors");
    assert!(
        sides[2].a > sides[0].a,
        "bottom elevation edge is stronger than the top stroke"
    );
    let input = &ir.root.children[2].attrs;
    let focus_shadows = input.focus_shadows.as_ref().expect("focus underline");
    assert!(focus_shadows[0].inner && focus_shadows[0].offset_y < 0.0);
    let primary = &ir.root.children[1].attrs;
    assert!(primary.bg.is_some(), "primary accent fill");

    // Linux/adwaita: flat borderless button (no border style), 50%
    // disabled opacity, bold suggested-action.
    let linux = parse_css(lumenc::skins::LINUX).expect("linux css");
    let mut ir = widget_fixture();
    apply_css(&mut ir, &linux).expect("apply linux");
    let button = &ir.root.children[0].attrs;
    assert_eq!(button.border_style, None, "adwaita buttons are borderless");
    assert_eq!(button.disabled_opacity, Some(0.5));
    let primary = &ir.root.children[1].attrs;
    assert_eq!(primary.font_weight, Some(700), "suggested-action is bold");
    let toggle = &ir.root.children[4].attrs;
    assert_eq!(toggle.width, Some(lumenc::layout_ir::LengthSpec::Px(46.0)));
    assert_eq!(toggle.height, Some(lumenc::layout_ir::LengthSpec::Px(26.0)));
}

#[test]
fn author_css_wins_over_skin_css() {
    // Mirrors the runtime: skin rules concatenate BEFORE author rules
    // into one combined sheet (continuous source order), so at equal
    // specificity the author wins by source order - UA-origin injection
    // identical to default.css.
    let combined_src = format!(
        "{}
{}",
        lumenc::skins::WINDOWS,
        "button { bg: #ff0000; radius: 9; }"
    );
    let combined = parse_css(&combined_src).expect("combined css");
    let mut ir = widget_fixture();
    apply_css(&mut ir, &combined).expect("apply combined");
    let button = &ir.root.children[0].attrs;
    match button.bg.as_ref().expect("bg") {
        lumenc::layout_ir::BgSpec::Solid(c) => assert!(c.r > 0.99 && c.g < 0.01),
        other => panic!("expected solid, got {other:?}"),
    }
    assert_eq!(button.radius, Some(9.0));
    // Skin values the author didn't touch survive.
    assert_eq!(
        button.min_height,
        Some(lumenc::layout_ir::LengthSpec::Px(32.0))
    );
}

// ---------------------------------------------------------------------------
// caret-color / selection-text-color -> TextInputPaint
// ---------------------------------------------------------------------------

/// Both opt-in text-input paint properties parse from CSS, survive the
/// cascade, and land on `TextInputPaint` on the spawned `<input>`. The
/// selection *background* rides `TextStyle::selection_color`; these two
/// are the caret tint and the selected-glyph foreground.
#[test]
fn caret_and_selection_text_color_reach_text_input_paint() {
    use bevy_ecs::prelude::With;
    use bevy_ecs::world::World;
    use lumen_core::components::{TextInput, TextInputPaint};

    let mut ir = parse_html(r#"<root><input class="field" /></root>"#).expect("html");
    let css =
        parse_css(".field { caret-color: #ff0000; selection-text-color: #00ff00; }").expect("css");
    apply_css(&mut ir, &css).expect("apply");

    // Parse / cascade proof: both slots resolved on the input's attrs.
    let attrs = &ir.root.children[0].attrs;
    assert_rgba(
        attrs.caret_color.expect("caret-color parsed"),
        1.0,
        0.0,
        0.0,
    );
    assert_rgba(
        attrs
            .selection_text_color
            .expect("selection-text-color parsed"),
        0.0,
        1.0,
        0.0,
    );

    // Spawn proof: the reconciler populates `TextInputPaint` on the input.
    let mut world = World::new();
    ir.spawn_into(&mut world);
    let paint = world
        .query_filtered::<&TextInputPaint, With<TextInput>>()
        .iter(&world)
        .next()
        .copied()
        .expect("spawned input carries TextInputPaint");
    let c = paint.caret_color.expect("caret_color on TextInputPaint");
    assert!(
        (c.r - 1.0).abs() < 0.01 && c.g < 0.01 && c.b < 0.01,
        "caret color threads through as red, got ({},{},{})",
        c.r,
        c.g,
        c.b
    );
    let f = paint
        .selection_foreground
        .expect("selection_foreground on TextInputPaint");
    assert!(
        f.r < 0.01 && (f.g - 1.0).abs() < 0.01 && f.b < 0.01,
        "selection foreground threads through as green, got ({},{},{})",
        f.r,
        f.g,
        f.b
    );
}

/// The two properties are opt-in and `<input>`/`<textarea>`-scoped: a
/// plain text node never gains `TextInputPaint`, and an input that sets
/// neither property stays component-light (no extra archetype row).
#[test]
fn text_input_paint_is_opt_in_and_input_scoped() {
    use bevy_ecs::world::World;
    use lumen_core::components::TextInputPaint;

    // caret-color on a non-input is inert (no component spawned).
    let mut ir = parse_html(r#"<root><label class="l">hi</label></root>"#).expect("html");
    let css = parse_css(".l { caret-color: #ff0000; }").expect("css");
    apply_css(&mut ir, &css).expect("apply");
    let mut world = World::new();
    ir.spawn_into(&mut world);
    assert_eq!(
        world.query::<&TextInputPaint>().iter(&world).count(),
        0,
        "caret-color on a non-input must not spawn TextInputPaint"
    );

    // An input with neither override carries no paint component.
    let ir2 = parse_html(r#"<root><input /></root>"#).expect("html");
    let mut world2 = World::new();
    ir2.spawn_into(&mut world2);
    assert_eq!(
        world2.query::<&TextInputPaint>().iter(&world2).count(),
        0,
        "an input with no caret / selection-text overrides stays component-light"
    );
}

/// Inline attributes (`caret-color="..." selection-text-color="..."`) parse
/// on `<input>` the same as the CSS longhands.
#[test]
fn caret_and_selection_text_color_parse_as_inline_attrs() {
    let ir = parse_html(
        r##"<root><input caret-color="#ff0000" selection-text-color="#00ff00" /></root>"##,
    )
    .expect("html");
    let attrs = &ir.root.children[0].attrs;
    assert_rgba(
        attrs.caret_color.expect("inline caret-color"),
        1.0,
        0.0,
        0.0,
    );
    assert_rgba(
        attrs
            .selection_text_color
            .expect("inline selection-text-color"),
        0.0,
        1.0,
        0.0,
    );
}
