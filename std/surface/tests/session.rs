//! A surface session against a producer over the Unix transport, with
//! no GPU: the consumer's backend records what it would import and show, and
//! the producer is the protocol crate's own. Everything about the session
//! (handshake, refusals, pacing, slot reuse, resize generations, stalls,
//! input, crashes and reconnects) runs here on any Unix machine.

#![cfg(unix)]

use lumen_surface::protocol::producer::{FrameTimes, Producer, ProducerEvent, ProducerHello};
use lumen_surface::protocol::wire::{
    AdapterId, Api, BUFFER_DEDICATED, BufferDesc, Buffers, ByeReason, Format, InputEvent,
    RejectReason,
};
use lumen_surface::session::{self, Backend, Happened, Imported, Shared, SurfaceStateKind, Wanted};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ADAPTER: AdapterId = AdapterId {
    device_uuid: [7; 16],
    driver_uuid: [8; 16],
    luid: [0; 8],
    registry_id: 0,
    vendor: 0x1002,
    device: 0x73bf,
};

/// A backend that imports nothing and shows nothing, but records both, and
/// lets go of a shown frame when the next one is shown, as a renderer whose
/// GPU work is done would.
#[derive(Default)]
struct Recorder {
    shown: Mutex<Vec<(u32, u32, u64)>>,
    showing: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    imported: Mutex<Vec<(u32, usize)>>,
    generation: AtomicU64,
    /// How long a ring's first-use work takes, as a slow GPU's would.
    prepare: Mutex<Duration>,
}

impl Backend for Recorder {
    type Texture = (u32, u32);

