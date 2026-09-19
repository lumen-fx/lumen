//! Layout solving with an exact measure memo.
//!
//! taffy stores intrinsic-size results in nine fixed slots per node, picked
//! from how many dimensions are known and whether the free axis is under a
//! min-content or max-content constraint. Two queries that land in the same
//! slot but differ in some other part of the key evict each other, and the
//! evicted result is recomputed the next time it is asked for.
//!
//! Nested column containers with content-derived widths ask for exactly such
//! a pair at every level: once with the parent's width known (the stretched
//! path) and once with it unknown (the hypothetical-cross-size path that
//! flexbox needs before it can size the line). The two answers share a slot
//! and differ only in the parent size recorded in the key, so each level
//! recomputes both of its children's answers, and the work doubles per level.
//! A twenty-deep document takes tens of seconds; a row-direction document of
//! the same shape takes milliseconds, because its queries happen to spread
//! across distinct slots.
//!
//! [`compute_layout_memoised`] runs taffy's own algorithms over its own tree
//! through the public [`taffy::LayoutPartialTree`] family, with one change:
//! intrinsic-size results are also kept in a per-solve map keyed on the whole
//! layout input, so a repeated query is answered instead of recomputed. Every
//! entry it returns is one taffy stored under the identical key during the
//! same solve, so the geometry is the same as taffy's own solver produces.
//! Full-layout (`RunMode::PerformLayout`) results keep taffy's single-entry
//! behaviour, since replaying one of those skips the descendant writes it
//! performed the first time.
//!
//! Node styles, children and contexts stay in the [`taffy::TaffyTree`]. The
//! computed boxes land in a [`Geometry`] map here, because taffy has no
//! public setter for a node's own layout.

use std::collections::HashMap;

use taffy::compute::compute_block_layout;
use taffy::prelude::*;
use taffy::tree::Layout;
use taffy::{
    BlockContext, CacheTree, LayoutBlockContainer, LayoutFlexboxContainer, LayoutGridContainer,
    LayoutInput, LayoutOutput, LayoutPartialTree, RequestedAxis, RoundTree, RunMode, SizingMode,
    TraversePartialTree, TraverseTree, compute_cached_layout, compute_flexbox_layout,
    compute_grid_layout, compute_hidden_layout, compute_leaf_layout, compute_root_layout,
    round_layout,
};

/// Every field of a [`LayoutInput`] that changes the answer, packed into a
/// hashable key. Floats go in as bit patterns: layout inputs are reproduced
/// exactly from one query to the next, so bitwise equality is the right test
/// and `NaN` never reaches here.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MeasureKey {
    /// Known width / height.
    known: (u32, u32),
    /// Parent size, which percentage lengths resolve against.
    parent: (u32, u32),
    /// Available space per axis.
    available: (u32, u32),
    /// Requested axis, sizing mode and the two margin-collapse flags.
    flags: u8,
}

/// Bit pattern for an optional length; `None` maps to a value no finite
/// `f32` produces.
fn known_bits(v: Option<f32>) -> u32 {
    match v {
        Some(v) => v.to_bits(),
        None => u32::MAX,
    }
}

/// Bit pattern for an available-space constraint.
fn available_bits(a: AvailableSpace) -> u32 {
    match a {
        AvailableSpace::Definite(v) => v.to_bits(),
        AvailableSpace::MinContent => u32::MAX - 1,
        AvailableSpace::MaxContent => u32::MAX - 2,
    }
}

impl MeasureKey {
    fn new(i: &LayoutInput) -> Self {
        let axis = match i.axis {
            RequestedAxis::Horizontal => 0u8,
            RequestedAxis::Vertical => 1,
            RequestedAxis::Both => 2,
        };
        let sizing = match i.sizing_mode {
            SizingMode::ContentSize => 0u8,
            SizingMode::InherentSize => 1 << 2,
        };
        let collapsible = ((i.vertical_margins_are_collapsible.start as u8) << 3)
            | ((i.vertical_margins_are_collapsible.end as u8) << 4);
        Self {
            known: (
                known_bits(i.known_dimensions.width),
                known_bits(i.known_dimensions.height),
            ),
            parent: (
                known_bits(i.parent_size.width),
                known_bits(i.parent_size.height),
            ),
            available: (
                available_bits(i.available_space.width),
                available_bits(i.available_space.height),
            ),
            flags: axis | sizing | collapsible,
        }
    }
}

/// Where a node's computed boxes live. taffy owns styles and children; it
/// exposes no way to write a node's layout from outside, so the solver keeps
/// them here.
#[derive(Clone, Copy, Default)]
struct NodeGeometry {
    /// Layout as the algorithms produced it, before pixel rounding.
    unrounded: Layout,
    /// Layout after taffy's position-aware pixel rounding. This is what the
    /// rest of Lumen reads.
    rounded: Layout,
}

