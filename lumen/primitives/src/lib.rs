//! Headless interaction primitives: scroll, drag, press, focus, resize.
//!
//! - Headless: state machines and per-OS physics constants only, with no visual styling.
//! - Apps style by applying CSS skins or by assigning [`Visuals`] directly.
//! - Scroll: each [`MouseWheel`] accumulates into the offset of the hovered scroll container; per-OS physics constants live in [`physics`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod baseline;
pub mod checkbox;
pub mod controls;
pub mod cursor;
pub mod drag;
pub mod hover;
pub mod physics;
pub mod popup;
pub mod popup_nav;
pub mod press;
pub mod progress;
pub mod radio;
pub mod scroll;
pub mod scrollbar;
pub mod state_style;
pub mod switch;
pub mod tabs;
pub mod tooltip;
pub mod transition;
pub mod validation;
pub mod wake;

pub use checkbox::{
    CheckboxBaseFill, CheckboxBox, CheckboxPlugin, CheckboxStyle, Indeterminate,
    clear_indeterminate_on_user_toggle, sync_checkbox_visuals,
};
pub use controls::{
    ControlsPlugin, KnobGeometry, SliderChanged, SliderDragOrigin, SliderThumb, ToggleChanged,
    ToggleKnob, ToggleStyle, WHEEL_NOTCH_PX, adjust_slider_on_wheel, cancel_slider_drag_on_escape,
    sync_slider_thumb, sync_toggle_visuals,
};
pub use cursor::{CursorPlugin, update_cursor_request};
pub use drag::{DragConfig, DragPlugin, DragState, Draggable};
pub use hover::{
    BaseBorder, FocusOutlineSpec, HoverBaseColor, HoverTintPlugin, HoverTween, Interaction,
    PressBaseColor, PressTween, apply_state_borders,
};
pub use lumen_core::prelude::{Scroll, ScrollAxis, ScrollOffset};
pub use popup::{
    PopupGap, PopupPanel, PopupSide, anchored_popup_origin, dismiss_popups_on_outside_press,
    flip_open_dropdown_panels,
};
pub use popup_nav::{
    ActivePopupNav, PopupNavConfig, PopupNavSession, TypeAheadBuffer, closed_dropdown_keys,
    follow_hover_highlight, popup_nav_keys, popup_nav_lifecycle,
};
pub use press::{PressConfig, PressPlugin, PressStartedAt};
pub use progress::{
    PROGRESS_PERIOD_MS, ProgressBar, ProgressChunk, ProgressFill, ProgressPlugin,
    apply_progress_bindings, sync_progress_fill,
};
pub use radio::{
    RadioBaseFill, RadioButton, RadioDot, RadioPlugin, RadioStyle, dispatch_radio_clicks,
    radio_group_keys, sync_radio_selected, sync_radio_tab_index, sync_radio_visuals,
};
pub use scroll::{ScrollPlugin, accumulate_wheel};
pub use scrollbar::update_scrollbars;
pub use state_style::{
    StatePatch, StateStylePlugin, StateVisuals, apply_state_visuals, eject_interaction_on_disable,
};
pub use switch::{
    SWITCH_SLIDE_EASING, SWITCH_SLIDE_MS, SwitchPlugin, SwitchStyle, SwitchThumb, SwitchThumbSlide,
    register_switch_systems, step_switch_thumb, sync_switch_visuals,
};
pub use tabs::{
    DropdownButton, DropdownOptionButton, DropdownOptionSpec, MenuItemButton, TAB_SELECTED_BG,
    TabButtonStyle, TabStripButton, TabsPlugin, sync_tab_button_visuals, sync_tab_selected,
};
pub use tooltip::{
    HoverStartedAt, TooltipPlugin, TooltipPopup, TooltipSource, cursor_tooltip_origin,
};
pub use transition::{
    BackgroundTransition, BorderColorTransition, Easing, Lerp, OpacityTransition,
    TextColorTransition, Transition, TransitionPlugin, TransitionProperty, TransitionSpec,
    TransitionSpecs, retarget,
};
pub use validation::{ValidationPlugin, apply_validation, evaluate, matches_pattern};
