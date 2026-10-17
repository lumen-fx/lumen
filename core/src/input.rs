//! Pointer, keyboard, drag, IME, and file-drop event types plus shared input resources.
//!
//! - Window backends translate raw OS events into the typed messages defined here.
//! - `lumen-input` reads them to drive hit-testing, focus routing, and Click/Hover dispatch.

use crate::time::Instant;
use bevy_ecs::message::Message;
use bevy_ecs::prelude::*;
use glam::Vec2;
use std::sync::atomic::{AtomicU64, Ordering};

/// Button identifier carried by pointer-press and pointer-release messages.
///
/// A touch contact and a pen tip touching the surface are the primary
/// button, as on the web.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PointerButton {
    /// Primary (typically left mouse, a touch contact, a pen tip).
    #[default]
    Primary,
    /// Secondary (typically right mouse, a pen barrel button).
    Secondary,
    /// Middle button / wheel click.
    Middle,
    /// The back button (X1).
    Back,
    /// The forward button (X2).
    Forward,
    /// The eraser end of a pen.
    Eraser,
    /// Any other button, identified by raw OS code.
    Other(u16),
}

impl PointerButton {
    /// The web `PointerEvent.button` code: `0` primary, `1` middle, `2`
    /// secondary, `3` back, `4` forward, `5` eraser. Any other button
    /// reports its raw code.
    pub fn code(self) -> i64 {
        match self {
            PointerButton::Primary => 0,
            PointerButton::Middle => 1,
            PointerButton::Secondary => 2,
            PointerButton::Back => 3,
            PointerButton::Forward => 4,
            PointerButton::Eraser => 5,
            PointerButton::Other(code) => code as i64,
        }
    }

    /// This button's bit in the web `PointerEvent.buttons` mask: `1`
    /// primary, `2` secondary, `4` middle, `8` back, `16` forward, `32`
    /// eraser. A button with no web code has no bit.
    pub fn bit(self) -> u32 {
        match self {
            PointerButton::Primary => 1,
            PointerButton::Secondary => 2,
            PointerButton::Middle => 4,
            PointerButton::Back => 8,
            PointerButton::Forward => 16,
            PointerButton::Eraser => 32,
            PointerButton::Other(_) => 0,
        }
    }
}

/// The kind of device behind a pointer, the web `PointerEvent.pointerType`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PointerKind {
    /// A mouse, or a touchpad driving the cursor.
    #[default]
    Mouse,
    /// A pen or stylus on a tablet or screen.
    Pen,
    /// A finger on a touch screen.
    Touch,
}

impl PointerKind {
    /// The web spelling: `"mouse"`, `"pen"` or `"touch"`.
    pub fn as_str(self) -> &'static str {
        match self {
            PointerKind::Mouse => "mouse",
            PointerKind::Pen => "pen",
            PointerKind::Touch => "touch",
        }
    }
}

impl From<&str> for PointerKind {
    /// Parses the web spelling; anything else is a mouse.
    fn from(name: &str) -> Self {
        match name {
            "pen" => PointerKind::Pen,
            "touch" => PointerKind::Touch,
            _ => PointerKind::Mouse,
        }
    }
}

/// What a pointer event says about the device that produced it, with the
/// fields, units and defaults of the web `PointerEvent`.
///
/// A device that cannot measure a value reports the web default for it: a
/// mouse has no tilt, and anything without a pressure sensor reports `0.5`
/// while a button is held and `0` otherwise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerData {
    /// Identifies the pointer for as long as it is in contact or in range:
    /// the mouse is always [`PointerData::MOUSE_ID`], each touch contact and
    /// each pen gets its own id.
    pub id: u64,
    /// The device kind.
    pub kind: PointerKind,
    /// The primary pointer of its kind: the mouse, the pen, or the first
    /// finger down while no other finger is. Lumen's own widgets (buttons,
    /// sliders, scrollbars, text selection) follow the primary pointer only.
    pub is_primary: bool,
    /// The buttons held after this event, as a web `buttons` mask (see
    /// [`PointerButton::bit`]).
    pub buttons: u32,
    /// Normalized pressure, `0.0` to `1.0`.
    pub pressure: f32,
    /// Normalized barrel (tangential) pressure, `-1.0` to `1.0`.
    pub tangential_pressure: f32,
    /// Pen tilt from vertical toward the right, `-90` to `90` degrees.
    pub tilt_x: f32,
    /// Pen tilt from vertical toward the user, `-90` to `90` degrees.
    pub tilt_y: f32,
    /// Pen rotation about its own axis, `0` to `359` degrees.
    pub twist: f32,
    /// Contact width in logical pixels; `1` for a point device.
    pub width: f32,
    /// Contact height in logical pixels; `1` for a point device.
    pub height: f32,
    /// Where this event falls among every pointer event, across the move,
    /// press, release and cancel messages: a quick stroke can press, move and
    /// release inside one frame, and a reader that interleaves the message
    /// types sorts by this to see them in the order they happened. A backend
    /// sets it with [`PointerData::stamped`]; `0` leaves the reader's own
    /// order.
    pub order: u64,
    /// When the event happened: the web's `timeStamp`. A backend that is
    /// told when the platform saw it passes that on; one that is not stamps
    /// the moment it received it. Subtracting it from the
    /// [`crate::tick::FrameTiming`] of the frame that showed its effect gives
    /// the input-to-screen latency.
    pub time_stamp: Instant,
}

