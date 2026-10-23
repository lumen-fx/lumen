//! `<split>`: panes side by side with a draggable gutter between each pair.
//!
//! A `.split-gutter` goes between every two children of a `<split>`: the
//! parser puts them in for markup, and the scene keeps them in step with the
//! split's children however those were added, by a script, by a module's
//! systems or by the parser. A gutter is a focusable separator,
//! as the web's window splitter pattern describes one: dragging it with the
//! primary button, or the arrow keys while it is focused, moves the boundary
//! between the two panes beside it, and Home and End give the pane before it
//! its smallest and its largest size. The panes lay out along the split's
//! own `flex-direction`, a row unless a style says otherwise, and each keeps
//! within its own `min-width` / `max-width` (`min-height` / `max-height` in
//! a column).
//!
//! A resize holds the panes' `flex-basis` in [`StyleOverrides`]: the first
//! one holds every pane at the size it has, so the rest of the split keeps
//! its place, and each step then moves size from one neighbour to the
//! other. The panes' `flex-grow` and `flex-shrink` stay as styled, so when
//! the split itself grows or shrinks the panes share the difference as
//! their styles say.
//!
//! What a resize starts from is the split's measured layout,
//! [`Split::panes`], which the platform keeps: on the desktop
//! [`measure_splits`] reads it off layout after every solve; a page's DOM
//! backend reads it off the browser's boxes. Everything else here runs the
//! same on both.
//!
//! A module or an app's Rust reads and sets the pane sizes through the
//! [`SplitSizes`] system param; a resize the user makes ends with a
//! [`SplitResized`] message, once a gutter drag is released and after each
//! key that moved a gutter. Every split system runs in [`SplitSet`].
//!
//! Each gutter carries the separator's value the way the web pattern gives
//! it: `aria-valuenow` is the share of its two panes the one before it
//! takes, in percent, between `aria-valuemin` and `aria-valuemax`, the
//! shares the panes' limits allow, and `aria-orientation` is `vertical` for
//! a gutter between panes side by side. Beside `split-gutter`, a gutter
//! carries `split-gutter-row` in a split laid out as a row and
//! `split-gutter-column` in one laid out as a column, which is what the
//! user-agent sheet keys its `col-resize` and `row-resize` cursors on.

use bevy_ecs::message::{Message, MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemParam;
use lumen_core::components::{
    A11yValue, BoxSizing, FlexDirection, Length, LumenAttributes, LumenClasses, Style, StyleDirty,
};
use lumen_core::input::FocusedKey;
use lumen_core::prelude::*;
use lumen_core::style_overrides::{Live, StyleOverrides, StyleOwner, StyleValues};

/// How far one arrow-key press moves a focused gutter, in logical pixels.
pub const SPLIT_KEY_STEP_PX: f32 = 10.0;

/// The direction a split lays its panes out in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SplitAxis {
    /// Side by side, from `flex-direction: row`; the gutters stand upright.
    #[default]
    Row,
    /// One above the other, from `flex-direction: column`; the gutters lie
    /// flat.
    Column,
}

impl SplitAxis {
    /// The axis a `flex-direction` lays out along.
    pub fn of(direction: FlexDirection) -> Self {
        match direction {
            FlexDirection::Row | FlexDirection::RowReverse => Self::Row,
            FlexDirection::Column | FlexDirection::ColumnReverse => Self::Column,
        }
    }

    /// The component of `v` along this axis.
    pub fn along(self, v: glam::Vec2) -> f32 {
        match self {
            Self::Row => v.x,
            Self::Column => v.y,
        }
    }

    /// The `aria-orientation` of a gutter between panes laid out this way.
    pub fn orientation(self) -> &'static str {
        match self {
            Self::Row => "vertical",
            Self::Column => "horizontal",
        }
    }

    /// The class a gutter of a split laid out this way carries beside
    /// `split-gutter`, so a stylesheet can tell the two apart: the
    /// user-agent sheet keys the gutter's resize cursor on it.
    pub fn gutter_class(self) -> &'static str {
        match self {
            Self::Row => "split-gutter-row",
            Self::Column => "split-gutter-column",
        }
    }
}

/// One pane of a split, as last measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitPaneGeometry {
    /// The pane.
    pub entity: Entity,
    /// Where it starts along the split's axis, in window coordinates.
    pub start: f32,
    /// Its outer size along the axis.
    pub size: f32,
    /// The smallest size its styles allow.
    pub min: f32,
    /// The largest size its styles allow.
    pub max: f32,
    /// Padding and border along the axis when its `box-sizing` is
    /// `content-box`, which a `flex-basis` does not count; else 0.
    pub frame: f32,
}

