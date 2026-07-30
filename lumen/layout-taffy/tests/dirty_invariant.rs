//! Property test for plan invariant 1: after layout sync, every entity's
//! computed [`Transform`] matches what a from-scratch full recompute (every
//! entity initially dirty) produces - regardless of which subset of entities
//! the user marked dirty.
//!
//! Strategy: build a random hierarchy of styled entities. Apply a random
//! sequence of style mutations, marking only the directly-mutated entities
//! dirty. Tick. Snapshot all transforms. Then build the *same* hierarchy
//! with all entities dirty, tick once, snapshot. The two snapshots must
//! agree.
//!
//! If dirty propagation is broken (e.g., missed ancestor marking), the
//! "incremental" result will diverge from the "from-scratch" baseline.

use lumen_core::prelude::*;
use lumen_layout_taffy::{LayoutResource, TaffyLayoutPlugin};
use proptest::collection::vec;
use proptest::prelude::*;
use std::collections::HashMap;

/// One mutation: mutate this entity's width to this new pixel value.
#[derive(Debug, Clone)]
struct Mutation {
    target_index: usize,
    new_width_px: f32,
}

fn mutation_strat(n_entities: usize) -> impl Strategy<Value = Mutation> {
    // Whole-pixel widths only: taffy's pixel rounding is position-aware
    // (`round(x + w) - round(x)`), so a boundary-local solve - which
    // runs at local origin 0 - can legitimately differ from the
    // from-scratch global solve by 1px when fractional offsets are in
    // play. That's an accepted property of subtree recomputes, not a
    // propagation bug; keeping inputs integral keeps the invariant
    // exact for what this test polices.
    (0..n_entities, 10.0f32..400.0f32).prop_map(|(target_index, new_width_px)| Mutation {
        target_index,
        new_width_px: new_width_px.round(),
    })
}

/// D1: entities are randomly promoted to [`RelayoutBoundary`]. Every
/// spawned entity carries fixed `Px` width + height, which is exactly
/// the constraint-imposed-size condition `lumenc`'s
/// `is_relayout_boundary` requires - so any subset is a valid boundary
/// set, and the incremental result must STILL match the from-scratch
/// baseline: boundary subtree recomputes may not teleport or resize the
/// boundary, and a boundary's own style mutation must pierce to its
/// parent.
fn hierarchy_strat() -> impl Strategy<Value = (usize, Vec<usize>, Vec<bool>)> {
    // n in [2, 8]; for each child i in [1, n), pick parent in [0, i).
    (2usize..=8).prop_flat_map(|n| {
        let parents = (1..n).map(|i| (0..i).boxed()).collect::<Vec<_>>();
        let boundaries = vec(any::<bool>(), n);
        (Just(n), parents, boundaries)
    })
}

fn build_app(
    n: usize,
    parents: &[usize],
    boundaries: &[bool],
    widths: &HashMap<usize, f32>,
) -> (App, Vec<Entity>) {
    let mut app = App::new();
    app.add_plugin(TaffyLayoutPlugin);
    {
        let mut layout = app
            .world
            .get_non_send_resource_mut::<LayoutResource>()
            .unwrap();
        layout.set_viewport(2000.0, 2000.0);
    }
    let mut entities = Vec::with_capacity(n);
    for i in 0..n {
        let w = *widths.get(&i).unwrap_or(&100.0);
        let style = Style {
            width: Length::Px(w),
            height: Length::Px(100.0),
            flex_direction: FlexDirection::Row,
            ..Default::default()
        };
        let e = if i == 0 {
            app.world.spawn((style, DirtyLayout)).id()
        } else {
            let parent = entities[parents[i - 1]];
            app.world.spawn((style, ChildOf(parent), DirtyLayout)).id()
        };
        if boundaries[i] {
            app.world.entity_mut(e).insert(RelayoutBoundary);
        }
        entities.push(e);
    }
    (app, entities)
}

fn snapshot(app: &App, entities: &[Entity]) -> Vec<Option<Transform>> {
    entities
        .iter()
        .map(|e| app.world.get::<Transform>(*e).copied())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Random hierarchy + random mutation sequence: incremental dirty flush
    /// must match a from-scratch full recompute.
    #[test]
    fn dirty_propagation_matches_from_scratch(
        (n, parents, boundaries) in hierarchy_strat(),
        mutations in vec(mutation_strat(8), 0..6),
    ) {
        // Phase A: from-scratch baseline.
        //
        // Pretend the final widths from all mutations are the initial state.
        // Build with every entity initially dirty; tick once.
        let mut final_widths: HashMap<usize, f32> = HashMap::new();
        for m in &mutations {
            if m.target_index < n {
                final_widths.insert(m.target_index, m.new_width_px);
            }
        }
        let (mut app_a, entities_a) = build_app(n, &parents, &boundaries, &final_widths);
        app_a.tick();
        let snap_a = snapshot(&app_a, &entities_a);

        // Phase B: incremental - build with all-default widths, tick to
        // settle, then apply mutations one at a time, marking ONLY the
        // mutated entity dirty (the layout plugin's propagate_dirty_layout
        // system is responsible for marking ancestors).
        let empty: HashMap<usize, f32> = HashMap::new();
        let (mut app_b, entities_b) = build_app(n, &parents, &boundaries, &empty);
        app_b.tick();

        for m in &mutations {
            if m.target_index >= n {
                continue;
            }
            let e = entities_b[m.target_index];
            {
                let mut style = app_b.world.get_mut::<Style>(e).unwrap();
                style.width = Length::Px(m.new_width_px);
            }
            app_b.world.entity_mut(e).insert(DirtyLayout);
            app_b.tick();
        }
        let snap_b = snapshot(&app_b, &entities_b);

        // Compare with float tolerance.
        for i in 0..n {
            let a = snap_a[i].expect("baseline transform");
            let b = snap_b[i].expect("incremental transform");
            let dx = (a.absolute.x - b.absolute.x).abs();
            let dy = (a.absolute.y - b.absolute.y).abs();
            let dw = (a.size.x - b.size.x).abs();
            let dh = (a.size.y - b.size.y).abs();
            prop_assert!(
                dx < 0.01 && dy < 0.01 && dw < 0.01 && dh < 0.01,
                "entity {i}: baseline {a:?} != incremental {b:?}"
            );
        }
    }
}
