//! What a visitor gets back, and what the visitor after them does not.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use lumen_core::property_store::{PropertyKey, PropertyValue, push_external_property};
use lumen_ir::artifact::{CompiledApp, CompiledPages, CompiledScript};
use lumen_ir::fragment::{Fragment, FragmentKind, FragmentParam, FragmentTable};
use lumen_ir::layout_ir::{Attributes, Element, FragmentUse, LayoutIR};
use lumen_script::{HttpDispatch, HttpDone, HttpRequest, HttpResponse};
use lumen_ssr::{
    Budget, FetchPolicy, HeaderPolicy, RenderOptions, Renderer, SsrError, SsrRequest, SsrResponse,
    SsrSite,
};
use lumen_web::{LocaleSpec, PageSpec, SiteSpec, WebSpec};

/// A program that publishes what it can read of the request.
const READS_REQUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/reads_request.cdlb"));

/// A program that answers with a status, headers and a redirect.
const ANSWERS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/answers.cdlb"));

/// A program that asks an API for what the page shows.
const FETCHES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fetches.cdlb"));

/// A program holding the component the tree below leaves a marker for.
const COMPONENTS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/components.cdlb"));

/// The name the renderer's thread carries, which is where every app is built
/// and dropped.
const WORKER: &str = "lumen-ssr";

/// A process has one renderer, so the tests take it in turn.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn in_turn() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

fn element(tag: &str, attrs: Attributes, children: Vec<Element>) -> Element {
    Element {
        tag: tag.to_string(),
        attrs,
        children,
        ..Element::default()
    }
}

/// A branch that is in the document when `signal` holds `value`.
fn gate(signal: &str, value: &str, text: &str) -> Element {
    let label = element(
        "label",
        Attributes {
            text: Some(text.to_string()),
            ..Attributes::default()
        },
        Vec::new(),
    );
    element(
        "if",
        Attributes {
            if_signal: Some(signal.to_string()),
            if_eq: Some(value.to_string()),
            ..Attributes::default()
        },
        vec![label],
    )
}

/// A two-page app whose markup says which page it is on and whether the data
/// arrived.
fn app_with(program: &[u8]) -> CompiledApp {
    CompiledApp {
        ir: LayoutIR {
            root: element(
                "root",
                Attributes::default(),
                vec![
                    gate("route.path", "user", "the user page"),
                    gate("status", "ready", "the data arrived"),
                ],
            ),
            ..LayoutIR::default()
        },
        pages: Some(CompiledPages {
            entry: "index".to_string(),
            keys: vec!["user".to_string(), "index".to_string()],
        }),
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(program.to_vec()),
        }],
        ..CompiledApp::default()
    }
}

fn site(program: &[u8]) -> Arc<SsrSite> {
    Arc::new(SsrSite::new(app_with(program), WebSpec::default()).expect("the entry is a page"))
}

/// Options that reach the test's own transport and nothing else.
fn options(dispatch: Arc<dyn HttpDispatch>) -> RenderOptions {
    RenderOptions {
        fetch: FetchPolicy::default().allow_host("api.example.com"),
        dispatch,
        ..RenderOptions::default()
    }
}

/// A transport that answers nothing, for an app that asks nothing.
struct Silent;

impl HttpDispatch for Silent {
    fn dispatch(&self, _label: &str, request: HttpRequest, _body_limit: u64, done: HttpDone) {
        done(Err(format!("nothing answers {} in this test", request.url)));
    }
}

fn render(program: &[u8], request: SsrRequest) -> SsrResponse {
    let renderer =
        Renderer::start(site(program), options(Arc::new(Silent))).expect("nothing else is running");
    renderer.render(request).expect("the document is written")
}

#[test]
fn a_deep_path_reaches_its_page_with_the_rest_left_for_it() {
    let _turn = in_turn();
    let renderer =
        Renderer::start(site(READS_REQUEST), options(Arc::new(Silent))).expect("nothing running");

    let deep = renderer
        .render(SsrRequest::get("/user/42"))
        .expect("the document is written");
    assert!(deep.body.contains("the user page"), "{}", deep.body);
    // The tail of the path is the page's to read, and it reaches the browser
    // with the rest of the state.
    assert!(deep.body.contains("/42"), "{}", deep.body);

    let home = renderer
        .render(SsrRequest::get("/"))
        .expect("the document is written");
    assert!(!home.body.contains("the user page"), "{}", home.body);
}

