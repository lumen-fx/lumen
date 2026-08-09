//! Pointer, keyboard, drag, IME, and file-drop event types plus shared input resources.
//!
//! - Window backends translate raw OS events into the typed messages defined here.
//! - `lumen-input` reads them to drive hit-testing, focus routing, and Click/Hover dispatch.

use bevy_ecs::message::Message;
use bevy_ecs::prelude::*;
use glam::Vec2;

/// Mouse button identifier carried by pointer-press and pointer-release messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerButton {
    /// Primary (typically left mouse).
    Primary,
    /// Secondary (typically right mouse).
    Secondary,
    /// Middle button / wheel click.
    Middle,
    /// Any other button, identified by raw OS code.
    Other(u16),
}

/// Pointer moved to a new position in window coordinates.
#[derive(Message, Clone, Copy, Debug)]
pub struct PointerMoved {
    /// New position in logical pixels, top-left origin.
    pub position: Vec2,
}

/// Pointer button pressed.
#[derive(Message, Clone, Copy, Debug)]
pub struct PointerPressed {
    /// Position at time of press.
    pub position: Vec2,
    /// Which button.
    pub button: PointerButton,
}

/// Pointer button released.
#[derive(Message, Clone, Copy, Debug)]
pub struct PointerReleased {
    /// Position at time of release.
    pub position: Vec2,
    /// Which button.
    pub button: PointerButton,
}

/// Pointer left the window.
#[derive(Message, Clone, Copy, Debug)]
pub struct PointerLeft;

/// Mouse-wheel scroll event. `delta` is in logical pixels (positive y scrolls content down).
/// Backends normalise line-based wheel input to pixels with a fixed 32 px/line.
#[derive(Message, Clone, Copy, Debug)]
pub struct MouseWheel {
    /// Scroll delta in logical pixels.
    pub delta: Vec2,
    /// Cursor position at the moment of the scroll, used for hit-testing.
    pub position: Vec2,
}

/// Axis (or axes) a scroll container responds to. Used by `lumen-input` for scroll-aware hit-testing and by `lumen-primitives` for the accumulator and extract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollAxis {
    /// Vertical scrolling only.
    #[default]
    Y,
    /// Horizontal scrolling only.
    X,
    /// Both axes.
    Both,
}

impl ScrollAxis {
    /// True when this axis setting scrolls horizontally (`X` or `Both`).
    pub fn allows_x(self) -> bool {
        matches!(self, ScrollAxis::X | ScrollAxis::Both)
    }

    /// True when this axis setting scrolls vertically (`Y` or `Both`).
    pub fn allows_y(self) -> bool {
        matches!(self, ScrollAxis::Y | ScrollAxis::Both)
    }
}

/// Scrollable container configuration. Each instance carries its own sensitivity, inertia, and momentum.
#[derive(Component, Clone, Copy, Debug)]
pub struct Scroll {
    /// Allowed scroll axes.
    pub axis: ScrollAxis,
    /// Multiplier on raw wheel-delta pixels (`1.0` = normal, lower slows scrolling).
    pub sensitivity: f32,
    /// Fraction of each wheel delta added to velocity instead of being applied to offset directly. Range `[0.0, 1.0]`; `0.0` produces instant jumps, `~0.4` a gentle glide, higher values longer fling.
    pub inertia: f32,
    /// Per-container momentum in logical pixels per tick. Wheel events add to it; `integrate_scroll` decays it by `INERTIA_DECAY` each frame and writes the delta into [`ScrollOffset`].
    pub velocity: glam::Vec2,
}

impl Default for Scroll {
    fn default() -> Self {
        Self::vertical()
    }
}

impl Scroll {
    /// Returns a vertical scroller (`axis = Y`, `sensitivity = 1.0`, `inertia = 0.4`).
    pub const fn vertical() -> Self {
        Self {
            axis: ScrollAxis::Y,
            sensitivity: 1.0,
            inertia: 0.4,
            velocity: glam::Vec2::ZERO,
        }
    }

    /// Returns a horizontal scroller (`axis = X`, `sensitivity = 1.0`, `inertia = 0.4`).
    pub const fn horizontal() -> Self {
        Self {
            axis: ScrollAxis::X,
            sensitivity: 1.0,
            inertia: 0.4,
            velocity: glam::Vec2::ZERO,
        }
    }

    /// Returns a two-axis scroller (`axis = Both`, `sensitivity = 1.0`, `inertia = 0.4`).
    pub const fn both() -> Self {
        Self {
            axis: ScrollAxis::Both,
            sensitivity: 1.0,
            inertia: 0.4,
            velocity: glam::Vec2::ZERO,
        }
    }

    /// Returns `self` with `sensitivity` overridden.
    pub const fn with_sensitivity(mut self, sensitivity: f32) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    /// Returns `self` with `inertia` overridden.
    pub const fn with_inertia(mut self, inertia: f32) -> Self {
        self.inertia = inertia;
        self
    }
}

