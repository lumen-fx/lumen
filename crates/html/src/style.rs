//! Turning a Lumen declaration into the CSS a browser reads.
//!
//! Most of Lumen's property names are the standard ones and are written
//! out unchanged. The rest fall into four groups: names Lumen shortened
//! (`bg`, `radius`, `grow`), properties that stand for a state rather than
//! a value (`hover-bg` is `:hover { background }`), knobs no browser has a
//! property for (`knob-color`, `scrollbar-thickness`), and the two names
//! that are markup attributes wearing a property's clothes (`tab-index`,
//! `draggable`), which the document already carries.
//!
//! Values are touched as little as possible. The one rewrite that always
//! happens is the unit: Lumen reads a bare number in a length as pixels
//! and CSS reads it as nothing at all, so `padding: 8 16` is written
//! `padding: 8px 16px`. A length that reaches a property through `var()`
//! holds whatever its custom property holds, which is why the emitter puts
//! the unit on the definition instead; [`is_length_property`] is what it
//! asks to tell a length's numbers from a plain one's.

use lumen_ir::css::canonical_property_name;

/// One `name: value` pair as the emitted stylesheet writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebDecl {
    /// Property name.
    pub name: String,
    /// Value, without `!important`; the emitter appends that.
    pub value: String,
}

impl WebDecl {
    /// A declaration of `name` holding `value`.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// What one Lumen declaration becomes on the web.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emission {
    /// Declarations that stay on the rule that carried them.
    Plain(Vec<WebDecl>),
    /// Declarations that belong on a rule of their own, selected by the
    /// origin rule's selector with `pseudo` appended. `hover-bg` is the
    /// plainest case: on the web a hover fill is a second rule, not a
    /// second property.
    StateRule {
        /// Text appended to each selector of the origin rule.
        pseudo: &'static str,
        /// What that rule declares.
        decls: Vec<WebDecl>,
    },
    /// A custom property: an author's own `--name` on its way through, or
    /// a `--lm-` name standing for a knob the browser has no property for
    /// and the Lumen runtime reads back off the element.
    CustomProp(WebDecl),
    /// Nothing, with the reason there is nothing.
    Drop(&'static str),
}

impl Emission {
    /// One plain declaration.
    fn one(name: &str, value: impl Into<String>) -> Self {
        Emission::Plain(vec![WebDecl::new(name, value)])
    }
}

/// The reason a property nothing in Lumen recognizes is dropped. The
/// emitter reports these; the others are expected.
pub const UNKNOWN_PROPERTY: &str = "not a property Lumen applies";

/// The reason a declaration whose value decides its property is dropped.
pub const UNREADABLE_VALUE: &str = "the value does not name a form Lumen accepts";

/// The prefix Lumen-only knobs take as custom properties.
pub const LM_PROPERTY_PREFIX: &str = "--lm-";

/// How a property's value is written out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Value {
    /// As authored.
    AsIs,
    /// With bare numbers written as pixel lengths.
    Length,
}

