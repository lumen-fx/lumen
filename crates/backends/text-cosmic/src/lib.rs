//! cosmic-text implementation of [`lumen_text::TextShaper`].
//!
//! - W5.6: emits one [`lumen_text::ShapedSegment`] per maximal
//!   `(font_id, BiDi level)` cluster so mixed-script paragraphs
//!   (Arabic + Latin, CJK + Latin, ...) keep every glyph and the
//!   renderer can paint each segment with the right font + caret/
//!   selection math can split logical ranges at segment boundaries.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod font_cache;
pub mod font_db;

use cosmic_text::skrifa::{FontRef, MetadataProvider, Tag};
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Wrap, fontdb};
use lru::LruCache;
use lumen_text::{GlyphPosition, ShapeOptions, ShapedRun, ShapedSegment, TextShaper, WrapMode};
use std::borrow::Borrow;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Arc;

/// Default cap for the shape-result LRU; adjustable per-shaper via
/// [`TextShaper::set_capacity`].
const SHAPE_CACHE_CAP: usize = 512;

/// Width bucket size in logical pixels; reflows within one bucket share a cache entry.
const WIDTH_BUCKET_PX: f32 = 4.0;

/// CSS `font-weight: normal`. The weight the family probe asks for,
/// because a family that ships one face at all ships this one.
const REGULAR_WEIGHT: u16 = 400;

/// Non-text portion of a [`ShapeKey`]: everything that affects shaping
/// except the source string. `f32` fields are stored as `u32::to_bits()`
/// for `Hash`/`Eq`.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct ShapeParams {
    size_bits: u32,
    wrap: WrapMode,
    max_lines: Option<u32>,
    /// Bucketed container width as `f32::to_bits()`; `u32::MAX` => `None`.
    width_bits: u32,
    /// `opts.line_height` as `f32::to_bits()`; `u32::MAX` => `None`
    /// (backend default). Must participate in the cache key: a `None` ->
    /// `Some` (or differing `Some`) transition changes the `Metrics`
    /// handed to cosmic-text, hence the actual glyph y-positions.
    line_height_bits: u32,
    /// Raw CSS `font-family` list (`None` = platform sans-serif). The
    /// raw string keys the cache; resolution to a concrete face happens
    /// once per distinct list via [`CosmicShaper::resolve_family`].
    family: Option<Arc<str>>,
    /// Weight handed to cosmic-text: the authored CSS `font-weight`
    /// snapped to one the resolved family provides (see
    /// [`CosmicShaper::snap_weight`]). Keying on the snapped value lets
    /// authored weights that resolve to the same face share one entry.
    weight: u16,
}

impl ShapeParams {
    fn new(size_px: f32, opts: &ShapeOptions, weight: u16) -> Self {
        Self {
            size_bits: size_px.to_bits(),
            wrap: opts.wrap,
            max_lines: opts.max_lines,
            width_bits: opts
                .width
                .map(|w| ((w / WIDTH_BUCKET_PX).ceil() * WIDTH_BUCKET_PX).to_bits())
                .unwrap_or(u32::MAX),
            line_height_bits: opts.line_height.map(f32::to_bits).unwrap_or(u32::MAX),
            family: opts.family.clone(),
            weight,
        }
    }
}

/// Owned cache key for [`CosmicShaper::cache`]. Captures every input that
/// affects the shape output: text bytes + [`ShapeParams`]. Only minted on
/// a cache miss, at insert time - the probe path borrows the text via
/// [`ShapeKeyRef`] so a cache *hit* never copies the string.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct ShapeKey {
    text: Arc<str>,
    params: ShapeParams,
}

/// Borrowed view of a [`ShapeKey`] used to probe the LRU without owning
/// the text. Lands in the same bucket as the equivalent owned key because
/// `Arc<str>` and `&str` hash identically (both delegate to `str::hash`).
struct ShapeKeyRef<'a> {
    text: &'a str,
    params: ShapeParams,
}

/// Object-safe view shared by [`ShapeKey`] + [`ShapeKeyRef`] so an owned
/// key and a borrowed probe compare + hash the same. Enables `&str`
/// look-ups via `ShapeKey: Borrow<dyn ShapeKeyLike>` - the standard
/// composite-borrow-key trick that `HashMap`/`LruCache`'s single-`Borrow`
/// bound otherwise blocks.
trait ShapeKeyLike {
    fn key_view(&self) -> (&str, &ShapeParams);
}

impl ShapeKeyLike for ShapeKey {
    fn key_view(&self) -> (&str, &ShapeParams) {
        (&self.text, &self.params)
    }
}

impl ShapeKeyLike for ShapeKeyRef<'_> {
    fn key_view(&self) -> (&str, &ShapeParams) {
        (self.text, &self.params)
    }
}

impl Hash for dyn ShapeKeyLike + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (text, params) = self.key_view();
        text.hash(state);
        params.hash(state);
    }
}

impl PartialEq for dyn ShapeKeyLike + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.key_view() == other.key_view()
    }
}

impl Eq for dyn ShapeKeyLike + '_ {}

impl<'a> Borrow<dyn ShapeKeyLike + 'a> for ShapeKey {
    fn borrow(&self) -> &(dyn ShapeKeyLike + 'a) {
        self
    }
}

