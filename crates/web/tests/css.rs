//! Emitting a stylesheet the browser reads the way Lumen read it.

use lumen_ir::css::{
    ColorSchemePreference, Declaration, LegacySelectorShim, MediaFeature, MediaQuery, Origin, Rule,
    Stylesheet, parse_selector_list,
};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_web::{
    CssMode, LocaleSpec, MarkupSheet, PageSpec, SiteSpec, WebSpec, emit, rules_css, styles_css,
};

fn rule(selectors: &str, decls: &[(&str, &str)]) -> Rule {
    Rule {
        selectors: parse_selector_list(selectors).expect("selectors parse"),
        declarations: decls
            .iter()
            .map(|(name, value)| Declaration {
                name: (*name).to_string(),
                value: (*value).to_string(),
                important: false,
            })
            .collect(),
        origin: Origin::Author,
        source_order: 0,
        media: None,
        selector: LegacySelectorShim::default(),
    }
}

fn sheet(rules: Vec<Rule>) -> Stylesheet {
    let rules = rules
        .into_iter()
        .enumerate()
        .map(|(order, mut rule)| {
            if rule.source_order == 0 {
                rule.source_order = order;
            }
            rule
        })
        .collect();
    Stylesheet { rules }
}

fn dark() -> MediaQuery {
    MediaQuery {
        features: vec![MediaFeature::PrefersColorScheme(
            ColorSchemePreference::Dark,
        )],
    }
}

/// The emitted rules, without the reset or the palette in front of them.
fn css(rules: Vec<Rule>) -> String {
    rules_css(&sheet(rules))
}

#[test]
fn a_tag_rule_becomes_a_class_rule_that_weighs_nothing() {
    assert_eq!(
        css(vec![rule("button", &[("bg", "#0a3358"), ("radius", "8")])]),
        ":where(.lm-button) {\n  background: #0a3358;\n  border-radius: 8px;\n}\n"
    );
}

#[test]
fn a_state_property_becomes_a_rule_of_its_own_beside_the_one_it_came_from() {
    assert_eq!(
        css(vec![rule(
            "button",
            &[
                ("bg", "#0a3358"),
                ("hover-bg", "#114570"),
                ("press-bg", "#073056")
            ]
        )]),
        concat!(
            ":where(.lm-button) {\n  background: #0a3358;\n}\n",
            ":where(.lm-button):hover {\n  background: #114570;\n}\n",
            ":where(.lm-button):active {\n  background: #073056;\n}\n",
        )
    );
}

#[test]
fn a_state_selector_keeps_matching_what_lumen_matched() {
    assert_eq!(
        css(vec![rule(".tab:selected", &[("bg", "#33c7ce")])]),
        ".tab[data-lm-selected] {\n  background: #33c7ce;\n}\n"
    );
    assert_eq!(
        css(vec![rule("toggle:checked", &[("bg", "#33c7ce")])]),
        ":where(.lm-toggle):is(:checked, [data-lm-checked]) {\n  background: #33c7ce;\n}\n"
    );
}

#[test]
fn the_rule_lumen_would_pick_is_written_last() {
    // The browser cannot see what settles this: the skin rule is
    // user-agent origin, which Lumen puts under any author rule whatever
    // its specificity or where it was written. Both are one class here, so
    // the only thing left to separate them is the order they are in.
    let mut skin = rule(".panel", &[("bg", "#111111")]);
    skin.origin = Origin::UserAgent;
    skin.source_order = 9;
    let mut author = rule(".panel", &[("bg", "#ffffff")]);
    author.source_order = 1;

    let emitted = rules_css(&Stylesheet {
        rules: vec![author, skin],
    });
    let first = emitted.find("#111111").expect("the skin rule is emitted");
    let second = emitted.find("#ffffff").expect("the author rule is emitted");
    assert!(
        first < second,
        "the author rule has to come last to win in the browser:\n{emitted}"
    );
}

#[test]
fn a_selector_list_splits_when_its_selectors_do_not_weigh_the_same() {
    // `row` counts as a tag in Lumen and as nothing in the browser, so it
    // cannot share a position in the file with a class.
    let emitted = css(vec![rule("row, .card", &[("bg", "#000000")])]);
    assert_eq!(
        emitted,
        concat!(
            ":where(.lm-row) {\n  background: #000000;\n}\n",
            ".card {\n  background: #000000;\n}\n",
        )
    );
}

#[test]
fn selectors_that_weigh_the_same_stay_in_one_rule() {
    assert_eq!(
        css(vec![rule("input, textarea", &[("min-height", "24")])]),
        ":where(.lm-input), :where(.lm-textarea) {\n  min-height: 24px;\n}\n"
    );
}