/// On a `<split>`: the panes and gutters it holds, as last measured.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct Split {
    /// The axis the panes lay out along.
    pub axis: SplitAxis,
    /// The panes, in document order.
    pub panes: Vec<SplitPaneGeometry>,
    /// The gutters, in document order: gutter `i` sits between pane `i`
    /// and pane `i + 1`.
    pub gutters: Vec<Entity>,
}

impl Split {
    /// Each pane's outer size along the axis, in document order, as last
    /// measured. Empty until the split has been laid out once.
    pub fn sizes(&self) -> Vec<f32> {
        self.panes.iter().map(|p| p.size).collect()
    }

    /// Which gutter `gutter` is, counted from the first.
    pub fn gutter_index(&self, gutter: Entity) -> Option<usize> {
        self.gutters.iter().position(|g| *g == gutter)
    }

    /// The panes on each side of gutter `index`, when both were measured.
    fn neighbours(&self, index: usize) -> Option<(&SplitPaneGeometry, &SplitPaneGeometry)> {
        Some((self.panes.get(index)?, self.panes.get(index + 1)?))
    }

    /// The sizes the pane before gutter `index` may take, given that it and
    /// the pane after share `sum` between them: from the most the pane
    /// after lets go to the least it keeps.
    fn range(&self, index: usize, sum: f32) -> Option<(f32, f32)> {
        let (a, b) = self.neighbours(index)?;
        let lo = a.min.max(sum - b.max);
        let hi = a.max.min(sum - b.min);
        Some(if lo > hi { (lo, lo) } else { (lo, hi) })
    }

    /// The share of its two panes the pane before gutter `index` takes, in
    /// percent, with the least and the most it may take: the gutter's
    /// `aria-valuenow`, `aria-valuemin` and `aria-valuemax`.
    pub fn gutter_value(&self, index: usize, sizes: &[f32]) -> Option<A11yValue> {
        let a = *sizes.get(index)?;
        let b = *sizes.get(index + 1)?;
        let sum = a + b;
        if sum <= 0.0 {
            return None;
        }
        let (lo, hi) = self.range(index, sum)?;
        let pct = |v: f32| (f64::from(v / sum) * 100.0).round();
        Some(A11yValue {
            now: pct(a),
            min: pct(lo),
            max: pct(hi),
            step: 0.0,
            text: None,
        })
    }

    /// Whether the pane before gutter `index` sits after the pane following
    /// it on screen (a reversed or right-to-left row), so moving the gutter
    /// forward shrinks it.
    fn reversed(&self, index: usize) -> bool {
        self.neighbours(index)
            .is_some_and(|(a, b)| a.start > b.start)
    }
}

/// On a pane of a `<split>`: what holds its `flex-basis` while the split is
/// resized.
#[derive(Component, Clone, Copy, Debug, Default)]
#[require(StyleOverrides)]
pub struct SplitPane;

impl StyleOwner for SplitPane {
    const RANK: u8 = 20;
}

/// On a `.split-gutter`, the separator between two panes.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SplitGutter;

/// The system set every `<split>` system runs in. Order a system that reads
/// a [`SplitResized`] after it.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct SplitSet;

/// A resize the user made has ended: a gutter drag was released after it
/// moved the gutter, or a key moved a focused gutter. `sizes` are the pane
/// sizes it left, as in [`Split::sizes`]. Not written for sizes set through
/// [`SplitSizes`], nor when the split itself changes size.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct SplitResized {
    /// The `<split>`.
    pub split: Entity,
    /// Each pane's outer size along the axis, in document order.
    pub sizes: Vec<f32>,
}

/// What a gutter move asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GutterMove {
    /// Move it by this much along the axis, on screen: positive is right or
    /// down.
    By(f32),
    /// Give the pane before it its smallest size.
    Start,
    /// Give the pane before it its largest size.
    End,
}

/// The pane sizes after moving gutter `index` from `sizes`. Only its two
/// neighbours change, and they keep their sum.
pub fn moved_sizes(split: &Split, sizes: &[f32], index: usize, how: GutterMove) -> Vec<f32> {
    let mut out = sizes.to_vec();
    let (Some(&a), Some(&b)) = (sizes.get(index), sizes.get(index + 1)) else {
        return out;
    };
    let sum = a + b;
    let Some((lo, hi)) = split.range(index, sum) else {
        return out;
    };
    let next = match how {
        GutterMove::By(d) => {
            let grow = if split.reversed(index) { -d } else { d };
            (a + grow).clamp(lo, hi)
        }
        GutterMove::Start => lo,
        GutterMove::End => hi,
    };
    out[index] = next;
    out[index + 1] = sum - next;
    out
}