#[test]
fn a_link_inside_the_site_reaches_the_page_it_points_at() {
    let _turn = in_turn();
    let renderer =
        Renderer::start(site(READS_REQUEST), options(Arc::new(Silent))).expect("nothing running");

    // What a build writes a link to a page as, which is what a visitor
    // clicking one asks for.
    for target in ["/user.html", "/user.html?tab=posts"] {
        let page = renderer
            .render(SsrRequest::get(target))
            .expect("the document is written");
        assert!(
            page.body.contains("the user page"),
            "{target}: {}",
            page.body
        );
    }

    // The entry page's own document answers with the entry page.
    let page = renderer
        .render(SsrRequest::get("/index.html"))
        .expect("the document is written");
    assert!(!page.body.contains("the user page"), "{}", page.body);
}

#[test]
fn an_address_no_page_answers_for_is_not_found() {
    let _turn = in_turn();
    let renderer =
        Renderer::start(site(READS_REQUEST), options(Arc::new(Silent))).expect("nothing running");

    // A document the build never wrote, and a path that starts at no page.
    // Both get the status a static host sends and the shell it sends with it,
    // so a site answers such an address the same way whichever half answers.
    for target in ["/nowhere.html", "/nowhere", "/nowhere/deeper"] {
        let missing = renderer
            .render(SsrRequest::get(target))
            .expect("the shell is written");
        assert_eq!(missing.status, 404, "{target}");
        assert_eq!(
            missing.header("Content-Type"),
            Some("text/html; charset=utf-8"),
            "{target}"
        );
        assert!(
            !missing.body.is_empty(),
            "{target}: the shell is a document"
        );
        assert!(
            !missing.body.contains("the user page"),
            "{target}: the shell shows no page: {}",
            missing.body
        );
    }

    // A deep path a page does answer for is a page, not a miss. That is what
    // `route.segment` is for, and the not-found rule leaves it alone.
    let deep = renderer
        .render(SsrRequest::get("/user/42"))
        .expect("the document is written");
    assert_eq!(deep.status, 200);
    assert!(deep.body.contains("the user page"), "{}", deep.body);
}

#[test]
fn the_app_reads_the_request_it_is_rendered_for() {
    let _turn = in_turn();
    let request = SsrRequest::new("POST", "/user/42?tab=posts")
        .with_header("Accept-Language", "en-GB")
        .with_header("Authorization", "Bearer swordfish")
        .with_header("Cookie", "session=a-session-value")
        .with_body("a body the page can read");
    let response = render(READS_REQUEST, request);

    for expected in [
        "en-GB",
        "a-session-value",
        "a body the page can read",
        "tab=posts",
        "/user/42",
        "POST",
    ] {
        assert!(
            response.body.contains(expected),
            "the document is missing `{expected}`: {}",
            response.body
        );
    }
    // A credential the app was not given stays out of its reach.
    assert!(!response.body.contains("swordfish"), "{}", response.body);
}

#[test]
fn what_one_visitor_reads_is_not_in_the_next_visitors_page() {
    let _turn = in_turn();
    let renderer =
        Renderer::start(site(READS_REQUEST), options(Arc::new(Silent))).expect("nothing running");

    for round in 0..3 {
        let mine = format!("visitor-{round}");
        let first = renderer
            .render(SsrRequest::get("/").with_header("Accept-Language", &mine))
            .expect("the document is written");
        assert!(first.body.contains(&mine), "{}", first.body);

        let second = renderer
            .render(SsrRequest::get("/"))
            .expect("the document is written");
        assert!(
            !second.body.contains(&mine),
            "round {round} carried the visitor before it: {}",
            second.body
        );
    }
}

/// A transport that answers after a delay, on a thread of its own, which is
/// what a render has to wait for.
struct After(Duration);

impl HttpDispatch for After {
    fn dispatch(&self, _label: &str, _request: HttpRequest, _body_limit: u64, done: HttpDone) {
        let wait = self.0;
        thread::spawn(move || {
            thread::sleep(wait);
            done(Ok(HttpResponse {
                status: 200,
                body: "ready".to_string(),
                ..HttpResponse::default()
            }));
        });
    }
}

#[test]
fn a_render_waits_for_the_data_the_app_asked_for() {
    let _turn = in_turn();
    let response = {
        let renderer = Renderer::start(
            site(FETCHES),
            options(Arc::new(After(Duration::from_millis(50)))),
        )
        .expect("nothing running");
        renderer
            .render(SsrRequest::get("/"))
            .expect("the document is written")
    };

    assert!(
        response.body.contains("the data arrived"),
        "the render did not wait for the reply: {}",
        response.body
    );
    assert_eq!(response.header("X-Lumen-Render"), None);
}