/// Properties that become one declaration under a possibly different
/// name. Everything with a value form of its own is handled before this
/// table is consulted.
const PLAIN: &[(&str, &str, Value)] = &[
    // Box
    ("width", "width", Value::Length),
    ("height", "height", Value::Length),
    ("min-width", "min-width", Value::Length),
    ("min-height", "min-height", Value::Length),
    ("max-width", "max-width", Value::Length),
    ("max-height", "max-height", Value::Length),
    ("padding", "padding", Value::Length),
    ("margin", "margin", Value::Length),
    ("inset", "inset", Value::Length),
    ("position", "position", Value::AsIs),
    ("box-sizing", "box-sizing", Value::AsIs),
    ("aspect-ratio", "aspect-ratio", Value::AsIs),
    ("z-index", "z-index", Value::AsIs),
    ("display", "display", Value::AsIs),
    ("overflow", "overflow", Value::AsIs),
    ("overflow-x", "overflow-x", Value::AsIs),
    ("overflow-y", "overflow-y", Value::AsIs),
    // Flex and grid
    ("flex", "flex", Value::AsIs),
    ("flex-direction", "flex-direction", Value::AsIs),
    ("flex-wrap", "flex-wrap", Value::AsIs),
    ("grow", "flex-grow", Value::AsIs),
    ("flex-shrink", "flex-shrink", Value::AsIs),
    ("flex-basis", "flex-basis", Value::Length),
    ("gap", "gap", Value::Length),
    ("row-gap", "row-gap", Value::Length),
    ("column-gap", "column-gap", Value::Length),
    ("align", "align-items", Value::AsIs),
    ("align-items", "align-items", Value::AsIs),
    ("align-self", "align-self", Value::AsIs),
    ("align-content", "align-content", Value::AsIs),
    ("justify-items", "justify-items", Value::AsIs),
    ("justify-self", "justify-self", Value::AsIs),
    ("grid-template-rows", "grid-template-rows", Value::Length),
    (
        "grid-template-columns",
        "grid-template-columns",
        Value::Length,
    ),
    ("grid-row", "grid-row", Value::AsIs),
    ("grid-column", "grid-column", Value::AsIs),
    // Paint
    ("bg", "background", Value::AsIs),
    ("opacity", "opacity", Value::AsIs),
    ("shadow", "box-shadow", Value::Length),
    ("box-shadow", "box-shadow", Value::Length),
    ("fit", "object-fit", Value::AsIs),
    ("radius", "border-radius", Value::Length),
    (
        "border-top-left-radius",
        "border-top-left-radius",
        Value::Length,
    ),
    (
        "border-top-right-radius",
        "border-top-right-radius",
        Value::Length,
    ),
    (
        "border-bottom-right-radius",
        "border-bottom-right-radius",
        Value::Length,
    ),
    (
        "border-bottom-left-radius",
        "border-bottom-left-radius",
        Value::Length,
    ),
    // Borders and outlines
    ("border", "border", Value::Length),
    ("border-top", "border-top", Value::Length),
    ("border-right", "border-right", Value::Length),
    ("border-bottom", "border-bottom", Value::Length),
    ("border-left", "border-left", Value::Length),
    ("border-width", "border-width", Value::Length),
    ("border-top-width", "border-top-width", Value::Length),
    ("border-right-width", "border-right-width", Value::Length),
    ("border-bottom-width", "border-bottom-width", Value::Length),
    ("border-left-width", "border-left-width", Value::Length),
    ("border-style", "border-style", Value::AsIs),
    ("border-color", "border-color", Value::AsIs),
    ("border-top-color", "border-top-color", Value::AsIs),
    ("border-right-color", "border-right-color", Value::AsIs),
    ("border-bottom-color", "border-bottom-color", Value::AsIs),
    ("border-left-color", "border-left-color", Value::AsIs),
    ("outline-offset", "outline-offset", Value::Length),
    // Text
    ("text-color", "color", Value::AsIs),
    ("font-size", "font-size", Value::Length),
    ("font-family", "font-family", Value::AsIs),
    ("font-weight", "font-weight", Value::AsIs),
    ("line-height", "line-height", Value::AsIs),
    ("text-align", "text-align", Value::AsIs),
    ("text-overflow", "text-overflow", Value::AsIs),
    ("caret-color", "caret-color", Value::AsIs),
    // Logical box
    (
        "padding-inline-start",
        "padding-inline-start",
        Value::Length,
    ),
    ("padding-inline-end", "padding-inline-end", Value::Length),
    ("padding-block-start", "padding-block-start", Value::Length),
    ("padding-block-end", "padding-block-end", Value::Length),
    ("margin-inline-start", "margin-inline-start", Value::Length),
    ("margin-inline-end", "margin-inline-end", Value::Length),
    ("margin-block-start", "margin-block-start", Value::Length),
    ("margin-block-end", "margin-block-end", Value::Length),
    ("inset-inline-start", "inset-inline-start", Value::Length),
    ("inset-inline-end", "inset-inline-end", Value::Length),
    ("inset-block-start", "inset-block-start", Value::Length),
    ("inset-block-end", "inset-block-end", Value::Length),
    (
        "border-inline-start-width",
        "border-inline-start-width",
        Value::Length,
    ),
    (
        "border-inline-end-width",
        "border-inline-end-width",
        Value::Length,
    ),
    (
        "border-block-start-width",
        "border-block-start-width",
        Value::Length,
    ),
    (
        "border-block-end-width",
        "border-block-end-width",
        Value::Length,
    ),
    // Scrollbars, the two the browser also has
    ("scrollbar-width", "scrollbar-width", Value::AsIs),
    // Transitions
    ("transition-duration", "transition-duration", Value::AsIs),
    (
        "transition-timing-function",
        "transition-timing-function",
        Value::AsIs,
    ),
];

