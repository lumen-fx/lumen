//! Offscreen layers kept across frames for faded groups.
//!
//! CSS `opacity` on an element with children fades the element and its
//! subtree as one picture: the subtree paints into a layer and the layer
//! composites at the element's opacity. Painting that layer every frame costs
//! the subtree plus a compositing pass. A [`LayerCache`] keeps the layer's
//! pixels instead, keyed by the group's identity, so a frame in which the
//! group paints the same composites the kept pixels and paints none of the
//! subtree. A changing opacity alone, a fade, reuses them too: the alpha is
//! the composite's, not the pixels'.
//!
//! A group is kept once it has painted the same for two frames in a row, so a
//! group that changes every frame (an animation inside it, a scroll) never
//! pays for a layer it cannot reuse, and paints directly instead. "The same"
//! is the frame diff's sense: every leaf compares equal, at the same place.
//! A change of scale factor or target size forgets every layer, and a group
//! that does not paint in a frame (gone, hidden, no longer faded) loses its
//! layer when the frame ends.
//!
//! The pixels kept are bounded by a byte budget; a group whose layer would
//! not fit paints directly. Layers are also rasterized a limited number per
//! frame, so a page that shows many faded groups at once spreads the work
//! over a few frames rather than stalling one.

use crate::OffscreenLayer;
use crate::walker::subtree_changed;
use lumen_core::node_ir::Node;
use peniko::kurbo::Rect;
use std::collections::HashMap;
use std::sync::Arc;

/// Default budget for kept layer pixels, in bytes.
pub const DEFAULT_LAYER_BUDGET: usize = 32 * 1024 * 1024;

/// Default number of layers rasterized in one frame.
pub const DEFAULT_RENDERS_PER_FRAME: usize = 32;

/// Bytes per layer pixel: the sinks keep RGBA8.
const BYTES_PER_PIXEL: usize = 4;

/// Counters a [`LayerCache`] reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayerStats {
    /// Groups composited from a kept layer.
    pub hits: u64,
    /// Layers rasterized to be kept.
    pub renders: u64,
    /// Groups painted directly: first seen, changed, over budget, or over the
    /// per-frame render limit.
    pub direct: u64,
    /// Layers kept right now.
    pub layers: usize,
    /// Bytes of pixels kept right now.
    pub bytes: usize,
}

/// What the walker does with one faded group this frame.
pub(crate) enum Plan {
    /// Composite the kept layer.
    Composite(OffscreenLayer),
    /// Paint the group into a new layer, hand it to [`LayerCache::store`],
    /// and composite it.
    Render,
    /// Paint the group directly, in a layer that is not kept.
    Direct,
}

struct Entry {
    /// The group's content as last painted.
    content: Arc<Node>,
    /// Where in the target the layer covers, in target pixels.
    bounds: Rect,
    /// The kept pixels and their size in bytes, once the group held still.
    layer: Option<(OffscreenLayer, usize)>,
    /// The frame that last painted the group.
    seen: u64,
}

/// Layers kept across frames, one per faded group. A renderer whose sink
/// paints offscreen owns one and hands it to every frame's walk.
pub struct LayerCache {
    entries: HashMap<u64, Entry>,
    /// Bytes of the layers in `entries`.
    bytes: usize,
    budget: usize,
    renders_per_frame: usize,
    renders_left: usize,
    frame: u64,
    /// The scale factor bits and target size the kept layers were painted for.
    target: Option<(u32, u32, u32)>,
    stats: LayerStats,
}

impl Default for LayerCache {
    fn default() -> Self {
        Self::new(DEFAULT_LAYER_BUDGET)
    }
}

impl std::fmt::Debug for LayerCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerCache")
            .field("budget", &self.budget)
            .field("stats", &self.stats())
            .finish()
    }
}

