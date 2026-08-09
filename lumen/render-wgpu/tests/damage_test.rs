//! Damage-rect diff integration test.
//!
//! Builds two retained scenes that differ by a single rect's color and asserts that
//! [`diff_retained_scenes`] accumulates damage covering only that rect's bounds - not the full viewport.

use lumen_core::components::Color;
use lumen_core::node_ir::Node;
use lumen_core::render_world::{Brush, FrameDamage, Rect};
use lumen_render_wgpu::diff_retained_scenes;
use std::sync::Arc;

fn rect(origin: (f32, f32), size: (f32, f32), color: Color) -> Arc<Node> {
    Arc::new(Node::Rect {
        bounds: Rect {
            origin: glam::Vec2::new(origin.0, origin.1),
            size: glam::Vec2::new(size.0, size.1),
        },
        brush: Brush::Solid(color),
        corner: 0.0,
        corners: None,
    })
}

fn container(children: Vec<Arc<Node>>) -> Arc<Node> {
    Arc::new(Node::Container { children })
}

#[test]
fn single_rect_color_change_damages_only_that_rect() {
    let viewport = Rect {
        origin: glam::Vec2::ZERO,
        size: glam::Vec2::new(800.0, 600.0),
    };

    // Three sibling rects. Only the middle one changes color between prev and curr.
    let a = rect((10.0, 10.0), (50.0, 50.0), Color::rgb(1.0, 0.0, 0.0));
    let b_prev = rect((100.0, 100.0), (40.0, 40.0), Color::rgb(0.0, 1.0, 0.0));
    let b_curr = rect((100.0, 100.0), (40.0, 40.0), Color::rgb(0.0, 0.0, 1.0));
    let c = rect((200.0, 200.0), (60.0, 60.0), Color::rgb(0.5, 0.5, 0.5));

    let prev = container(vec![a.clone(), b_prev, c.clone()]);
    let curr = container(vec![a, b_curr, c]);

    let mut damage = FrameDamage::default();
    diff_retained_scenes(Some(&prev), Some(&curr), viewport, &mut damage);

    assert!(!damage.is_empty(), "expected damage for the color change");

    // Each rect contributes 40*40 = 1600 px^2. The whole viewport is 800*600 = 480_000 px^2. The total damage
    // area should be far smaller than the viewport.
    let viewport_area = viewport.size.x * viewport.size.y;
    let damage_area: f32 = damage.0.iter().map(|r| r.size.x * r.size.y).sum();
    assert!(
        damage_area < viewport_area / 10.0,
        "damage covered too much: {damage_area} px^2 vs viewport {viewport_area} px^2"
    );

    // Every damage rect must fit inside the changed rect's bounds (100..140, 100..140).
    for r in &damage.0 {
        let x0 = r.origin.x;
        let y0 = r.origin.y;
        let x1 = x0 + r.size.x;
        let y1 = y0 + r.size.y;
        assert!(
            x0 >= 100.0 - 1.0 && x1 <= 140.0 + 1.0,
            "damage rect x range {x0}..{x1} outside changed-rect x range"
        );
        assert!(
            y0 >= 100.0 - 1.0 && y1 <= 140.0 + 1.0,
            "damage rect y range {y0}..{y1} outside changed-rect y range"
        );
    }
}

#[test]
fn identical_arcs_short_circuit() {
    let viewport = Rect {
        origin: glam::Vec2::ZERO,
        size: glam::Vec2::new(800.0, 600.0),
    };
    let r = rect((10.0, 10.0), (50.0, 50.0), Color::rgb(1.0, 0.0, 0.0));
    let tree = container(vec![r]);

    let mut damage = FrameDamage::default();
    diff_retained_scenes(Some(&tree), Some(&tree), viewport, &mut damage);

    assert!(damage.is_empty(), "ptr-eq trees should emit no damage");
}

#[test]
fn insertion_damages_only_new_subtree() {
    let viewport = Rect {
        origin: glam::Vec2::ZERO,
        size: glam::Vec2::new(800.0, 600.0),
    };
    let a = rect((10.0, 10.0), (50.0, 50.0), Color::rgb(1.0, 0.0, 0.0));
    let b = rect((300.0, 300.0), (40.0, 40.0), Color::rgb(0.0, 1.0, 0.0));

    let prev = container(vec![a.clone()]);
    let curr = container(vec![a, b]);

    let mut damage = FrameDamage::default();
    diff_retained_scenes(Some(&prev), Some(&curr), viewport, &mut damage);

    assert!(!damage.is_empty());
    for r in &damage.0 {
        let x0 = r.origin.x;
        let y0 = r.origin.y;
        let x1 = x0 + r.size.x;
        let y1 = y0 + r.size.y;
        assert!(
            x0 >= 299.0 && x1 <= 341.0 && y0 >= 299.0 && y1 <= 341.0,
            "expected damage to cover inserted rect bounds (300,300,40,40), got {r:?}"
        );
    }
}