/// Knobs with no CSS property behind them. Each becomes `--lm-` plus its
/// own name, which is where the stylesheet and the browser runtime read it
/// back. A knob measured in pixels is written as a length like any other, so
/// whatever reads it gets something it can put in a `top` or a `width`.
const KNOBS: &[(&str, Value)] = &[
    ("knob-color", Value::AsIs),
    ("knob-inset", Value::Length),
    ("thumb-size", Value::Length),
    ("popup-gap", Value::Length),
    ("caret-width", Value::Length),
    ("caret-blink", Value::AsIs),
    ("password-character", Value::AsIs),
    ("disabled-opacity", Value::AsIs),
    ("progress-duration", Value::AsIs),
    ("progress-chunk", Value::AsIs),
    ("sensitivity", Value::AsIs),
    ("inertia", Value::AsIs),
    ("scrollbar-thickness", Value::Length),
    ("scrollbar-thickness-thin", Value::Length),
    ("scrollbar-margin", Value::Length),
    ("scrollbar-min-thumb", Value::Length),
    ("scrollbar-track-hover", Value::AsIs),
    ("scrollbar-hover-boost", Value::AsIs),
    ("scrollbar-fade-delay", Value::AsIs),
    ("scrollbar-fade-duration", Value::AsIs),
];

/// True when a bare number in this property's value means pixels.
///
/// The emitter asks this of a use site to decide what a custom property
/// holding a bare number is: a length written without its unit, which a
/// browser drops, or a plain number that has to stay one.
#[must_use]
pub fn is_length_property(name: &str) -> bool {
    let name = canonical_property_name(name);
    if matches!(
        name,
        "hover-border" | "focus-border" | "focus-outline" | "outline"
    ) {
        return true;
    }
    if let Some((_, _, form)) = PLAIN.iter().find(|(lumen, _, _)| *lumen == name) {
        return *form == Value::Length;
    }
    KNOBS
        .iter()
        .any(|(knob, form)| *knob == name && *form == Value::Length)
}

/// What `name: value` becomes on the web.
///
/// Both Lumen's spelling of a property and the standard one are accepted,
/// the same way the cascade accepts them.
pub fn rewrite_property(name: &str, value: &str) -> Emission {
    let value = value.trim();
    if name.starts_with("--") {
        return Emission::CustomProp(WebDecl::new(name, value));
    }
    let name = canonical_property_name(name);
    match name {
        // States. A Lumen property that names a state is a second rule
        // here, so the browser swaps the value in the way it knows.
        "hover-bg" => state(":hover", "background", value),
        "press-bg" => state(":active", "background", value),
        "hover-border" => state(":hover", "border", &lengths(value)),
        "focus-border" => state(":focus", "border", &lengths(value)),
        // `focus-outline` is the short way to write `:focus { outline }`,
        // so it lands where a rule written that way lands.
        "focus-outline" => state(":focus", "outline", &outline(value)),
        "selection-color" => state("::selection", "background", value),
        "selection-text-color" => state("::selection", "color", value),
        // Values that pick the property.
        "justify" => Emission::one("justify-content", justify(value)),
        "wrap" => wrap(value),
        "scroll" => scroll(value),
        "max-lines" => line_clamp(value),
        "scrollbar-color" => Emission::one("scrollbar-color", scrollbar_color(value)),
        "transition" => Emission::one("transition", transition(value)),
        "transition-property" => Emission::one("transition-property", transition_property(value)),
        // Already in the document as an attribute.
        "tab-index" => Emission::Drop("the document carries it as `tabindex`"),
        "draggable" => Emission::Drop("the document carries it as `draggable`"),
        // No web meaning.
        "transition-delay" => {
            Emission::Drop("Lumen starts a transition on the tick the value changes")
        }
        "layout-boundary" => {
            Emission::Drop("a hint to Lumen's layout engine; the browser lays the page out itself")
        }
        _ => {
            if let Some((_, css, form)) = PLAIN.iter().find(|(lumen, _, _)| *lumen == name) {
                let value = match form {
                    Value::AsIs => value.to_string(),
                    Value::Length => lengths(value),
                };
                return Emission::one(css, value);
            }
            if let Some((_, form)) = KNOBS.iter().find(|(knob, _)| *knob == name) {
                let value = match form {
                    Value::AsIs => value.to_string(),
                    Value::Length => lengths(value),
                };
                return Emission::CustomProp(WebDecl::new(
                    format!("{LM_PROPERTY_PREFIX}{name}"),
                    value,
                ));
            }
            Emission::Drop(UNKNOWN_PROPERTY)
        }
    }
}