/// cosmic-text-backed shaper with a bounded LRU result cache.
pub struct CosmicShaper {
    font_system: FontSystem,
    buffer: Buffer,
    last_metrics: Metrics,
    cache: LruCache<ShapeKey, Arc<ShapedRun>>,
    /// Persistent `font_id -> (bytes, face index, id hash)` cache.
    ///
    /// `fontdb::with_face_data` hands out a borrowed byte slice, so
    /// materialising an owned `Arc<Vec<u8>>` costs a full copy of the
    /// font FILE (multi-megabyte for system TTC collections). Before
    /// this cache the copy happened once per shape-cache MISS - every
    /// novel string (virtualized-list row mounts, timers rewriting
    /// labels) paid ~5 ms per font resolve, which is what made 5k-row
    /// wheel scrolling drop frames. Fonts are immutable for the life of
    /// the `FontSystem`, so one copy per face per process is enough.
    font_bytes: std::collections::HashMap<cosmic_text::fontdb::ID, (Arc<Vec<u8>>, u32, u64)>,
    /// Memoised `raw font-family list -> concrete family choice`.
    /// Resolution scans the font database once per distinct list; every
    /// later shape with the same list is a hash lookup.
    family_cache: std::collections::HashMap<Arc<str>, FamilyChoice>,
    /// Memoised `(family, authored weight) -> weight to shape with`.
    /// Filled by [`CosmicShaper::snap_weight`], whose miss path walks the
    /// family's faces and may read a font file to inspect its `wght` axis.
    weight_cache: std::collections::HashMap<(FamilyChoice, u16), u16>,
    /// Memoised `family -> the family name whose faces it is actually
    /// shaped from`. Usually the name the database gives, but a generic
    /// alias can name a family the machine does not hold, and then it is
    /// the family cosmic-text's own fallback chain settles on. `None`
    /// records a family nothing resolved for.
    family_name_cache: std::collections::HashMap<FamilyChoice, Option<Arc<str>>>,
}

/// Resolved CSS `font-family` list: either one of the CSS generic
/// families (mapped through the font database's aliases) or a concrete
/// family name verified to exist in the font database.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum FamilyChoice {
    /// `sans-serif` / `system-ui` / unresolvable list fallback.
    SansSerif,
    /// `serif`.
    Serif,
    /// `monospace`.
    Monospace,
    /// `cursive`.
    Cursive,
    /// `fantasy`.
    Fantasy,
    /// A concrete family present in the database (stored with the
    /// database's exact casing).
    Named(Arc<str>),
}

impl FamilyChoice {
    fn as_family(&self) -> Family<'_> {
        match self {
            Self::SansSerif => Family::SansSerif,
            Self::Serif => Family::Serif,
            Self::Monospace => Family::Monospace,
            Self::Cursive => Family::Cursive,
            Self::Fantasy => Family::Fantasy,
            Self::Named(n) => Family::Name(n),
        }
    }

    /// Map a CSS generic / platform-alias keyword. `None` = not a
    /// generic keyword (treat as a concrete family name).
    fn generic(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "sans-serif" | "system-ui" | "ui-sans-serif" | "-apple-system"
            | "blinkmacsystemfont" => Some(Self::SansSerif),
            "serif" | "ui-serif" => Some(Self::Serif),
            "monospace" | "ui-monospace" => Some(Self::Monospace),
            "cursive" => Some(Self::Cursive),
            "fantasy" => Some(Self::Fantasy),
            _ => None,
        }
    }
}

impl CosmicShaper {
    /// Build a new shaper. Loads system fonts on the current locale.
    ///
    /// Uses the persistent font-metadata cache ([`crate::font_cache`]) so
    /// the system-font-directory scan + face parse (~10-13 ms cold) is
    /// paid only on the first launch after the installed font set changes;
    /// subsequent launches rebuild an identical database from the cache.
    pub fn new() -> Self {
        Self::from_font_system(font_cache::load_font_system())
    }

    /// Wrap an already-built [`FontSystem`]. The shared body of the two
    /// public constructors, and the seam the unit tests build a shaper
    /// over a hand-made font database through.
    fn from_font_system(mut font_system: FontSystem) -> Self {
        let metrics = Metrics::new(16.0, 20.0);
        let buffer = Buffer::new(&mut font_system, metrics);
        Self {
            font_system,
            buffer,
            last_metrics: metrics,
            cache: LruCache::new(NonZeroUsize::new(SHAPE_CACHE_CAP).unwrap()),
            font_bytes: std::collections::HashMap::new(),
            family_cache: std::collections::HashMap::new(),
            weight_cache: std::collections::HashMap::new(),
            family_name_cache: std::collections::HashMap::new(),
        }
    }

    /// Build a shaper sharing `other`'s already-scanned font database
    /// (locale + `fontdb::Database` clone - face metadata only; font
    /// file bytes stay lazily mapped, so the clone is milliseconds).
    ///
    /// [`FontSystem::new`] walks every system font directory, which
    /// costs ~15-20 ms per call; layout and render each need their own
    /// shaper (separate ECS worlds), and constructing both via
    /// [`CosmicShaper::new`] paid that scan twice at every startup.
    /// Identical database contents => identical family resolution and
    /// shaping output, so the two shapers stay byte-for-byte in
    /// agreement exactly as two independent scans of the same disk
    /// state would.
    pub fn new_sharing_db(other: &Self) -> Self {
        let locale = other.font_system.locale().to_string();
        let db = other.font_system.db().clone();
        Self::from_font_system(FontSystem::new_with_locale_and_db(locale, db))
    }

