//! Pins the encoding of the paint command list.
//!
//! Every [`PaintOp`] variant is built with fixed field values, encoded, and
//! compared against bytes checked in here. Appending a variant leaves the
//! earlier bytes alone and passes; inserting or reordering one changes them and
//! fails, which is the point: the encoding writes a variant by its index, and a
//! peer decoding an index it disagrees about draws a different op with the same
//! confidence.
//!
//! Regenerate the table with
//! `cargo test -p lumen-core --test paint_wire -- --ignored --nocapture`, and
//! only after deciding the change is intended and bumping
//! [`PAINT_WIRE_VERSION`].

use bincode::Options;
use lumen_core::components::Color;
use lumen_core::paint::{
    Cap, FillRule, GradientStop, Join, PAINT_WIRE_VERSION, PaintBrush, PaintList, PaintOp,
    PaintPath, PathEl,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Upper bound on one decoded payload, matching the plugin boundary's codec.
const MAX_PAYLOAD: u64 = 512 * 1024 * 1024;

/// The encode half of the boundary codec: plain `bincode::serialize`.
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    bincode::serialize(value).map_err(|e| e.to_string())
}

/// The decode half: `DefaultOptions` defaults to varint, so fixint has to be
/// asked for or it would mis-read what `encode` wrote.
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PAYLOAD)
        .deserialize(bytes)
        .map_err(|e| e.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn path() -> PaintPath {
    PaintPath {
        els: vec![
            PathEl::MoveTo([0.0, 0.0]),
            PathEl::LineTo([1.0, 0.0]),
            PathEl::QuadTo([1.0, 1.0], [0.0, 1.0]),
            PathEl::CurveTo([0.0, 2.0], [1.0, 2.0], [2.0, 2.0]),
            PathEl::Close,
        ],
    }
}

fn solid() -> PaintBrush {
    PaintBrush::Solid(Color::rgba(0.0, 0.25, 0.5, 1.0))
}

/// One sample per [`PaintOp`] variant, in declaration order, covering every
/// [`PaintBrush`] and [`PathEl`] variant along the way.
fn op_samples() -> Vec<(&'static str, PaintOp)> {
    vec![
        ("Save", PaintOp::Save),
        ("Restore", PaintOp::Restore),
        (
            "Transform",
            PaintOp::Transform([1.0, 0.0, 0.0, 1.0, 8.0, 4.0]),
        ),
        ("Clip", PaintOp::Clip(path())),
        (
            "Fill",
            PaintOp::Fill {
                path: path(),
                brush: solid(),
                rule: FillRule::EvenOdd,
            },
        ),
        (
            "Stroke",
            PaintOp::Stroke {
                path: path(),
                brush: PaintBrush::LinearGradient {
                    start: [0.0, 0.0],
                    end: [4.0, 0.0],
                    stops: vec![
                        GradientStop {
                            offset: 0.0,
                            color: Color::rgb(1.0, 0.0, 0.0),
                        },
                        GradientStop {
                            offset: 1.0,
                            color: Color::rgb(0.0, 0.0, 1.0),
                        },
                    ],
                },
                width: 2.0,
                cap: Cap::Round,
                join: Join::Bevel,
            },
        ),
        (
            "Pixels",
            PaintOp::Pixels {
                buffer: 1,
                epoch: 2,
                w: 1,
                h: 1,
                data: Some(vec![255, 0, 0, 255]),
                dst: [0.0, 0.0, 16.0, 16.0],
            },
        ),
        (
            "Pixels/cached",
            PaintOp::Pixels {
                buffer: 1,
                epoch: 2,
                w: 1,
                h: 1,
                data: None,
                dst: [0.0, 0.0, 16.0, 16.0],
            },
        ),
        (
            "Text",
            PaintOp::Text {
                origin: [2.0, 12.0],
                text: "hi".to_string(),
                size_px: 14.0,
                family: Some("Inter".to_string()),
                weight: 700,
                italic: true,
                brush: PaintBrush::RadialGradient {
                    center: [1.0, 1.0],
                    radius: 3.0,
                    stops: vec![GradientStop {
                        offset: 0.5,
                        color: Color::rgba(0.0, 0.0, 0.0, 0.5),
                    }],
                },
            },
        ),
        (
            "PushLayer",
            PaintOp::PushLayer {
                alpha: 0.5,
                clip: Some(path()),
            },
        ),
        ("PopLayer", PaintOp::PopLayer),
    ]
}

/// The same ops as one list, which is how they travel.
fn sample_list() -> PaintList {
    PaintList {
        ops: op_samples().into_iter().map(|(_, op)| op).collect(),
    }
}

const GOLDEN: &[(&str, &str)] = &[
    ("Save", "00000000"),
    ("Restore", "01000000"),
    (
        "Transform",
        "020000000000803f00000000000000000000803f0000004100008040",
    ),
    (
        "Clip",
        "030000000500000000000000000000000000000000000000010000000000803f00000000020000000000803f0000803f000000000000803f0300000000000000000000400000803f00000040000000400000004004000000",
    ),
    (
        "Fill",
        "040000000500000000000000000000000000000000000000010000000000803f00000000020000000000803f0000803f000000000000803f0300000000000000000000400000803f0000004000000040000000400400000000000000000000000000803e0000003f0000803f01000000",
    ),
    (
        "Stroke",
        "050000000500000000000000000000000000000000000000010000000000803f00000000020000000000803f0000803f000000000000803f0300000000000000000000400000803f0000004000000040000000400400000001000000000000000000000000008040000000000200000000000000000000000000803f00000000000000000000803f0000803f00000000000000000000803f0000803f000000400100000002000000",
    ),
    (
        "Pixels",
        "06000000010000000000000002000000000000000100000001000000010400000000000000ff0000ff00000000000000000000804100008041",
    ),
    (
        "Pixels/cached",
        "060000000100000000000000020000000000000001000000010000000000000000000000000000804100008041",
    ),
    (
        "Text",
        "0700000000000040000040410200000000000000686900006041010500000000000000496e746572bc0201020000000000803f0000803f0000404001000000000000000000003f0000000000000000000000000000003f",
    ),
    (
        "PushLayer",
        "080000000000003f010500000000000000000000000000000000000000010000000000803f00000000020000000000803f0000803f000000000000803f0300000000000000000000400000803f00000040000000400000004004000000",
    ),
    ("PopLayer", "09000000"),
];

#[test]
fn wire_version_is_pinned() {
    assert_eq!(PAINT_WIRE_VERSION, 1);
}

#[test]
fn paint_encoding_is_pinned() {
    let samples = op_samples();
    assert_eq!(
        samples.len(),
        GOLDEN.len(),
        "every op variant needs a golden; regenerate with --ignored"
    );
    for ((name, op), (golden_name, golden)) in samples.iter().zip(GOLDEN) {
        assert_eq!(name, golden_name, "sample order drifted from the golden");
        let bytes = encode(op).expect("sample encodes");
        assert_eq!(&hex(&bytes), golden, "encoding of {name} changed");
    }
}

#[test]
fn paint_lists_round_trip() {
    let list = sample_list();
    let bytes = encode(&list).expect("the sample encodes");
    let back: PaintList = decode(&bytes).expect("the sample decodes");
    assert_eq!(list, back);
}

#[test]
fn an_empty_list_encodes() {
    let bytes = encode(&PaintList::default()).expect("an empty list encodes");
    let back: PaintList = decode(&bytes).expect("an empty list decodes");
    assert!(back.ops.is_empty());
}

/// Prints the golden table. Run with `--ignored --nocapture` and paste the
/// output over the constant above.
#[test]
#[ignore = "regenerates the golden table"]
fn print_golden() {
    println!("const GOLDEN: &[(&str, &str)] = &[");
    for (name, op) in op_samples() {
        println!("    (\"{name}\", \"{}\"),", hex(&encode(&op).unwrap()));
    }
    println!("];");
}
