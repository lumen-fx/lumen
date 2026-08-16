//! Emitting a site from hand-built IR.

use std::collections::HashMap;

use lumen_html::contract::{
    DEFAULT_CSS_FILE, DEFAULT_MANIFEST_FILE, LM_CONTRACT_VERSION, Manifest, Seed, SeedValue,
};
use lumen_ir::layout_ir::{Attributes, Element, FragmentUse, IfModeSpec, LayoutIR};
use lumen_web::{
    EmitError, HostRewrite, LocaleSpec, PageSpec, SignalEnv, Site, SiteSpec, WebSpec, emit,
};

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
fn a_site_is_a_document_per_page_plus_the_stylesheet_and_the_manifest() {
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
        vec![
            "index.html",
            "settings.html",
            "404.html",
            DEFAULT_CSS_FILE,
            DEFAULT_MANIFEST_FILE
        ]
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
    assert!(html.contains(r#"<img class="lm-image" src="/logo.png" alt="Lumen" data-lm="0.0">"#));
    assert!(!html.contains("</img>"));
    assert!(!html.contains("</input>"));
    assert_well_formed(&html);
}

#[test]
fn a_style_written_in_markup_outranks_the_stylesheet() {
    let page = PageSpec::new(
        "index",
        ir(element(
            "root",
            Attributes::default(),
            vec![element(
                "tile",
                Attributes {
                    markup_styles: vec![
                        ("bg".into(), "#101014".into()),
                        ("gap".into(), "8".into()),
                    ],
                    ..Attributes::default()
                },
                Vec::new(),
            )],
        )),
    );
    let html = page_html(&site(vec![page]), "index.html");
    assert!(
        html.contains(r#"style="background: #101014 !important; gap: 8px !important;""#),
        "{html}"
    );
    assert_well_formed(&html);
}

#[test]
fn an_image_carries_the_alt_its_author_wrote() {
    let page = PageSpec::new(
        "index",
        ir(element(
            "root",
            Attributes::default(),
            vec![element(
                "image",
                Attributes {
                    src: Some("logo.png".into()),
                    alt: Some("The Lumen logo".into()),
                    ..Attributes::default()
                },
                Vec::new(),
            )],
        )),
    );
    let html = page_html(&site(vec![page]), "index.html");
    assert!(html.contains(r#"alt="The Lumen logo""#), "{html}");
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
fn a_dialog_is_open_when_the_signal_it_names_is_true() {
    let dialog = element(
        "dialog",
        Attributes {
            if_signal: Some("dialog_open".into()),
            if_mode: IfModeSpec::Hide,
            ..Attributes::default()
        },
        vec![element("label", labelled("Confirm?"), Vec::new())],
    );
    let page = PageSpec::new(
        "index",
        ir(element("root", Attributes::default(), vec![dialog])),
    );
    let mut spec = site(vec![page]);

    let closed = page_html(&spec, "index.html");
    assert!(
        closed.contains(r#"<dialog class="lm-dialog" data-lm="0.0" data-lm-hidden="">"#),
        "a dialog whose signal is false is neither open nor shown: {closed}"
    );
    assert!(closed.contains(">Confirm?<"), "its body stays mounted");

    spec.pages[0].signals = SignalEnv::new().with_global("dialog_open", "true");
    let shown = page_html(&spec, "index.html");
    assert!(
        shown.contains(r#"<dialog class="lm-dialog" data-lm="0.0" open="">"#),
        "{shown}"
    );
    assert!(!shown.contains("data-lm-hidden"));
}

#[test]
fn a_dialog_with_no_signal_is_always_open() {
    let dialog = element(
        "dialog",
        Attributes::default(),
        vec![element("label", labelled("Always"), Vec::new())],
    );
    let page = PageSpec::new(
        "index",
        ir(element("root", Attributes::default(), vec![dialog])),
    );
    let html = page_html(&site(vec![page]), "index.html");
    assert!(html.contains(r#"open="""#), "{html}");
    assert!(!html.contains("data-lm-hidden"));
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

#[test]
fn the_entry_page_is_the_document_a_server_hands_out_for_the_site() {
    let mut spec = site(vec![
        PageSpec::new("main", ir(element("root", Attributes::default(), vec![]))),
        PageSpec::new(
            "settings",
            ir(element("root", Attributes::default(), vec![])),
        ),
    ]);
    spec.web.entry = "main".into();
    let site = emitted(&spec);
    let paths: Vec<&str> = site.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"index.html"), "{paths:?}");
    assert!(!paths.contains(&"main.html"), "{paths:?}");

    let manifest: Manifest =
        serde_json::from_str(&site.file(DEFAULT_MANIFEST_FILE).expect("manifest").contents)
            .expect("manifest parses");
    assert_eq!(
        manifest.pages.get("main").map(String::as_str),
        Some("index.html")
    );

    // Two pages cannot both be the site's front door.
    let mut clash = site_spec_with_index_and_main();
    clash.web.entry = "main".into();
    assert_eq!(
        emit(&clash),
        Err(EmitError::DuplicateDocument("index.html".into()))
    );
}

/// A site keyed both `index` and `main`, which only collides once `main` is
/// the entry.
fn site_spec_with_index_and_main() -> SiteSpec {
    site(vec![
        PageSpec::new("main", ir(element("root", Attributes::default(), vec![]))),
        simple_page(),
    ])
}

#[test]
fn a_link_to_a_page_points_at_the_document_it_was_emitted_as() {
    let mut spec = site(vec![
        simple_page(),
        PageSpec::new(
            "settings",
            ir(element("root", Attributes::default(), vec![])),
        ),
    ]);
    spec.web.base_path = "/docs".into();
    let html = page_html(&spec, "index.html");
    assert!(html.contains(r#"href="/docs/settings.html""#), "{html}");
}

#[test]
fn every_site_carries_the_shell_a_deep_path_falls_back_to() {
    let mut page = simple_page();
    page.signals = SignalEnv::new().with_global("route.path", "index");
    page.ir.root.children.push(element(
        "if",
        Attributes {
            if_signal: Some("route.path".into()),
            if_eq: Some("index".into()),
            ..Attributes::default()
        },
        vec![element("label", labelled("Only on the page"), Vec::new())],
    ));
    let site = emitted(&site(vec![page]));
    let shell = site
        .file("404.html")
        .expect("the shell is emitted")
        .contents
        .clone();
    // The shell shows no page: which one it is comes from the address bar.
    assert!(!shell.contains("Only on the page"), "{shell}");
    assert!(shell.contains(r#"data-lm-page="index""#));
    assert!(
        site.file("index.html")
            .expect("index")
            .contents
            .contains("Only on the page")
    );
}

#[test]
fn a_host_that_can_rewrite_gets_the_file_that_tells_it_to() {
    let mut spec = site(vec![simple_page()]);
    spec.web.base_path = "/docs".into();
    spec.web.host = HostRewrite::Netlify;
    assert_eq!(
        emitted(&spec)
            .file("_redirects")
            .expect("rewrites")
            .contents,
        "/docs/*  /docs/404.html  200\n"
    );

    spec.web.host = HostRewrite::Vercel;
    assert!(
        emitted(&spec)
            .file("vercel.json")
            .expect("rewrites")
            .contents
            .contains("\"destination\": \"/docs/404.html\"")
    );

    spec.web.host = HostRewrite::Static;
    assert!(emitted(&spec).file("_redirects").is_none());
    assert!(emitted(&spec).file("vercel.json").is_none());
}

#[test]
fn a_locale_tree_sits_under_its_tag_and_shares_the_site_s_files() {
    let mut spec = site(vec![
        simple_page(),
        PageSpec::new(
            "settings",
            ir(element("root", Attributes::default(), vec![])),
        ),
    ]);
    spec.web.url = Some("https://example.com".into());
    spec.locale = LocaleSpec {
        default_locale: "en-US".into(),
        alternates: vec!["en-US".into()],
        ..LocaleSpec::new("de-DE")
    };
    let site = emitted(&spec);
    let paths: Vec<&str> = site.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["de-DE/index.html", "de-DE/settings.html", "de-DE/404.html"]
    );

    let html = site
        .file("de-DE/index.html")
        .expect("the German home page")
        .contents
        .clone();
    // Its own links stay in its own tree; the stylesheet is the site's.
    assert!(html.contains(r#"href="/de-DE/settings.html""#), "{html}");
    assert!(html.contains(r#"href="/styles.css""#), "{html}");
    assert!(
        html.contains(r#"<link rel="canonical" href="https://example.com/de-DE/index.html">"#),
        "{html}"
    );
    assert!(
        html.contains(r#"hreflang="en-US" href="https://example.com/index.html""#),
        "{html}"
    );
    assert!(
        html.contains(r#"hreflang="x-default" href="https://example.com/index.html""#),
        "{html}"
    );
}

#[test]
fn a_site_with_a_url_lists_its_pages_for_a_crawler() {
    let mut spec = site(vec![
        simple_page(),
        PageSpec::new(
            "settings",
            ir(element("root", Attributes::default(), vec![])),
        ),
    ]);
    spec.web.url = Some("https://example.com".into());
    spec.web.sitemap = true;
    let sitemap = emitted(&spec)
        .file("sitemap.xml")
        .expect("a sitemap")
        .contents
        .clone();
    assert!(
        sitemap.contains("<loc>https://example.com/index.html</loc>"),
        "{sitemap}"
    );
    assert!(
        sitemap.contains("<loc>https://example.com/settings.html</loc>"),
        "{sitemap}"
    );

    spec.web.sitemap = false;
    assert!(emitted(&spec).file("sitemap.xml").is_none());
}

#[test]
fn a_site_emitted_without_the_runtime_loads_nothing() {
    let mut spec = site(vec![simple_page()]);
    spec.web.runtime = false;
    let html = page_html(&spec, "index.html");
    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("lumen-web.wasm"), "{html}");
    // The page itself is all there, which is the point of the mode.
    assert!(html.contains("Hello"));
    assert!(html.contains(r#"href="/styles.css""#));
}

#[test]
fn a_fragment_that_was_never_expanded_is_an_error() {
    let mut placeholder = element("column", Attributes::default(), Vec::new());
    placeholder.frag_use = Some(Box::new(FragmentUse {
        key: "card".into(),
        args: Vec::new(),
        slot_children: false,
    }));
    let page = PageSpec::new(
        "index",
        ir(element("root", Attributes::default(), vec![placeholder])),
    );
    assert_eq!(
        emit(&site(vec![page])),
        Err(EmitError::UnexpandedFragment {
            page: "index".into(),
            key: "card".into(),
        })
    );
}