#[test]
fn rules_under_one_query_share_one_media_block() {
    let mut first = rule(".card", &[("bg", "#101014")]);
    first.media = Some(dark());
    first.source_order = 1;
    let mut second = rule(".panel", &[("bg", "#202024")]);
    second.media = Some(dark());
    second.source_order = 2;
    let mut plain = rule(".footer", &[("bg", "#303034")]);
    plain.source_order = 3;

    let emitted = rules_css(&Stylesheet {
        rules: vec![first, second, plain],
    });
    assert_eq!(
        emitted,
        concat!(
            "@media (prefers-color-scheme: dark) {\n",
            "  .card {\n    background: #101014;\n  }\n",
            "  .panel {\n    background: #202024;\n  }\n",
            "}\n",
            ".footer {\n  background: #303034;\n}\n",
        )
    );
}

#[test]
fn a_query_never_swallows_a_rule_that_comes_after_it() {
    // Grouping is by run, not by query: moving a later rule up into an
    // earlier block would move it up the cascade with it.
    let mut early = rule(".card", &[("bg", "#101014")]);
    early.media = Some(dark());
    early.source_order = 1;
    let mut middle = rule(".card", &[("bg", "#ffffff")]);
    middle.source_order = 2;
    let mut late = rule(".card", &[("bg", "#000000")]);
    late.media = Some(dark());
    late.source_order = 3;

    let emitted = rules_css(&Stylesheet {
        rules: vec![early, middle, late],
    });
    assert_eq!(emitted.matches("@media").count(), 2, "{emitted}");
    let plain = emitted.find("#ffffff").expect("emitted");
    let last = emitted.find("#000000").expect("emitted");
    assert!(plain < last, "{emitted}");
}

#[test]
fn important_survives_the_rewrite() {
    let mut card = rule(".card", &[("bg", "#ffffff"), ("hover-bg", "#eeeeee")]);
    for decl in &mut card.declarations {
        decl.important = true;
    }
    let emitted = rules_css(&Stylesheet { rules: vec![card] });
    assert_eq!(emitted.matches("!important").count(), 2, "{emitted}");
    assert!(
        emitted.contains("background: #ffffff !important;"),
        "{emitted}"
    );
}

#[test]
fn a_custom_property_travels_unchanged_and_a_knob_becomes_one() {
    assert_eq!(
        css(vec![rule(
            ".slider",
            &[
                ("--lumen-knob", "#ebebf0"),
                ("knob-color", "var(--lumen-knob)")
            ]
        )]),
        ".slider {\n  --lumen-knob: #ebebf0;\n  --lm-knob-color: var(--lumen-knob);\n}\n"
    );
}

#[test]
fn a_token_holding_a_length_gains_the_unit_it_was_written_without() {
    // Lumen reads `16` as 16 pixels; a browser reads `border-radius: 16` as
    // nothing and drops it. The use site cannot be fixed, so the definition is.
    assert_eq!(
        css(vec![
            rule(":root", &[("--radius-md", "16")]),
            rule(".card", &[("radius", "var(--radius-md)")]),
        ]),
        ":root {\n  --radius-md: 16px;\n}\n.card {\n  border-radius: var(--radius-md);\n}\n"
    );
}

#[test]
fn a_token_that_already_carries_its_unit_is_left_alone() {
    assert_eq!(
        css(vec![
            rule(":root", &[("--radius-md", "16px"), ("--tint", "#0a3358")]),
            rule(
                ".card",
                &[("radius", "var(--radius-md)"), ("bg", "var(--tint)")]
            ),
        ]),
        ":root {\n  --radius-md: 16px;\n  --tint: #0a3358;\n}\n.card {\n  border-radius: \
         var(--radius-md);\n  background: var(--tint);\n}\n"
    );
}

#[test]
fn a_token_the_sheet_only_ever_reads_as_a_number_keeps_being_one() {
    let emitted = css(vec![
        rule(":root", &[("--weight", "2")]),
        rule(".card", &[("grow", "var(--weight)")]),
    ]);
    assert!(emitted.contains("--weight: 2;"), "{emitted}");
}

#[test]
fn a_token_read_both_ways_is_left_as_authored_and_reported() {
    let sheet = sheet(vec![
        rule(":root", &[("--n", "2")]),
        rule(".card", &[("radius", "var(--n)"), ("grow", "var(--n)")]),
    ]);
    let emitted = rules_css(&sheet);
    assert!(emitted.contains("--n: 2;"), "{emitted}");
    let warnings = lumen_web::token_warnings(Some(&sheet), CssMode::Sheet);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("--n"), "{warnings:?}");
}

#[test]
fn a_token_that_stands_for_another_is_fixed_where_the_number_is() {
    let emitted = css(vec![
        rule(":root", &[("--base", "8"), ("--gap", "var(--base)")]),
        rule(".card", &[("gap", "var(--gap)")]),
    ]);
    assert!(emitted.contains("--base: 8px;"), "{emitted}");
    assert!(emitted.contains("--gap: var(--base);"), "{emitted}");
}

#[test]
fn a_knob_measured_in_pixels_is_written_as_a_length() {
    assert_eq!(
        css(vec![rule("toggle", &[("knob-inset", "4")])]),
        ":where(.lm-toggle) {\n  --lm-knob-inset: 4px;\n}\n"
    );
}