/// A transport that takes a request and never answers it, until the test says
/// so.
#[derive(Default)]
struct Held(Mutex<Vec<HttpDone>>);

impl HttpDispatch for Held {
    fn dispatch(&self, _label: &str, _request: HttpRequest, _body_limit: u64, done: HttpDone) {
        self.0.lock().expect("the test holds the lock").push(done);
    }
}

impl Held {
    fn answer(&self, body: &str) {
        for done in self.0.lock().expect("the test holds the lock").drain(..) {
            done(Ok(HttpResponse {
                status: 200,
                body: body.to_string(),
                ..HttpResponse::default()
            }));
        }
    }
}

#[test]
fn a_reply_that_missed_its_render_reaches_nobody_elses() {
    let _turn = in_turn();
    let held = Arc::new(Held::default());
    let renderer = Renderer::start(
        site(FETCHES),
        RenderOptions {
            budget: Budget {
                ticks: u32::MAX,
                time: Duration::from_millis(150),
            },
            ..options(Arc::clone(&held) as Arc<dyn HttpDispatch>)
        },
    )
    .expect("nothing running");

    let waited = renderer
        .render(SsrRequest::get("/"))
        .expect("the document is written");
    assert_eq!(
        waited.header("X-Lumen-Render"),
        Some("partial"),
        "a render that gave up says so"
    );
    assert!(waited.body.contains("asking"), "{}", waited.body);

    // The upstream answers after the visitor it was answering for has gone.
    held.answer("late-data");
    let next = renderer
        .render(SsrRequest::get("/"))
        .expect("the document is written");
    assert!(
        !next.body.contains("late-data"),
        "a late reply landed in the next visitor's page: {}",
        next.body
    );
}

#[test]
fn a_redirect_answers_instead_of_a_document() {
    let _turn = in_turn();
    let renderer = Renderer::start(
        site(ANSWERS),
        RenderOptions {
            // The app decides where to send the visitor from a header of its
            // own, which is a header it has to be given.
            headers: HeaderPolicy::default().allow("x-go"),
            ..options(Arc::new(Silent))
        },
    )
    .expect("nothing running");
    let response = renderer
        .render(SsrRequest::get("/").with_header("x-go", "/somewhere-else"))
        .expect("the document is written");

    assert_eq!(response.status, 302);
    assert_eq!(response.header("Location"), Some("/somewhere-else"));
    assert!(response.body.is_empty(), "{}", response.body);
}

#[test]
fn the_headers_a_page_may_not_send_are_refused_and_reported() {
    let _turn = in_turn();
    let response = render(ANSWERS, SsrRequest::get("/"));

    assert_eq!(response.status, 404);
    assert_eq!(response.header("X-Made-Up"), Some("yes"));
    assert_eq!(response.header("X-Sneaky"), None);
    assert_eq!(response.header("Content-Length"), None);
    assert_eq!(
        response.warnings.len(),
        2,
        "both refusals are reported: {:?}",
        response.warnings
    );
}

#[test]
fn the_same_request_twice_is_the_same_document() {
    let _turn = in_turn();
    let renderer =
        Renderer::start(site(READS_REQUEST), options(Arc::new(Silent))).expect("nothing running");
    let ask = || {
        renderer
            .render(SsrRequest::get("/user/42?tab=posts").with_header("Accept-Language", "en-GB"))
            .expect("the document is written")
    };
    assert_eq!(ask(), ask());
}

#[test]
fn a_process_renders_one_request_at_a_time() {
    let _turn = in_turn();
    let first =
        Renderer::start(site(READS_REQUEST), options(Arc::new(Silent))).expect("nothing running");
    let second = Renderer::start(site(READS_REQUEST), options(Arc::new(Silent)));
    assert!(matches!(second, Err(SsrError::AlreadyRunning)));

    // The one that started is the one that renders, and the process is free
    // again once it stops.
    first.shutdown();
    let third =
        Renderer::start(site(READS_REQUEST), options(Arc::new(Silent))).expect("the first is gone");
    drop(third);
}

/// Reports the thread it was dropped on, so a test can see where the app that
/// held it went.
struct Witness;

static DROPPED_ON: Mutex<Option<String>> = Mutex::new(None);
static TICKED_ON: Mutex<Option<String>> = Mutex::new(None);

impl Drop for Witness {
    fn drop(&mut self) {
        *DROPPED_ON.lock().expect("the test holds the lock") = thread_name();
    }
}

fn thread_name() -> Option<String> {
    thread::current().name().map(str::to_string)
}

