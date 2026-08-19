//! The shared, serde-friendly view of the running app that the TCP server
//! reads from.
//!
//! Updated each tick by main-world and render-world systems. The TCP handler
//! takes a read-lock and serializes JSON-RPC responses out of these structs.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use bevy_ecs::prelude::*;
use serde::Serialize;

/// Cap on per-type message rings. Older messages drop oldest-first on overflow.
pub const MESSAGE_RING_CAP: usize = 256;

/// A 2D point in logical pixels.
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct V2 {
    /// X (horizontal) component.
    pub x: f32,
    /// Y (vertical) component.
    pub y: f32,
}

impl From<glam::Vec2> for V2 {
    fn from(v: glam::Vec2) -> Self {
        Self { x: v.x, y: v.y }
    }
}

/// RGBA color in `[0, 1]` per channel.
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct ColorView {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha.
    pub a: f32,
}

/// CSS hex rendering: `#rrggbb`, or `#rrggbbaa` when translucent.
impl From<&ColorView> for String {
    fn from(c: &ColorView) -> Self {
        let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        if c.a >= 1.0 {
            format!("#{:02x}{:02x}{:02x}", ch(c.r), ch(c.g), ch(c.b))
        } else {
            format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                ch(c.r),
                ch(c.g),
                ch(c.b),
                ch(c.a)
            )
        }
    }
}

impl From<lumen_core::components::Color> for ColorView {
    fn from(c: lumen_core::components::Color) -> Self {
        Self {
            r: c.r,
            g: c.g,
            b: c.b,
            a: c.a,
        }
    }
}

/// Serializable view of a main-world entity for `lumen.list_entities`.
#[derive(Serialize, Clone, Debug)]
pub struct EntityView {
    /// Bevy entity index (lower 32 bits are sufficient for human use; we
    /// expose the full 64-bit bits for round-trip safety).
    pub id: u64,
    /// Fully-qualified type names of recognised components on the entity.
    pub components: Vec<&'static str>,
}

/// Inspect-result view: all recognised component values on one entity.
#[derive(Serialize, Clone, Debug, Default)]
pub struct EntityInspect {
    /// Echo of the entity bits.
    pub id: u64,
    /// Optional [`lumen_core::components::Transform`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<TransformView>,
    /// Optional [`lumen_core::components::Style`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<StyleView>,
    /// Optional [`lumen_core::components::Visuals`] view (fill + radius + shadow).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visuals: Option<VisualsView>,
    /// Optional text content string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    /// Optional [`lumen_core::components::TextStyle`] (color/size/align/wrap/max-lines).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_style: Option<TextStyleView>,
    /// True iff the entity carries the [`lumen_core::input::Hovered`] marker.
    pub hovered: bool,
    /// True iff the entity carries the [`lumen_core::input::Focused`] marker.
    pub focused: bool,
    /// True iff the entity carries the [`lumen_core::input::Pressed`] marker.
    pub pressed: bool,
    /// Tab index, if [`lumen_core::components::TabIndex`] is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_index: Option<i32>,
    /// Scroll axis, if [`lumen_core::input::Scroll`] is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll: Option<&'static str>,
    /// Scroll offset, if [`lumen_core::input::ScrollOffset`] is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_offset: Option<V2>,
    /// `[lumen_core::components::Opacity`] scalar in `[0, 1]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// `[lumen_assets::ImageSource`] absolute or app-relative path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_source: Option<String>,
    /// `[lumen_assets::LoadedImage`] decoded dimensions (width, height).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaded_image: Option<LoadedImageView>,
    /// `[lumen_assets::LoadedSvg`] intrinsic size from the SVG viewBox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaded_svg: Option<V2>,
    /// `[lumen_core::components::Toggleable`] checked state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toggleable: Option<bool>,
    /// `[lumen_core::components::SliderValue`] tuple `(value, min, max)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slider_value: Option<SliderValueView>,
    /// `[lumen_primitives::Interaction`] view (hover_tint + press_tint + focus_outline).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interaction: Option<InteractionView>,
    /// `[lumen_core::components::BindText`] signal name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_text: Option<String>,
    /// Parent entity id from `bevy_ecs::hierarchy::ChildOf` (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    /// Direct children ids from `bevy_ecs::hierarchy::Children` (if any).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<u64>,
    /// Markup tag name from [`lumen_core::components::LumenTag`] (attached to
    /// selector-reachable entities only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Markup `id="..."` from [`lumen_core::components::LumenId`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lumen_id: Option<String>,
    /// Markup `class="a b c"` list from [`lumen_core::components::LumenClasses`].
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub classes: Vec<String>,
}