/// Current scroll offset (positive = content shifted up/left).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ScrollOffset(pub Vec2);

/// Overlay-scrollbar paint + fade state, auto-attached to every
/// [`Scroll`] entity by `lumen_primitives::scrollbar::update_scrollbars`
/// (spec section 16.2 / section 16.6). The interaction FSM (hover / drag) is global
/// per-pointer and lives in [`ScrollbarInteraction`]; this component only
/// carries the per-container fade clock the extract pass reads.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarState {
    /// Current fade alpha in `[0, 1]`; multiplied into the bar colors at
    /// extract time. `0` = fully faded out (bars skip paint AND hit).
    pub alpha: f32,
    /// Ticks-of-inactivity accumulator in seconds. Reset to zero on any
    /// activity (offset change, bar hover, drag); once it exceeds the
    /// fade delay the alpha ramps down.
    pub idle_secs: f32,
    /// [`ScrollOffset`] observed last tick - used to detect scroll
    /// activity from any source (wheel, keyboard, inertia, script).
    pub last_offset: Vec2,
}

impl Default for ScrollbarState {
    fn default() -> Self {
        Self {
            // Bars start visible so a freshly-mounted overflowing
            // container advertises its scrollability, then fade.
            alpha: 1.0,
            idle_secs: 0.0,
            last_offset: Vec2::ZERO,
        }
    }
}

/// CSS `scrollbar-width` keyword (CSS Scrollbars Styling Level 1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollbarWidthMode {
    /// Platform-default overlay thickness.
    #[default]
    Auto,
    /// Narrow rail.
    Thin,
    /// Bars hidden entirely (content still scrolls).
    None,
}

/// Overlay-scrollbar styling for one scroll container - the runtime
/// mirror of the standard CSS properties `scrollbar-color: <thumb>
/// [<track>]` and `scrollbar-width: auto | thin | none` (CSS Scrollbars
/// Styling Level 1; both transpile 1:1 to the web backend). Set from
/// the stylesheet by `lumenc`; the skin defaults live in
/// `skins/default.css` via the `--lumen-scrollbar-*` tokens.
///
/// This component's [`Default`] is the only place fallback visuals are
/// defined (the blank-no-css contract): when no stylesheet rule matches,
/// both the paint extract and the interaction FSM read these values.
/// Fields without a real-CSS spelling today (minimum thumb length, fade
/// timings) still live here - one struct a per-OS skin layer or future
/// custom property can override without touching the FSM or the paint
/// code.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarStyle {
    /// Thumb fill (`scrollbar-color` first value).
    pub thumb: crate::components::Color,
    /// Track fill (`scrollbar-color` second value). `Some` = painted
    /// whenever the bar is visible (CSS semantics); `None` = the
    /// fallback translucent track shown only while the bar is hovered
    /// (overlay convention).
    pub track: Option<crate::components::Color>,
    /// `scrollbar-width` keyword.
    pub width: ScrollbarWidthMode,
    /// Bar thickness in logical pixels at `scrollbar-width: auto`
    /// (`scrollbar-thickness` CSS property).
    pub thickness: f32,
    /// Bar thickness in logical pixels at `scrollbar-width: thin`
    /// (`scrollbar-thickness-thin` CSS property).
    pub thickness_thin: f32,
    /// Thumb alpha multiplier while the bar is hovered / dragged
    /// (`scrollbar-hover-boost` CSS property).
    pub hover_boost: f32,
    /// Fallback hover-only track fill used when [`Self::track`] is
    /// `None` (`scrollbar-track-hover` CSS property).
    pub hover_track: crate::components::Color,
    /// Minimum thumb length in logical pixels (theme minimum;
    /// `scrollbar-min-thumb` CSS property).
    pub min_thumb: f32,
    /// Inset from the viewport edges in logical pixels
    /// (`scrollbar-margin` CSS property).
    pub margin: f32,
    /// Seconds of inactivity before the bars start fading out
    /// (`scrollbar-fade-delay` CSS property).
    pub fade_delay_secs: f32,
    /// Fade-out ramp length in seconds (`scrollbar-fade-duration` CSS
    /// property).
    pub fade_secs: f32,
}

/// Fallback thumb alpha multiplier while hovered / dragged.
pub const SCROLLBAR_HOVER_BOOST: f32 = 1.6;
/// Fallback hover-only track fill used when no `scrollbar-color` track
/// value is authored.
pub const SCROLLBAR_HOVER_TRACK: crate::components::Color =
    crate::components::Color::rgba(0.5, 0.5, 0.5, 0.16);
/// Fallback idle time, in seconds, before an overlay scrollbar starts
/// fading out.
pub const SCROLLBAR_FADE_DELAY_SECS: f32 = 1.0;
/// Fallback fade-out ramp length, in seconds.
pub const SCROLLBAR_FADE_SECS: f32 = 0.25;

