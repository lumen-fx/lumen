//! The link's handle on the `window-winit-drag` capability.
//!
//! One constructor, in a crate of its own so it lands in an object of its
//! own: a link that names the register symbol pulls this object and, through
//! it, the drag across windows on the winit backend; a link that does not
//! carries none of either. Anything more in here would give the linker
//! another reason to pull it.

#![forbid(unsafe_code)]

use lumen_capability::{Phase, lumen_capability};

lumen_capability!(
    "window-winit-drag",
    Phase::Backends,
    lumen_window_winit_drag::install,
    select = lumen_window_winit_drag::SELECT
);
