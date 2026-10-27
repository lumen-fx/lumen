use super::*;

/// The editor dock of the design: hierarchy and assets on the left, scene
/// and game in the center, the inspector on the right.
fn editor() -> Workspace {
    let left = Node::Split(Split {
        axis: Axis::Column,
        id: "x2".into(),
        children: vec![
            Child::grow(Node::Stack(Stack::with("s1", ["hierarchy"])), 1.0),
            Child::grow(Node::Stack(Stack::with("s2", ["assets"])), 1.0),
        ],
    });
    let mut center = Stack::with("center", ["scene", "game"]);
    center.center = true;
    let root = Node::Split(Split {
        axis: Axis::Row,
        id: "x1".into(),
        children: vec![
            Child {
                basis: Some(260.0),
                grow: 0.0,
                node: left,
            },
            Child::grow(Node::Stack(center), 1.0),
            Child {
                basis: Some(320.0),
                grow: 0.0,
                node: Node::Stack(Stack::with("s3", ["inspector"])),
            },
        ],
    });
    let mut w = Workspace::default();
    w.docks.insert("editor".into(), DockArea::new(root));
    w.check().expect("the fixture is valid");
    w
}

fn docked() -> PanelInfo {
    PanelInfo {
        dock: Some("editor".into()),
        place: Place::Center,
        dockable: true,
    }
}

#[test]
fn hide_and_show_return_a_panel_to_its_stack() {
    let mut w = editor();
    let effects = w.hide("game").unwrap();
    assert_eq!(effects, vec![Effect::Hidden("game".into())]);
    assert!(!w.is_shown("game"));
    assert_eq!(w.stack("center").unwrap().active.as_deref(), Some("scene"));
    w.check().unwrap();

    let effects = w.show("game", &docked()).unwrap();
    assert!(effects.contains(&Effect::Shown("game".into())));
    let loc = w.find("game").unwrap();
    assert_eq!((loc.stack.as_str(), loc.index), ("center", 1));
    assert_eq!(w.stack("center").unwrap().active.as_deref(), Some("game"));
    assert!(w.homes.is_empty());
    w.check().unwrap();
}

#[test]
fn a_hidden_panel_whose_stack_went_comes_back_in_its_split() {
    let mut w = editor();
    w.hide("assets").unwrap();
    // The left column held two stacks; with one left the split collapses.
    assert!(w.stack("s2").is_none());
    w.hide("hierarchy").unwrap();
    // Both gone: the left column is gone too.
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!("still a split")
    };
    assert_eq!(root.children.len(), 2);
    w.check().unwrap();
    // Its stack went, and the split it went with collapsed into the root
    // split, which is where the stack is rebuilt, at the left.
    w.show("hierarchy", &docked()).unwrap();
    assert_eq!(w.find("hierarchy").unwrap().stack, "s1");
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!()
    };
    assert_eq!(root.children[0].node.id(), "s1");
    w.check().unwrap();
}

#[test]
fn a_stack_comes_back_into_its_split_at_its_slot() {
    let mut w = editor();
    w.hide("assets").unwrap();
    // The left split collapsed into the hierarchy stack: assets' split is
    // gone. The inspector's stack sits in the root split.
    w.hide("inspector").unwrap();
    assert!(w.stack("s3").is_none());
    w.show("inspector", &docked()).unwrap();
    let loc = w.find("inspector").unwrap();
    assert_eq!(loc.stack, "s3", "the stack is rebuilt under its old id");
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!()
    };
    assert_eq!(root.children.last().unwrap().node.id(), "s3");
    w.check().unwrap();
}

#[test]
fn moving_to_an_edge_splits_the_target() {
    let mut w = editor();
    let effects = w
        .move_to(
            "game",
            &Target::Edge {
                node: "s3".into(),
                side: Side::Bottom,
            },
            &docked(),
        )
        .unwrap();
    assert!(effects.iter().any(|e| matches!(e, Effect::Moved { .. })));
    let loc = w.find("game").unwrap();
    assert_ne!(loc.stack, "center");
    // The inspector's slot is now a column split of inspector over game.
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!()
    };
    let Node::Split(right) = &root.children[2].node else {
        panic!("the right child became a split")
    };
    assert_eq!(right.axis, Axis::Column);
    assert_eq!(right.children[1].node.stacks()[0].panels, vec!["game"]);
    w.check().unwrap();
}