/// Computed boxes for every node the solver has visited, keyed by taffy node.
#[derive(Default)]
pub struct Geometry {
    entries: HashMap<u64, NodeGeometry>,
}

impl Geometry {
    /// Final (rounded) layout for a node, or `None` if it has never been laid
    /// out.
    pub fn get(&self, node: NodeId) -> Option<Layout> {
        self.entries.get(&u64::from(node)).map(|g| g.rounded)
    }

    /// Forget a node's geometry. Call this whenever the node leaves the tree,
    /// so the map does not outlive the tree it describes.
    pub fn remove(&mut self, node: NodeId) {
        self.entries.remove(&u64::from(node));
    }
}

/// Solve `root` and write the resulting geometry into `geometry`.
///
/// `measure` is called for leaf nodes exactly as taffy's own
/// `compute_layout_with_measure` calls it, except that the node context
/// arrives by value: leaves are measured, never mutated, and copying the
/// context is what lets the style and the context be read at the same time.
///
/// Returns the number of nodes the solve computed (cache misses).
/// With the memo in place this is proportional to the size of the subtree;
/// without it, it grows exponentially with column nesting depth, which is
/// what [`crate::LayoutResource::visits_last_sync`] exists to police.
pub fn compute_layout_memoised<C, M>(
    tree: &mut TaffyTree<C>,
    geometry: &mut Geometry,
    root: NodeId,
    available_space: taffy::Size<AvailableSpace>,
    measure: M,
) -> usize
where
    C: Copy,
    M: FnMut(
        taffy::Size<Option<f32>>,
        taffy::Size<AvailableSpace>,
        NodeId,
        Option<C>,
        &Style,
    ) -> taffy::Size<f32>,
{
    let mut view = View {
        tree,
        geometry,
        measure,
        memo: HashMap::new(),
        visits: 0,
    };
    compute_root_layout(&mut view, root, available_space);
    round_layout(&mut view, root);
    view.visits
}

/// One solve in progress: taffy's tree plus the memo and the geometry it
/// writes into.
struct View<'a, C, M> {
    tree: &'a mut TaffyTree<C>,
    geometry: &'a mut Geometry,
    measure: M,
    memo: HashMap<u64, HashMap<MeasureKey, LayoutOutput>>,
    visits: usize,
}

impl<C, M> View<'_, C, M> {
    fn style_of(&self, node: NodeId) -> &Style {
        self.tree.style(node).expect("node is in the taffy tree")
    }
}

impl<C, M> View<'_, C, M>
where
    C: Copy,
    M: FnMut(
        taffy::Size<Option<f32>>,
        taffy::Size<AvailableSpace>,
        NodeId,
        Option<C>,
        &Style,
    ) -> taffy::Size<f32>,
{
    /// The dispatch both [`LayoutPartialTree::compute_child_layout`] and
    /// [`LayoutBlockContainer::compute_block_child_layout`] route through.
    /// Mirrors taffy's own: hidden layout short-circuits, everything else
    /// goes through the cache and then to the algorithm the node's `display`
    /// selects.
    fn compute(
        &mut self,
        node: NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return compute_hidden_layout(self, node);
        }
        compute_cached_layout(self, node, inputs, |view, node, inputs| {
            view.visits += 1;
            let display = view.style_of(node).display;
            let has_children = view.child_count(node) > 0;
            match (display, has_children) {
                (Display::None, _) => compute_hidden_layout(view, node),
                (Display::Block, true) => compute_block_layout(view, node, inputs, block_ctx),
                (Display::FlowRoot, true) => compute_block_layout(view, node, inputs, None),
                (Display::Flex, true) => compute_flexbox_layout(view, node, inputs),
                (Display::Grid, true) => compute_grid_layout(view, node, inputs),
                (_, false) => {
                    let View { tree, measure, .. } = view;
                    let style = tree.style(node).expect("node is in the taffy tree");
                    let context = tree.get_node_context(node).copied();
                    compute_leaf_layout(
                        inputs,
                        style,
                        |_, _| 0.0,
                        |known, available| measure(known, available, node, context, style),
                    )
                }
            }
        })
    }
}

impl<C, M> TraversePartialTree for View<'_, C, M> {
    type ChildIter<'a>
        = <TaffyTree<C> as TraversePartialTree>::ChildIter<'a>
    where
        Self: 'a;

    fn child_ids(&self, parent: NodeId) -> Self::ChildIter<'_> {
        self.tree.child_ids(parent)
    }

    fn child_count(&self, parent: NodeId) -> usize {
        self.tree.child_count(parent)
    }

    fn get_child_id(&self, parent: NodeId, index: usize) -> NodeId {
        self.tree.get_child_id(parent, index)
    }
}

impl<C, M> TraverseTree for View<'_, C, M> {}