impl Default for ScrollbarStyle {
    fn default() -> Self {
        Self {
            // Neutral translucent thumb, readable on light and dark
            // surfaces; skins override via `--lumen-scrollbar-thumb`.
            thumb: crate::components::Color::rgba(0.62, 0.67, 0.74, 0.55),
            track: None,
            width: ScrollbarWidthMode::Auto,
            thickness: SCROLLBAR_THICKNESS,
            thickness_thin: SCROLLBAR_THICKNESS_THIN,
            hover_boost: SCROLLBAR_HOVER_BOOST,
            hover_track: SCROLLBAR_HOVER_TRACK,
            min_thumb: SCROLLBAR_MIN_THUMB,
            margin: SCROLLBAR_MARGIN,
            fade_delay_secs: SCROLLBAR_FADE_DELAY_SECS,
            fade_secs: SCROLLBAR_FADE_SECS,
        }
    }
}

impl ScrollbarStyle {
    /// Resolved bar thickness in logical pixels; `None` = bars hidden
    /// (`scrollbar-width: none`).
    pub fn thickness(&self) -> Option<f32> {
        match self.width {
            ScrollbarWidthMode::Auto => Some(self.thickness),
            ScrollbarWidthMode::Thin => Some(self.thickness_thin),
            ScrollbarWidthMode::None => None,
        }
    }

    /// Geometry inputs for [`vertical_scrollbar`] / [`horizontal_scrollbar`];
    /// `None` when bars are disabled.
    pub fn metrics(&self) -> Option<ScrollbarMetrics> {
        Some(ScrollbarMetrics {
            thickness: self.thickness()?,
            margin: self.margin,
            min_thumb: self.min_thumb,
        })
    }
}

#[cfg(test)]
mod scrollbar_style_tests {
    use super::*;

    /// `ScrollbarStyle::default()` reproduces today's fixed thickness /
    /// hover-boost / hover-track / fade timing constants exactly - the
    /// no-CSS fallback must equal current behaviour.
    #[test]
    fn default_matches_the_fallback_constants() {
        let sb = ScrollbarStyle::default();
        assert_eq!(sb.thickness, SCROLLBAR_THICKNESS);
        assert_eq!(sb.thickness_thin, SCROLLBAR_THICKNESS_THIN);
        assert_eq!(sb.hover_boost, SCROLLBAR_HOVER_BOOST);
        assert_eq!(sb.hover_track, SCROLLBAR_HOVER_TRACK);
        assert_eq!(sb.min_thumb, SCROLLBAR_MIN_THUMB);
        assert_eq!(sb.margin, SCROLLBAR_MARGIN);
        assert_eq!(sb.fade_delay_secs, SCROLLBAR_FADE_DELAY_SECS);
        assert_eq!(sb.fade_secs, SCROLLBAR_FADE_SECS);
        assert_eq!(sb.thickness(), Some(SCROLLBAR_THICKNESS));
    }

    /// A `scrollbar-thickness` / `scrollbar-thickness-thin` override (as
    /// the spawn / restyle path would set from CSS) changes the resolved
    /// bar thickness for the matching `scrollbar-width` mode, without
    /// touching the other mode's value.
    #[test]
    fn thickness_override_is_selected_by_width_mode() {
        let sb = ScrollbarStyle {
            thickness: 12.0,
            thickness_thin: 3.0,
            ..Default::default()
        };
        assert_eq!(sb.thickness(), Some(12.0));
        let thin = ScrollbarStyle {
            width: ScrollbarWidthMode::Thin,
            ..sb
        };
        assert_eq!(thin.thickness(), Some(3.0));
        let none = ScrollbarStyle {
            width: ScrollbarWidthMode::None,
            ..sb
        };
        assert_eq!(
            none.thickness(),
            None,
            "scrollbar-width: none hides bars regardless of thickness"
        );
    }
}

/// Which functional part of an overlay scrollbar the pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollbarPart {
    /// The draggable thumb.
    Thumb,
    /// The track outside the thumb (click = jump-to-position).
    Track,
}

/// Axis of the bar under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollbarAxisPick {
    /// The vertical bar on the right edge.
    Vertical,
    /// The horizontal bar on the bottom edge.
    Horizontal,
}

/// An in-flight thumb drag (pointer captured by the scrollbar).
#[derive(Clone, Copy, Debug)]
pub struct ScrollbarDrag {
    /// Scroll container whose bar is being dragged.
    pub entity: Entity,
    /// Bar axis.
    pub axis: ScrollbarAxisPick,
    /// Pointer offset from the thumb's leading edge at press time, in
    /// logical pixels along the bar axis. Keeps the grab point glued to
    /// the same spot on the thumb for the whole drag (absolute 1:1
    /// mapping).
    pub grab: f32,
    /// The container's [`ScrollOffset`] when the drag began, so Escape
    /// can cancel the drag and restore the pre-drag scroll position
    /// (Qt drag-cancel contract).
    pub start_offset: glam::Vec2,
}