/// [`lumen_primitives::Interaction`] view: hover-tint + press-tint + focus-outline.
#[derive(Serialize, Clone, Debug)]
pub struct InteractionView {
    /// Hover tint color, if set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover_tint: Option<ColorView>,
    /// Press tint color, if set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub press_tint: Option<ColorView>,
    /// Focus outline `(width, color)`, if set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_outline: Option<FocusOutlineView>,
}

/// [`lumen_core::components::TextStyle`] view: color + size + align + wrap + max-lines.
#[derive(Serialize, Clone, Debug)]
pub struct TextStyleView {
    /// Fill color.
    pub color: ColorView,
    /// Font size in logical pixels.
    pub size_px: f32,
    /// `"start" | "center" | "end"`.
    pub align: &'static str,
    /// `"none" | "word" | "glyph"`.
    pub wrap: &'static str,
    /// Hard cap on lines after shaping (`None` = unbounded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u32>,
}

/// [`lumen_core::components::Visuals`] view: fill + radius + shadow.
#[derive(Serialize, Clone, Debug)]
pub struct VisualsView {
    /// Background fill (`None` = no rect painted).
    pub fill: Option<FillView>,
    /// Uniform corner radius in logical pixels.
    pub radius: f32,
    /// Stacked shadows in source order. Empty = no shadow.
    pub shadows: Vec<ShadowView>,
}

/// Fill brush variants surfaced in [`VisualsView::fill`].
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FillView {
    /// Single uniform color.
    Solid {
        /// The fill color.
        color: ColorView,
    },
    /// Linear gradient.
    Linear {
        /// CSS-style angle in degrees.
        angle_deg: f32,
        /// `(offset, color)` pairs ascending.
        stops: Vec<(f32, ColorView)>,
    },
    /// Radial gradient centred at 50% / 50% with a normalised radius.
    Radial {
        /// Normalised radius in `0..=1` of half the min dimension.
        radius: f32,
        /// `(offset, color)` pairs in ascending order.
        stops: Vec<(f32, ColorView)>,
    },
    /// Conic (sweep) gradient.
    Conic {
        /// Starting angle in degrees.
        from_deg: f32,
        /// `(offset, color)` pairs ascending.
        stops: Vec<(f32, ColorView)>,
    },
}

/// Shadow view. `inner` flips drop -> inset.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct ShadowView {
    /// Horizontal offset (px).
    pub offset_x: f32,
    /// Vertical offset (px).
    pub offset_y: f32,
    /// Gaussian blur radius (px).
    pub blur: f32,
    /// Shadow color.
    pub color: ColorView,
    /// `true` for an inset shadow.
    pub inner: bool,
}

/// Decoded image dimensions.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct LoadedImageView {
    /// Decoded width (px).
    pub width: u32,
    /// Decoded height (px).
    pub height: u32,
}

/// Slider value snapshot.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct SliderValueView {
    /// Current value, clamped to `[min, max]`.
    pub value: f32,
    /// Lower bound.
    pub min: f32,
    /// Upper bound.
    pub max: f32,
}

/// Focus-outline view.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct FocusOutlineView {
    /// Stroke width (px).
    pub width: f32,
    /// Stroke color.
    pub color: ColorView,
}

