//! A renderer that turns a request away rather than queueing it without end,
//! and that says so when a render does not come back.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use lumen_ir::artifact::{CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_script::{HttpDispatch, HttpDone, HttpRequest};
use lumen_ssr::{FetchPolicy, RenderOptions, Renderer, SsrError, SsrRequest, SsrSite};
use lumen_web::WebSpec;

/// A program that asks an API for something on start, which is the call the
/// transport below holds on to.
const FETCHES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fetches.cdlb"));

/// A process has one renderer, so the tests take it in turn.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn in_turn() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// A transport whose call does not return until the test lets go of it, so
/// the tick that made the call does not return either.
struct Blocks {
    entered: Mutex<Sender<()>>,
    release: Mutex<Receiver<()>>,
}

impl HttpDispatch for Blocks {
    fn dispatch(&self, _label: &str, _request: HttpRequest, _body_limit: u64, done: HttpDone) {
        if let Ok(entered) = self.entered.lock() {
            let _ = entered.send(());
        }
        if let Ok(release) = self.release.lock() {
            let _ = release.recv();
        }
        done(Err("the test let go".to_string()));
    }
}

/// A page of one label, whose script asks the API for something on start.
fn app() -> CompiledApp {
    CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                children: vec![Element {
                    tag: "label".to_string(),
                    attrs: Attributes {
                        text: Some("rendered".to_string()),
                        ..Attributes::default()
                    },
                    ..Element::default()
                }],
                ..Element::default()
            },
            ..LayoutIR::default()
        },
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: String::new(),
            bytecode: Some(FETCHES.to_vec()),
        }],
        ..CompiledApp::default()
    }
}

/// A renderer whose every render blocks in its first tick, plus the two ends
/// the test drives it with: told when a render is held, and letting one go.
fn blocking(queue: usize) -> (Arc<Renderer>, Receiver<()>, Sender<()>) {
    let (entered, held) = channel();
    let (release, released) = channel();
    let site = SsrSite::new(app(), WebSpec::default()).expect("the entry is the page");
    let options = RenderOptions {
        fetch: FetchPolicy::default().allow_host("api.example.com"),
        dispatch: Arc::new(Blocks {
            entered: Mutex::new(entered),
            release: Mutex::new(released),
        }),
        queue: Some(queue),
        ..RenderOptions::default()
    };
    let renderer = Renderer::start(Arc::new(site), options).expect("nothing else is rendering");
    (Arc::new(renderer), held, release)
}

/// Wait until `ready` holds, or fail the test after a generous while.
fn until(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "never saw {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_full_queue_turns_a_request_away_at_once() {
    let _turn = in_turn();
    let (renderer, held, release) = blocking(1);
    let limit = Duration::from_secs(60);

    let first = {
        let renderer = Arc::clone(&renderer);
        thread::spawn(move || renderer.try_render(SsrRequest::get("/"), limit))
    };
    held.recv_timeout(Duration::from_secs(30))
        .expect("the first render reached the transport");
    assert!(!renderer.is_saturated(), "one place in the queue is free");

    let second = {
        let renderer = Arc::clone(&renderer);
        thread::spawn(move || renderer.try_render(SsrRequest::get("/"), limit))
    };
    until("the second request queued", || renderer.is_saturated());

    let started = Instant::now();
    let third = renderer.try_render(SsrRequest::get("/"), limit);
    assert!(matches!(third, Err(SsrError::Busy)), "{third:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a full queue answered only after {:?}",
        started.elapsed()
    );

    // Both of the others are rendered once the transport lets go.
    let _ = release.send(());
    held.recv_timeout(Duration::from_secs(30))
        .expect("the second render reached the transport");
    let _ = release.send(());
    for (name, render) in [("first", first), ("second", second)] {
        let response = render.join().expect("the caller's thread");
        let response = response.unwrap_or_else(|e| panic!("the {name} render failed: {e}"));
        assert!(response.body.contains("rendered"), "{}", response.body);
    }
    assert!(!renderer.is_saturated());
}

#[test]
fn a_render_past_its_limit_wedges_the_renderer_and_empties_the_queue() {
    let _turn = in_turn();
    let (renderer, held, release) = blocking(4);

    let stuck = {
        let renderer = Arc::clone(&renderer);
        thread::spawn(move || renderer.try_render(SsrRequest::get("/"), Duration::from_millis(200)))
    };
    held.recv_timeout(Duration::from_secs(30))
        .expect("the render reached the transport");
    let queued = {
        let renderer = Arc::clone(&renderer);
        thread::spawn(move || renderer.try_render(SsrRequest::get("/"), Duration::from_secs(60)))
    };

    let stuck = stuck.join().expect("the caller's thread");
    assert!(matches!(stuck, Err(SsrError::TimedOut)), "{stuck:?}");
    assert!(renderer.is_stopped());
    // What waited behind it is answered rather than left waiting.
    let queued = queued.join().expect("the caller's thread");
    assert!(matches!(queued, Err(SsrError::Stopped)), "{queued:?}");
    let after = renderer.try_render(SsrRequest::get("/"), Duration::from_secs(60));
    assert!(matches!(after, Err(SsrError::Stopped)), "{after:?}");

    // Dropping a wedged renderer does not wait on the tick that never ended,
    // and the process gets its renderer back once that tick does.
    let dropped = Instant::now();
    drop(renderer);
    assert!(dropped.elapsed() < Duration::from_secs(5));
    let _ = release.send(());
    until("the process free to render again", || {
        Renderer::start(
            Arc::new(SsrSite::new(app(), WebSpec::default()).expect("a site")),
            RenderOptions::default(),
        )
        .is_ok()
    });
}