fn state(pseudo: &'static str, name: &str, value: &str) -> Emission {
    Emission::StateRule {
        pseudo,
        decls: vec![WebDecl::new(name, value)],
    }
}

/// Lumen writes a focus ring as a width and a colour; CSS wants a style
/// between them, and Lumen only draws solid ones.
fn outline(value: &str) -> String {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        [width, color] => format!("{} solid {color}", lengths(width)),
        _ => lengths(value),
    }
}

/// Lumen accepts the short spelling of the three distributed alignments.
fn justify(value: &str) -> &str {
    match value {
        "between" => "space-between",
        "around" => "space-around",
        "evenly" => "space-evenly",
        other => other,
    }
}

/// `wrap` is two CSS properties depending on how far the break may go.
fn wrap(value: &str) -> Emission {
    let decls = match value {
        "none" | "nowrap" => vec![WebDecl::new("white-space", "nowrap")],
        "word" | "normal" => vec![
            WebDecl::new("white-space", "normal"),
            WebDecl::new("overflow-wrap", "normal"),
        ],
        "glyph" | "char" => vec![
            WebDecl::new("white-space", "normal"),
            WebDecl::new("overflow-wrap", "anywhere"),
        ],
        _ => return Emission::Drop(UNREADABLE_VALUE),
    };
    Emission::Plain(decls)
}

/// A Lumen scroll container scrolls on the axes it names, and shows a bar
/// only when there is something to scroll, which is `auto`.
fn scroll(value: &str) -> Emission {
    let decls = match value {
        "x" => vec![WebDecl::new("overflow-x", "auto")],
        "y" => vec![WebDecl::new("overflow-y", "auto")],
        "both" => vec![
            WebDecl::new("overflow-x", "auto"),
            WebDecl::new("overflow-y", "auto"),
        ],
        _ => return Emission::Drop(UNREADABLE_VALUE),
    };
    Emission::Plain(decls)
}

/// Clamping text to a line count is one property in Lumen and a block of
/// four in CSS, where it is still spelled with the `-webkit-` prefix every
/// engine implements.
fn line_clamp(value: &str) -> Emission {
    Emission::Plain(vec![
        WebDecl::new("display", "-webkit-box"),
        WebDecl::new("-webkit-box-orient", "vertical"),
        WebDecl::new("-webkit-line-clamp", value),
        WebDecl::new("overflow", "hidden"),
    ])
}

/// CSS takes both scrollbar colours or neither, so a value that names
/// only the thumb leaves the track alone.
fn scrollbar_color(value: &str) -> String {
    if value == "auto" || value.split_whitespace().count() != 1 {
        return value.to_string();
    }
    format!("{value} transparent")
}

/// Rewrite the property name each entry of a `transition` list starts
/// with, leaving its duration and easing alone.
fn transition(value: &str) -> String {
    join_list(value, |entry| {
        let mut parts = entry.split_whitespace();
        let Some(first) = parts.next() else {
            return entry.to_string();
        };
        let rest: Vec<&str> = parts.collect();
        let name = animated_property(first);
        if rest.is_empty() {
            name
        } else {
            format!("{name} {}", rest.join(" "))
        }
    })
}

fn transition_property(value: &str) -> String {
    join_list(value, |entry| animated_property(entry.trim()))
}

/// The CSS name of a property a transition can name. `all` and `none`
/// stand for themselves, and anything Lumen cannot animate is left as
/// written so the browser ignores the same entry Lumen does.
fn animated_property(name: &str) -> String {
    let canonical = canonical_property_name(name);
    PLAIN
        .iter()
        .find(|(lumen, _, _)| *lumen == canonical)
        .map(|(_, css, _)| (*css).to_string())
        .unwrap_or_else(|| name.to_string())
}