    fn device(&self) -> Option<(Api, AdapterId)> {
        Some((Api::Vulkan, ADAPTER))
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn import(&self, buffers: &Buffers, fds: Vec<OwnedFd>) -> Result<Imported<(u32, u32)>, String> {
        assert_eq!(fds.len(), buffers.buffers.len(), "a descriptor per buffer");
        self.imported
            .lock()
            .unwrap()
            .push((buffers.generation, fds.len()));
        let textures = (0..buffers.buffers.len() as u32)
            .map(|slot| (buffers.generation, slot))
            .collect();
        let prepare = *self.prepare.lock().unwrap();
        if prepare.is_zero() {
            return Ok(Imported::now(textures));
        }
        let until = Instant::now() + prepare;
        Ok(Imported {
            textures,
            ready: Box::new(move || Instant::now() >= until),
        })
    }

    fn present(&self, texture: (u32, u32), stamp: u64, released: Box<dyn FnOnce() + Send>) {
        self.shown
            .lock()
            .unwrap()
            .push((texture.0, texture.1, stamp));
        let previous = self.showing.lock().unwrap().replace(released);
        if let Some(previous) = previous {
            previous();
        }
    }
}

fn hello() -> ProducerHello {
    ProducerHello {
        api: Api::Vulkan,
        adapter: ADAPTER,
        engine: "test producer".into(),
        caps: 0,
    }
}

/// Wait for `check`, up to five seconds.
fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Everything that happened, gathered until `check` holds.
fn wait_happened(
    shared: &Shared,
    seen: &mut Vec<Happened>,
    mut check: impl FnMut(&[Happened]) -> bool,
) {
    wait_for("an event", || {
        seen.extend(shared.take().0);
        check(seen)
    });
}

/// A ring of `n` buffers whose descriptors are open files: the consumer
/// side here imports nothing, it only needs a descriptor per buffer.
fn ring(generation: u32, size: (u32, u32), n: usize) -> (Buffers, Vec<std::fs::File>) {
    let files: Vec<std::fs::File> = (0..n)
        .map(|_| std::fs::File::open("/dev/null").unwrap())
        .collect();
    (
        Buffers {
            generation,
            width: size.0,
            height: size.1,
            format: Format::Rgba8Srgb,
            usage: 0x17,
            flags: BUFFER_DEDICATED,
            buffers: (0..n as u64)
                .map(|handle| BufferDesc {
                    alloc_size: 4096,
                    handle,
                    ..BufferDesc::default()
                })
                .collect(),
        },
        files,
    )
}

fn offer(producer: &mut Producer, generation: u32, size: (u32, u32), n: usize) {
    let (buffers, files) = ring(generation, size, n);
    let fds: Vec<i32> = files.iter().map(|f| f.as_raw_fd()).collect();
    producer
        .offer_buffers(buffers, &fds)
        .expect("the ring is accepted");
}

/// Render one frame: wait for a slot, announce it, and let the consumer's
/// element take it the way its systems do each tick (which asks for the
/// next one).
fn one_frame(producer: &mut Producer, shared: &Shared) -> u64 {
    let before = shared.frame().and_then(|f| f.received_at);
    let slot = producer
        .next_slot(Duration::from_secs(5))
        .expect("the session is up")
        .expect("a slot is free and asked for");
    let frame = producer
        .frame_ready(slot, b"meta", FrameTimes::default())
        .expect("announced");
    wait_for("the frame to show", || {
        shared
            .frame()
            .is_some_and(|f| f.frame == frame && f.received_at != before)
    });
    shared.request_frame();
    frame
}

fn start() -> (Arc<Shared>, Arc<Recorder>) {
    let backend = Arc::new(Recorder::default());
    let shared = session::start("scene", "scene", backend.clone()).expect("listening");
    (shared, backend)
}

/// A producer that knows the endpoint is welcomed at the size the element
/// wants; its frames show one after another and every slot comes back, so a
/// ring of three keeps going for as long as frames are asked for.
#[test]
fn frames_flow_and_slots_come_back() {
    let (shared, backend) = start();
    shared.set_wanted(Wanted {
        width: 320,
        height: 200,
        scale: 1.0,
    });
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    assert_eq!(
        (producer.welcome().width, producer.welcome().height),
        (320, 200)
    );
    offer(&mut producer, 1, (320, 200), 3);
    for _ in 0..10 {
        one_frame(&mut producer, &shared);
    }
    assert_eq!(shared.state(), SurfaceStateKind::Live);
    let shown = backend.shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 10);
    assert!(shown.iter().all(|(g, ..)| *g == 1));
    assert_eq!(shared.frame().unwrap().meta, b"meta");
    let mut seen = Vec::new();
    wait_happened(&shared, &mut seen, |h| {
        h.iter()
            .any(|e| matches!(e, Happened::Connected { engine, .. } if engine == "test producer"))
    });
    producer.close(ByeReason::Exit, "done");
    wait_happened(&shared, &mut seen, |h| {
        h.iter()
            .any(|e| matches!(e, Happened::Disconnected { reason: "exit", .. }))
    });
    assert_eq!(
        shared.state(),
        SurfaceStateKind::Lost,
        "the last frame stays"
    );
    shared.close();
}

/// A producer with the wrong token, another GPU, or a second one on a busy
/// channel is refused, and told why.
#[test]
fn the_wrong_producer_is_refused() {
    let (shared, _) = start();
    let endpoint = shared.endpoint().expect("a transport");
    let mut forged: lumen_surface::protocol::endpoint::Endpoint = endpoint.parse().unwrap();
    forged.token[0] ^= 1;
    let reason =
        |result: Result<Producer, lumen_surface::protocol::producer::ProducerError>| match result {
            Err(lumen_surface::protocol::producer::ProducerError::Rejected(r)) => r.reason,
            Err(e) => panic!("refused for another reason: {e}"),
            Ok(p) => panic!("welcomed: {:?}", p.welcome()),
        };
    assert_eq!(
        reason(Producer::connect(&forged.to_string(), &hello())),
        RejectReason::Token
    );
    let mut other_gpu = hello();
    other_gpu.adapter.device_uuid = [9; 16];
    assert_eq!(
        reason(Producer::connect(&endpoint, &other_gpu)),
        RejectReason::Adapter
    );
    let first = Producer::connect(&endpoint, &hello()).expect("the first one is welcome");
    assert_eq!(
        reason(Producer::connect(&endpoint, &hello())),
        RejectReason::Busy
    );
    drop(first);
    shared.close();
}

