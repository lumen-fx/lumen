//! Hardcoded documentation strings for Lumen tags and attributes.
//!
//! Kept here (not extracted from rustdoc) so the LSP stays self-contained.
//! Sync with `lumenc::parser_html` if the supported set changes.

/// Recognized layout tag names accepted by `lumenc::parse_html`.
///
/// `script` is intentionally omitted: it is a parser-special-cased
/// non-layout element. Including it in completion would mislead users
/// into treating it like a layout node.
pub const TAGS: &[&str] = &[
    "root",
    "column",
    "row",
    "scroll",
    "tile",
    "label",
    "div",
    "image",
    "input",
    "spacer",
    "dialog",
    "template",
    "for",
    "if",
    "overlay",
    "button",
    "toggle",
    "slider",
    "checkbox",
    "radio",
    "progress",
    "title-bar",
];

/// Recognized attribute names. Subset that `lumenc::parser_html` knows
/// how to validate (other attributes are quietly ignored today but we
/// don't surface them as completions - explicit is better than implicit).
pub const ATTRS: &[&str] = &[
    // sizing / layout
    "width",
    "height",
    "min-width",
    "min-height",
    "max-width",
    "max-height",
    "aspect-ratio",
    "flex",
    "grow",
    "shrink",
    "gap",
    "align",
    "justify",
    "padding",
    "margin",
    "position",
    "inset",
    "overflow",
    "overflow-x",
    "overflow-y",
    // visuals
    "bg",
    "radius",
    "border",
    "z-index",
    "shadow",
    "opacity",
    "text",
    "text-color",
    "selection-color",
    "text-align",
    "font-size",
    "font-family",
    "font-weight",
    "knob-color",
    "style",
    "placeholder",
    "wrap",
    "max-lines",
    "text-overflow",
    // interaction state
    "hover-bg",
    "press-bg",
    "focus-outline",
    "draggable",
    "drop",
    "drag",
    "tab-index",
    "fit",
    // scroll
    "scroll",
    "sensitivity",
    "inertia",
    // identity
    "id",
    "class",
    // form controls
    "label",
    "group",
    "value",
    "checked",
    "indeterminate",
    "duration",
    // reactivity / templates
    "bind-text",
    "bind-checked",
    "bind-value",
    "bind-scroll",
    "signal",
    "each",
    "key",
    "mode",
    "name",
    "src",
    "template",
    // misc
    "frameless",
    "skin",
];