/// Puts a value into the app's own state on the way past, which is a value
/// the app then owns and drops.
#[derive(Default)]
struct Planted(AtomicUsize);

impl HttpDispatch for Planted {
    fn dispatch(&self, _label: &str, _request: HttpRequest, _body_limit: u64, done: HttpDone) {
        *TICKED_ON.lock().expect("the test holds the lock") = thread_name();
        // Only the first request plants one: what this proves is where the
        // app that holds it is dropped, and one is enough to show that.
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            push_external_property(
                PropertyKey::global("planted"),
                PropertyValue::Custom(Arc::new(Witness)),
            );
        }
        done(Err("nothing answers in this test".to_string()));
    }
}

#[test]
fn the_app_is_built_and_dropped_on_the_renderers_own_thread() {
    let _turn = in_turn();
    let response = {
        let renderer = Renderer::start(site(FETCHES), options(Arc::new(Planted::default())))
            .expect("nothing running");
        renderer
            .render(SsrRequest::get("/"))
            .expect("the document is written")
    };

    assert_eq!(
        TICKED_ON
            .lock()
            .expect("the test holds the lock")
            .as_deref(),
        Some(WORKER),
        "the app ticks on the renderer's thread, not the caller's"
    );
    assert_eq!(
        DROPPED_ON
            .lock()
            .expect("the test holds the lock")
            .as_deref(),
        Some(WORKER),
        "and the app that held the value is dropped there too"
    );
    // A value a document cannot carry is named rather than dropped in silence.
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("planted")),
        "{:?}",
        response.warnings
    );
}

#[test]
fn an_app_reaches_only_the_hosts_it_was_given() {
    let _turn = in_turn();
    let renderer = Renderer::start(
        site(FETCHES),
        RenderOptions {
            fetch: FetchPolicy::default(),
            dispatch: Arc::new(Silent),
            headers: HeaderPolicy::default(),
            ..RenderOptions::default()
        },
    )
    .expect("nothing running");

    let response = renderer
        .render(SsrRequest::get("/"))
        .expect("the document is written");
    // The app carries on with what it has, which is what it does in a browser
    // with no network.
    assert!(response.body.contains("refused"), "{}", response.body);
}

/// A component that has to run, as an app ships it: the tree carries a marker
/// and the fragment it builds from travels beside it.
fn app_with_a_component() -> CompiledApp {
    let mut marker = element("Shout", Attributes::default(), Vec::new());
    marker.frag_use = Some(Box::new(FragmentUse {
        key: "Shout".to_string(),
        args: vec![("who".to_string(), "ann".to_string())],
        slot_children: false,
    }));
    let mut table = FragmentTable::new();
    table
        .insert(Fragment {
            key: "shout".to_string(),
            params: vec![FragmentParam {
                name: "who".to_string(),
                default: None,
            }],
            body: vec![element(
                "label",
                Attributes {
                    text: Some("{who}".to_string()),
                    ..Attributes::default()
                },
                Vec::new(),
            )],
            origins: Vec::new(),
            kind: FragmentKind::Markup,
            components: Vec::new(),
        })
        .expect("one key");
    CompiledApp {
        ir: LayoutIR {
            root: element(
                "root",
                Attributes::default(),
                vec![marker, gate("shouted", "ann!", "the call ran")],
            ),
            ..LayoutIR::default()
        },
        fragments: table,
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(COMPONENTS.to_vec()),
        }],
        ..CompiledApp::default()
    }
}

/// An artifact whose components were never resolved still renders, and the
/// render still runs them.
///
/// `lumenc web` resolves a component while it builds the site, so the app a
/// server is meant to be handed carries markup here rather than a marker. This
/// is the other artifact: one compiled some other way, with the markers still
/// in it. The render calls the component, so what the call publishes is state
/// the document is written with, and the marker holds the place its body would
/// have taken for the browser to fill.
#[test]
fn an_unresolved_component_still_runs_and_its_marker_holds_its_place() {
    let _turn = in_turn();
    let renderer = Renderer::start(
        Arc::new(
            SsrSite::new(app_with_a_component(), WebSpec::default()).expect("the entry is a page"),
        ),
        options(Arc::new(Silent)),
    )
    .expect("nothing else is running");
    let response = renderer
        .render(SsrRequest::get("/"))
        .expect("the document is written");

    assert!(response.body.contains("the call ran"), "{}", response.body);
    assert!(
        response.body.contains(r#"class="lm-fragment""#),
        "{}",
        response.body
    );
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("built nodes of their own")),
        "{:?}",
        response.warnings
    );
}