/// What a resize writes: the panes' held basis, and the gutters' separator
/// values.
#[derive(SystemParam)]
pub struct SplitWrites<'w, 's> {
    panes: Query<'w, 's, (&'static mut Style, &'static mut StyleOverrides), With<SplitPane>>,
    gutters: GutterValues<'w, 's>,
}

/// The gutters' separator values, and the commands that add them to a
/// gutter that has none yet.
#[derive(SystemParam)]
pub struct GutterValues<'w, 's> {
    values: Query<
        'w,
        's,
        (
            Option<&'static mut A11yValue>,
            Option<&'static mut LumenAttributes>,
        ),
        With<SplitGutter>,
    >,
    commands: Commands<'w, 's>,
}

/// Hold every pane of `split` at `sizes`, and give each gutter the value
/// they leave it.
fn hold_sizes(split: &Split, sizes: &[f32], writes: &mut SplitWrites) {
    for (pane, size) in split.panes.iter().zip(sizes) {
        let Ok((mut style, mut overrides)) = writes.panes.get_mut(pane.entity) else {
            continue;
        };
        let basis = Length::Px((size - pane.frame).max(0.0));
        if overrides
            .held_by::<SplitPane>()
            .is_some_and(|v| v.basis == Some(basis))
        {
            continue;
        }
        let out = overrides.hold::<SplitPane>(
            StyleValues {
                basis: Some(basis),
                ..StyleValues::default()
            },
            &Live {
                style: Some(&style),
                ..Live::default()
            },
        );
        out.write_style(&mut style);
    }
    write_gutter_values(split, sizes, &mut writes.gutters);
}

/// Give each gutter of `split` the separator value `sizes` leave it.
fn write_gutter_values(split: &Split, sizes: &[f32], gutters: &mut GutterValues) {
    for (index, gutter) in split.gutters.iter().enumerate() {
        let Ok((value, attrs)) = gutters.values.get_mut(*gutter) else {
            continue;
        };
        let Some(want) = split.gutter_value(index, sizes) else {
            continue;
        };
        let pairs = [
            ("aria-valuenow", format!("{}", want.now)),
            ("aria-valuemin", format!("{}", want.min)),
            ("aria-valuemax", format!("{}", want.max)),
            ("aria-orientation", split.axis.orientation().to_string()),
        ];
        match attrs {
            Some(mut attrs) => {
                if pairs.iter().any(|(k, v)| attrs.get(k) != Some(v.as_str())) {
                    for (k, v) in &pairs {
                        attrs.set(k, v.clone());
                    }
                }
            }
            None => {
                let mut attrs = LumenAttributes::default();
                for (k, v) in &pairs {
                    attrs.set(k, v.clone());
                }
                gutters.commands.entity(*gutter).insert(attrs);
            }
        }
        match value {
            Some(mut value) if *value != want => *value = want,
            Some(_) => {}
            None => {
                gutters.commands.entity(*gutter).insert(want);
            }
        }
    }
}

/// The split a gutter sits in, and which gutter it is.
fn split_of(
    gutter: Entity,
    parents: &Query<&ChildOf>,
    splits: &Query<&Split>,
) -> Option<(Entity, usize)> {
    let parent = parents.get(gutter).ok()?.parent();
    let index = splits.get(parent).ok()?.gutter_index(gutter)?;
    Some((parent, index))
}

/// A gutter drag in flight: the split, the gutter, where the pointer went
/// down along the axis, the pane sizes it started from, and the sizes its
/// last move held, if it has moved.
#[derive(Clone, Debug)]
pub struct GutterDrag {
    split: Entity,
    index: usize,
    origin: f32,
    sizes: Vec<f32>,
    held: Option<Vec<f32>>,
}