/// The next [`PointerData::order`] stamp.
static POINTER_ORDER: AtomicU64 = AtomicU64::new(1);

impl PointerData {
    /// The mouse's pointer id, the one browsers give it.
    pub const MOUSE_ID: u64 = 1;

    /// The mouse with `buttons` held.
    pub fn mouse(buttons: u32) -> Self {
        Self {
            id: Self::MOUSE_ID,
            kind: PointerKind::Mouse,
            is_primary: true,
            buttons,
            pressure: Self::default_pressure(buttons),
            tangential_pressure: 0.0,
            tilt_x: 0.0,
            tilt_y: 0.0,
            twist: 0.0,
            width: 1.0,
            height: 1.0,
            order: 0,
            time_stamp: Instant::now(),
        }
    }

    /// This data stamped as the latest pointer event, for a backend to call
    /// as it writes each message.
    pub fn stamped(mut self) -> Self {
        self.order = POINTER_ORDER.fetch_add(1, Ordering::Relaxed);
        self
    }

    /// The pressure a device without a sensor reports: `0.5` while a
    /// button is held, `0` otherwise.
    pub fn default_pressure(buttons: u32) -> f32 {
        if buttons == 0 { 0.0 } else { 0.5 }
    }
}

impl Default for PointerData {
    /// The mouse with no button held.
    fn default() -> Self {
        Self::mouse(0)
    }
}

/// Pointer moved to a new position in window coordinates.
#[derive(Message, Clone, Copy, Debug, Default)]
pub struct PointerMoved {
    /// New position in logical pixels, top-left origin.
    pub position: Vec2,
    /// The position relative to the target's border box, when the backend
    /// measured that box itself: a page asks the browser, which lays it out.
    /// `None` leaves it to Lumen's own layout, which is what a native window
    /// backend does.
    pub local: Option<Vec2>,
    /// The device that moved, pressed or released, and its pen and touch
    /// readings.
    pub pointer: PointerData,
}

/// Pointer button pressed.
#[derive(Message, Clone, Copy, Debug, Default)]
pub struct PointerPressed {
    /// Position at time of press.
    pub position: Vec2,
    /// Which button.
    pub button: PointerButton,
    /// The position relative to the target's border box, when the backend
    /// measured that box itself: a page asks the browser, which lays it out.
    /// `None` leaves it to Lumen's own layout, which is what a native window
    /// backend does.
    pub local: Option<Vec2>,
    /// The device that moved, pressed or released, and its pen and touch
    /// readings.
    pub pointer: PointerData,
}

/// Pointer button released.
#[derive(Message, Clone, Copy, Debug, Default)]
pub struct PointerReleased {
    /// Position at time of release.
    pub position: Vec2,
    /// Which button.
    pub button: PointerButton,
    /// The position relative to the target's border box, when the backend
    /// measured that box itself: a page asks the browser, which lays it out.
    /// `None` leaves it to Lumen's own layout, which is what a native window
    /// backend does.
    pub local: Option<Vec2>,
    /// The device that moved, pressed or released, and its pen and touch
    /// readings.
    pub pointer: PointerData,
}

impl PointerPressed {
    /// The primary button of the primary pointer: the press Lumen's own
    /// widgets act on. A second finger's touch is not one.
    pub fn is_primary(&self) -> bool {
        self.button == PointerButton::Primary && self.pointer.is_primary
    }
}