/// Markdown documentation for a tag. Returns `None` for unknown tags.
pub fn tag_doc(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "root" => {
            "**`<root>`** - Top-level Lumen element. \
Defaults to `width=\"100%\" height=\"100%\" flex=\"column\"`. \
Exactly one `<root>` per markup file."
        }
        "column" => "**`<column>`** - Vertical flex container. Defaults to `flex=\"column\"`.",
        "row" => "**`<row>`** - Horizontal flex container. Defaults to `flex=\"row\"`.",
        "scroll" => {
            "**`<scroll>`** - Scrollable container. Defaults to `scroll=\"y\"` and `flex=\"column\"`. \
Carries `Scroll` + `ScrollOffset` components at runtime."
        }
        "tile" => "**`<tile>`** - Styled, clickable box. Suitable for buttons, cards, list rows.",
        "label" => {
            "**`<label>`** - Text-bearing node. Inner text becomes the `text` attribute when not set explicitly."
        }
        "div" => "**`<div>`** - Generic container with no defaults.",
        "image" => {
            "**`<image>`** - Bitmap or SVG. Attributes: `src`, `fit`, `width`, `height`, `opacity`."
        }
        "input" => {
            "**`<input>`** - Single-line text input. `placeholder`, `bind-text`, `on_text_input`."
        }
        "spacer" => "**`<spacer>`** - Flex spacer. Expands to fill remaining axis space.",
        "template" => {
            "**`<template name=\"...\">`** - Reusable subtree. Instantiate as a tag matching the name; `{slot}` placeholders fill from attrs."
        }
        "for" => {
            "**`<for each=\"item in signal\" template=\"row\" key=\"id\">`** - Keyed list reconciler driven by an `ArraySignals` entry."
        }
        "if" => {
            "**`<if signal=\"name\" mode=\"render|hide\">`** - Conditional subtree gated on a truthy signal."
        }
        "overlay" => {
            "**`<overlay>`** - Absolute-positioned layer mounted at the top of the tree. Inherits `inset`."
        }
        "dialog" => {
            "**`<dialog open=\"signal\">`** - Modal overlay. Sugar for `<overlay>` + `<if signal=... mode=\"hide\">`. Centers children; preserves descendant state across show/hide."
        }
        "button" => "**`<button>`** - Clickable control. Fires `on_click(id)`.",
        "toggle" => {
            "**`<toggle>`** - Boolean control. `bind-checked`, fires `on_toggle(id, checked)`."
        }
        "slider" => {
            "**`<slider min max value step>`** - Scalar range. `bind-value`, fires `on_slider(id, value)`. `step` sets the keyboard/wheel increment (default `(max-min)/100`)."
        }
        "checkbox" => {
            "**`<checkbox label=\"...\">`** - Box + label bool control. `checked`, `bind-checked`, fires `on_toggle(id, checked)`; click or Space toggles. `indeterminate=\"true\"` renders a dash until the first user toggle clears it."
        }
        "radio" => {
            "**`<radio group=\"g\" value=\"v\" label=\"...\">`** - Name-grouped exclusive choice. The group's selected value lives in the `g` signal; `checked=\"true\"` seeds it. Arrow keys move selection within the group (wrapping, skipping disabled); Tab enters/leaves the group as one stop (roving tabindex)."
        }
        "progress" => {
            "**`<progress value max>`** - Progress bar. With `value` / `bind-value`: determinate fill at `value / max`. Without: indeterminate animated sweep (`duration=` ms, token `--lumen-progress-period`). Not focusable, no interaction."
        }
        "title-bar" => {
            "**`<title-bar drag=\"true\">`** - Custom title-bar region for `<root frameless=\"true\">`. With `drag=\"true\"` pressing the bar requests a native window drag via the platform backend."
        }
        _ => return None,
    })
}