#[test]
fn moving_into_a_row_split_inserts_beside() {
    let mut w = editor();
    w.move_to(
        "hierarchy",
        &Target::Edge {
            node: "center".into(),
            side: Side::Right,
        },
        &docked(),
    )
    .unwrap();
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!()
    };
    // The left column lost hierarchy and collapsed to assets; the new
    // stack sits right of the center.
    assert_eq!(root.children.len(), 4);
    assert_eq!(root.children[0].node.id(), "s2");
    assert_eq!(root.children[2].node.stacks()[0].panels, vec!["hierarchy"]);
    w.check().unwrap();
}

#[test]
fn a_tab_move_inside_a_stack_reorders() {
    let mut w = editor();
    w.move_to(
        "scene",
        &Target::Tab {
            stack: "center".into(),
            index: 5,
        },
        &docked(),
    )
    .unwrap();
    assert_eq!(w.stack("center").unwrap().panels, vec!["game", "scene"]);
    // Beside its own one-panel stack is no move at all.
    let before = w.clone();
    let effects = w
        .move_to(
            "inspector",
            &Target::Edge {
                node: "s3".into(),
                side: Side::Left,
            },
            &docked(),
        )
        .unwrap();
    assert!(effects.is_empty());
    assert_eq!(w, before);
}

#[test]
fn root_edges_wrap_the_tree() {
    let mut w = editor();
    w.move_to(
        "game",
        &Target::RootEdge {
            dock: "editor".into(),
            side: Side::Bottom,
        },
        &docked(),
    )
    .unwrap();
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!()
    };
    assert_eq!(root.axis, Axis::Column);
    assert_eq!(root.children[1].node.stacks()[0].panels, vec!["game"]);
    w.move_to(
        "scene",
        &Target::RootEdge {
            dock: "editor".into(),
            side: Side::Top,
        },
        &docked(),
    )
    .unwrap();
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!()
    };
    assert_eq!(
        root.children.len(),
        3,
        "the column split took it at the top"
    );
    assert_eq!(root.children[0].node.stacks()[0].panels, vec!["scene"]);
    // The center emptied and stays.
    assert!(w.stack("center").unwrap().panels.is_empty());
    w.check().unwrap();
}

#[test]
fn unknown_targets_and_panels_are_refused_without_change() {
    let mut w = editor();
    let before = w.clone();
    assert_eq!(w.hide("nope"), Err(OpError::UnknownPanel("nope".into())));
    assert!(matches!(
        w.move_to(
            "game",
            &Target::Tab {
                stack: "zzz".into(),
                index: 0
            },
            &docked()
        ),
        Err(OpError::UnknownTarget(_))
    ));
    assert!(matches!(
        w.move_to(
            "game",
            &Target::Edge {
                node: "zzz".into(),
                side: Side::Left
            },
            &docked()
        ),
        Err(OpError::UnknownTarget(_))
    ));
    assert!(matches!(
        w.move_to(
            "game",
            &Target::RootEdge {
                dock: "zzz".into(),
                side: Side::Left
            },
            &docked()
        ),
        Err(OpError::UnknownTarget(_))
    ));
    assert!(matches!(
        w.attach("game", &docked()),
        Err(OpError::NotInWindow(_))
    ));
    assert!(matches!(
        w.collapse("zzz", true),
        Err(OpError::UnknownTarget(_))
    ));
    assert!(matches!(w.maximize("zzz"), Err(OpError::UnknownPanel(_))));
    let undockable = PanelInfo {
        dockable: false,
        ..docked()
    };
    assert!(matches!(
        w.move_to(
            "game",
            &Target::Tab {
                stack: "s3".into(),
                index: 0
            },
            &undockable
        ),
        Err(OpError::NotDockable(_))
    ));
    assert_eq!(w, before);
    for e in [
        OpError::UnknownPanel("a".into()),
        OpError::UnknownTarget("a".into()),
        OpError::NotDockable("a".into()),
        OpError::NotInWindow("a".into()),
        OpError::NotInDock("a".into()),
    ] {
        assert!(e.to_string().contains("'a'"));
    }
}