impl LayerCache {
    /// A cache keeping at most `budget` bytes of layer pixels.
    pub fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            budget,
            renders_per_frame: DEFAULT_RENDERS_PER_FRAME,
            renders_left: 0,
            frame: 0,
            target: None,
            stats: LayerStats::default(),
        }
    }

    /// Change the byte budget. Kept layers over the new budget are dropped
    /// now, the largest first.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        while self.bytes > budget {
            let largest = self
                .entries
                .iter()
                .filter_map(|(k, e)| e.layer.as_ref().map(|(_, b)| (*k, *b)))
                .max_by_key(|(_, b)| *b);
            let Some((key, _)) = largest else { break };
            if let Some(entry) = self.entries.get_mut(&key) {
                forget(&mut self.bytes, entry);
            }
        }
    }

    /// The byte budget.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Change how many layers one frame rasterizes at most.
    pub fn set_renders_per_frame(&mut self, renders: usize) {
        self.renders_per_frame = renders;
    }

    /// Counters, with the layers and bytes kept right now.
    pub fn stats(&self) -> LayerStats {
        LayerStats {
            layers: self.entries.values().filter(|e| e.layer.is_some()).count(),
            bytes: self.bytes,
            ..self.stats
        }
    }

    /// Drop every kept layer.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Start a frame painted at `dpr` into a `width` x `height` target. A
    /// different scale factor or size than the last frame forgets every
    /// layer: their pixels were painted for the old one.
    pub fn begin_frame(&mut self, dpr: f32, width: u32, height: u32) {
        let target = Some((dpr.to_bits(), width, height));
        if self.target != target {
            self.clear();
            self.target = target;
        }
        self.frame += 1;
        self.renders_left = self.renders_per_frame;
    }

    /// End the frame: forget the groups it did not paint.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        let bytes = &mut self.bytes;
        self.entries.retain(|_, e| {
            if e.seen != frame {
                forget(bytes, e);
            }
            e.seen == frame
        });
    }

    /// The target the frame paints into, in target pixels.
    pub(crate) fn target_rect(&self) -> Rect {
        let (w, h) = self.target.map_or((0, 0), |(_, w, h)| (w, h));
        Rect::new(0.0, 0.0, f64::from(w), f64::from(h))
    }

    /// Decide how the group `key` paints `content` over `bounds` this frame.
    pub(crate) fn plan(&mut self, key: u64, content: &Arc<Node>, bounds: Rect) -> Plan {
        let frame = self.frame;
        let Some(entry) = self.entries.get_mut(&key) else {
            self.entries.insert(
                key,
                Entry {
                    content: content.clone(),
                    bounds,
                    layer: None,
                    seen: frame,
                },
            );
            self.stats.direct += 1;
            return Plan::Direct;
        };
        if entry.seen == frame {
            // The same key twice in one frame: nothing to keep apart, so
            // neither is kept.
            forget(&mut self.bytes, entry);
            self.stats.direct += 1;
            return Plan::Direct;
        }
        entry.seen = frame;
        let same = entry.bounds == bounds && !subtree_changed(&entry.content, content);
        // Keep the newest tree either way, so the next diff starts from it.
        entry.content = content.clone();
        if !same {
            entry.bounds = bounds;
            forget(&mut self.bytes, entry);
            self.stats.direct += 1;
            return Plan::Direct;
        }
        if let Some((layer, _)) = &entry.layer {
            self.stats.hits += 1;
            return Plan::Composite(layer.clone());
        }
        if self.renders_left == 0 || self.bytes + layer_bytes(bounds) > self.budget {
            self.stats.direct += 1;
            return Plan::Direct;
        }
        self.renders_left -= 1;
        Plan::Render
    }

    /// Keep `layer` as the pixels of the group `key`, painted this frame
    /// after [`Self::plan`] answered [`Plan::Render`].
    pub(crate) fn store(&mut self, key: u64, layer: OffscreenLayer) {
        if let Some(entry) = self.entries.get_mut(&key) {
            forget(&mut self.bytes, entry);
            let bytes = layer_bytes(entry.bounds);
            entry.layer = Some((layer, bytes));
            self.bytes += bytes;
            self.stats.renders += 1;
        }
    }
}

/// Drop `entry`'s pixels, keeping `bytes` the count of what stays kept.
fn forget(bytes: &mut usize, entry: &mut Entry) {
    if let Some((_, b)) = entry.layer.take() {
        *bytes -= b;
    }
}

