//! Per-OS physics constants used by the scroll, drag, and press primitives.
//!
//! Values sourced from `NSScrollView` (macOS) and `IInertiaProcessor` (Windows); see `PHYSICS.md`.

/// Inertia decay (1/s). Higher = stops faster. Per-OS.
#[allow(clippy::if_same_then_else)]
pub const INERTIA_DECAY: f32 = if cfg!(target_os = "macos") {
    6.0
} else if cfg!(target_os = "windows") {
    8.0
} else {
    8.0
};

/// Rubber-band stiffness applied when scroll position exceeds bounds.
/// Higher = stiffer pullback. Macs use rubber-band; Windows/Linux clamp.
pub const RUBBER_BAND_STIFFNESS: f32 = if cfg!(target_os = "macos") { 0.55 } else { 0.0 };

/// Lines-per-wheel-detent -> pixels conversion (used by the winit bridge to
/// normalize `MouseScrollDelta::LineDelta`).
pub const LINE_HEIGHT_PX: f32 = 32.0;