/// Serializable [`lumen_core::components::Transform`].
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct TransformView {
    /// Layout-absolute origin (top-left), logical pixels, BEFORE ancestor
    /// scroll offsets. The on-screen origin is `absolute` minus the sum of
    /// ancestor `scroll_offset` - `methods::on_screen_rect` applies that correction
    /// for `lumen.find` / `lumen.element_at`.
    pub absolute: V2,
    /// Computed size.
    pub size: V2,
}

/// Serializable [`lumen_core::components::Style`].
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct StyleView {
    /// Width specifier, formatted as `"auto" | "Npx" | "N%"`.
    pub width: &'static str,
    /// Numeric value of `width` (zero when auto).
    pub width_value: f32,
    /// Height specifier (same encoding as `width`).
    pub height: &'static str,
    /// Numeric value of `height`.
    pub height_value: f32,
    /// `"row" | "column"`.
    pub flex_direction: &'static str,
    /// Padding edges (left, right, top, bottom).
    pub padding: [f32; 4],
    /// Margin edges.
    pub margin: [f32; 4],
}

/// Serializable view of [`lumen_core::render_world::ExtractedRect`].
#[derive(Serialize, Clone, Copy, Debug)]
pub struct ExtractedRectView {
    /// Top-left in window coordinates.
    pub origin: V2,
    /// Width x height.
    pub size: V2,
    /// Fill color.
    pub fill: ColorView,
    /// Corner radius (logical pixels).
    pub radius: f32,
}

/// Serializable view of [`lumen_core::render_world::ExtractedText`].
#[derive(Serialize, Clone, Debug)]
pub struct ExtractedTextView {
    /// Baseline origin in window coordinates.
    pub origin: V2,
    /// Unshaped string.
    pub text: String,
    /// Font size in logical pixels.
    pub size_px: f32,
    /// Fill color.
    pub fill: ColorView,
}

/// Viewport resource view.
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct ViewportView {
    /// Logical pixel size.
    pub size: V2,
    /// Clear color.
    pub clear: ColorView,
}

/// Pointer-state view.
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct PointerStateView {
    /// `None` when cursor is outside the window.
    pub position: Option<V2>,
    /// Primary button held?
    pub primary_down: bool,
}

/// Keyboard modifier view.
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct ModifiersView {
    /// Shift held.
    pub shift: bool,
    /// Ctrl (or Cmd on macOS).
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Super / Cmd / Windows.
    pub super_: bool,
}

/// Focus tracker view.
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct FocusView {
    /// The focused entity's u64 bits, if any.
    pub entity: Option<u64>,
}

/// Recorded view of a [`lumen_core::input::PointerMoved`] message.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct RecordedPointerMoved {
    /// Pointer position at time of move.
    pub position: V2,
}

/// Recorded view of a [`lumen_core::input::PointerPressed`] message.
#[derive(Serialize, Clone, Debug)]
pub struct RecordedPointerPressed {
    /// Position.
    pub position: V2,
    /// Button name (e.g. `"primary" | "secondary" | "middle" | "other(N)"`).
    pub button: String,
}

/// Recorded view of a [`lumen_core::input::PointerReleased`] message.
pub type RecordedPointerReleased = RecordedPointerPressed;

/// Recorded view of a [`lumen_core::input::ClickEvent`] message.
#[derive(Serialize, Clone, Debug)]
pub struct RecordedClickEvent {
    /// The clicked entity's u64 bits.
    pub entity: u64,
    /// Position at click time.
    pub position: V2,
    /// Button name.
    pub button: String,
}

/// Recorded view of a [`lumen_core::input::KeyPressed`] message.
#[derive(Serialize, Clone, Debug)]
pub struct RecordedKeyPressed {
    /// Human-readable key (`"Enter"`, `"a"`, `"Tab"`, etc.).
    pub key: String,
    /// Modifier state.
    pub modifiers: ModifiersView,
    /// True if OS-generated key repeat.
    pub repeat: bool,
}

