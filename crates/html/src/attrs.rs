//! Projecting the IR attribute bag onto HTML attributes.
//!
//! This is the non-style half of an element's attributes: identity, links,
//! form-control state, text direction, the things a browser gives meaning to
//! on its own. Everything the cascade owns (colors, sizes, spacing) reaches
//! the browser as CSS instead, because a value that arrives as CSS keeps the
//! precedence the author wrote.
//!
//! A style the author wrote on the element itself is CSS too, and comes back
//! from [`markup_rules`] as declarations for the emitter to write as a rule.
//! Lumen ranks those above every rule and a browser does not, so where that
//! rule goes in the file is what settles the order.
//!
//! A few Lumen states have no HTML attribute to land on. A `<toggle>` is a
//! button, not a checkbox, so it has no `:checked`; a `<div>` cannot be
//! `disabled`. Those states are mirrored onto the `data-lm-*` attributes in
//! [`contract`](crate::contract) so a stylesheet still has something to
//! match.

use lumen_core::components::LayoutDirection;
use lumen_ir::layout_ir::Attributes;

use crate::contract::{DATA_LM_CHECKED, DATA_LM_DISABLED};
use crate::style::{Emission, WebDecl, rewrite_property};
use crate::tags::{html_tag_for, lm_class};

/// HTML elements that take a `disabled` attribute.
const DISABLEABLE: &[&str] = &[
    "button", "input", "textarea", "select", "fieldset", "option",
];

/// True when the browser can disable `html_name` itself, which takes the
/// element out of the tab order and stops it answering events.
#[must_use]
pub fn is_disableable(html_name: &str) -> bool {
    DISABLEABLE.contains(&html_name)
}

/// The `class` value for an element: its tag class, then author classes.
///
/// The tag class comes first so a stylesheet can rely on it being there, and
/// author classes keep their written order.
pub fn class_list(ir_tag: &str, attrs: &Attributes) -> String {
    let mut classes = lm_class(ir_tag);
    for class in &attrs.classes {
        if class.is_empty() {
            continue;
        }
        classes.push(' ');
        classes.push_str(class);
    }
    classes
}

