//! Random operation sequences keep the model's invariants, never lose or
//! duplicate a panel, and survive a save and a restore unchanged.

use std::collections::BTreeSet;

use lumen_dock_model::{
    Axis, Child, DockArea, Node, PanelInfo, Place, Rect, Side, Split, Stack, Target, WindowPlace,
    Workspace,
};
use proptest::prelude::*;

const PANELS: [&str; 8] = ["a", "b", "c", "d", "e", "f", "g", "h"];

fn start() -> Workspace {
    let mut center = Stack::with("center", ["c", "d"]);
    center.center = true;
    let mut w = Workspace::default();
    w.docks.insert(
        "dock".into(),
        DockArea::new(Node::Split(Split {
            axis: Axis::Row,
            id: "x1".into(),
            children: vec![
                Child::grow(Node::Stack(Stack::with("s1", ["a", "b"])), 0.0),
                Child::grow(Node::Stack(center), 1.0),
                Child::grow(Node::Stack(Stack::with("s2", ["e", "f", "g", "h"])), 0.0),
            ],
        })),
    );
    w
}

#[derive(Clone, Debug)]
enum Op {
    Hide(usize),
    Show(usize, u8),
    Activate(usize),
    Tab(usize, usize, usize),
    Edge(usize, usize, u8),
    RootEdge(usize, u8),
    Detach(usize),
    DetachGroup(usize),
    Attach(usize),
    CloseWindow(usize, bool),
    Collapse(usize, bool),
    Maximize(usize),
    Restore,
    Group(usize, usize, u8),
    Float(usize, u8),
    Pin(usize, u8),
    Unpin(usize),
    OpenPin(usize),
    ClosePin,
}

fn side(n: u8) -> Side {
    [Side::Left, Side::Right, Side::Top, Side::Bottom][n as usize % 4]
}

fn op() -> impl Strategy<Value = Op> {
    let p = 0..PANELS.len();
    prop_oneof![
        p.clone().prop_map(Op::Hide),
        (p.clone(), any::<u8>()).prop_map(|(a, b)| Op::Show(a, b)),
        p.clone().prop_map(Op::Activate),
        (p.clone(), p.clone(), 0..5usize).prop_map(|(a, b, c)| Op::Tab(a, b, c)),
        (p.clone(), p.clone(), any::<u8>()).prop_map(|(a, b, c)| Op::Edge(a, b, c)),
        (p.clone(), any::<u8>()).prop_map(|(a, b)| Op::RootEdge(a, b)),
        p.clone().prop_map(Op::Detach),
        p.clone().prop_map(Op::DetachGroup),
        p.clone().prop_map(Op::Attach),
        (0..3usize, any::<bool>()).prop_map(|(a, b)| Op::CloseWindow(a, b)),
        (p.clone(), any::<bool>()).prop_map(|(a, b)| Op::Collapse(a, b)),
        p.clone().prop_map(Op::Maximize),
        Just(Op::Restore),
        (p.clone(), p.clone(), any::<u8>()).prop_map(|(a, b, c)| Op::Group(a, b, c)),
        (p.clone(), any::<u8>()).prop_map(|(a, b)| Op::Float(a, b)),
        (p.clone(), any::<u8>()).prop_map(|(a, b)| Op::Pin(a, b)),
        p.clone().prop_map(Op::Unpin),
        p.prop_map(Op::OpenPin),
        Just(Op::ClosePin),
    ]
}

fn info(place: u8) -> PanelInfo {
    PanelInfo {
        dock: Some("dock".into()),
        place: match place % 4 {
            0 => Place::Center,
            1 => Place::Edge(side(place / 4)),
            2 => Place::Window,
            _ => Place::Beside("a".into()),
        },
        dockable: true,
    }
}

