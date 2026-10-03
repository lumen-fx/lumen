//! Frame-diff integration test.
//!
//! Builds pairs of retained scenes and asserts that [`diff_retained_scenes`] reports a change exactly
//! when the painted output differs, which is what gates whether a renderer repaints at all.

use lumen_core::components::Color;
use lumen_core::node_ir::Node;
use lumen_core::render_world::{Brush, Rect};
use lumen_paint::diff_retained_scenes;
use std::sync::Arc;

fn viewport() -> Rect {
    Rect {
        origin: glam::Vec2::ZERO,
        size: glam::Vec2::new(800.0, 600.0),
    }
}

fn changed(prev: &Arc<Node>, curr: &Arc<Node>) -> bool {
    diff_retained_scenes(Some(prev), Some(curr), viewport())
}

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

fn native(origin: (f32, f32), size: (f32, f32), revision: u64) -> Arc<Node> {
    Arc::new(Node::Native {
        extension_id: "test.chart".into(),
        // A fresh payload Arc every call, the way a real producer rebuilds it each dirty frame.
        payload: Arc::new(revision),
        bounds: Rect {
            origin: glam::Vec2::new(origin.0, origin.1),
            size: glam::Vec2::new(size.0, size.1),
        },
        revision,
        clip_to_bounds: true,
    })
}

/// A 50-widget grid, with widget #23 recoloured when `recolor` is set. Every call allocates fresh
/// `Arc`s, the way the IR producer rebuilds every leaf each frame.
fn grid(recolor: bool) -> Arc<Node> {
    let children: Vec<Arc<Node>> = (0..50)
        .map(|i| {
            let x = (i % 10) as f32 * 60.0 + 5.0;
            let y = (i / 10) as f32 * 60.0 + 5.0;
            let color = if recolor && i == 23 {
                Color::rgb(0.9, 0.1, 0.1)
            } else {
                Color::rgb(0.2, 0.4, 0.6)
            };
            rect((x, y), (40.0, 40.0), color)
        })
        .collect();
    container(children)
}

#[test]
fn a_single_rect_color_change_is_a_change() {
    let a = rect((10.0, 10.0), (50.0, 50.0), Color::rgb(1.0, 0.0, 0.0));
    let b_prev = rect((100.0, 100.0), (40.0, 40.0), Color::rgb(0.0, 1.0, 0.0));
    let b_curr = rect((100.0, 100.0), (40.0, 40.0), Color::rgb(0.0, 0.0, 1.0));
    let c = rect((200.0, 200.0), (60.0, 60.0), Color::rgb(0.5, 0.5, 0.5));

    let prev = container(vec![a.clone(), b_prev, c.clone()]);
    let curr = container(vec![a, b_curr, c]);
    assert!(changed(&prev, &curr), "a recoloured rect repaints");
}

#[test]
fn identical_arcs_short_circuit() {
    let tree = container(vec![rect(
        (10.0, 10.0),
        (50.0, 50.0),
        Color::rgb(1.0, 0.0, 0.0),
    )]);
    assert!(!changed(&tree, &tree), "a ptr-eq tree is unchanged");
}

/// Production scenario: the IR producer (`transform_extracted_to_nodes`) rebuilds every leaf as a
/// fresh `Arc` each frame, so no subtree is ever `Arc`-shared across frames. A purely identity-based
/// diff would then report every frame changed. Two independently built trees with identical
/// appearance are unchanged even though every `Arc` differs.
#[test]
fn fresh_identical_trees_are_unchanged() {
    let (prev, curr) = (grid(false), grid(false));
    assert!(
        !Arc::ptr_eq(&prev, &curr),
        "test setup: trees must not share storage"
    );
    assert!(!changed(&prev, &curr));
}

/// One bound label changes among many fresh, non-shared widgets: still a change.
#[test]
fn one_change_among_fresh_trees_is_a_change() {
    assert!(changed(&grid(false), &grid(true)));
}

#[test]
fn an_insertion_or_a_deletion_is_a_change() {
    let a = rect((10.0, 10.0), (50.0, 50.0), Color::rgb(1.0, 0.0, 0.0));
    let b = rect((300.0, 300.0), (40.0, 40.0), Color::rgb(0.0, 1.0, 0.0));
    let one = container(vec![a.clone()]);
    let two = container(vec![a, b]);
    assert!(changed(&one, &two), "an inserted rect repaints");
    assert!(changed(&two, &one), "a removed rect repaints");
}

/// A whole tree appearing or disappearing is a change; nothing to nothing is not.
#[test]
fn a_tree_appearing_or_vanishing_is_a_change() {
    let tree = grid(false);
    assert!(diff_retained_scenes(None, Some(&tree), viewport()));
    assert!(diff_retained_scenes(Some(&tree), None, viewport()));
    assert!(!diff_retained_scenes(None, None, viewport()));
}

/// A change that covers no area paints nothing, so it does not cost a frame.
#[test]
fn a_change_with_no_area_is_not_a_change() {
    let prev = container(vec![rect(
        (10.0, 10.0),
        (0.0, 50.0),
        Color::rgb(1.0, 0.0, 0.0),
    )]);
    let curr = container(vec![rect(
        (10.0, 10.0),
        (0.0, 50.0),
        Color::rgb(0.0, 1.0, 0.0),
    )]);
    assert!(!changed(&prev, &curr));
}

/// A plugin's leaf whose content did not change costs nothing, even though its payload is a
/// different allocation this frame. Without the revision contract every native leaf would repaint
/// the window on every frame.
#[test]
fn an_unchanged_native_leaf_is_unchanged() {
    let build = || {
        container(vec![
            rect((10.0, 10.0), (50.0, 50.0), Color::rgb(1.0, 0.0, 0.0)),
            native((200.0, 100.0), (120.0, 80.0), 42),
        ])
    };
    assert!(
        !changed(&build(), &build()),
        "a leaf at the same revision means the same pixels"
    );
}

#[test]
fn a_new_revision_or_a_move_is_a_change() {
    let at = |x: f32, revision: u64| container(vec![native((x, 100.0), (40.0, 40.0), revision)]);
    assert!(changed(&at(100.0, 1), &at(100.0, 2)), "a new revision");
    assert!(changed(&at(100.0, 7), &at(300.0, 7)), "a moved leaf");
}

/// Swapping a native leaf for an ordinary rect is a change rather than the pair being read as the
/// same leaf.
#[test]
fn replacing_a_native_leaf_with_a_rect_is_a_change() {
    let prev = container(vec![native((100.0, 100.0), (40.0, 40.0), 7)]);
    let curr = container(vec![rect(
        (400.0, 400.0),
        (40.0, 40.0),
        Color::rgb(0.0, 1.0, 0.0),
    )]);
    assert!(changed(&prev, &curr));
}

/// A native leaf that declares no area still repaints when its revision moves: it falls back to
/// the viewport rather than freeze on screen.
#[test]
fn a_native_leaf_with_no_area_still_repaints() {
    let prev = container(vec![native((100.0, 100.0), (120.0, 0.0), 1)]);
    let curr = container(vec![native((100.0, 100.0), (120.0, 0.0), 2)]);
    assert!(changed(&prev, &curr));
}
