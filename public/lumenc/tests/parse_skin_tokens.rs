// Same feature gate as `parse.rs`: `parse_html` needs `runtime-parse`, both
// on by default, gated here purely for consistency with the rest of the
// integration test suite.
#![cfg(feature = "dev-run")]

//! Inline-markup-attribute coverage for the skin-tokens CSS property batch
//! (widget geometry, caret/text, scrollbar). `public/lumenc/src/parser_html.rs`
//! keeps a keyword table separate from the stylesheet cascade
//! (`lumen_ir::css::apply_declaration`, unit-tested in `crates/ir/src/css.rs`
//! itself); a property landing in only one of the two tables works in a
//! stylesheet and silently does nothing as an inline attribute. Each test
//! here has a stylesheet-declaration counterpart in `crates/ir/src/css.rs`'s
//! `skin_token_property_tests` module.

use lumenc::layout_ir::LineHeightSpec;
use lumenc::parse_html;

#[test]
fn knob_inset_inline_attribute() {
    let ir = parse_html(r##"<root><toggle knob-inset="2px"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.knob_inset, Some(2.0));
}

#[test]
fn thumb_size_inline_attribute() {
    let ir = parse_html(r##"<root><slider thumb-size="20"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.thumb_size, Some(20.0));
}

#[test]
fn popup_gap_inline_attribute() {
    // `popup-gap` is a plain, tag-agnostic property (like `hover-bg`);
    // a bare `<tile>` avoids pulling in the `<dropdown>` desugar pass,
    // which is unrelated to what this test checks.
    let ir = parse_html(r##"<root><tile popup-gap="4px"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.popup_gap, Some(4.0));
}

#[test]
fn length_px_inline_attribute_rejects_non_numeric() {
    // Malformed case for the "length px" shape, inline-attribute side.
    let r = parse_html(r##"<root><toggle knob-inset="wide"/></root>"##);
    assert!(matches!(
        r,
        Err(lumenc::ParseError::BadAttribute { name, .. }) if name == "knob-inset"
    ));
}

#[test]
fn progress_chunk_inline_attribute_via_chunk_shorthand() {
    // Markup mirror of CSS `progress-chunk` is the short `chunk`
    // attribute (scoped to `<progress>`), matching the pre-existing
    // `duration` shorthand for `progress-duration` - the long hyphenated
    // form is not itself a recognized inline attribute, on either
    // property.
    let ir = parse_html(r##"<root><progress chunk="0.5"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.progress_chunk, Some(0.5));
}

#[test]
fn progress_chunk_inline_attribute_rejects_out_of_range() {
    let r = parse_html(r##"<root><progress chunk="1.5"/></root>"##);
    assert!(matches!(
        r,
        Err(lumenc::ParseError::BadAttribute { name, .. }) if name == "chunk"
    ));
}

#[test]
fn disabled_opacity_inline_attribute() {
    let ir = parse_html(r##"<root><button disabled-opacity="0.4"/></root>"##).expect("parse");
    assert_eq!(
        ir.root.children[0].attrs.disabled_opacity_default,
        Some(0.4)
    );
}

#[test]
fn disabled_opacity_inline_attribute_clamps_out_of_range() {
    let ir = parse_html(r##"<root><button disabled-opacity="2.0"/></root>"##).expect("parse");
    assert_eq!(
        ir.root.children[0].attrs.disabled_opacity_default,
        Some(1.0)
    );
}

#[test]
fn caret_width_inline_attribute() {
    let ir = parse_html(r##"<root><input caret-width="2px"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.caret_width, Some(2.0));
}

#[test]
fn caret_blink_inline_attribute_ms() {
    let ir = parse_html(r##"<root><input caret-blink="530ms"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.caret_blink_ms, Some(530));
}

#[test]
fn caret_blink_inline_attribute_seconds() {
    let ir = parse_html(r##"<root><input caret-blink="0.5s"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.caret_blink_ms, Some(500));
}

#[test]
fn duration_inline_attribute_rejects_missing_unit() {
    // Malformed case for the "duration" shape, inline-attribute side.
    let r = parse_html(r##"<root><input caret-blink="500"/></root>"##);
    assert!(matches!(
        r,
        Err(lumenc::ParseError::BadAttribute { name, .. }) if name == "caret-blink"
    ));
}

#[test]
fn password_character_inline_attribute() {
    let ir = parse_html(r##"<root><input password-character="*"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.password_character, Some('*'));
}

#[test]
fn password_character_inline_attribute_rejects_multiple_characters() {
    // Malformed case for the "single character" shape, inline-attribute side.
    let r = parse_html(r##"<root><input password-character="**"/></root>"##);
    assert!(matches!(
        r,
        Err(lumenc::ParseError::BadAttribute { name, .. }) if name == "password-character"
    ));
}

#[test]
fn line_height_inline_attribute_unitless_multiplier() {
    let ir = parse_html(r##"<root><label line-height="1.2"/></root>"##).expect("parse");
    assert_eq!(
        ir.root.children[0].attrs.line_height,
        Some(LineHeightSpec::Multiplier(1.2))
    );
}

#[test]
fn line_height_inline_attribute_px() {
    let ir = parse_html(r##"<root><label line-height="19px"/></root>"##).expect("parse");
    assert_eq!(
        ir.root.children[0].attrs.line_height,
        Some(LineHeightSpec::Px(19.0))
    );
}

#[test]
fn line_height_inline_attribute_unitless_and_px_are_distinct() {
    let a = parse_html(r##"<root><label line-height="1.2"/></root>"##).expect("parse");
    let b = parse_html(r##"<root><label line-height="1.2px"/></root>"##).expect("parse");
    assert_ne!(
        a.root.children[0].attrs.line_height,
        b.root.children[0].attrs.line_height
    );
}

#[test]
fn line_height_inline_attribute_rejects_negative() {
    // Malformed case for the "unitless-vs-px" shape, inline-attribute side.
    let r = parse_html(r##"<root><label line-height="-1"/></root>"##);
    assert!(matches!(
        r,
        Err(lumenc::ParseError::BadAttribute { name, .. }) if name == "line-height"
    ));
}

#[test]
fn scrollbar_thickness_inline_attribute() {
    let ir = parse_html(r##"<root><scroll scrollbar-thickness="10px"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.scrollbar_thickness, Some(10.0));
}

#[test]
fn scrollbar_thickness_thin_inline_attribute() {
    let ir =
        parse_html(r##"<root><scroll scrollbar-thickness-thin="6px"/></root>"##).expect("parse");
    assert_eq!(
        ir.root.children[0].attrs.scrollbar_thickness_thin,
        Some(6.0)
    );
}

#[test]
fn scrollbar_margin_inline_attribute() {
    let ir = parse_html(r##"<root><scroll scrollbar-margin="2px"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.scrollbar_margin, Some(2.0));
}

#[test]
fn scrollbar_min_thumb_inline_attribute() {
    let ir = parse_html(r##"<root><scroll scrollbar-min-thumb="24px"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.scrollbar_min_thumb, Some(24.0));
}

#[test]
fn scrollbar_track_hover_inline_attribute() {
    let ir =
        parse_html(r##"<root><scroll scrollbar-track-hover="#334455"/></root>"##).expect("parse");
    assert!(ir.root.children[0].attrs.scrollbar_track_hover.is_some());
}

#[test]
fn color_inline_attribute_rejects_non_hex() {
    // Malformed case for the "colour" shape, inline-attribute side.
    let r = parse_html(r##"<root><scroll scrollbar-track-hover="blue"/></root>"##);
    assert!(matches!(
        r,
        Err(lumenc::ParseError::BadAttribute { name, .. }) if name == "scrollbar-track-hover"
    ));
}

#[test]
fn scrollbar_hover_boost_inline_attribute() {
    let ir = parse_html(r##"<root><scroll scrollbar-hover-boost="1.4"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.scrollbar_hover_boost, Some(1.4));
}

#[test]
fn scrollbar_fade_delay_inline_attribute() {
    let ir = parse_html(r##"<root><scroll scrollbar-fade-delay="800ms"/></root>"##).expect("parse");
    assert_eq!(ir.root.children[0].attrs.scrollbar_fade_delay_ms, Some(800));
}

#[test]
fn scrollbar_fade_duration_inline_attribute() {
    let ir =
        parse_html(r##"<root><scroll scrollbar-fade-duration="0.2s"/></root>"##).expect("parse");
    assert_eq!(
        ir.root.children[0].attrs.scrollbar_fade_duration_ms,
        Some(200)
    );
}