/// The HTML attributes an element carries, `class` first, in a fixed order.
///
/// Values are unescaped; the caller escapes them as it writes them. An
/// attribute whose HTML form is boolean (`disabled`, `checked`) comes back
/// with an empty value, which is how HTML writes a set boolean attribute.
///
/// Attributes the mapping itself implies, such as the `type` on a checkbox,
/// are not repeated here; they live on [`HtmlTag::fixed`](crate::HtmlTag).
pub fn html_attrs(ir_tag: &str, attrs: &Attributes) -> Vec<(&'static str, String)> {
    let html = html_tag_for(ir_tag);
    let name = html.map(|t| t.name).unwrap_or("div");
    let fixed = html.map(|t| t.fixed).unwrap_or(&[]);
    let input_type = fixed
        .iter()
        .find(|(k, _)| *k == "type")
        .map(|(_, v)| *v)
        .unwrap_or("");
    // A Lumen `<input>` maps to an `input` with no fixed type, which is a
    // text field; the typed ones are their own tags.
    let is_text_entry = name == "textarea" || (name == "input" && input_type.is_empty());
    let mut out: Vec<(&'static str, String)> = vec![("class", class_list(ir_tag, attrs))];

    if let Some(id) = &attrs.id {
        out.push(("id", id.clone()));
    }
    if name == "a"
        && let Some(href) = &attrs.href
    {
        out.push(("href", href.clone()));
    }
    if name == "img" {
        if let Some(src) = &attrs.src {
            out.push(("src", src.clone()));
        }
        // An `alt` the author wrote is what the image says it is. Without
        // one, an image that carries text lends it, which is what a Lumen
        // app written before `alt` existed relies on. An image with neither
        // gets no `alt`.
        if let Some(alt) = attrs.alt.as_ref().or(attrs.text.as_ref()) {
            out.push(("alt", alt.clone()));
        }
    }
    if let Some(tooltip) = &attrs.tooltip {
        out.push(("title", tooltip.text.clone()));
    }
    if let Some(index) = attrs.tab_index {
        out.push(("tabindex", index.to_string()));
    }
    if attrs.disabled {
        if is_disableable(name) {
            out.push(("disabled", String::new()));
        }
        out.push((DATA_LM_DISABLED, String::new()));
    }
    if is_text_entry {
        // A text field's text is its value, and an `input` has nowhere else to
        // put it: it takes no children, so the text node every other element
        // holds its text in has no place to go. A `textarea` does hold one,
        // and the emitter writes it there. This is the same split the runtime
        // makes when it projects an entity's text onto its element.
        if name == "input"
            && attrs.value.is_none()
            && let Some(text) = attrs.text.as_ref().filter(|text| !text.is_empty())
        {
            out.push(("value", text.clone()));
        }
        if let Some(placeholder) = &attrs.placeholder {
            out.push(("placeholder", placeholder.clone()));
        }
        if attrs.required {
            out.push(("required", String::new()));
        }
        if let Some(pattern) = &attrs.pattern {
            out.push(("pattern", pattern.clone()));
        }
    }
    if input_type == "radio" {
        if let Some(group) = &attrs.radio_group {
            out.push(("name", group.clone()));
        }
        if let Some(value) = &attrs.radio_value {
            out.push(("value", value.clone()));
        }
    } else if (name == "input" || name == "progress")
        && let Some(value) = attrs.value
    {
        out.push(("value", number(value)));
    }
    if input_type == "range" {
        if let Some(min) = attrs.min {
            out.push(("min", number(min)));
        }
        if let Some(max) = attrs.max {
            out.push(("max", number(max)));
        }
        if let Some(step) = attrs.step {
            out.push(("step", number(step)));
        }
    } else if name == "progress"
        && let Some(max) = attrs.max
    {
        // `<progress>` has a maximum but no minimum; it always starts at 0.
        out.push(("max", number(max)));
    }
    if let Some(checked) = attrs.checked {
        // `role="switch"` is only meaningful with the state beside it.
        if fixed.contains(&("role", "switch")) {
            out.push(("aria-checked", checked.to_string()));
        } else if checked && matches!(input_type, "checkbox" | "radio") {
            out.push(("checked", String::new()));
        }
        if checked {
            out.push((DATA_LM_CHECKED, String::new()));
        }
    }
    if attrs.autofocus && (name == "button" || name == "input" || name == "textarea") {
        out.push(("autofocus", String::new()));
    }
    if attrs.draggable {
        out.push(("draggable", "true".to_string()));
    }
    match attrs.dir {
        Some(LayoutDirection::Ltr) => out.push(("dir", "ltr".to_string())),
        Some(LayoutDirection::Rtl) => out.push(("dir", "rtl".to_string())),
        Some(LayoutDirection::Auto) | None => {}
    }
    if let Some(lang) = &attrs.lang {
        out.push(("lang", lang.clone()));
    }
    out
}

/// What the styling attributes written on one element declare.
///
/// The declarations are written the way a browser reads them, through the
/// same rewrite the stylesheet goes through: `bg="#101014"` arrives as
/// `background: #101014`, and `padding="8"` as `padding: 8px`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkupRules {
    /// What the element declares about itself.
    pub base: Vec<WebDecl>,
    /// What it declares about a state, under the pseudo-class that state is
    /// spelled with. `hover-bg` is the plainest case.
    pub states: Vec<(&'static str, Vec<WebDecl>)>,
}

impl MarkupRules {
    /// True when the element declares nothing the browser can be told.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.base.is_empty() && self.states.is_empty()
    }
}

/// What an element's own styling attributes become on the web.
///
/// The two surfaces disagree about which wins. In Lumen an attribute written
/// on the element beats every rule that targets it; in a browser a rule beats
/// an element's own styling unless that styling is inline. So these come back
/// as rules of their own rather than as values, and the emitter puts them
/// where an unlayered rule outranks the stylesheet.
///
/// Leaving the inline tier is what lets a state and an animation reach these
/// declarations at all: an inline style cannot carry `:hover`, and a script
/// writing one style replaces every inline declaration on the element.
pub fn markup_rules(attrs: &Attributes) -> MarkupRules {
    let mut rules = MarkupRules::default();
    for (property, value) in &attrs.markup_styles {
        match rewrite_property(property, value) {
            Emission::Plain(decls) => rules.base.extend(decls),
            Emission::CustomProp(decl) => rules.base.push(decl),
            Emission::StateRule { pseudo, decls } => {
                match rules.states.iter_mut().find(|(name, _)| *name == pseudo) {
                    Some((_, existing)) => existing.extend(decls),
                    None => rules.states.push((pseudo, decls)),
                }
            }
            Emission::Drop(_) => {}
        }
    }
    rules
}

