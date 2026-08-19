//! Which HTML element an IR tag becomes.
//!
//! The mapping is the same one twice over: the emitter builds elements from
//! it when it writes a page, and the browser runtime builds elements from it
//! when it creates a node that was never in the page (a new `<for>` row, a
//! branch that just became true). Neither may carry its own table.
//!
//! Layout is CSS's job, not the element's. A `<row>` and a `<column>` are
//! both a `div`; what makes them lay out differently is the `lm-row` and
//! `lm-column` class the stylesheet targets. That is also why every element
//! gets its class whether or not any rule matches it today.

/// An HTML element an IR tag maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlTag {
    /// Element name, as written in the document.
    pub name: &'static str,
    /// Attributes the mapping itself implies, such as the `type` that makes
    /// an `input` a checkbox. They are written before author attributes.
    pub fixed: &'static [(&'static str, &'static str)],
    /// True when the element takes no children and no end tag.
    pub void: bool,
}

impl HtmlTag {
    const fn plain(name: &'static str) -> Self {
        Self {
            name,
            fixed: &[],
            void: false,
        }
    }
}

/// HTML elements that take no children and no end tag.
pub const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// Every IR tag [`html_tag_for`] answers for.
///
/// The tags a Lumen author writes that are not here are the ones the parser
/// resolves before the IR exists: a `<tabs>` is already a column of buttons
/// and branches by then.
pub const MAPPED_TAGS: &[&str] = &[
    "root",
    "column",
    "row",
    "tile",
    "div",
    "spacer",
    "scroll",
    "overlay",
    "title-bar",
    "for",
    "if",
    "fragment",
    "label",
    "a",
    "image",
    "input",
    "textarea",
    "button",
    "progress",
    "dialog",
    "toggle",
    "switch",
    "checkbox",
    "radio",
    "slider",
];

/// Prefix of the class an element carries for its tag.
pub const LM_CLASS_PREFIX: &str = "lm-";

/// True when `html_name` is an HTML void element.
pub fn is_void(html_name: &str) -> bool {
    VOID_ELEMENTS.contains(&html_name)
}

/// The class every element of `ir_tag` carries, [`LM_CLASS_PREFIX`] plus
/// the tag name.
pub fn lm_class(ir_tag: &str) -> String {
    format!("{LM_CLASS_PREFIX}{ir_tag}")
}

/// The HTML element `ir_tag` becomes, or `None` for a tag with no mapping.
///
/// `root` becomes a plain `div` inside `<body>` rather than the document or
/// body element itself. It is one node in the tree like any other, so a rule
/// written against `root` still matches it, and a runtime that adopts the
/// page starts from an element it can also have created.
pub fn html_tag_for(ir_tag: &str) -> Option<HtmlTag> {
    let tag = match ir_tag {
        // Boxes. Direction, scrolling and stacking all come from CSS.
        "root" | "column" | "row" | "tile" | "div" | "spacer" | "scroll" | "overlay"
        | "title-bar" => HtmlTag::plain("div"),
        // Reactive blocks are real boxes on the desktop, so they are real
        // boxes here: the anchor stays in the document with its rows or its
        // branch inside it.
        "for" | "if" => HtmlTag::plain("div"),
        // A component the runtime fills. It stands in the document as the
        // empty box it is until the first tick replaces it, so the node the
        // call builds has a place to take.
        "fragment" => HtmlTag::plain("div"),
        // A label is a run of text inside its parent's flow, not a block.
        "label" => HtmlTag::plain("span"),
        "a" => HtmlTag::plain("a"),
        "image" => HtmlTag {
            name: "img",
            fixed: &[],
            void: true,
        },
        // `type="button"` because a Lumen button never submits anything.
        "button" => HtmlTag {
            name: "button",
            fixed: &[("type", "button")],
            void: false,
        },
        "input" => HtmlTag {
            name: "input",
            fixed: &[],
            void: true,
        },
        "textarea" => HtmlTag::plain("textarea"),
        "progress" => HtmlTag::plain("progress"),
        "dialog" => HtmlTag::plain("dialog"),
        // A toggle and a switch are the same state in two presentations, and
        // `role="switch"` is what tells a screen reader it is on or off.
        "toggle" | "switch" => HtmlTag {
            name: "button",
            fixed: &[("type", "button"), ("role", "switch")],
            void: false,
        },
        "checkbox" => HtmlTag {
            name: "input",
            fixed: &[("type", "checkbox")],
            void: true,
        },
        "radio" => HtmlTag {
            name: "input",
            fixed: &[("type", "radio")],
            void: true,
        },
        "slider" => HtmlTag {
            name: "input",
            fixed: &[("type", "range")],
            void: true,
        },
        _ => return None,
    };
    Some(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mapped_tag_answers() {
        for tag in MAPPED_TAGS {
            let mapped = html_tag_for(tag).unwrap_or_else(|| panic!("no mapping for `{tag}`"));
            assert!(!mapped.name.is_empty());
            assert_eq!(mapped.void, is_void(mapped.name));
        }
    }

    #[test]
    fn boxes_become_divs_and_text_becomes_a_span() {
        for tag in ["root", "column", "row", "tile", "div", "for", "if"] {
            assert_eq!(html_tag_for(tag).expect("mapped").name, "div");
        }
        assert_eq!(html_tag_for("label").expect("mapped").name, "span");
        assert_eq!(html_tag_for("a").expect("mapped").name, "a");
    }

    #[test]
    fn controls_map_onto_the_matching_html_control() {
        let checkbox = html_tag_for("checkbox").expect("mapped");
        assert_eq!(checkbox.name, "input");
        assert_eq!(checkbox.fixed, &[("type", "checkbox")]);
        assert!(checkbox.void);

        let slider = html_tag_for("slider").expect("mapped");
        assert_eq!(slider.fixed, &[("type", "range")]);

        let toggle = html_tag_for("toggle").expect("mapped");
        assert_eq!(toggle.name, "button");
        assert_eq!(toggle.fixed, &[("type", "button"), ("role", "switch")]);
        assert_eq!(toggle, html_tag_for("switch").expect("mapped"));
    }

    #[test]
    fn a_tag_the_parser_resolves_away_has_no_mapping() {
        for tag in ["tabs", "dropdown", "menu", "date-picker", "tooltip"] {
            assert_eq!(html_tag_for(tag), None);
        }
    }

    #[test]
    fn classes_name_the_authored_tag() {
        assert_eq!(lm_class("row"), "lm-row");
        assert_eq!(lm_class("title-bar"), "lm-title-bar");
    }
}