/// A new size goes to the producer, whose next ring is a new generation;
/// the old ring is retired once its slots are back.
#[test]
fn a_resize_brings_a_new_generation_and_retires_the_old() {
    let (shared, backend) = start();
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    one_frame(&mut producer, &shared);
    assert!(shared.set_wanted(Wanted {
        width: 640,
        height: 480,
        scale: 2.0,
    }));
    let mut resized = None;
    wait_for("the resize", || {
        while let Ok(Some(event)) = producer.poll_event() {
            if let ProducerEvent::Resize {
                width,
                height,
                scale,
            } = event
            {
                resized = Some((width, height, scale));
            }
        }
        resized.is_some()
    });
    assert_eq!(resized, Some((640, 480, 2.0)));
    offer(&mut producer, 2, (640, 480), 3);
    one_frame(&mut producer, &shared);
    one_frame(&mut producer, &shared);
    assert_eq!(shared.frame().unwrap().size, (640, 480));
    let mut retired = false;
    wait_for("generation 1 to retire", || {
        while let Ok(Some(event)) = producer.poll_event() {
            retired |= event == ProducerEvent::Retired(1);
        }
        retired
    });
    assert_eq!(
        backend.imported.lock().unwrap().clone(),
        vec![(1, 3), (2, 3)]
    );
    shared.close();
}

/// A frame asked for and not delivered within the stall timeout stalls the
/// surface; the next frame resumes it.
#[test]
fn a_late_frame_stalls_and_the_next_resumes() {
    let (shared, _) = start();
    shared.set_stall_timeout(Duration::from_millis(100));
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    let mut seen = Vec::new();
    wait_happened(&shared, &mut seen, |h| {
        h.iter().any(|e| matches!(e, Happened::Stalled { .. }))
    });
    assert_eq!(shared.state(), SurfaceStateKind::Stalled);
    one_frame(&mut producer, &shared);
    wait_happened(&shared, &mut seen, |h| h.contains(&Happened::Resumed));
    assert_eq!(shared.state(), SurfaceStateKind::Live);
    shared.close();
}

/// Input goes to the producer in order with rising sequence numbers, which
/// its frames echo; messages go both ways.
#[test]
fn input_and_messages_cross() {
    let (shared, _) = start();
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    let key = InputEvent::Key {
        code: 26,
        pressed: true,
        repeat: false,
        modifiers: 0,
        key: "w".into(),
    };
    let first = shared
        .send_input(InputEvent::Focus(true))
        .expect("connected");
    let second = shared.send_input(key.clone()).expect("connected");
    assert!(second > first);
    assert!(shared.send_user(b"from the app"));
    let mut got = Vec::new();
    wait_for("the input", || {
        while let Ok(Some(event)) = producer.poll_event() {
            got.push(event);
        }
        got.len() >= 3
    });
    assert!(
        matches!(&got[0], ProducerEvent::Input { seq, event: InputEvent::Focus(true), .. } if *seq == first)
    );
    assert!(
        matches!(&got[1], ProducerEvent::Input { seq, event, .. } if *seq == second && *event == key)
    );
    assert_eq!(got[2], ProducerEvent::User(b"from the app".to_vec()));
    let frame = one_frame(&mut producer, &shared);
    assert!(frame > 0);
    assert_eq!(
        shared.frame().unwrap().input_seq,
        second,
        "the frame echoes the input"
    );
    producer.send_user(b"from the game").unwrap();
    producer
        .set_cursor(lumen_surface::protocol::wire::cursor::CROSSHAIR)
        .unwrap();
    producer.request_pointer_lock(true).unwrap();
    let mut seen = Vec::new();
    wait_happened(&shared, &mut seen, |h| h.len() >= 4);
    assert!(seen.contains(&Happened::User(b"from the game".to_vec())));
    assert!(seen.contains(&Happened::Cursor("crosshair")));
    assert!(seen.contains(&Happened::PointerLock(true)));
    shared.close();
}

