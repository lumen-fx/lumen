//! The byte-array conversions on `Color`: the const `from_rgba8` palettes
//! are written in, `to_rgba8` packs back out, and the `From` impls carry
//! both directions as standard traits.

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
