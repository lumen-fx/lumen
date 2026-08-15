//! Emitting a site from hand-built IR.

use std::collections::HashMap;

use lumen_html::contract::{DEFAULT_MANIFEST_FILE, LM_CONTRACT_VERSION, Manifest, Seed, SeedValue};
use lumen_ir::layout_ir::{Attributes, Element, IfModeSpec, LayoutIR};
use lumen_web::{EmitError, LocaleSpec, PageSpec, SignalEnv, Site, SiteSpec, WebSpec, emit};

fn element(tag: &str, attrs: Attributes, children: Vec<Element>) -> Element {
    Element {
        tag: tag.to_string(),
        attrs,
        children,
        ..Element::default()
    }
}

fn labelled(text: &str) -> Attributes {
    Attributes {
        text: Some(text.to_string()),
        ..Attributes::default()
    }
}

fn ir(root: Element) -> LayoutIR {
    LayoutIR {
        root,
        ..LayoutIR::default()
    }
}

/// A page whose root holds a heading and a link.
fn simple_page() -> PageSpec {
    PageSpec::new(
        "index",
        ir(element(
            "root",
            Attributes::default(),
            vec![
                element("label", labelled("Hello"), Vec::new()),
                element(
                    "a",
                    Attributes {
                        href: Some("settings".into()),
                        text: Some("Settings".into()),
                        ..Attributes::default()
                    },
                    Vec::new(),
                ),
            ],
        )),
    )
}

fn site(pages: Vec<PageSpec>) -> SiteSpec {
    SiteSpec {
        pages,
        web: WebSpec {
            title: "Demo".into(),
            ..WebSpec::default()
        },
        locale: LocaleSpec::new("en-US"),
        assets: Vec::new(),
    }
}

fn emitted(spec: &SiteSpec) -> Site {
    emit(spec).expect("emits")
}

fn page_html(spec: &SiteSpec, name: &str) -> String {
    emitted(spec)
        .file(name)
        .unwrap_or_else(|| panic!("no `{name}` in the site"))
        .contents
        .clone()
}

/// Every `data-lm` value in a document, in document order.
fn node_paths(html: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("data-lm=\"") {
        rest = &rest[at + "data-lm=\"".len()..];
        let end = rest.find('"').expect("unterminated attribute");
        paths.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    paths
}

/// Fail unless every element in the document is closed, in order.
fn assert_well_formed(html: &str) {
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(at) = html[i..].find('<') {
        let start = i + at;
        if html[start..].starts_with("<!") {
            i = start + html[start..].find('>').expect("unterminated declaration") + 1;
            continue;
        }
        let end = start + html[start..].find('>').expect("unterminated tag");
        let inner = &html[start + 1..end];
        i = end + 1;
        if let Some(name) = inner.strip_prefix('/') {
            assert_eq!(
                stack.pop().as_deref(),
                Some(name.trim()),
                "`</{name}>` closes nothing"
            );
            continue;
        }
        let name = inner
            .split(|c: char| c.is_whitespace())
            .next()
            .expect("tag name")
            .to_string();
        assert!(!name.is_empty(), "empty tag name");
        if lumen_html::is_void(&name) {
            continue;
        }
        // `script` holds text, not markup: skip to its end tag.
        if name == "script" || name == "style" {
            let close = format!("</{name}>");
            i = i + html[i..].find(&close).expect("unterminated script") + close.len();
            continue;
        }
        stack.push(name);
    }
    assert!(stack.is_empty(), "unclosed elements: {stack:?}");
}

#[test]
fn a_site_is_a_document_per_page_and_one_manifest() {
    let site = emitted(&site(vec![
        simple_page(),
        PageSpec::new(
            "settings",
            ir(element("root", Attributes::default(), vec![])),
        ),
    ]));
    let paths: Vec<&str> = site.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["index.html", "settings.html", DEFAULT_MANIFEST_FILE]
    );

    let manifest: Manifest =
        serde_json::from_str(&site.file(DEFAULT_MANIFEST_FILE).expect("manifest").contents)
            .expect("manifest parses");
    assert_eq!(manifest.contract_version, LM_CONTRACT_VERSION);
    assert_eq!(manifest.pages.len(), 2);
    assert_eq!(manifest.entry, "index");
}