/// A program that writes onto a node the markup declares.
const WRITES_NODES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/writes_nodes.cdlb"));

/// A render that writes onto the nodes the markup declares carries what it
/// wrote, so it says nothing about nodes the browser has to build.
#[test]
fn a_class_a_script_set_is_in_the_document_and_is_not_reported_as_missing() {
    let _turn = in_turn();
    let response = render(WRITES_NODES, SsrRequest::get("/"));

    assert!(response.body.contains("theme-dark"), "{}", response.body);
    assert!(
        !response
            .warnings
            .iter()
            .any(|warning| warning.contains("built nodes of their own")),
        "{:?}",
        response.warnings
    );
}

/// A one-page app whose only text is `greeting`, so a document says which
/// language it was rendered in without anything running.
fn app_saying(greeting: &str) -> CompiledApp {
    CompiledApp {
        ir: LayoutIR {
            root: element(
                "root",
                Attributes::default(),
                vec![element(
                    "label",
                    Attributes {
                        translatable: Some("greeting".to_string()),
                        text: Some(greeting.to_string()),
                        ..Attributes::default()
                    },
                    Vec::new(),
                )],
            ),
            ..LayoutIR::default()
        },
        ..CompiledApp::default()
    }
}

/// A site in English at the root and German under `/de-DE/`, the way a build
/// hands one over: the same pages, from a tree translated for each.
fn bilingual() -> Arc<SsrSite> {
    let english =
        SsrSite::new(app_saying("Hello"), WebSpec::default()).expect("the entry is the page");
    let german = SiteSpec {
        pages: vec![PageSpec::new("index", app_saying("Hallo").ir.clone())],
        locale: LocaleSpec {
            default_locale: "en-US".to_string(),
            ..LocaleSpec::new("de-DE")
        },
        ..english.spec().clone()
    };
    Arc::new(english.with_locale(german).expect("it has every page"))
}

/// The value of `name`, whatever case the response wrote it in.
fn header<'a>(response: &'a SsrResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(held, _)| held.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[test]
fn a_visitor_is_answered_in_the_language_they_asked_for() {
    let _turn = in_turn();
    let renderer =
        Renderer::start(bilingual(), options(Arc::new(Silent))).expect("nothing running");

    let asked = [
        SsrRequest::get("/").with_header("Accept-Language", "de-DE,de;q=0.9,en;q=0.5"),
        // The address an `hreflang` link points at reaches the same tree.
        SsrRequest::get("/de-DE/index.html"),
        // And so does a proxy that has already decided.
        SsrRequest::get("/").with_locale("de-DE"),
    ];
    for request in asked {
        let path = request.path.clone();
        let page = renderer.render(request).expect("the document is written");
        assert!(page.body.contains("Hallo"), "{path}: {}", page.body);
        assert!(
            page.body.contains(r#"lang="de-DE""#),
            "{path}: {}",
            page.body
        );
        assert_eq!(header(&page, "Content-Language"), Some("de-DE"), "{path}");
        assert_eq!(header(&page, "Vary"), Some("Accept-Language"), "{path}");
    }

    // A language the site holds no tree for is answered from the site root.
    let french = renderer
        .render(SsrRequest::get("/").with_header("Accept-Language", "fr-FR"))
        .expect("the document is written");
    assert!(french.body.contains("Hello"), "{}", french.body);
    assert_eq!(header(&french, "Content-Language"), Some("en-US"));

    // An address no page answers for is a 404 in the language that asked.
    let missing = renderer
        .render(SsrRequest::get("/nowhere").with_locale("de-DE"))
        .expect("the shell is written");
    assert_eq!(missing.status, 404);
    assert!(missing.body.contains("Hallo"), "{}", missing.body);
    assert_eq!(header(&missing, "Content-Language"), Some("de-DE"));
}

#[test]
fn a_site_in_one_language_says_nothing_about_varying() {
    let _turn = in_turn();
    let site = Arc::new(
        SsrSite::new(app_saying("Hello"), WebSpec::default()).expect("the entry is the page"),
    );
    let renderer = Renderer::start(site, options(Arc::new(Silent))).expect("nothing running");
    let page = renderer
        .render(SsrRequest::get("/").with_header("Accept-Language", "de-DE"))
        .expect("the document is written");

    assert!(page.body.contains("Hello"), "{}", page.body);
    assert_eq!(header(&page, "Content-Language"), Some("en-US"));
    assert_eq!(header(&page, "Vary"), None);
}