/// Production scenario: the IR producer (`transform_extracted_to_nodes`)
/// rebuilds every leaf as a fresh `Arc` each frame, so NO subtree is ever
/// `Arc`-shared across frames. A purely identity/bounds-based diff would then
/// mark every leaf dirty and never yield a partial region. This asserts the
/// content-based leaf comparison: two independently-built trees with identical
/// appearance emit ZERO damage even though every `Arc` differs.
#[test]
fn fresh_identical_trees_emit_no_damage() {
    let viewport = Rect {
        origin: glam::Vec2::ZERO,
        size: glam::Vec2::new(800.0, 600.0),
    };

    // Two separately-allocated trees (no `.clone()` sharing) with the same 50
    // rects laid out on a grid - models a static 50-widget UI re-extracted on a
    // false-positive `FrameDirty`.
    let build = || {
        let children: Vec<Arc<Node>> = (0..50)
            .map(|i| {
                let x = (i % 10) as f32 * 60.0 + 5.0;
                let y = (i / 10) as f32 * 60.0 + 5.0;
                rect((x, y), (40.0, 40.0), Color::rgb(0.2, 0.4, 0.6))
            })
            .collect();
        container(children)
    };
    let prev = build();
    let curr = build();
    assert!(
        !Arc::ptr_eq(&prev, &curr),
        "test setup: trees must not share storage"
    );

    let mut damage = FrameDamage::default();
    diff_retained_scenes(Some(&prev), Some(&curr), viewport, &mut damage);

    assert!(
        damage.is_empty(),
        "content-identical fresh trees must emit no damage, got {} rects",
        damage.0.len()
    );
}

/// Production scenario: one bound label changes among many widgets. With fresh
/// (non-shared) `Arc`s on both sides, the damage must still collapse to the
/// single changed leaf - proportional to the change, not the scene size.
#[test]
fn fresh_trees_single_change_damages_proportionally() {
    let viewport = Rect {
        origin: glam::Vec2::ZERO,
        size: glam::Vec2::new(800.0, 600.0),
    };

    let build = |changed: bool| {
        let children: Vec<Arc<Node>> = (0..50)
            .map(|i| {
                let x = (i % 10) as f32 * 60.0 + 5.0;
                let y = (i / 10) as f32 * 60.0 + 5.0;
                // Widget #23 flips color in the `changed` tree; every other
                // widget is byte-identical (but a distinct Arc).
                let color = if changed && i == 23 {
                    Color::rgb(0.9, 0.1, 0.1)
                } else {
                    Color::rgb(0.2, 0.4, 0.6)
                };
                rect((x, y), (40.0, 40.0), color)
            })
            .collect();
        container(children)
    };
    let prev = build(false);
    let curr = build(true);

    let mut damage = FrameDamage::default();
    diff_retained_scenes(Some(&prev), Some(&curr), viewport, &mut damage);

    assert!(
        !damage.is_empty(),
        "expected damage for the one changed widget"
    );

    // Damage must be bounded to widget #23's bounds, not the whole scene.
    let viewport_area = viewport.size.x * viewport.size.y;
    let damage_area: f32 = damage.0.iter().map(|r| r.size.x * r.size.y).sum();
    let one_widget = 40.0 * 40.0;
    assert!(
        damage_area <= one_widget + 1.0,
        "damage {damage_area} px^2 should cover ~one 40x40 widget ({one_widget} px^2), \
         not the {viewport_area} px^2 scene"
    );
    // Widget #23 sits at column 3, row 2: x = 3*60+5 = 185, y = 2*60+5 = 125.
    for r in &damage.0 {
        assert!(
            r.origin.x >= 184.0
                && r.origin.x + r.size.x <= 226.0
                && r.origin.y >= 124.0
                && r.origin.y + r.size.y <= 166.0,
            "damage rect {r:?} outside changed widget #23 bounds (185,125,40,40)"
        );
    }
}

#[test]
fn deletion_damages_only_old_subtree() {
    let viewport = Rect {
        origin: glam::Vec2::ZERO,
        size: glam::Vec2::new(800.0, 600.0),
    };
    let a = rect((10.0, 10.0), (50.0, 50.0), Color::rgb(1.0, 0.0, 0.0));
    let b = rect((300.0, 300.0), (40.0, 40.0), Color::rgb(0.0, 1.0, 0.0));

    let prev = container(vec![a.clone(), b]);
    let curr = container(vec![a]);

    let mut damage = FrameDamage::default();
    diff_retained_scenes(Some(&prev), Some(&curr), viewport, &mut damage);

    assert!(!damage.is_empty());
    for r in &damage.0 {
        let x0 = r.origin.x;
        let y0 = r.origin.y;
        let x1 = x0 + r.size.x;
        let y1 = y0 + r.size.y;
        assert!(
            x0 >= 299.0 && x1 <= 341.0 && y0 >= 299.0 && y1 <= 341.0,
            "expected damage to cover removed rect bounds, got {r:?}"
        );
    }
}