/// Recorded view of a [`lumen_core::input::KeyReleased`] message.
#[derive(Serialize, Clone, Debug)]
pub struct RecordedKeyReleased {
    /// Human-readable key.
    pub key: String,
    /// Modifier state.
    pub modifiers: ModifiersView,
}

/// Recorded view of a [`lumen_core::input::MouseWheel`] message.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct RecordedMouseWheel {
    /// Scroll delta in logical pixels.
    pub delta: V2,
    /// Cursor position when scroll happened.
    pub position: V2,
}

/// Recorded view of a [`lumen_core::input::FocusedKey`] message.
#[derive(Serialize, Clone, Debug)]
pub struct RecordedFocusedKey {
    /// Recipient entity's u64 bits.
    pub entity: u64,
    /// Human-readable key.
    pub key: String,
    /// Modifier state.
    pub modifiers: ModifiersView,
    /// Repeat flag.
    pub repeat: bool,
}

/// Bounded ring buffer for `MessageReader<T>` drains.
#[derive(Clone, Debug)]
pub struct MessageRing<T> {
    /// Underlying deque. Oldest at front.
    pub items: VecDeque<T>,
    /// Maximum length.
    pub cap: usize,
}

impl<T> Default for MessageRing<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::with_capacity(MESSAGE_RING_CAP),
            cap: MESSAGE_RING_CAP,
        }
    }
}