/// Drag gutters with the primary button: a press on a gutter takes it, and
/// every move until the release moves it to follow the pointer, wherever
/// the pointer goes. A release after the gutter moved writes a
/// [`SplitResized`].
#[allow(clippy::too_many_arguments)]
pub fn drag_split_gutters(
    (mut presses, mut moves, mut releases): (
        MessageReader<PointerPressed>,
        MessageReader<PointerMoved>,
        MessageReader<PointerReleased>,
    ),
    hovered: Query<Entity, With<Hovered>>,
    parents: Query<&ChildOf>,
    splits: Query<&Split>,
    marks: Query<(), With<SplitGutter>>,
    mut writes: SplitWrites,
    mut resized: MessageWriter<SplitResized>,
    mut drag: Local<Option<GutterDrag>>,
) {
    for press in presses.read() {
        if !press.is_primary() {
            continue;
        }
        *drag = hovered
            .iter()
            .find(|e| marks.contains(*e))
            .and_then(|g| split_of(g, &parents, &splits))
            .and_then(|(split, index)| {
                let s = splits.get(split).ok()?;
                Some(GutterDrag {
                    split,
                    index,
                    origin: s.axis.along(press.position),
                    sizes: s.sizes(),
                    held: None,
                })
            });
    }
    let last = moves
        .read()
        .filter(|m| m.pointer.is_primary)
        .last()
        .map(|m| m.position);
    if let (Some(d), Some(at)) = (drag.as_mut(), last)
        && let Ok(split) = splits.get(d.split)
        && split.panes.len() == d.sizes.len()
    {
        let by = split.axis.along(at) - d.origin;
        let sizes = moved_sizes(split, &d.sizes, d.index, GutterMove::By(by));
        hold_sizes(split, &sizes, &mut writes);
        d.held = Some(sizes);
    }
    if releases.read().any(|r| r.is_primary())
        && let Some(d) = drag.take()
        && let Some(sizes) = d.held.filter(|held| *held != d.sizes)
    {
        resized.write(SplitResized {
            split: d.split,
            sizes,
        });
    }
}

/// The arrow keys along the split's axis move a focused gutter by
/// [`SPLIT_KEY_STEP_PX`], and Home and End give the pane before it its
/// smallest and largest size. Each key that moves the gutter writes a
/// [`SplitResized`].
pub fn step_split_on_keys(
    mut keys: MessageReader<FocusedKey>,
    parents: Query<&ChildOf>,
    splits: Query<&Split>,
    marks: Query<(), With<SplitGutter>>,
    mut writes: SplitWrites,
    mut resized: MessageWriter<SplitResized>,
) {
    for key in keys.read() {
        if !marks.contains(key.entity) {
            continue;
        }
        let Some((split_e, index)) = split_of(key.entity, &parents, &splits) else {
            continue;
        };
        let Ok(split) = splits.get(split_e) else {
            continue;
        };
        let how = match (split.axis, &key.key) {
            (SplitAxis::Row, Key::Named(NamedKey::ArrowLeft))
            | (SplitAxis::Column, Key::Named(NamedKey::ArrowUp)) => {
                GutterMove::By(-SPLIT_KEY_STEP_PX)
            }
            (SplitAxis::Row, Key::Named(NamedKey::ArrowRight))
            | (SplitAxis::Column, Key::Named(NamedKey::ArrowDown)) => {
                GutterMove::By(SPLIT_KEY_STEP_PX)
            }
            (_, Key::Named(NamedKey::Home)) => GutterMove::Start,
            (_, Key::Named(NamedKey::End)) => GutterMove::End,
            _ => continue,
        };
        // A key press since the last solve starts from what the last one
        // held, so presses in one tick add up.
        let sizes = held_sizes(split, &writes);
        let next = moved_sizes(split, &sizes, index, how);
        if next != sizes {
            hold_sizes(split, &next, &mut writes);
            resized.write(SplitResized {
                split: split_e,
                sizes: next,
            });
        }
    }
}

/// The size each pane of `split` is held at, or its measured size where
/// nothing holds it.
fn held_sizes(split: &Split, writes: &SplitWrites) -> Vec<f32> {
    split
        .panes
        .iter()
        .map(|p| {
            writes
                .panes
                .get(p.entity)
                .ok()
                .and_then(|(_, ov)| ov.held_by::<SplitPane>()?.basis)
                .and_then(|b| match b {
                    Length::Px(v) => Some(v + p.frame),
                    _ => None,
                })
                .unwrap_or(p.size)
        })
        .collect()
}

/// Reading and setting a split's pane sizes from a system.
#[derive(SystemParam)]
pub struct SplitSizes<'w, 's> {
    splits: Query<'w, 's, &'static Split>,
    writes: SplitWrites<'w, 's>,
}

impl SplitSizes<'_, '_> {
    /// Each pane's outer size along the axis, in document order, as last
    /// laid out; `None` for an entity that is not a split, and empty before
    /// the split's first layout.
    pub fn get(&self, split: Entity) -> Option<Vec<f32>> {
        self.splits.get(split).ok().map(Split::sizes)
    }

