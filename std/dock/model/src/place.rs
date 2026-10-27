//! Where a window goes on the monitors present now, and how its place is
//! written down so it can go there again.
//!
//! A saved rectangle is relative to its monitor, in that monitor's logical
//! pixels, with the monitor's name, origin and scale beside it. On restore
//! the window goes back to the monitor of that name (else the one at that
//! origin) at the same offset, kept inside the monitor's work area; with
//! neither present it is centred on the first monitor. A platform that
//! cannot position windows ignores the position and still gets the size.

use crate::{MonitorKey, Rect};

/// One monitor of the desktop, in the terms placement needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    /// The platform's name for it.
    pub name: String,
    /// Top-left corner in physical desktop pixels.
    pub origin: [i32; 2],
    /// Size in physical pixels.
    pub size: [u32; 2],
    /// Where windows go, in physical desktop pixels: top-left and size.
    pub work: ([i32; 2], [u32; 2]),
    /// Logical to physical multiplier.
    pub scale: f32,
}

impl Monitor {
    fn contains(&self, p: [i32; 2]) -> bool {
        p[0] >= self.origin[0]
            && p[1] >= self.origin[1]
            && p[0] < self.origin[0] + self.size[0] as i32
            && p[1] < self.origin[1] + self.size[1] as i32
    }
}

/// Where to open a window.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    /// The outer top-left corner in physical desktop pixels; `None` when no
    /// monitor is known.
    pub position: Option<[i32; 2]>,
    /// The inner size in logical pixels.
    pub size: [f32; 2],
}

/// Place a window that was at `rect` on `monitor`, on the `monitors` present
/// now. `fallback` is the inner size, in logical pixels, of a window with no
/// saved rectangle.
pub fn place(
    rect: Option<Rect>,
    monitor: Option<&MonitorKey>,
    monitors: &[Monitor],
    fallback: [f32; 2],
) -> Placement {
    let size = rect.map_or(fallback, |r| [r.w.max(1.0), r.h.max(1.0)]);
    let saved_on = monitor.and_then(|key| {
        monitors
            .iter()
            .find(|m| m.name == key.name)
            .or_else(|| monitors.iter().find(|m| m.origin == key.origin))
    });
    let (on, offset) = match (saved_on, rect) {
        (Some(m), Some(r)) => (m, Some([r.x, r.y])),
        _ => match monitors.first() {
            Some(m) => (m, None),
            None => {
                return Placement {
                    position: None,
                    size,
                };
            }
        },
    };
    let scale = on.scale.max(0.01);
    let outer = [(size[0] * scale) as i32, (size[1] * scale) as i32];
    let (work_at, work_size) = on.work;
    let work_end = [
        work_at[0] + work_size[0] as i32,
        work_at[1] + work_size[1] as i32,
    ];
    let wanted = match offset {
        Some([x, y]) => [
            on.origin[0] + (x * scale) as i32,
            on.origin[1] + (y * scale) as i32,
        ],
        None => [
            work_at[0] + (work_size[0] as i32 - outer[0]) / 2,
            work_at[1] + (work_size[1] as i32 - outer[1]) / 2,
        ],
    };
    // Inside the work area: the far edges first, then the near ones, so a
    // window larger than the area keeps its top-left corner on screen.
    let clamp = |v: i32, axis: usize| v.min(work_end[axis] - outer[axis]).max(work_at[axis]);
    Placement {
        position: Some([clamp(wanted[0], 0), clamp(wanted[1], 1)]),
        size,
    }
}

/// Write down where a window is: `position` is its outer top-left corner in
/// physical desktop pixels (`None` where the platform does not tell), `size`
/// its inner size in logical pixels. A window whose position is unknown
/// keeps the offset and monitor of `previous`, so a layout saved where
/// positions are not reported (Wayland) still restores in full elsewhere.
pub fn record(
    position: Option<[i32; 2]>,
    size: [f32; 2],
    monitors: &[Monitor],
    previous: Option<(Option<Rect>, Option<MonitorKey>)>,
) -> (Rect, Option<MonitorKey>) {
    let on = position.and_then(|p| monitors.iter().find(|m| m.contains(p)));
    match (position, on) {
        (Some(p), Some(m)) => {
            let scale = m.scale.max(0.01);
            (
                Rect {
                    x: (p[0] - m.origin[0]) as f32 / scale,
                    y: (p[1] - m.origin[1]) as f32 / scale,
                    w: size[0],
                    h: size[1],
                },
                Some(MonitorKey {
                    name: m.name.clone(),
                    origin: m.origin,
                    scale: m.scale,
                }),
            )
        }
        _ => {
            let (rect, key) = previous.unwrap_or((None, None));
            let r = rect.unwrap_or_default();
            (
                Rect {
                    x: r.x,
                    y: r.y,
                    w: size[0],
                    h: size[1],
                },
                key,
            )
        }
    }
}
