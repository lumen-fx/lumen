//! Fragment cache.
//!
//! - Records each drawn primitive once as a [`Fragment`] keyed by its position-independent appearance (size, brush, radius, blur), on a sink that supports fragments.
//! - The walker replays the cached fragment under a translation instead of encoding the primitive again each frame.
//! - Caches the rect, shadow, and outline primitive families, plus each SVG asset's drawing. Text shaping is cached separately by the text shaper; image uploads are keyed by `peniko::Blob` identity in the sink.

use crate::Fragment;
use lru::LruCache;
use lumen_core::render_world::Brush as LumenBrush;
use lumen_core::render_world::{ExtractedOutline, ExtractedRect, ExtractedShadow};
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;

/// Default fragment-cache capacity in entries; the runtime may resize via [`FragmentCache::set_capacity`].
const FRAGMENT_CAP: usize = 256;

/// Lookup key derived from a primitive's *appearance* - everything
/// except origin. Two entries with the same key produce the same
/// rendered glyphs once translated.
#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
pub struct FragmentKey(u64);

impl FragmentKey {
    /// The key for one SVG asset's drawing, by the id its asset carries.
    pub fn svg(asset_id: u64) -> Self {
        let mut h = fragment_hasher();
        3u8.hash(&mut h);
        asset_id.hash(&mut h);
        Self(h.finish())
    }
}

impl From<&ExtractedRect> for FragmentKey {
    fn from(r: &ExtractedRect) -> Self {
        let mut h = fragment_hasher();
        // `disc` is the primitive-family discriminant - two different kinds
        // must never collide on the hash even if their numeric fields line up.
        hash_head(&mut h, 0, r.size, r.radius);
        if let Some(cs) = r.corner_radii {
            for c in cs {
                c.to_bits().hash(&mut h);
            }
        }
        hash_brush(&r.brush, &mut h);
        Self(h.finish())
    }
}

impl From<&ExtractedShadow> for FragmentKey {
    fn from(s: &ExtractedShadow) -> Self {
        let mut h = fragment_hasher();
        hash_head(&mut h, 1, s.size, s.radius);
        s.spread.to_bits().hash(&mut h);
        s.blur.to_bits().hash(&mut h);
        hash_color(&s.color, &mut h);
        Self(h.finish())
    }
}

impl From<&ExtractedOutline> for FragmentKey {
    fn from(o: &ExtractedOutline) -> Self {
        let mut h = fragment_hasher();
        hash_head(&mut h, 2, o.size, o.radius);
        o.width.to_bits().hash(&mut h);
        hash_color(&o.stroke, &mut h);
        Self(h.finish())
    }
}

/// Hashes the family discriminant + size + corner radius shared by the head
/// of every primitive key. Keeping the field order identical across the three
/// `From` impls preserves the exact hash byte stream.
fn hash_head(h: &mut impl Hasher, disc: u8, size: glam::Vec2, radius: f32) {
    disc.hash(h);
    size.x.to_bits().hash(h);
    size.y.to_bits().hash(h);
    radius.to_bits().hash(h);
}

/// Hashes a colour's four channels in RGBA order.
fn hash_color(c: &lumen_core::components::Color, h: &mut impl Hasher) {
    c.r.to_bits().hash(h);
    c.g.to_bits().hash(h);
    c.b.to_bits().hash(h);
    c.a.to_bits().hash(h);
}

fn hash_brush(b: &LumenBrush, h: &mut impl Hasher) {
    match b {
        LumenBrush::Solid(c) => {
            0u8.hash(h);
            hash_color(c, h);
        }
        LumenBrush::Linear { angle_deg, stops } => {
            1u8.hash(h);
            angle_deg.to_bits().hash(h);
            for (off, c) in stops.iter() {
                off.to_bits().hash(h);
                hash_color(c, h);
            }
        }
        LumenBrush::Radial { radius, stops } => {
            2u8.hash(h);
            radius.to_bits().hash(h);
            for (off, c) in stops.iter() {
                off.to_bits().hash(h);
                hash_color(c, h);
            }
        }
        LumenBrush::Conic { from_deg, stops } => {
            3u8.hash(h);
            from_deg.to_bits().hash(h);
            for (off, c) in stops.iter() {
                off.to_bits().hash(h);
                hash_color(c, h);
            }
        }
    }
}

/// Returns a fresh [`rustc_hash::FxHasher`] used as the per-key fragment
/// hasher. Fragment keys are trusted internal data, so FxHash's speed beats
/// the std `DefaultHasher`'s SipHash (whose DoS resistance is dead weight
/// here).
fn fragment_hasher() -> rustc_hash::FxHasher {
    rustc_hash::FxHasher::default()
}