/// Pointer <-> overlay-scrollbar arbitration, shared between
/// `lumen_primitives::scrollbar` (writer) and `lumen-input`'s `hit_test`
/// (reader). While the pointer is over a visible bar - or a thumb drag
/// is active - the hit-test resolves to the scroll container itself, so
/// bars sit above content for clicks/hover, wheel events still route
/// through the container's normal scroll chain (bars never steal
/// wheel), and dragging keeps working when the pointer leaves the bar
/// (pointer capture).
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct ScrollbarInteraction {
    /// Bar region currently under the pointer (visible bars only).
    pub hover: Option<(Entity, ScrollbarAxisPick, ScrollbarPart)>,
    /// Active thumb drag, if any. Takes precedence over `hover`.
    pub drag: Option<ScrollbarDrag>,
}

// --- Overlay-scrollbar geometry (spec section 16.2) ---------------------------------
//
// Pure math shared by the paint extract (`render_world::extract_scrollbars`)
// and the interaction FSM (`lumen_primitives::scrollbar`), so hit regions and
// painted pixels can never disagree. Visual metrics are inputs
// ([`ScrollbarMetrics`], resolved from [`ScrollbarStyle`] = CSS) - only the
// mapping math lives here.

/// Fallback overlay bar thickness (`scrollbar-width: auto`).
pub const SCROLLBAR_THICKNESS: f32 = 8.0;
/// Narrow rail thickness (`scrollbar-width: thin`).
pub const SCROLLBAR_THICKNESS_THIN: f32 = 4.0;
/// Fallback inset from the viewport edges, in logical pixels.
pub const SCROLLBAR_MARGIN: f32 = 2.0;
/// Fallback minimum thumb length in logical pixels - a 100 000-px
/// document must still leave a grabbable thumb.
pub const SCROLLBAR_MIN_THUMB: f32 = 24.0;

/// Resolved geometry inputs for one bar, derived from
/// [`ScrollbarStyle`] (see [`ScrollbarStyle::metrics`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarMetrics {
    /// Bar thickness (thumb + track width) in logical pixels.
    pub thickness: f32,
    /// Inset from the viewport edges in logical pixels.
    pub margin: f32,
    /// Minimum thumb length in logical pixels.
    pub min_thumb: f32,
}

impl Default for ScrollbarMetrics {
    fn default() -> Self {
        Self {
            thickness: SCROLLBAR_THICKNESS,
            margin: SCROLLBAR_MARGIN,
            min_thumb: SCROLLBAR_MIN_THUMB,
        }
    }
}

/// Resolved geometry for one overlay bar, in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarGeometry {
    /// Track rect origin (top-left).
    pub track_origin: Vec2,
    /// Track rect size.
    pub track_size: Vec2,
    /// Thumb rect origin (top-left).
    pub thumb_origin: Vec2,
    /// Thumb rect size.
    pub thumb_size: Vec2,
    /// Maximum scroll offset on this axis (`content - viewport`, > 0).
    pub max_offset: f32,
}

impl ScrollbarGeometry {
    /// Map a pointer coordinate along the bar axis to the scroll offset
    /// that puts the thumb's leading edge at `pointer - grab` (the
    /// absolute 1:1 inverse of the thumb-position formula).
    pub fn offset_for_thumb_pos(&self, pointer_along: f32, grab: f32, vertical: bool) -> f32 {
        let (track_start, track_len, thumb_len) = if vertical {
            (self.track_origin.y, self.track_size.y, self.thumb_size.y)
        } else {
            (self.track_origin.x, self.track_size.x, self.thumb_size.x)
        };
        let range = (track_len - thumb_len).max(f32::EPSILON);
        let frac = ((pointer_along - grab - track_start) / range).clamp(0.0, 1.0);
        frac * self.max_offset
    }

    /// `true` when `p` lies inside the thumb rect.
    pub fn point_in_thumb(&self, p: Vec2) -> bool {
        p.x >= self.thumb_origin.x
            && p.y >= self.thumb_origin.y
            && p.x < self.thumb_origin.x + self.thumb_size.x
            && p.y < self.thumb_origin.y + self.thumb_size.y
    }

    /// `true` when `p` lies inside the track rect (thumb included).
    pub fn point_in_track(&self, p: Vec2) -> bool {
        p.x >= self.track_origin.x
            && p.y >= self.track_origin.y
            && p.x < self.track_origin.x + self.track_size.x
            && p.y < self.track_origin.y + self.track_size.y
    }
}