fn run(w: &mut Workspace, op: &Op) {
    let p = |i: usize| PANELS[i];
    let i = info(0);
    let _ = match op {
        Op::Hide(a) => w.hide(p(*a)).map(drop),
        Op::Show(a, place) => w.show(p(*a), &info(*place)).map(drop),
        Op::Activate(a) => w.activate(p(*a)).map(drop),
        Op::Tab(a, b, index) => match w.find(p(*b)) {
            Some(loc) => w
                .move_to(
                    p(*a),
                    &Target::Tab {
                        stack: loc.stack,
                        index: *index,
                    },
                    &i,
                )
                .map(drop),
            None => Ok(()),
        },
        Op::Edge(a, b, s) => match w.find(p(*b)) {
            Some(loc) => w
                .move_to(
                    p(*a),
                    &Target::Edge {
                        node: loc.stack,
                        side: side(*s),
                    },
                    &i,
                )
                .map(drop),
            None => Ok(()),
        },
        Op::RootEdge(a, s) => w
            .move_to(
                p(*a),
                &Target::RootEdge {
                    dock: "dock".into(),
                    side: side(*s),
                },
                &i,
            )
            .map(drop),
        Op::Detach(a) => w
            .detach(
                p(*a),
                WindowPlace {
                    rect: Some(Rect {
                        x: 1.0,
                        y: 2.0,
                        w: 300.0,
                        h: 200.0,
                    }),
                    ..WindowPlace::default()
                },
                &i,
            )
            .map(drop),
        Op::DetachGroup(a) => w.detach_group(p(*a), WindowPlace::default()).map(drop),
        Op::Attach(a) => w.attach(p(*a), &i).map(drop),
        Op::CloseWindow(n, back) => {
            if let Some(id) = w.windows.get(*n).map(|x| x.id.clone()) {
                w.close_window(&id, *back, &|_| info(0));
            }
            Ok(())
        }
        Op::Collapse(a, c) => w.collapse(p(*a), *c).map(drop),
        Op::Maximize(a) => w.maximize(p(*a)).map(drop),
        Op::Restore => {
            w.restore_maximized("dock");
            Ok(())
        }
        // A whole stack onto another panel's stack, beside it, along the
        // dock's edge, or out into a window, by the low bits of `how`.
        Op::Group(a, b, how) => match (w.find(p(*a)), w.find(p(*b))) {
            (Some(from), Some(to)) => {
                let target = match how % 4 {
                    0 => Target::Tab {
                        stack: to.stack,
                        index: (*how / 4) as usize,
                    },
                    1 => Target::Edge {
                        node: to.stack,
                        side: side(*how / 4),
                    },
                    2 => Target::RootEdge {
                        dock: "dock".into(),
                        side: side(*how / 4),
                    },
                    3 if how & 16 == 0 => Target::NewWindow(WindowPlace::default()),
                    _ => Target::NewFloat {
                        dock: "dock".into(),
                        rect: Rect {
                            x: 3.0,
                            y: 4.0,
                            w: 50.0,
                            h: 60.0,
                        },
                    },
                };
                w.move_group(&from.stack, &target, &|_| info(0)).map(drop)
            }
            _ => Ok(()),
        },
        Op::Float(a, at) => w
            .float_panel(
                p(*a),
                "dock",
                Rect {
                    x: f32::from(*at),
                    y: 1.0,
                    w: 100.0,
                    h: 80.0,
                },
                &i,
            )
            .map(drop),
        Op::Pin(a, s) => w.pin(p(*a), side(*s), &i).map(drop),
        Op::Unpin(a) => w.unpin(p(*a), &i).map(drop),
        Op::OpenPin(a) => w.open_pinned(p(*a)).map(drop),
        Op::ClosePin => {
            w.close_pinned("dock");
            Ok(())
        }
    };
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn operations_keep_the_invariants(ops in proptest::collection::vec(op(), 1..40)) {
        let mut w = start();
        for op in &ops {
            let before = w.clone();
            run(&mut w, op);
            if let Err(e) = w.check() {
                panic!("{op:?} broke the model: {e}\nbefore: {before:#?}\nafter: {w:#?}");
            }
            // Every panel is either shown once or remembered as hidden;
            // none is lost.
            let shown: BTreeSet<String> = w.placed_panels().into_iter().collect();
            for p in PANELS {
                prop_assert!(
                    shown.contains(p) || w.homes.contains_key(p),
                    "{p} was lost by {op:?}"
                );
            }
        }
        let text = w.to_json();
        let back = Workspace::from_json(&text, w.layout_version).expect("restores");
        // A slid-out panel is not saved.
        for d in w.docks.values_mut() {
            d.open = None;
        }
        prop_assert_eq!(back, w);
    }

    #[test]
    fn a_hidden_panel_shows_again(ops in proptest::collection::vec(op(), 0..20), pick in 0..PANELS.len()) {
        let mut w = start();
        for op in &ops {
            run(&mut w, op);
        }
        let panel = PANELS[pick];
        if w.is_placed(panel) {
            w.hide(panel).unwrap();
        }
        w.show(panel, &info(0)).unwrap();
        prop_assert!(w.in_view(panel));
        prop_assert!(w.check().is_ok());
    }
}