#[test]
fn the_reset_draws_the_parts_a_control_has_no_element_for() {
    let emitted = styles_css(None, &MarkupSheet::default(), CssMode::Sheet);
    // The knob a `<toggle>` and a `<switch>` are painted with, from the same
    // properties the desktop reads.
    assert!(emitted.contains("var(--lm-knob-color"), "{emitted}");
    assert!(emitted.contains("var(--lm-knob-inset"), "{emitted}");
    assert!(emitted.contains("var(--lm-thumb-size"), "{emitted}");
    // The states the runtime marks an element with, which decide where the
    // knob sits and whether the tick is drawn.
    assert!(emitted.contains("[data-lm-checked]"), "{emitted}");
    assert!(emitted.contains("[data-lm-selected]"), "{emitted}");
    assert!(emitted.contains("[data-lm-disabled]"), "{emitted}");
}

#[test]
fn a_rule_left_with_nothing_to_say_is_not_written() {
    assert_eq!(css(vec![rule(".card", &[("tab-index", "0")])]), "");
}

#[test]
fn emitting_twice_writes_the_same_bytes() {
    let sheet = sheet(vec![
        rule("button", &[("bg", "#0a3358"), ("hover-bg", "#114570")]),
        rule("input, textarea", &[("padding", "8 12")]),
        rule(".card:selected", &[("bg", "#33c7ce")]),
    ]);
    assert_eq!(rules_css(&sheet), rules_css(&sheet));
}

#[test]
fn the_palette_is_written_once_and_only_when_it_is_missing() {
    let bare = sheet(vec![rule("button", &[("bg", "#0a3358")])]);
    let emitted = styles_css(Some(&bare), &MarkupSheet::default(), CssMode::Sheet);
    assert!(
        emitted.contains(&lumen_ir::css::palette_root_css()),
        "a stylesheet with no palette gets one:\n{emitted}"
    );

    let carried = Stylesheet {
        rules: vec![rule(
            ":root",
            &lumen_core::palette::Palette::adwaita_light()
                .root_vars()
                .iter()
                .map(|(name, value)| (format!("--{name}"), value.clone()))
                .collect::<Vec<_>>()
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect::<Vec<_>>(),
        )],
    };
    let emitted = styles_css(Some(&carried), &MarkupSheet::default(), CssMode::Sheet);
    assert!(
        !emitted.contains(&lumen_ir::css::palette_root_css()),
        "a stylesheet that already carries the palette does not get a second:\n{emitted}"
    );
    assert_eq!(emitted.matches("--accent-color:").count(), 1, "{emitted}");
}

#[test]
fn the_reset_comes_before_anything_the_app_says() {
    let emitted = styles_css(
        Some(&sheet(vec![rule("button", &[("bg", "#0a3358")])])),
        &MarkupSheet::default(),
        CssMode::Sheet,
    );
    assert!(
        emitted.starts_with("@layer lumen.reset, lumen.sheet;"),
        "{emitted}"
    );
    let reset = emitted.find("box-sizing: border-box").expect("the reset");
    let app = emitted.find("#0a3358").expect("the app's own rule");
    assert!(reset < app, "{emitted}");
}

/// A site whose entry page carries a stylesheet.
fn site(mode: CssMode) -> SiteSpec {
    let mut ir = LayoutIR {
        root: Element {
            tag: "root".to_string(),
            attrs: Attributes {
                bg: Some(lumen_ir::layout_ir::BgSpec::Solid(
                    lumen_ir::layout_ir::Rgba {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                )),
                ..Attributes::default()
            },
            ..Element::default()
        },
        ..LayoutIR::default()
    };
    ir.combined_stylesheet = Some(sheet(vec![rule("root", &[("bg", "#000000")])]));
    SiteSpec {
        pages: vec![PageSpec::new("index", ir)],
        web: WebSpec {
            title: "Demo".into(),
            css_mode: mode,
            ..WebSpec::default()
        },
        locale: LocaleSpec::new("en-US"),
        assets: Vec::new(),
        markup: MarkupSheet::default(),
    }
}

#[test]
fn the_site_emits_the_stylesheet_its_documents_link_to() {
    let spec = site(CssMode::Sheet);
    let site = emit(&spec).expect("emits");
    let css = site.file(&spec.web.css).expect("styles.css is emitted");
    assert!(
        css.contents.contains(":where(.lm-root)"),
        "{}",
        css.contents
    );
    let page = site.file("index.html").expect("the page is emitted");
    assert!(
        page.contents.contains(&format!(
            "<link rel=\"stylesheet\" href=\"/{}\">",
            spec.web.css
        )),
        "{}",
        page.contents
    );
}

#[test]
fn computed_mode_puts_the_resolved_values_on_the_elements() {
    let spec = site(CssMode::Computed);
    let site = emit(&spec).expect("emits");
    let css = site.file(&spec.web.css).expect("styles.css is emitted");
    assert!(
        !css.contents.contains(":where(.lm-root)"),
        "computed mode emits no rules:\n{}",
        css.contents
    );
    assert!(css.contents.contains("box-sizing: border-box"));
    let page = site.file("index.html").expect("the page is emitted");
    assert!(
        page.contents.contains("style=\"background:#000000\""),
        "{}",
        page.contents
    );
}