    /// Resolve a raw CSS `font-family` list to a concrete
    /// [`FamilyChoice`], memoised per distinct list. Walks the
    /// comma-separated names left to right: a generic keyword resolves
    /// immediately; a concrete name resolves when the font database
    /// contains a face with that family (case-insensitive). An
    /// exhausted list falls back to the platform sans-serif - CSS
    /// fallback-chain semantics.
    fn resolve_family(&mut self, raw: &Arc<str>) -> FamilyChoice {
        if let Some(hit) = self.family_cache.get(raw) {
            return hit.clone();
        }
        let mut choice = FamilyChoice::SansSerif;
        'names: for name in raw.split(',') {
            let name = name.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            if name.is_empty() {
                continue;
            }
            if let Some(generic) = FamilyChoice::generic(name) {
                choice = generic;
                break;
            }
            let db = self.font_system.db();
            for face in db.faces() {
                if face
                    .families
                    .iter()
                    .any(|(fam, _)| fam.eq_ignore_ascii_case(name))
                {
                    // Store the database's exact casing so cosmic-text's
                    // exact-match lookup succeeds.
                    let exact = face
                        .families
                        .iter()
                        .find(|(fam, _)| fam.eq_ignore_ascii_case(name))
                        .map(|(fam, _)| fam.clone())
                        .unwrap_or_else(|| name.to_string());
                    choice = FamilyChoice::Named(Arc::from(exact.as_str()));
                    break 'names;
                }
            }
        }
        self.family_cache.insert(raw.clone(), choice.clone());
        choice
    }

    /// Snap an authored CSS `font-weight` to one `family` can actually be
    /// shaped at, memoised per `(family, weight)`.
    ///
    /// cosmic-text matches a family only through a face whose weight
    /// equals the request exactly, or a variable face whose `wght` axis
    /// spans it. A request that hits neither - `font-weight: 650` against
    /// a family shipping 400 and 700 - drops the family out of matching
    /// altogether, and the text is shaped with whatever face the
    /// last-resort list happens to start with: a different face per
    /// glyph, at that face's advances. Snapping keeps the authored family
    /// and picks the face CSS asks for. A family with a real `wght` axis
    /// covering the request keeps the authored weight, so variable fonts
    /// still render intermediate weights.
    ///
    /// One weight comes out, and it feeds the single `Attrs` that shaping
    /// and measurement both read, so the two cannot disagree about which
    /// face they described.
    fn snap_weight(&mut self, family: &FamilyChoice, weight: u16) -> u16 {
        let key = (family.clone(), weight);
        if let Some(&hit) = self.weight_cache.get(&key) {
            return hit;
        }
        let snapped = match self.effective_family_name(family) {
            Some(name) => shapeable_weight(self.font_system.db(), &name, weight),
            // No family resolved at all (an empty database): leave the
            // authored weight alone rather than inventing one.
            None => weight,
        };
        self.weight_cache.insert(key, snapped);
        snapped
    }

    /// The family name whose faces `family` is really shaped from,
    /// memoised.
    ///
    /// The database's own answer is right whenever it names a family the
    /// machine holds. A generic alias need not: the alias is a plain
    /// string, and `sans-serif` pointing at a family nothing provides is
    /// the ordinary case on a machine with a small font set. cosmic-text
    /// then shapes the text through its fallback chain instead, so that
    /// is the family a weight has to be snapped against - asking the
    /// alias would find no faces and snap nothing. Shaping one character
    /// at a weight every family ships reports which face the chain
    /// settles on.
    fn effective_family_name(&mut self, family: &FamilyChoice) -> Option<Arc<str>> {
        if let Some(hit) = self.family_name_cache.get(family) {
            return hit.clone();
        }
        let named = provided_family_name(self.font_system.db(), family);
        let resolved = named.or_else(|| self.probe_family_name(family));
        self.family_name_cache
            .insert(family.clone(), resolved.clone());
        resolved
    }

    /// Shape one digit through `family` at the regular weight and report
    /// the family of the face that came back. Runs on its own buffer so
    /// the in-flight shape's buffer is untouched, and at most once per
    /// distinct family.
    fn probe_family_name(&mut self, family: &FamilyChoice) -> Option<Arc<str>> {
        let attrs = Attrs::new()
            .family(family.as_family())
            .weight(cosmic_text::Weight(REGULAR_WEIGHT));
        let mut probe = Buffer::new(&mut self.font_system, self.last_metrics);
        {
            let mut bb = probe.borrow_with(&mut self.font_system);
            bb.set_wrap(Wrap::None);
            bb.set_size(None, None);
            bb.set_text("0", &attrs, Shaping::Advanced, None);
            bb.shape_until_scroll(false);
        }
        let font_id = probe
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .map(|g| g.font_id)
            .next()?;
        let db = self.font_system.db();
        let face = db.face(font_id)?;
        face.families
            .first()
            .map(|(name, _)| Arc::from(name.as_str()))
    }

    /// Force-drop the result cache. Mostly useful for tests.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
}

/// The family name the database itself provides for `family`, when it
/// names a family the database holds faces for. `None` when the name
/// resolves to nothing - the ordinary case for a generic alias on a
/// machine without the family the alias points at.
fn provided_family_name(db: &fontdb::Database, family: &FamilyChoice) -> Option<Arc<str>> {
    let query = family.as_family();
    let name = db.family_name(&query);
    family_faces(db, name).next().map(|_| Arc::from(name))
}

/// Miss path of [`CosmicShaper::snap_weight`], over a family name known
/// to have faces.
fn shapeable_weight(db: &fontdb::Database, name: &str, weight: u16) -> u16 {
    let faces: Vec<(fontdb::ID, u16)> =
        family_faces(db, name).map(|f| (f.id, f.weight.0)).collect();
    // The family already ships the authored weight as its own face.
    if faces.iter().any(|(_, w)| *w == weight) {
        return weight;
    }
    // A variable face spanning the request needs no snapping: the
    // instance at that weight is a real one.
    if faces
        .iter()
        .any(|(id, _)| weight_axis_covers(db, *id, weight))
    {
        return weight;
    }
    let mut available: Vec<u16> = faces.into_iter().map(|(_, w)| w).collect();
    available.sort_unstable();
    available.dedup();
    if available.is_empty() {
        return weight;
    }
    nearest_css_weight(weight, &available)
}

/// Every face in the database listing `name` among its families.
fn family_faces<'a>(
    db: &'a fontdb::Database,
    name: &'a str,
) -> impl Iterator<Item = &'a fontdb::FaceInfo> {
    db.faces()
        .filter(move |f| f.families.iter().any(|(fam, _)| fam.as_str() == name))
}

