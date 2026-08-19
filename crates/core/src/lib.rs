//! Lumen UI framework core crate.
//!
//! - Owns the tick loop, [`command`] queue, ECS components, and the two ECS worlds (main + render). See [`render_world`] for the cross-world flow.
//! - Backend traits are marker types; concrete backends register systems via [`app::Plugin`].
//! - Hierarchy uses [`bevy_ecs::hierarchy::ChildOf`] and [`bevy_ecs::hierarchy::Children`], re-exported from [`prelude`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod command;
pub mod components;
pub mod i18n;
pub mod input;
pub mod introspect;
pub mod nav;
pub mod net_capture;
pub mod node;
pub mod node_ir;
pub mod output;
pub mod palette;
pub mod property_store;
pub mod render_world;
pub mod request;
pub mod signals;
pub mod task;
pub mod text_events;
pub mod text_model;
pub mod tick;
pub mod time;
pub mod traits;
pub mod window;
pub mod window_state;

/// Crate prelude re-exporting the common types. Glob-import with `use lumen_core::prelude::*;`.
pub mod prelude {
    pub use crate::app::{App, AppError, EventLoopWaker, Plugin, PluginMetadata};
    pub use crate::command::{
        Command, CommandHandlerFn, CommandQueue, CommandQueueOverflow, CommandRegistry,
        apply_property_commands,
    };
    #[allow(deprecated)]
    pub use crate::components::OsTheme;
    pub use crate::components::{
        A11yAnnouncement, A11yAnnouncementQueue, A11yContextMenuRequests, A11yDescription,
        A11yLabel, A11yLevel, A11yLive, A11yRelations, A11yRole, A11yRootLabel,
        A11yScrollIntoViewRequests, A11ySetSize, A11yState, A11yValue, AlignContent, BindChecked,
        BindDisabled, BindParentChecked, BindParentText, BindParentValue, BindSelfChecked,
        BindSelfText, BindSelfValue, BindText, BindValue, Border, BoxSizing, CaretBlink, Color,
        ColorScheme, DefaultLayoutDirection, DirtyA11y, DirtyLayout, Disabled, DocumentOrder,
        DropHovered, DropTarget, Edges, Fill, FlexAlign, FlexDirection, FlexJustify, FlexWrap,
        FocusBoundary, ImageBlob, ImageComponent, ImageFit, ImeState, Lang, LayoutDirection,
        Length, LumenClasses, LumenId, LumenTag, MemoryBudget, Opacity, Overflow,
        PendingA11yUpdate, Position, RelayoutBoundary, ResolvedDirection, RootWindowEntity,
        Selected, ShadowSpec, SliderValue, Style, StyleManager, StyleVersion, SvgPayload, TabIndex,
        TextAlign, TextContent, TextInput, TextInputPaint, TextInputScroll, TextStyle, TextWrap,
        TitleBarDraggable, Toggleable, Transform, Validation, Visible, Visuals, WindowDragRequest,
        ZIndex, apply_bind_parent_checked, apply_bind_parent_text, apply_bind_parent_value,
        apply_bind_self_checked, apply_bind_self_text, apply_bind_self_value, hidden_via_ancestors,
        resolve_layout_direction,
    };
    pub use crate::input::{
        ClickEvent, CursorRequest, CursorShape, DoubleClickEvent, DragEndEvent, DragMoveEvent,
        DragStartEvent, EscapePressCancel, FileDropped, FileHoverCancelled, FileHovered,
        FocusTracker, Focused, FocusedKey, Hovered, ImeEvent, ImeRequest, Key, KeyPressed,
        KeyReleased, LongPressEvent, Modifiers, ModifiersState, MouseWheel, NamedKey,
        PointerButton, PointerLeft, PointerMoved, PointerPressed, PointerReleased, PointerState,
        Pressed, SCROLLBAR_MARGIN, SCROLLBAR_MIN_THUMB, SCROLLBAR_THICKNESS,
        SCROLLBAR_THICKNESS_THIN, Scroll, ScrollAxis, ScrollOffset, ScrollbarAxisPick,
        ScrollbarDrag, ScrollbarGeometry, ScrollbarInteraction, ScrollbarMetrics, ScrollbarPart,
        ScrollbarState, ScrollbarStyle, ScrollbarWidthMode, ShowContextMenu, TextInputCommitted,
        horizontal_scrollbar, vertical_scrollbar,
    };
    pub use crate::nav::{NavOp, navigate as nav_navigate, request as nav_request};
    pub use crate::node::{
        DomIndex, DomRecord, NodeHandle, NodeHandles, dom_index_snapshot, intern_node,
        publish_dom_index, resolve_node,
    };
    pub use crate::node_ir::{
        Affine2, ClipShape, DrawEntry, Node, PreviousScene, RetainedScene,
        transform_extracted_to_nodes,
    };
    pub use crate::palette::Palette;
    pub use crate::property_store::{
        BindingId, ListenerId, Property, PropertyCell, PropertyKey, PropertyStore, PropertyValue,
        clear_property_store_dirty,
    };
    pub use crate::render_world::{
        AnimationsActive, Brush, ExtractFn, ExtractSchedule, ExtractSet, ExtractedClipBox,
        ExtractedImage, ExtractedOutline, ExtractedRect, ExtractedScrollbar, ExtractedShadow,
        ExtractedText, FrameDamage, FrameDirty, Rect, Render, RenderStage, ScrollbarDrawRect,
        SurfaceCapture, SurfaceFrame, Viewport,
    };
    #[allow(deprecated)]
    pub use crate::signals::Signals;
    pub use crate::signals::{
        ArrayItem, ArraySignals, ExternalMutation, apply_text_bindings, drain_external_signals,
        init_external_signals, push_external_array, push_external_clear, push_external_signal,
        push_textinput_to_signal,
    };
    pub use crate::task::{BoxFuture, SpawnService, TimerService};
    pub use crate::text_events::{
        Anchor, AppliedKind, CursorMotion, ImeSurroundingRequested, ImeSurroundingResponse,
        MoveMode, RejectReason, TextEditApplied, TextEditRejected, TextEditRequest, TextEditSet,
    };
    pub use crate::text_model::{
        Affinity, Bias, ImePreedit, MarkId, TagKind, TextBuffer, TextBufferKind, TextCursor,
        TextEditable, TextMark, TextPos, TextTagRange,
    };
    pub use crate::tick::{Tick, TickStage};
    pub use crate::traits::{
        A11yBackend, Bindable, FrameRequest, LayoutEngine, RenderTarget, Renderer, Spawn,
        SurfaceError, SurfaceRenderer, Timer, WindowBackend,
    };

    // Re-export the bevy_ecs hierarchy components used across the crate.
    pub use bevy_ecs::hierarchy::{ChildOf, Children};
    pub use bevy_ecs::prelude::*;
}