fn join_list(value: &str, mut entry: impl FnMut(&str) -> String) -> String {
    value
        .split(',')
        .map(|part| entry(part.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Write every bare number in `value` as the pixel length Lumen reads it
/// as. A term that is not a plain number, a `var()` reference included, is
/// left as it was written.
pub fn lengths(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut term = String::new();
    for ch in value.chars() {
        if ch.is_whitespace() || matches!(ch, ',' | '(' | ')' | '/') {
            push_term(&mut out, &mut term);
            out.push(ch);
        } else {
            term.push(ch);
        }
    }
    push_term(&mut out, &mut term);
    out
}

fn push_term(out: &mut String, term: &mut String) {
    out.push_str(term);
    if is_number(term) {
        out.push_str("px");
    }
    term.clear();
}

/// True when every term of `value` is a decimal number with no unit on it,
/// which is how Lumen writes a length and how CSS writes a plain number. An
/// empty value is not one.
#[must_use]
pub fn is_bare_number(value: &str) -> bool {
    let mut terms = value.split_whitespace().peekable();
    terms.peek().is_some() && terms.all(is_number)
}

/// True for a decimal number with no unit on it.
fn is_number(term: &str) -> bool {
    let digits = term.strip_prefix(['+', '-']).unwrap_or(term);
    if digits.is_empty() || !digits.contains(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let mut seen_point = false;
    for ch in digits.chars() {
        if ch == '.' {
            if seen_point {
                return false;
            }
            seen_point = true;
        } else if !ch.is_ascii_digit() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_ir::css::STYLE_PROPERTIES;

    /// How a rewrite reads, so a table of them stays readable.
    fn show(emission: &Emission) -> String {
        fn decls(decls: &[WebDecl]) -> String {
            decls
                .iter()
                .map(|d| format!("{}: {}", d.name, d.value))
                .collect::<Vec<_>>()
                .join("; ")
        }
        match emission {
            Emission::Plain(list) => decls(list),
            Emission::StateRule { pseudo, decls: d } => format!("{pseudo} {{ {} }}", decls(d)),
            Emission::CustomProp(decl) => format!("{}: {}", decl.name, decl.value),
            Emission::Drop(_) => "drop".to_string(),
        }
    }

    fn rewritten(name: &str, value: &str) -> String {
        show(&rewrite_property(name, value))
    }

    /// Every property the cascade applies, the value it is tested with,
    /// and what it becomes. `STYLE_PROPERTIES` is walked against this, so
    /// a property added to the cascade fails the test below until the web
    /// target says what it turns into.
    const CASES: &[(&str, &str, &str)] = &[
        ("width", "100%", "width: 100%"),
        ("height", "32", "height: 32px"),
        ("min-width", "160", "min-width: 160px"),
        ("min-height", "24", "min-height: 24px"),
        ("max-width", "40rem", "max-width: 40rem"),
        ("max-height", "var(--h)", "max-height: var(--h)"),
        ("padding", "8 16", "padding: 8px 16px"),
        ("margin", "4 0", "margin: 4px 0px"),
        ("inset", "0", "inset: 0px"),
        ("position", "absolute", "position: absolute"),
        ("box-sizing", "border-box", "box-sizing: border-box"),
        ("aspect-ratio", "1.5", "aspect-ratio: 1.5"),
        ("z-index", "3", "z-index: 3"),
        ("display", "grid", "display: grid"),
        ("overflow", "hidden", "overflow: hidden"),
        ("overflow-x", "scroll", "overflow-x: scroll"),
        ("overflow-y", "scroll", "overflow-y: scroll"),
        ("flex", "1 1 0", "flex: 1 1 0"),
        ("flex-direction", "column", "flex-direction: column"),
        ("flex-wrap", "wrap", "flex-wrap: wrap"),
        ("grow", "1", "flex-grow: 1"),
        ("flex-shrink", "0", "flex-shrink: 0"),
        ("flex-basis", "120", "flex-basis: 120px"),
        ("gap", "4 8", "gap: 4px 8px"),
        ("row-gap", "4", "row-gap: 4px"),
        ("column-gap", "4", "column-gap: 4px"),
        ("align", "center", "align-items: center"),
        ("align-items", "stretch", "align-items: stretch"),
        ("align-self", "flex-end", "align-self: flex-end"),
        (
            "align-content",
            "space-between",
            "align-content: space-between",
        ),
        ("justify", "between", "justify-content: space-between"),
        ("justify-items", "center", "justify-items: center"),
        ("justify-self", "start", "justify-self: start"),
        (
            "grid-template-rows",
            "40 1fr",
            "grid-template-rows: 40px 1fr",
        ),
        (
            "grid-template-columns",
            "minmax(100, 1fr) auto",
            "grid-template-columns: minmax(100px, 1fr) auto",
        ),
        ("grid-row", "1 / 3", "grid-row: 1 / 3"),
        ("grid-column", "2", "grid-column: 2"),
        ("bg", "#0a3358", "background: #0a3358"),
        ("opacity", "0.5", "opacity: 0.5"),
        ("shadow", "0 2 6 #0008", "box-shadow: 0px 2px 6px #0008"),
        (
            "box-shadow",
            "inset 0 1 0 #fff",
            "box-shadow: inset 0px 1px 0px #fff",
        ),
        ("fit", "cover", "object-fit: cover"),
        ("radius", "8", "border-radius: 8px"),
        ("border-top-left-radius", "6", "border-top-left-radius: 6px"),
        (
            "border-top-right-radius",
            "6",
            "border-top-right-radius: 6px",
        ),
        (
            "border-bottom-right-radius",
            "6",
            "border-bottom-right-radius: 6px",
        ),
        (
            "border-bottom-left-radius",
            "6",
            "border-bottom-left-radius: 6px",
        ),
        ("border", "1 solid #fff", "border: 1px solid #fff"),
        ("border-top", "2 solid #fff", "border-top: 2px solid #fff"),
        (
            "border-right",
            "2 solid #fff",
            "border-right: 2px solid #fff",
        ),
        (
            "border-bottom",
            "2 solid #fff",
            "border-bottom: 2px solid #fff",
        ),
        ("border-left", "2 solid #fff", "border-left: 2px solid #fff"),
        ("border-width", "1 2", "border-width: 1px 2px"),
        ("border-top-width", "1", "border-top-width: 1px"),
        ("border-right-width", "1", "border-right-width: 1px"),
        ("border-bottom-width", "1", "border-bottom-width: 1px"),
        ("border-left-width", "1", "border-left-width: 1px"),
        ("border-style", "solid", "border-style: solid"),
        ("border-color", "#1c4666", "border-color: #1c4666"),
        ("border-top-color", "#1c4666", "border-top-color: #1c4666"),
        (
            "border-right-color",
            "#1c4666",
            "border-right-color: #1c4666",
        ),
        (
            "border-bottom-color",
            "#1c4666",
            "border-bottom-color: #1c4666",
        ),
        ("border-left-color", "#1c4666", "border-left-color: #1c4666"),
        ("outline-offset", "2", "outline-offset: 2px"),
        (
            "focus-outline",
            "2 #33c7ce",
            ":focus { outline: 2px solid #33c7ce }",
        ),
        (
            "hover-border",
            "1 solid #fff",
            ":hover { border: 1px solid #fff }",
        ),
        (
            "focus-border",
            "1 solid #fff",
            ":focus { border: 1px solid #fff }",
        ),
        ("hover-bg", "#114570", ":hover { background: #114570 }"),
        ("press-bg", "#073056", ":active { background: #073056 }"),
        (
            "selection-color",
            "#33c7ce",
            "::selection { background: #33c7ce }",
        ),
        (
            "selection-text-color",
            "#062028",
            "::selection { color: #062028 }",
        ),
        ("text-color", "#ffffff", "color: #ffffff"),
        ("font-size", "14", "font-size: 14px"),
        (
            "font-family",
            "Inter, sans-serif",
            "font-family: Inter, sans-serif",
        ),
        ("font-weight", "bold", "font-weight: bold"),
        ("line-height", "1.2", "line-height: 1.2"),
        ("text-align", "center", "text-align: center"),
        ("text-overflow", "ellipsis", "text-overflow: ellipsis"),
        ("caret-color", "#fff", "caret-color: #fff"),
        ("wrap", "none", "white-space: nowrap"),
        (
            "max-lines",
            "2",
            "display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; overflow: hidden",
        ),
        ("scroll", "y", "overflow-y: auto"),
        ("padding-inline-start", "8", "padding-inline-start: 8px"),
        ("padding-inline-end", "8", "padding-inline-end: 8px"),
        ("padding-block-start", "8", "padding-block-start: 8px"),
        ("padding-block-end", "8", "padding-block-end: 8px"),
        ("margin-inline-start", "8", "margin-inline-start: 8px"),
        ("margin-inline-end", "8", "margin-inline-end: 8px"),
        ("margin-block-start", "8", "margin-block-start: 8px"),
        ("margin-block-end", "8", "margin-block-end: 8px"),
        ("inset-inline-start", "8", "inset-inline-start: 8px"),
        ("inset-inline-end", "8", "inset-inline-end: 8px"),
        ("inset-block-start", "8", "inset-block-start: 8px"),
        ("inset-block-end", "8", "inset-block-end: 8px"),
        (
            "border-inline-start-width",
            "1",
            "border-inline-start-width: 1px",
        ),
        (
            "border-inline-end-width",
            "1",
            "border-inline-end-width: 1px",
        ),
        (
            "border-block-start-width",
            "1",
            "border-block-start-width: 1px",
        ),
        ("border-block-end-width", "1", "border-block-end-width: 1px"),
        (
            "scrollbar-color",
            "#4a4a4a",
            "scrollbar-color: #4a4a4a transparent",
        ),
        ("scrollbar-width", "thin", "scrollbar-width: thin"),
        (
            "transition",
            "bg 130ms ease, text-color 100ms",
            "transition: background 130ms ease, color 100ms",
        ),
        (
            "transition-property",
            "bg, opacity",
            "transition-property: background, opacity",
        ),
        ("transition-duration", "130ms", "transition-duration: 130ms"),
        (
            "transition-timing-function",
            "ease-out",
            "transition-timing-function: ease-out",
        ),
        ("transition-delay", "100ms", "drop"),
        ("tab-index", "0", "drop"),
        ("draggable", "true", "drop"),
        ("layout-boundary", "true", "drop"),
        ("knob-color", "#ebebf0", "--lm-knob-color: #ebebf0"),
        ("knob-inset", "4", "--lm-knob-inset: 4px"),
        ("thumb-size", "16", "--lm-thumb-size: 16px"),
        ("popup-gap", "4", "--lm-popup-gap: 4px"),
        ("caret-width", "2", "--lm-caret-width: 2px"),
        ("caret-blink", "530ms", "--lm-caret-blink: 530ms"),
        ("password-character", "*", "--lm-password-character: *"),
        ("disabled-opacity", "0.5", "--lm-disabled-opacity: 0.5"),
        ("progress-duration", "1200", "--lm-progress-duration: 1200"),
        ("progress-chunk", "0.3", "--lm-progress-chunk: 0.3"),
        ("sensitivity", "1.0", "--lm-sensitivity: 1.0"),
        ("inertia", "0.4", "--lm-inertia: 0.4"),
        ("scrollbar-thickness", "8", "--lm-scrollbar-thickness: 8px"),
        (
            "scrollbar-thickness-thin",
            "4",
            "--lm-scrollbar-thickness-thin: 4px",
        ),
        ("scrollbar-margin", "2", "--lm-scrollbar-margin: 2px"),
        (
            "scrollbar-min-thumb",
            "24",
            "--lm-scrollbar-min-thumb: 24px",
        ),
        (
            "scrollbar-track-hover",
            "#22222240",
            "--lm-scrollbar-track-hover: #22222240",
        ),
        (
            "scrollbar-hover-boost",
            "1.6",
            "--lm-scrollbar-hover-boost: 1.6",
        ),
        (
            "scrollbar-fade-delay",
            "1000ms",
            "--lm-scrollbar-fade-delay: 1000ms",
        ),
        (
            "scrollbar-fade-duration",
            "250ms",
            "--lm-scrollbar-fade-duration: 250ms",
        ),
    ];

    #[test]
    fn every_property_becomes_what_the_table_says() {
        for (name, value, expected) in CASES {
            assert_eq!(&rewritten(name, value), expected, "rewriting `{name}`");
        }
    }

    #[test]
    fn every_property_the_cascade_applies_has_an_answer() {
        for &name in STYLE_PROPERTIES {
            assert!(
                CASES.iter().any(|(cased, _, _)| *cased == name),
                "`{name}` is a property the cascade applies and the web target has no answer for it"
            );
            assert!(
                !matches!(
                    rewrite_property(name, "1"),
                    Emission::Drop(UNKNOWN_PROPERTY)
                ),
                "`{name}` is a property the cascade applies but the rewriter calls it unknown"
            );
        }
    }

    #[test]
    fn the_standard_spelling_of_a_property_answers_the_same() {
        for (standard, lumen) in [
            ("color", "text-color"),
            ("background", "bg"),
            ("background-color", "bg"),
            ("border-radius", "radius"),
            ("flex-grow", "grow"),
            ("justify-content", "justify"),
            ("object-fit", "fit"),
            ("shrink", "flex-shrink"),
        ] {
            assert_eq!(
                rewritten(standard, "8"),
                rewritten(lumen, "8"),
                "`{standard}` and `{lumen}` are the same property"
            );
        }
    }

    #[test]
    fn a_custom_property_travels_under_its_own_name() {
        assert_eq!(
            rewrite_property("--lumen-accent", "#33c7ce"),
            Emission::CustomProp(WebDecl::new("--lumen-accent", "#33c7ce"))
        );
    }

    #[test]
    fn a_property_lumen_does_not_apply_is_dropped_as_unknown() {
        for name in ["float", "content", "bg-color"] {
            assert_eq!(
                rewrite_property(name, "1"),
                Emission::Drop(UNKNOWN_PROPERTY)
            );
        }
    }

    #[test]
    fn a_value_that_picks_the_property_is_dropped_when_it_reads_as_nothing() {
        for (name, value) in [("wrap", "sideways"), ("scroll", "diagonal")] {
            assert_eq!(
                rewrite_property(name, value),
                Emission::Drop(UNREADABLE_VALUE)
            );
        }
    }

    #[test]
    fn wrapping_picks_how_far_a_break_may_go() {
        assert_eq!(rewritten("wrap", "nowrap"), "white-space: nowrap");
        assert_eq!(
            rewritten("wrap", "word"),
            "white-space: normal; overflow-wrap: normal"
        );
        assert_eq!(
            rewritten("wrap", "char"),
            "white-space: normal; overflow-wrap: anywhere"
        );
        // `white-space` is the standard spelling of the same property.
        assert_eq!(rewritten("white-space", "nowrap"), "white-space: nowrap");
    }

    #[test]
    fn scrolling_names_the_axes_it_scrolls_on() {
        assert_eq!(rewritten("scroll", "x"), "overflow-x: auto");
        assert_eq!(
            rewritten("scroll", "both"),
            "overflow-x: auto; overflow-y: auto"
        );
    }

    #[test]
    fn a_bare_number_becomes_pixels_and_everything_else_stays() {
        assert_eq!(lengths("8"), "8px");
        assert_eq!(lengths("8 16"), "8px 16px");
        assert_eq!(lengths("-2.5"), "-2.5px");
        assert_eq!(lengths("50%"), "50%");
        assert_eq!(lengths("auto"), "auto");
        assert_eq!(lengths("1px solid #fff"), "1px solid #fff");
        assert_eq!(lengths("var(--gap)"), "var(--gap)");
        assert_eq!(lengths("minmax(100, 1fr)"), "minmax(100px, 1fr)");
        assert_eq!(lengths(""), "");
    }

    #[test]
    fn scrollbar_colour_always_names_both_halves() {
        assert_eq!(
            rewritten("scrollbar-color", "auto"),
            "scrollbar-color: auto"
        );
        assert_eq!(
            rewritten("scrollbar-color", "#4a4a4a #1a1a1a"),
            "scrollbar-color: #4a4a4a #1a1a1a"
        );
    }

    #[test]
    fn a_rewrite_is_the_same_every_time() {
        for (name, value, _) in CASES {
            assert_eq!(rewrite_property(name, value), rewrite_property(name, value));
        }
    }
}