    /// Hold the panes of `split` at `sizes`: each pane's outer size along the
    /// axis, in document order, in logical pixels. A pane's own `min-width`
    /// and `max-width` (`min-height` and `max-height` in a column) still bound
    /// it, and a size missing from the end of the list keeps the pane's
    /// size. Returns false, and holds nothing, for an entity that is not a
    /// split or a split not laid out yet: set the sizes once
    /// [`get`](Self::get) reports some.
    pub fn set(&mut self, split: Entity, sizes: &[f32]) -> bool {
        let Ok(split) = self.splits.get(split) else {
            return false;
        };
        if split.panes.is_empty() {
            return false;
        }
        let mut held = split.sizes();
        for (size, want) in held.iter_mut().zip(sizes) {
            *size = want.max(0.0);
        }
        hold_sizes(split, &held, &mut self.writes);
        true
    }
}

/// Keep each gutter's separator value in step with its split's measured
/// layout.
pub fn sync_split_gutters(splits: Query<&Split, Changed<Split>>, mut gutters: GutterValues) {
    for split in &splits {
        let sizes: Vec<f32> = split.panes.iter().map(|p| p.size).collect();
        write_gutter_values(split, &sizes, &mut gutters);
    }
}

/// Give each gutter the class of its split's axis, and only that one:
/// [`SplitAxis::gutter_class`]. It follows the split's measured axis, so a
/// style that turns a row into a column moves the class, and it comes back
/// after a script rewrites the gutter's class list. A changed list restyles
/// the gutter.
pub fn mark_split_gutter_axes(
    splits: Query<Ref<Split>>,
    mut gutters: Query<(Entity, &ChildOf, &mut LumenClasses), With<SplitGutter>>,
    mut commands: Commands,
) {
    let axis_class =
        |c: &str| c == SplitAxis::Row.gutter_class() || c == SplitAxis::Column.gutter_class();
    for (gutter, child_of, mut classes) in &mut gutters {
        let Ok(split) = splits.get(child_of.parent()) else {
            continue;
        };
        if split.panes.is_empty() || (!split.is_changed() && !classes.is_changed()) {
            continue;
        }
        let want = split.axis.gutter_class();
        let stale = |c: &str| c != want && axis_class(c);
        let has_want = classes.has(want);
        if has_want && !classes.0.iter().any(|c| stale(c)) {
            continue;
        }
        classes.0.retain(|c| !stale(c));
        if !has_want {
            classes.0.push(want.into());
        }
        commands.entity(gutter).insert(StyleDirty);
    }
}

/// A length as pixels along an axis `of` long, `None` for `auto`.
fn resolve(len: Length, of: f32) -> Option<f32> {
    match len {
        Length::Px(v) => Some(v),
        Length::Percent(p) => Some(p / 100.0 * of),
        Length::Auto => None,
    }
}

/// Read each split's panes and gutters off layout: the desktop's source of
/// [`Split::panes`]. Runs after the layout solve; writes only what changed.
pub fn measure_splits(
    mut splits: Query<(&mut Split, &Style, &Transform, &Children)>,
    panes: Query<(&Style, &Transform), With<SplitPane>>,
    marks: Query<(), With<SplitGutter>>,
) {
    for (mut split, style, frame, children) in &mut splits {
        let axis = SplitAxis::of(style.flex_direction);
        let length = axis.along(frame.size);
        let mut measured = Split {
            axis,
            ..Split::default()
        };
        for &child in children {
            if marks.contains(child) {
                measured.gutters.push(child);
                continue;
            }
            let Ok((s, t)) = panes.get(child) else {
                continue;
            };
            let (min, max) = match axis {
                SplitAxis::Row => (s.min_width, s.max_width),
                SplitAxis::Column => (s.min_height, s.max_height),
            };
            let frame = match (s.box_sizing, axis) {
                (BoxSizing::BorderBox, _) => 0.0,
                (BoxSizing::ContentBox, SplitAxis::Row) => {
                    s.padding.left + s.padding.right + s.border.left + s.border.right
                }
                (BoxSizing::ContentBox, SplitAxis::Column) => {
                    s.padding.top + s.padding.bottom + s.border.top + s.border.bottom
                }
            };
            measured.panes.push(SplitPaneGeometry {
                entity: child,
                start: axis.along(t.absolute),
                size: axis.along(t.size),
                min: resolve(min, length).unwrap_or(0.0).max(0.0),
                max: resolve(max, length).unwrap_or(f32::INFINITY),
                frame,
            });
        }
        if *split != measured {
            *split = measured;
        }
    }
}