/// Markdown documentation for an attribute name. Returns `None` for
/// unknown attributes.
pub fn attr_doc(attr: &str) -> Option<&'static str> {
    Some(match attr {
        "width" => "**`width`** - `auto` | `<n>px` | `<n>%`. Element width.",
        "height" => "**`height`** - `auto` | `<n>px` | `<n>%`. Element height.",
        "flex" => "**`flex`** - `row` | `column`. Flex direction for children.",
        "bg" => "**`bg`** - `#rrggbb` or `#rrggbbaa`. Background color.",
        "radius" => {
            "**`radius`** - 1-4 pixel values. One value = uniform corner radius; 2-4 follow the CSS `border-radius` rotation `[top-left, top-right, bottom-right, bottom-left]` (`radius: 4 4 0 0` rounds only the top). CSS-side longhands: `border-<corner>-radius`."
        }
        "padding" => "**`padding`** - `<n>` (uniform) or `<l> <r> <t> <b>` (px).",
        "margin" => "**`margin`** - `<n>` (uniform) or `<l> <r> <t> <b>` (px).",
        "text" => "**`text`** - text content. Equivalent to placing text between tags.",
        "text-color" => "**`text-color`** - `#rrggbb` or `#rrggbbaa`. Glyph color.",
        "selection-color" => {
            "**`selection-color`** - `#rrggbb` or `#rrggbbaa`. Text-selection highlight \
             color on `<input>` / `<textarea>`; the default skin routes it through \
             `--lumen-selection`. Unset = text color at 32 % alpha."
        }
        "scroll" => "**`scroll`** - `y` | `x` | `both`. Scroll axis; implied `y` on `<scroll>`.",
        "sensitivity" => "**`sensitivity`** - `f32`. Scroll wheel sensitivity multiplier.",
        "inertia" => "**`inertia`** - `f32`. Scroll inertia factor (0 = no inertia).",
        "tab-index" => "**`tab-index`** - `i32`. Focus order (lower = earlier).",
        "id" => "**`id`** - string. Emits a `LumenId` marker for lookup.",
        "class" => "**`class`** - whitespace-separated class names for CSS matching.",
        "hover-bg" => "**`hover-bg`** - `#rrggbb` or `#rrggbbaa`. Background while hovered.",
        "draggable" => {
            "**`draggable`** - `true` | `false`. When true, drag input translates the element."
        }
        "min-width" => "**`min-width`** - `<n>px` | `<n>%`. Lower bound on width.",
        "min-height" => "**`min-height`** - `<n>px` | `<n>%`. Lower bound on height.",
        "max-width" => "**`max-width`** - `<n>px` | `<n>%`. Upper bound on width.",
        "max-height" => "**`max-height`** - `<n>px` | `<n>%`. Upper bound on height.",
        "aspect-ratio" => "**`aspect-ratio`** - `<f32>`. Width / height ratio.",
        "grow" => "**`grow`** - `<f32>`. Flex grow factor (CSS `flex-grow`).",
        "shrink" => "**`shrink`** - `<f32>`. Flex shrink factor (CSS `flex-shrink`; default 1).",
        "border" => {
            "**`border`** - `<width> [solid|none] <#color>` (any order), e.g. `1px solid #444`. \
Real CSS border: consumes layout space per the box model (`box-sizing: border-box` default) \
and paints inside the border box. CSS-side longhands: `border-width` (1-4 values, per side), \
`border-color`, `border-style` (`none` | `solid`), `border-<side>-width`, plus \
`hover-border` / `focus-border` (or `:hover { border: ... }` / `:focus { border: ... }`) for state swaps."
        }
        "z-index" => {
            "**`z-index`** - `i32`. Sibling paint-order override: higher paints on top; \
equal values keep document order (CSS stacking within the parent)."
        }
        "gap" => "**`gap`** - `<n>px`. Spacing between siblings along the main axis.",
        "align" => {
            "**`align`** - `start | end | center | stretch | between`. Cross-axis alignment."
        }
        "justify" => {
            "**`justify`** - `start | end | center | between | around`. Main-axis alignment."
        }
        "position" => "**`position`** - `static | relative | absolute`. Layout positioning mode.",
        "inset" => "**`inset`** - `<t> <r> <b> <l>` (px). Offsets for absolute children.",
        "overflow" => "**`overflow`** - `visible | hidden | scroll`. Both axes.",
        "overflow-x" => "**`overflow-x`** - `visible | hidden | scroll`.",
        "overflow-y" => "**`overflow-y`** - `visible | hidden | scroll`.",
        "shadow" => {
            "**`shadow`** - `[inset] <dx> <dy> <blur> [<spread>] <color>`. Drop / inset shadow; spread inflates the shadow rect before blurring (`0 0 0 2 #fff` = hard 2px ring)."
        }
        "font-size" => "**`font-size`** - pixels. Text size; inherited.",
        "font-family" => {
            "**`font-family`** - CSS fallback chain, e.g. `\"Segoe UI\", sans-serif`. First family present in the system font database wins; generic keywords (`sans-serif`, `serif`, `monospace`, ...) map to platform families. Inherited."
        }
        "font-weight" => {
            "**`font-weight`** - `normal` (400) | `bold` (700) | `1..=1000`. Selects the nearest face / variable-font weight. Inherited."
        }
        "knob-color" => {
            "**`knob-color`** - `<#color>`. Fill of a `<toggle>` knob / `<slider>` thumb child (Lumen-native analog property; the child is not selector-reachable). Skins seed a default."
        }
        "opacity" => {
            "**`opacity`** - `[0..1]`. Multiplies alpha of everything drawn for the entity."
        }
        "text-align" => "**`text-align`** - `start | center | end`.",
        "wrap" => {
            "**`wrap`** - `normal | nowrap | glyph | ellipsis`. Line-wrap mode; `ellipsis` elides overflowing single-line text with `...`."
        }
        "max-lines" => "**`max-lines`** - `<u32>`. Truncate after N lines with `...` glyph.",
        "text-overflow" => {
            "**`text-overflow`** - `clip | ellipsis` (CSS). `ellipsis` elides overflowing \
single-line text with a trailing `...` (Qt elide contract); combine with `max-lines` for a \
multi-line clamp. Not inherited."
        }
        "press-bg" => "**`press-bg`** - color. Background while pressed (active).",
        "focus-outline" => {
            "**`focus-outline`** - `<width>px <color>`. Outline ring when focused (any source). Use `:focus-visible { outline: ... }` for a keyboard-only ring and `outline-offset` for a gap between box edge and ring."
        }
        "drop" => "**`drop`** - `true` | `false`. Accept dropped files; fires `on_file_dropped`.",
        "drag" => {
            "**`drag`** - `true` | `false` on `<title-bar>`. Initiates a native window drag when pressed."
        }
        "fit" => "**`fit`** - `fill | cover | contain | none | scale-down`. Image fit mode.",
        "style" => "**`style`** - typography token (display-xl, headline-md, body-md, ...).",
        "placeholder" => "**`placeholder`** - string shown when an input is empty.",
        "label" => {
            "**`label`** - string. Visible caption for `<checkbox>` / `<radio>` (rendered as a `.checkbox-label` / `.radio-label` child)."
        }
        "group" => {
            "**`group`** - signal name. Radio group membership: all `<radio group=\"g\">` share one exclusive selection stored in the `g` signal."
        }
        "value" => {
            "**`value`** - number for `<slider>` / `<progress>`; the member's string value for `<radio>`."
        }
        "checked" => {
            "**`checked`** - `true` | `false`. Initial state for `<toggle>` / `<checkbox>`; on `<radio>`, seeds the group's selected value."
        }
        "indeterminate" => {
            "**`indeterminate`** - `true` | `false`. `<checkbox>` tri-state dash; cleared by the first user toggle (web `indeterminate` / Qt `PartiallyChecked`)."
        }
        "duration" => {
            "**`duration`** - milliseconds. `<progress>` indeterminate sweep period (CSS `progress-duration`, token `--lumen-progress-period`)."
        }
        "bind-text" => "**`bind-text`** - `signal-name`. Two-way binding to a string signal.",
        "bind-checked" => "**`bind-checked`** - `signal-name`. Two-way binding to a bool signal.",
        "bind-value" => "**`bind-value`** - `signal-name`. Two-way binding to a slider value.",
        "bind-scroll" => {
            "**`bind-scroll`** - `signal-name`. Two-way binding between an f32 signal (logical px) and a `<scroll>` container's vertical offset: the signal drives the offset reactively (no per-frame hooks), and user scrolling writes the settled offset back."
        }
        "signal" => "**`signal`** - name of an `ArraySignals` / `Signals` entry driving an `<if>`.",
        "each" => "**`each`** - `item in <signal>`. Drives `<for>` iteration.",
        "key" => "**`key`** - field name used as the stable id for keyed list reconciliation.",
        "mode" => {
            "**`mode`** - `<if>` mode: `render` (despawn/respawn) or `hide` (toggle visible)."
        }
        "name" => "**`name`** - `<template name=\"...\">` or `<theme>` token name.",
        "src" => "**`src`** - path or URL. Used by `<image>`, `<script>`.",
        "template" => "**`template`** - name of a `<template>` to instantiate inside `<for>`.",
        "frameless" => "**`frameless`** - `true` | `false`. Suppress OS chrome on `<root>`.",
        "skin" => "**`skin`** - name of an embedded skin (e.g. `default`).",
        _ => return None,
    })
}

/// Suggested values for a given attribute's value position, if known.
/// Empty slice means "free-form value, no completions".
pub fn attr_value_completions(attr: &str) -> &'static [&'static str] {
    match attr {
        "flex" => &["row", "column"],
        "scroll" => &["y", "x", "both"],
        "draggable" => &["true", "false"],
        "drop" => &["true", "false"],
        "drag" => &["true", "false"],
        "frameless" => &["true", "false"],
        "position" => &["static", "relative", "absolute"],
        "align" => &["start", "end", "center", "stretch", "between"],
        "justify" => &["start", "end", "center", "between", "around"],
        "overflow" | "overflow-x" | "overflow-y" => &["visible", "hidden", "scroll"],
        "text-align" => &["start", "center", "end"],
        "wrap" => &["normal", "nowrap", "glyph", "ellipsis"],
        "text-overflow" => &["clip", "ellipsis"],
        "fit" => &["fill", "cover", "contain", "none", "scale-down"],
        "mode" => &["render", "hide"],
        "checked" => &["true", "false"],
        "indeterminate" => &["true", "false"],
        "skin" => &["default"],
        _ => &[],
    }
}