#[test]
fn detach_and_attach_round_trip() {
    let mut w = editor();
    let place = WindowPlace {
        rect: Some(Rect {
            x: 10.0,
            y: 20.0,
            w: 640.0,
            h: 480.0,
        }),
        ..WindowPlace::default()
    };
    let effects = w.detach("game", place.clone(), &docked()).unwrap();
    assert!(effects.contains(&Effect::WindowOpened("w1".into())));
    assert_eq!(w.where_is("game"), Where::Window("w1".into()));
    assert_eq!(w.windows[0].home.as_deref(), Some("editor"));
    assert_eq!(w.windows[0].place, place);
    w.check().unwrap();
    // Detaching again changes nothing: it is alone in its window.
    assert!(
        w.detach("game", WindowPlace::default(), &docked())
            .unwrap()
            .is_empty()
    );

    let effects = w.attach("game", &docked()).unwrap();
    assert!(effects.contains(&Effect::WindowClosed("w1".into())));
    assert!(w.windows.is_empty());
    let loc = w.find("game").unwrap();
    assert_eq!((loc.stack.as_str(), loc.index), ("center", 1));
    assert!(w.homes.is_empty());
    w.check().unwrap();
}

/// A panel whose stack went with it into a window comes back in a stack of
/// the size the old one had, not one that takes the split's spare room.
#[test]
fn a_stack_made_again_takes_the_room_the_old_one_had() {
    let mut w = editor();
    w.detach("inspector", WindowPlace::default(), &docked())
        .unwrap();
    assert!(w.stack("s3").is_none(), "the emptied stack went");
    let home = w.homes["inspector"].dock.clone().expect("a spot");
    assert_eq!((home.basis, home.grow), (Some(320.0), Some(0.0)));
    w.attach("inspector", &docked()).unwrap();
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!("a split")
    };
    let back = root.children.last().expect("at its slot");
    assert_eq!((back.basis, back.grow), (Some(320.0), 0.0));
    w.check().unwrap();
}