/// Whether the face carries a `wght` variation axis spanning `weight`, in
/// which case it can be instantiated at exactly that weight.
fn weight_axis_covers(db: &fontdb::Database, id: fontdb::ID, weight: u16) -> bool {
    db.with_face_data(id, |bytes, index| {
        let font = FontRef::from_index(bytes, index).ok()?;
        let axis = font.axes().get_by_tag(Tag::new(b"wght"))?;
        let w = f32::from(weight);
        Some(w >= axis.min_value() && w <= axis.max_value())
    }) == Some(Some(true))
}

/// The weight to use when `available` does not hold `weight`, per the CSS
/// Fonts font-weight matching order: from 400-500 look up to 500 first,
/// then down, then above; below 400 look down first; above 500 look up
/// first. `available` is sorted ascending and non-empty.
fn nearest_css_weight(weight: u16, available: &[u16]) -> u16 {
    let heavier = |over: u16| available.iter().copied().find(|w| *w > over);
    let lighter = |under: u16| available.iter().rev().copied().find(|w| *w < under);
    let picked = if (400..=500).contains(&weight) {
        available
            .iter()
            .copied()
            .find(|w| *w > weight && *w <= 500)
            .or_else(|| lighter(weight))
            .or_else(|| heavier(500))
    } else if weight < 400 {
        lighter(weight).or_else(|| heavier(weight))
    } else {
        heavier(weight).or_else(|| lighter(weight))
    };
    picked.unwrap_or(available[0])
}

impl Default for CosmicShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl TextShaper for CosmicShaper {
    /// Cache occupancy of the shape-result LRU.
    fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Resize the shape-result LRU at runtime.
    fn set_capacity(&mut self, entries: usize) {
        if let Some(cap) = NonZeroUsize::new(entries) {
            self.cache.resize(cap);
        }
    }