#[test]
fn every_element_carries_its_path_from_the_page_root() {
    let html = page_html(&site(vec![simple_page()]), "index.html");
    assert_eq!(node_paths(&html), vec!["0", "0.0", "0.1"]);
    assert!(html.contains(r#"<div class="lm-root" data-lm="0">"#));
    assert!(html.contains(r#"<span class="lm-label" data-lm="0.0">Hello</span>"#));
}

#[test]
fn nested_children_number_down_the_tree() {
    let page = PageSpec::new(
        "index",
        ir(element(
            "root",
            Attributes::default(),
            vec![element(
                "column",
                Attributes::default(),
                vec![
                    element("label", labelled("one"), Vec::new()),
                    element(
                        "row",
                        Attributes::default(),
                        vec![element("label", labelled("two"), Vec::new())],
                    ),
                ],
            )],
        )),
    );
    let html = page_html(&site(vec![page]), "index.html");
    assert_eq!(
        node_paths(&html),
        vec!["0", "0.0", "0.0.0", "0.0.1", "0.0.1.0"]
    );
}

#[test]
fn no_two_nodes_of_a_page_share_a_path() {
    let mut children = Vec::new();
    for i in 0..8 {
        children.push(element(
            "column",
            Attributes::default(),
            vec![element("label", labelled(&format!("row {i}")), Vec::new())],
        ));
    }
    let page = PageSpec::new(
        "index",
        ir(element("root", Attributes::default(), children)),
    );
    let html = page_html(&site(vec![page]), "index.html");
    let paths = node_paths(&html);
    let mut unique = paths.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        paths.len(),
        unique.len(),
        "duplicate node path in {paths:?}"
    );
}

#[test]
fn emitting_twice_gives_the_same_bytes() {
    let spec = site(vec![
        simple_page(),
        PageSpec::new("about", ir(element("root", Attributes::default(), vec![]))),
    ]);
    assert_eq!(emit(&spec).expect("emits"), emit(&spec).expect("emits"));
}

#[test]
fn text_and_attribute_values_are_escaped() {
    let page = PageSpec::new(
        "index",
        ir(element(
            "root",
            Attributes::default(),
            vec![element(
                "label",
                Attributes {
                    text: Some("2 < 3 & \"quoted\"".into()),
                    id: Some(r#"a" onload="x"#.into()),
                    ..Attributes::default()
                },
                Vec::new(),
            )],
        )),
    );
    let html = page_html(&site(vec![page]), "index.html");
    assert!(html.contains("2 &lt; 3 &amp; \"quoted\""));
    assert!(html.contains(r#"id="a&quot; onload=&quot;x""#));
    assert!(!html.contains("onload=\"x\""));
    assert_well_formed(&html);
}

#[test]
fn a_text_bearing_element_holds_exactly_its_text() {
    let html = page_html(&site(vec![simple_page()]), "index.html");
    let tree = html
        .split("<body>\n")
        .nth(1)
        .expect("body")
        .split("\n<script")
        .next()
        .expect("tree");
    assert!(!tree.contains('\n'), "the tree was pretty-printed: {tree}");
    assert!(tree.contains(">Hello<"));
}

#[test]
fn documents_are_well_formed() {
    let spec = site(vec![simple_page()]);
    assert_well_formed(&page_html(&spec, "index.html"));
}

#[test]
fn a_void_element_has_no_end_tag() {
    let page = PageSpec::new(
        "index",
        ir(element(
            "root",
            Attributes::default(),
            vec![
                element(
                    "image",
                    Attributes {
                        src: Some("logo.png".into()),
                        text: Some("Lumen".into()),
                        ..Attributes::default()
                    },
                    Vec::new(),
                ),
                element("checkbox", Attributes::default(), Vec::new()),
            ],
        )),
    );
    let html = page_html(&site(vec![page]), "index.html");
    assert!(html.contains(r#"<img class="lm-image" src="logo.png" alt="Lumen" data-lm="0.0">"#));
    assert!(!html.contains("</img>"));
    assert!(!html.contains("</input>"));
    assert_well_formed(&html);
}

#[test]
fn a_hidden_branch_stays_in_the_document() {
    let branch = element(
        "if",
        Attributes {
            if_signal: Some("ready".into()),
            if_mode: IfModeSpec::Hide,
            ..Attributes::default()
        },
        vec![element("label", labelled("done"), Vec::new())],
    );
    let page = PageSpec::new(
        "index",
        ir(element("root", Attributes::default(), vec![branch])),
    );
    let mut spec = site(vec![page]);

    let html = page_html(&spec, "index.html");
    assert!(html.contains(r#"class="lm-if" data-lm="0.0" data-lm-hidden="""#));
    assert!(html.contains(">done<"));

    spec.pages[0].signals = SignalEnv::new().with_global("ready", "true");
    let shown = page_html(&spec, "index.html");
    assert!(!shown.contains("data-lm-hidden"));
    assert!(shown.contains(">done<"));
}

#[test]
fn a_rendered_branch_is_there_only_when_its_signal_holds() {
    let branch = element(
        "if",
        Attributes {
            if_signal: Some("route".into()),
            if_eq: Some("home".into()),
            ..Attributes::default()
        },
        vec![element("label", labelled("home"), Vec::new())],
    );
    let page = PageSpec::new(
        "index",
        ir(element("root", Attributes::default(), vec![branch])),
    );
    let mut spec = site(vec![page]);

    let empty = page_html(&spec, "index.html");
    assert!(empty.contains(r#"<div class="lm-if" data-lm="0.0"></div>"#));

    spec.pages[0].signals = SignalEnv::new().with_global("route", "home");
    let taken = page_html(&spec, "index.html");
    assert!(taken.contains(">home<"));
    assert_eq!(node_paths(&taken), vec!["0", "0.0", "0.0.0"]);
}

#[test]
fn a_for_block_emits_its_anchor_and_no_rows() {
    let block = element(
        "for",
        Attributes {
            each: Some("items".into()),
            key: Some("id".into()),
            ..Attributes::default()
        },
        vec![element("label", labelled("{row.name}"), Vec::new())],
    );
    let page = PageSpec::new(
        "index",
        ir(element("root", Attributes::default(), vec![block])),
    );
    let rows = vec![HashMap::from([("name".to_string(), "one".to_string())])];
    let mut spec = site(vec![page]);
    spec.pages[0].signals = SignalEnv::new().with_array("items", rows);

    let html = page_html(&spec, "index.html");
    assert!(html.contains(r#"<div class="lm-for" data-lm="0.0"></div>"#));
    assert!(!html.contains("row.name"));
}

#[test]
fn the_seed_block_is_json_the_runtime_can_read() {
    let mut page = simple_page();
    page.seed = Seed::new();
    page.seed.globals.insert("count".into(), SeedValue::I64(3));
    let html = page_html(&site(vec![page]), "index.html");

    let block = html
        .split(r#"<script type="application/json" id="lm-seed">"#)
        .nth(1)
        .expect("seed block")
        .split("</script>")
        .next()
        .expect("seed block end");
    let seed: Seed = serde_json::from_str(block).expect("seed parses");
    assert_eq!(seed.globals.get("count"), Some(&SeedValue::I64(3)));
    assert_eq!(seed.contract_version, LM_CONTRACT_VERSION);
}

#[test]
fn the_head_carries_what_a_crawler_reads() {
    let mut spec = site(vec![simple_page()]);
    spec.web.url = Some("https://example.com".into());
    spec.web.base_path = "/docs".into();
    spec.web.description = Some("A demo app".into());
    spec.web.og_image = Some("card.png".into());
    let html = page_html(&spec, "index.html");

    assert!(html.starts_with("<!doctype html>\n<html lang=\"en-US\" dir=\"ltr\""));
    assert!(html.contains(&format!(r#"data-lm-contract="{LM_CONTRACT_VERSION}""#)));
    assert!(html.contains(r#"data-lm-page="index""#));
    assert!(html.contains(r#"data-lm-base="/docs/""#));
    assert!(html.contains("<title>Demo</title>"));
    assert!(html.contains(r#"<meta name="description" content="A demo app">"#));
    assert!(html.contains(r#"<link rel="canonical" href="https://example.com/docs/index.html">"#));
    assert!(
        html.contains(r#"<meta property="og:image" content="https://example.com/docs/card.png">"#)
    );
    assert!(html.contains(r#"<meta name="twitter:card" content="summary_large_image">"#));
    assert!(html.contains(r#"<link rel="stylesheet" href="/docs/styles.css">"#));
    assert_well_formed(&html);
}

#[test]
fn a_page_title_beats_the_site_title() {
    let mut page = simple_page();
    page.title = Some("Settings".into());
    let html = page_html(&site(vec![page]), "index.html");
    assert!(html.contains("<title>Settings</title>"));
}

#[test]
fn every_page_boots_the_same_way() {
    let site = emitted(&site(vec![
        simple_page(),
        PageSpec::new("about", ir(element("root", Attributes::default(), vec![]))),
    ]));
    let boot = r#"<script type="module">import init, { boot } from "/lumen-web.js";init().then(boot);</script>"#;
    for file in site.files.iter().filter(|f| f.path.ends_with(".html")) {
        assert!(file.contents.contains(boot), "{} has no boot", file.path);
    }
}

#[test]
fn locale_direction_follows_the_language() {
    let mut spec = site(vec![simple_page()]);
    spec.locale = LocaleSpec::new("ar-EG");
    let html = page_html(&spec, "index.html");
    assert!(html.contains(r#"<html lang="ar-EG" dir="rtl""#));
}

#[test]
fn a_tag_with_no_html_mapping_is_an_error() {
    let page = PageSpec::new(
        "index",
        ir(element(
            "root",
            Attributes::default(),
            vec![element("sparkline", Attributes::default(), Vec::new())],
        )),
    );
    assert_eq!(
        emit(&site(vec![page])),
        Err(EmitError::UnknownTag {
            page: "index".into(),
            tag: "sparkline".into(),
        })
    );
}

#[test]
fn a_site_needs_pages_with_distinct_keys() {
    assert_eq!(emit(&site(Vec::new())), Err(EmitError::NoPages));
    assert_eq!(
        emit(&site(vec![simple_page(), simple_page()])),
        Err(EmitError::DuplicatePage("index".into()))
    );

    let mut spec = site(vec![simple_page()]);
    spec.web.entry = "missing".into();
    assert_eq!(emit(&spec), Err(EmitError::UnknownEntry("missing".into())));

    let mut spec = site(vec![PageSpec::new(
        "",
        ir(element("root", Attributes::default(), vec![])),
    )]);
    spec.web.entry = String::new();
    assert_eq!(emit(&spec), Err(EmitError::EmptyPageKey));
}