/// Geometry for the vertical overlay bar of a viewport at
/// `(viewport_origin, viewport_size)` whose content is `content_h` tall,
/// scrolled to `offset_y`. Returns `None` when the content does not
/// overflow (as-needed visibility, spec section 16.2). `corner_reserved` should
/// be `true` when the horizontal bar is also visible so the two bars
/// don't overlap in the corner. `m` supplies the style-resolved visual
/// metrics ([`ScrollbarStyle::metrics`]).
pub fn vertical_scrollbar(
    viewport_origin: Vec2,
    viewport_size: Vec2,
    content_h: f32,
    offset_y: f32,
    corner_reserved: bool,
    m: ScrollbarMetrics,
) -> Option<ScrollbarGeometry> {
    let max_offset = content_h - viewport_size.y;
    if max_offset <= 0.5 {
        return None;
    }
    let corner = if corner_reserved {
        m.thickness + m.margin
    } else {
        0.0
    };
    let track_len = (viewport_size.y - 2.0 * m.margin - corner).max(0.0);
    if track_len < m.min_thumb {
        return None;
    }
    let track_origin = Vec2::new(
        viewport_origin.x + viewport_size.x - m.thickness - m.margin,
        viewport_origin.y + m.margin,
    );
    // Thumb length proportional to the visible fraction, floored at the
    // theme minimum and capped at the track.
    let thumb_len = (viewport_size.y / content_h * track_len)
        .max(m.min_thumb)
        .min(track_len);
    let frac = (offset_y / max_offset).clamp(0.0, 1.0);
    let thumb_y = track_origin.y + frac * (track_len - thumb_len);
    Some(ScrollbarGeometry {
        track_origin,
        track_size: Vec2::new(m.thickness, track_len),
        thumb_origin: Vec2::new(track_origin.x, thumb_y),
        thumb_size: Vec2::new(m.thickness, thumb_len),
        max_offset,
    })
}

/// Horizontal counterpart of [`vertical_scrollbar`] (bar along the
/// bottom edge).
pub fn horizontal_scrollbar(
    viewport_origin: Vec2,
    viewport_size: Vec2,
    content_w: f32,
    offset_x: f32,
    corner_reserved: bool,
    m: ScrollbarMetrics,
) -> Option<ScrollbarGeometry> {
    let max_offset = content_w - viewport_size.x;
    if max_offset <= 0.5 {
        return None;
    }
    let corner = if corner_reserved {
        m.thickness + m.margin
    } else {
        0.0
    };
    let track_len = (viewport_size.x - 2.0 * m.margin - corner).max(0.0);
    if track_len < m.min_thumb {
        return None;
    }
    let track_origin = Vec2::new(
        viewport_origin.x + m.margin,
        viewport_origin.y + viewport_size.y - m.thickness - m.margin,
    );
    let thumb_len = (viewport_size.x / content_w * track_len)
        .max(m.min_thumb)
        .min(track_len);
    let frac = (offset_x / max_offset).clamp(0.0, 1.0);
    let thumb_x = track_origin.x + frac * (track_len - thumb_len);
    Some(ScrollbarGeometry {
        track_origin,
        track_size: Vec2::new(track_len, m.thickness),
        thumb_origin: Vec2::new(thumb_x, track_origin.y),
        thumb_size: Vec2::new(thumb_len, m.thickness),
        max_offset,
    })
}

/// Emitted by `lumen-input` when a press and release land on the same entity without leaving it.
#[derive(Message, Clone, Copy, Debug)]
pub struct ClickEvent {
    /// The entity that was clicked.
    pub entity: Entity,
    /// Position at time of click (release point).
    pub position: Vec2,
    /// Which button.
    pub button: PointerButton,
}

/// Emitted by `lumen-primitives::press` when an entity has carried [`Pressed`] continuously past the configured long-press threshold (default 500 ms).
#[derive(Message, Clone, Copy, Debug)]
pub struct LongPressEvent {
    /// The entity that was held.
    pub entity: Entity,
}

/// Emitted by `lumen-primitives::press` when two [`ClickEvent`]s land on the same entity within the double-click window (default 300 ms).
#[derive(Message, Clone, Copy, Debug)]
pub struct DoubleClickEvent {
    /// The entity that was double-clicked.
    pub entity: Entity,
    /// Position at time of the second click.
    pub position: Vec2,
}

/// Emitted once when the pointer moves past the drag-start threshold while pressed on the entity.
#[derive(Message, Clone, Copy, Debug)]
pub struct DragStartEvent {
    /// The entity being dragged.
    pub entity: Entity,
    /// Pointer position where the press began.
    pub start: Vec2,
    /// Current pointer position when the drag crossed the threshold.
    pub position: Vec2,
}

/// Emitted on each pointer move while a drag is active on the entity.
#[derive(Message, Clone, Copy, Debug)]
pub struct DragMoveEvent {
    /// The entity being dragged.
    pub entity: Entity,
    /// Current pointer position.
    pub position: Vec2,
    /// Position delta since the last move event.
    pub delta: Vec2,
}

/// Emitted once when the pointer releases at the end of an active drag.
#[derive(Message, Clone, Copy, Debug)]
pub struct DragEndEvent {
    /// The entity that was dragged.
    pub entity: Entity,
    /// Final pointer position.
    pub position: Vec2,
}

/// Aggregated pointer state refreshed each tick by the window backend.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct PointerState {
    /// Current cursor position in window coords; `None` when the pointer is outside the window.
    pub position: Option<Vec2>,
    /// `true` when the primary button is currently held.
    pub primary_down: bool,
}

/// Marker inserted/removed by the hit-test system to mark the entity currently under the pointer; matched by style systems for `:hover` behaviour.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Hovered;

