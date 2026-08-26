//! The byte-array conversions on `Color`: the const `from_rgba8` palettes
//! are written in, `to_rgba8` packs back out, and the `From` impls carry
//! both directions as standard traits. `Color::from_hex` is the engine's
//! one hex parser, shared by the palette literals and every script host's
//! color builtins.

use lumen_core::components::Color;

#[test]
fn rgba8_round_trips_through_both_directions() {
    let c = Color::from_rgba8([0x3a, 0x68, 0xd8, 0xff]);
    assert!((c.r - 0x3a as f32 / 255.0).abs() < 1e-6);
    assert!((c.a - 1.0).abs() < 1e-6);
    assert_eq!(c.to_rgba8(), [0x3a, 0x68, 0xd8, 0xff]);

    let via_from: Color = [0x20, 0x21, 0x24, 0x80].into();
    let back: [u8; 4] = via_from.into();
    assert_eq!(back, [0x20, 0x21, 0x24, 0x80]);
}

#[test]
fn to_rgba8_clamps_out_of_range_channels() {
    let c = Color::rgba(1.5, -0.2, 0.5, 1.0);
    assert_eq!(c.to_rgba8(), [255, 0, 128, 255]);
}

#[test]
fn hex_strings_parse_in_every_shape() {
    let opaque = Color::from_hex("#ff8800").expect("rrggbb parses");
    assert_eq!(opaque.to_rgba8(), [0xff, 0x88, 0x00, 0xff]);
    let with_alpha = Color::from_hex("ff880080").expect("bare rrggbbaa parses");
    assert_eq!(with_alpha.to_rgba8(), [0xff, 0x88, 0x00, 0x80]);
    // Short forms double each digit.
    let short = Color::from_hex("#f80").expect("rgb parses");
    assert_eq!(short.to_rgba8(), [0xff, 0x88, 0x00, 0xff]);
    let short_alpha = Color::from_hex("f808").expect("bare rgba parses");
    assert_eq!(short_alpha.to_rgba8(), [0xff, 0x88, 0x00, 0x88]);
}

#[test]
fn bad_hex_strings_are_misses_never_panics() {
    // Script input arrives arbitrary: `signal_set_color` hands whatever the
    // app's script passed straight to this parser, so multi-byte input must
    // be a parse miss instead of a panic on a char boundary.
    for bad in [
        "",
        "#",
        "#ff88x",
        "#ff88001",
        "zzzzzz",
        "\u{20ac}\u{20ac}",
        "#\u{e9}\u{e9}\u{e9}\u{e9}",
    ] {
        assert!(Color::from_hex(bad).is_none(), "{bad:?} must not parse");
    }
}