#[test]
fn detaching_a_group_moves_the_stack_and_leaves_the_center() {
    let mut w = editor();
    w.detach_group("scene", WindowPlace::default()).unwrap();
    assert!(w.stack("center").unwrap().panels.is_empty());
    let win = &w.windows[0];
    assert_eq!(win.root.panels(), vec!["scene", "game"]);
    w.check().unwrap();
    // Closing the window docks both back where they were.
    let effects = w.close_window("w1", true, &|_| docked());
    assert!(effects.contains(&Effect::WindowClosed("w1".into())));
    assert_eq!(w.stack("center").unwrap().panels, vec!["scene", "game"]);
    w.check().unwrap();

    // A non-center stack moves whole and comes back into its split.
    w.detach_group("inspector", WindowPlace::default()).unwrap();
    assert!(w.stack("s3").is_none());
    w.close_window("w1", true, &|_| docked());
    assert_eq!(w.find("inspector").unwrap().stack, "s3");
    w.check().unwrap();
    // A group in a window is not popped again.
    w.detach("game", WindowPlace::default(), &docked()).unwrap();
    assert!(
        w.detach_group("game", WindowPlace::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn closing_a_window_hides_what_does_not_dock() {
    let mut w = editor();
    w.detach("game", WindowPlace::default(), &docked()).unwrap();
    let effects = w.close_window("w1", false, &|_| docked());
    assert!(effects.contains(&Effect::Hidden("game".into())));
    assert!(!w.is_shown("game"));
    // Shown again, it opens a window where it was.
    w.show("game", &docked()).unwrap();
    assert!(matches!(w.where_is("game"), Where::Window(_)));
    // Its dock spot survives the trip, so attaching docks it home.
    w.attach("game", &docked()).unwrap();
    assert_eq!(w.find("game").unwrap().stack, "center");
    w.check().unwrap();
    assert!(w.close_window("nope", true, &|_| docked()).is_empty());
}

#[test]
fn free_panels_open_in_windows_and_never_dock() {
    let mut w = editor();
    let prefs = PanelInfo {
        dock: None,
        place: Place::Window,
        dockable: false,
    };
    w.show("prefs", &prefs).unwrap();
    assert!(matches!(w.where_is("prefs"), Where::Window(_)));
    assert_eq!(w.windows[0].home, None);
    assert_eq!(
        w.attach("prefs", &prefs),
        Err(OpError::NotDockable("prefs".into()))
    );
    // Closing its window hides it.
    w.close_window(&w.windows[0].id.clone(), true, &|_| prefs.clone());
    assert!(!w.is_shown("prefs"));
    w.check().unwrap();
}

#[test]
fn places_put_a_first_shown_panel() {
    let mut w = editor();
    let at = |place| PanelInfo {
        dock: Some("editor".into()),
        place,
        dockable: true,
    };
    w.show("profiler", &at(Place::Edge(Side::Right))).unwrap();
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!()
    };
    assert_eq!(
        root.children.last().unwrap().node.stacks()[0].panels,
        vec!["profiler"]
    );
    w.show("log", &at(Place::Beside("inspector".into())))
        .unwrap();
    assert_eq!(w.stack("s3").unwrap().panels, vec!["inspector", "log"]);
    w.show("notes", &at(Place::Beside("missing".into())))
        .unwrap();
    assert_eq!(w.find("notes").unwrap().stack, "center");
    w.show("float", &at(Place::Window)).unwrap();
    assert!(matches!(w.where_is("float"), Where::Window(_)));
    // Showing a shown panel activates it.
    assert_eq!(
        w.show("scene", &docked()).unwrap(),
        vec![Effect::Activated("scene".into())]
    );
    assert_eq!(Place::parse("beside:x"), Some(Place::Beside("x".into())));
    assert_eq!(Place::parse("top"), Some(Place::Edge(Side::Top)));
    assert_eq!(Place::parse(""), Some(Place::Center));
    assert_eq!(Place::parse("window"), Some(Place::Window));
    assert_eq!(Place::parse("bogus"), None);
    w.check().unwrap();
}

#[test]
fn an_empty_dock_takes_an_edge_panel_as_its_root() {
    let mut w = Workspace::default();
    w.docks.insert(
        "d".into(),
        DockArea::new(Node::Stack(Stack::with("s1", ["a"]))),
    );
    w.hide("a").unwrap();
    w.check().unwrap();
    let info = PanelInfo {
        dock: Some("d".into()),
        place: Place::Edge(Side::Left),
        dockable: true,
    };
    w.homes.clear();
    w.show("b", &info).unwrap();
    assert_eq!(w.docks["d"].root.panels(), vec!["b"]);
    w.check().unwrap();
}

#[test]
fn collapse_and_maximize() {
    let mut w = editor();
    w.collapse("inspector", true).unwrap();
    assert!(w.stack("s3").unwrap().collapsed);
    w.collapse("s3", false).unwrap();
    assert!(!w.stack("s3").unwrap().collapsed);
    w.collapse("s3", true).unwrap();

    w.maximize("inspector").unwrap();
    assert_eq!(w.docks["editor"].maximized.as_deref(), Some("inspector"));
    assert!(
        !w.stack("s3").unwrap().collapsed,
        "a maximized stack expands"
    );
    assert!(w.restore_maximized("editor"));
    assert!(!w.restore_maximized("editor"));
    w.maximize("game").unwrap();
    assert_eq!(w.stack("center").unwrap().active.as_deref(), Some("game"));
    // Hiding the maximized panel gives the dock its tree back.
    w.hide("game").unwrap();
    assert_eq!(w.docks["editor"].maximized, None);
    w.detach("scene", WindowPlace::default(), &docked())
        .unwrap();
    assert_eq!(w.maximize("scene"), Err(OpError::NotInDock("scene".into())));
    w.check().unwrap();
}

#[test]
fn resize_holds_child_sizes() {
    let mut w = editor();
    assert!(w.resize("x1", &[200.0, 600.0, 300.0]));
    let Node::Split(root) = &w.docks["editor"].root else {
        panic!()
    };
    assert_eq!(root.children[0].basis, Some(200.0));
    assert_eq!(root.children[1].basis, Some(600.0));
    assert!(!w.resize("nope", &[1.0]));
}

#[test]
fn forget_drops_a_panel_for_good() {
    let mut w = editor();
    w.panels.insert(
        "game".into(),
        PanelMeta {
            kind: "view".into(),
            state: "{}".into(),
        },
    );
    let effects = w.forget("game");
    assert_eq!(effects, vec![Effect::Hidden("game".into())]);
    assert!(!w.panels.contains_key("game"));
    w.hide("scene").unwrap();
    assert!(w.forget("scene").is_empty());
    assert!(!w.homes.contains_key("scene"));
}

#[test]
fn a_named_layout_places_what_it_names_and_keeps_the_rest() {
    let default = editor();
    let mut w = default.clone();
    w.hide("inspector").unwrap();
    // "Animate": scene alone in a center stack over a timeline stack.
    let mut center = Stack::with("center", ["scene"]);
    center.center = true;
    let mut named = Workspace::default();
    named.docks.insert(
        "editor".into(),
        DockArea::new(Node::Split(Split {
            axis: Axis::Column,
            id: "x9".into(),
            children: vec![
                Child::grow(Node::Stack(center), 1.0),
                Child::grow(Node::Stack(Stack::with("t1", ["timeline"])), 0.0),
            ],
        })),
    );
    let effects = w.apply_named(&named, &default);
    // Unnamed panels the default shows join the layout's center.
    let center = w.stack("center").unwrap();
    assert!(center.panels.contains(&"game".to_string()));
    assert!(center.panels.contains(&"hierarchy".to_string()));
    assert!(w.is_shown("timeline"));
    assert!(effects.contains(&Effect::Shown("timeline".into())));
    w.check().unwrap();
}

#[test]
fn reconcile_adds_new_panels_and_reports_missing_ones() {
    let default = editor();
    let mut saved = default.clone();
    saved.hide("assets").unwrap();
    // A panel the app no longer has, and the inspector the save never knew.
    saved.stack_mut("center").unwrap().panels.push("old".into());
    saved.forget("inspector");
    let known: BTreeSet<PanelId> = ["hierarchy", "assets", "scene", "game", "inspector"]
        .into_iter()
        .map(String::from)
        .collect();
    let missing = saved.reconcile(&default, &known);
    assert_eq!(missing, vec!["old".to_string()]);
    assert!(
        saved.is_shown("inspector"),
        "a new panel takes its default place"
    );
    assert!(!saved.is_shown("assets"), "a hidden one stays hidden");
    saved.check().unwrap();
    // A dock the save does not know takes the default's tree, and one the
    // page no longer has is dropped.
    let mut other = Workspace::default();
    other.docks.insert(
        "gone".into(),
        DockArea::new(Node::Stack(Stack::with("g", ["x"]))),
    );
    other.reconcile(&default, &known);
    assert!(other.docks.contains_key("editor"));
    assert!(!other.docks.contains_key("gone"));
}

#[test]
fn json_round_trips_and_checks_versions() {
    let mut w = editor();
    w.layout_version = 3;
    w.name = "Default".into();
    w.detach(
        "game",
        WindowPlace {
            rect: Some(Rect {
                x: 1.0,
                y: 2.0,
                w: 3.0,
                h: 4.0,
            }),
            monitor: Some(MonitorKey {
                name: "DP-2".into(),
                origin: [1920, 0],
                scale: 1.0,
            }),
            maximized: true,
            fullscreen: true,
        },
        &docked(),
    )
    .unwrap();
    w.hide("assets").unwrap();
    w.panels.insert(
        "scene-2".into(),
        PanelMeta {
            kind: "scene-view".into(),
            state: "{\"camera\":3}".into(),
        },
    );
    let text = w.to_json();
    assert!(text.contains("\"lumen-dock\": 2"));
    assert!(text.contains("\"rect\": ["));
    assert!(text.contains("\"fullscreen\": true"));
    assert_eq!(Workspace::from_json(&text, 3).unwrap(), w);
    assert_eq!(
        Workspace::from_json(&text, 4),
        Err(RestoreError::LayoutVersion {
            saved: 3,
            current: 4
        })
    );
    let newer = text.replace("\"lumen-dock\": 2", "\"lumen-dock\": 9");
    assert_eq!(
        Workspace::from_json(&newer, 3),
        Err(RestoreError::Format(9))
    );
    assert!(matches!(
        Workspace::from_json("{}", 3),
        Err(RestoreError::NotALayout(_))
    ));
    assert!(matches!(
        Workspace::from_json("not json", 3),
        Err(RestoreError::NotALayout(_))
    ));
    // A layout with a panel twice is refused.
    let twice = text.replacen("\"hierarchy\"", "\"scene\"", 1);
    assert!(matches!(
        Workspace::from_json(&twice, 3),
        Err(RestoreError::Invalid(_))
    ));
    for (e, reason) in [
        (RestoreError::NotALayout("x".into()), "parse"),
        (RestoreError::Format(9), "format"),
        (
            RestoreError::LayoutVersion {
                saved: 1,
                current: 2,
            },
            "version",
        ),
        (RestoreError::Invalid("x".into()), "invalid"),
    ] {
        assert_eq!(e.reason(), reason);
        assert!(!e.to_string().is_empty());
    }
}

#[test]
fn a_layout_file_keeps_named_layouts_beside_the_live_one() {
    let mut live = editor();
    live.layout_version = 2;
    let mut named = live.clone();
    named.name = "Mine".into();
    named.hide("game").unwrap();
    let mut stale = live.clone();
    stale.layout_version = 1;
    let file = LayoutFile {
        live: live.clone(),
        layouts: [
            ("Mine".to_string(), named.clone()),
            ("Old".to_string(), stale),
        ]
        .into_iter()
        .collect(),
    };
    let text = file.to_json();
    let (got, layouts) = LayoutFile::from_json(&text, 2).unwrap();
    assert_eq!(got, Some(live));
    assert_eq!(layouts.len(), 1, "a layout of another version is dropped");
    assert_eq!(layouts["Mine"], named);
    let (got, layouts) = LayoutFile::from_json(&text, 7).unwrap();
    assert!(got.is_none() && layouts.is_empty());
    assert!(LayoutFile::from_json("[]", 2).is_err());
}

#[test]
fn placement_follows_the_monitor_and_stays_on_screen() {
    let monitors = vec![
        Monitor {
            name: "left".into(),
            origin: [0, 0],
            size: [1920, 1080],
            work: ([0, 0], [1920, 1040]),
            scale: 1.0,
        },
        Monitor {
            name: "right".into(),
            origin: [1920, 0],
            size: [2560, 1440],
            work: ([1920, 0], [2560, 1440]),
            scale: 2.0,
        },
    ];
    let key = |name: &str, origin| MonitorKey {
        name: name.into(),
        origin,
        scale: 2.0,
    };
    let rect = Rect {
        x: 100.0,
        y: 50.0,
        w: 400.0,
        h: 300.0,
    };
    // Same monitor by name: the offset scales with it.
    let p = place(
        Some(rect),
        Some(&key("right", [1920, 0])),
        &monitors,
        [800.0, 600.0],
    );
    assert_eq!(p.position, Some([1920 + 200, 100]));
    assert_eq!(p.size, [400.0, 300.0]);
    // Renamed but at the same origin.
    let p = place(
        Some(rect),
        Some(&key("HDMI", [1920, 0])),
        &monitors,
        [800.0, 600.0],
    );
    assert_eq!(p.position, Some([2120, 100]));
    // Gone: centred on the first monitor's work area.
    let p = place(
        Some(rect),
        Some(&key("gone", [9000, 0])),
        &monitors,
        [800.0, 600.0],
    );
    assert_eq!(p.position, Some([(1920 - 400) / 2, (1040 - 300) / 2]));
    // Off the edge: pulled back inside the work area.
    let far = Rect {
        x: 1800.0,
        y: 1000.0,
        ..rect
    };
    let p = place(
        Some(far),
        Some(&key("left", [0, 0])),
        &monitors,
        [800.0, 600.0],
    );
    assert_eq!(p.position, Some([1920 - 400, 1040 - 300]));
    // No rectangle: the fallback size, centred.
    let p = place(None, None, &monitors, [800.0, 600.0]);
    assert_eq!(p.size, [800.0, 600.0]);
    assert_eq!(p.position, Some([560, 220]));
    // No monitors: a size and no position.
    assert_eq!(place(Some(rect), None, &[], [1.0, 1.0]).position, None);

    // Recording reads the offset back on the monitor the window is on.
    let (r, k) = record(Some([2120, 100]), [400.0, 300.0], &monitors, None);
    assert_eq!(r, rect);
    assert_eq!(k.unwrap().name, "right");
    // Unknown position (Wayland): the previous offset and monitor stay.
    let prev = (Some(rect), Some(key("right", [1920, 0])));
    let (r, k) = record(None, [500.0, 300.0], &monitors, Some(prev));
    assert_eq!((r.x, r.y, r.w), (100.0, 50.0, 500.0));
    assert_eq!(k.unwrap().name, "right");
    let (r, k) = record(None, [5.0, 6.0], &monitors, None);
    assert_eq!((r.x, r.w, r.h), (0.0, 5.0, 6.0));
    assert!(k.is_none());
}

#[test]
fn check_reports_each_broken_rule() {
    let mut w = editor();
    w.stack_mut("s1").unwrap().panels.push("scene".into());
    assert!(w.check().unwrap_err().contains("twice"));

    let mut w = editor();
    w.stack_mut("s1").unwrap().active = Some("zzz".into());
    assert!(w.check().is_err());

    let mut w = editor();
    w.windows.push(WindowLayout {
        id: "w1".into(),
        home: None,
        place: WindowPlace::default(),
        root: Node::Stack(Stack::new("e")),
    });
    assert!(w.check().unwrap_err().contains("holds no panel"));

    let mut w = editor();
    w.docks.get_mut("editor").unwrap().maximized = Some("zzz".into());
    assert!(w.check().unwrap_err().contains("maximizes"));

    let mut w = editor();
    w.docks.insert(
        "b".into(),
        DockArea::new(Node::Split(Split {
            axis: Axis::Row,
            id: "lonely".into(),
            children: vec![Child::grow(Node::Stack(Stack::with("q", ["z"])), 1.0)],
        })),
    );
    assert!(w.check().unwrap_err().contains("fewer than two"));

    let mut w = editor();
    w.stack_mut("s1").unwrap().id = "s2".into();
    assert!(w.check().unwrap_err().contains("node id"));
    assert_eq!(TabsMode::parse("never"), Some(TabsMode::Never));
    assert_eq!(TabsMode::parse("always"), Some(TabsMode::Always));
    assert_eq!(TabsMode::parse("auto"), Some(TabsMode::Auto));
    assert_eq!(TabsMode::parse("x"), None);
    assert_eq!(Side::parse("x"), None);
    assert_eq!(Where::Hidden.kind(), "hidden");
    assert_eq!(w.active_of("game").map(String::as_str), Some("scene"));
    assert_eq!(w.stacks_of(&Container::Dock("editor".into())).len(), 4);
    assert!(w.stacks_of(&Container::Window("none".into())).is_empty());
}