/// Plugin: gutter dragging, gutter keys, and the gutters' separator values,
/// on every platform. The platform measures the splits: the desktop with
/// [`measure_splits`], a page from the browser's boxes.
pub struct SplitPlugin;

impl Plugin for SplitPlugin {
    fn build(self, app: &mut App) {
        app.add_message::<SplitResized>();
        app.add_systems(
            TickStage::Systems,
            (
                drag_split_gutters.after(lumen_input::hit_test),
                step_split_on_keys.after(lumen_input::dispatch_focused_keys),
                sync_split_gutters,
                mark_split_gutter_axes,
            )
                .chain()
                .in_set(SplitSet)
                .run_if(any_with_component::<Split>),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;

    fn pane(entity: Entity, start: f32, size: f32, min: f32) -> SplitPaneGeometry {
        SplitPaneGeometry {
            entity,
            start,
            size,
            min,
            max: f32::INFINITY,
            frame: 0.0,
        }
    }

    /// Moving a gutter moves size between its two neighbours only, keeps
    /// their sum, and stops at the limits either one sets.
    #[test]
    fn a_move_trades_size_between_neighbours_within_their_limits() {
        let mut world = World::new();
        let [a, b, c] = [(); 3].map(|_| world.spawn_empty().id());
        let split = Split {
            axis: SplitAxis::Row,
            panes: vec![pane(a, 0.0, 200.0, 50.0), pane(b, 206.0, 300.0, 120.0), {
                pane(c, 512.0, 100.0, 0.0)
            }],
            gutters: Vec::new(),
        };
        let sizes = [200.0, 300.0, 100.0];
        assert_eq!(
            moved_sizes(&split, &sizes, 0, GutterMove::By(40.0)),
            vec![240.0, 260.0, 100.0]
        );
        assert_eq!(
            moved_sizes(&split, &sizes, 0, GutterMove::By(1000.0)),
            vec![380.0, 120.0, 100.0],
            "the pane after keeps its min-width"
        );
        assert_eq!(
            moved_sizes(&split, &sizes, 0, GutterMove::Start),
            vec![50.0, 450.0, 100.0]
        );
        assert_eq!(
            moved_sizes(&split, &sizes, 1, GutterMove::End),
            vec![200.0, 400.0, 0.0]
        );
        let value = split.gutter_value(0, &sizes).unwrap();
        assert_eq!((value.now, value.min, value.max), (40.0, 10.0, 76.0));
    }

    /// In a reversed row the pane before a gutter sits right of it, so a
    /// move to the right shrinks it.
    #[test]
    fn a_reversed_row_moves_the_other_way() {
        let mut world = World::new();
        let [a, b] = [(); 2].map(|_| world.spawn_empty().id());
        let split = Split {
            axis: SplitAxis::Row,
            panes: vec![pane(a, 306.0, 200.0, 0.0), pane(b, 0.0, 300.0, 0.0)],
            gutters: Vec::new(),
        };
        assert_eq!(
            moved_sizes(&split, &[200.0, 300.0], 0, GutterMove::By(40.0)),
            vec![160.0, 340.0]
        );
    }

    fn split_world() -> (World, Entity, [Entity; 2], Entity) {
        let mut world = World::new();
        world.init_resource::<Messages<PointerPressed>>();
        world.init_resource::<Messages<PointerMoved>>();
        world.init_resource::<Messages<PointerReleased>>();
        world.init_resource::<Messages<FocusedKey>>();
        world.init_resource::<Messages<SplitResized>>();
        let split = world.spawn_empty().id();
        let a = world
            .spawn((SplitPane, Style::default(), ChildOf(split)))
            .id();
        let gutter = world.spawn((SplitGutter, ChildOf(split))).id();
        let b = world
            .spawn((SplitPane, Style::default(), ChildOf(split)))
            .id();
        world.entity_mut(split).insert(Split {
            axis: SplitAxis::Row,
            panes: vec![pane(a, 0.0, 200.0, 50.0), pane(b, 206.0, 300.0, 0.0)],
            gutters: vec![gutter],
        });
        (world, split, [a, b], gutter)
    }

    /// A gutter carries the class of its split's axis and only that one,
    /// follows the axis when it changes, and gets it back after its class
    /// list is rewritten; each change restyles it.
    #[test]
    fn a_gutter_carries_its_split_axis_as_a_class() {
        let (mut world, split, _, gutter) = split_world();
        world
            .entity_mut(gutter)
            .insert(LumenClasses(vec!["split-gutter".into()]));
        let classes = |world: &World| -> Vec<String> {
            world
                .get::<LumenClasses>(gutter)
                .unwrap()
                .0
                .iter()
                .map(|c| c.to_string())
                .collect()
        };
        world.run_system_once(mark_split_gutter_axes).unwrap();
        assert_eq!(classes(&world), ["split-gutter", "split-gutter-row"]);
        assert!(world.get::<StyleDirty>(gutter).is_some());
        world.entity_mut(gutter).remove::<StyleDirty>();

        world.run_system_once(mark_split_gutter_axes).unwrap();
        assert!(
            world.get::<StyleDirty>(gutter).is_none(),
            "nothing changed, nothing restyles"
        );

        world.get_mut::<Split>(split).unwrap().axis = SplitAxis::Column;
        world.run_system_once(mark_split_gutter_axes).unwrap();
        assert_eq!(classes(&world), ["split-gutter", "split-gutter-column"]);
        assert!(world.get::<StyleDirty>(gutter).is_some());

        world
            .entity_mut(gutter)
            .insert(LumenClasses(vec!["split-gutter".into(), "mine".into()]));
        world.run_system_once(mark_split_gutter_axes).unwrap();
        assert_eq!(
            classes(&world),
            ["split-gutter", "mine", "split-gutter-column"]
        );
    }

    fn basis(world: &World, e: Entity) -> Length {
        world.get::<Style>(e).unwrap().basis
    }

    /// A press on a gutter and a move hold both panes' `flex-basis` where
    /// the pointer took the gutter, and the gutter's separator value follows.
    #[test]
    fn dragging_a_gutter_holds_the_neighbours_basis() {
        let (mut world, _split, [a, b], gutter) = split_world();
        world.entity_mut(gutter).insert(Hovered);
        world.write_message(PointerPressed {
            position: Vec2::new(203.0, 50.0),
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        });
        world.write_message(PointerMoved {
            position: Vec2::new(253.0, 80.0),
            local: None,
            pointer: Default::default(),
        });
        world.run_system_once(drag_split_gutters).unwrap();
        assert_eq!(basis(&world, a), Length::Px(250.0));
        assert_eq!(basis(&world, b), Length::Px(250.0));
        let attrs = world.get::<LumenAttributes>(gutter).unwrap();
        assert_eq!(attrs.get("aria-valuenow"), Some("50"));
        assert_eq!(attrs.get("aria-orientation"), Some("vertical"));
        assert_eq!(world.get::<A11yValue>(gutter).unwrap().now, 50.0);
    }

    fn resized(world: &World) -> Vec<SplitResized> {
        world
            .resource::<Messages<SplitResized>>()
            .iter_current_update_messages()
            .cloned()
            .collect()
    }

    /// A drag reports the sizes it left once, when the pointer is released,
    /// and a press and release that never moved the gutter reports nothing.
    #[test]
    fn a_released_drag_reports_the_sizes_it_left() {
        let (mut world, split, [a, _b], gutter) = split_world();
        world.entity_mut(gutter).insert(Hovered);
        let press = PointerPressed {
            position: Vec2::new(203.0, 50.0),
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        };
        let release = PointerReleased {
            position: Vec2::new(233.0, 50.0),
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        };
        // One system for the whole gesture, so its drag state lasts from the
        // press to the release.
        let drag = world.register_system(drag_split_gutters);
        world.write_message(press);
        world.write_message(PointerMoved {
            position: Vec2::new(233.0, 50.0),
            local: None,
            pointer: Default::default(),
        });
        world.run_system(drag).unwrap();
        assert!(resized(&world).is_empty(), "nothing ends while dragging");
        world.write_message(release);
        world.run_system(drag).unwrap();
        assert_eq!(
            resized(&world),
            vec![SplitResized {
                split,
                sizes: vec![230.0, 270.0]
            }]
        );
        assert_eq!(basis(&world, a), Length::Px(230.0));

        world.resource_mut::<Messages<SplitResized>>().clear();
        world.write_message(press);
        world.run_system(drag).unwrap();
        world.write_message(release);
        world.run_system(drag).unwrap();
        assert!(
            resized(&world).is_empty(),
            "a click on a gutter resizes nothing"
        );
    }

    /// Setting sizes holds every pane it names and keeps the size of the
    /// rest; a split not laid out yet takes none.
    #[test]
    fn set_sizes_hold_the_panes_once_measured() {
        let (mut world, split, [a, b], _gutter) = split_world();
        let panes = std::mem::take(&mut world.get_mut::<Split>(split).unwrap().panes);
        let set = move |world: &mut World| {
            world
                .run_system_once(move |mut sizes: SplitSizes| sizes.set(split, &[120.0]))
                .unwrap()
        };
        assert!(!set(&mut world), "not laid out, so nothing to hold");
        assert_eq!(basis(&world, a), Length::Auto);

        world.get_mut::<Split>(split).unwrap().panes = panes;
        assert!(set(&mut world));
        assert_eq!(basis(&world, a), Length::Px(120.0));
        assert_eq!(basis(&world, b), Length::Px(300.0), "the rest keep theirs");
        let read = world
            .run_system_once(move |sizes: SplitSizes| sizes.get(split))
            .unwrap();
        assert_eq!(
            read,
            Some(vec![200.0, 300.0]),
            "what the layout last measured"
        );
        let gutter = world
            .run_system_once(move |sizes: SplitSizes| sizes.get(_gutter))
            .unwrap();
        assert_eq!(gutter, None, "a gutter is not a split");
    }

    /// Arrow keys step a focused gutter and Home takes the pane before it to
    /// its min-width; keys on anything else do nothing.
    #[test]
    fn keys_step_a_focused_gutter() {
        let (mut world, _split, [a, b], gutter) = split_world();
        for (entity, key) in [
            (gutter, NamedKey::ArrowRight),
            (gutter, NamedKey::ArrowRight),
            (a, NamedKey::ArrowLeft),
        ] {
            world.write_message(FocusedKey {
                entity,
                key: Key::Named(key),
                code: lumen_core::input::KeyCode::Unidentified,
                modifiers: Modifiers::default(),
                repeat: false,
            });
        }
        world.run_system_once(step_split_on_keys).unwrap();
        assert_eq!(basis(&world, a), Length::Px(220.0));
        assert_eq!(basis(&world, b), Length::Px(280.0));
        let ends: Vec<Vec<f32>> = resized(&world).into_iter().map(|r| r.sizes).collect();
        assert_eq!(
            ends,
            vec![vec![210.0, 290.0], vec![220.0, 280.0]],
            "each key that moves the gutter ends a resize"
        );
        world.resource_mut::<Messages<FocusedKey>>().clear();
        world.write_message(FocusedKey {
            entity: gutter,
            key: Key::Named(NamedKey::Home),
            code: lumen_core::input::KeyCode::Unidentified,
            modifiers: Modifiers::default(),
            repeat: false,
        });
        world.run_system_once(step_split_on_keys).unwrap();
        assert_eq!(basis(&world, a), Length::Px(50.0));
        assert_eq!(basis(&world, b), Length::Px(450.0));
    }

    /// Measuring reads the axis from `flex-direction`, the panes' sizes off
    /// layout and their limits from their styles, leaving gutters out.
    #[test]
    fn measuring_reads_layout_and_limits() {
        let mut world = World::new();
        let split = world
            .spawn((
                Split::default(),
                Style {
                    flex_direction: FlexDirection::Column,
                    ..Style::default()
                },
                Transform::new(Vec2::ZERO, Vec2::new(400.0, 600.0)),
            ))
            .id();
        let a = world
            .spawn((
                SplitPane,
                Style {
                    min_height: Length::Percent(10.0),
                    max_height: Length::Px(400.0),
                    ..Style::default()
                },
                Transform::new(Vec2::ZERO, Vec2::new(400.0, 300.0)),
                ChildOf(split),
            ))
            .id();
        let gutter = world.spawn((SplitGutter, ChildOf(split))).id();
        world.spawn((
            SplitPane,
            Style::default(),
            Transform::new(Vec2::new(0.0, 306.0), Vec2::new(400.0, 294.0)),
            ChildOf(split),
        ));
        world.run_system_once(measure_splits).unwrap();
        let measured = world.get::<Split>(split).unwrap();
        assert_eq!(measured.axis, SplitAxis::Column);
        assert_eq!(measured.gutters, vec![gutter]);
        assert_eq!(measured.panes.len(), 2);
        assert_eq!(measured.panes[0].entity, a);
        assert_eq!(
            (
                measured.panes[0].size,
                measured.panes[0].min,
                measured.panes[0].max
            ),
            (300.0, 60.0, 400.0)
        );
        assert_eq!(measured.panes[1].start, 306.0);
    }
}