impl PointerReleased {
    /// The primary button of the primary pointer: the release Lumen's own
    /// widgets act on. A second finger's lift is not one.
    pub fn is_primary(&self) -> bool {
        self.button == PointerButton::Primary && self.pointer.is_primary
    }
}

/// Pointer left the window.
#[derive(Message, Clone, Copy, Debug)]
pub struct PointerLeft;

/// The system took a pointer away mid-gesture (a touch that became an OS
/// gesture, a pen that left range while down): the web `pointercancel`. No
/// release follows.
#[derive(Message, Clone, Copy, Debug, Default)]
pub struct PointerCancelled {
    /// Last known position in logical pixels.
    pub position: Vec2,
    /// The pointer that was cancelled.
    pub pointer: PointerData,
}

/// The entity under each non-primary pointer (a second finger) that moved,
/// pressed or released this tick, keyed by pointer id.
///
/// The primary pointer's entity is the one carrying [`Hovered`]; a
/// non-primary pointer hovers nothing, so whatever hit-tests for the
/// backend (the input layer on a desktop, the browser on a page) records
/// where it is here, for the script event driver to aim its events at.
#[derive(Resource, Clone, Debug, Default)]
pub struct PointerTargets(Vec<(u64, Entity)>);

impl PointerTargets {
    /// The entity under pointer `id`, if any.
    pub fn get(&self, id: u64) -> Option<Entity> {
        self.0.iter().find(|(p, _)| *p == id).map(|(_, e)| *e)
    }

    /// Record `target` (or nothing) under pointer `id`.
    pub fn set(&mut self, id: u64, target: Option<Entity>) {
        self.0.retain(|(p, _)| *p != id);
        if let Some(e) = target {
            self.0.push((id, e));
        }
    }