/// Marker indicating an active primary-button press on the entity.
/// Inserted on `PointerPressed`; removed on `PointerReleased` and `PointerLeft`.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Pressed;

/// Tick-scoped flag written by `lumen_input::cancel_press_on_escape`:
/// `true` exactly on ticks where an Escape key-press cancelled an
/// in-flight press (an entity carried [`Pressed`]). Consumers that also
/// react to Escape (dialog / popup close handlers) should treat such an
/// Escape as consumed and leave their state alone - cancelling a press
/// and closing a dialog must never happen on the same keystroke.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct EscapePressCancel(pub bool);

/// Cursor shape the UI wants for the current pointer position.
/// Deliberately tiny - only the shapes Lumen widgets actually request;
/// window backends map it onto the OS cursor set (winit `CursorIcon`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    /// The platform default arrow.
    #[default]
    Default,
    /// I-beam over editable text.
    Text,
    /// Pointing hand over clickable widgets.
    Pointer,
    /// Open hand over a grabbable handle (scrollbar / slider thumb).
    Grab,
    /// Closed hand while a handle drag is in flight.
    Grabbing,
}

/// Requested mouse cursor, written main-side each tick (see
/// `lumen_primitives::update_cursor_request`) and applied to the OS
/// window by the window backend, which tracks the last applied value so
/// the OS call only happens on change. Headless runners simply never
/// read it.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorRequest(pub CursorShape);

/// Named non-printable keys recognised by Lumen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedKey {
    /// Tab key (focus advance).
    Tab,
    /// Enter / Return.
    Enter,
    /// Escape.
    Escape,
    /// Backspace.
    Backspace,
    /// Spacebar.
    Space,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Left arrow.
    ArrowLeft,
    /// Right arrow.
    ArrowRight,
    /// Home.
    Home,
    /// End.
    End,
    /// Delete (forward delete).
    Delete,
}

/// Logical key, either a named navigation key or a Unicode character cluster (already resolved through modifiers and IME).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// Named non-printable key.
    Named(NamedKey),
    /// Character input, stored as a String to hold multi-scalar clusters.
    Character(String),
}

impl From<&str> for Key {
    /// Parses a human-readable key name into a [`Key`].
    /// Case-sensitive `NamedKey` lookups with `"Return"` aliasing `Enter`, `"Esc"` aliasing `Escape`, etc.; any other string becomes a `Key::Character`.
    fn from(name: &str) -> Self {
        match name {
            "Tab" => Key::Named(NamedKey::Tab),
            "Enter" | "Return" => Key::Named(NamedKey::Enter),
            "Escape" | "Esc" => Key::Named(NamedKey::Escape),
            "Backspace" => Key::Named(NamedKey::Backspace),
            "Space" => Key::Named(NamedKey::Space),
            "ArrowUp" | "Up" => Key::Named(NamedKey::ArrowUp),
            "ArrowDown" | "Down" => Key::Named(NamedKey::ArrowDown),
            "ArrowLeft" | "Left" => Key::Named(NamedKey::ArrowLeft),
            "ArrowRight" | "Right" => Key::Named(NamedKey::ArrowRight),
            "Home" => Key::Named(NamedKey::Home),
            "End" => Key::Named(NamedKey::End),
            "Delete" | "Del" => Key::Named(NamedKey::Delete),
            other => Key::Character(other.to_string()),
        }
    }
}

impl From<&str> for PointerButton {
    /// Parses a button name case-insensitively. Recognised values: `"primary"` / `"left"`, `"secondary"` / `"right"`, `"middle"`. Unknown names yield `Primary`.
    fn from(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "secondary" | "right" => PointerButton::Secondary,
            "middle" => PointerButton::Middle,
            _ => PointerButton::Primary,
        }
    }
}

#[cfg(test)]
mod from_str_tests {
    use super::*;

    #[test]
    fn key_from_str_named() {
        assert_eq!(Key::from("Enter"), Key::Named(NamedKey::Enter));
        assert_eq!(Key::from("Esc"), Key::Named(NamedKey::Escape));
        assert_eq!(Key::from("Up"), Key::Named(NamedKey::ArrowUp));
    }

    #[test]
    fn key_from_str_character() {
        assert_eq!(Key::from("a"), Key::Character("a".into()));
    }

    #[test]
    fn pointer_button_from_str() {
        assert_eq!(PointerButton::from("right"), PointerButton::Secondary);
        assert_eq!(PointerButton::from("MIDDLE"), PointerButton::Middle);
        assert_eq!(PointerButton::from("anything"), PointerButton::Primary);
    }
}

/// Active keyboard modifier flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Shift key held.
    pub shift: bool,
    /// Control held (or Cmd on macOS by app convention).
    pub ctrl: bool,
    /// Alt / Option held.
    pub alt: bool,
    /// Super / Cmd / Windows key held.
    pub super_: bool,
}

/// Live modifier state, refreshed by the window backend on `ModifiersChanged`.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct ModifiersState(pub Modifiers);