impl<C, M> LayoutPartialTree for View<'_, C, M>
where
    C: Copy,
    M: FnMut(
        taffy::Size<Option<f32>>,
        taffy::Size<AvailableSpace>,
        NodeId,
        Option<C>,
        &Style,
    ) -> taffy::Size<f32>,
{
    type CoreContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;
    type CustomIdent = String;

    fn get_core_container_style(&self, node: NodeId) -> &Style {
        self.style_of(node)
    }

    fn set_unrounded_layout(&mut self, node: NodeId, layout: &Layout) {
        self.geometry
            .entries
            .entry(u64::from(node))
            .or_default()
            .unrounded = *layout;
    }

    fn resolve_calc_value(&self, _value: *const (), _basis: f32) -> f32 {
        0.0
    }

    fn compute_child_layout(&mut self, node: NodeId, inputs: LayoutInput) -> LayoutOutput {
        self.compute(node, inputs, None)
    }
}

impl<C, M> CacheTree for View<'_, C, M> {
    fn cache_get(&self, node: NodeId, input: &LayoutInput) -> Option<LayoutOutput> {
        // The memo answers intrinsic-size queries exactly. Anything it does
        // not hold falls through to taffy's own per-node cache, which is what
        // carries results across ticks and what `mark_dirty` invalidates.
        if input.run_mode == RunMode::ComputeSize
            && let Some(hit) = self
                .memo
                .get(&u64::from(node))
                .and_then(|slots| slots.get(&MeasureKey::new(input)))
        {
            return Some(*hit);
        }
        self.tree.cache_get(node, input)
    }

    fn cache_store(&mut self, node: NodeId, input: &LayoutInput, output: LayoutOutput) {
        if input.run_mode == RunMode::ComputeSize {
            self.memo
                .entry(u64::from(node))
                .or_default()
                .insert(MeasureKey::new(input), output);
        }
        self.tree.cache_store(node, input, output);
    }

    fn cache_clear(&mut self, node: NodeId) {
        self.memo.remove(&u64::from(node));
        self.tree.cache_clear(node);
    }
}

impl<C, M> RoundTree for View<'_, C, M>
where
    C: Copy,
    M: FnMut(
        taffy::Size<Option<f32>>,
        taffy::Size<AvailableSpace>,
        NodeId,
        Option<C>,
        &Style,
    ) -> taffy::Size<f32>,
{
    fn get_unrounded_layout(&self, node: NodeId) -> Layout {
        self.geometry
            .entries
            .get(&u64::from(node))
            .map(|g| g.unrounded)
            .unwrap_or_default()
    }

    fn set_final_layout(&mut self, node: NodeId, layout: &Layout) {
        self.geometry
            .entries
            .entry(u64::from(node))
            .or_default()
            .rounded = *layout;
    }
}

impl<C, M> LayoutFlexboxContainer for View<'_, C, M>
where
    C: Copy,
    M: FnMut(
        taffy::Size<Option<f32>>,
        taffy::Size<AvailableSpace>,
        NodeId,
        Option<C>,
        &Style,
    ) -> taffy::Size<f32>,
{
    type FlexboxContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;
    type FlexboxItemStyle<'a>
        = &'a Style
    where
        Self: 'a;

    fn get_flexbox_container_style(&self, node: NodeId) -> &Style {
        self.style_of(node)
    }

    fn get_flexbox_child_style(&self, child: NodeId) -> &Style {
        self.style_of(child)
    }
}

impl<C, M> LayoutGridContainer for View<'_, C, M>
where
    C: Copy,
    M: FnMut(
        taffy::Size<Option<f32>>,
        taffy::Size<AvailableSpace>,
        NodeId,
        Option<C>,
        &Style,
    ) -> taffy::Size<f32>,
{
    type GridContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;
    type GridItemStyle<'a>
        = &'a Style
    where
        Self: 'a;

    fn get_grid_container_style(&self, node: NodeId) -> &Style {
        self.style_of(node)
    }

    fn get_grid_child_style(&self, child: NodeId) -> &Style {
        self.style_of(child)
    }
}

impl<C, M> LayoutBlockContainer for View<'_, C, M>
where
    C: Copy,
    M: FnMut(
        taffy::Size<Option<f32>>,
        taffy::Size<AvailableSpace>,
        NodeId,
        Option<C>,
        &Style,
    ) -> taffy::Size<f32>,
{
    type BlockContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;
    type BlockItemStyle<'a>
        = &'a Style
    where
        Self: 'a;

    fn get_block_container_style(&self, node: NodeId) -> &Style {
        self.style_of(node)
    }

    fn get_block_child_style(&self, child: NodeId) -> &Style {
        self.style_of(child)
    }

    fn compute_block_child_layout(
        &mut self,
        node: NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        self.compute(node, inputs, block_ctx)
    }
}