/// Counters for the fragment cache surfaced via tracing.
#[derive(Default, Clone, Copy, Debug)]
pub struct CacheStats {
    /// Times an existing fragment was reused (cheap append).
    pub hits: u64,
    /// Times a fresh fragment was encoded.
    pub misses: u64,
}

/// Cache of recorded, position-independent fragments. A renderer whose sink records fragments owns
/// one and hands it to every frame's walk, so it survives from frame to frame.
pub struct FragmentCache {
    entries: LruCache<FragmentKey, Fragment>,
    stats: CacheStats,
}

impl Default for FragmentCache {
    fn default() -> Self {
        Self::with_capacity(FRAGMENT_CAP)
    }
}

impl FragmentCache {
    /// Build with a fixed entry cap.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            entries: LruCache::new(NonZeroUsize::new(cap.max(1)).unwrap()),
            stats: CacheStats::default(),
        }
    }

    /// Resizes the LRU cap at runtime; consulted by [`lumen_core::components::MemoryBudget`] eviction.
    pub fn set_capacity(&mut self, cap: usize) {
        if let Some(c) = NonZeroUsize::new(cap) {
            self.entries.resize(c);
        }
    }

    /// Fetch (and mark recent) the cached fragment for `key`, or `None`
    /// if cold. Caller is expected to encode + [`Self::insert`] on miss.
    pub fn get(&mut self, key: FragmentKey) -> Option<Fragment> {
        if let Some(s) = self.entries.get(&key) {
            self.stats.hits += 1;
            Some(s.clone())
        } else {
            self.stats.misses += 1;
            None
        }
    }

    /// Insert a freshly recorded fragment.
    pub fn insert(&mut self, key: FragmentKey, fragment: Fragment) {
        self.entries.put(key, fragment);
    }

    /// Stats snapshot.
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Cache empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every cached fragment. Tests use this between scenarios.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.stats = CacheStats::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;
    use lumen_core::components::Color;

    fn solid_rect(size: Vec2, color: Color, radius: f32) -> ExtractedRect {
        ExtractedRect {
            origin: Vec2::ZERO,
            size,
            brush: LumenBrush::Solid(color),
            radius,
            corner_radii: None,
            order: 0,
        }
    }

    #[test]
    fn identical_appearance_collides_origin_does_not() {
        let r1 = solid_rect(
            Vec2::new(100.0, 40.0),
            Color {
                r: 0.5,
                g: 0.5,
                b: 0.5,
                a: 1.0,
            },
            6.0,
        );
        let mut r2 = r1.clone();
        r2.origin = Vec2::new(80.0, 200.0); // move only
        let mut r3 = r1.clone();
        r3.radius = 4.0; // appearance changed
        assert_eq!(FragmentKey::from(&r1), FragmentKey::from(&r2));
        assert_ne!(FragmentKey::from(&r1), FragmentKey::from(&r3));
    }

    #[test]
    fn hit_miss_stats_increment() {
        let mut cache = FragmentCache::default();
        let r = solid_rect(
            Vec2::new(10.0, 10.0),
            Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0.0,
        );
        let key = FragmentKey::from(&r);
        assert!(cache.get(key).is_none());
        cache.insert(key, Fragment::new(std::sync::Arc::new(())));
        assert!(cache.get(key).is_some());
        assert!(cache.get(key).is_some());
        let s = cache.stats();
        assert_eq!(s.hits, 2);
        assert_eq!(s.misses, 1);
    }

    #[test]
    fn different_brush_keys_distinct() {
        let r_solid = solid_rect(
            Vec2::new(50.0, 50.0),
            Color {
                r: 0.2,
                g: 0.3,
                b: 0.4,
                a: 1.0,
            },
            0.0,
        );
        let r_grad = ExtractedRect {
            origin: Vec2::ZERO,
            size: Vec2::new(50.0, 50.0),
            brush: LumenBrush::Linear {
                angle_deg: 45.0,
                stops: std::sync::Arc::from(
                    [(
                        0.0,
                        Color {
                            r: 0.2,
                            g: 0.3,
                            b: 0.4,
                            a: 1.0,
                        },
                    )]
                    .as_slice(),
                ),
            },
            radius: 0.0,
            corner_radii: None,
            order: 0,
        };
        assert_ne!(FragmentKey::from(&r_solid), FragmentKey::from(&r_grad));
    }
}