/// Raw global key-press event. Subscribe directly for global shortcuts, or read [`FocusedKey`] for per-entity routing through `lumen-input`.
#[derive(Message, Clone, Debug)]
pub struct KeyPressed {
    /// Logical key.
    pub key: Key,
    /// Modifier state at time of press.
    pub modifiers: Modifiers,
    /// Whether this is an OS-generated key repeat (held key).
    pub repeat: bool,
}

/// Raw global key-release event.
#[derive(Message, Clone, Debug)]
pub struct KeyReleased {
    /// Logical key.
    pub key: Key,
    /// Modifier state at time of release.
    pub modifiers: Modifiers,
}

/// Marker placed by `lumen-input`'s focus router on the keyboard-focused entity. The router maintains at most one [`Focused`] at a time.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Focused;

/// Marker placed alongside [`Focused`] when focus arrived via the
/// keyboard (Tab / Shift-Tab cycling), mirroring the CSS
/// `:focus-visible` heuristic. Pointer-driven focus (click-to-focus)
/// carries [`Focused`] alone. Styling keyed on `:focus-visible`
/// (keyboard-only focus rings) reads this marker; `:focus` styling
/// stays always-on.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FocusVisible;

/// Resource mirror of [`Focused`], letting systems look up the focused entity without a query.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct FocusTracker(pub Option<Entity>);

/// Pending file-drop entries from the OS this frame.
///
/// - Window backends push `(path, pos)` in arrival order.
/// - `lumen-input::dispatch_file_drops` drains the queue and emits [`FileDropped`] at the topmost [`DropTarget`] under the cursor.
#[derive(Resource, Default)]
pub struct PendingFileDrops {
    /// Pending raw drops in arrival order.
    pub drops: Vec<(std::path::PathBuf, Vec2)>,
}

/// Emitted while a file is dragged over the window prior to drop; forwarded by backends from the OS drag-and-drop session.
#[derive(Message, Clone, Debug)]
pub struct FileHovered {
    /// Path of the source file (absolute when the OS provides it).
    pub path: std::path::PathBuf,
    /// Cursor position at the time of the hover event.
    pub position: Vec2,
}

/// Emitted when the user cancels a drag-and-drop session (release outside the window or Escape).
#[derive(Message, Clone, Copy, Debug)]
pub struct FileHoverCancelled;

/// Emitted when a file drops on a [`DropTarget`].
/// `entity` is the topmost `DropTarget` under the cursor at drop time; drops without a matching target are not emitted.
#[derive(Message, Clone, Debug)]
pub struct FileDropped {
    /// Recipient entity (carries `DropTarget`).
    pub entity: Entity,
    /// File path the OS handed us.
    pub path: std::path::PathBuf,
    /// Cursor position at the time of drop.
    pub position: Vec2,
}

/// Emitted by the OS for a previously-registered global hotkey. Routed by the scripting layer as `on_hotkey(name)` and through `on("hotkey", name, fn)`.
#[derive(Message, Clone, Debug)]
pub struct HotkeyFired {
    /// Identifier matching the `register_hotkey(name, ...)` call that installed the binding.
    pub name: String,
}

/// Emitted when the user clicks a native menu item. Routed as `on_menu(id)` and through `on("menu", id, fn)`.
#[derive(Message, Clone, Debug)]
pub struct MenuClicked {
    /// `id="..."` attribute on the markup `<menuitem>`.
    pub id: String,
}

/// Emitted exactly once per `<dialog>` close (open -> closed edge).
///
/// `accepted = true` when the close went through the dialog's DEFAULT
/// button (Enter-anywhere or a direct click on it) - Qt
/// `QDialog::accepted`; every other close path (Escape, cancel/close
/// buttons, script signal write) is `accepted = false` -
/// `QDialog::rejected`. Never both, never twice per open/close cycle.
/// The scripting layer routes it as `on_dialog_accepted(id)` /
/// `on_dialog_rejected(id)` plus the per-id
/// `on("dialog_accepted", id, fn)` / `on("dialog_rejected", id, fn)`
/// registries.
#[derive(Message, Clone, Debug)]
pub struct DialogClosed {
    /// The `<dialog>` entity.
    pub entity: Entity,
    /// The dialog's markup `id="..."` when present, else its bound open
    /// signal name - the key handed to script handlers.
    pub id: String,
    /// `true` = accepted (default-button path), `false` = rejected.
    pub accepted: bool,
}

/// Emitted when the user clicks a system tray icon. Routed as `on_tray(id)` and through `on("tray", id, fn)`.
#[derive(Message, Clone, Debug)]
pub struct TrayClicked {
    /// Identifier matching the `tray_icon(id, ...)` registration call.
    pub id: String,
}