impl<T> MessageRing<T> {
    /// Push, evicting oldest if at cap.
    pub fn push(&mut self, item: T) {
        if self.items.len() == self.cap {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    /// Get the last `max` items as a Vec (newest last).
    pub fn last_n(&self, max: usize) -> Vec<&T> {
        let n = max.min(self.items.len());
        self.items.iter().rev().take(n).collect::<Vec<_>>()
    }
}

impl<T: Clone> MessageRing<T> {
    /// Clone the last `max` items into an owned Vec (newest last).
    pub fn last_n_owned(&self, max: usize) -> Vec<T> {
        let n = max.min(self.items.len());
        let mut out: Vec<T> = self.items.iter().rev().take(n).cloned().collect::<Vec<_>>();
        out.reverse();
        out
    }
}

/// One global signal cell sampled from `lumen_core::property_store::PropertyStore`.
/// Surfaced by `lumen.signals` (MCP agent tooling, via the `lumen-mcp-server`
/// bridge) and read in-process by the `lumen-devtools` overlay's Signals +
/// Perf tab.
#[derive(Serialize, Clone, Debug)]
pub struct SignalView {
    /// Global property name (`PropertyKey::Global`).
    pub name: String,
    /// Stringified value (matches the coercion `Signals::get` applies).
    pub value: String,
    /// Stored variant name: `str | bool | i64 | f64 | color | vec2 | custom`.
    pub kind: &'static str,
    /// Monotonic per-cell write counter from the store.
    pub generation: u64,
    /// Snapshot frame at which the cell's generation last changed. `0` when
    /// the cell predates snapshot history (value never observed changing).
    pub last_changed_frame: u64,
}

/// The full per-tick snapshot. Held inside an `Arc<RwLock<Snapshot>>`.
#[derive(Default)]
pub struct Snapshot {
    /// Monotonic tick counter; incremented by the main-world snapshot system.
    pub frame: u64,
    /// Wall-clock duration of the most recent tick, in microseconds,
    /// measured from the tick's start (`lumen_core::tick::Tick::now`).
    ///
    /// W6 T5: this used to time only the snapshot system's own body
    /// (single-digit us, useless). It now spans the real work: written at
    /// `TickStage::A11ySync` (full main schedule) and overwritten at
    /// `RenderStage::Render` on ticks that render (main schedule +
    /// extract + encode) - so on painted frames it is the full frame
    /// cost, on idle ticks the main-schedule cost. The old JSON key is
    /// kept for compatibility.
    pub last_tick_micros: u64,
    /// Start instant of the current tick, bridged from the main world's
    /// `Tick.now` so the render-world timing system (which has no `Tick`
    /// resource) can compute the full-tick span. Not serialized.
    pub tick_started_at: Option<std::time::Instant>,
    /// All main-world entities + the recognised component types on each.
    pub entities: Vec<EntityView>,
    /// Per-entity inspect data, keyed by raw entity bits. Mirrors `entities`.
    pub inspect: std::collections::HashMap<u64, EntityInspect>,
    /// Most recent extracted rects (render-world).
    pub rects: Vec<ExtractedRectView>,
    /// Most recent extracted text (render-world).
    pub texts: Vec<ExtractedTextView>,
    /// Main-world resources.
    pub viewport: ViewportView,
    /// Pointer state.
    pub pointer: PointerStateView,
    /// Modifier state.
    pub modifiers: ModifiersView,
    /// Focus tracker.
    pub focus: FocusView,
    /// PointerMoved ring.
    pub pointer_moved: MessageRing<RecordedPointerMoved>,
    /// PointerPressed ring.
    pub pointer_pressed: MessageRing<RecordedPointerPressed>,
    /// PointerReleased ring.
    pub pointer_released: MessageRing<RecordedPointerReleased>,
    /// ClickEvent ring.
    pub click_event: MessageRing<RecordedClickEvent>,
    /// KeyPressed ring.
    pub key_pressed: MessageRing<RecordedKeyPressed>,
    /// KeyReleased ring.
    pub key_released: MessageRing<RecordedKeyReleased>,
    /// MouseWheel ring.
    pub mouse_wheel: MessageRing<RecordedMouseWheel>,
    /// FocusedKey ring.
    pub focused_key: MessageRing<RecordedFocusedKey>,
    /// Per-entity fingerprints from the most recent snap tick. Compared
    /// against [`Self::history`] entries to compute `lumen.diff_since`.
    pub fingerprints: std::collections::HashMap<u64, EntityFingerprint>,
    /// Bounded ring of past fingerprint sets. Each entry stamps the tick's
    /// `frame` so a client can diff against an arbitrary historical point.
    /// Cap [`HISTORY_RING_CAP`] entries (oldest evicted on overflow).
    pub history: VecDeque<HistorySnapshot>,
    /// Global signal cells sampled from `PropertyStore` on the last snapshot
    /// tick, sorted by name. Empty when the app has no `PropertyStore`.
    pub signals: Vec<SignalView>,
    /// Persistent per-signal change memory: name -> `(generation, frame)` of
    /// the last observed generation bump. Backs
    /// [`SignalView::last_changed_frame`]. Not serialized.
    pub signal_changes: std::collections::HashMap<String, (u64, u64)>,
}

/// A small position-and-style fingerprint of one entity. Equal fingerprints
/// imply "looks the same in the snapshot" - useful as a cheap diff key.
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct EntityFingerprint(pub u64);

/// One historical snapshot of `(frame, fingerprints)`.
#[derive(Clone, Debug)]
pub struct HistorySnapshot {
    /// The tick frame this snapshot was captured at.
    pub frame: u64,
    /// Per-entity fingerprints for that frame.
    pub fingerprints: std::collections::HashMap<u64, EntityFingerprint>,
}

/// Cap on the [`Snapshot::history`] ring. ~16 entries is enough to diff a
/// hot-reload (which touches one or two snapshots), without ballooning the
/// per-tick clone cost.
pub const HISTORY_RING_CAP: usize = 16;

/// Shared handle to the snapshot. Inserted as a Resource into both worlds so
/// any system can update it.
#[derive(Resource, Clone, Default)]
pub struct SnapshotHandle(pub Arc<RwLock<Snapshot>>);