    /// Forget every pointer.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Whether no pointer is recorded.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Mouse-wheel scroll event. `delta` is in logical pixels and follows the DOM
/// `WheelEvent` convention: positive y scrolls toward the end of the content
/// (the wheel turned toward the user), positive x toward the right.
/// Backends normalise line-based wheel input to pixels with a fixed 32 px/line.
#[derive(Message, Clone, Copy, Debug)]
pub struct MouseWheel {
    /// Scroll delta in logical pixels.
    pub delta: Vec2,
    /// Cursor position at the moment of the scroll, used for hit-testing.
    pub position: Vec2,
    /// The position relative to the target's border box, when the backend
    /// measured that box itself: a page asks the browser, which lays it out.
    /// `None` leaves it to Lumen's own layout, which is what a native window
    /// backend does.
    pub local: Option<Vec2>,
    /// When the event happened (see [`PointerData::time_stamp`]).
    pub time_stamp: Instant,
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

/// Cross-crate [`SystemSet`] label for the systems that write [`ClickEvent`],
/// [`DoubleClickEvent`] and [`LongPressEvent`].
///
/// `lumen-input` tags its pointer click (`dispatch_clicks`) and its Enter /
/// Space activation (`activate_focused_on_enter`) with [`Self::Producers`];
/// `lumen-primitives::press` tags its double-click and long-press detectors,
/// and the scene tags the dialog's Enter-activates-the-default-button system.
/// A system that reacts to these messages schedules itself
/// `.after(ClickSet::Producers)` in [`crate::tick::TickStage::Systems`] and
/// sees the event on the tick it fires, whichever producer wrote it. A reader
/// with no such edge may run first and see the event a tick late, and the tick
/// that fired it paints nothing new. The edge is inert when no producer is
/// installed (an empty set).
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ClickSet {
    /// Systems that write [`ClickEvent`], [`DoubleClickEvent`] or
    /// [`LongPressEvent`].
    Producers,
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
    /// The position relative to the target's border box, when the backend
    /// measured that box itself: a page asks the browser, which lays it out.
    /// `None` leaves it to Lumen's own layout, which is what a native window
    /// backend does.
    pub local: Option<Vec2>,
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

/// Declares the CSS cursor keywords once: the enum, its CSS spelling, and
/// the parse back from it.
macro_rules! cursor_keywords {
    ($($(#[$doc:meta])* $variant:ident => $css:literal,)*) => {
        /// A CSS `cursor` keyword: the standard set, which window backends
        /// map onto the platform's cursors (winit `CursorIcon`).
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        pub enum CursorKeyword {
            $($(#[$doc])* $variant,)*
        }

        impl CursorKeyword {
            /// Every keyword, in the order CSS lists them.
            pub const ALL: &'static [CursorKeyword] = &[$(CursorKeyword::$variant,)*];

            /// The keyword as CSS spells it.
            pub fn as_css(self) -> &'static str {
                match self {
                    $(CursorKeyword::$variant => $css,)*
                }
            }

            /// The keyword CSS spells `css`, ASCII case-insensitively.
            pub fn from_css(css: &str) -> Option<Self> {
                let css = css.trim();
                Self::ALL
                    .iter()
                    .copied()
                    .find(|k| k.as_css().eq_ignore_ascii_case(css))
            }
        }
    };
}

cursor_keywords! {
    /// The initial value: the platform arrow, unless a widget asks for more.
    #[default]
    Auto => "auto",
    /// The platform arrow.
    Default => "default",
    /// No cursor: the pointer is hidden over the element.
    None => "none",
    /// A context menu is available.
    ContextMenu => "context-menu",
    /// Help is available.
    Help => "help",
    /// A link: the pointing hand.
    Pointer => "pointer",
    /// Busy in the background, still interactive.
    Progress => "progress",
    /// Busy; wait.
    Wait => "wait",
    /// A cell or set of cells can be selected.
    Cell => "cell",
    /// A precise crosshair.
    Crosshair => "crosshair",
    /// Text can be selected: the I-beam.
    Text => "text",
    /// Vertical text can be selected.
    VerticalText => "vertical-text",
    /// An alias or shortcut is to be created.
    Alias => "alias",
    /// Something is to be copied.
    Copy => "copy",
    /// Something is to be moved.
    Move => "move",
    /// The dragged item cannot be dropped here.
    NoDrop => "no-drop",
    /// The action is not allowed.
    NotAllowed => "not-allowed",
    /// Something can be grabbed: an open hand.
    Grab => "grab",
    /// Something is being grabbed: a closed hand.
    Grabbing => "grabbing",
    /// An edge is to be moved east.
    EResize => "e-resize",
    /// An edge is to be moved north.
    NResize => "n-resize",
    /// An edge is to be moved north-east.
    NeResize => "ne-resize",
    /// An edge is to be moved north-west.
    NwResize => "nw-resize",
    /// An edge is to be moved south.
    SResize => "s-resize",
    /// An edge is to be moved south-east.
    SeResize => "se-resize",
    /// An edge is to be moved south-west.
    SwResize => "sw-resize",
    /// An edge is to be moved west.
    WResize => "w-resize",
    /// Resizes east and west.
    EwResize => "ew-resize",
    /// Resizes north and south.
    NsResize => "ns-resize",
    /// Resizes north-east and south-west.
    NeswResize => "nesw-resize",
    /// Resizes north-west and south-east.
    NwseResize => "nwse-resize",
    /// A column can be resized.
    ColResize => "col-resize",
    /// A row can be resized.
    RowResize => "row-resize",
    /// Something can be scrolled in any direction.
    AllScroll => "all-scroll",
    /// Something can be zoomed in.
    ZoomIn => "zoom-in",
    /// Something can be zoomed out.
    ZoomOut => "zoom-out",
}

/// One `url(...)` image in a `cursor` value, with its hotspot.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorImageRef {
    /// Where the image loads from, resolved the way an `<image src>` is.
    pub path: std::path::PathBuf,
    /// The hotspot in image pixels from the top-left corner. `None` is the
    /// corner itself.
    pub hotspot: Option<(f32, f32)>,
}

/// A computed CSS `cursor` value: the images to try in order, then the
/// keyword to show when none of them loads.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct Cursor {
    /// The `url(...)` images, in the order they were written.
    pub images: std::sync::Arc<[CursorImageRef]>,
    /// The keyword the value ends with.
    pub keyword: CursorKeyword,
}

impl Cursor {
    /// A cursor that is only a keyword.
    pub fn keyword(keyword: CursorKeyword) -> Self {
        Self {
            images: std::sync::Arc::new([]),
            keyword,
        }
    }
}

/// A decoded custom cursor image, ready for a window backend to hand the
/// platform.
#[derive(Clone, Debug)]
pub struct CursorImage {
    /// Stable identity of the decoded pixels, so a backend can keep the
    /// platform cursor it made from them.
    pub id: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Straight RGBA8 pixels, row-major.
    pub rgba: std::sync::Arc<[u8]>,
    /// The hotspot in pixels, inside the image.
    pub hotspot: (u32, u32),
}

impl PartialEq for CursorImage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.hotspot == other.hotspot
    }
}

/// Requested mouse cursor, written main-side each tick (see
/// `lumen_primitives::update_cursor_request`) and applied to the OS
/// window by the window backend, which tracks the last applied value so
/// the OS call only happens on change. Headless runners simply never
/// read it.
///
/// `cursor` is what the pointer should show: the hovered element's
/// computed CSS `cursor`, or what a widget handle under a drag asks for.
/// `image` is the first of its images the asset pipeline has decoded; the
/// backend shows it when present and the keyword otherwise.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct CursorRequest {
    /// The wanted cursor.
    pub cursor: Cursor,
    /// The decoded image to show in place of the keyword, when one loaded.
    pub image: Option<CursorImage>,
}

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
    /// Parses a button name case-insensitively. Recognised values: `"primary"` / `"left"`, `"secondary"` / `"right"`, `"middle"`, `"back"`, `"forward"`, `"eraser"`. Unknown names yield `Primary`.
    fn from(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "secondary" | "right" => PointerButton::Secondary,
            "middle" => PointerButton::Middle,
            "back" => PointerButton::Back,
            "forward" => PointerButton::Forward,
            "eraser" => PointerButton::Eraser,
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
    /// When the event happened (see [`PointerData::time_stamp`]).
    pub time_stamp: Instant,
}

/// Raw global key-release event.
#[derive(Message, Clone, Debug)]
pub struct KeyReleased {
    /// Logical key.
    pub key: Key,
    /// Modifier state at time of release.
    pub modifiers: Modifiers,
    /// When the event happened (see [`PointerData::time_stamp`]).
    pub time_stamp: Instant,
}

/// Marker placed by `lumen-input`'s focus router on the keyboard-focused entity. The router maintains at most one [`Focused`] at a time.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Focused;

/// Marker on a focused single-line text field whose edit Enter committed, until
/// the next key reaches it. A field with an edit in flight holds back signal
/// writes so a script cannot clobber the typing; a committed field has none in
/// flight, so [`crate::signals::apply_text_bindings`] gives it the signal's
/// value while it keeps focus, and a submit handler that clears the signal
/// clears the field.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct EditCommitted;

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

/// A control that is not a text field changed its value: a color picker
/// moved to a new color, or a color field the browser drew picked one. The
/// script event driver raises it as the DOM `input` event, or as `change`
/// when `committed`: the user settled on the value the `input`s led to.
///
/// A text field's edits are [`crate::text_events::TextEditApplied`] and its
/// commits [`TextInputCommitted`] instead.
#[derive(Message, Clone, Debug)]
pub struct ControlInput {
    /// The control.
    pub entity: Entity,
    /// Its value as the DOM's `value` reads it, `#rrggbb` for a color.
    pub value: String,
    /// Whether this is the commit of a value earlier `input`s already
    /// reported, which is what `change` is: a color panel closing on a new
    /// color.
    pub committed: bool,
}

/// The user asked for a context menu on `entity`; the script event driver
/// raises it as the DOM `contextmenu` event.
///
/// - Producers: `lumen-input` for a secondary-button press (a release on
///   Windows, where the platform opens context menus on release, as a
///   browser there does) on the hovered entity, and for the ContextMenu key
///   or Shift+F10 on the focused entity; the accessibility bridge for an
///   assistive technology's `ShowContextMenu` action; a page's backend for
///   the browser's own `contextmenu` event.
/// - Consumers: the DOM event driver (`contextmenu` handlers) and any
///   `MessageReader<ContextMenuEvent>` in an app.
/// - Mirrors the DOM `contextmenu` event and Qt's
///   `QWidget::customContextMenuRequested`.
#[derive(Message, Clone, Copy, Debug)]
pub struct ContextMenuEvent {
    /// The element the menu is for.
    pub entity: Entity,
    /// Where a menu opened for this request belongs, in window
    /// coordinates: the pointer for a pointer request, below the element's
    /// leading edge for a keyboard or assistive-technology request.
    pub position: Vec2,
    /// [`PointerButton::Secondary`] for a pointer request and
    /// [`PointerButton::Primary`] otherwise, which is what the DOM event's
    /// `button` reports (`2` and `0`).
    pub button: PointerButton,
    /// The position relative to the target's border box, when the backend
    /// measured that box itself (a page). `None` leaves it to Lumen's own
    /// layout.
    pub local: Option<Vec2>,
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