/// Write a number the way markup does: no trailing `.0` on whole values.
fn number(value: f32) -> String {
    if value.is_finite() && value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs() -> Attributes {
        Attributes::default()
    }

    fn find<'a>(pairs: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn the_tag_class_comes_before_author_classes() {
        let mut a = attrs();
        a.classes = vec!["card".into(), "wide".into()];
        assert_eq!(class_list("tile", &a), "lm-tile card wide");
        assert_eq!(class_list("tile", &attrs()), "lm-tile");
    }

    #[test]
    fn class_is_always_the_first_attribute() {
        let pairs = html_attrs("column", &attrs());
        assert_eq!(pairs[0].0, "class");
        assert_eq!(pairs[0].1, "lm-column");
    }

    #[test]
    fn identity_and_links_project_one_to_one() {
        let mut a = attrs();
        a.id = Some("save".into());
        a.href = Some("settings".into());
        a.tab_index = Some(2);
        let pairs = html_attrs("a", &a);
        assert_eq!(find(&pairs, "id"), Some("save"));
        assert_eq!(find(&pairs, "href"), Some("settings"));
        assert_eq!(find(&pairs, "tabindex"), Some("2"));
    }

    #[test]
    fn href_only_lands_on_an_anchor() {
        let mut a = attrs();
        a.href = Some("settings".into());
        assert_eq!(find(&html_attrs("tile", &a), "href"), None);
    }

    #[test]
    fn an_image_lends_its_text_to_alt() {
        let mut a = attrs();
        a.src = Some("logo.png".into());
        a.text = Some("Lumen".into());
        let pairs = html_attrs("image", &a);
        assert_eq!(find(&pairs, "src"), Some("logo.png"));
        assert_eq!(find(&pairs, "alt"), Some("Lumen"));
    }

    #[test]
    fn an_authored_alt_wins_over_the_borrowed_one() {
        let mut a = attrs();
        a.src = Some("logo.png".into());
        a.text = Some("Lumen".into());
        a.alt = Some("The Lumen logo".into());
        assert_eq!(
            find(&html_attrs("image", &a), "alt"),
            Some("The Lumen logo")
        );

        a.alt = Some(String::new());
        assert_eq!(
            find(&html_attrs("image", &a), "alt"),
            Some(""),
            "an empty alt marks a decorative image and is not a missing one"
        );
    }

    #[test]
    fn markup_styles_come_back_as_declarations_to_write_as_a_rule() {
        let mut a = attrs();
        a.markup_styles = vec![
            ("bg".into(), "#101014".into()),
            ("padding".into(), " 8 ".into()),
        ];
        assert_eq!(
            markup_rules(&a).base,
            vec![
                WebDecl::new("background", "#101014"),
                WebDecl::new("padding", "8px"),
            ]
        );
        assert_eq!(
            find(&html_attrs("tile", &a), "style"),
            None,
            "an element's own styling is a rule now, not an inline declaration"
        );
    }

    #[test]
    fn a_state_style_written_on_the_element_gets_a_rule_of_its_own() {
        let mut a = attrs();
        a.markup_styles = vec![
            ("hover-bg".into(), "#222".into()),
            ("bg".into(), "#111".into()),
        ];
        let rules = markup_rules(&a);
        assert_eq!(rules.base, vec![WebDecl::new("background", "#111")]);
        assert_eq!(
            rules.states,
            vec![(":hover", vec![WebDecl::new("background", "#222")])]
        );
    }

    #[test]
    fn two_states_spelled_the_same_way_share_one_rule() {
        let mut a = attrs();
        a.markup_styles = vec![
            ("hover-bg".into(), "#222".into()),
            ("hover-border".into(), "1 #444".into()),
        ];
        let rules = markup_rules(&a);
        assert!(rules.base.is_empty());
        assert_eq!(rules.states.len(), 1, "{:?}", rules.states);
        assert_eq!(rules.states[0].0, ":hover");
        assert_eq!(rules.states[0].1.len(), 2, "{:?}", rules.states);
    }

    #[test]
    fn an_element_styled_only_by_css_declares_nothing_of_its_own() {
        assert!(markup_rules(&attrs()).is_empty());
        assert_eq!(find(&html_attrs("tile", &attrs()), "style"), None);
    }

    #[test]
    fn disabled_reaches_a_control_and_a_box_differently() {
        let mut a = attrs();
        a.disabled = true;
        let button = html_attrs("button", &a);
        assert_eq!(find(&button, "disabled"), Some(""));
        assert_eq!(find(&button, DATA_LM_DISABLED), Some(""));

        let tile = html_attrs("tile", &a);
        assert_eq!(find(&tile, "disabled"), None);
        assert_eq!(find(&tile, DATA_LM_DISABLED), Some(""));
    }

    #[test]
    fn a_checkbox_is_checked_and_a_switch_is_aria_checked() {
        let mut a = attrs();
        a.checked = Some(true);
        let checkbox = html_attrs("checkbox", &a);
        assert_eq!(find(&checkbox, "checked"), Some(""));
        assert_eq!(find(&checkbox, "aria-checked"), None);
        assert_eq!(find(&checkbox, DATA_LM_CHECKED), Some(""));

        let switch = html_attrs("switch", &a);
        assert_eq!(find(&switch, "aria-checked"), Some("true"));
        assert_eq!(find(&switch, "checked"), None);

        a.checked = Some(false);
        let off = html_attrs("switch", &a);
        assert_eq!(find(&off, "aria-checked"), Some("false"));
        assert_eq!(find(&off, DATA_LM_CHECKED), None);
    }

    #[test]
    fn a_slider_carries_its_range() {
        let mut a = attrs();
        a.value = Some(0.5);
        a.min = Some(0.0);
        a.max = Some(1.0);
        a.step = Some(0.25);
        let pairs = html_attrs("slider", &a);
        assert_eq!(find(&pairs, "value"), Some("0.5"));
        assert_eq!(find(&pairs, "min"), Some("0"));
        assert_eq!(find(&pairs, "max"), Some("1"));
        assert_eq!(find(&pairs, "step"), Some("0.25"));
    }

    #[test]
    fn a_radio_carries_its_group_as_a_name() {
        let mut a = attrs();
        a.radio_group = Some("size".into());
        a.radio_value = Some("large".into());
        let pairs = html_attrs("radio", &a);
        assert_eq!(find(&pairs, "name"), Some("size"));
        assert_eq!(find(&pairs, "value"), Some("large"));
    }

    #[test]
    fn text_entry_attributes_stay_on_text_entries() {
        let mut a = attrs();
        a.placeholder = Some("Name".into());
        a.required = true;
        assert_eq!(find(&html_attrs("input", &a), "placeholder"), Some("Name"));
        assert_eq!(find(&html_attrs("input", &a), "required"), Some(""));
        assert_eq!(find(&html_attrs("slider", &a), "placeholder"), None);
        assert_eq!(find(&html_attrs("tile", &a), "required"), None);
    }

    #[test]
    fn direction_and_language_project_when_set() {
        let mut a = attrs();
        a.lang = Some("de-DE".into());
        a.dir = Some(LayoutDirection::Rtl);
        let pairs = html_attrs("column", &a);
        assert_eq!(find(&pairs, "lang"), Some("de-DE"));
        assert_eq!(find(&pairs, "dir"), Some("rtl"));

        a.dir = Some(LayoutDirection::Auto);
        assert_eq!(find(&html_attrs("column", &a), "dir"), None);
    }

    #[test]
    fn a_tooltip_becomes_a_title() {
        let mut a = attrs();
        a.tooltip = Some(lumen_ir::layout_ir::TooltipSpec {
            text: "Save the file".into(),
            delay_ms: None,
            offset: None,
        });
        assert_eq!(
            find(&html_attrs("button", &a), "title"),
            Some("Save the file")
        );
    }

    #[test]
    fn projection_is_the_same_every_time() {
        let mut a = attrs();
        a.id = Some("x".into());
        a.classes = vec!["b".into(), "a".into()];
        a.disabled = true;
        assert_eq!(html_attrs("button", &a), html_attrs("button", &a));
    }
}