/// A producer that dies without a goodbye leaves the surface lost with its
/// last frame; a new producer on the same channel is welcomed and shows
/// frames again.
#[test]
fn a_crash_is_reported_and_a_new_producer_takes_over() {
    let (shared, backend) = start();
    let endpoint = shared.endpoint().expect("a transport");
    let mut producer = Producer::connect(&endpoint, &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    one_frame(&mut producer, &shared);
    drop(producer);
    let mut seen = Vec::new();
    wait_happened(&shared, &mut seen, |h| {
        h.iter().any(|e| {
            matches!(
                e,
                Happened::Disconnected {
                    reason: "crash",
                    ..
                }
            )
        })
    });
    assert_eq!(shared.state(), SurfaceStateKind::Lost);
    assert!(shared.frame().is_some(), "the last frame stays");

    let mut again = Producer::connect(&endpoint, &hello()).expect("welcomed again");
    offer(&mut again, 1, (300, 150), 2);
    one_frame(&mut again, &shared);
    assert_eq!(shared.state(), SurfaceStateKind::Live);
    assert_eq!(backend.shown.lock().unwrap().len(), 2);
    shared.close();
}

/// Nobody seeing the surface is news to the producer; so is the device
/// changing, after which the producer offers a new ring on the same
/// connection.
#[test]
fn visibility_and_device_changes_reach_the_producer() {
    let (shared, backend) = start();
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    one_frame(&mut producer, &shared);
    shared.set_visible(false);
    backend.generation.fetch_add(1, Ordering::SeqCst);
    let mut got = Vec::new();
    wait_for("visibility and the reset", || {
        while let Ok(Some(event)) = producer.poll_event() {
            got.push(event);
        }
        got.contains(&ProducerEvent::Reset)
    });
    assert!(got.contains(&ProducerEvent::Visibility(false)));
    shared.set_visible(true);
    offer(&mut producer, 2, (300, 150), 3);
    one_frame(&mut producer, &shared);
    assert_eq!(shared.frame().unwrap().generation, 2);
    shared.close();
}

/// A producer that stops answering is hung: after a ping goes unanswered the
/// session ends, and the surface listens for the next one.
#[test]
fn a_producer_that_stops_answering_is_hung() {
    let (shared, _) = start();
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 2);
    // From here the producer never reads its socket.
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !seen
        .iter()
        .any(|e| matches!(e, Happened::Disconnected { reason: "hung", .. }))
    {
        assert!(
            Instant::now() < deadline,
            "the hung producer was not dropped"
        );
        seen.extend(shared.take().0);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(shared.state(), SurfaceStateKind::Lost);
    drop(producer);
    shared.close();
}

/// A ring whose first-use work outlasts the producer's answer timeout is
/// still accepted: the consumer keeps serving while it prepares, pinging the
/// waiting producer, and every message restarts the producer's wait. The
/// first frame then flows as usual.
#[test]
fn a_ring_that_takes_long_to_prepare_is_still_accepted() {
    let (shared, backend) = start();
    *backend.prepare.lock().unwrap() =
        lumen_surface::protocol::producer::ANSWER_TIMEOUT + Duration::from_secs(1);
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    let started = Instant::now();
    offer(&mut producer, 1, (64, 48), 3);
    assert!(
        started.elapsed() > lumen_surface::protocol::producer::ANSWER_TIMEOUT,
        "the ring was accepted before its preparation finished"
    );
    shared.request_frame();
    one_frame(&mut producer, &shared);
    assert_eq!(shared.state(), SurfaceStateKind::Live);
    producer.close(ByeReason::Exit, "done");
    shared.close();
}