/// Emitted when the user resolves a native file dialog (open / save / folder / multi-open).
///
/// - One message per closed dialog; cancelled dialogs still emit with empty [`Self::paths`] so scripts can clean up.
/// - The scripting layer routes by [`Self::kind`] to `on_file_picked(tag, path)`, `on_files_picked(tag, paths)` (paths joined by `|`), or `on_folder_picked(tag, path)`.
#[derive(Message, Clone, Debug)]
pub struct FilePicked {
    /// `"open"` | `"open_multi"` | `"save"` | `"folder"`. Selects which scripting dispatcher receives the message.
    pub kind: &'static str,
    /// Identifier carried through from `pick_file(tag)` / `pick_files(tag)` / `pick_folder(tag)` / `save_file(tag, name)`, routed through the per-id `on()` registry.
    pub tag: String,
    /// Resolved path(s): single entry for open / save / folder; one or more for `open_multi`; empty for cancellation.
    pub paths: Vec<std::path::PathBuf>,
}

/// IME (input-method editor) state-machine event forwarded by window backends. The variant set mirrors winit's `Ime` enum.
///
/// A typical CJK composition produces: `Enabled` -> `Preedit("ni")` -> `Preedit("nih", caret)` -> `Preedit(composed)` -> `Commit(composed)` -> `Preedit("")` -> `Disabled`.
#[derive(Message, Clone, Debug)]
pub enum ImeEvent {
    /// IME activated for the focused widget.
    Enabled,
    /// Preedit (composition) buffer changed.
    Preedit {
        /// Current composition text; an empty string clears the preedit.
        text: String,
        /// Optional caret/selection byte range `(start, end)` within `text`; `None` hides the caret.
        cursor: Option<(usize, usize)>,
    },
    /// Composition finalised; `text` is inserted at the caret.
    Commit(String),
    /// IME deactivated.
    Disabled,
}

/// Per-window IME control written by `lumen-input`'s focus router and read by the window backend, which forwards changes to winit's `set_ime_allowed` and `set_ime_cursor_area`.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct ImeRequest {
    /// `true` enables IME input acceptance.
    pub allowed: bool,
    /// Caret area in physical-pixel screen coordinates, `(origin, size)`; used by the IME UI to position the candidate window.
    pub cursor_area: Option<(Vec2, Vec2)>,
}

/// Emitted by `lumen-input`'s IME router after splicing a commit segment into [`TextContent`].
#[derive(Message, Clone, Debug)]
pub struct TextInputCommitted {
    /// Target entity (typically the focused entity at commit time).
    pub entity: Entity,
    /// Final committed text segment.
    pub text: String,
}

/// Emitted by `lumen-input` on each [`KeyPressed`] when [`FocusTracker`] points at an entity; subscribe instead of [`KeyPressed`] for per-entity routing.
#[derive(Message, Clone, Debug)]
pub struct FocusedKey {
    /// Recipient.
    pub entity: Entity,
    /// Logical key.
    pub key: Key,
    /// Modifier state.
    pub modifiers: Modifiers,
    /// OS-generated repeat.
    pub repeat: bool,
}

/// Emitted by the a11y inbound-action plumbing (and any other route)
/// when assistive technology asks an entity to present a context menu.
///
/// - Producer: `handle_a11y_action` in `lumen-window-winit` for the
///   `Action::ShowContextMenu` AccessKit action; pointer-secondary
///   handlers in apps may also emit this directly.
/// - Consumer: app code (typically scripts wired through `on_menu` /
///   `on_context_menu` handlers) subscribes via `MessageReader`.
/// - Mirrors `gtk_widget_show` of a `GtkPopoverMenu` and
///   `QAbstractItemView::customContextMenuRequested` semantics.
#[derive(Message, Clone, Copy, Debug)]
pub struct ShowContextMenu {
    /// Entity the menu should attach to.
    pub entity: Entity,
}

/// Emitted by window backends when the OS reports the window gained or
/// lost keyboard focus. Backends also pause the redraw scheduler while
/// `focused = false` so unfocused windows stop polling at vsync.
#[derive(Message, Clone, Copy, Debug)]
pub struct WindowFocused {
    /// `true` on focus-in, `false` on focus-out.
    pub focused: bool,
}

/// Emitted by window backends when the OS reports the window was
/// occluded (covered by another window or moved off-screen) or revealed.
/// Backends pause the redraw scheduler while `occluded = true` to avoid
/// spending GPU on invisible frames.
#[derive(Message, Clone, Copy, Debug)]
pub struct WindowOccluded {
    /// `true` when the window is fully occluded, `false` when visible.
    pub occluded: bool,
}

/// Emitted by window backends on `WindowEvent::CloseRequested`. Apps that
/// want to veto the close (e.g. to show a "save before quit?" dialog) set
/// `vetoed = true` in a system reading this message; if no system vetoes,
/// the backend exits the event loop on the next `about_to_wait`.
///
/// Vetoing here mirrors `QCloseEvent::ignore()` and GTK4's
/// `close-request -> TRUE`.
#[derive(Message, Clone, Copy, Debug, Default)]
pub struct CloseRequest {
    /// Set by an app system to keep the window open.
    pub vetoed: bool,
}