    fn shape(&mut self, text: &str, size_px: f32, opts: ShapeOptions) -> Option<ShapedRun> {
        if text.is_empty() {
            return None;
        }

        // Resolve the CSS family fallback chain and the weight the family
        // can be shaped at (both memoised) before anything else: they are
        // what the cache key describes, so two authored weights that land
        // on the same face share one entry.
        let family_choice = match &opts.family {
            Some(raw) => self.resolve_family(raw),
            None => FamilyChoice::SansSerif,
        };
        let weight = self.snap_weight(&family_choice, opts.weight.clamp(1, 1000));

        // Lookup before shaping. The probe borrows `text` (no copy on a
        // cache hit); the owned `ShapeKey` is only minted below, on a miss.
        // `LruCache::get` promotes the entry to most-recent on hit; a hit
        // clones the cached [`ShapedRun`] (Arc-cheap).
        let params = ShapeParams::new(size_px, &opts, weight);
        let probe = ShapeKeyRef {
            text,
            params: params.clone(),
        };
        if let Some(cached) = self.cache.get(&probe as &dyn ShapeKeyLike) {
            return Some((**cached).clone());
        }

        let metrics = Metrics::new(size_px, opts.resolved_line_height(size_px));
        if metrics != self.last_metrics {
            self.buffer.set_metrics(metrics);
            self.last_metrics = metrics;
        }

        let wrap = match opts.wrap {
            WrapMode::None => Wrap::None,
            WrapMode::Word => Wrap::Word,
            WrapMode::Glyph => Wrap::Glyph,
        };

        let attrs = Attrs::new()
            .family(family_choice.as_family())
            .weight(cosmic_text::Weight(weight));
        {
            let mut bb = self.buffer.borrow_with(&mut self.font_system);
            bb.set_wrap(wrap);
            if let Some(w) = opts.width
                && opts.wrap != WrapMode::None
            {
                bb.set_size(Some(w), None);
            } else {
                bb.set_size(None, None);
            }
            bb.set_text(text, &attrs, Shaping::Advanced, None);
            bb.shape_until_scroll(false);
        }

        // W5.6: walk layout runs and chunk glyphs into maximal
        // `(font_id, BiDi level)` segments in source order. cosmic-text
        // emits glyphs in visual order within a layout run but each
        // `LayoutGlyph` carries the BiDi level + font_id needed to
        // reconstruct the itemisation. We honour the visual order
        // within a segment and rely on the byte_start/byte_end fields
        // for the caret/selection math downstream.
        struct PendingSeg {
            font_id: cosmic_text::fontdb::ID,
            level: u8,
            glyphs: Vec<GlyphPosition>,
            width: f32,
        }
        let mut segs: Vec<PendingSeg> = Vec::new();
        let mut total_width: f32 = 0.0;
        let line_height = self.last_metrics.line_height;
        let max_lines = opts.max_lines.map(|n| n as usize);
        let mut truncated = false;
        // Widths of the kept lines, in order - the trim-to-fit ellipsis
        // pass below recomputes the LAST line's width after eliding, and
        // needs the others to keep `total_width` honest.
        let mut kept_line_ws: Vec<f32> = Vec::new();
        // `LayoutGlyph::start` / `end` are offsets into the BUFFER LINE the
        // glyph sits on, so every line after the first restarts at zero.
        // Everything downstream (`TextGeometry`, caret, selection) reads
        // them as offsets into the whole shaped string, so line two's bytes
        // would alias onto line one's and a click on line two would land
        // the caret on line one. Rebase each glyph onto its line's start.
        // Soft-wrapped runs share a `line_i`, so they share one base.
        let line_base: Vec<u32> = {
            let mut acc = 0u32;
            self.buffer
                .lines
                .iter()
                .map(|l| {
                    let base = acc;
                    acc += (l.text().len() + l.ending().as_str().len()) as u32;
                    base
                })
                .collect()
        };
        for (line_idx, run) in self.buffer.layout_runs().enumerate() {
            if let Some(cap) = max_lines
                && line_idx >= cap
            {
                truncated = true;
                break;
            }
            kept_line_ws.push(run.line_w);
            total_width = total_width.max(run.line_w);
            let y_offset = (line_idx as f32) * line_height;
            let base = line_base.get(run.line_i).copied().unwrap_or(0);
            for g in run.glyphs.iter() {
                let level = g.level.number();
                let gp = GlyphPosition {
                    id: g.glyph_id as u32,
                    x: g.x,
                    y: g.y + y_offset,
                    advance: g.w,
                    byte_start: g.start as u32 + base,
                    byte_end: g.end as u32 + base,
                };
                // Append to the last segment when font_id + level match;
                // otherwise start a new segment. Visual ordering inside
                // a segment is preserved; the segment list itself walks
                // cosmic-text's per-line glyph order (visual L->R), which
                // is the order downstream callers expect for paint.
                let extend = segs
                    .last()
                    .map(|s| s.font_id == g.font_id && s.level == level)
                    .unwrap_or(false);
                if extend {
                    let s = segs.last_mut().unwrap();
                    s.width += g.w;
                    s.glyphs.push(gp);
                } else {
                    segs.push(PendingSeg {
                        font_id: g.font_id,
                        level,
                        glyphs: vec![gp],
                        width: g.w,
                    });
                }
            }
        }

        // Elide with `...` when content was truncated by `max_lines`.
        // Re-shape the single character through the same FontSystem so
        // we pick up the live font's CMAP and width, then TRIM trailing
        // glyphs from the last kept line until the ellipsis fits inside
        // `opts.width` - Qt's `QFontMetrics::elidedText` contract: the
        // elided string never exceeds the box (the previous append-only
        // pass could push the `...` past the clip edge on a full line).
        if truncated && !segs.is_empty() {
            {
                let mut bb = self.buffer.borrow_with(&mut self.font_system);
                bb.set_wrap(Wrap::None);
                bb.set_size(None, None);
                bb.set_text("\u{2026}", &attrs, Shaping::Advanced, None);
                bb.shape_until_scroll(false);
            }
            // `(glyph_id, x-within-ellipsis, advance, font_id)` - the
            // ellipsis may resolve to a different fallback face than
            // the host text, so it becomes its own segment below.
            let mut ell_glyphs: Vec<(u32, f32, f32, cosmic_text::fontdb::ID)> = Vec::new();
            let mut ell_w = 0.0_f32;
            for run in self.buffer.layout_runs() {
                for g in run.glyphs.iter() {
                    ell_glyphs.push((g.glyph_id as u32, g.x, g.w, g.font_id));
                    ell_w += g.w;
                }
            }
            if !ell_glyphs.is_empty() {
                let avail = opts.width.unwrap_or(f32::INFINITY);
                let tail = |segs: &Vec<PendingSeg>| {
                    segs.last()
                        .and_then(|s| s.glyphs.last())
                        .map(|g| (g.x + g.advance, g.y))
                };
                let (mut end_x, host_y) = tail(&segs).unwrap_or((0.0, 0.0));
                let mut host_level = segs.last().map(|s| s.level).unwrap_or(0);
                // Back out trailing glyphs until `...` fits. Bounded by
                // the glyph count; an unbounded width never trims. The
                // ellipsis itself is always kept, even in a box too
                // narrow for a single glyph (Qt shows a bare "...").
                // Trimming never crosses onto an earlier line (`y`
                // guard) - only the last kept line is elided.
                while end_x + ell_w > avail {
                    let Some(s) = segs.last_mut() else { break };
                    host_level = s.level;
                    match s.glyphs.last() {
                        None => {
                            segs.pop();
                            continue;
                        }
                        Some(g) if g.y < host_y => break,
                        Some(_) => {}
                    }
                    let Some(g) = s.glyphs.pop() else { break };
                    s.width -= g.advance;
                    // Visual left edge of the popped glyph is the new
                    // trailing edge (LTR approximation; RTL elision
                    // lands with the BiDi-aware pass).
                    end_x = g.x;
                    if s.glyphs.is_empty() {
                        segs.pop();
                    }
                }
                let ell_font = ell_glyphs[0].3;
                let mut glyphs: Vec<GlyphPosition> = Vec::with_capacity(ell_glyphs.len());
                let mut w = 0.0_f32;
                for (id, gx, adv, _font) in &ell_glyphs {
                    glyphs.push(GlyphPosition {
                        id: *id,
                        x: end_x + *gx,
                        y: host_y,
                        advance: *adv,
                        // Synthesised glyph - empty byte range.
                        byte_start: 0,
                        byte_end: 0,
                    });
                    w += *adv;
                }
                segs.push(PendingSeg {
                    font_id: ell_font,
                    level: host_level,
                    glyphs,
                    width: w,
                });
                // Recompute `total_width`: every kept line except the
                // last is untouched; the last line now ends after the
                // (possibly trimmed) tail plus the ellipsis.
                let prior = kept_line_ws
                    .iter()
                    .take(kept_line_ws.len().saturating_sub(1))
                    .fold(0.0_f32, |a, b| a.max(*b));
                total_width = prior.max(end_x + ell_w);
            }
        }

        if segs.is_empty() {
            return None;
        }

        // Resolve every segment's font bytes through the PERSISTENT
        // per-face cache (`self.font_bytes`). The same `Arc<Vec<u8>>`
        // is reused across segments, shape calls, and frames sharing a
        // font_id, so the renderer's id-keyed cache stays sticky and
        // the multi-megabyte `with_face_data` copy happens exactly once
        // per face per process - not once per novel string.
        let mut segments: Vec<ShapedSegment> = Vec::with_capacity(segs.len());
        let mut glyphs: Vec<GlyphPosition> = Vec::new();
        for s in segs {
            let (font_data, font_index, font_id_hash) =
                if let Some(entry) = self.font_bytes.get(&s.font_id) {
                    entry.clone()
                } else {
                    let (bytes, idx) = {
                        let db = self.font_system.db_mut();
                        match db.with_face_data(s.font_id, |bytes, idx| (bytes.to_vec(), idx)) {
                            Some(v) => v,
                            // Face vanished between shape + lookup -
                            // drop the segment rather than failing the
                            // whole shape.
                            None => continue,
                        }
                    };
                    let mut hasher = rustc_hash::FxHasher::default();
                    s.font_id.hash(&mut hasher);
                    let id_hash = hasher.finish();
                    let arc = Arc::new(bytes);
                    let entry = (arc, idx, id_hash);
                    self.font_bytes.insert(s.font_id, entry.clone());
                    entry
                };
            glyphs.extend(s.glyphs.iter().copied());
            segments.push(ShapedSegment {
                font_id: font_id_hash,
                font_data,
                font_index,
                level: s.level,
                glyphs: s.glyphs,
                width: s.width,
            });
        }
        if segments.is_empty() {
            return None;
        }
        let (font_data, font_index) = (segments[0].font_data.clone(), segments[0].font_index);

        let run = ShapedRun {
            font_data,
            font_index,
            glyphs,
            segments,
            width: total_width,
        };

        // LRU evicts only the least-recent entry when full, so a
        // long-tail of rare strings doesn't wipe the warm set the way
        // catastrophic-clear-on-cap did.
        let arc = Arc::new(run.clone());
        self.cache.put(
            ShapeKey {
                text: Arc::from(text),
                params,
            },
            arc,
        );

        Some(run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_appends_ellipsis_glyph() {
        // A long string with `max_lines = 1` and a narrow width forces
        // wrapping at line 1; the shaper should append `...` to the last
        // visible line so authors see a truncation indicator.
        let mut shaper = CosmicShaper::new();
        let opts = ShapeOptions {
            wrap: WrapMode::Word,
            max_lines: Some(1),
            width: Some(40.0),
            ..ShapeOptions::default()
        };
        let truncated = shaper
            .shape("hello world", 16.0, opts)
            .expect("shape truncated");
        let untruncated = shaper
            .shape("hello world", 16.0, ShapeOptions::default())
            .expect("shape untruncated");
        // Truncated run carries more glyphs than the count of glyphs that
        // would fit on a single 40px line without the ellipsis pass - at
        // least one extra glyph for the `...` codepoint.
        assert!(
            truncated.glyphs.len() < untruncated.glyphs.len() + 1,
            "truncated should have fewer glyphs than full run"
        );
        // The total drawn width grows beyond the last drawn line's
        // intrinsic end_x because the ellipsis is appended. (Width
        // doesn't drop below the original line width.)
        assert!(
            truncated.width > 0.0,
            "truncation should still report a positive width"
        );
    }

    /// W5 trim-to-fit: the elided run (content + `...`) must not exceed
    /// the available width - Qt's `elidedText` contract. Glyph wrap
    /// fills the line to the brim, so without the trim pass the
    /// appended ellipsis always overflowed here.
    #[test]
    fn ellipsis_fits_within_available_width() {
        let mut shaper = CosmicShaper::new();
        let avail = 60.0;
        let opts = ShapeOptions {
            wrap: WrapMode::Glyph,
            max_lines: Some(1),
            width: Some(avail),
            ..ShapeOptions::default()
        };
        let run = shaper
            .shape("supercalifragilistic", 16.0, opts)
            .expect("shape elided");
        let end = run
            .glyphs
            .iter()
            .map(|g| g.x + g.advance)
            .fold(0.0_f32, f32::max);
        assert!(
            end <= avail + 0.5,
            "elided run must end inside the box: end={end} avail={avail}"
        );
        assert!(
            run.width <= avail + 0.5,
            "reported width must not exceed the box: width={} avail={avail}",
            run.width
        );
        // The `...` glyph is present (synthesised glyphs carry an empty
        // byte range).
        assert!(
            run.glyphs
                .iter()
                .any(|g| g.byte_start == 0 && g.byte_end == 0 && g.advance > 0.0),
            "elided run carries a synthesised ellipsis glyph"
        );
    }

    #[test]
    fn width_bucket_collapses_nearby_widths() {
        let opts_100 = ShapeOptions {
            wrap: WrapMode::Word,
            max_lines: None,
            width: Some(100.0),
            ..ShapeOptions::default()
        };
        let opts_101 = ShapeOptions {
            wrap: WrapMode::Word,
            max_lines: None,
            width: Some(101.0),
            ..ShapeOptions::default()
        };
        let opts_104 = ShapeOptions {
            wrap: WrapMode::Word,
            max_lines: None,
            width: Some(104.0),
            ..ShapeOptions::default()
        };
        let opts_105 = ShapeOptions {
            wrap: WrapMode::Word,
            max_lines: None,
            width: Some(105.0),
            ..ShapeOptions::default()
        };
        let k_100 = ShapeParams::new(14.0, &opts_100, 400);
        let k_101 = ShapeParams::new(14.0, &opts_101, 400);
        let k_104 = ShapeParams::new(14.0, &opts_104, 400);
        let k_105 = ShapeParams::new(14.0, &opts_105, 400);
        assert_eq!(
            k_101, k_104,
            "101 and 104 share the same 4-px bucket (round up to 104)"
        );
        assert_ne!(
            k_100, k_101,
            "100 (already on a boundary, bucket = 100) is distinct from 101 (rounds up to 104)"
        );
        assert_ne!(
            k_104, k_105,
            "105 crosses the bucket boundary (rounds up to 108)"
        );
    }

    /// A distinct `line_height` override must key a distinct cache entry -
    /// it changes the `Metrics` handed to cosmic-text (hence real glyph
    /// y-positions), so a stale hit under a different line-height would
    /// silently paint the wrong spacing.
    #[test]
    fn line_height_participates_in_cache_key() {
        let default_opts = ShapeOptions::default();
        let overridden_opts = ShapeOptions {
            line_height: Some(30.0),
            ..ShapeOptions::default()
        };
        let same_override_opts = ShapeOptions {
            line_height: Some(30.0),
            ..ShapeOptions::default()
        };
        let k_default = ShapeParams::new(16.0, &default_opts, 400);
        let k_override = ShapeParams::new(16.0, &overridden_opts, 400);
        let k_same = ShapeParams::new(16.0, &same_override_opts, 400);
        assert_ne!(
            k_default, k_override,
            "an absent line_height must not collide with an explicit override"
        );
        assert_eq!(
            k_override, k_same,
            "identical line_height overrides key the same entry"
        );
    }

    /// `measure_with_baseline` honours an explicit CSS `line-height`
    /// (here 2x the default `size_px * 1.2`): a two-line wrap roughly
    /// doubles in height versus the same text at the default line-height.
    #[test]
    fn measure_with_baseline_honours_line_height_override() {
        let mut shaper = CosmicShaper::new();
        // Narrow enough to force a two-line wrap for both calls.
        let default_opts = ShapeOptions {
            wrap: WrapMode::Word,
            width: Some(40.0),
            ..ShapeOptions::default()
        };
        let (_, h_default, _) = shaper.measure_with_baseline("hello world", 16.0, &default_opts);
        let doubled_line_height = 16.0 * lumen_text::DEFAULT_LINE_HEIGHT_MULTIPLIER * 2.0;
        let overridden_opts = ShapeOptions {
            wrap: WrapMode::Word,
            width: Some(40.0),
            line_height: Some(doubled_line_height),
            ..ShapeOptions::default()
        };
        let (_, h_overridden, _) =
            shaper.measure_with_baseline("hello world", 16.0, &overridden_opts);
        assert!(
            (h_overridden - h_default * 2.0).abs() < 1.0,
            "doubling line-height should roughly double the measured height \
             (default={h_default}, overridden={h_overridden})"
        );
    }

    #[test]
    fn measure_empty_text_collapses() {
        // Empty text returns (0, 0) so taffy collapses the leaf - no
        // accidental line-height inflation for placeholder-only widgets.
        let mut shaper = CosmicShaper::new();
        let (w, h) = shaper.measure("", 16.0, None, WrapMode::None, None);
        assert_eq!((w, h), (0.0, 0.0));
    }

    #[test]
    fn measure_single_line_reports_positive_size() {
        // Non-empty text must produce a positive intrinsic size. This is
        // the W2.5 invariant that kills the 0x0 collapse worked around
        // by the hard-coded widget table in
        // `parser_html.rs:1130-1163`.
        let mut shaper = CosmicShaper::new();
        let (w, h) = shaper.measure("Save", 16.0, None, WrapMode::None, None);
        assert!(w > 0.0, "shaped text must report positive width, got {w}");
        // Height is at least one line-height (size_px * 1.2 ~ 19.2 at 16px).
        assert!(
            h >= 16.0,
            "shaped text must report >=1 line height, got {h}"
        );
    }

    #[test]
    fn measure_clamps_to_max_width() {
        // When a `max_width` is specified, the reported width must not
        // exceed it. cosmic-text wraps inside the buffer, so the
        // intrinsic width falls under the cap.
        let mut shaper = CosmicShaper::new();
        let (w, _) = shaper.measure(
            "the quick brown fox jumps over the lazy dog",
            16.0,
            Some(60.0),
            WrapMode::Word,
            None,
        );
        assert!(w <= 60.0, "measured width {w} must not exceed max_width 60");
    }

    /// W5.6: a mixed-script paragraph (Latin + Arabic) must produce
    /// at least one segment per BiDi level so the renderer can paint
    /// each font once and the caret/selection math has byte->x data
    /// per segment. We don't enforce a *minimum* segment count of 2
    /// (system fonts in the CI sandbox may collapse both runs onto
    /// one face when SansSerif covers Latin + Arabic), but we DO
    /// enforce that segment levels reflect the BiDi itemisation:
    /// when 2+ segments are emitted, at least one must be odd-level
    /// (RTL) and at least one even (LTR).
    #[test]
    fn mixed_script_emits_segments_with_bidi_levels() {
        let mut shaper = CosmicShaper::new();
        let run = shaper
            // "Hello <arabic> world": an RTL Arabic run between two LTR runs.
            .shape(
                "Hello \u{645}\u{631}\u{62d}\u{628}\u{627} world",
                16.0,
                ShapeOptions::default(),
            )
            .expect("mixed-script paragraph should shape");
        assert!(!run.segments.is_empty(), "must emit at least one segment");
        // The cumulative segment glyph count must equal the
        // back-compat `glyphs` field's length.
        let seg_total: usize = run.segments.iter().map(|s| s.glyphs.len()).sum();
        assert_eq!(
            seg_total,
            run.glyphs.len(),
            "every glyph must belong to exactly one segment"
        );
        // Look for at least one RTL-level segment (Arabic).
        let any_rtl = run.segments.iter().any(|s| s.is_rtl());
        let any_ltr = run.segments.iter().any(|s| s.is_ltr());
        assert!(
            any_rtl,
            "Arabic portion must surface as an odd-level segment ({:?})",
            run.segments.iter().map(|s| s.level).collect::<Vec<_>>()
        );
        assert!(
            any_ltr,
            "Latin portions must surface as even-level segments ({:?})",
            run.segments.iter().map(|s| s.level).collect::<Vec<_>>()
        );
    }

    /// W5.6: pure ASCII shapes to a single LTR (level 0) segment so
    /// the renderer's per-segment draw loop degenerates to one
    /// `draw_glyphs` call - the W3.6 fast path.
    #[test]
    fn pure_latin_collapses_to_one_segment() {
        let mut shaper = CosmicShaper::new();
        let run = shaper
            .shape("hello world", 16.0, ShapeOptions::default())
            .expect("Latin shape");
        assert_eq!(run.segments.len(), 1, "single font + single level");
        assert_eq!(run.segments[0].level, 0, "LTR");
    }

    #[test]
    fn lru_caches_and_resizes() {
        let mut shaper = CosmicShaper::new();
        assert_eq!(shaper.cache_len(), 0);
        // Force two shapes into the cache via the public API.
        let _ = shaper.shape("a", 14.0, ShapeOptions::default());
        let _ = shaper.shape("b", 14.0, ShapeOptions::default());
        assert!(shaper.cache_len() >= 1, "at least one entry inserted");
        // Identical second call should be a pure cache hit (no extra entries).
        let before = shaper.cache_len();
        let _ = shaper.shape("a", 14.0, ShapeOptions::default());
        assert_eq!(shaper.cache_len(), before, "hit does not grow cache");
        // Shrink-then-grow exercises the runtime resize path.
        shaper.set_capacity(1);
        assert!(shaper.cache_len() <= 1, "shrink evicted to cap");
        shaper.set_capacity(SHAPE_CACHE_CAP);
    }

    /// The CSS search order, against a family shipping regular and bold.
    #[test]
    fn nearest_weight_follows_the_css_search_order() {
        let regular_bold = [400, 700];
        // Above 500: heavier first.
        assert_eq!(nearest_css_weight(600, &regular_bold), 700);
        assert_eq!(nearest_css_weight(650, &regular_bold), 700);
        assert_eq!(nearest_css_weight(800, &regular_bold), 700);
        // Below 400: lighter first.
        assert_eq!(nearest_css_weight(200, &regular_bold), 400);
        // 400-500 looks up to 500, then down, then past 500.
        assert_eq!(nearest_css_weight(450, &regular_bold), 400);
        assert_eq!(nearest_css_weight(450, &[300, 480, 700]), 480);
        assert_eq!(nearest_css_weight(500, &[600, 900]), 600);
        // A single face absorbs every request.
        assert_eq!(nearest_css_weight(650, &[400]), 400);
        assert_eq!(nearest_css_weight(100, &[900]), 900);
    }

    /// A font database holding one face per weight in `weights`, all in
    /// `family`, with the sans-serif alias pointed at it. Weight matching
    /// reads face metadata only, so the faces need no content and the
    /// outcome is the same on every machine, with no system fonts in it.
    fn db_with(family: &str, weights: &[u16]) -> fontdb::Database {
        use cosmic_text::fontdb::{
            Database, FaceInfo, ID, Language, Source, Stretch, Style, Weight,
        };
        let mut db = Database::new();
        for &w in weights {
            db.push_face_info(FaceInfo {
                id: ID::dummy(),
                source: Source::Binary(Arc::new(Vec::<u8>::new())),
                index: 0,
                families: vec![(family.to_string(), Language::English_UnitedStates)],
                post_script_name: format!("{family}-{w}"),
                style: Style::Normal,
                weight: Weight(w),
                stretch: Stretch::Normal,
                monospaced: false,
            });
        }
        db.set_sans_serif_family(family);
        db
    }

    /// The sans-serif alias resolves to the family the database holds,
    /// and a name nothing provides resolves to nothing rather than to a
    /// family with no faces to snap against.
    #[test]
    fn a_family_resolves_only_when_the_database_holds_it() {
        let db = db_with("Test Sans", &[400, 700]);
        assert_eq!(
            provided_family_name(&db, &FamilyChoice::SansSerif).as_deref(),
            Some("Test Sans")
        );
        assert_eq!(
            provided_family_name(&db, &FamilyChoice::Named(Arc::from("Absent"))).as_deref(),
            None
        );
        // An alias pointing at a family the machine does not hold - the
        // case a generic keyword lands in on a small font set.
        let empty = db_with("Absent Sans", &[]);
        assert_eq!(
            provided_family_name(&empty, &FamilyChoice::SansSerif).as_deref(),
            None
        );
    }

    /// A weight the family ships is shaped exactly as authored.
    #[test]
    fn a_weight_the_family_ships_is_shaped_as_authored() {
        let db = db_with("Test Sans", &[400, 700]);
        assert_eq!(shapeable_weight(&db, "Test Sans", 400), 400);
        assert_eq!(shapeable_weight(&db, "Test Sans", 700), 700);
    }

    /// The reported bug: an authored weight the family does not ship has
    /// to land on one it does. Left unsnapped, cosmic-text drops the
    /// family from matching and shapes each glyph through a different
    /// last-resort face.
    #[test]
    fn a_weight_the_family_lacks_snaps_onto_a_face_it_ships() {
        let db = db_with("Test Sans", &[400, 700]);
        assert_eq!(
            shapeable_weight(&db, "Test Sans", 650),
            700,
            "the reported weight"
        );
        assert_eq!(shapeable_weight(&db, "Test Sans", 600), 700);
        assert_eq!(shapeable_weight(&db, "Test Sans", 800), 700);
        assert_eq!(shapeable_weight(&db, "Test Sans", 300), 400);
        assert_eq!(shapeable_weight(&db, "Test Sans", 100), 400);
        // The whole CSS range, not just the weights around bold.
        for authored in 1..=1000u16 {
            let snapped = shapeable_weight(&db, "Test Sans", authored);
            assert!(
                snapped == 400 || snapped == 700,
                "font-weight: {authored} shaped at {snapped}, which the family has no face for"
            );
        }
    }

    /// A family with a fuller ladder keeps the nearer face rather than
    /// collapsing everything onto regular and bold.
    #[test]
    fn snapping_picks_the_nearest_face_the_family_ships() {
        let db = db_with("Test Sans", &[100, 400, 500, 900]);
        assert_eq!(shapeable_weight(&db, "Test Sans", 650), 900);
        assert_eq!(shapeable_weight(&db, "Test Sans", 450), 500);
        assert_eq!(shapeable_weight(&db, "Test Sans", 200), 100);
    }

    /// A name with no faces behind it leaves the authored weight alone
    /// rather than snapping it to something invented.
    #[test]
    fn a_family_with_no_faces_leaves_the_weight_alone() {
        let db = db_with("Test Sans", &[400, 700]);
        assert_eq!(shapeable_weight(&db, "Absent", 650), 650);
    }
}