/// The bytes a layer over `bounds` keeps.
fn layer_bytes(bounds: Rect) -> usize {
    (bounds.width().max(0.0) as usize) * (bounds.height().max(0.0) as usize) * BYTES_PER_PIXEL
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::components::Color;
    use lumen_core::render_world::{Brush, Rect as LumenRect};

    fn content(color: f32) -> Arc<Node> {
        Arc::new(Node::Rect {
            bounds: LumenRect {
                origin: glam::Vec2::ZERO,
                size: glam::Vec2::new(10.0, 10.0),
            },
            brush: Brush::Solid(Color::rgb(color, 0.0, 0.0)),
            corner: 0.0,
            corners: None,
        })
    }

    fn square(side: f64) -> Rect {
        Rect::new(0.0, 0.0, side, side)
    }

    fn layer() -> OffscreenLayer {
        OffscreenLayer::new(Arc::new(()))
    }

    /// Plans `groups` (key, side) for one frame, storing a layer wherever the plan says render.
    /// Returns what each group was told, as `'c'`omposite, `'r'`ender or `'d'`irect.
    fn frame(cache: &mut LayerCache, groups: &[(u64, f64)]) -> String {
        cache.begin_frame(1.0, 100, 100);
        let node = content(1.0);
        let plans = groups
            .iter()
            .map(|&(key, side)| match cache.plan(key, &node, square(side)) {
                Plan::Composite(_) => 'c',
                Plan::Render => {
                    cache.store(key, layer());
                    'r'
                }
                Plan::Direct => 'd',
            })
            .collect();
        cache.end_frame();
        plans
    }

    /// Lowering the budget drops the largest kept layers first, until the rest fit, and a group
    /// that would not fit then paints directly.
    #[test]
    fn a_lower_budget_drops_the_largest_layers_first() {
        let mut cache = LayerCache::default();
        assert_eq!(cache.budget(), DEFAULT_LAYER_BUDGET);
        let groups = [(1, 10.0), (2, 20.0), (3, 5.0)];
        assert_eq!(frame(&mut cache, &groups), "ddd");
        assert_eq!(frame(&mut cache, &groups), "rrr");
        assert_eq!(cache.stats().bytes, (100 + 400 + 25) * 4);

        cache.set_budget(600);
        assert_eq!(cache.budget(), 600);
        let stats = cache.stats();
        assert_eq!(
            (stats.layers, stats.bytes),
            (2, 500),
            "the 20 x 20 layer went"
        );
        assert_eq!(
            frame(&mut cache, &groups),
            "cdc",
            "the dropped group no longer fits"
        );

        cache.set_budget(0);
        assert_eq!(cache.stats().layers, 0);
        assert_eq!(cache.stats().bytes, 0);
        cache.set_budget(0);
        assert_eq!(cache.stats().layers, 0, "nothing left to drop");
    }

    /// One frame rasterizes no more layers than its limit; the rest wait for a later frame.
    #[test]
    fn renders_are_spread_over_frames() {
        let mut cache = LayerCache::default();
        cache.set_renders_per_frame(1);
        let groups = [(1, 4.0), (2, 4.0), (3, 4.0)];
        assert_eq!(frame(&mut cache, &groups), "ddd");
        assert_eq!(frame(&mut cache, &groups), "rdd");
        assert_eq!(frame(&mut cache, &groups), "crd");
        assert_eq!(frame(&mut cache, &groups), "ccr");
        let stats = cache.stats();
        assert_eq!((stats.renders, stats.hits, stats.layers), (3, 3, 3));
        assert_eq!(stats.direct, 3 + 2 + 1);
    }

    /// A key painted twice in one frame names two groups, so neither keeps a layer.
    #[test]
    fn a_key_seen_twice_in_a_frame_is_not_kept() {
        let mut cache = LayerCache::default();
        assert_eq!(frame(&mut cache, &[(1, 4.0)]), "d");
        assert_eq!(frame(&mut cache, &[(1, 4.0)]), "r");
        assert_eq!(frame(&mut cache, &[(1, 4.0), (1, 4.0)]), "cd");
        assert_eq!(cache.stats().layers, 0);
        assert_eq!(cache.stats().bytes, 0);
    }

    /// A group that moves or changes repaints directly and loses its layer; one that stops
    /// painting is forgotten when the frame ends; a new scale factor forgets every layer.
    #[test]
    fn changes_drop_layers() {
        let mut cache = LayerCache::default();
        frame(&mut cache, &[(1, 4.0), (2, 4.0)]);
        assert_eq!(frame(&mut cache, &[(1, 4.0), (2, 4.0)]), "rr");
        assert_eq!(
            frame(&mut cache, &[(1, 6.0), (2, 4.0)]),
            "dc",
            "group 1 grew"
        );
        assert_eq!(cache.stats().layers, 1);

        cache.begin_frame(1.0, 100, 100);
        assert!(matches!(
            cache.plan(2, &content(0.5), square(4.0)),
            Plan::Direct
        ));
        cache.end_frame();
        assert_eq!(cache.stats().layers, 0, "group 2 painted differently");

        assert_eq!(frame(&mut cache, &[(2, 4.0)]), "d", "changed back");
        assert_eq!(frame(&mut cache, &[(2, 4.0)]), "r");
        assert_eq!(frame(&mut cache, &[]), "");
        assert_eq!(
            frame(&mut cache, &[(2, 4.0)]),
            "d",
            "forgotten while it did not paint"
        );

        assert_eq!(frame(&mut cache, &[(2, 4.0)]), "r");
        cache.begin_frame(2.0, 100, 100);
        assert_eq!(cache.stats().layers, 0, "painted for another scale factor");
        assert!(matches!(
            cache.plan(2, &content(1.0), square(4.0)),
            Plan::Direct
        ));
    }

    /// The debug form names the budget and the counters, and clearing drops every layer.
    #[test]
    fn debug_and_clear() {
        let mut cache = LayerCache::new(1024);
        frame(&mut cache, &[(1, 4.0)]);
        frame(&mut cache, &[(1, 4.0)]);
        let debug = format!("{cache:?}");
        assert!(
            debug.contains("budget: 1024") && debug.contains("layers: 1"),
            "{debug}"
        );
        cache.clear();
        assert_eq!(cache.stats().layers, 0);
        assert_eq!(cache.stats().bytes, 0);
        assert_eq!(cache.target_rect(), Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(LayerCache::default().target_rect(), Rect::ZERO);
    }
}
